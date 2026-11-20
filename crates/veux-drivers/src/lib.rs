//! Device drivers. Each module is one device kind.

use veux_model::KindSpec;

pub mod device;
pub mod esphome;
pub mod explain;
pub mod metrics;
pub mod respeaker_2mic_v1;
pub mod slug;
pub mod xvf3800;

pub use device::{
    Device, DeviceError, DeviceSpec, FirmwareInfo, Persisted, firmware_info, flash_firmware,
    open_local,
};

/// All device kinds that veux supports.
pub fn catalog() -> Vec<KindSpec> {
    vec![xvf3800::kind(), respeaker_2mic_v1::kind()]
}
