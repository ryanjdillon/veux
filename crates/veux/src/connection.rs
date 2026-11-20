//! Open a device from the config, for the CLI and the TUI.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;
use std::sync::mpsc::Receiver;

use anyhow::{Context, Result, anyhow, bail};
use veux_drivers::esphome::EsphomeDevice;
use veux_drivers::{Device, DeviceSpec, open_local};
use veux_model::{Config, Device as DeviceConfig, Host, Link};
use veux_rpc::RemoteDevice;

use crate::remote::{self, SshSession};

/// Opens a connection to a named device from the config.
pub type Connector = Box<dyn FnMut(&str, &DeviceConfig) -> Result<Box<dyn Device>>>;

/// A connector for the devices in `config`. It keeps one helper session for
/// each remote host, and all devices of the host share it.
pub fn connector(config: &Config) -> Connector {
    let hosts = config.hosts.clone();
    let mut sessions: HashMap<String, Rc<RefCell<SshSession>>> = HashMap::new();
    Box::new(move |name, device| connect(name, device, &hosts, &mut sessions))
}

fn connect(
    name: &str,
    device: &DeviceConfig,
    hosts: &BTreeMap<String, Host>,
    sessions: &mut HashMap<String, Rc<RefCell<SshSession>>>,
) -> Result<Box<dyn Device>> {
    match (device.link, &device.host) {
        (Link::Esphome, _) => Ok(Box::new(esphome(name, device)?)),
        (_, None) => Ok(open_local(&spec(device)).with_context(|| format!("device {name:?}"))?),
        (_, Some(host_name)) => {
            let host = hosts
                .get(host_name)
                .ok_or_else(|| anyhow!("device {name:?}: unknown host {host_name:?}"))?;
            let session = match sessions.get(host_name) {
                Some(session) => session.clone(),
                None => {
                    let session = remote::start(host_name, host)?;
                    sessions.insert(host_name.clone(), session.clone());
                    session
                }
            };
            match RemoteDevice::open(session, spec(device)) {
                Ok(remote) => Ok(Box::new(remote)),
                Err(e) => {
                    // The session can be broken; start a new one next time.
                    sessions.remove(host_name);
                    Err(anyhow!("device {name:?} on host {host_name}: {e}"))
                }
            }
        }
    }
}

fn esphome(name: &str, device: &DeviceConfig) -> Result<EsphomeDevice> {
    let address = device
        .address
        .as_deref()
        .ok_or_else(|| anyhow!("device {name:?} has no address"))?;
    let key = match &device.noise_key_env {
        Some(var) => Some(
            std::env::var(var)
                .with_context(|| format!("device {name:?}: set {var} to the API encryption key"))?,
        ),
        None => None,
    };
    Ok(EsphomeDevice::connect(address, key.as_deref())?)
}

/// What the host of a device needs to open it.
pub fn spec(device: &DeviceConfig) -> DeviceSpec {
    DeviceSpec {
        kind: device.kind.clone(),
        link: device.link,
        selector: device.selector.clone(),
    }
}

/// Where a device's firmware is flashed: on this machine, by the helper on
/// its host, or by an ESPHome device from the image in its build.
pub enum FirmwareTarget {
    Local(DeviceSpec),
    Remote {
        host: String,
        session: Rc<RefCell<SshSession>>,
        spec: DeviceSpec,
    },
    Esphome(Box<RefCell<EsphomeDevice>>),
}

pub fn firmware_target(config: &Config, name: &str) -> Result<FirmwareTarget> {
    let device = config
        .devices
        .get(name)
        .ok_or_else(|| anyhow!("unknown device {name:?}"))?;
    if device.link == Link::Esphome {
        let device = esphome(name, device)?;
        return Ok(FirmwareTarget::Esphome(Box::new(RefCell::new(device))));
    }
    Ok(match &device.host {
        None => FirmwareTarget::Local(spec(device)),
        Some(host_name) => {
            let host = config
                .hosts
                .get(host_name)
                .ok_or_else(|| anyhow!("device {name:?}: unknown host {host_name:?}"))?;
            FirmwareTarget::Remote {
                host: host_name.clone(),
                session: remote::start(host_name, host)?,
                spec: spec(device),
            }
        }
    })
}

impl FirmwareTarget {
    pub fn info(&self) -> Result<veux_drivers::FirmwareInfo> {
        Ok(match self {
            Self::Local(spec) => veux_drivers::firmware_info(spec)?,
            Self::Remote { session, spec, .. } => {
                session.borrow_mut().firmware_info(spec.clone())?
            }
            Self::Esphome(device) => device.borrow().firmware_info()?,
        })
    }

    /// Write `image`. An ESPHome device writes the image of its build and
    /// takes no image.
    pub fn flash(&self, image: &[u8], progress: &mut dyn FnMut(usize, usize)) -> Result<()> {
        match self {
            Self::Esphome(_) if !image.is_empty() => {
                bail!("the ESP32 writes the XVF3800 image of its ESPHome build; veux sends none")
            }
            Self::Esphome(device) => device.borrow_mut().flash_bundled(progress)?,
            Self::Local(spec) => veux_drivers::flash_firmware(spec, image, progress)?,
            Self::Remote { session, spec, .. } => {
                session.borrow_mut().flash(spec.clone(), image)?
            }
        }
        Ok(())
    }

    pub fn place(&self) -> String {
        match self {
            Self::Local(_) => "this machine".into(),
            Self::Remote { host, .. } => format!("host {host}"),
            Self::Esphome(_) => "the ESP32".into(),
        }
    }
}

/// What a flash in a thread reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FlashEvent {
    Progress { sent: usize, total: usize },
    Done(Result<(), String>),
}

/// Firmware facts and flashes for the TUI, by device name.
pub trait Firmware {
    fn info(&mut self, name: &str) -> Result<veux_drivers::FirmwareInfo>;
    /// Start a flash in a thread. The last event is `Done`.
    fn start_flash(&mut self, name: &str, image: Vec<u8>) -> Receiver<FlashEvent>;
}

/// Firmware access for the devices in a config.
pub struct ConfigFirmware(pub Config);

impl Firmware for ConfigFirmware {
    fn info(&mut self, name: &str) -> Result<veux_drivers::FirmwareInfo> {
        firmware_target(&self.0, name)?.info()
    }

    /// The thread opens its own target: a helper session cannot move
    /// between threads.
    fn start_flash(&mut self, name: &str, image: Vec<u8>) -> Receiver<FlashEvent> {
        let (tx, rx) = std::sync::mpsc::channel();
        let config = self.0.clone();
        let name = name.to_string();
        std::thread::spawn(move || {
            let result = firmware_target(&config, &name).and_then(|target| {
                target.flash(&image, &mut |sent, total| {
                    let _ = tx.send(FlashEvent::Progress { sent, total });
                })
            });
            let _ = tx.send(FlashEvent::Done(result.map_err(|e| format!("{e:#}"))));
        });
        rx
    }
}
