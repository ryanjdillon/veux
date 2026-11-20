//! Map ESPHome entities to veux parameters. No network I/O here.
//!
//! The schema of an ESPHome device comes from the device: veux lists its
//! entities when it connects. A parameter ID is `<platform>.<slug>`, for
//! example `number.asr_output_gain`. The slug comes from the entity name,
//! because ESPHome 2026 sends an empty `object_id`.

use std::collections::BTreeMap;

use esphome_client::types::{
    EspHomeMessage, LightCommandRequest, MediaPlayerCommand, MediaPlayerCommandRequest,
    NumberCommandRequest, SelectCommandRequest, SwitchCommandRequest,
};
use veux_model::{Access, ParamId, ParamSpec, ParamType, Schema, Value, Values};

use crate::slug::slug;

/// One value of a light. A light entity gives a parameter for each value
/// that its color modes support.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LightField {
    On,
    Brightness,
    Rgb,
    Effect,
}

impl LightField {
    fn as_str(self) -> &'static str {
        match self {
            LightField::On => "on",
            LightField::Brightness => "brightness",
            LightField::Rgb => "rgb",
            LightField::Effect => "effect",
        }
    }
}

/// One value of a media player.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaField {
    Volume,
    Muted,
}

impl MediaField {
    fn as_str(self) -> &'static str {
        match self {
            MediaField::Volume => "volume",
            MediaField::Muted => "muted",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Number,
    Select,
    Switch,
    Sensor,
    BinarySensor,
    TextSensor,
    Light(LightField),
    MediaPlayer(MediaField),
}

impl Platform {
    fn as_str(self) -> &'static str {
        match self {
            Platform::Number => "number",
            Platform::Select => "select",
            Platform::Switch => "switch",
            Platform::Sensor => "sensor",
            Platform::BinarySensor => "binary_sensor",
            Platform::TextSensor => "text_sensor",
            Platform::Light(_) => "light",
            Platform::MediaPlayer(_) => "media_player",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Entity {
    pub key: u32,
    pub platform: Platform,
    pub spec: ParamSpec,
}

/// The entities of one device, by key and by parameter ID. A light has more
/// than one parameter for its key.
#[derive(Debug, Clone, Default)]
pub struct Catalog {
    by_key: BTreeMap<u32, Vec<ParamId>>,
    by_id: BTreeMap<ParamId, Entity>,
}

/// Color capability bits of an ESPHome `ColorMode`.
const CAPABILITY_BRIGHTNESS: i32 = 2;
const CAPABILITY_RGB: i32 = 32;

impl Catalog {
    /// Add an entity from a `ListEntities*Response`. Returns false for
    /// messages that are not a supported entity.
    pub fn add(&mut self, message: &EspHomeMessage) -> bool {
        let Some((key, stem, entities)) = entities_from(message) else {
            return false;
        };
        // Two entities can have the same name. Keep both, in listing order.
        let mut unique = stem.clone();
        let mut n = 2;
        while self.by_id.keys().any(|id| {
            let id = id.as_str();
            id == unique || id.starts_with(&format!("{unique}."))
        }) {
            unique = format!("{stem}_{n}");
            n += 1;
        }
        let ids = self.by_key.entry(key).or_default();
        for (field, mut entity) in entities {
            entity.spec.id = ParamId::new(match field {
                Some(field) => format!("{unique}.{field}"),
                None => unique.clone(),
            });
            ids.push(entity.spec.id.clone());
            self.by_id.insert(entity.spec.id.clone(), entity);
        }
        true
    }

    pub fn schema(&self) -> Schema {
        Schema::new(self.by_id.values().map(|e| e.spec.clone()))
    }

    pub fn get(&self, id: &ParamId) -> Option<&Entity> {
        self.by_id.get(id)
    }

    /// The number of entities, not parameters.
    pub fn len(&self) -> usize {
        self.by_key.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_key.is_empty()
    }

    /// Store the values of a `*StateResponse` in `values`. Returns false
    /// for other messages, unknown keys, and states that the device marks
    /// as missing.
    pub fn apply_state(&self, message: &EspHomeMessage, values: &mut Values) -> bool {
        let (key, value) = match message {
            EspHomeMessage::NumberStateResponse(s) if !s.missing_state => {
                (s.key, Value::Float(s.state.into()))
            }
            EspHomeMessage::SelectStateResponse(s) if !s.missing_state => {
                (s.key, Value::Enum(s.state.clone()))
            }
            EspHomeMessage::SwitchStateResponse(s) => (s.key, Value::Bool(s.state)),
            EspHomeMessage::SensorStateResponse(s) if !s.missing_state => {
                (s.key, Value::Float(s.state.into()))
            }
            EspHomeMessage::BinarySensorStateResponse(s) if !s.missing_state => {
                (s.key, Value::Bool(s.state))
            }
            EspHomeMessage::TextSensorStateResponse(s) if !s.missing_state => {
                (s.key, Value::Text(s.state.clone()))
            }
            EspHomeMessage::LightStateResponse(s) => {
                let Some(ids) = self.by_key.get(&s.key) else {
                    return false;
                };
                for id in ids {
                    let Platform::Light(field) = self.by_id[id].platform else {
                        continue;
                    };
                    let value = match field {
                        LightField::On => Value::Bool(s.state),
                        LightField::Brightness => Value::Float(s.brightness.into()),
                        LightField::Rgb => {
                            Value::Vector(vec![s.red.into(), s.green.into(), s.blue.into()])
                        }
                        LightField::Effect => Value::Enum(s.effect.clone()),
                    };
                    values.insert(id.clone(), value);
                }
                return true;
            }
            EspHomeMessage::MediaPlayerStateResponse(s) => {
                let Some(ids) = self.by_key.get(&s.key) else {
                    return false;
                };
                for id in ids {
                    let value = match self.by_id[id].platform {
                        Platform::MediaPlayer(MediaField::Volume) => Value::Float(s.volume.into()),
                        Platform::MediaPlayer(MediaField::Muted) => Value::Bool(s.muted),
                        _ => continue,
                    };
                    values.insert(id.clone(), value);
                }
                return true;
            }
            _ => return false,
        };
        let Some(id) = self.by_key.get(&key).and_then(|ids| ids.first()) else {
            return false;
        };
        values.insert(id.clone(), value);
        true
    }

    /// The command that sets `id` to `value`. `None` when the entity is
    /// read-only or the value has the wrong type.
    pub fn command(&self, id: &ParamId, value: &Value) -> Option<EspHomeMessage> {
        let entity = self.get(id)?;
        let key = entity.key;
        Some(match (entity.platform, value) {
            (Platform::Number, Value::Float(v)) => NumberCommandRequest {
                key,
                state: *v as f32,
                ..Default::default()
            }
            .into(),
            (Platform::Number, Value::Int(v)) => NumberCommandRequest {
                key,
                state: *v as f32,
                ..Default::default()
            }
            .into(),
            (Platform::Select, Value::Enum(v)) => SelectCommandRequest {
                key,
                state: v.clone(),
                ..Default::default()
            }
            .into(),
            (Platform::Switch, Value::Bool(v)) => SwitchCommandRequest {
                key,
                state: *v,
                ..Default::default()
            }
            .into(),
            (Platform::Light(field), value) => light_command(key, field, value)?.into(),
            (Platform::MediaPlayer(MediaField::Volume), Value::Float(v)) => {
                MediaPlayerCommandRequest {
                    key,
                    has_volume: true,
                    volume: *v as f32,
                    ..Default::default()
                }
                .into()
            }
            (Platform::MediaPlayer(MediaField::Muted), Value::Bool(muted)) => {
                let command = if *muted {
                    MediaPlayerCommand::Mute
                } else {
                    MediaPlayerCommand::Unmute
                };
                MediaPlayerCommandRequest {
                    key,
                    has_command: true,
                    command: command as i32,
                    ..Default::default()
                }
                .into()
            }
            _ => return None,
        })
    }
}

/// A light command that changes only `field`. A brightness or color
/// command does not turn the light on (verified on ESPHome 2026).
fn light_command(key: u32, field: LightField, value: &Value) -> Option<LightCommandRequest> {
    let mut cmd = LightCommandRequest {
        key,
        ..Default::default()
    };
    match (field, value) {
        (LightField::On, Value::Bool(on)) => {
            cmd.has_state = true;
            cmd.state = *on;
        }
        (LightField::Brightness, Value::Float(b)) => {
            cmd.has_brightness = true;
            cmd.brightness = *b as f32;
        }
        (LightField::Rgb, Value::Vector(rgb)) => {
            let [r, g, b] = rgb.as_slice() else {
                return None;
            };
            cmd.has_rgb = true;
            (cmd.red, cmd.green, cmd.blue) = (*r as f32, *g as f32, *b as f32);
        }
        (LightField::Effect, Value::Enum(effect)) => {
            cmd.has_effect = true;
            cmd.effect = effect.clone();
        }
        _ => return None,
    }
    Some(cmd)
}

/// The key, the ID stem `<platform>.<slug>`, and the parameters of an
/// entity. A parameter with a field name gets the ID `<stem>.<field>`.
type Listed = (u32, String, Vec<(Option<&'static str>, Entity)>);

fn entities_from(message: &EspHomeMessage) -> Option<Listed> {
    if let EspHomeMessage::ListEntitiesLightResponse(e) = message {
        return Some(light_entities(e));
    }
    if let EspHomeMessage::ListEntitiesMediaPlayerResponse(e) = message {
        return Some(media_player_entities(e));
    }
    let (platform, key, name, ty, unit, access) = match message {
        EspHomeMessage::ListEntitiesNumberResponse(e) => (
            Platform::Number,
            e.key,
            &e.name,
            ParamType::Float {
                min: e.min_value.into(),
                max: e.max_value.into(),
            },
            &e.unit_of_measurement,
            Access::ReadWrite,
        ),
        EspHomeMessage::ListEntitiesSelectResponse(e) => (
            Platform::Select,
            e.key,
            &e.name,
            ParamType::Enum {
                variants: e.options.clone(),
            },
            &String::new(),
            Access::ReadWrite,
        ),
        EspHomeMessage::ListEntitiesSwitchResponse(e) => (
            Platform::Switch,
            e.key,
            &e.name,
            ParamType::Bool,
            &String::new(),
            Access::ReadWrite,
        ),
        EspHomeMessage::ListEntitiesSensorResponse(e) => (
            Platform::Sensor,
            e.key,
            &e.name,
            ParamType::Float {
                min: f64::MIN,
                max: f64::MAX,
            },
            &e.unit_of_measurement,
            Access::ReadOnly,
        ),
        EspHomeMessage::ListEntitiesBinarySensorResponse(e) => (
            Platform::BinarySensor,
            e.key,
            &e.name,
            ParamType::Bool,
            &String::new(),
            Access::ReadOnly,
        ),
        EspHomeMessage::ListEntitiesTextSensorResponse(e) => (
            Platform::TextSensor,
            e.key,
            &e.name,
            ParamType::Text {
                max_len: usize::MAX,
            },
            &String::new(),
            Access::ReadOnly,
        ),
        _ => return None,
    };
    let stem = format!("{}.{}", platform.as_str(), slug(name));
    let entity = entity(key, platform, name, ty, unit, access);
    Some((key, stem, vec![(None, entity)]))
}

fn entity(
    key: u32,
    platform: Platform,
    name: &str,
    ty: ParamType,
    unit: &str,
    access: Access,
) -> Entity {
    Entity {
        key,
        platform,
        spec: ParamSpec {
            // `Catalog::add` sets the unique ID.
            id: ParamId::new(String::new()),
            group: platform.as_str().into(),
            description: name.into(),
            ty,
            unit: (!unit.is_empty()).then(|| unit.into()),
            access,
            // ESPHome saves `restore_value` entities by itself. veux cannot
            // see which entities do that, or read the saved value.
            persistent: false,
        },
    }
}

/// The volume and mute of a media player. Play, pause, and media are
/// actions, not settings, so they are not parameters.
fn media_player_entities(e: &esphome_client::types::ListEntitiesMediaPlayerResponse) -> Listed {
    let field = |f: MediaField, ty| {
        let name = format!("{} {}", e.name, f.as_str());
        let platform = Platform::MediaPlayer(f);
        let entity = entity(e.key, platform, &name, ty, "", Access::ReadWrite);
        (Some(f.as_str()), entity)
    };
    let fields = vec![
        field(MediaField::Volume, ParamType::Float { min: 0.0, max: 1.0 }),
        field(MediaField::Muted, ParamType::Bool),
    ];
    (e.key, format!("media_player.{}", slug(&e.name)), fields)
}

fn light_entities(e: &esphome_client::types::ListEntitiesLightResponse) -> Listed {
    let has = |capability| e.supported_color_modes.iter().any(|m| m & capability != 0);
    let field = |f: LightField, ty, unit: &str| {
        let name = format!("{} {}", e.name, f.as_str());
        (
            Some(f.as_str()),
            entity(
                e.key,
                Platform::Light(f),
                &name,
                ty,
                unit,
                Access::ReadWrite,
            ),
        )
    };
    let mut fields = vec![field(LightField::On, ParamType::Bool, "")];
    if has(CAPABILITY_BRIGHTNESS) {
        fields.push(field(
            LightField::Brightness,
            ParamType::Float { min: 0.0, max: 1.0 },
            "",
        ));
    }
    if has(CAPABILITY_RGB) {
        fields.push(field(
            LightField::Rgb,
            ParamType::Vector {
                len: 3,
                min: 0.0,
                max: 1.0,
            },
            "",
        ));
    }
    if !e.effects.is_empty() {
        fields.push(field(
            LightField::Effect,
            ParamType::Enum {
                variants: e.effects.clone(),
            },
            "",
        ));
    }
    (e.key, format!("light.{}", slug(&e.name)), fields)
}

#[cfg(test)]
mod tests {
    use esphome_client::types::{
        ListEntitiesNumberResponse, ListEntitiesSelectResponse, ListEntitiesSensorResponse,
        ListEntitiesSwitchResponse, NumberStateResponse, SelectStateResponse, SwitchStateResponse,
    };

    use super::*;

    fn catalog() -> Catalog {
        let mut c = Catalog::default();
        let messages: Vec<EspHomeMessage> = vec![
            ListEntitiesNumberResponse {
                object_id: "asr_output_gain".into(),
                key: 1,
                name: "ASR output gain".into(),
                min_value: 1.0,
                max_value: 8.0,
                step: 0.1,
                ..Default::default()
            }
            .into(),
            ListEntitiesSelectResponse {
                object_id: "dsp_route".into(),
                key: 2,
                name: "DSP route".into(),
                options: vec![
                    "ASR to STT and wake word".into(),
                    "Comms to STT, ASR to wake word".into(),
                ],
                ..Default::default()
            }
            .into(),
            ListEntitiesSwitchResponse {
                object_id: "wake_sound".into(),
                key: 3,
                name: "Wake sound".into(),
                ..Default::default()
            }
            .into(),
            ListEntitiesSensorResponse {
                object_id: "next_timer".into(),
                key: 4,
                name: "Next timer".into(),
                unit_of_measurement: "s".into(),
                ..Default::default()
            }
            .into(),
        ];
        for m in &messages {
            assert!(c.add(m));
        }
        c
    }

    #[test]
    fn ids_come_from_names_and_stay_unique() {
        let mut c = Catalog::default();
        for key in [10, 11] {
            c.add(
                &ListEntitiesSelectResponse {
                    key,
                    name: "Logger Level".into(),
                    ..Default::default()
                }
                .into(),
            );
        }
        let ids: Vec<String> = c.schema().params().map(|p| p.id.to_string()).collect();
        assert_eq!(ids, ["select.logger_level", "select.logger_level_2"]);
    }

    fn ring() -> EspHomeMessage {
        esphome_client::types::ListEntitiesLightResponse {
            key: 20,
            name: "LED Ring".into(),
            // RGB: on/off, brightness, and RGB capabilities.
            supported_color_modes: vec![35],
            effects: vec!["None".into(), "Rainbow".into()],
            ..Default::default()
        }
        .into()
    }

    #[test]
    fn a_light_gives_a_parameter_for_each_capability() {
        let mut c = Catalog::default();
        assert!(c.add(&ring()));
        assert!(
            c.add(
                &esphome_client::types::ListEntitiesLightResponse {
                    key: 21,
                    name: "Status".into(),
                    supported_color_modes: vec![1],
                    ..Default::default()
                }
                .into()
            )
        );
        let ids: Vec<String> = c.schema().params().map(|p| p.id.to_string()).collect();
        assert_eq!(
            ids,
            [
                "light.led_ring.brightness",
                "light.led_ring.effect",
                "light.led_ring.on",
                "light.led_ring.rgb",
                "light.status.on",
            ]
        );
        assert_eq!(c.len(), 2);
        // A second light with the same name keeps its own stem.
        c.add(
            &esphome_client::types::ListEntitiesLightResponse {
                key: 22,
                name: "Status".into(),
                supported_color_modes: vec![1],
                ..Default::default()
            }
            .into(),
        );
        assert!(c.get(&"light.status_2.on".into()).is_some());
    }

    #[test]
    fn light_state_fills_each_field_and_commands_change_one() {
        let mut c = Catalog::default();
        c.add(&ring());
        let mut values = Values::new();
        assert!(
            c.apply_state(
                &esphome_client::types::LightStateResponse {
                    key: 20,
                    state: true,
                    brightness: 0.5,
                    red: 1.0,
                    green: 0.25,
                    blue: 0.0,
                    effect: "Rainbow".into(),
                    ..Default::default()
                }
                .into(),
                &mut values
            )
        );
        assert_eq!(values.len(), 4);
        assert_eq!(
            values[&ParamId::from("light.led_ring.rgb")],
            Value::Vector(vec![1.0, 0.25, 0.0])
        );
        let cmd = c
            .command(
                &"light.led_ring.rgb".into(),
                &Value::Vector(vec![0.0, 0.5, 1.0]),
            )
            .unwrap();
        let EspHomeMessage::LightCommandRequest(cmd) = cmd else {
            panic!("not a light command");
        };
        assert!(cmd.has_rgb && !cmd.has_state && !cmd.has_brightness);
        assert_eq!((cmd.key, cmd.green, cmd.blue), (20, 0.5, 1.0));
        assert!(
            c.command(&"light.led_ring.rgb".into(), &Value::Vector(vec![1.0]))
                .is_none()
        );
        let cmd = c
            .command(&"light.led_ring.on".into(), &Value::Bool(false))
            .unwrap();
        assert!(matches!(cmd, EspHomeMessage::LightCommandRequest(r) if r.has_state && !r.state));
    }

    #[test]
    fn a_media_player_gives_volume_and_mute() {
        use esphome_client::types::{ListEntitiesMediaPlayerResponse, MediaPlayerStateResponse};
        let mut c = Catalog::default();
        assert!(
            c.add(
                &ListEntitiesMediaPlayerResponse {
                    key: 30,
                    name: "Media Player".into(),
                    ..Default::default()
                }
                .into()
            )
        );
        let ids: Vec<String> = c.schema().params().map(|p| p.id.to_string()).collect();
        assert_eq!(
            ids,
            [
                "media_player.media_player.muted",
                "media_player.media_player.volume"
            ]
        );
        let mut values = Values::new();
        c.apply_state(
            &MediaPlayerStateResponse {
                key: 30,
                volume: 0.5,
                muted: true,
                ..Default::default()
            }
            .into(),
            &mut values,
        );
        assert_eq!(
            values[&ParamId::from("media_player.media_player.volume")],
            Value::Float(0.5)
        );
        let cmd = c
            .command(
                &"media_player.media_player.volume".into(),
                &Value::Float(0.8),
            )
            .unwrap();
        assert!(matches!(cmd, EspHomeMessage::MediaPlayerCommandRequest(r)
            if r.has_volume && !r.has_command && r.volume == 0.8));
        let cmd = c
            .command(
                &"media_player.media_player.muted".into(),
                &Value::Bool(false),
            )
            .unwrap();
        assert!(matches!(cmd, EspHomeMessage::MediaPlayerCommandRequest(r)
            if r.has_command && r.command == MediaPlayerCommand::Unmute as i32));
    }

    #[test]
    fn builds_schema_from_entities() {
        let schema = catalog().schema();
        let ids: Vec<String> = schema.params().map(|p| p.id.to_string()).collect();
        assert_eq!(
            ids,
            [
                "number.asr_output_gain",
                "select.dsp_route",
                "sensor.next_timer",
                "switch.wake_sound"
            ]
        );
        let gain = schema.get(&"number.asr_output_gain".into()).unwrap();
        assert_eq!(gain.ty, ParamType::Float { min: 1.0, max: 8.0 });
        assert_eq!(gain.description, "ASR output gain");
        let timer = schema.get(&"sensor.next_timer".into()).unwrap();
        assert_eq!(timer.access, Access::ReadOnly);
        assert_eq!(timer.unit.as_deref(), Some("s"));
    }

    #[test]
    fn profile_values_parse_against_discovered_schema() {
        let schema = catalog().schema();
        let route = schema.get(&"select.dsp_route".into()).unwrap();
        assert!(
            route
                .parse(&toml::Value::String(
                    "Comms to STT, ASR to wake word".into()
                ))
                .is_ok()
        );
        assert!(route.parse(&toml::Value::String("Comms".into())).is_err());
        let gain = schema.get(&"number.asr_output_gain".into()).unwrap();
        assert!(gain.parse(&toml::Value::Float(9.0)).is_err());
    }

    #[test]
    fn applies_states_by_key() {
        let c = catalog();
        let mut values = Values::new();
        assert!(
            c.apply_state(
                &NumberStateResponse {
                    key: 1,
                    state: 3.0,
                    ..Default::default()
                }
                .into(),
                &mut values
            )
        );
        assert!(
            c.apply_state(
                &SwitchStateResponse {
                    key: 3,
                    state: false,
                    ..Default::default()
                }
                .into(),
                &mut values
            )
        );
        assert!(
            !c.apply_state(
                &SelectStateResponse {
                    key: 2,
                    missing_state: true,
                    ..Default::default()
                }
                .into(),
                &mut values
            )
        );
        assert!(
            !c.apply_state(
                &SwitchStateResponse {
                    key: 99,
                    ..Default::default()
                }
                .into(),
                &mut values
            )
        );
        assert_eq!(
            values[&ParamId::from("number.asr_output_gain")],
            Value::Float(3.0)
        );
        assert_eq!(
            values[&ParamId::from("switch.wake_sound")],
            Value::Bool(false)
        );
        assert_eq!(values.len(), 2);
    }

    #[test]
    fn builds_commands_for_writable_entities() {
        let c = catalog();
        let cmd = c
            .command(&"number.asr_output_gain".into(), &Value::Float(3.0))
            .unwrap();
        assert_eq!(
            cmd,
            NumberCommandRequest {
                key: 1,
                state: 3.0,
                ..Default::default()
            }
            .into()
        );
        let cmd = c
            .command(
                &"select.dsp_route".into(),
                &Value::Enum("Comms to STT, ASR to wake word".into()),
            )
            .unwrap();
        assert!(matches!(cmd, EspHomeMessage::SelectCommandRequest(r) if r.key == 2));
        assert!(
            c.command(&"sensor.next_timer".into(), &Value::Float(1.0))
                .is_none()
        );
        assert!(
            c.command(&"switch.wake_sound".into(), &Value::Int(1))
                .is_none()
        );
    }
}
