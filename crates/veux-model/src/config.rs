//! The `veux.toml` file: hosts, devices, and profiles.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::schema::ParamId;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub hosts: BTreeMap<String, Host>,
    #[serde(default)]
    pub devices: BTreeMap<String, Device>,
    #[serde(default)]
    pub profiles: BTreeMap<String, Profile>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Host {
    /// SSH destination, for example `user@host.local`.
    pub ssh: String,
    /// Run this helper binary and do not upload one.
    pub helper_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Link {
    Usb,
    Alsa,
    Esphome,
}

impl Link {
    pub fn as_str(self) -> &'static str {
        match self {
            Link::Usb => "usb",
            Link::Alsa => "alsa",
            Link::Esphome => "esphome",
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Device {
    pub kind: String,
    pub link: Link,
    /// Name of an entry in `hosts`. No host means the local machine.
    pub host: Option<String>,
    /// `host:port` of an ESPHome device.
    pub address: Option<String>,
    /// Environment variable that holds the ESPHome Noise key.
    pub noise_key_env: Option<String>,
    /// USB attributes that select one device, for example `serial`.
    #[serde(rename = "match", default)]
    pub selector: BTreeMap<String, String>,
    pub profile: Option<String>,
}

/// Raw profile values, keyed by parameter ID.
///
/// A profile accepts quoted IDs (`"aec.enabled" = true`) and TOML dotted
/// keys or sub-tables (`aec.enabled = true`). Both give the same ID.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(from = "toml::Table")]
pub struct Profile {
    pub values: BTreeMap<ParamId, toml::Value>,
}

impl From<toml::Table> for Profile {
    fn from(table: toml::Table) -> Self {
        let mut values = BTreeMap::new();
        flatten("", table, &mut values);
        Self { values }
    }
}

fn flatten(prefix: &str, table: toml::Table, out: &mut BTreeMap<ParamId, toml::Value>) {
    for (key, value) in table {
        let id = if prefix.is_empty() {
            key
        } else {
            format!("{prefix}.{key}")
        };
        match value {
            toml::Value::Table(sub) => flatten(&id, sub, out),
            other => {
                out.insert(ParamId::new(id), other);
            }
        }
    }
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("cannot read {path}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("cannot parse {path}")]
    Parse {
        path: PathBuf,
        source: Box<toml::de::Error>,
    },
}

impl Config {
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        Self::parse(&text).map_err(|source| ConfigError::Parse {
            path: path.to_path_buf(),
            source: Box::new(source),
        })
    }

    pub fn parse(text: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_flattens_dotted_and_quoted_keys() {
        let config = Config::parse(
            r#"
            [profiles.p]
            "aec.enabled" = true
            agc.max_gain_db = 30
            [profiles.p.led]
            brightness = 5
            "#,
        )
        .unwrap();
        let ids: Vec<&str> = config.profiles["p"]
            .values
            .keys()
            .map(ParamId::as_str)
            .collect();
        assert_eq!(ids, ["aec.enabled", "agc.max_gain_db", "led.brightness"]);
    }

    #[test]
    fn device_rejects_unknown_field() {
        let err = Config::parse(
            r#"
            [devices.d]
            kind = "xvf3800"
            link = "usb"
            colour = "red"
            "#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("colour"));
    }

    #[test]
    fn device_reads_match_table() {
        let config = Config::parse(
            r#"
            [devices.d]
            kind = "xvf3800"
            link = "usb"
            match = { serial = "abc" }
            "#,
        )
        .unwrap();
        assert_eq!(config.devices["d"].selector["serial"], "abc");
    }
}
