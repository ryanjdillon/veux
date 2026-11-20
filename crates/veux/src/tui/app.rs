//! TUI state and key handling. No terminal I/O here.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use veux_model::{
    Access, Config, Device as DeviceConfig, ParamId, ParamState, ParamType, Profile, Schema, Value,
    Values, diff,
};

use veux_drivers::metrics::{Metrics, Tone, ToneChannel};
use veux_drivers::{Device, FirmwareInfo};

use crate::connection::{Connector, Firmware, FlashEvent};
use crate::profile::{parse_toml_value, profile_issues, profile_values};

pub enum Status {
    Offline,
    Online(String),
    Failed(String),
}

pub struct DeviceEntry {
    pub name: String,
    pub device: DeviceConfig,
    pub schema: Option<Schema>,
    /// The raw profile. It is parsed again when the device reports its own
    /// schema.
    profile_raw: Option<Profile>,
    pub profile: Values,
    /// Profile entries that the schema does not accept, as `id: reason`.
    pub profile_issues: Vec<String>,
    pub runtime: Values,
    /// What the device keeps across a power cycle, when it can say.
    pub persisted: Option<veux_drivers::Persisted>,
    pub status: Status,
    connection: Option<Box<dyn Device>>,
}

impl DeviceEntry {
    pub fn rows(&self) -> Vec<ParamState> {
        match &self.schema {
            Some(schema) => diff(
                schema,
                &self.profile,
                &self.runtime,
                self.persisted.as_ref().map(|p| &p.values),
            ),
            None => Vec::new(),
        }
    }

    pub fn drift_count(&self) -> usize {
        self.rows().iter().filter(|r| r.drift.any()).count()
    }

    pub fn location(&self) -> String {
        let place = match (&self.device.host, &self.device.address) {
            (Some(host), _) => host.as_str(),
            (None, Some(address)) => address.as_str(),
            (None, None) => "local",
        };
        format!("{} {}", self.device.link.as_str(), place)
    }
}

/// An edit in progress. The form follows the parameter type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Edit {
    /// Pick one of the options. Used for bool and enum parameters.
    Choice { options: Vec<String>, index: usize },
    /// Type a value. Text parameters take the input as it is; numbers and
    /// vectors take a number or a TOML array.
    Text { buffer: String },
}

/// A question that waits for a key.
#[derive(Debug, Clone, PartialEq)]
pub enum Prompt {
    /// Quit was asked while values were unsaved.
    Quit,
    /// Write these profile values to the device.
    Apply(Values),
    /// Persist the runtime values on the device.
    Commit,
    /// Choose the channel of a test tone.
    Tone,
    /// Type the path of a firmware image.
    Image { buffer: String },
    /// Write the checked image. The bytes wait in `App::image`.
    Flash {
        path: String,
        size: usize,
        sha256: String,
    },
    /// Have the device write the image that it holds.
    FlashBundled,
}

/// The image name that the firmware pane shows for `Prompt::FlashBundled`.
pub const BUNDLED_IMAGE: &str = "the image of the ESPHome build";

/// The frequency of the test tone.
pub const TONE_HZ: u32 = 440;

/// How often the monitor reads the device.
const MONITOR_PERIOD: Duration = Duration::from_millis(100);

/// Live levels of one device, read while the monitor is open.
pub struct Monitor {
    pub device: usize,
    pub metrics: Option<Metrics>,
    last_read: Option<Instant>,
}

/// The firmware of one device, shown in place of the detail pane.
pub struct FirmwarePane {
    pub device: usize,
    /// The firmware facts, or why the device has none to show.
    pub info: Result<FirmwareInfo, String>,
    pub flash: Option<FlashState>,
}

pub struct FlashState {
    pub image: String,
    pub sent: usize,
    pub total: usize,
    /// `None` while the flash runs.
    pub result: Option<Result<(), String>>,
    events: Receiver<FlashEvent>,
}

impl FlashState {
    pub fn running(&self) -> bool {
        self.result.is_none()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Devices,
    Params,
}

pub struct App {
    pub devices: Vec<DeviceEntry>,
    pub selected: usize,
    pub param: usize,
    pub focus: Focus,
    pub edit: Option<Edit>,
    pub message: String,
    pub quit: bool,
    /// Values written in this session that are not in `veux.toml`, as
    /// (device index, parameter).
    pub unsaved: BTreeSet<(usize, ParamId)>,
    pub prompt: Option<Prompt>,
    pub monitor: Option<Monitor>,
    pub firmware: Option<FirmwarePane>,
    /// A tone to play on the next tick, after the screen shows it.
    pending_tone: Option<Tone>,
    /// The image that `Prompt::Flash` asks about.
    image: Option<Vec<u8>>,
    config_path: Option<PathBuf>,
    connector: Connector,
    firmware_access: Box<dyn Firmware>,
}

impl App {
    /// `config_path` is the file that `s` saves to. `None` turns saving off.
    pub fn new(
        config: &Config,
        config_path: Option<PathBuf>,
        mut schema_of: impl FnMut(&str) -> Option<Schema>,
        connector: Connector,
        firmware_access: Box<dyn Firmware>,
    ) -> Self {
        let devices = config
            .devices
            .iter()
            .map(|(name, device)| {
                let schema = schema_of(&device.kind);
                let profile_raw = device
                    .profile
                    .as_ref()
                    .and_then(|p| config.profiles.get(p))
                    .cloned();
                let profile = profile_values(profile_raw.as_ref(), schema.as_ref());
                DeviceEntry {
                    name: name.clone(),
                    device: device.clone(),
                    schema,
                    profile_raw,
                    profile,
                    profile_issues: Vec::new(),
                    runtime: Values::new(),
                    persisted: None,
                    status: Status::Offline,
                    connection: None,
                }
            })
            .collect();
        Self {
            devices,
            selected: 0,
            param: 0,
            focus: Focus::Devices,
            edit: None,
            message: String::new(),
            quit: false,
            unsaved: BTreeSet::new(),
            prompt: None,
            monitor: None,
            firmware: None,
            pending_tone: None,
            image: None,
            config_path,
            connector,
            firmware_access,
        }
    }

    pub fn current(&self) -> Option<&DeviceEntry> {
        self.devices.get(self.selected)
    }

    pub fn selected_row(&self) -> Option<ParamState> {
        self.current()?.rows().into_iter().nth(self.param)
    }

    pub fn handle_key(&mut self, key: KeyEvent) {
        if self.flashing() {
            // An interrupted flash can leave the device in DFU mode.
            self.message = "a flash is running; wait for it to end".into();
            return;
        }
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.quit = true;
            return;
        }
        if let Some(Prompt::Image { buffer }) = self.prompt.as_mut() {
            match key.code {
                KeyCode::Char(c) => buffer.push(c),
                KeyCode::Backspace => {
                    buffer.pop();
                }
                KeyCode::Enter => {
                    let path = buffer.clone();
                    self.prompt = None;
                    self.check_image(&path);
                }
                KeyCode::Esc => self.prompt = None,
                _ => {}
            }
            return;
        }
        if let Some(prompt) = self.prompt.clone() {
            match (prompt, key.code) {
                (Prompt::Quit, KeyCode::Char('s')) => {
                    self.prompt = None;
                    self.save_all();
                    // Quit only when the save worked; else show its error.
                    self.quit = self.unsaved.is_empty();
                }
                (Prompt::Quit, KeyCode::Char('q')) => self.quit = true,
                (Prompt::Apply(values), KeyCode::Char('y')) => {
                    self.prompt = None;
                    self.apply(values);
                }
                (Prompt::Commit, KeyCode::Char('y')) => {
                    self.prompt = None;
                    self.commit();
                }
                (Prompt::Tone, KeyCode::Char(c @ ('l' | 'r' | 'b'))) => {
                    self.prompt = None;
                    let channel = match c {
                        'l' => ToneChannel::Left,
                        'r' => ToneChannel::Right,
                        _ => ToneChannel::Both,
                    };
                    self.pending_tone = Some(Tone {
                        frequency_hz: TONE_HZ,
                        channel,
                    });
                    self.message = format!("playing {TONE_HZ} Hz on {c:?}…");
                }
                (Prompt::Flash { path, .. }, KeyCode::Char('y')) => {
                    self.prompt = None;
                    self.start_flash(path);
                }
                (Prompt::FlashBundled, KeyCode::Char('y')) => {
                    self.prompt = None;
                    self.image = Some(Vec::new());
                    self.start_flash(BUNDLED_IMAGE.into());
                }
                (_, KeyCode::Esc | KeyCode::Char('n')) => {
                    self.prompt = None;
                    self.image = None;
                }
                _ => {}
            }
            return;
        }
        if self.edit.is_some() {
            self.handle_edit_key(key);
            return;
        }
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => {
                if self.unsaved.is_empty() {
                    self.quit = true;
                } else {
                    self.prompt = Some(Prompt::Quit);
                }
            }
            KeyCode::Char('a') => self.ask_apply(),
            KeyCode::Char('c') => self.ask_commit(),
            KeyCode::Char('u') => self.revert_selected(),
            KeyCode::Char('s') => self.save_selected(),
            KeyCode::Char('S') => self.save_all(),
            KeyCode::Char('m') => self.toggle_monitor(),
            KeyCode::Char('t') => self.ask_tone(),
            KeyCode::Char('f') => self.toggle_firmware(),
            KeyCode::Char('F') => self.ask_image(),
            KeyCode::Tab | KeyCode::BackTab => {
                self.focus = match self.focus {
                    Focus::Devices => Focus::Params,
                    Focus::Params => Focus::Devices,
                }
            }
            KeyCode::Left | KeyCode::Char('h') => self.focus = Focus::Devices,
            KeyCode::Right | KeyCode::Char('l') => self.focus = Focus::Params,
            KeyCode::Down | KeyCode::Char('j') => self.step(1),
            KeyCode::Up | KeyCode::Char('k') => self.step(-1),
            KeyCode::Char('r') => self.refresh(),
            KeyCode::Enter | KeyCode::Char('e') => self.start_edit(),
            _ => {}
        }
    }

    fn step(&mut self, delta: isize) {
        // A stale message would hide the description of the new row.
        self.message.clear();
        match self.focus {
            Focus::Devices => {
                let selected = clamp_step(self.selected, delta, self.devices.len());
                if selected != self.selected {
                    self.stop_monitor();
                    self.firmware = None;
                }
                self.selected = selected;
                self.param = 0;
            }
            Focus::Params => {
                let len = self.current().map_or(0, |d| d.rows().len());
                self.param = clamp_step(self.param, delta, len);
            }
        }
    }

    /// Connect if necessary, then read all runtime values.
    pub fn refresh(&mut self) {
        let connector = &mut self.connector;
        let Some(entry) = self.devices.get_mut(self.selected) else {
            return;
        };
        if entry.connection.is_none() {
            match connector(&entry.name, &entry.device) {
                Ok(c) => {
                    // The device knows best what it supports, for example
                    // the commands of its firmware version.
                    let schema = c.schema();
                    entry.profile = profile_values(entry.profile_raw.as_ref(), Some(&schema));
                    entry.schema = Some(schema);
                    entry.profile_issues =
                        profile_issues(entry.profile_raw.as_ref(), entry.schema.as_ref());
                    entry.connection = Some(c);
                }
                Err(e) => {
                    entry.status = Status::Failed(format!("{e:#}"));
                    self.message = format!("{}: {e:#}", entry.name);
                    return;
                }
            }
        }
        let connection = entry.connection.as_mut().expect("connected above");
        match connection.read_all() {
            Ok(values) => {
                entry.runtime = values;
                // A device that cannot report it, or fails to, shows "-".
                entry.persisted = connection.read_persisted().ok().flatten();
                entry.status = Status::Online(connection.describe());
                self.message = format!("{}: read {} values", entry.name, entry.runtime.len());
                if !entry.profile_issues.is_empty() {
                    self.message +=
                        &format!("; profile not applied: {}", entry.profile_issues.join(", "));
                }
            }
            Err(e) => {
                entry.status = Status::Failed(format!("{e:#}"));
                entry.connection = None;
                self.message = format!("{}: {e:#}", entry.name);
            }
        }
    }

    fn start_edit(&mut self) {
        if self.focus != Focus::Params {
            self.focus = Focus::Params;
            return;
        }
        let Some(row) = self.selected_row() else {
            return;
        };
        let entry = &self.devices[self.selected];
        if entry.connection.is_none() {
            self.message = "press r to connect first".into();
            return;
        }
        let Some(spec) = entry.schema.as_ref().and_then(|s| s.get(&row.id)) else {
            return;
        };
        if spec.access != Access::ReadWrite {
            self.message = format!("{} is read-only", row.id);
            return;
        }
        let current = row.runtime.as_ref().map(Value::to_string);
        self.edit = Some(match &spec.ty {
            ParamType::Bool => choice(vec!["false".into(), "true".into()], current),
            ParamType::Enum { variants } => choice(variants.clone(), current),
            _ => Edit::Text {
                buffer: current.unwrap_or_default(),
            },
        });
    }

    fn handle_edit_key(&mut self, key: KeyEvent) {
        let Some(edit) = self.edit.as_mut() else {
            return;
        };
        match (edit, key.code) {
            (_, KeyCode::Esc) => self.edit = None,
            (_, KeyCode::Enter) => {
                let edit = self.edit.take().expect("edit is active");
                let input = match edit {
                    Edit::Choice { options, index } => options[index].clone(),
                    Edit::Text { buffer } => buffer,
                };
                if let Err(e) = self.write_selected(&input) {
                    self.message = format!("{e:#}");
                }
            }
            (Edit::Choice { options, index }, KeyCode::Down | KeyCode::Char('j')) => {
                *index = (*index + 1).min(options.len() - 1);
            }
            (Edit::Choice { index, .. }, KeyCode::Up | KeyCode::Char('k')) => {
                *index = index.saturating_sub(1);
            }
            (Edit::Text { buffer }, KeyCode::Backspace) => {
                buffer.pop();
            }
            (Edit::Text { buffer }, KeyCode::Char(c)) => buffer.push(c),
            _ => {}
        }
    }

    /// Write the edit input. A choice gives an option name; a text edit
    /// gives what the user typed.
    fn write_selected(&mut self, input: &str) -> Result<()> {
        let row = self
            .selected_row()
            .ok_or_else(|| anyhow!("no parameter selected"))?;
        let entry = &mut self.devices[self.selected];
        let spec = entry
            .schema
            .as_ref()
            .and_then(|s| s.get(&row.id))
            .ok_or_else(|| anyhow!("unknown parameter {}", row.id))?;
        let raw = match spec.ty {
            ParamType::Bool => toml::Value::Boolean(input == "true"),
            ParamType::Enum { .. } | ParamType::Text { .. } => toml::Value::String(input.into()),
            _ => parse_toml_value(input)?,
        };
        let value = spec.parse(&raw).map_err(|e| anyhow!("{}: {e}", row.id))?;
        let connection = entry
            .connection
            .as_mut()
            .ok_or_else(|| anyhow!("{} is not connected", entry.name))?;
        connection.write(&Values::from([(row.id.clone(), value)]))?;
        self.message = format!(
            "{}: wrote {} (press s to save it to veux.toml)",
            entry.name, row.id
        );
        self.unsaved.insert((self.selected, row.id.clone()));
        self.refresh();
        Ok(())
    }
}

impl App {
    /// Ask to write the profile values that differ from the runtime.
    fn ask_apply(&mut self) {
        let Some(entry) = self.current() else {
            return;
        };
        if entry.connection.is_none() {
            self.message = "press r to connect first".into();
            return;
        }
        let values: Values = entry
            .rows()
            .into_iter()
            .filter(|r| r.drift.apply)
            .filter_map(|r| Some((r.id, r.profile?)))
            .collect();
        if values.is_empty() {
            self.message = format!("{}: nothing to apply", entry.name);
            return;
        }
        self.prompt = Some(Prompt::Apply(values));
    }

    fn ask_commit(&mut self) {
        let Some(entry) = self.current() else {
            return;
        };
        if entry.connection.is_none() {
            self.message = "press r to connect first".into();
            return;
        }
        if entry.device.link == veux_model::Link::Esphome {
            self.message = format!(
                "{}: nothing to commit; the device saves by itself",
                entry.name
            );
            return;
        }
        self.prompt = Some(Prompt::Commit);
    }

    /// Write profile values to the device. These values match the profile
    /// now, so they are no longer unsaved edits.
    fn apply(&mut self, values: Values) {
        let selected = self.selected;
        let entry = &mut self.devices[selected];
        let Some(connection) = entry.connection.as_mut() else {
            return;
        };
        if let Err(e) = connection.write(&values) {
            self.message = format!("{}: apply failed: {e}", entry.name);
            return;
        }
        let name = entry.name.clone();
        for id in values.keys() {
            self.unsaved.remove(&(selected, id.clone()));
        }
        self.refresh();
        self.message = format!(
            "{name}: applied {} value(s); press c to persist",
            values.len()
        );
    }

    fn commit(&mut self) {
        let entry = &mut self.devices[self.selected];
        let Some(connection) = entry.connection.as_mut() else {
            return;
        };
        let name = entry.name.clone();
        if let Err(e) = connection.persist() {
            self.message = format!("{name}: commit failed: {e}");
            return;
        }
        self.refresh();
        self.message = format!("{name}: persisted on the device");
    }

    /// Set the highlighted parameter back to its profile value.
    fn revert_selected(&mut self) {
        let Some(row) = self.selected_row() else {
            return;
        };
        let Some(value) = row.profile.clone().filter(|_| row.drift.apply) else {
            self.message = format!(
                "{}: runtime already matches the profile, or no profile value",
                row.id
            );
            return;
        };
        if self.devices[self.selected].connection.is_none() {
            self.message = "press r to connect first".into();
            return;
        }
        self.apply(Values::from([(row.id, value)]));
    }

    fn ask_tone(&mut self) {
        let Some(entry) = self.current() else {
            return;
        };
        let Some(connection) = &entry.connection else {
            self.message = "press r to connect first".into();
            return;
        };
        if !connection.can_play_tone() {
            self.message = format!("{}: this device cannot play a test tone", entry.name);
            return;
        }
        self.prompt = Some(Prompt::Tone);
    }

    /// Open the monitor of the selected device, or close it.
    fn toggle_monitor(&mut self) {
        if self.monitor.is_some() {
            self.stop_monitor();
            self.message = "monitor closed".into();
            return;
        }
        let Some(entry) = self.current() else {
            return;
        };
        if entry.connection.is_none() {
            self.message = "press r to connect first".into();
            return;
        }
        let name = entry.name.clone();
        self.firmware = None;
        self.monitor = Some(Monitor {
            device: self.selected,
            metrics: None,
            last_read: None,
        });
        self.message = format!("{name}: monitor open; press m to close");
    }

    fn stop_monitor(&mut self) {
        let Some(monitor) = self.monitor.take() else {
            return;
        };
        if let Some(connection) = self.devices[monitor.device].connection.as_mut() {
            connection.stop_metrics();
        }
    }

    /// Show the firmware of the selected device, or close it. The facts are
    /// read when the pane opens.
    fn toggle_firmware(&mut self) {
        if self.firmware.is_some() {
            self.firmware = None;
            self.message = "firmware closed".into();
            return;
        }
        let Some(entry) = self.current() else {
            return;
        };
        let name = entry.name.clone();
        self.stop_monitor();
        let info = self
            .firmware_access
            .info(&name)
            .map_err(|e| format!("{e:#}"));
        self.message = match &info {
            Ok(_) => format!("{name}: firmware; F flashes an image, f closes"),
            Err(_) => format!("{name}: no firmware facts"),
        };
        self.firmware = Some(FirmwarePane {
            device: self.selected,
            info,
            flash: None,
        });
    }

    fn ask_image(&mut self) {
        match &self.firmware {
            Some(FirmwarePane { info: Ok(info), .. }) if info.bundled_image => {
                self.prompt = Some(Prompt::FlashBundled);
            }
            Some(FirmwarePane { info: Ok(_), .. }) => {
                self.prompt = Some(Prompt::Image {
                    buffer: String::new(),
                });
            }
            Some(_) => self.message = "this device has no firmware update here".into(),
            None => self.message = "press f to show the firmware first".into(),
        }
    }

    /// Read the image, and ask with its size and sha256.
    fn check_image(&mut self, path: &str) {
        use sha2::{Digest, Sha256};

        let path = path.trim();
        let file = crate::expand_home(PathBuf::from(path), std::env::var_os("HOME"));
        match std::fs::read(&file) {
            Ok(bytes) if bytes.is_empty() => self.message = format!("{path} is empty"),
            Ok(bytes) => {
                let sha256 = Sha256::digest(&bytes)
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect();
                self.prompt = Some(Prompt::Flash {
                    path: path.to_string(),
                    size: bytes.len(),
                    sha256,
                });
                self.image = Some(bytes);
            }
            Err(e) => self.message = format!("cannot read {path}: {e}"),
        }
    }

    /// Close the device connection, which the flash resets, and start the
    /// flash in a thread.
    fn start_flash(&mut self, path: String) {
        let (Some(pane), Some(image)) = (self.firmware.as_mut(), self.image.take()) else {
            return;
        };
        let entry = &mut self.devices[pane.device];
        if self
            .monitor
            .as_ref()
            .is_some_and(|m| m.device == pane.device)
        {
            self.monitor = None;
        }
        entry.connection = None;
        entry.status = Status::Offline;
        let total = image.len();
        pane.flash = Some(FlashState {
            image: path,
            sent: 0,
            total,
            result: None,
            events: self.firmware_access.start_flash(&entry.name, image),
        });
        self.message = format!("{}: flashing…", entry.name);
    }

    fn flashing(&self) -> bool {
        self.firmware
            .as_ref()
            .and_then(|p| p.flash.as_ref())
            .is_some_and(FlashState::running)
    }

    /// Take the progress of a running flash.
    fn poll_flash(&mut self) {
        let Some(pane) = self.firmware.as_mut() else {
            return;
        };
        let name = &self.devices[pane.device].name;
        let Some(flash) = pane.flash.as_mut().filter(|f| f.running()) else {
            return;
        };
        loop {
            match flash.events.try_recv() {
                Ok(FlashEvent::Progress { sent, total }) => {
                    flash.sent = sent;
                    flash.total = total;
                }
                Ok(FlashEvent::Done(result)) => {
                    self.message = match &result {
                        Ok(()) => format!("{name}: flashed; the device boots the new image"),
                        Err(e) => format!("{name}: flash failed: {e}"),
                    };
                    flash.result = Some(result);
                    return;
                }
                Err(TryRecvError::Empty) => return,
                Err(TryRecvError::Disconnected) => {
                    let e = "the flash thread ended without a result".to_string();
                    self.message = format!("{name}: {e}");
                    flash.result = Some(Err(e));
                    return;
                }
            }
        }
    }

    /// True when the app has work to do without a key: a tone to play, a
    /// monitor to read, or a flash to follow.
    pub fn wants_tick(&self) -> bool {
        self.pending_tone.is_some() || self.monitor.is_some() || self.flashing()
    }

    /// Play a pending tone, follow a flash, and read the monitor when it
    /// is due.
    pub fn tick(&mut self) {
        self.poll_flash();
        if let Some(tone) = self.pending_tone.take() {
            let entry = &mut self.devices[self.selected];
            if let Some(connection) = entry.connection.as_mut() {
                self.message = match connection.play_tone(tone) {
                    Ok(()) => format!("{}: tone done", entry.name),
                    Err(e) => format!("{}: {e}", entry.name),
                };
            }
        }
        let Some(monitor) = self.monitor.as_mut() else {
            return;
        };
        if monitor
            .last_read
            .is_some_and(|t| t.elapsed() < MONITOR_PERIOD)
        {
            return;
        }
        monitor.last_read = Some(Instant::now());
        let entry = &mut self.devices[monitor.device];
        let Some(connection) = entry.connection.as_mut() else {
            self.monitor = None;
            return;
        };
        match connection.metrics() {
            Ok(Some(metrics)) => monitor.metrics = Some(metrics),
            Ok(None) => {
                self.message = format!("{}: the device reports no live levels", entry.name);
                self.monitor = None;
            }
            Err(e) => {
                self.message = format!("{}: {e}", entry.name);
                self.stop_monitor();
            }
        }
    }

    /// The unsaved values, as `device: id`, for the quit prompt.
    pub fn unsaved_names(&self) -> Vec<String> {
        self.unsaved
            .iter()
            .map(|(d, id)| format!("{}: {id}", self.devices[*d].name))
            .collect()
    }

    /// Save the runtime value of the highlighted parameter.
    fn save_selected(&mut self) {
        let Some(row) = self.selected_row() else {
            return;
        };
        if !self.devices[self.selected].runtime.contains_key(&row.id) {
            self.message = format!("{} has no runtime value to save; press r first", row.id);
            return;
        }
        self.save(vec![(self.selected, row.id)]);
    }

    /// Save every value written in this session.
    fn save_all(&mut self) {
        if self.unsaved.is_empty() {
            self.message = "no unsaved edits".into();
            return;
        }
        let rows = self.unsaved.iter().cloned().collect();
        self.save(rows);
    }

    fn save(&mut self, rows: Vec<(usize, ParamId)>) {
        if let Err(e) = self.try_save(rows) {
            self.message = format!("save failed: {e:#}");
        }
    }

    fn try_save(&mut self, rows: Vec<(usize, ParamId)>) -> Result<()> {
        let path = self
            .config_path
            .clone()
            .ok_or_else(|| anyhow!("no config file to save to"))?;
        let mut by_device: std::collections::BTreeMap<usize, Vec<(ParamId, veux_model::Value)>> =
            std::collections::BTreeMap::new();
        for (d, id) in &rows {
            if let Some(v) = self.devices[*d].runtime.get(id) {
                by_device
                    .entry(*d)
                    .or_default()
                    .push((id.clone(), v.clone()));
            }
        }
        let mut notes = Vec::new();
        for (d, values) in by_device {
            let name = self.devices[d].name.clone();
            let profile = crate::config_edit::save_to_file(&path, &name, &values)?;
            let config = Config::load(&path)?;
            let sharing: Vec<&str> = config
                .devices
                .iter()
                .filter(|(n, dev)| *n != &name && dev.profile.as_deref() == Some(profile.as_str()))
                .map(|(n, _)| n.as_str())
                .collect();
            for entry in &mut self.devices {
                if let Some(fresh) = config.devices.get(&entry.name) {
                    entry.device.profile = fresh.profile.clone();
                }
                if entry.device.profile.as_deref() == Some(profile.as_str()) {
                    entry.profile_raw = config.profiles.get(&profile).cloned();
                    entry.profile =
                        profile_values(entry.profile_raw.as_ref(), entry.schema.as_ref());
                    entry.profile_issues =
                        profile_issues(entry.profile_raw.as_ref(), entry.schema.as_ref());
                }
            }
            let mut note = format!("{} to profile {profile}", values.len());
            if !sharing.is_empty() {
                note += &format!(" (also used by {})", sharing.join(", "));
            }
            notes.push(note);
            for (id, _) in &values {
                self.unsaved.remove(&(d, id.clone()));
            }
        }
        self.message = format!("saved {} in {}", notes.join("; "), path.display());
        Ok(())
    }
}

/// A choice with the current value selected, or the first option.
fn choice(options: Vec<String>, current: Option<String>) -> Edit {
    let index = current
        .and_then(|c| options.iter().position(|o| *o == c))
        .unwrap_or(0);
    Edit::Choice { options, index }
}

fn clamp_step(index: usize, delta: isize, len: usize) -> usize {
    if len == 0 {
        return 0;
    }
    index.saturating_add_signed(delta).min(len - 1)
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use veux_model::{Access, ParamSpec, ParamType, Value};

    use super::*;

    /// Stores written values, and returns them on read. `persist` copies
    /// the values, which `read_persisted` then returns.
    struct FakeConnection {
        values: Rc<RefCell<Values>>,
        persisted: RefCell<Option<Values>>,
    }

    impl FakeConnection {
        fn new(values: Rc<RefCell<Values>>) -> Self {
            Self {
                values,
                persisted: RefCell::new(None),
            }
        }
    }

    impl Device for FakeConnection {
        fn describe(&self) -> String {
            "fake".into()
        }

        fn schema(&self) -> Schema {
            schema()
        }

        fn read_all(&mut self) -> Result<Values, veux_drivers::DeviceError> {
            Ok(self.values.borrow().clone())
        }

        fn write(&mut self, values: &Values) -> Result<(), veux_drivers::DeviceError> {
            self.values.borrow_mut().extend(values.clone());
            Ok(())
        }

        fn persist(&mut self) -> Result<(), veux_drivers::DeviceError> {
            *self.persisted.borrow_mut() = Some(self.values.borrow().clone());
            Ok(())
        }

        fn read_persisted(
            &mut self,
        ) -> Result<Option<veux_drivers::Persisted>, veux_drivers::DeviceError> {
            Ok(self
                .persisted
                .borrow()
                .clone()
                .map(veux_drivers::Persisted::read))
        }

        /// Reports the value of g.a as speech, so a test sees each read.
        fn metrics(&mut self) -> Result<Option<Metrics>, veux_drivers::DeviceError> {
            let speech = self.values.borrow().get(&"g.a".into()) == Some(&Value::Int(1));
            Ok(Some(Metrics {
                speech: Some(speech),
                ..Default::default()
            }))
        }

        fn can_play_tone(&self) -> bool {
            true
        }

        fn play_tone(&mut self, tone: Tone) -> Result<(), veux_drivers::DeviceError> {
            let n = match tone.channel {
                ToneChannel::Left => 1,
                ToneChannel::Right => 2,
                ToneChannel::Both => 3,
            };
            self.values.borrow_mut().insert("g.b".into(), Value::Int(n));
            Ok(())
        }
    }

    /// Device "one" has firmware 1.2.3; the other has none. The test sends
    /// the flash events through `events`.
    #[derive(Default, Clone)]
    struct FakeFirmware {
        /// Device "one" holds its image, as an ESPHome device does.
        bundled: bool,
        flashed: Rc<RefCell<Vec<Vec<u8>>>>,
        events: Rc<RefCell<Option<std::sync::mpsc::Sender<FlashEvent>>>>,
    }

    impl Firmware for FakeFirmware {
        fn info(&mut self, name: &str) -> Result<FirmwareInfo> {
            if name != "one" {
                anyhow::bail!("the ESP32 updates it");
            }
            Ok(FirmwareInfo {
                version: Some("1.2.3".into()),
                slots: vec!["0 factory".into(), "1 upgrade".into()],
                dfu_mode: false,
                bundled_image: self.bundled,
            })
        }

        fn start_flash(&mut self, _name: &str, image: Vec<u8>) -> Receiver<FlashEvent> {
            let (tx, rx) = std::sync::mpsc::channel();
            self.flashed.borrow_mut().push(image);
            *self.events.borrow_mut() = Some(tx);
            rx
        }
    }

    /// Rows in ID order: g.a int, g.b read-only int, g.c bool, g.d enum,
    /// g.e text.
    fn schema() -> Schema {
        let spec = |id: &str, ty, access| ParamSpec {
            id: id.into(),
            group: "g".into(),
            description: String::new(),
            ty,
            unit: None,
            access,
            persistent: true,
        };
        let int = || ParamType::Int { min: 0, max: 10 };
        Schema::new([
            spec("g.a", int(), Access::ReadWrite),
            spec("g.b", int(), Access::ReadOnly),
            spec("g.c", ParamType::Bool, Access::ReadWrite),
            spec(
                "g.d",
                ParamType::Enum {
                    variants: vec!["low".into(), "high".into()],
                },
                Access::ReadWrite,
            ),
            spec("g.e", ParamType::Text { max_len: 20 }, Access::ReadWrite),
        ])
    }

    fn app(device_values: Rc<RefCell<Values>>) -> App {
        app_with_firmware(device_values, FakeFirmware::default())
    }

    fn app_with_firmware(device_values: Rc<RefCell<Values>>, firmware: FakeFirmware) -> App {
        let config = Config::parse(
            r#"
            [devices.one]
            kind = "fake"
            link = "usb"
            profile = "p"
            [devices.two]
            kind = "other"
            link = "alsa"
            [profiles.p]
            g.a = 5
            "#,
        )
        .unwrap();
        App::new(
            &config,
            None,
            |kind| (kind == "fake").then(schema),
            Box::new(move |name, _| {
                if name == "one" {
                    Ok(Box::new(FakeConnection::new(device_values.clone())))
                } else {
                    Err(anyhow!("unreachable"))
                }
            }),
            Box::new(firmware),
        )
    }

    fn press(app: &mut App, keys: &str) {
        for c in keys.chars() {
            let code = match c {
                '\n' => KeyCode::Enter,
                '\t' => KeyCode::Tab,
                c => KeyCode::Char(c),
            };
            app.handle_key(KeyEvent::from(code));
        }
    }

    #[test]
    fn refresh_reads_values_and_shows_drift() {
        let values = Rc::new(RefCell::new(Values::from([
            ("g.a".into(), Value::Int(1)),
            ("g.b".into(), Value::Int(2)),
        ])));
        let mut app = app(values);
        assert_eq!(app.devices[0].drift_count(), 0);
        press(&mut app, "r");
        assert!(matches!(app.devices[0].status, Status::Online(_)));
        assert_eq!(app.devices[0].runtime.len(), 2);
        assert_eq!(app.devices[0].drift_count(), 1);
    }

    #[test]
    fn edit_writes_value_and_clears_drift() {
        let values = Rc::new(RefCell::new(Values::from([("g.a".into(), Value::Int(1))])));
        let mut app = app(values.clone());
        press(&mut app, "r\t\n");
        assert_eq!(app.edit, Some(Edit::Text { buffer: "1".into() }));
        app.handle_key(KeyEvent::from(KeyCode::Backspace));
        press(&mut app, "5\n");
        assert_eq!(values.borrow()[&ParamId::from("g.a")], Value::Int(5));
        assert_eq!(app.devices[0].drift_count(), 0);
    }

    #[test]
    fn monitor_reads_until_closed_and_closes_on_device_change() {
        let values = Rc::new(RefCell::new(Values::from([("g.a".into(), Value::Int(1))])));
        let mut app = app(values);
        press(&mut app, "m");
        assert!(app.monitor.is_none(), "needs a connection");
        press(&mut app, "rm");
        assert!(app.wants_tick());
        app.tick();
        let monitor = app.monitor.as_ref().unwrap();
        assert_eq!(monitor.metrics.as_ref().unwrap().speech, Some(true));
        press(&mut app, "j");
        assert!(app.monitor.is_none());
        press(&mut app, "m");
        assert!(app.monitor.is_none(), "two is not connected");
        press(&mut app, "km");
        assert!(app.monitor.is_some());
        press(&mut app, "m");
        assert!(app.monitor.is_none());
    }

    #[test]
    fn tone_plays_on_the_next_tick() {
        let values = Rc::new(RefCell::new(Values::new()));
        let mut app = app(values.clone());
        press(&mut app, "rt");
        assert_eq!(app.prompt, Some(Prompt::Tone));
        press(&mut app, "r");
        assert!(app.prompt.is_none());
        assert!(values.borrow().get(&"g.b".into()).is_none());
        app.tick();
        assert_eq!(values.borrow()[&ParamId::from("g.b")], Value::Int(2));
        assert!(!app.wants_tick());
    }

    #[test]
    fn firmware_pane_shows_facts_or_why_not() {
        let mut app = app(Rc::new(RefCell::new(Values::new())));
        press(&mut app, "f");
        let pane = app.firmware.as_ref().unwrap();
        assert_eq!(
            pane.info.as_ref().unwrap().version.as_deref(),
            Some("1.2.3")
        );
        press(&mut app, "j");
        assert!(app.firmware.is_none(), "a device change closes the pane");
        press(&mut app, "fF");
        assert!(app.firmware.as_ref().unwrap().info.is_err());
        assert!(app.prompt.is_none(), "no flash without firmware facts");
    }

    #[test]
    fn flash_asks_with_size_and_sha256_then_follows_progress() {
        let file = std::env::temp_dir().join(format!("veux-image-{}.bin", std::process::id()));
        std::fs::write(&file, b"abc").unwrap();
        let firmware = FakeFirmware::default();
        let mut app = app_with_firmware(Rc::new(RefCell::new(Values::new())), firmware.clone());
        press(&mut app, "rfF");
        press(&mut app, &format!("{}\n", file.display()));
        let Some(Prompt::Flash { size, sha256, .. }) = app.prompt.clone() else {
            panic!("no flash prompt: {}", app.message);
        };
        assert_eq!(size, 3);
        assert!(sha256.starts_with("ba7816bf"), "{sha256}");
        assert!(firmware.flashed.borrow().is_empty());
        press(&mut app, "y");
        assert_eq!(firmware.flashed.borrow()[0], b"abc");
        assert!(
            app.devices[0].connection.is_none(),
            "the flash resets the device"
        );
        assert!(app.wants_tick());

        let events = firmware.events.borrow().clone().unwrap();
        events
            .send(FlashEvent::Progress { sent: 2, total: 3 })
            .unwrap();
        app.tick();
        assert_eq!(
            app.firmware.as_ref().unwrap().flash.as_ref().unwrap().sent,
            2
        );
        press(&mut app, "q");
        assert!(!app.quit, "quit waits for the flash");
        events.send(FlashEvent::Done(Ok(()))).unwrap();
        app.tick();
        assert!(!app.wants_tick());
        assert!(app.message.contains("flashed"), "{}", app.message);
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(140, 30)).unwrap();
        terminal
            .draw(|f| super::super::ui::draw(f, &app, &super::super::theme::Theme::ligne_claire()))
            .unwrap();
        let screen = format!("{:?}", terminal.backend().buffer());
        for text in ["Firmware", "1.2.3", "1 upgrade", "the device boots"] {
            assert!(screen.contains(text), "{text} not on the screen");
        }
        let _ = std::fs::remove_file(file);
    }

    #[test]
    fn a_device_with_its_own_image_flashes_after_one_question() {
        let firmware = FakeFirmware {
            bundled: true,
            ..Default::default()
        };
        let mut app = app_with_firmware(Rc::new(RefCell::new(Values::new())), firmware.clone());
        press(&mut app, "rfF");
        assert_eq!(app.prompt, Some(Prompt::FlashBundled));
        press(&mut app, "y");
        assert_eq!(firmware.flashed.borrow()[0], Vec::<u8>::new());
        let events = firmware.events.borrow().clone().unwrap();
        events
            .send(FlashEvent::Progress {
                sent: 40,
                total: 100,
            })
            .unwrap();
        events.send(FlashEvent::Done(Ok(()))).unwrap();
        app.tick();
        let flash = app.firmware.as_ref().unwrap().flash.as_ref().unwrap();
        assert_eq!((flash.sent, flash.total), (40, 100));
        assert_eq!(flash.result, Some(Ok(())));
    }

    #[test]
    fn escape_cancels_the_flash_and_drops_the_image() {
        let file = std::env::temp_dir().join(format!("veux-image-esc-{}.bin", std::process::id()));
        std::fs::write(&file, b"abc").unwrap();
        let firmware = FakeFirmware::default();
        let mut app = app_with_firmware(Rc::new(RefCell::new(Values::new())), firmware.clone());
        press(&mut app, "fF");
        press(&mut app, &format!("{}\n", file.display()));
        app.handle_key(KeyEvent::from(KeyCode::Esc));
        assert!(app.prompt.is_none() && app.image.is_none());
        assert!(firmware.flashed.borrow().is_empty());
        let _ = std::fs::remove_file(file);
    }

    #[test]
    fn tone_on_a_device_without_tones_says_so() {
        struct Silent(FakeConnection);
        impl Device for Silent {
            fn describe(&self) -> String {
                self.0.describe()
            }
            fn schema(&self) -> Schema {
                self.0.schema()
            }
            fn read_all(&mut self) -> Result<Values, veux_drivers::DeviceError> {
                self.0.read_all()
            }
            fn write(&mut self, values: &Values) -> Result<(), veux_drivers::DeviceError> {
                self.0.write(values)
            }
            fn persist(&mut self) -> Result<(), veux_drivers::DeviceError> {
                self.0.persist()
            }
        }
        let config = Config::parse("[devices.one]\nkind = \"fake\"\nlink = \"usb\"\n").unwrap();
        let values = Rc::new(RefCell::new(Values::new()));
        let mut app = App::new(
            &config,
            None,
            |kind| (kind == "fake").then(schema),
            Box::new(move |_, _| Ok(Box::new(Silent(FakeConnection::new(values.clone()))))),
            Box::new(FakeFirmware::default()),
        );
        press(&mut app, "rt");
        assert!(app.prompt.is_none());
        assert!(
            app.message.contains("cannot play a test tone"),
            "{}",
            app.message
        );
    }

    #[test]
    fn edit_rejects_out_of_range_value() {
        let values = Rc::new(RefCell::new(Values::from([("g.a".into(), Value::Int(1))])));
        let mut app = app(values.clone());
        press(&mut app, "r\t\n");
        app.handle_key(KeyEvent::from(KeyCode::Backspace));
        press(&mut app, "99\n");
        assert_eq!(values.borrow()[&ParamId::from("g.a")], Value::Int(1));
        assert!(app.message.contains("out of range"), "{}", app.message);
    }

    #[test]
    fn read_only_parameter_cannot_be_edited() {
        let values = Rc::new(RefCell::new(Values::new()));
        let mut app = app(values);
        press(&mut app, "r\tj\n");
        assert!(app.edit.is_none());
        assert!(app.message.contains("read-only"));
    }

    #[test]
    fn device_schema_replaces_missing_kind_schema() {
        let config = Config::parse(
            r#"
            [devices.sat]
            kind = "esp"
            link = "esphome"
            address = "sat.local:6053"
            profile = "p"
            [profiles.p]
            g.a = 5
            g.missing = 1
            "#,
        )
        .unwrap();
        let values = Rc::new(RefCell::new(Values::from([("g.a".into(), Value::Int(2))])));
        let mut app = App::new(
            &config,
            None,
            |_| None,
            Box::new(move |_, _| Ok(Box::new(FakeConnection::new(values.clone())))),
            Box::new(FakeFirmware::default()),
        );
        assert!(app.devices[0].schema.is_none());
        press(&mut app, "r");
        assert!(app.devices[0].schema.is_some());
        assert_eq!(app.devices[0].profile.len(), 1);
        assert_eq!(app.devices[0].drift_count(), 1);
        assert_eq!(app.devices[0].profile_issues, ["g.missing: not on device"]);
        assert!(
            app.message.contains("g.missing: not on device"),
            "{}",
            app.message
        );
    }

    #[test]
    fn bool_edit_is_a_choice_with_the_current_value_selected() {
        let values = Rc::new(RefCell::new(Values::from([(
            "g.c".into(),
            Value::Bool(false),
        )])));
        let mut app = app(values.clone());
        press(&mut app, "r\tjj\n");
        assert_eq!(
            app.edit,
            Some(Edit::Choice {
                options: vec!["false".into(), "true".into()],
                index: 0
            })
        );
        press(&mut app, "j\n");
        assert_eq!(values.borrow()[&ParamId::from("g.c")], Value::Bool(true));
    }

    #[test]
    fn enum_edit_offers_the_variants() {
        let values = Rc::new(RefCell::new(Values::from([(
            "g.d".into(),
            Value::Enum("high".into()),
        )])));
        let mut app = app(values.clone());
        press(&mut app, "r\tjjj\n");
        assert!(matches!(app.edit, Some(Edit::Choice { index: 1, .. })));
        press(&mut app, "kk\n");
        assert_eq!(
            values.borrow()[&ParamId::from("g.d")],
            Value::Enum("low".into())
        );
    }

    #[test]
    fn text_edit_needs_no_quotes() {
        let values = Rc::new(RefCell::new(Values::from([(
            "g.e".into(),
            Value::Text("ab".into()),
        )])));
        let mut app = app(values.clone());
        press(&mut app, "r\tjjjj\n");
        app.handle_key(KeyEvent::from(KeyCode::Backspace));
        app.handle_key(KeyEvent::from(KeyCode::Backspace));
        press(&mut app, "hello world\n");
        assert_eq!(
            values.borrow()[&ParamId::from("g.e")],
            Value::Text("hello world".into())
        );
    }

    #[test]
    fn escape_cancels_a_choice_without_writing() {
        let values = Rc::new(RefCell::new(Values::from([(
            "g.c".into(),
            Value::Bool(false),
        )])));
        let mut app = app(values.clone());
        press(&mut app, "r\tjj\nj");
        app.handle_key(KeyEvent::from(KeyCode::Esc));
        assert!(app.edit.is_none());
        assert!(!app.quit);
        assert_eq!(values.borrow()[&ParamId::from("g.c")], Value::Bool(false));
    }

    const SAVE_CONFIG: &str = r#"# test config
[devices.one]
kind = "fake"
link = "usb"
profile = "p"

[profiles.p]
g.a = 5   # tuned
"#;

    /// An app on a real config file in a fresh temp directory.
    fn app_on_file(values: Rc<RefCell<Values>>) -> (App, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "veux-save-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("veux.toml");
        std::fs::write(&path, SAVE_CONFIG).unwrap();
        let config = Config::parse(SAVE_CONFIG).unwrap();
        let app = App::new(
            &config,
            Some(path.clone()),
            |kind| (kind == "fake").then(schema),
            Box::new(move |_, _| Ok(Box::new(FakeConnection::new(values.clone())))),
            Box::new(FakeFirmware::default()),
        );
        (app, path)
    }

    /// Connect, and set g.a to 7 from the edit line.
    fn edit_a_to_7(app: &mut App) {
        press(app, "r\t\n");
        app.handle_key(KeyEvent::from(KeyCode::Backspace));
        press(app, "7\n");
    }

    #[test]
    fn s_saves_the_runtime_value_into_the_file() {
        let values = Rc::new(RefCell::new(Values::from([("g.a".into(), Value::Int(1))])));
        let (mut app, path) = app_on_file(values);
        edit_a_to_7(&mut app);
        assert_eq!(app.unsaved.len(), 1);
        assert_eq!(app.devices[0].drift_count(), 1);
        press(&mut app, "s");
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("g.a = 7   # tuned"), "{text}");
        assert!(app.unsaved.is_empty(), "{}", app.message);
        assert_eq!(app.devices[0].drift_count(), 0);
        assert!(app.message.contains("to profile p"), "{}", app.message);
    }

    #[test]
    fn quit_with_unsaved_edits_asks_first() {
        let values = Rc::new(RefCell::new(Values::from([("g.a".into(), Value::Int(1))])));
        let (mut app, path) = app_on_file(values);
        edit_a_to_7(&mut app);
        press(&mut app, "q");
        assert!(app.prompt == Some(Prompt::Quit) && !app.quit);
        assert_eq!(app.unsaved_names(), ["one: g.a"]);
        app.handle_key(KeyEvent::from(KeyCode::Esc));
        assert!(app.prompt.is_none() && !app.quit);
        press(&mut app, "qq");
        assert!(app.quit);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), SAVE_CONFIG);
    }

    #[test]
    fn quit_prompt_can_save_and_quit() {
        let values = Rc::new(RefCell::new(Values::from([("g.a".into(), Value::Int(1))])));
        let (mut app, path) = app_on_file(values);
        edit_a_to_7(&mut app);
        press(&mut app, "qs");
        assert!(app.quit);
        assert!(std::fs::read_to_string(&path).unwrap().contains("g.a = 7"));
    }

    #[test]
    fn a_applies_the_profile_after_yes() {
        let values = Rc::new(RefCell::new(Values::from([("g.a".into(), Value::Int(1))])));
        let mut app = app(values.clone());
        press(&mut app, "ra");
        assert_eq!(
            app.prompt,
            Some(Prompt::Apply(Values::from([("g.a".into(), Value::Int(5))])))
        );
        press(&mut app, "y");
        assert_eq!(values.borrow()[&ParamId::from("g.a")], Value::Int(5));
        assert_eq!(app.devices[0].drift_count(), 0);
        assert!(app.prompt.is_none());
    }

    #[test]
    fn escape_cancels_apply_without_writing() {
        let values = Rc::new(RefCell::new(Values::from([("g.a".into(), Value::Int(1))])));
        let mut app = app(values.clone());
        press(&mut app, "ra");
        app.handle_key(KeyEvent::from(KeyCode::Esc));
        assert!(app.prompt.is_none() && !app.quit);
        assert_eq!(values.borrow()[&ParamId::from("g.a")], Value::Int(1));
    }

    #[test]
    fn u_reverts_the_highlighted_value_to_the_profile() {
        let values = Rc::new(RefCell::new(Values::from([("g.a".into(), Value::Int(1))])));
        let mut app = app(values.clone());
        edit_to(&mut app, "7");
        assert_eq!(app.unsaved.len(), 1);
        press(&mut app, "u");
        assert_eq!(values.borrow()[&ParamId::from("g.a")], Value::Int(5));
        assert!(app.unsaved.is_empty());
    }

    #[test]
    fn c_persists_and_the_persisted_column_fills() {
        let values = Rc::new(RefCell::new(Values::from([("g.a".into(), Value::Int(5))])));
        let mut app = app(values);
        press(&mut app, "r");
        assert!(app.devices[0].persisted.is_none());
        press(&mut app, "cy");
        let persisted = app.devices[0].persisted.clone().unwrap();
        assert_eq!(persisted.values[&ParamId::from("g.a")], Value::Int(5));
        assert!(app.message.contains("persisted"), "{}", app.message);
    }

    /// Connect and set g.a from the edit line.
    fn edit_to(app: &mut App, value: &str) {
        press(app, "r\t\n");
        app.handle_key(KeyEvent::from(KeyCode::Backspace));
        press(app, &format!("{value}\n"));
    }

    #[test]
    fn failed_connection_is_reported() {
        let mut app = app(Rc::new(RefCell::new(Values::new())));
        press(&mut app, "jr");
        assert!(matches!(app.devices[1].status, Status::Failed(_)));
    }
}
