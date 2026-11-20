//! USB DFU 1.1 download, the same steps that `dfu-util -D` takes.
//!
//! The XVF3800 has a factory slot (alt 0), which cannot be written, and an
//! upgrade slot (alt 1). A failed download leaves the upgrade slot invalid,
//! and the board then boots the factory image (Safe Mode). See
//! `docs/devices/xvf3800.md`.

use std::time::Duration;

use thiserror::Error;

const DETACH: u8 = 0;
const DNLOAD: u8 = 1;
const GETSTATUS: u8 = 3;
const CLRSTATUS: u8 = 4;
const ABORT: u8 = 6;

/// DFU states from the DFU 1.1 specification.
pub mod state {
    pub const APP_IDLE: u8 = 0;
    pub const IDLE: u8 = 2;
    pub const DNLOAD_SYNC: u8 = 3;
    pub const DNBUSY: u8 = 4;
    pub const DNLOAD_IDLE: u8 = 5;
    pub const MANIFEST_SYNC: u8 = 6;
    pub const MANIFEST: u8 = 7;
    pub const MANIFEST_WAIT_RESET: u8 = 8;
    pub const ERROR: u8 = 10;
}

/// A device does not need more polls than this for one block or for the
/// manifest phase. The limit stops a loop on a device that never settles.
const MAX_POLLS: u32 = 2000;

#[derive(Debug, Error)]
pub enum DfuError {
    #[error("USB: {0}")]
    Transport(String),
    #[error("device reports DFU error: status {status}, state {state}")]
    Device { status: u8, state: u8 },
    #[error("device stays in state {0} and does not become ready")]
    Stuck(u8),
    #[error("image is empty")]
    EmptyImage,
    #[error("{0}")]
    Setup(String),
}

/// Class requests to the DFU interface.
pub trait DfuPort {
    fn control_out(&mut self, request: u8, value: u16, data: &[u8]) -> Result<(), DfuError>;
    fn control_in(&mut self, request: u8, value: u16, buf: &mut [u8]) -> Result<usize, DfuError>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Status {
    pub status: u8,
    pub poll: Duration,
    pub state: u8,
}

pub fn get_status(port: &mut impl DfuPort) -> Result<Status, DfuError> {
    let mut buf = [0u8; 6];
    let got = port.control_in(GETSTATUS, 0, &mut buf)?;
    if got < 6 {
        return Err(DfuError::Transport(format!(
            "GETSTATUS returned {got} bytes, expected 6"
        )));
    }
    let poll_ms = u32::from_le_bytes([buf[1], buf[2], buf[3], 0]);
    Ok(Status {
        status: buf[0],
        poll: Duration::from_millis(poll_ms.into()),
        state: buf[4],
    })
}

/// Ask a device in run-time mode to switch to DFU mode.
pub fn detach(port: &mut impl DfuPort, timeout_ms: u16) -> Result<(), DfuError> {
    port.control_out(DETACH, timeout_ms, &[])
}

/// Bring the device to the idle state: clear an error, abort a transfer.
fn prepare(port: &mut impl DfuPort) -> Result<(), DfuError> {
    let mut status = get_status(port)?;
    if status.state == state::ERROR {
        port.control_out(CLRSTATUS, 0, &[])?;
        status = get_status(port)?;
    }
    if status.state != state::IDLE {
        port.control_out(ABORT, 0, &[])?;
        status = get_status(port)?;
    }
    if status.state != state::IDLE {
        return Err(DfuError::Setup(format!(
            "device is in state {}, not ready for a download",
            status.state
        )));
    }
    Ok(())
}

/// Poll until the device leaves the busy states. Returns the final status.
fn settle(port: &mut impl DfuPort, sleep: &mut impl FnMut(Duration)) -> Result<Status, DfuError> {
    for _ in 0..MAX_POLLS {
        let status = get_status(port)?;
        match status.state {
            state::ERROR => {
                return Err(DfuError::Device {
                    status: status.status,
                    state: status.state,
                });
            }
            state::DNLOAD_SYNC | state::DNBUSY | state::MANIFEST_SYNC | state::MANIFEST => {
                sleep(status.poll);
            }
            _ => return Ok(status),
        }
    }
    let last = get_status(port)?;
    Err(DfuError::Stuck(last.state))
}

/// Download `image` in blocks of `transfer_size` bytes. `progress` gets the
/// bytes sent so far and the total.
pub fn download(
    port: &mut impl DfuPort,
    image: &[u8],
    transfer_size: usize,
    progress: &mut impl FnMut(usize, usize),
    sleep: &mut impl FnMut(Duration),
) -> Result<(), DfuError> {
    if image.is_empty() {
        return Err(DfuError::EmptyImage);
    }
    prepare(port)?;
    let mut sent = 0;
    let mut block: u16 = 0;
    for chunk in image.chunks(transfer_size) {
        port.control_out(DNLOAD, block, chunk)?;
        let status = settle(port, sleep)?;
        if status.state != state::DNLOAD_IDLE {
            return Err(DfuError::Setup(format!(
                "after block {block} the device is in state {}",
                status.state
            )));
        }
        sent += chunk.len();
        progress(sent, image.len());
        block = block.wrapping_add(1);
    }
    // A zero-length download ends the transfer and starts the manifest.
    port.control_out(DNLOAD, block, &[])?;
    match settle(port, sleep) {
        Ok(status)
            if matches!(
                status.state,
                state::IDLE | state::MANIFEST_WAIT_RESET | state::DNLOAD_IDLE
            ) =>
        {
            Ok(())
        }
        Ok(status) => Err(DfuError::Setup(format!(
            "after the manifest the device is in state {}",
            status.state
        ))),
        // A device that resets itself at the end of the manifest is gone
        // before it can answer.
        Err(DfuError::Transport(_)) => Ok(()),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A DFU device that follows the state machine of the specification.
    struct FakeDevice {
        state: u8,
        written: Vec<u8>,
        blocks: Vec<u16>,
        busy_polls: u8,
        fail_at_block: Option<u16>,
        manifested: bool,
    }

    impl FakeDevice {
        fn new(state: u8) -> Self {
            Self {
                state,
                written: Vec::new(),
                blocks: Vec::new(),
                busy_polls: 0,
                fail_at_block: None,
                manifested: false,
            }
        }
    }

    impl DfuPort for FakeDevice {
        fn control_out(&mut self, request: u8, value: u16, data: &[u8]) -> Result<(), DfuError> {
            match request {
                DNLOAD if data.is_empty() => self.state = state::MANIFEST_SYNC,
                DNLOAD => {
                    if self.fail_at_block == Some(value) {
                        self.state = state::ERROR;
                        return Ok(());
                    }
                    self.blocks.push(value);
                    self.written.extend_from_slice(data);
                    self.state = state::DNBUSY;
                    self.busy_polls = 2;
                }
                CLRSTATUS | ABORT => self.state = state::IDLE,
                _ => {}
            }
            Ok(())
        }

        fn control_in(&mut self, request: u8, _: u16, buf: &mut [u8]) -> Result<usize, DfuError> {
            assert_eq!(request, GETSTATUS);
            let reported = self.state;
            match self.state {
                state::DNBUSY if self.busy_polls > 1 => self.busy_polls -= 1,
                state::DNBUSY => self.state = state::DNLOAD_IDLE,
                state::MANIFEST_SYNC => self.state = state::MANIFEST,
                state::MANIFEST => {
                    self.manifested = true;
                    self.state = state::IDLE;
                }
                _ => {}
            }
            let status = if reported == state::ERROR { 0x0e } else { 0 };
            buf[..6].copy_from_slice(&[status, 5, 0, 0, reported, 0]);
            Ok(6)
        }
    }

    fn run(device: &mut FakeDevice, image: &[u8]) -> Result<Vec<usize>, DfuError> {
        let mut steps = Vec::new();
        download(
            device,
            image,
            256,
            &mut |sent, _| steps.push(sent),
            &mut |_| {},
        )?;
        Ok(steps)
    }

    #[test]
    fn downloads_all_blocks_and_manifests() {
        let image: Vec<u8> = (0..1000u32).map(|i| i as u8).collect();
        let mut device = FakeDevice::new(state::IDLE);
        let steps = run(&mut device, &image).unwrap();
        assert_eq!(device.written, image);
        assert_eq!(device.blocks, [0, 1, 2, 3]);
        assert_eq!(steps, [256, 512, 768, 1000]);
        assert!(device.manifested);
    }

    #[test]
    fn clears_a_previous_error_first() {
        let mut device = FakeDevice::new(state::ERROR);
        run(&mut device, &[1, 2, 3]).unwrap();
        assert_eq!(device.written, [1, 2, 3]);
    }

    #[test]
    fn reports_a_device_error_during_download() {
        let mut device = FakeDevice::new(state::IDLE);
        device.fail_at_block = Some(2);
        let error = run(&mut device, &[0; 1000]).unwrap_err();
        assert!(
            matches!(
                error,
                DfuError::Device {
                    state: state::ERROR,
                    ..
                }
            ),
            "{error}"
        );
        assert_eq!(device.blocks, [0, 1]);
    }

    #[test]
    fn refuses_an_empty_image() {
        let mut device = FakeDevice::new(state::IDLE);
        assert!(matches!(run(&mut device, &[]), Err(DfuError::EmptyImage)));
        assert!(device.blocks.is_empty());
    }

    #[test]
    fn sleeps_for_the_poll_timeout_the_device_asks_for() {
        let mut device = FakeDevice::new(state::IDLE);
        let mut slept = Duration::ZERO;
        download(&mut device, &[0; 10], 256, &mut |_, _| {}, &mut |d| {
            slept += d
        })
        .unwrap();
        // Two busy polls for the block, and two manifest polls, of 5 ms each.
        assert_eq!(slept, Duration::from_millis(20));
    }
}
