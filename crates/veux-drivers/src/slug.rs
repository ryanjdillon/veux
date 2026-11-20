//! Parameter IDs made from human-readable names.

/// Lower case ASCII letters and digits; each run of other characters becomes
/// one `_`. This gives the same slug as a Home Assistant entity ID.
pub fn slug(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for c in name.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.is_empty() && !out.ends_with('_') {
            out.push('_');
        }
    }
    while out.ends_with('_') {
        out.pop();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_matches_home_assistant() {
        assert_eq!(slug("AGC max gain (comms)"), "agc_max_gain_comms");
        assert_eq!(slug("Mute-unmute sound"), "mute_unmute_sound");
        assert_eq!(slug("  LED Ring Color Preset "), "led_ring_color_preset");
    }
}
