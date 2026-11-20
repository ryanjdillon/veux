//! The values saved at the last commit.
//!
//! The XVF3800 cannot read back what SAVE_CONFIGURATION stores. veux records
//! the runtime values at each commit in a file for each serial number, and
//! shows them as the persisted state. The file keeps the firmware version: a
//! different version at open means a flash, which can lose the saved values.
//! A clear of the saved values by another tool is not seen.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use veux_model::{Access, ParamType, Schema, Value, Values};

use crate::slug::slug;

#[derive(Serialize, Deserialize)]
struct Snapshot {
    firmware: String,
    values: toml::Table,
}

/// The snapshot file of the device with this USB serial number, in
/// `$XDG_STATE_HOME/veux`, or `~/.local/state/veux`.
pub fn path(serial: &str) -> Option<PathBuf> {
    let dir = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))?;
    Some(
        dir.join("veux")
            .join(format!("xvf3800-{}.toml", slug(serial))),
    )
}

/// Record the persistent values in `values`.
pub fn save(path: &Path, firmware: &str, schema: &Schema, values: &Values) -> std::io::Result<()> {
    let values = schema
        .params()
        .filter(|s| s.access == Access::ReadWrite && s.persistent)
        .filter_map(|s| Some((s.id.to_string(), to_toml(values.get(&s.id)?)?)))
        .collect();
    let text = toml::to_string(&Snapshot {
        firmware: firmware.into(),
        values,
    })
    .map_err(std::io::Error::other)?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, text)
}

/// The recorded values. `None` without a snapshot, or when it was made with
/// other firmware.
pub fn load(path: &Path, firmware: &str, schema: &Schema) -> Option<Values> {
    let snapshot: Snapshot = toml::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    if snapshot.firmware != firmware {
        return None;
    }
    Some(
        schema
            .params()
            .filter_map(|s| {
                let value = from_toml(&s.ty, snapshot.values.get(s.id.as_str())?)?;
                Some((s.id.clone(), value))
            })
            .collect(),
    )
}

pub fn remove(path: &Path) {
    let _ = std::fs::remove_file(path);
}

/// NaN has no TOML form, so a value with NaN is not recorded.
fn to_toml(value: &Value) -> Option<toml::Value> {
    Some(match value {
        Value::Bool(b) => toml::Value::Boolean(*b),
        Value::Int(i) => toml::Value::Integer(*i),
        Value::Float(f) if f.is_finite() => toml::Value::Float(*f),
        Value::Float(_) => return None,
        Value::Enum(s) | Value::Text(s) => toml::Value::String(s.clone()),
        Value::Vector(v) if v.iter().all(|x| x.is_finite()) => {
            toml::Value::Array(v.iter().map(|x| toml::Value::Float(*x)).collect())
        }
        Value::Vector(_) => return None,
    })
}

/// The value of the type that the schema gives. There is no range check: the
/// snapshot shows what the device had, and some ranges are not confirmed.
fn from_toml(ty: &ParamType, raw: &toml::Value) -> Option<Value> {
    let number = |v: &toml::Value| match v {
        toml::Value::Float(f) => Some(*f),
        toml::Value::Integer(i) => Some(*i as f64),
        _ => None,
    };
    Some(match (ty, raw) {
        (ParamType::Bool, toml::Value::Boolean(b)) => Value::Bool(*b),
        (ParamType::Int { .. }, toml::Value::Integer(i)) => Value::Int(*i),
        (ParamType::Float { .. }, v) => Value::Float(number(v)?),
        (ParamType::Enum { .. }, toml::Value::String(s)) => Value::Enum(s.clone()),
        (ParamType::Text { .. }, toml::Value::String(s)) => Value::Text(s.clone()),
        (ParamType::Vector { .. }, toml::Value::Array(items)) => {
            Value::Vector(items.iter().map(number).collect::<Option<_>>()?)
        }
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use veux_model::{ParamSpec, ParamType};

    use super::*;

    fn spec(id: &str, ty: ParamType, access: Access) -> ParamSpec {
        ParamSpec {
            id: id.into(),
            group: "test".into(),
            description: String::new(),
            ty,
            unit: None,
            access,
            persistent: access == Access::ReadWrite,
        }
    }

    #[test]
    fn keeps_writable_values_for_the_same_firmware_only() {
        let schema = Schema::new([
            spec(
                "a.gain",
                ParamType::Float {
                    min: 0.0,
                    max: 10.0,
                },
                Access::ReadWrite,
            ),
            spec(
                "a.beams",
                ParamType::Vector {
                    len: 2,
                    min: 0.0,
                    max: 1.0,
                },
                Access::ReadWrite,
            ),
            spec(
                "a.energy",
                ParamType::Float { min: 0.0, max: 1.0 },
                Access::ReadOnly,
            ),
        ]);
        let values = Values::from([
            // Out of the schema range, as the device reported it.
            ("a.gain".into(), Value::Float(12.0)),
            ("a.beams".into(), Value::Vector(vec![0.0, 1.0])),
            ("a.energy".into(), Value::Float(0.3)),
        ]);
        let dir = std::env::temp_dir().join(format!("veux-snapshot-{}", std::process::id()));
        let file = dir.join("xvf3800-x.toml");
        save(&file, "2.0.6", &schema, &values).unwrap();
        let loaded = load(&file, "2.0.6", &schema).unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[&"a.gain".into()], Value::Float(12.0));
        assert_eq!(loaded[&"a.beams".into()], Value::Vector(vec![0.0, 1.0]));
        assert_eq!(load(&file, "2.0.7", &schema), None);
        remove(&file);
        assert_eq!(load(&file, "2.0.6", &schema), None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn path_uses_a_safe_file_name() {
        let p = path("AB:12/../x").unwrap();
        assert_eq!(p.file_name().unwrap(), "xvf3800-ab_12_x.toml");
    }
}
