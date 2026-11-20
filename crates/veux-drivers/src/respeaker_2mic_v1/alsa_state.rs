//! Parse the text that `alsactl store` writes.
//!
//! The format is nested blocks of `key value` lines. A value is a bare word
//! or a string in single quotes. veux reads it, because one `alsactl` call
//! gives every control with its value, type, range, and items.

use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq)]
pub enum ControlType {
    Integer {
        min: i64,
        max: i64,
    },
    Boolean,
    Enumerated {
        items: Vec<String>,
    },
    /// A type that veux does not edit, for example IEC958 or BYTES.
    Other(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Control {
    pub numid: u32,
    pub iface: String,
    pub name: String,
    pub ty: ControlType,
    pub writable: bool,
    /// One value for each channel, as text.
    pub values: Vec<String>,
    /// Range in units of 0.01 dB, when the control has a dB scale.
    pub db_range: Option<(i64, i64)>,
}

/// The controls of one card in a state file that can hold several cards,
/// such as `/var/lib/alsa/asound.state`.
pub fn parse_card(text: &str, card: &str) -> Vec<Control> {
    parse_blocks(text, Some(card))
}

/// Parse a state file. Controls that the parser cannot read are skipped.
pub fn parse(text: &str) -> Vec<Control> {
    parse_blocks(text, None)
}

fn parse_blocks(text: &str, card: Option<&str>) -> Vec<Control> {
    let wanted = card.map(|c| format!("state.{c}"));
    let mut controls = Vec::new();
    // Path of open blocks, for example ["state.card", "control.3", "comment"].
    let mut path: Vec<String> = Vec::new();
    let mut fields: BTreeMap<String, String> = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if line == "}" {
            let in_card = wanted.as_ref().is_none_or(|w| path.first() == Some(w));
            if path.len() == 2
                && in_card
                && let Some(control) = control_from(&path[1], &fields)
            {
                controls.push(control);
            }
            if path.len() == 2 {
                fields.clear();
            }
            path.pop();
            continue;
        }
        let (key, rest) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
        let rest = rest.trim();
        if rest == "{" {
            path.push(key.to_string());
            continue;
        }
        if path.len() >= 2 {
            let prefix = if path.len() == 3 { "comment." } else { "" };
            fields.insert(format!("{prefix}{key}"), unquote(rest));
        }
    }
    controls
}

fn unquote(value: &str) -> String {
    value
        .strip_prefix('\'')
        .and_then(|v| v.strip_suffix('\''))
        .unwrap_or(value)
        .to_string()
}

fn control_from(block: &str, f: &BTreeMap<String, String>) -> Option<Control> {
    let numid = block.strip_prefix("control.")?.parse().ok()?;
    let ty = match f.get("comment.type")?.as_str() {
        "INTEGER" => {
            let (min, max) = f.get("comment.range")?.split_once(" - ")?;
            ControlType::Integer {
                min: min.trim().parse().ok()?,
                max: max.trim().parse().ok()?,
            }
        }
        "BOOLEAN" => ControlType::Boolean,
        "ENUMERATED" => {
            let items = (0..)
                .map_while(|i| f.get(&format!("comment.item.{i}")).cloned())
                .collect();
            ControlType::Enumerated { items }
        }
        other => ControlType::Other(other.to_string()),
    };
    let count: usize = f
        .get("comment.count")
        .and_then(|c| c.parse().ok())
        .unwrap_or(1);
    let values = if count == 1 {
        vec![f.get("value")?.clone()]
    } else {
        (0..count)
            .map(|i| f.get(&format!("value.{i}")).cloned())
            .collect::<Option<_>>()?
    };
    let db_range = match (f.get("comment.dbmin"), f.get("comment.dbmax")) {
        (Some(min), Some(max)) => Some((min.parse().ok()?, max.parse().ok()?)),
        _ => None,
    };
    Some(Control {
        numid,
        iface: f.get("iface")?.clone(),
        name: f.get("name")?.clone(),
        ty,
        writable: f
            .get("comment.access")
            .is_some_and(|a| a.split_whitespace().any(|w| w == "write")),
        values,
        db_range,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIVE: &str = include_str!("../../tests/fixtures/respeaker2mic.state");

    fn find<'a>(controls: &'a [Control], name: &str) -> &'a Control {
        controls.iter().find(|c| c.name == name).unwrap()
    }

    #[test]
    fn parse_card_keeps_only_that_card() {
        let two = format!(
            "{LIVE}\nstate.vc4hdmi {{\n\tcontrol.1 {{\n\t\tiface PCM\n\t\tname 'X'\n\t\tvalue true\n\t\tcomment {{\n\t\t\taccess 'read write'\n\t\t\ttype BOOLEAN\n\t\t\tcount 1\n\t\t}}\n\t}}\n}}\n"
        );
        assert_eq!(parse(&two).len(), 60);
        assert_eq!(parse_card(&two, "respeaker2mic").len(), 59);
        assert_eq!(parse_card(&two, "vc4hdmi").len(), 1);
        assert!(parse_card(&two, "other").is_empty());
    }

    #[test]
    fn parses_every_control_of_the_live_card() {
        let controls = parse(LIVE);
        assert_eq!(controls.len(), 59);
        assert!(controls.iter().all(|c| c.iface == "MIXER"));
    }

    #[test]
    fn parses_stereo_integer_with_db_range() {
        let controls = parse(LIVE);
        let c = find(&controls, "Capture Volume");
        assert_eq!(c.ty, ControlType::Integer { min: 0, max: 63 });
        assert_eq!(c.values, ["39", "39"]);
        assert_eq!(c.db_range, Some((-1725, 3000)));
        assert!(c.writable);
    }

    #[test]
    fn parses_boolean_and_enumerated() {
        let controls = parse(LIVE);
        let boost = find(&controls, "Left Input Mixer Boost Switch");
        assert_eq!(boost.ty, ControlType::Boolean);
        assert_eq!(boost.values, ["false"]);
        let polarity = find(&controls, "ADC Polarity");
        let ControlType::Enumerated { items } = &polarity.ty else {
            panic!("not enumerated: {:?}", polarity.ty);
        };
        assert_eq!(items.len(), 4);
        assert_eq!(polarity.values, ["No Inversion"]);
    }
}
