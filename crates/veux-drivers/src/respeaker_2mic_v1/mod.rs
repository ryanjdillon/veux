//! ReSpeaker 2-Mics Pi HAT v1.x (WM8960 codec, APA102 LEDs).
//!
//! The HAT V2.0 has a different codec (TLV320AIC3104). It needs its own
//! kind.
//!
//! The driver runs on the host that has the HAT. It reads all mixer controls
//! with `alsactl`, writes with `amixer`, and persists with `alsactl store`.
//! Running the tools, not linking libasound, keeps the helper a static
//! binary.

pub mod alsa_state;
pub mod leds;

use std::collections::BTreeMap;
use std::process::Command;

use veux_model::{Access, KindSpec, Link, ParamId, ParamSpec, ParamType, Schema, Value, Values};

use crate::device::{Device, DeviceError, Persisted};
use crate::metrics::{LevelMeter, Metrics, Tone, ToneChannel};
use crate::slug::slug;
use alsa_state::{Control, ControlType};

pub const KIND: &str = "respeaker-2mic-v1";
/// The card name that the satellite hosts give the HAT.
pub const DEFAULT_CARD: &str = "respeaker2mic";
/// The file that `alsactl store` writes and alsa-restore loads at boot.
const STATE_FILE: &str = "/var/lib/alsa/asound.state";

/// An `alsactl` dump of the HAT on a satellite host. It is the source of the
/// static schema that `veux check` uses.
const REFERENCE_STATE: &str = include_str!("../../tests/fixtures/respeaker2mic.state");

pub fn kind() -> KindSpec {
    KindSpec {
        id: KIND,
        links: &[Link::Alsa],
        // The LED parameters are in the static schema, so `veux check`
        // accepts them in a profile. A host without SPI0 does not report them.
        schema: Some(Schema::new(
            alsa_state::parse(REFERENCE_STATE)
                .iter()
                .filter_map(spec_of)
                .chain(leds::specs()),
        )),
    }
}

/// Runs a program on the host. Tests replace it.
pub trait Runner {
    fn run(&mut self, program: &str, args: &[&str]) -> Result<String, DeviceError>;
}

/// Runs programs with `std::process`.
pub struct System;

impl Runner for System {
    fn run(&mut self, program: &str, args: &[&str]) -> Result<String, DeviceError> {
        let output = Command::new(program)
            .args(args)
            .output()
            .map_err(|e| DeviceError(format!("cannot run {program}: {e}")))?;
        if !output.status.success() {
            return Err(DeviceError(format!(
                "{program} {}: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }
}

pub struct Mixer<R = System> {
    card: String,
    runner: R,
    controls: BTreeMap<ParamId, Control>,
    meter: Option<Capture>,
    /// The LEDs, when the host has the SPI device for them.
    leds: Option<leds::Leds>,
}

/// A running `arecord` and the meter that reads it. Dropping it stops the
/// capture.
struct Capture {
    child: std::process::Child,
    meter: LevelMeter,
}

impl Drop for Capture {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Mixer {
    pub fn open(card: &str) -> Result<Self, DeviceError> {
        let mut mixer = Self::with_runner(card, System)?;
        mixer.leds = leds::Leds::open_local();
        Ok(mixer)
    }
}

impl<R: Runner> Mixer<R> {
    pub fn with_runner(card: &str, runner: R) -> Result<Self, DeviceError> {
        let mut mixer = Self {
            card: card.to_string(),
            runner,
            controls: BTreeMap::new(),
            meter: None,
            leds: None,
        };
        mixer.reload()?;
        Ok(mixer)
    }

    fn reload(&mut self) -> Result<(), DeviceError> {
        let text = self
            .runner
            .run("alsactl", &["-f", "-", "store", &self.card])?;
        self.controls = alsa_state::parse(&text)
            .into_iter()
            .filter(|c| spec_of(c).is_some())
            .map(|c| (param_id(&c), c))
            .collect();
        if self.controls.is_empty() {
            return Err(DeviceError(format!(
                "ALSA card {} has no mixer controls",
                self.card
            )));
        }
        Ok(())
    }
}

impl<R: Runner> Device for Mixer<R> {
    fn describe(&self) -> String {
        format!("ALSA card {}", self.card)
    }

    fn schema(&self) -> Schema {
        let mixer = self.controls.values().filter_map(spec_of);
        let leds = self.leds.iter().flat_map(|_| leds::specs());
        Schema::new(mixer.chain(leds))
    }

    fn read_all(&mut self) -> Result<Values, DeviceError> {
        self.reload()?;
        let mut values: Values = self
            .controls
            .iter()
            .filter_map(|(id, c)| Some((id.clone(), value_of(c)?)))
            .collect();
        if let Some(leds) = &self.leds {
            values.extend(leds.values());
        }
        Ok(values)
    }

    fn write(&mut self, values: &Values) -> Result<(), DeviceError> {
        let (led_values, values): (Values, Values) = values
            .clone()
            .into_iter()
            .partition(|(id, _)| leds::is_led(id));
        if !led_values.is_empty() && self.leds.is_none() {
            return Err(DeviceError(format!(
                "this host has no {}; the LEDs need SPI0",
                leds::SPI_DEVICE
            )));
        }
        // Check all values first, so that a bad value writes nothing.
        let mut commands = Vec::with_capacity(values.len());
        for (id, value) in &values {
            let control = self
                .controls
                .get(id)
                .ok_or_else(|| DeviceError(format!("unknown parameter {id}")))?;
            if !control.writable {
                return Err(DeviceError(format!("parameter {id} is read-only")));
            }
            let text = cset_text(control, value)
                .ok_or_else(|| DeviceError(format!("{id}: wrong value type {value}")))?;
            commands.push((format!("numid={}", control.numid), text));
        }
        for (numid, text) in commands {
            self.runner
                .run("amixer", &["-c", &self.card, "-q", "cset", &numid, &text])?;
        }
        if let Some(leds) = self.leds.as_mut().filter(|_| !led_values.is_empty()) {
            leds.write(&led_values)?;
        }
        Ok(())
    }

    /// Levels of both microphones, from a capture that runs until
    /// `stop_metrics`. The capture fails while another program, such as the
    /// voice satellite, holds the microphones.
    fn metrics(&mut self) -> Result<Option<Metrics>, DeviceError> {
        if self.meter.is_none() {
            let device = format!("hw:CARD={},DEV=0", self.card);
            let mut child = Command::new("arecord")
                .args([
                    "-q", "-D", &device, "-f", "S16_LE", "-r", "16000", "-c", "2", "-t", "raw",
                ])
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .spawn()
                .map_err(|e| DeviceError(format!("cannot run arecord: {e}")))?;
            let stdout = child.stdout.take().expect("stdout is piped");
            let meter = LevelMeter::start(
                stdout,
                16000,
                2,
                vec!["MIC1 (left)".into(), "MIC2 (right)".into()],
            );
            self.meter = Some(Capture { child, meter });
        }
        let capture = self.meter.as_ref().expect("started above");
        let metrics = capture.meter.read();
        if metrics
            .note
            .as_deref()
            .is_some_and(|n| n.starts_with("capture ended"))
        {
            self.meter = None;
            return Err(DeviceError(format!(
                "the capture of {} stopped; is another program using the microphones?",
                self.card
            )));
        }
        Ok(Some(metrics))
    }

    fn stop_metrics(&mut self) {
        self.meter = None;
    }

    fn can_play_tone(&self) -> bool {
        true
    }

    /// A sine of about 5 s with `speaker-test`. Its level follows the
    /// playback and speaker or headphone volumes.
    fn play_tone(&mut self, tone: Tone) -> Result<(), DeviceError> {
        let device = format!("hw:CARD={},DEV=0", self.card);
        let frequency = tone.frequency_hz.to_string();
        let mut args = vec![
            "20",
            "speaker-test",
            "-D",
            &device,
            "-c",
            "2",
            "-r",
            "48000",
            "-t",
            "sine",
            "-f",
            &frequency,
            "-l",
            "1",
        ];
        match tone.channel {
            ToneChannel::Left => args.extend(["-s", "1"]),
            ToneChannel::Right => args.extend(["-s", "2"]),
            ToneChannel::Both => {}
        }
        self.runner.run("timeout", &args)?;
        Ok(())
    }

    /// The values in `/var/lib/alsa/asound.state`, which alsa-restore loads
    /// at boot. `None` before the first store.
    fn read_persisted(&mut self) -> Result<Option<Persisted>, DeviceError> {
        let text = match self.runner.run("cat", &[STATE_FILE]) {
            Ok(text) => text,
            Err(_) => return Ok(None),
        };
        let stored = alsa_state::parse_card(&text, &self.card);
        if stored.is_empty() {
            return Ok(None);
        }
        Ok(Some(Persisted::read(
            stored
                .iter()
                .filter(|c| self.controls.contains_key(&param_id(c)))
                .filter_map(|c| Some((param_id(c), value_of(c)?)))
                .collect(),
        )))
    }

    /// `alsactl store` writes `/var/lib/alsa/asound.state`, which only root
    /// can write. Without root the driver uses `sudo -n`, which fails at once
    /// if sudo needs a password.
    fn persist(&mut self) -> Result<(), DeviceError> {
        let uid = self.runner.run("id", &["-u"])?;
        if uid.trim() == "0" {
            self.runner.run("alsactl", &["store", &self.card])?;
        } else {
            self.runner
                .run("sudo", &["-n", "alsactl", "store", &self.card])?;
        }
        Ok(())
    }
}

fn param_id(control: &Control) -> ParamId {
    ParamId::new(format!("mixer.{}", slug(&control.name)))
}

/// The parameter for a control. `None` for controls that veux does not edit:
/// other interfaces, other types, and multi-channel enums.
fn spec_of(control: &Control) -> Option<ParamSpec> {
    if control.iface != "MIXER" {
        return None;
    }
    let channels = control.values.len();
    let ty = match (&control.ty, channels) {
        (ControlType::Integer { min, max }, 1) => ParamType::Int {
            min: *min,
            max: *max,
        },
        (ControlType::Integer { min, max }, n) => ParamType::Vector {
            len: n,
            min: *min as f64,
            max: *max as f64,
        },
        (ControlType::Boolean, 1) => ParamType::Bool,
        (ControlType::Boolean, n) => ParamType::Vector {
            len: n,
            min: 0.0,
            max: 1.0,
        },
        (ControlType::Enumerated { items }, 1) => ParamType::Enum {
            variants: items.clone(),
        },
        _ => return None,
    };
    let description = match control.db_range {
        Some((min, max)) => format!(
            "{} ({} … {} dB)",
            control.name,
            min as f64 / 100.0,
            max as f64 / 100.0
        ),
        None => control.name.clone(),
    };
    Some(ParamSpec {
        id: param_id(control),
        group: "mixer".into(),
        description,
        ty,
        unit: None,
        access: if control.writable {
            Access::ReadWrite
        } else {
            Access::ReadOnly
        },
        persistent: control.writable,
    })
}

fn value_of(control: &Control) -> Option<Value> {
    let number = |v: &String| -> Option<f64> {
        match v.as_str() {
            "true" => Some(1.0),
            "false" => Some(0.0),
            n => n.parse().ok(),
        }
    };
    match (&control.ty, control.values.as_slice()) {
        (ControlType::Integer { .. }, [v]) => v.parse().ok().map(Value::Int),
        (ControlType::Boolean, [v]) => Some(Value::Bool(v == "true")),
        (ControlType::Enumerated { .. }, [v]) => Some(Value::Enum(v.clone())),
        (ControlType::Integer { .. } | ControlType::Boolean, vs) => vs
            .iter()
            .map(number)
            .collect::<Option<_>>()
            .map(Value::Vector),
        _ => None,
    }
}

/// The value argument of `amixer cset`.
fn cset_text(control: &Control, value: &Value) -> Option<String> {
    let switch = |on: bool| if on { "on" } else { "off" }.to_string();
    match (&control.ty, value) {
        (ControlType::Integer { .. }, Value::Int(v)) => Some(v.to_string()),
        (ControlType::Boolean, Value::Bool(v)) => Some(switch(*v)),
        (ControlType::Enumerated { items }, Value::Enum(v)) if items.contains(v) => Some(v.clone()),
        (ControlType::Integer { .. }, Value::Vector(vs)) => Some(
            vs.iter()
                .map(|v| (*v as i64).to_string())
                .collect::<Vec<_>>()
                .join(","),
        ),
        (ControlType::Boolean, Value::Vector(vs)) => Some(
            vs.iter()
                .map(|v| switch(*v != 0.0))
                .collect::<Vec<_>>()
                .join(","),
        ),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Answers `alsactl` dumps with the reference state and records every
    /// other command.
    #[derive(Default)]
    struct FakeHost {
        uid: &'static str,
        commands: Vec<String>,
        /// The content of the state file, if there is one.
        stored: Option<String>,
    }

    impl Runner for FakeHost {
        fn run(&mut self, program: &str, args: &[&str]) -> Result<String, DeviceError> {
            match (program, args) {
                ("alsactl", ["-f", "-", "store", _]) => Ok(REFERENCE_STATE.to_string()),
                ("id", ["-u"]) => Ok(format!("{}\n", self.uid)),
                ("cat", [STATE_FILE]) => match &self.stored {
                    Some(text) => Ok(text.clone()),
                    None => Err(DeviceError("no such file".into())),
                },
                _ => {
                    self.commands.push(format!("{program} {}", args.join(" ")));
                    Ok(String::new())
                }
            }
        }
    }

    fn mixer(uid: &'static str) -> Mixer<FakeHost> {
        Mixer::with_runner(
            "respeaker2mic",
            FakeHost {
                uid,
                ..Default::default()
            },
        )
        .unwrap()
    }

    struct Sent(std::rc::Rc<std::cell::RefCell<Vec<Vec<u8>>>>);

    impl leds::LedPort for Sent {
        fn send(&mut self, bytes: &[u8]) -> Result<(), DeviceError> {
            self.0.borrow_mut().push(bytes.to_vec());
            Ok(())
        }
    }

    #[test]
    fn led_parameters_exist_only_with_spi_and_go_to_the_leds() {
        let mut m = mixer("1000");
        let power = ParamId::from(leds::POWER);
        assert!(m.schema().get(&power).is_none());
        let on = Values::from([(power.clone(), Value::Bool(true))]);
        assert!(m.write(&on).unwrap_err().0.contains("SPI0"));

        let sent = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        m.leds = Some(leds::Leds::new(Box::new(Sent(sent.clone())), None));
        assert!(m.schema().get(&power).is_some());
        let mut values = on.clone();
        values.insert(
            "mixer.capture_volume".into(),
            Value::Vector(vec![40.0, 40.0]),
        );
        m.write(&values).unwrap();
        assert_eq!(sent.borrow().len(), 1);
        assert_eq!(m.runner.commands.len(), 1, "{:?}", m.runner.commands);
        assert_eq!(m.read_all().unwrap()[&power], Value::Bool(true));
    }

    #[test]
    fn schema_has_typed_mixer_parameters() {
        let schema = kind().schema.unwrap();
        let capture = schema.get(&"mixer.capture_volume".into()).unwrap();
        assert_eq!(
            capture.ty,
            ParamType::Vector {
                len: 2,
                min: 0.0,
                max: 63.0
            }
        );
        assert_eq!(capture.description, "Capture Volume (-17.25 … 30 dB)");
        let boost = schema
            .get(&"mixer.left_input_mixer_boost_switch".into())
            .unwrap();
        assert_eq!(boost.ty, ParamType::Bool);
        let polarity = schema.get(&"mixer.adc_polarity".into()).unwrap();
        assert!(matches!(polarity.ty, ParamType::Enum { .. }));
    }

    #[test]
    fn reads_values_from_the_dump() {
        let values = mixer("1000").read_all().unwrap();
        assert_eq!(
            values[&ParamId::from("mixer.capture_volume")],
            Value::Vector(vec![39.0, 39.0])
        );
        assert_eq!(
            values[&ParamId::from("mixer.left_input_mixer_boost_switch")],
            Value::Bool(false)
        );
        assert_eq!(
            values[&ParamId::from("mixer.adc_polarity")],
            Value::Enum("No Inversion".into())
        );
    }

    #[test]
    fn writes_with_amixer_by_numid() {
        let mut m = mixer("1000");
        let numid = m.controls[&ParamId::from("mixer.left_input_mixer_boost_switch")].numid;
        m.write(&Values::from([
            (
                "mixer.left_input_mixer_boost_switch".into(),
                Value::Bool(true),
            ),
            (
                "mixer.capture_volume".into(),
                Value::Vector(vec![40.0, 41.0]),
            ),
        ]))
        .unwrap();
        let capture = m.controls[&ParamId::from("mixer.capture_volume")].numid;
        assert_eq!(
            m.runner.commands,
            [
                format!("amixer -c respeaker2mic -q cset numid={capture} 40,41"),
                format!("amixer -c respeaker2mic -q cset numid={numid} on"),
            ]
        );
    }

    #[test]
    fn rejects_a_bad_value_without_writing() {
        let mut m = mixer("1000");
        let result = m.write(&Values::from([
            ("mixer.adc_polarity".into(), Value::Enum("Upside".into())),
            ("mixer.capture_volume".into(), Value::Vector(vec![1.0, 1.0])),
        ]));
        assert!(result.is_err());
        assert!(m.runner.commands.is_empty());
    }

    #[test]
    fn reads_persisted_values_from_the_state_file() {
        let mut m = mixer("1000");
        assert_eq!(m.read_persisted().unwrap(), None);
        m.runner.stored = Some(
            REFERENCE_STATE.replace("value.0 39\n\t\tvalue.1 39", "value.0 43\n\t\tvalue.1 43"),
        );
        let stored = m.read_persisted().unwrap().unwrap().values;
        assert_eq!(
            stored[&ParamId::from("mixer.capture_volume")],
            Value::Vector(vec![43.0, 43.0])
        );
        assert_eq!(stored.len(), m.read_all().unwrap().len());
    }

    #[test]
    fn plays_a_tone_on_one_channel() {
        let mut m = mixer("1000");
        m.play_tone(Tone {
            frequency_hz: 440,
            channel: ToneChannel::Right,
        })
        .unwrap();
        assert_eq!(
            m.runner.commands,
            [
                "timeout 20 speaker-test -D hw:CARD=respeaker2mic,DEV=0 -c 2 -r 48000 -t sine -f 440 -l 1 -s 2"
            ]
        );
    }

    #[test]
    fn persists_with_sudo_unless_root() {
        let mut user = mixer("1000");
        user.persist().unwrap();
        assert_eq!(
            user.runner.commands,
            ["sudo -n alsactl store respeaker2mic"]
        );
        let mut root = mixer("0");
        root.persist().unwrap();
        assert_eq!(root.runner.commands, ["alsactl store respeaker2mic"]);
    }
}
