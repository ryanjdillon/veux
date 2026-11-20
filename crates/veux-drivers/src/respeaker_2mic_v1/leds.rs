//! The three APA102 LEDs of the HAT, on SPI0 (`/dev/spidev0.0`).
//!
//! A write to the spidev node sends the bytes at the speed of the device
//! tree (8 MHz on the satellites), in SPI mode 0, which the APA102 accepts.
//!
//! The LEDs cannot be read back. veux keeps the last written state in a
//! small file, so that a later session can show it. Without the file the
//! state is unknown, and the LED parameters have no runtime value.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use veux_model::{Access, ParamId, ParamSpec, ParamType, Value, Values};

use crate::device::DeviceError;

pub const SPI_DEVICE: &str = "/dev/spidev0.0";
pub const COUNT: usize = 3;

pub const POWER: &str = "led.power";
pub const BRIGHTNESS: &str = "led.brightness";
pub const RGB: &str = "led.rgb";

/// Sends bytes to the LEDs. Tests replace it.
pub trait LedPort {
    fn send(&mut self, bytes: &[u8]) -> Result<(), DeviceError>;
}

pub struct SpiDev(pub PathBuf);

impl LedPort for SpiDev {
    fn send(&mut self, bytes: &[u8]) -> Result<(), DeviceError> {
        let path = self.0.display();
        OpenOptions::new()
            .write(true)
            .open(&self.0)
            .and_then(|mut f| f.write_all(bytes))
            .map_err(|e| DeviceError(format!("cannot write {path}: {e}")))
    }
}

/// What the LEDs show. All three LEDs have the same color.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LedState {
    pub power: bool,
    /// The APA102 global brightness, 0 to 31.
    pub brightness: u8,
    pub rgb: [u8; 3],
}

impl Default for LedState {
    fn default() -> Self {
        Self {
            power: false,
            brightness: 8,
            rgb: [255, 255, 255],
        }
    }
}

/// The APA102 frame for `count` LEDs: a start frame of 32 zero bits, one
/// frame for each LED (`111` and 5 brightness bits, then blue, green,
/// red), and an end frame of 32 one bits to clock the last LED.
pub fn frame(state: &LedState, count: usize) -> Vec<u8> {
    let (brightness, [r, g, b]) = if state.power {
        (state.brightness.min(31), state.rgb)
    } else {
        (0, [0, 0, 0])
    };
    let mut out = vec![0u8; 4];
    for _ in 0..count {
        out.extend_from_slice(&[0xE0 | brightness, b, g, r]);
    }
    out.extend_from_slice(&[0xFF; 4]);
    out
}

pub fn specs() -> Vec<ParamSpec> {
    let spec = |id: &str, description: &str, ty| ParamSpec {
        id: id.into(),
        group: "led".into(),
        description: description.into(),
        ty,
        unit: None,
        access: Access::ReadWrite,
        // The LEDs go dark at power off.
        persistent: false,
    };
    vec![
        spec(POWER, "LEDs on", ParamType::Bool),
        spec(
            BRIGHTNESS,
            "LED brightness (APA102 global brightness)",
            ParamType::Int { min: 0, max: 31 },
        ),
        spec(
            RGB,
            "LED color, red, green, and blue",
            ParamType::Vector {
                len: 3,
                min: 0.0,
                max: 255.0,
            },
        ),
    ]
}

pub fn is_led(id: &ParamId) -> bool {
    id.as_str().starts_with("led.")
}

pub struct Leds {
    port: Box<dyn LedPort>,
    /// Where the last written state is kept. `None` keeps it in memory only.
    state_file: Option<PathBuf>,
    state: Option<LedState>,
}

impl Leds {
    pub fn new(port: Box<dyn LedPort>, state_file: Option<PathBuf>) -> Self {
        let state = state_file
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|text| toml::from_str(&text).ok());
        Self {
            port,
            state_file,
            state,
        }
    }

    /// The LEDs on this host, if the SPI device exists.
    pub fn open_local() -> Option<Self> {
        let path = PathBuf::from(SPI_DEVICE);
        if !path.exists() {
            return None;
        }
        let state_file =
            std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache/veux/leds.toml"));
        Some(Self::new(Box::new(SpiDev(path)), state_file))
    }

    /// The last written state. Empty when it is unknown.
    pub fn values(&self) -> Values {
        let Some(s) = self.state else {
            return Values::new();
        };
        Values::from([
            (POWER.into(), Value::Bool(s.power)),
            (BRIGHTNESS.into(), Value::Int(s.brightness.into())),
            (
                RGB.into(),
                Value::Vector(s.rgb.iter().map(|&c| f64::from(c)).collect()),
            ),
        ])
    }

    /// Change the LED values in `values` and send the new state. Other
    /// LED values keep their last state, or the default.
    pub fn write(&mut self, values: &Values) -> Result<(), DeviceError> {
        let mut next = self.state.unwrap_or_default();
        for (id, value) in values.iter().filter(|(id, _)| is_led(id)) {
            match (id.as_str(), value) {
                (POWER, Value::Bool(on)) => next.power = *on,
                (BRIGHTNESS, Value::Int(b)) if (0..=31).contains(b) => next.brightness = *b as u8,
                (RGB, Value::Vector(rgb)) if rgb.len() == 3 => {
                    for (out, c) in next.rgb.iter_mut().zip(rgb) {
                        if !(0.0..=255.0).contains(c) {
                            return Err(DeviceError(format!("{id}: {c} is out of range")));
                        }
                        *out = c.round() as u8;
                    }
                }
                _ => return Err(DeviceError(format!("{id}: wrong value {value}"))),
            }
        }
        self.port.send(&frame(&next, COUNT))?;
        self.state = Some(next);
        if let Some(path) = &self.state_file {
            // The LEDs show the state already; a lost file only loses the
            // display of it in a later session.
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(path, toml::to_string(&next).unwrap_or_default());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    #[derive(Clone, Default)]
    struct Recorder(Arc<Mutex<Vec<Vec<u8>>>>);

    impl LedPort for Recorder {
        fn send(&mut self, bytes: &[u8]) -> Result<(), DeviceError> {
            self.0.lock().unwrap().push(bytes.to_vec());
            Ok(())
        }
    }

    #[test]
    fn frame_has_start_leds_in_bgr_and_end() {
        let state = LedState {
            power: true,
            brightness: 40,
            rgb: [1, 2, 3],
        };
        let f = frame(&state, 2);
        assert_eq!(
            f,
            [
                0, 0, 0, 0, 0xFF, 3, 2, 1, 0xFF, 3, 2, 1, 0xFF, 0xFF, 0xFF, 0xFF
            ]
        );
        let off = frame(
            &LedState {
                power: false,
                ..state
            },
            1,
        );
        assert_eq!(&off[4..8], &[0xE0, 0, 0, 0]);
    }

    #[test]
    fn state_is_unknown_until_written_and_then_kept_in_the_file() {
        let dir = std::env::temp_dir().join(format!("veux-leds-{}", std::process::id()));
        let file = dir.join("leds.toml");
        let _ = std::fs::remove_file(&file);
        let sent = Recorder::default();
        let mut leds = Leds::new(Box::new(sent.clone()), Some(file.clone()));
        assert!(leds.values().is_empty());
        leds.write(&Values::from([
            (POWER.into(), Value::Bool(true)),
            (RGB.into(), Value::Vector(vec![0.0, 128.0, 255.0])),
        ]))
        .unwrap();
        assert_eq!(sent.0.lock().unwrap()[0][4..8], [0xE0 | 8, 255, 128, 0]);
        let again = Leds::new(Box::new(Recorder::default()), Some(file.clone()));
        assert_eq!(again.values()[&ParamId::from(POWER)], Value::Bool(true));
        assert_eq!(again.values()[&ParamId::from(BRIGHTNESS)], Value::Int(8));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_bad_value_sends_nothing() {
        let sent = Recorder::default();
        let mut leds = Leds::new(Box::new(sent.clone()), None);
        let bad = Values::from([(RGB.into(), Value::Vector(vec![0.0, 300.0, 0.0]))]);
        assert!(leds.write(&bad).is_err());
        assert!(sent.0.lock().unwrap().is_empty());
        assert!(leds.values().is_empty());
    }
}
