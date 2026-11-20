//! ESPHome devices over the native API (TCP 6053, optional Noise
//! encryption). See `docs/devices/esphome.md`.

pub mod entities;
pub mod tone;

use std::collections::BTreeMap;
use std::time::Duration;

use esphome_client::EspHomeClient;
use esphome_client::types::{
    EspHomeMessage, ExecuteServiceArgument, ExecuteServiceRequest, ListEntitiesRequest,
    MediaPlayerCommandRequest, SubscribeStatesRequest,
};
use thiserror::Error;
use tokio::runtime::Runtime;
use tokio::time::timeout;
use veux_model::{ParamId, ParamSpec, Schema, Values};

use crate::xvf3800::Xvf3800;
use crate::xvf3800::commands::Version;
use crate::xvf3800::protocol::Client;

use entities::Catalog;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// The device sends all states after the subscription. A silence of this
/// length ends the burst.
const STATE_QUIET: Duration = Duration::from_millis(300);
const READ_LIMIT: Duration = Duration::from_secs(5);
const ACTION_LIMIT: Duration = Duration::from_secs(5);

/// The raw XVF3800 actions of the raw parameter actions. See `docs/devices/esphome.md`.
pub const XVF_GET: &str = "xvf_get";
pub const XVF_SET: &str = "xvf_set";
pub const XVF_FLASH: &str = "xvf_flash";
/// The entities that report the update of `xvf_flash`.
const UPDATE_STATE: &str = "text_sensor.xvf3800_update";
const UPDATE_PROGRESS: &str = "sensor.xvf3800_update_progress";
/// How long the update has to start, and to end.
const UPDATE_START: Duration = Duration::from_secs(15);
const UPDATE_LIMIT: Duration = Duration::from_secs(15 * 60);

/// Raw XVF3800 parameters that the formatBCE `respeaker_xvf3800` component
/// writes from its own entities, again at boot and after an XVF3800 reset.
/// A raw write would not last, so veux shows the entity and hides these:
/// the LED ring (`set_led_ring`, 20 times a second), the ASR route and
/// gains (`apply_audio_tuning_`), and the beam lock (`lock_beam`).
const OWNED_BY_COMPONENT: &[&str] = &[
    "aec.asroutonoff",
    "aec.asroutgain",
    "aec.fixedbeamsonoff",
    "aec.fixedbeamsazimuth_values",
    "audio_mgr.op_l",
    "audio_mgr.op_r",
    "audio_mgr.op_all",
    "pp.agcmaxgain",
];
/// Raw parameter groups that the component owns.
const GROUPS_OWNED_BY_COMPONENT: &[&str] = &["led"];
/// Parameters of the USB firmware only. On the I2S firmware the bit depth
/// reads 0, and a write reboots the XVF3800.
const USB_ONLY: &[&str] = &["app.usb_bit_depth"];

#[derive(Debug, Error)]
pub enum EsphomeError {
    #[error("ESPHome {address}: {message}")]
    Client { address: String, message: String },
    #[error("ESPHome {0}: timeout")]
    Timeout(String),
    #[error("unknown entity {0}")]
    UnknownEntity(ParamId),
    #[error("entity {0} is read-only or the value has the wrong type")]
    NotWritable(ParamId),
    #[error("ESPHome {address}: {source}")]
    Raw {
        address: String,
        source: crate::xvf3800::DriverError,
    },
    #[error("ESPHome {address}: action {action}: {message}")]
    Action {
        address: String,
        action: String,
        message: String,
    },
}

pub struct EsphomeDevice {
    address: String,
    runtime: Runtime,
    client: EspHomeClient,
    catalog: Catalog,
    values: Values,
    /// The API actions of the device, by name, as their keys.
    actions: BTreeMap<String, u32>,
    /// The key of the first media player, which plays test tones.
    media_player: Option<u32>,
    next_call: u32,
    /// The XVF3800 firmware version, when the device has the raw actions.
    xvf_version: Option<Version>,
    /// Why the raw actions do not work, when the device has them.
    raw_error: Option<String>,
    /// The raw XVF3800 parameters that this link shows.
    raw_specs: Vec<ParamSpec>,
}

impl EsphomeDevice {
    /// Connect, list the entities, and subscribe to their states.
    ///
    /// `key` is the base64 API encryption key. `None` means a plaintext
    /// connection.
    pub fn connect(address: &str, key: Option<&str>) -> Result<Self, EsphomeError> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| client_error(address, e))?;
        let (client, catalog, values, actions, media_player) = runtime.block_on(async {
            let mut builder = EspHomeClient::builder()
                .address(address)
                .client_info("veux");
            if let Some(key) = key {
                builder = builder.key(key);
            }
            let mut client = timeout(CONNECT_TIMEOUT, builder.connect())
                .await
                .map_err(|_| EsphomeError::Timeout(address.into()))?
                .map_err(|e| client_error(address, e))?;

            client
                .try_write(ListEntitiesRequest {})
                .await
                .map_err(|e| client_error(address, e))?;
            let mut catalog = Catalog::default();
            let mut actions = BTreeMap::new();
            let mut media_player = None;
            loop {
                let message = timeout(CONNECT_TIMEOUT, client.try_read())
                    .await
                    .map_err(|_| EsphomeError::Timeout(address.into()))?
                    .map_err(|e| client_error(address, e))?;
                match message {
                    EspHomeMessage::ListEntitiesDoneResponse(_) => break,
                    EspHomeMessage::ListEntitiesServicesResponse(a) => {
                        actions.insert(a.name, a.key);
                    }
                    EspHomeMessage::ListEntitiesMediaPlayerResponse(ref m) => {
                        media_player.get_or_insert(m.key);
                        catalog.add(&message);
                    }
                    message => {
                        catalog.add(&message);
                    }
                }
            }

            client
                .try_write(SubscribeStatesRequest {})
                .await
                .map_err(|e| client_error(address, e))?;
            let mut values = Values::new();
            drain_states(&mut client, &catalog, &mut values, address).await?;
            Ok::<_, EsphomeError>((client, catalog, values, actions, media_player))
        })?;
        let mut device = Self {
            address: address.into(),
            runtime,
            client,
            catalog,
            values,
            actions,
            media_player,
            next_call: 1,
            xvf_version: None,
            raw_error: None,
            raw_specs: Vec::new(),
        };
        if device.actions.contains_key(XVF_GET) && device.actions.contains_key(XVF_SET) {
            // The entities work without the raw actions, so a failure only
            // shows in the description.
            match Xvf3800::open(Client::new(&mut device)) {
                Ok(xvf) => {
                    let version = xvf.version();
                    device.raw_specs = raw_specs(&xvf);
                    device.xvf_version = Some(version);
                }
                Err(e) => device.raw_error = Some(e.to_string()),
            }
        }
        Ok(device)
    }

    /// Read one XVF3800 command through the `xvf_get` action: the status
    /// byte, then `len` payload bytes. The ESP32 sends one I2C read; the
    /// caller handles the status and retries, as on USB.
    pub fn xvf_get(&mut self, resid: u8, cmd: u8, len: usize) -> Result<Vec<u8>, EsphomeError> {
        let args = [int(resid.into()), int(cmd.into()), int(len as i32)];
        let data = self.call_action(XVF_GET, args.into())?;
        parse_reply(&data).map_err(|message| EsphomeError::Action {
            address: self.address.clone(),
            action: XVF_GET.into(),
            message,
        })
    }

    /// Write the payload of one XVF3800 command through the `xvf_set`
    /// action. This changes a runtime value only.
    pub fn xvf_set(&mut self, resid: u8, cmd: u8, payload: &[u8]) -> Result<(), EsphomeError> {
        // A hex string: the ESPHome array argument cannot go through
        // `api.respond` on the device.
        let bytes = ExecuteServiceArgument {
            string: payload.iter().map(|b| format!("{b:02x}")).collect(),
            ..Default::default()
        };
        let args = vec![int(resid.into()), int(cmd.into()), bytes];
        self.call_action(XVF_SET, args).map(|_| ())
    }

    /// Play a tone on the media player: serve a FLAC on `tone::PORT`, send
    /// its URL, and wait until the device fetched it and the tone ended.
    pub fn play_tone(&mut self, tone: crate::metrics::Tone) -> Result<(), EsphomeError> {
        let address = self.address.clone();
        let error = |message: String| EsphomeError::Client {
            address: address.clone(),
            message,
        };
        let key = self
            .media_player
            .ok_or_else(|| error("the device has no media player".into()))?;
        let ip = tone::local_ip_for(&self.address).map_err(|e| error(e.to_string()))?;
        let listener = std::net::TcpListener::bind(("0.0.0.0", tone::PORT))
            .map_err(|e| error(format!("cannot listen on port {}: {e}", tone::PORT)))?;
        let fetched = tone::serve_once(listener, tone::flac(tone, tone::SECONDS));
        let channel = format!("{:?}", tone.channel).to_lowercase();
        let url = format!(
            "http://{ip}:{}/veux-tone-{}-{channel}.flac",
            tone::PORT,
            tone.frequency_hz
        );
        let command = MediaPlayerCommandRequest {
            key,
            has_media_url: true,
            media_url: url,
            ..Default::default()
        };
        let Self {
            address,
            runtime,
            client,
            ..
        } = self;
        runtime
            .block_on(client.try_write(command))
            .map_err(|e| client_error(address, e))?;
        fetched
            .recv()
            .map_err(|e| error(e.to_string()))?
            .map_err(error)?;
        std::thread::sleep(Duration::from_secs_f64(tone::SECONDS));
        Ok(())
    }

    /// Run an action, and wait for its response. State messages that
    /// arrive first update the values. The arguments are in the order of
    /// the action's variables.
    fn call_action(
        &mut self,
        name: &str,
        args: Vec<ExecuteServiceArgument>,
    ) -> Result<Vec<u8>, EsphomeError> {
        let address = self.address.clone();
        let action_error = |message: String| EsphomeError::Action {
            address: address.clone(),
            action: name.into(),
            message,
        };
        let key = *self
            .actions
            .get(name)
            .ok_or_else(|| action_error("the device has no such action".into()))?;
        let call_id = self.next_call;
        self.next_call = self.next_call.wrapping_add(1).max(1);
        let request = ExecuteServiceRequest {
            key,
            args,
            call_id,
            return_response: true,
        };
        let Self {
            address,
            runtime,
            client,
            catalog,
            values,
            ..
        } = self;
        let reply = runtime.block_on(async {
            client
                .try_write(request)
                .await
                .map_err(|e| client_error(address, e))?;
            let deadline = tokio::time::Instant::now() + ACTION_LIMIT;
            loop {
                let message = tokio::time::timeout_at(deadline, client.try_read())
                    .await
                    .map_err(|_| EsphomeError::Timeout(address.clone()))?
                    .map_err(|e| client_error(address, e))?;
                match message {
                    EspHomeMessage::ExecuteServiceResponse(r) if r.call_id == call_id => {
                        return Ok(r);
                    }
                    message => {
                        catalog.apply_state(&message, values);
                    }
                }
            }
        })?;
        if !reply.success {
            return Err(action_error(reply.error_message));
        }
        Ok(reply.response_data)
    }

    /// The entities, and the raw XVF3800 parameters when the device has
    /// the raw actions.
    pub fn schema(&self) -> Schema {
        Schema::new(
            self.catalog
                .schema()
                .params()
                .cloned()
                .chain(self.raw_specs.iter().cloned()),
        )
    }

    /// The firmware facts: the running XVF3800 version, read with the raw
    /// actions. The ESP32 holds the image, so there are no DFU slots.
    pub fn firmware_info(&self) -> Result<crate::FirmwareInfo, EsphomeError> {
        let version = self.xvf_version.ok_or_else(|| EsphomeError::Client {
            address: self.address.clone(),
            message: match &self.raw_error {
                Some(e) => format!("the raw XVF3800 actions failed: {e}"),
                None => "the device has no raw XVF3800 actions (xvf_get, xvf_set); \
                         the ESP32 updates the XVF3800 itself"
                    .into(),
            },
        })?;
        Ok(crate::FirmwareInfo {
            version: Some(crate::xvf3800::fmt_version(version)),
            slots: Vec::new(),
            dfu_mode: false,
            bundled_image: self.actions.contains_key(XVF_FLASH),
        })
    }

    /// Run `xvf_flash`: the ESP32 writes the XVF3800 image of its ESPHome
    /// build. Report the percent done until the update ends.
    pub fn flash_bundled(
        &mut self,
        progress: &mut dyn FnMut(usize, usize),
    ) -> Result<(), EsphomeError> {
        let address = self.address.clone();
        let error = |message: String| EsphomeError::Client {
            address: address.clone(),
            message,
        };
        self.call_action(XVF_FLASH, Vec::new())?;
        let started = std::time::Instant::now();
        let mut running = false;
        loop {
            // Only the entities: the XVF3800 does not answer raw reads
            // during DFU.
            let values = self.read_entities()?;
            let state = match values.get(&ParamId::from(UPDATE_STATE)) {
                Some(veux_model::Value::Text(s)) => s.clone(),
                _ => String::new(),
            };
            if let Some(veux_model::Value::Float(p)) = values.get(&ParamId::from(UPDATE_PROGRESS))
                && running
            {
                progress(p.clamp(0.0, 100.0) as usize, 100);
            }
            match state.as_str() {
                "flashing" => running = true,
                "done" if running => {
                    progress(100, 100);
                    return Ok(());
                }
                s if running && s.starts_with("error") => {
                    return Err(error(format!("the update failed: {s}")));
                }
                _ if !running && started.elapsed() > UPDATE_START => {
                    return Err(error(
                        "the update did not start; see the ESP32 log (firmware invalid?)".into(),
                    ));
                }
                _ => {}
            }
            if started.elapsed() > UPDATE_LIMIT {
                return Err(error("the update did not end in 15 minutes".into()));
            }
            std::thread::sleep(Duration::from_millis(500));
        }
    }

    /// The XVF3800 driver over the raw actions.
    fn raw(&mut self) -> Option<Xvf3800<&mut Self>> {
        let version = self.xvf_version?;
        Some(Xvf3800::with_version(Client::new(self), version))
    }

    fn raw_error(&self, source: crate::xvf3800::DriverError) -> EsphomeError {
        EsphomeError::Raw {
            address: self.address.clone(),
            source,
        }
    }

    /// Runtime values. The subscription keeps them current, so this only
    /// collects the updates that arrived since the last call.
    pub fn read(&mut self) -> Result<Values, EsphomeError> {
        let mut all = self.read_entities()?;
        let ids: Vec<ParamId> = self.raw_specs.iter().map(|s| s.id.clone()).collect();
        if let Some(mut xvf) = self.raw().filter(|_| !ids.is_empty()) {
            let raw = xvf.read(&ids);
            all.extend(raw.map_err(|e| self.raw_error(e))?);
        }
        Ok(all)
    }

    /// The entity values only.
    fn read_entities(&mut self) -> Result<Values, EsphomeError> {
        let Self {
            address,
            runtime,
            client,
            catalog,
            values,
            ..
        } = self;
        runtime.block_on(drain_states(client, catalog, values, address))?;
        Ok(values.clone())
    }

    /// Send one command for each value. Check all values first, so that a
    /// bad value sends nothing.
    pub fn write(&mut self, values: &Values) -> Result<(), EsphomeError> {
        let (raw, values): (Values, Values) = values
            .clone()
            .into_iter()
            .partition(|(id, _)| self.raw_specs.iter().any(|s| &s.id == id));
        let mut commands = Vec::with_capacity(values.len());
        for (id, value) in &values {
            if self.catalog.get(id).is_none() {
                return Err(EsphomeError::UnknownEntity(id.clone()));
            }
            let command = self
                .catalog
                .command(id, value)
                .ok_or_else(|| EsphomeError::NotWritable(id.clone()))?;
            commands.push(command);
        }
        // The driver checks all raw values before it sends one.
        if !raw.is_empty() {
            let result = match self.raw() {
                Some(mut xvf) => xvf.write(&raw),
                None => Err(crate::xvf3800::DriverError::Open(
                    "the device has no raw XVF3800 actions".into(),
                )),
            };
            result.map_err(|e| self.raw_error(e))?;
        }
        let Self {
            address,
            runtime,
            client,
            ..
        } = self;
        runtime.block_on(async {
            for command in commands {
                client
                    .try_write(command)
                    .await
                    .map_err(|e| client_error(address, e))?;
            }
            Ok(())
        })
    }
}

/// The raw parameters of the driver that this link shows. Their values do
/// not survive a reboot of the XVF3800: `commit` does nothing on ESPHome.
fn raw_specs<P: crate::xvf3800::protocol::ControlPort>(xvf: &Xvf3800<P>) -> Vec<ParamSpec> {
    xvf.available()
        .filter(|c| {
            !OWNED_BY_COMPONENT.contains(&c.id)
                && !USB_ONLY.contains(&c.id)
                && !GROUPS_OWNED_BY_COMPONENT
                    .iter()
                    .any(|g| c.id.split('.').next() == Some(g))
        })
        .map(|c| ParamSpec {
            persistent: false,
            ..c.to_spec()
        })
        .collect()
}

/// Apply state messages until the device is quiet.
async fn drain_states(
    client: &mut EspHomeClient,
    catalog: &Catalog,
    values: &mut Values,
    address: &str,
) -> Result<(), EsphomeError> {
    let deadline = tokio::time::Instant::now() + READ_LIMIT;
    while tokio::time::Instant::now() < deadline {
        match timeout(STATE_QUIET, client.try_read()).await {
            Err(_) => break,
            Ok(Err(e)) => return Err(client_error(address, e)),
            Ok(Ok(message)) => {
                catalog.apply_state(&message, values);
            }
        }
    }
    Ok(())
}

fn int(int: i32) -> ExecuteServiceArgument {
    ExecuteServiceArgument {
        int,
        ..Default::default()
    }
}

/// The response data of `xvf_get`: `{"reply": [status, payload...]}`.
fn parse_reply(data: &[u8]) -> Result<Vec<u8>, String> {
    #[derive(serde::Deserialize)]
    struct Reply {
        reply: Vec<u8>,
    }
    serde_json::from_slice::<Reply>(data)
        .map(|r| r.reply)
        .map_err(|e| format!("bad response data: {e}"))
}

/// The raw actions as the XVF3800 control port, so that the USB driver and
/// its retries work over ESPHome. `value` is the command, with the read
/// flag on reads, and `index` is the resource, as in the USB setup packet.
impl crate::xvf3800::protocol::ControlPort for EsphomeDevice {
    fn control_in(
        &mut self,
        value: u16,
        index: u16,
        buf: &mut [u8],
    ) -> Result<usize, crate::xvf3800::protocol::Error> {
        let transport = |e: EsphomeError| crate::xvf3800::protocol::Error::Transport(e.to_string());
        let reply = self
            .xvf_get(
                index as u8,
                (value & 0x7F) as u8,
                buf.len().saturating_sub(1),
            )
            .map_err(transport)?;
        let n = reply.len().min(buf.len());
        buf[..n].copy_from_slice(&reply[..n]);
        Ok(n)
    }

    fn control_out(
        &mut self,
        value: u16,
        index: u16,
        data: &[u8],
    ) -> Result<(), crate::xvf3800::protocol::Error> {
        self.xvf_set(index as u8, value as u8, data)
            .map_err(|e| crate::xvf3800::protocol::Error::Transport(e.to_string()))
    }
}

fn client_error(address: &str, e: impl std::fmt::Display) -> EsphomeError {
    EsphomeError::Client {
        address: address.into(),
        message: e.to_string(),
    }
}

impl crate::Device for EsphomeDevice {
    fn describe(&self) -> String {
        match (&self.xvf_version, &self.raw_error) {
            (Some(v), _) => format!(
                "esphome {}, XVF3800 firmware {}",
                self.address,
                crate::xvf3800::fmt_version(*v)
            ),
            (None, Some(e)) => format!("esphome {}, raw XVF3800 actions failed: {e}", self.address),
            (None, None) => format!("esphome {}", self.address),
        }
    }

    fn schema(&self) -> Schema {
        EsphomeDevice::schema(self)
    }

    fn read_all(&mut self) -> Result<Values, crate::DeviceError> {
        self.read().map_err(crate::DeviceError::from_display)
    }

    fn write(&mut self, values: &Values) -> Result<(), crate::DeviceError> {
        EsphomeDevice::write(self, values).map_err(crate::DeviceError::from_display)
    }

    fn can_play_tone(&self) -> bool {
        self.media_player.is_some()
    }

    fn play_tone(&mut self, tone: crate::metrics::Tone) -> Result<(), crate::DeviceError> {
        EsphomeDevice::play_tone(self, tone).map_err(crate::DeviceError::from_display)
    }

    /// ESPHome saves `restore_value` entities to flash by itself.
    fn persist(&mut self) -> Result<(), crate::DeviceError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reply_comes_from_the_response_json() {
        assert_eq!(
            parse_reply(br#"{"reply":[0,2,0,6]}"#).unwrap(),
            [0, 2, 0, 6]
        );
        assert!(parse_reply(br#"{"reply":[300]}"#).is_err());
        assert!(parse_reply(b"").is_err());
    }

    /// On a device with the raw actions: the schema has the entities and
    /// the raw parameters, without those the component owns. Write then
    /// read one int, one float, and one vector parameter through the
    /// `Device` interface, and restore each value. It needs a device:
    /// `VEUX_TEST_ESPHOME=host:6053 VEUX_TEST_ESPHOME_KEY=... cargo test
    /// -p veux-drivers -- --ignored raw_actions --nocapture`.
    #[test]
    #[ignore = "needs an ESPHome XVF3800 with xvf_get and xvf_set"]
    fn raw_actions_round_trip_on_a_device() {
        use crate::Device;
        use veux_model::Value;

        let address = std::env::var("VEUX_TEST_ESPHOME").expect("VEUX_TEST_ESPHOME");
        let key = std::env::var("VEUX_TEST_ESPHOME_KEY").ok();
        let mut device = EsphomeDevice::connect(&address, key.as_deref()).unwrap();
        println!("{}", device.describe());
        let schema = Device::schema(&device);
        for hidden in ["led.effect", "aec.asroutgain", "app.usb_bit_depth"] {
            assert!(schema.get(&hidden.into()).is_none(), "{hidden} is shown");
        }
        let all = device.read_all().unwrap();
        assert!(all.keys().any(|id| id.as_str().starts_with("number.")));
        assert!(all.contains_key(&"pp.gamma_e".into()));
        println!("{} values", all.len());
        let tests: [(&str, Value); 3] = [
            ("pp.dtsensitive", Value::Int(3)),
            ("pp.gamma_e", Value::Float(1.25)),
            ("aec.fixedbeamnoisethr", Value::Vector(vec![0.5, 0.25])),
        ];
        for (id, new) in tests {
            let id = ParamId::from(id);
            let old = all[&id].clone();
            let write = |d: &mut EsphomeDevice, v: &Value| {
                Device::write(d, &Values::from([(id.clone(), v.clone())])).unwrap();
            };
            write(&mut device, &new);
            let got = device.read_all().unwrap()[&id].clone();
            write(&mut device, &old);
            assert!(got.matches(&new), "{id}: wrote {new}, read {got}");
            assert!(
                device.read_all().unwrap()[&id].matches(&old),
                "{id}: not restored"
            );
            println!("{id}: {old} -> {got} -> {old}");
        }
    }
}
