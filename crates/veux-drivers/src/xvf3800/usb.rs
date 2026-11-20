//! `rusb` access to an XVF3800 that runs the USB firmware.

use std::time::Duration;

use rusb::{Device, DeviceHandle, GlobalContext};

use super::DriverError;
use super::dfu::{self, DfuError, DfuPort};
use super::protocol::{ControlPort, Error};

pub const VENDOR_ID: u16 = 0x2886;
pub const PRODUCT_ID: u16 = 0x001A;

const REQUEST_IN: u8 = 0xC0;
const REQUEST_OUT: u8 = 0x40;
const REQUEST: u8 = 0;
const TIMEOUT: Duration = Duration::from_millis(500);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsbDeviceInfo {
    pub bus: u8,
    pub address: u8,
    /// `None` when the device cannot be opened, for example without a udev
    /// rule.
    pub serial: Option<String>,
}

pub struct UsbPort {
    handle: DeviceHandle<GlobalContext>,
    serial: Option<String>,
}

impl UsbPort {
    /// The USB serial number, when the device has one.
    pub fn serial(&self) -> Option<&str> {
        self.serial.as_deref()
    }
}

impl ControlPort for UsbPort {
    fn control_in(&mut self, value: u16, index: u16, buf: &mut [u8]) -> Result<usize, Error> {
        self.handle
            .read_control(REQUEST_IN, REQUEST, value, index, buf, TIMEOUT)
            .map_err(transport)
    }

    fn control_out(&mut self, value: u16, index: u16, data: &[u8]) -> Result<(), Error> {
        self.handle
            .write_control(REQUEST_OUT, REQUEST, value, index, data, TIMEOUT)
            .map(drop)
            .map_err(transport)
    }
}

fn transport(e: rusb::Error) -> Error {
    Error::Transport(e.to_string())
}

fn matching_devices() -> Result<Vec<Device<GlobalContext>>, DriverError> {
    let devices = rusb::devices().map_err(|e| DriverError::Open(format!("USB: {e}")))?;
    Ok(devices
        .iter()
        .filter(|d| {
            d.device_descriptor()
                .is_ok_and(|desc| desc.vendor_id() == VENDOR_ID && desc.product_id() == PRODUCT_ID)
        })
        .collect())
}

fn serial(device: &Device<GlobalContext>) -> Option<String> {
    let desc = device.device_descriptor().ok()?;
    let handle = device.open().ok()?;
    handle.read_serial_number_string_ascii(&desc).ok()
}

/// List the attached XVF3800 devices.
pub fn list() -> Result<Vec<UsbDeviceInfo>, DriverError> {
    Ok(matching_devices()?
        .iter()
        .map(|d| UsbDeviceInfo {
            bus: d.bus_number(),
            address: d.address(),
            serial: serial(d),
        })
        .collect())
}

/// The one attached XVF3800, or the one with the given serial number.
fn select(serial_number: Option<&str>) -> Result<Device<GlobalContext>, DriverError> {
    let mut candidates = matching_devices()?;
    if let Some(wanted) = serial_number {
        candidates.retain(|d| serial(d).as_deref() == Some(wanted));
    }
    match candidates.len() {
        1 => Ok(candidates.remove(0)),
        0 => Err(DriverError::Open(format!(
            "no XVF3800 found ({VENDOR_ID:04x}:{PRODUCT_ID:04x}{})",
            serial_number.map_or(String::new(), |s| format!(", serial {s}"))
        ))),
        n => Err(DriverError::Open(format!(
            "{n} XVF3800 devices found: set match.serial in the device config"
        ))),
    }
}

fn open_handle(device: &Device<GlobalContext>) -> Result<DeviceHandle<GlobalContext>, DriverError> {
    device.open().map_err(|e| {
        DriverError::Open(format!(
            "cannot open XVF3800 on bus {} address {}: {e} (check the udev rule)",
            device.bus_number(),
            device.address()
        ))
    })
}

/// Open the one attached XVF3800, or the one with the given serial number.
pub fn open(serial_number: Option<&str>) -> Result<UsbPort, DriverError> {
    let device = select(serial_number)?;
    let handle = open_handle(&device)?;
    Ok(UsbPort {
        serial: serial(&device),
        handle,
    })
}

const DFU_CLASS: u8 = 0xFE;
const DFU_SUBCLASS: u8 = 0x01;
const DFU_FUNCTIONAL_DESCRIPTOR: u8 = 0x21;
const DFU_PROTOCOL_RUNTIME: u8 = 1;
const DFU_WILL_DETACH: u8 = 0x08;
const DFU_TIMEOUT: Duration = Duration::from_secs(5);
/// How long the board can take to come back on USB after a detach.
const REENUMERATE: Duration = Duration::from_secs(10);

/// One DFU alternate setting: a firmware slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DfuSlot {
    pub interface: u8,
    pub alt: u8,
    pub name: String,
    /// 1 in run-time mode, 2 in DFU mode.
    pub protocol: u8,
    pub transfer_size: u16,
    pub detach_timeout: u16,
    pub will_detach: bool,
}

/// The DFU slots that the device offers now.
pub fn dfu_slots(serial_number: Option<&str>) -> Result<Vec<DfuSlot>, DriverError> {
    let device = select(serial_number)?;
    let handle = open_handle(&device)?;
    read_slots(&device, &handle)
}

fn read_slots(
    device: &Device<GlobalContext>,
    handle: &DeviceHandle<GlobalContext>,
) -> Result<Vec<DfuSlot>, DriverError> {
    let usb = |e: rusb::Error| DriverError::Open(format!("USB descriptors: {e}"));
    let config = device.active_config_descriptor().map_err(usb)?;
    let language = handle
        .read_languages(DFU_TIMEOUT)
        .ok()
        .and_then(|l| l.first().copied());
    let mut slots = Vec::new();
    for interface in config.interfaces() {
        for desc in interface.descriptors() {
            if desc.class_code() != DFU_CLASS || desc.sub_class_code() != DFU_SUBCLASS {
                continue;
            }
            let name = language
                .and_then(|l| handle.read_interface_string(l, &desc, DFU_TIMEOUT).ok())
                .unwrap_or_default();
            let (attributes, detach_timeout, transfer_size) =
                functional_descriptor(desc.extra()).unwrap_or((0, 1000, 256));
            slots.push(DfuSlot {
                interface: desc.interface_number(),
                alt: desc.setting_number(),
                name,
                protocol: desc.protocol_code(),
                transfer_size,
                detach_timeout,
                will_detach: attributes & DFU_WILL_DETACH != 0,
            });
        }
    }
    Ok(slots)
}

/// bmAttributes, wDetachTimeOut, and wTransferSize of the DFU functional
/// descriptor in the extra bytes of an interface descriptor.
fn functional_descriptor(extra: &[u8]) -> Option<(u8, u16, u16)> {
    let mut rest = extra;
    while rest.len() >= 2 {
        let len = usize::from(rest[0]);
        if len < 2 || len > rest.len() {
            return None;
        }
        if rest[1] == DFU_FUNCTIONAL_DESCRIPTOR && len >= 7 {
            return Some((
                rest[2],
                u16::from_le_bytes([rest[3], rest[4]]),
                u16::from_le_bytes([rest[5], rest[6]]),
            ));
        }
        rest = &rest[len..];
    }
    None
}

/// The slot to write: the one named "Upgrade". The factory slot (alt 0)
/// cannot be written.
fn upgrade_slot(slots: &[DfuSlot]) -> Result<DfuSlot, DriverError> {
    slots
        .iter()
        .find(|s| s.name.to_lowercase().contains("upgrade"))
        .cloned()
        .ok_or_else(|| {
            let names: Vec<String> = slots
                .iter()
                .map(|s| format!("{} {:?}", s.alt, s.name))
                .collect();
            DriverError::Open(format!(
                "no DFU upgrade slot on the device (slots: {})",
                if names.is_empty() {
                    "none".into()
                } else {
                    names.join(", ")
                }
            ))
        })
}

struct UsbDfuPort<'a> {
    handle: &'a DeviceHandle<GlobalContext>,
    interface: u16,
}

impl DfuPort for UsbDfuPort<'_> {
    fn control_out(&mut self, request: u8, value: u16, data: &[u8]) -> Result<(), DfuError> {
        self.handle
            .write_control(0x21, request, value, self.interface, data, DFU_TIMEOUT)
            .map(drop)
            .map_err(|e| DfuError::Transport(e.to_string()))
    }

    fn control_in(&mut self, request: u8, value: u16, buf: &mut [u8]) -> Result<usize, DfuError> {
        self.handle
            .read_control(0xA1, request, value, self.interface, buf, DFU_TIMEOUT)
            .map_err(|e| DfuError::Transport(e.to_string()))
    }
}

/// Write `image` to the upgrade slot, then reset the device. If the device
/// runs its application, it is first switched to DFU mode.
pub fn flash(
    serial_number: Option<&str>,
    image: &[u8],
    progress: &mut dyn FnMut(usize, usize),
) -> Result<(), DriverError> {
    let dfu = |e: DfuError| DriverError::Open(format!("DFU: {e}"));
    let usb = |e: rusb::Error| DriverError::Open(format!("DFU: {e}"));
    let mut device = select(serial_number)?;
    let mut handle = open_handle(&device)?;
    let mut slot = upgrade_slot(&read_slots(&device, &handle)?)?;

    if slot.protocol == DFU_PROTOCOL_RUNTIME {
        handle.claim_interface(slot.interface).map_err(usb)?;
        let mut port = UsbDfuPort {
            handle: &handle,
            interface: slot.interface.into(),
        };
        dfu::detach(&mut port, slot.detach_timeout).map_err(dfu)?;
        let _ = handle.release_interface(slot.interface);
        if !slot.will_detach {
            let _ = handle.reset();
        }
        drop(handle);
        let deadline = std::time::Instant::now() + REENUMERATE;
        loop {
            std::thread::sleep(Duration::from_millis(500));
            let found = select(serial_number).and_then(|d| {
                let h = open_handle(&d)?;
                let s = upgrade_slot(&read_slots(&d, &h)?)?;
                Ok((d, h, s))
            });
            match found {
                Ok((d, h, s)) if s.protocol != DFU_PROTOCOL_RUNTIME => {
                    (device, handle, slot) = (d, h, s);
                    break;
                }
                _ if std::time::Instant::now() > deadline => {
                    return Err(DriverError::Open(
                        "the device did not come back in DFU mode after the detach".into(),
                    ));
                }
                _ => {}
            }
        }
    }

    let _ = handle.set_auto_detach_kernel_driver(true);
    handle.claim_interface(slot.interface).map_err(usb)?;
    handle
        .set_alternate_setting(slot.interface, slot.alt)
        .map_err(usb)?;
    let mut port = UsbDfuPort {
        handle: &handle,
        interface: slot.interface.into(),
    };
    dfu::download(
        &mut port,
        image,
        slot.transfer_size.into(),
        &mut |sent, total| progress(sent, total),
        &mut std::thread::sleep,
    )
    .map_err(dfu)?;
    // A USB reset alone does not reboot the XVF3800: it keeps running the
    // old image from RAM. DFU_DETACH after the manifest makes the bootloader
    // start the new image, as `dfu-util -e -R` does. The device can leave
    // the bus before it answers, so errors here are expected.
    let _ = dfu::detach(&mut port, slot.detach_timeout);
    let _ = handle.release_interface(slot.interface);
    let _ = handle.reset();
    drop(device);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_dfu_functional_descriptor() {
        // An unknown descriptor first, then the DFU one: will-detach and
        // download set, detach timeout 1000 ms, transfer size 256.
        let extra = [
            3, 0x24, 0, 9, 0x21, 0x0b, 0xe8, 0x03, 0x00, 0x01, 0x10, 0x01,
        ];
        assert_eq!(functional_descriptor(&extra), Some((0x0b, 1000, 256)));
        assert_eq!(functional_descriptor(&[2, 0x24]), None);
        assert_eq!(functional_descriptor(&[9, 0x21, 1]), None);
    }

    #[test]
    fn upgrade_slot_is_found_by_name() {
        let slot = |alt, name: &str| DfuSlot {
            interface: 3,
            alt,
            name: name.into(),
            protocol: 2,
            transfer_size: 256,
            detach_timeout: 1000,
            will_detach: true,
        };
        let slots = [
            slot(0, "reSpeaker DFU Factory"),
            slot(1, "reSpeaker DFU Upgrade"),
        ];
        assert_eq!(upgrade_slot(&slots).unwrap().alt, 1);
        let error = upgrade_slot(&slots[..1]).unwrap_err().to_string();
        assert!(error.contains("0 \"reSpeaker DFU Factory\""), "{error}");
    }
}
