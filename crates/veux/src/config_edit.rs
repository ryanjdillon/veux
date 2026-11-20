//! Write tuned values back into `veux.toml`.
//!
//! The edit keeps comments, order, and key style: a profile written with
//! dotted keys (`number.gain = 3`) stays dotted. Only the keys that change
//! are touched.

use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use toml_edit::{Array, DocumentMut, Item, Table, value};
use veux_model::{ParamId, Value};

/// Set `values` in the profile of `device`. A device without a profile gets
/// one named after it. Returns the profile name.
pub fn save_values(
    text: &str,
    device: &str,
    values: &[(ParamId, Value)],
) -> Result<(String, String)> {
    let mut doc: DocumentMut = text.parse().context("parse veux.toml")?;
    let profile = {
        let devices = doc
            .get_mut("devices")
            .and_then(Item::as_table_like_mut)
            .ok_or_else(|| anyhow!("veux.toml has no [devices]"))?;
        let entry = devices
            .get_mut(device)
            .and_then(Item::as_table_like_mut)
            .ok_or_else(|| anyhow!("veux.toml has no device {device:?}"))?;
        match entry.get("profile").and_then(Item::as_str) {
            Some(name) => name.to_string(),
            None => {
                entry.insert("profile", value(device));
                device.to_string()
            }
        }
    };
    let profiles = doc
        .entry("profiles")
        .or_insert_with(|| {
            let mut t = Table::new();
            t.set_implicit(true);
            Item::Table(t)
        })
        .as_table_like_mut()
        .ok_or_else(|| anyhow!("[profiles] in veux.toml is not a table"))?;
    let table = profiles
        .entry(&profile)
        .or_insert(Item::Table(Table::new()))
        .as_table_like_mut()
        .ok_or_else(|| anyhow!("profile {profile:?} in veux.toml is not a table"))?;
    for (id, v) in values {
        set_path(table, id.as_str(), to_item(v)?)?;
    }
    Ok((profile, doc.to_string()))
}

/// Set a dotted ID inside a profile table. A missing level becomes a dotted
/// key, so the file keeps the `group.name = value` form.
fn set_path(table: &mut dyn toml_edit::TableLike, id: &str, item: Item) -> Result<()> {
    // A profile can also hold the ID as one quoted key.
    if table.contains_key(id) {
        replace(table, id, item);
        return Ok(());
    }
    match id.split_once('.') {
        None => {
            replace(table, id, item);
            Ok(())
        }
        Some((head, rest)) => {
            let child = table.entry(head).or_insert_with(|| {
                let mut t = Table::new();
                t.set_dotted(true);
                Item::Table(t)
            });
            let child = child
                .as_table_like_mut()
                .ok_or_else(|| anyhow!("{head} in the profile is a value, not a group"))?;
            set_path(child, rest, item)
        }
    }
}

/// Insert `item`, and keep the spacing and trailing comment of the value it
/// replaces.
fn replace(table: &mut dyn toml_edit::TableLike, key: &str, mut item: Item) {
    if let (Some(old), Some(new)) = (table.get(key).and_then(Item::as_value), item.as_value_mut()) {
        *new.decor_mut() = old.decor().clone();
    }
    table.insert(key, item);
}

pub fn to_item(v: &Value) -> Result<Item> {
    Ok(match v {
        Value::Bool(b) => value(*b),
        Value::Int(i) => value(*i),
        Value::Float(f) if f.is_finite() => value(*f),
        Value::Enum(s) | Value::Text(s) => value(s.as_str()),
        Value::Vector(vs) => {
            let mut array = Array::new();
            for x in vs {
                if !x.is_finite() {
                    bail!("a vector with NaN cannot go in a profile");
                }
                // Integer-valued vectors, for example mixer channels, stay
                // integers in the file.
                if x.fract() == 0.0 && x.abs() < 1e15 {
                    array.push(*x as i64);
                } else {
                    array.push(*x);
                }
            }
            value(array)
        }
        Value::Float(_) => bail!("NaN cannot go in a profile"),
    })
}

/// Read, edit, and write the file in one step.
pub fn save_to_file(path: &Path, device: &str, values: &[(ParamId, Value)]) -> Result<String> {
    let text = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let (profile, new) = save_values(&text, device, values)?;
    std::fs::write(path, new).with_context(|| format!("write {}", path.display()))?;
    Ok(profile)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONFIG: &str = r#"# My satellites.
[devices.desk]
kind = "xvf3800"
link = "esphome"
address = "sat.local:6053"
profile = "desk"   # tuned by ear

[devices.hat]
kind = "respeaker-2mic-v1"
link = "alsa"

[profiles.desk]
switch.wake_sound = false
number.asr_output_gain = 3.0   # was too quiet at 1
"#;

    fn save(values: &[(&str, Value)], device: &str) -> (String, String) {
        let values: Vec<(ParamId, Value)> = values
            .iter()
            .map(|(id, v)| (ParamId::from(*id), v.clone()))
            .collect();
        save_values(CONFIG, device, &values).unwrap()
    }

    #[test]
    fn updates_a_value_and_keeps_comments_and_style() {
        let (profile, text) = save(&[("number.asr_output_gain", Value::Float(4.5))], "desk");
        assert_eq!(profile, "desk");
        assert!(text.contains("number.asr_output_gain = 4.5"), "{text}");
        assert!(text.contains("# was too quiet at 1"), "{text}");
        assert!(text.starts_with("# My satellites."));
        assert!(text.contains(r#"profile = "desk"   # tuned by ear"#));
        assert_eq!(text.lines().count(), CONFIG.lines().count());
    }

    #[test]
    fn adds_new_ids_as_dotted_keys() {
        let (_, text) = save(
            &[
                ("select.dsp_route", Value::Enum("Comms".into())),
                ("number.agc_max_gain_comms", Value::Int(96)),
            ],
            "desk",
        );
        assert!(text.contains(r#"select.dsp_route = "Comms""#), "{text}");
        assert!(text.contains("number.agc_max_gain_comms = 96"), "{text}");
        let config = veux_model::Config::parse(&text).unwrap();
        assert_eq!(config.profiles["desk"].values.len(), 4);
    }

    #[test]
    fn creates_a_profile_for_a_device_without_one() {
        let (profile, text) = save(
            &[("mixer.capture_volume", Value::Vector(vec![40.0, 40.0]))],
            "hat",
        );
        assert_eq!(profile, "hat");
        let config = veux_model::Config::parse(&text).unwrap();
        assert_eq!(config.devices["hat"].profile.as_deref(), Some("hat"));
        let saved = &config.profiles["hat"].values[&ParamId::from("mixer.capture_volume")];
        assert_eq!(saved.to_string(), "[40, 40]");
    }

    #[test]
    fn refuses_nan_and_unknown_devices() {
        let values = [(ParamId::from("a.b"), Value::Float(f64::NAN))];
        assert!(save_values(CONFIG, "desk", &values).is_err());
        let values = [(ParamId::from("a.b"), Value::Int(1))];
        assert!(save_values(CONFIG, "toaster", &values).is_err());
    }
}
