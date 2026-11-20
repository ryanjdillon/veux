//! ReSpeaker XVF3800 4-mic array. See `docs/devices/xvf3800.md`.

pub mod commands;
pub mod dfu;
pub mod protocol;
pub mod snapshot;
pub mod usb;

use thiserror::Error;
use veux_model::{Access, KindSpec, Link, ParamId, Schema, Values};

use commands::{COMMANDS, Command, Version, action};
use protocol::{Client, ControlPort, decode, encode};

pub const KIND: &str = "xvf3800";

pub fn kind() -> KindSpec {
    KindSpec {
        id: KIND,
        links: &[Link::Usb, Link::Esphome],
        schema: Some(schema()),
    }
}

/// The schema for the newest firmware. A connected device can support fewer
/// commands; see [`Xvf3800::available`].
pub fn schema() -> Schema {
    Schema::new(COMMANDS.iter().map(Command::to_spec))
}

#[derive(Debug, Error)]
pub enum DriverError {
    #[error(transparent)]
    Protocol(#[from] protocol::Error),
    #[error("read {id}: {source}")]
    Read {
        id: ParamId,
        source: protocol::Error,
    },
    #[error("{0}")]
    Open(String),
    #[error("unknown parameter {0}")]
    UnknownParam(ParamId),
    #[error("parameter {id} is read-only")]
    ReadOnly { id: ParamId },
    #[error("parameter {id} needs firmware {}, the device has {}", fmt_version(*.since), fmt_version(*.version))]
    NotSupported {
        id: ParamId,
        since: Version,
        version: Version,
    },
}

pub fn fmt_version((major, minor, patch): Version) -> String {
    format!("{major}.{minor}.{patch}")
}

pub struct Xvf3800<P> {
    client: Client<P>,
    version: Version,
    /// Where the values of the last commit are recorded.
    snapshot: Option<std::path::PathBuf>,
}

impl<P: ControlPort> Xvf3800<P> {
    /// Connect and read the firmware version.
    pub fn open(client: Client<P>) -> Result<Self, DriverError> {
        let mut client = client;
        let v = commands::VERSION;
        let bytes = client.read(v.resource, v.command, v.payload_len())?;
        Ok(Self {
            client,
            version: (bytes[0], bytes[1], bytes[2]),
            snapshot: None,
        })
    }

    /// A device whose firmware version is known already, so that no read is
    /// necessary.
    pub fn with_version(client: Client<P>, version: Version) -> Self {
        Self {
            client,
            version,
            snapshot: None,
        }
    }

    /// Record the values of each commit in `path`, and report them as the
    /// persisted state.
    pub fn with_snapshot(mut self, path: Option<std::path::PathBuf>) -> Self {
        self.snapshot = path;
        self
    }

    pub fn version(&self) -> Version {
        self.version
    }

    /// The commands that the firmware of this device supports.
    pub fn available(&self) -> impl Iterator<Item = &'static Command> + '_ {
        COMMANDS.iter().filter(|c| c.since <= self.version)
    }

    /// Read the given parameters. No IDs means all available parameters.
    pub fn read(&mut self, ids: &[ParamId]) -> Result<Values, DriverError> {
        let selected: Vec<&Command> = if ids.is_empty() {
            self.available().collect()
        } else {
            ids.iter()
                .map(|id| self.command(id))
                .collect::<Result<_, _>>()?
        };
        let mut values = Values::new();
        for c in selected {
            let bytes = self
                .client
                .read(c.resource, c.command, c.payload_len())
                .map_err(|source| DriverError::Read {
                    id: c.id.into(),
                    source,
                })?;
            values.insert(c.id.into(), decode(c.ty, c.count, &bytes));
        }
        Ok(values)
    }

    /// Write runtime values. This does not persist them.
    pub fn write(&mut self, values: &Values) -> Result<(), DriverError> {
        let mut payloads = Vec::with_capacity(values.len());
        for (id, value) in values {
            let c = self.command(id)?;
            if c.access != Access::ReadWrite {
                return Err(DriverError::ReadOnly { id: id.clone() });
            }
            payloads.push((c, encode(c.ty, c.count, value)?));
        }
        // Encode all values first, so that a bad value writes nothing.
        for (c, bytes) in payloads {
            self.client.write(c.resource, c.command, &bytes)?;
        }
        Ok(())
    }

    /// Save all writable runtime values to flash.
    pub fn persist(&mut self) -> Result<(), DriverError> {
        let (resource, command) = action::SAVE_CONFIGURATION;
        self.client.write(resource, command, &[1])?;
        Ok(())
    }

    fn command(&self, id: &ParamId) -> Result<&'static Command, DriverError> {
        let c = commands::find(id.as_str()).ok_or_else(|| DriverError::UnknownParam(id.clone()))?;
        if c.since > self.version {
            return Err(DriverError::NotSupported {
                id: id.clone(),
                since: c.since,
                version: self.version,
            });
        }
        Ok(c)
    }
}

impl<P: ControlPort> Xvf3800<P> {
    /// Direction of arrival and speech. `doa.value` (firmware 2.0.6 and
    /// later) gives degrees and a speech flag; older firmware gives only the
    /// processed azimuth, which is NaN without speech.
    pub fn direction(&mut self) -> Result<crate::metrics::Metrics, DriverError> {
        use crate::metrics::Metrics;
        use veux_model::Value;
        let doa = ParamId::from("doa.value");
        if self.available().any(|c| c.id == "doa.value") {
            let values = self.read(std::slice::from_ref(&doa))?;
            if let Some(Value::Vector(v)) = values.get(&doa)
                && let [deg, speech] = v.as_slice()
            {
                return Ok(Metrics {
                    direction_deg: Some(*deg),
                    speech: Some(*speech != 0.0),
                    ..Default::default()
                });
            }
        }
        let azimuths = ParamId::from("audio_mgr.selected_azimuths");
        let values = self.read(std::slice::from_ref(&azimuths))?;
        let processed = match values.get(&azimuths) {
            Some(Value::Vector(v)) => v.first().copied(),
            _ => None,
        };
        Ok(Metrics {
            direction_deg: processed
                .filter(|r| r.is_finite())
                .map(|r| r.to_degrees().rem_euclid(360.0)),
            speech: processed.map(f64::is_finite),
            ..Default::default()
        })
    }
}

impl<P: ControlPort> crate::Device for Xvf3800<P> {
    fn describe(&self) -> String {
        format!("firmware {}", fmt_version(self.version))
    }

    /// Only the commands that this firmware supports.
    fn schema(&self) -> Schema {
        Schema::new(self.available().map(Command::to_spec))
    }

    fn read_all(&mut self) -> Result<Values, crate::DeviceError> {
        self.read(&[]).map_err(crate::DeviceError::from_display)
    }

    fn write(&mut self, values: &Values) -> Result<(), crate::DeviceError> {
        Xvf3800::write(self, values).map_err(crate::DeviceError::from_display)
    }

    fn persist(&mut self) -> Result<(), crate::DeviceError> {
        Xvf3800::persist(self).map_err(crate::DeviceError::from_display)?;
        if let Some(path) = self.snapshot.clone() {
            let values = self.read(&[]).map_err(crate::DeviceError::from_display)?;
            let firmware = fmt_version(self.version);
            snapshot::save(&path, &firmware, &crate::Device::schema(self), &values).map_err(
                |e| crate::DeviceError(format!("saved, but cannot record {}: {e}", path.display())),
            )?;
        }
        Ok(())
    }

    /// The snapshot of the last commit: the device cannot report what it
    /// saved.
    fn read_persisted(&mut self) -> Result<Option<crate::Persisted>, crate::DeviceError> {
        let firmware = fmt_version(self.version);
        let schema = crate::Device::schema(self);
        Ok(self
            .snapshot
            .as_deref()
            .and_then(|path| snapshot::load(path, &firmware, &schema))
            .map(|values| crate::Persisted {
                values,
                snapshot: true,
            }))
    }

    fn metrics(&mut self) -> Result<Option<crate::metrics::Metrics>, crate::DeviceError> {
        self.direction()
            .map(Some)
            .map_err(crate::DeviceError::from_display)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::time::Duration;

    use veux_model::Value;

    use super::protocol::Error;
    use super::*;

    /// Emulates the device as a map from (resource, command) to bytes.
    #[derive(Default)]
    struct FakeDevice {
        registers: HashMap<(u8, u8), Vec<u8>>,
        writes: Vec<(u8, u8, Vec<u8>)>,
    }

    impl ControlPort for FakeDevice {
        fn control_in(&mut self, value: u16, index: u16, buf: &mut [u8]) -> Result<usize, Error> {
            let key = (index as u8, (value & 0x7F) as u8);
            match self.registers.get(&key) {
                Some(bytes) => {
                    buf[0] = 0;
                    buf[1..=bytes.len()].copy_from_slice(bytes);
                    Ok(bytes.len() + 1)
                }
                None => {
                    buf[0] = 2;
                    Ok(1)
                }
            }
        }

        fn control_out(&mut self, value: u16, index: u16, data: &[u8]) -> Result<(), Error> {
            let key = (index as u8, value as u8);
            self.writes.push((key.0, key.1, data.to_vec()));
            self.registers.insert(key, data.to_vec());
            Ok(())
        }
    }

    fn device(version: Version) -> Xvf3800<FakeDevice> {
        let mut fake = FakeDevice::default();
        fake.registers
            .insert((48, 0), vec![version.0, version.1, version.2]);
        for c in COMMANDS.iter().filter(|c| c.id != "app.version") {
            fake.registers
                .insert((c.resource, c.command), vec![0; c.payload_len()]);
        }
        Xvf3800::open(Client::new(fake).with_retry(Duration::ZERO, 0)).unwrap()
    }

    fn ids(list: &[&str]) -> Vec<ParamId> {
        list.iter().map(|s| ParamId::from(*s)).collect()
    }

    #[test]
    fn direction_comes_from_doa_value() {
        let mut d = device((2, 1, 1));
        let doa: Vec<u8> = [341u16, 1].iter().flat_map(|v| v.to_le_bytes()).collect();
        d.client.port_mut().registers.insert((20, 18), doa);
        let m = d.direction().unwrap();
        assert_eq!(m.direction_deg, Some(341.0));
        assert_eq!(m.speech, Some(true));
    }

    #[test]
    fn old_firmware_uses_the_azimuth_and_nan_means_no_speech() {
        let mut d = device((2, 0, 5));
        let az: Vec<u8> = [f32::NAN, 1.0]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        d.client.port_mut().registers.insert((35, 11), az);
        let m = d.direction().unwrap();
        assert_eq!(m.direction_deg, None);
        assert_eq!(m.speech, Some(false));
    }

    #[test]
    fn reads_version_on_open() {
        assert_eq!(device((2, 1, 1)).version(), (2, 1, 1));
    }

    #[test]
    fn write_then_read_round_trips() {
        let mut d = device((2, 1, 1));
        let values: Values = [
            ("led.effect".into(), Value::Int(3)),
            ("led.color".into(), Value::Int(0x00FF80)),
            ("pp.agcmaxgain".into(), Value::Float(30.0)),
        ]
        .into();
        d.write(&values).unwrap();
        let read = d
            .read(&ids(&["led.effect", "led.color", "pp.agcmaxgain"]))
            .unwrap();
        assert_eq!(read, values);
    }

    #[test]
    fn hides_commands_newer_than_firmware() {
        let mut d = device((2, 0, 6));
        assert!(d.available().any(|c| c.id == "doa.value"));
        assert!(!d.available().any(|c| c.id == "led.ring_color"));
        assert!(matches!(
            d.read(&ids(&["led.ring_color"])),
            Err(DriverError::NotSupported { .. })
        ));
        let all = d.read(&[]).unwrap();
        assert!(!all.contains_key(&ParamId::from("aic3104.hp_level")));
    }

    #[test]
    fn rejects_read_only_write_without_side_effects() {
        let mut d = device((2, 1, 1));
        let values: Values = [
            ("led.effect".into(), Value::Int(1)),
            ("aec.azimuth_values".into(), Value::Vector(vec![0.0; 4])),
        ]
        .into();
        assert!(matches!(
            d.write(&values),
            Err(DriverError::ReadOnly { .. })
        ));
        assert!(d.client_port().writes.is_empty());
    }

    #[test]
    fn persist_sends_save_configuration() {
        let mut d = device((2, 1, 1));
        d.persist().unwrap();
        assert_eq!(d.client_port().writes, [(48, 9, vec![1])]);
    }

    #[test]
    fn commit_records_the_snapshot_that_read_persisted_returns() {
        use crate::Device;
        let file = std::env::temp_dir().join(format!("veux-xvf-{}.toml", std::process::id()));
        let mut d = device((2, 1, 1)).with_snapshot(Some(file.clone()));
        assert_eq!(d.read_persisted().unwrap(), None);
        Device::persist(&mut d).unwrap();
        let persisted = d.read_persisted().unwrap().unwrap();
        assert!(persisted.snapshot);
        let writable = d
            .schema()
            .params()
            .filter(|s| s.access == Access::ReadWrite)
            .count();
        assert_eq!(persisted.values.len(), writable);
        let _ = std::fs::remove_file(file);
    }

    impl Xvf3800<FakeDevice> {
        fn client_port(&self) -> &FakeDevice {
            self.client.port()
        }
    }
}
