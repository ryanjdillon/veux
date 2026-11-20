//! What a parameter does, in plain words, for the TUI detail pane.
//!
//! Devices report little: an ESPHome entity reports only its name. These
//! explanations are curated from the sources that each entry cites. A key
//! is `<kind>/<link>/<parameter id>`, for example
//! `xvf3800/esphome/switch.beam_lock`.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use serde::Deserialize;
use veux_model::{Link, ParamId};

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Explanation {
    pub text: String,
    pub source: String,
}

const FILES: [(&str, &str); 4] = [
    ("esphome.toml", include_str!("explanations/esphome.toml")),
    (
        "xvf3800-usb.toml",
        include_str!("explanations/xvf3800-usb.toml"),
    ),
    ("wm8960.toml", include_str!("explanations/wm8960.toml")),
    ("apa102.toml", include_str!("explanations/apa102.toml")),
];

fn all() -> &'static BTreeMap<String, Explanation> {
    static ALL: OnceLock<BTreeMap<String, Explanation>> = OnceLock::new();
    ALL.get_or_init(|| {
        let mut map = BTreeMap::new();
        for (name, text) in FILES {
            let entries: BTreeMap<String, Explanation> = toml::from_str(text)
                .unwrap_or_else(|e| panic!("explanations/{name} does not parse: {e}"));
            for (key, mut entry) in entries {
                entry.text = reflow(&entry.text);
                assert!(
                    map.insert(key.clone(), entry).is_none(),
                    "explanation {key} is in two files"
                );
            }
        }
        map
    })
}

/// Join the lines of each paragraph, so that the TUI can wrap the text to
/// its own width. A blank line separates paragraphs.
fn reflow(text: &str) -> String {
    text.trim()
        .split("\n\n")
        .map(|p| p.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// The explanation of a parameter, if veux has one. A raw XVF3800
/// parameter on the ESPHome link is the same command as on USB, so it
/// gets the USB explanation.
pub fn explain(kind: &str, link: Link, id: &ParamId) -> Option<&'static Explanation> {
    let get = |link: Link| all().get(&format!("{kind}/{}/{id}", link.as_str()));
    get(link).or_else(|| (link == Link::Esphome).then(|| get(Link::Usb)).flatten())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{respeaker_2mic_v1, xvf3800};

    #[test]
    fn raw_parameters_on_esphome_get_the_usb_explanation() {
        let (key, usb) = all()
            .iter()
            .find(|(k, _)| k.starts_with("xvf3800/usb/"))
            .unwrap();
        let id = ParamId::from(key.rsplit('/').next().unwrap());
        assert_eq!(explain("xvf3800", Link::Esphome, &id), Some(usb));
    }

    #[test]
    fn reflow_joins_lines_and_keeps_paragraphs() {
        assert_eq!(reflow("\nOne\ntwo.\n\nThree\n"), "One two.\nThree");
    }

    #[test]
    fn every_entry_has_text_and_a_source() {
        for (key, e) in all() {
            assert!(!e.text.is_empty(), "{key} has no text");
            assert!(e.source.starts_with("https://"), "{key} has no URL source");
            assert_eq!(key.matches('/').count(), 2, "{key} is not kind/link/id");
        }
    }

    /// An explanation for a parameter that does not exist would never show.
    #[test]
    fn keys_name_real_parameters() {
        let usb = xvf3800::schema();
        let mixer = respeaker_2mic_v1::kind().schema.unwrap();
        for key in all().keys() {
            let mut parts = key.splitn(3, '/');
            let (kind, link, id) = (
                parts.next().unwrap(),
                parts.next().unwrap(),
                parts.next().unwrap(),
            );
            let id = ParamId::from(id);
            match (kind, link) {
                ("xvf3800", "usb") => assert!(usb.get(&id).is_some(), "{key}: no such command"),
                ("respeaker-2mic-v1", "alsa") => {
                    assert!(mixer.get(&id).is_some(), "{key}: no such mixer control")
                }
                // ESPHome entities come from the device, so only the form
                // can be checked here.
                ("xvf3800", "esphome") => assert!(id.as_str().contains('.'), "{key}"),
                _ => panic!("{key}: unknown kind and link"),
            }
        }
    }
}
