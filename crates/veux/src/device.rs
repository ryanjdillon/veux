//! CLI commands that talk to devices.

use std::process::ExitCode;

use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use veux_drivers::xvf3800::{Xvf3800, fmt_version, protocol::Client, usb};
use veux_model::{Config, ParamId, Schema, Values};

use veux_drivers::Device;
use veux_drivers::metrics::{Level, Metrics, Tone, ToneChannel};

use crate::connection::connector;
use crate::profile::parse_toml_value;

pub fn probe() -> Result<ExitCode> {
    let found = usb::list()?;
    if found.is_empty() {
        println!(
            "no XVF3800 on local USB ({:04x}:{:04x})",
            usb::VENDOR_ID,
            usb::PRODUCT_ID
        );
        return Ok(ExitCode::SUCCESS);
    }
    for info in found {
        let serial = info.serial.as_deref().unwrap_or("unknown");
        println!(
            "XVF3800 on bus {} address {}, serial {serial}",
            info.bus, info.address
        );
        let mut dev = match usb::open(info.serial.as_deref()) {
            Ok(port) => Xvf3800::open(Client::new(port))?,
            Err(e) => {
                println!("  {e}");
                continue;
            }
        };
        println!("  firmware {}", fmt_version(dev.version()));
        print_values(&dev.read(&[])?);
    }
    Ok(ExitCode::SUCCESS)
}

pub fn read(config: &Config, name: &str, ids: &[String]) -> Result<ExitCode> {
    let (mut conn, schema) = open(config, name)?;
    let mut values = conn.read_all()?;
    if !ids.is_empty() {
        let wanted: Vec<ParamId> = ids.iter().map(|s| ParamId::from(s.as_str())).collect();
        if let Some(id) = wanted.iter().find(|id| schema.get(id).is_none()) {
            return Err(anyhow!("unknown parameter {id}"));
        }
        values.retain(|id, _| wanted.contains(id));
    }
    print_values(&values);
    Ok(ExitCode::SUCCESS)
}

pub fn write(config: &Config, name: &str, assignments: &[String]) -> Result<ExitCode> {
    let (mut conn, schema) = open(config, name)?;
    let mut values = Values::new();
    for a in assignments {
        let (id, raw) = a
            .split_once('=')
            .ok_or_else(|| anyhow!("expected ID=VALUE, got {a:?}"))?;
        let id = ParamId::from(id.trim());
        let spec = schema
            .get(&id)
            .ok_or_else(|| anyhow!("unknown parameter {id}"))?;
        let raw = parse_toml_value(raw.trim()).with_context(|| format!("value of {id}"))?;
        values.insert(
            id.clone(),
            spec.parse(&raw).with_context(|| format!("{id}"))?,
        );
    }
    conn.write(&values)?;
    let mut after = conn.read_all()?;
    after.retain(|id, _| values.contains_key(id));
    print_values(&after);
    Ok(ExitCode::SUCCESS)
}

/// Connect, and return the schema that the device reports.
fn open(config: &Config, name: &str) -> Result<(Box<dyn Device>, Schema)> {
    let device = config
        .devices
        .get(name)
        .ok_or_else(|| anyhow!("unknown device {name:?}"))?;
    let conn = connector(config)(name, device)?;
    let schema = conn.schema();
    Ok((conn, schema))
}

fn print_values(values: &Values) {
    let width = values.keys().map(|id| id.as_str().len()).max().unwrap_or(0);
    for (id, value) in values {
        println!("  {:width$}  {value}", id.as_str());
    }
}

/// What `firmware` writes.
pub enum Flash<'a> {
    /// An image file.
    Image(&'a std::path::Path),
    /// The image that an ESPHome device holds in its build.
    Bundled,
}

/// Show firmware facts, and flash after a confirmation.
pub fn firmware(config: &Config, name: &str, flash: Option<Flash>, yes: bool) -> Result<ExitCode> {
    use std::io::{BufRead, Write};

    use sha2::{Digest, Sha256};

    let target = crate::connection::firmware_target(config, name)?;
    let info = target.info()?;
    println!("{name} on {}", target.place());
    println!(
        "  running   {}",
        match (&info.version, info.dfu_mode) {
            (_, true) => "DFU only (Safe Mode or bootloader)".to_string(),
            (Some(v), false) => format!("firmware {v}"),
            (None, false) => "firmware version unknown".to_string(),
        }
    );
    if info.bundled_image {
        println!("  image     the XVF3800 image of the ESPHome build; an OTA update changes it");
    } else {
        println!("  slots     {}", info.slots.join(", "));
    }
    let (bytes, question) = match flash {
        None => return Ok(ExitCode::SUCCESS),
        Some(Flash::Bundled) if !info.bundled_image => {
            bail!("{name}: --bundled needs an ESPHome device with the xvf_flash action")
        }
        Some(Flash::Image(_)) if info.bundled_image => {
            bail!("{name}: the ESP32 writes the image of its ESPHome build; use --bundled")
        }
        Some(Flash::Bundled) => (
            Vec::new(),
            format!("Have the ESP32 write the XVF3800 image of its build to {name}?"),
        ),
        Some(Flash::Image(path)) => {
            let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
            let digest: String = Sha256::digest(&bytes)
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect();
            println!("  image     {}", path.display());
            println!("            {} bytes, sha256 {digest}", bytes.len());
            let question = format!("Write this image to the upgrade slot of {name}?");
            (bytes, question)
        }
    };
    if !yes {
        print!("{question} Type yes: ");
        std::io::stdout().flush()?;
        let mut answer = String::new();
        std::io::stdin().lock().read_line(&mut answer)?;
        if answer.trim() != "yes" {
            println!("Cancelled. Nothing was written.");
            return Ok(ExitCode::FAILURE);
        }
    }
    if !matches!(target, crate::connection::FirmwareTarget::Local(_)) {
        println!("Flashing through {}…", target.place());
    }
    target.flash(&bytes, &mut |sent, total| {
        print!("\r  {:>3}%  {sent}/{total}", sent * 100 / total);
        let _ = std::io::stdout().flush();
    })?;
    println!("\nDone. The device boots the new image.");
    if !info.bundled_image {
        println!("An I2S image leaves the USB bus; that is expected.");
    }
    Ok(ExitCode::SUCCESS)
}

#[derive(Clone, Copy, Debug, Default, clap::ValueEnum)]
pub enum Channel {
    Left,
    Right,
    #[default]
    Both,
}

impl From<Channel> for ToneChannel {
    fn from(c: Channel) -> Self {
        match c {
            Channel::Left => ToneChannel::Left,
            Channel::Right => ToneChannel::Right,
            Channel::Both => ToneChannel::Both,
        }
    }
}

/// The scale of a level bar.
pub const BAR_FLOOR_DBFS: f64 = -60.0;

/// The number of the `width` cells of a level bar that `dbfs` fills.
pub fn bar_cells(dbfs: f64, width: usize) -> usize {
    let f = ((dbfs - BAR_FLOOR_DBFS) / -BAR_FLOOR_DBFS).clamp(0.0, 1.0);
    (f * width as f64).round() as usize
}

/// A bar of `width` cells for a level in dBFS, with `|` at the peak hold.
pub fn level_bar(level: &Level, width: usize) -> String {
    let fill = bar_cells(level.rms_dbfs, width);
    let hold = bar_cells(level.peak_hold_dbfs, width).saturating_sub(1);
    (0..width)
        .map(|i| match i {
            _ if i < fill => '█',
            _ if i == hold && level.peak_hold_dbfs > BAR_FLOOR_DBFS => '|',
            _ => '·',
        })
        .collect()
}

fn metrics_line(m: &Metrics) -> String {
    if let Some(note) = &m.note {
        return note.clone();
    }
    let mut parts: Vec<String> = m
        .levels
        .iter()
        .map(|l| {
            format!(
                "{} {} rms {:>5.1} peak {:>5.1} dBFS",
                l.name,
                level_bar(l, 24),
                l.rms_dbfs,
                l.peak_dbfs
            )
        })
        .collect();
    if let Some(speech) = m.speech {
        parts.push(if speech { "speech" } else { "quiet" }.into());
    }
    if let Some(deg) = m.direction_deg {
        parts.push(format!("{deg:>3.0}°"));
    }
    parts.join("  ")
}

pub fn meter(config: &Config, name: &str, seconds: u64) -> Result<ExitCode> {
    let (mut conn, _) = open(config, name)?;
    let end = Instant::now() + Duration::from_secs(seconds);
    let result = (|| {
        while Instant::now() < end {
            let Some(m) = conn.metrics()? else {
                bail!("{name} reports no live levels");
            };
            println!("{}", metrics_line(&m));
            std::thread::sleep(Duration::from_millis(250));
        }
        Ok(())
    })();
    conn.stop_metrics();
    result.map(|()| ExitCode::SUCCESS)
}

pub fn tone(config: &Config, name: &str, channel: Channel, frequency: u32) -> Result<ExitCode> {
    let (mut conn, _) = open(config, name)?;
    println!("Playing {frequency} Hz on {channel:?} of {name}…");
    conn.play_tone(Tone {
        frequency_hz: frequency,
        channel: channel.into(),
    })?;
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn level(rms: f64, hold: f64) -> Level {
        Level {
            name: "L".into(),
            peak_dbfs: hold,
            rms_dbfs: rms,
            peak_hold_dbfs: hold,
        }
    }

    #[test]
    fn bar_fills_to_the_rms_and_marks_the_peak_hold() {
        assert_eq!(level_bar(&level(-30.0, -12.0), 10), "█████··|··");
        assert_eq!(level_bar(&level(-96.0, -96.0), 4), "····");
        assert_eq!(level_bar(&level(0.0, 0.0), 4), "████");
        assert_eq!(level_bar(&level(-14.5, -0.0), 24).chars().count(), 24);
    }
}
