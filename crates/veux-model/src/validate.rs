//! Checks a config against the device kinds that the drivers declare.

use std::fmt;

use crate::config::{Config, Device, Link};
use crate::schema::Schema;

/// What a driver declares about one device kind.
#[derive(Debug, Clone)]
pub struct KindSpec {
    pub id: &'static str,
    pub links: &'static [Link],
    /// `None` when the driver cannot check profile values yet.
    pub schema: Option<Schema>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub severity: Severity,
    /// Dotted path in the config, for example `devices.kitchen.host`.
    pub location: String,
    pub message: String,
}

impl fmt::Display for Finding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let level = match self.severity {
            Severity::Warning => "warning",
            Severity::Error => "error",
        };
        write!(f, "{level}: {}: {}", self.location, self.message)
    }
}

pub fn validate(config: &Config, kinds: &[KindSpec]) -> Vec<Finding> {
    let mut findings = Vec::new();
    for (name, device) in &config.devices {
        validate_device(config, kinds, name, device, &mut findings);
    }
    findings
}

fn validate_device(
    config: &Config,
    kinds: &[KindSpec],
    name: &str,
    device: &Device,
    out: &mut Vec<Finding>,
) {
    let at = |field: &str| format!("devices.{name}.{field}");
    let mut error = |location: String, message: String| {
        out.push(Finding {
            severity: Severity::Error,
            location,
            message,
        })
    };

    if let Some(host) = &device.host
        && !config.hosts.contains_key(host)
    {
        error(at("host"), format!("unknown host {host:?}"));
    }

    match device.link {
        Link::Esphome => {
            if device.address.is_none() {
                error(at("address"), "the esphome link needs an address".into());
            }
            if device.host.is_some() {
                error(at("host"), "the esphome link does not use a host".into());
            }
        }
        Link::Usb | Link::Alsa => {
            if device.address.is_some() {
                error(
                    at("address"),
                    format!("the {} link does not use an address", device.link.as_str()),
                );
            }
            if device.noise_key_env.is_some() {
                error(
                    at("noise_key_env"),
                    format!("the {} link does not use a Noise key", device.link.as_str()),
                );
            }
        }
    }
    if device.link == Link::Esphome && !device.selector.is_empty() {
        error(at("match"), "the esphome link does not use match".into());
    }

    let profile = match &device.profile {
        Some(p) => match config.profiles.get(p) {
            Some(profile) => Some((p, profile)),
            None => {
                error(at("profile"), format!("unknown profile {p:?}"));
                None
            }
        },
        None => None,
    };

    let Some(kind) = kinds.iter().find(|k| k.id == device.kind) else {
        let known: Vec<&str> = kinds.iter().map(|k| k.id).collect();
        error(
            at("kind"),
            format!(
                "unknown kind {:?}, expected one of: {}",
                device.kind,
                known.join(", ")
            ),
        );
        return;
    };
    if !kind.links.contains(&device.link) {
        let links: Vec<&str> = kind.links.iter().map(|l| l.as_str()).collect();
        error(
            at("link"),
            format!(
                "kind {} does not support the {} link, expected one of: {}",
                kind.id,
                device.link.as_str(),
                links.join(", ")
            ),
        );
    }

    let Some((profile_name, profile)) = profile else {
        return;
    };
    if device.link == Link::Esphome {
        out.push(Finding {
            severity: Severity::Warning,
            location: at("profile"),
            message: format!(
                "values of profile {profile_name:?} are checked when veux connects: \
                 the esphome link reads its schema from the device"
            ),
        });
        return;
    }
    let Some(schema) = &kind.schema else {
        out.push(Finding {
            severity: Severity::Warning,
            location: at("profile"),
            message: format!(
                "values of profile {profile_name:?} are not checked: kind {} has no schema",
                kind.id
            ),
        });
        return;
    };
    for (id, raw) in &profile.values {
        let location = format!("profiles.{profile_name}.{id}");
        let message = match schema.get(id) {
            None => format!(
                "kind {} has no parameter {id} (used by device {name})",
                kind.id
            ),
            Some(spec) => match spec.parse(raw) {
                Ok(_) => continue,
                Err(e) => format!("{e} (used by device {name})"),
            },
        };
        out.push(Finding {
            severity: Severity::Error,
            location,
            message,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{Access, ParamSpec, ParamType};

    fn kinds() -> Vec<KindSpec> {
        let schema = Schema::new([ParamSpec {
            id: "gain.db".into(),
            group: "gain".into(),
            description: String::new(),
            ty: ParamType::Int { min: 0, max: 30 },
            unit: Some("dB".into()),
            access: Access::ReadWrite,
            persistent: true,
        }]);
        vec![
            KindSpec {
                id: "checked",
                links: &[Link::Usb, Link::Esphome],
                schema: Some(schema),
            },
            KindSpec {
                id: "unchecked",
                links: &[Link::Alsa],
                schema: None,
            },
        ]
    }

    fn run(text: &str) -> Vec<String> {
        let config = Config::parse(text).unwrap();
        validate(&config, &kinds())
            .iter()
            .map(ToString::to_string)
            .collect()
    }

    #[test]
    fn valid_config_has_no_findings() {
        let findings = run(r#"
            [hosts.pi]
            ssh = "me@pi"
            [devices.a]
            kind = "checked"
            link = "usb"
            host = "pi"
            profile = "p"
            [devices.b]
            kind = "checked"
            link = "esphome"
            address = "sat.local:6053"
            [profiles.p]
            gain.db = 12
        "#);
        assert!(findings.is_empty(), "{findings:?}");
    }

    #[test]
    fn reports_references_and_link_fields() {
        let findings = run(r#"
            [devices.a]
            kind = "checked"
            link = "esphome"
            host = "nowhere"
            profile = "missing"
        "#);
        assert_eq!(
            findings,
            [
                "error: devices.a.host: unknown host \"nowhere\"",
                "error: devices.a.address: the esphome link needs an address",
                "error: devices.a.host: the esphome link does not use a host",
                "error: devices.a.profile: unknown profile \"missing\"",
            ]
        );
    }

    #[test]
    fn reports_unknown_kind_and_unsupported_link() {
        let findings = run(r#"
            [devices.a]
            kind = "toaster"
            link = "usb"
            [devices.b]
            kind = "unchecked"
            link = "usb"
        "#);
        assert_eq!(
            findings,
            [
                "error: devices.a.kind: unknown kind \"toaster\", expected one of: checked, unchecked",
                "error: devices.b.link: kind unchecked does not support the usb link, expected one of: alsa",
            ]
        );
    }

    #[test]
    fn reports_bad_profile_values() {
        let findings = run(r#"
            [devices.a]
            kind = "checked"
            link = "usb"
            profile = "p"
            [profiles.p]
            gain.db = 99
            other = true
        "#);
        assert_eq!(
            findings,
            [
                "error: profiles.p.gain.db: 99 is out of range 0..=30 (used by device a)",
                "error: profiles.p.other: kind checked has no parameter other (used by device a)",
            ]
        );
    }

    #[test]
    fn esphome_profile_is_checked_at_connect_time() {
        let findings = run(r#"
            [devices.a]
            kind = "checked"
            link = "esphome"
            address = "sat.local:6053"
            profile = "p"
            [profiles.p]
            switch.wake_sound = false
        "#);
        assert_eq!(
            findings,
            [
                "warning: devices.a.profile: values of profile \"p\" are checked when veux connects: the esphome link reads its schema from the device"
            ]
        );
    }

    #[test]
    fn warns_when_kind_has_no_schema() {
        let findings = run(r#"
            [devices.a]
            kind = "unchecked"
            link = "alsa"
            profile = "p"
            [profiles.p]
            anything = 1
        "#);
        assert_eq!(
            findings,
            [
                "warning: devices.a.profile: values of profile \"p\" are not checked: kind unchecked has no schema"
            ]
        );
    }
}
