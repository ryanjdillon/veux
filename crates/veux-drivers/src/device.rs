//! One interface to every device, local or behind the helper.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use veux_model::{Link, Schema, Values};

use crate::metrics::{Metrics, Tone};
use crate::{respeaker_2mic_v1, xvf3800};

/// A device error as a message. It is a plain string so that it can cross
/// the helper protocol.
#[derive(Debug, Clone, PartialEq, Eq, Error, Serialize, Deserialize)]
#[error("{0}")]
pub struct DeviceError(pub String);

impl DeviceError {
    pub fn from_display(e: impl std::fmt::Display) -> Self {
        Self(e.to_string())
    }
}

/// The values that a device keeps across a power cycle.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Persisted {
    pub values: Values,
    /// True when veux recorded the values at the last commit, because the
    /// device cannot report them.
    pub snapshot: bool,
}

impl Persisted {
    pub fn read(values: Values) -> Self {
        Self {
            values,
            snapshot: false,
        }
    }
}

/// An open device.
pub trait Device {
    /// A short description, for example the firmware version.
    fn describe(&self) -> String;
    /// The parameters that this device supports.
    fn schema(&self) -> Schema;
    fn read_all(&mut self) -> Result<Values, DeviceError>;
    /// Write runtime values. This does not persist them.
    fn write(&mut self, values: &Values) -> Result<(), DeviceError>;
    /// Make the runtime values survive a power cycle.
    fn persist(&mut self) -> Result<(), DeviceError>;
    /// The values the device keeps across a power cycle. `None` when the
    /// device cannot report them.
    fn read_persisted(&mut self) -> Result<Option<Persisted>, DeviceError> {
        Ok(None)
    }
    /// Live levels, direction, or speech. The first call can start a
    /// capture. `None` when the device has nothing live to report.
    fn metrics(&mut self) -> Result<Option<Metrics>, DeviceError> {
        Ok(None)
    }
    /// End what `metrics` started, for example a capture.
    fn stop_metrics(&mut self) {}
    /// True when `play_tone` works on this device.
    fn can_play_tone(&self) -> bool {
        false
    }
    /// Play a short sine on the device output.
    fn play_tone(&mut self, _tone: Tone) -> Result<(), DeviceError> {
        Err(DeviceError("this device cannot play a test tone".into()))
    }
}

/// What a host needs to open one of its own devices.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceSpec {
    pub kind: String,
    pub link: Link,
    /// Attributes that select one device, for example a USB `serial` or an
    /// ALSA `card`.
    #[serde(default)]
    pub selector: BTreeMap<String, String>,
}

/// Open a device that is attached to this machine.
pub fn open_local(spec: &DeviceSpec) -> Result<Box<dyn Device>, DeviceError> {
    match (spec.kind.as_str(), spec.link) {
        (xvf3800::KIND, Link::Usb) => {
            let port = xvf3800::usb::open(spec.selector.get("serial").map(String::as_str))
                .map_err(DeviceError::from_display)?;
            let snapshot = port.serial().and_then(xvf3800::snapshot::path);
            let device = xvf3800::Xvf3800::open(xvf3800::protocol::Client::new(port))
                .map_err(DeviceError::from_display)?
                .with_snapshot(snapshot);
            Ok(Box::new(device))
        }
        (respeaker_2mic_v1::KIND, Link::Alsa) => {
            let card = spec
                .selector
                .get("card")
                .map_or(respeaker_2mic_v1::DEFAULT_CARD, String::as_str);
            Ok(Box::new(respeaker_2mic_v1::Mixer::open(card)?))
        }
        (kind, link) => Err(DeviceError(format!(
            "kind {kind} with the {} link cannot be opened on this host",
            link.as_str()
        ))),
    }
}

/// What a device reports about its firmware, for the confirmation before a
/// flash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FirmwareInfo {
    /// The running application version, when the device answers.
    pub version: Option<String>,
    /// The DFU slots, as `alt name`.
    pub slots: Vec<String>,
    /// True when only DFU runs (Safe Mode or a bootloader).
    pub dfu_mode: bool,
    /// True when the device writes an image that it holds, for example
    /// the one in an ESPHome build. Then veux sends no image.
    #[serde(default)]
    pub bundled_image: bool,
}

/// Firmware facts of a device attached to this machine.
pub fn firmware_info(spec: &DeviceSpec) -> Result<FirmwareInfo, DeviceError> {
    let serial = usb_xvf3800(spec)?;
    let slots = xvf3800::usb::dfu_slots(serial).map_err(DeviceError::from_display)?;
    let version = xvf3800::usb::open(serial)
        .ok()
        .and_then(|port| xvf3800::Xvf3800::open(xvf3800::protocol::Client::new(port)).ok())
        .map(|d| xvf3800::fmt_version(d.version()));
    // The DFU protocol code does not tell the modes apart: the USB firmware
    // reports DFU mode (2) on its DFU interface while it runs. Only DFU runs
    // when the control interface does not answer.
    let dfu_mode = version.is_none() && !slots.is_empty();
    Ok(FirmwareInfo {
        version,
        slots: slots
            .iter()
            .map(|s| format!("{} {}", s.alt, s.name))
            .collect(),
        dfu_mode,
        bundled_image: false,
    })
}

/// Write a firmware image to a device attached to this machine.
pub fn flash_firmware(
    spec: &DeviceSpec,
    image: &[u8],
    progress: &mut dyn FnMut(usize, usize),
) -> Result<(), DeviceError> {
    let serial = usb_xvf3800(spec)?;
    // The new firmware can lose the saved values, so the snapshot of the
    // last commit is no longer valid.
    if let Some(path) = xvf3800::usb::open(serial)
        .ok()
        .and_then(|port| port.serial().and_then(xvf3800::snapshot::path))
    {
        xvf3800::snapshot::remove(&path);
    }
    xvf3800::usb::flash(serial, image, progress).map_err(DeviceError::from_display)
}

/// The USB serial selector, for the one kind that has firmware updates.
fn usb_xvf3800(spec: &DeviceSpec) -> Result<Option<&str>, DeviceError> {
    if spec.kind != xvf3800::KIND || spec.link != Link::Usb {
        return Err(DeviceError(format!(
            "firmware update needs an xvf3800 on the usb link, not {} on {}",
            spec.kind,
            spec.link.as_str()
        )));
    }
    Ok(spec.selector.get("serial").map(String::as_str))
}
