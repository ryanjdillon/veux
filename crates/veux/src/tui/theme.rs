//! Colours and borders of the TUI.
//!
//! `ligne-claire` follows the 1930s European comic style of that name: ink
//! outlines on cream paper, and flat colour. It paints its own background,
//! so it looks the same on dark and light terminals. `plain` uses the
//! terminal palette.

use clap::ValueEnum;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::BorderType;
use veux_model::{Access, ParamSpec, ParamType, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
pub enum ThemeName {
    #[default]
    LigneClaire,
    Plain,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Theme {
    /// Background of the whole screen. `None` keeps the terminal background.
    pub paper: Option<Color>,
    pub ink: Color,
    /// Read-only rows, hints, and offline devices.
    pub muted: Color,
    pub frame: Color,
    pub focus: Color,
    pub number: Color,
    pub on: Color,
    pub off: Color,
    pub option: Color,
    pub text: Color,
    pub drift: Color,
    pub commit: Color,
    pub online: Color,
    pub failed: Color,
    pub selected: Style,
    pub border: BorderType,
    /// Show a title card above the panes.
    pub banner: bool,
}

impl Theme {
    pub fn named(name: ThemeName) -> Self {
        match name {
            ThemeName::LigneClaire => Self::ligne_claire(),
            ThemeName::Plain => Self::plain(),
        }
    }

    /// Ink on cream paper. The colours are dark enough to read on the
    /// paper: each has a contrast ratio of at least 4.5:1 to it.
    pub fn ligne_claire() -> Self {
        let ink = Color::Rgb(0x1a, 0x1a, 0x1a);
        Self {
            paper: Some(Color::Rgb(0xf3, 0xea, 0xd3)),
            ink,
            muted: Color::Rgb(0x6e, 0x66, 0x5a),
            frame: ink,
            focus: Color::Rgb(0x1f, 0x4e, 0x8c),
            number: Color::Rgb(0x1f, 0x4e, 0x8c),
            on: Color::Rgb(0x1f, 0x4e, 0x8c),
            off: Color::Rgb(0xa8, 0x2a, 0x27),
            option: Color::Rgb(0x8a, 0x5a, 0x00),
            text: Color::Rgb(0x2a, 0x5e, 0x45),
            drift: Color::Rgb(0xa8, 0x2a, 0x27),
            commit: Color::Rgb(0x6b, 0x35, 0x7d),
            online: Color::Rgb(0x2a, 0x5e, 0x45),
            failed: Color::Rgb(0xa8, 0x2a, 0x27),
            selected: Style::new()
                .bg(Color::Rgb(0xe8, 0xc7, 0x66))
                .fg(ink)
                .add_modifier(Modifier::BOLD),
            border: BorderType::Plain,
            banner: true,
        }
    }

    pub fn plain() -> Self {
        Self {
            paper: None,
            ink: Color::Reset,
            muted: Color::DarkGray,
            frame: Color::DarkGray,
            focus: Color::Cyan,
            number: Color::Cyan,
            on: Color::Green,
            off: Color::Red,
            option: Color::Yellow,
            text: Color::Reset,
            drift: Color::Yellow,
            commit: Color::Magenta,
            online: Color::Green,
            failed: Color::Red,
            selected: Style::new().add_modifier(Modifier::REVERSED),
            border: BorderType::Plain,
            banner: false,
        }
    }

    /// Ink on paper: the style of the whole screen.
    pub fn base(&self) -> Style {
        let style = Style::new().fg(self.ink);
        match self.paper {
            Some(paper) => style.bg(paper),
            None => style,
        }
    }

    /// The style of a value cell. Read-only parameters are muted, so the
    /// eye finds the editable rows first.
    pub fn value(&self, spec: Option<&ParamSpec>, value: Option<&Value>) -> Style {
        let base = Style::new();
        let Some(spec) = spec else {
            return base.fg(self.muted);
        };
        if spec.access == Access::ReadOnly {
            return base.fg(self.muted);
        }
        let color = match (&spec.ty, value) {
            (_, None) => self.muted,
            (ParamType::Bool, Some(Value::Bool(true))) => self.on,
            (ParamType::Bool, Some(_)) => self.off,
            (ParamType::Enum { .. }, _) => self.option,
            (ParamType::Text { .. }, _) => self.text,
            (ParamType::Int { .. } | ParamType::Float { .. } | ParamType::Vector { .. }, _) => {
                self.number
            }
        };
        base.fg(color)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(ty: ParamType, access: Access) -> ParamSpec {
        ParamSpec {
            id: "g.x".into(),
            group: "g".into(),
            description: String::new(),
            ty,
            unit: None,
            access,
            persistent: true,
        }
    }

    #[test]
    fn values_are_coloured_by_type() {
        let t = Theme::ligne_claire();
        let rw = Access::ReadWrite;
        let on = t.value(Some(&spec(ParamType::Bool, rw)), Some(&Value::Bool(true)));
        let off = t.value(Some(&spec(ParamType::Bool, rw)), Some(&Value::Bool(false)));
        assert_eq!(on.fg, Some(t.on));
        assert_eq!(off.fg, Some(t.off));
        assert_ne!(on.fg, off.fg);
        let options = ParamType::Enum {
            variants: vec!["a".into()],
        };
        let option = t.value(Some(&spec(options, rw)), Some(&Value::Enum("a".into())));
        assert_eq!(option.fg, Some(t.option));
        let number = ParamType::Float { min: 0.0, max: 1.0 };
        let gain = t.value(Some(&spec(number, rw)), Some(&Value::Float(0.5)));
        assert_eq!(gain.fg, Some(t.number));
    }

    #[test]
    fn read_only_and_unknown_values_are_muted() {
        let t = Theme::plain();
        let ro = spec(ParamType::Int { min: 0, max: 9 }, Access::ReadOnly);
        assert_eq!(t.value(Some(&ro), Some(&Value::Int(3))).fg, Some(t.muted));
        let rw = spec(ParamType::Int { min: 0, max: 9 }, Access::ReadWrite);
        assert_eq!(t.value(Some(&rw), None).fg, Some(t.muted));
    }

    /// WCAG contrast ratio of two sRGB colours.
    fn contrast(a: Color, b: Color) -> f64 {
        let luminance = |c: Color| {
            let Color::Rgb(r, g, b) = c else {
                panic!("not an RGB colour: {c:?}");
            };
            let channel = |v: u8| {
                let v = f64::from(v) / 255.0;
                if v <= 0.03928 {
                    v / 12.92
                } else {
                    ((v + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b)
        };
        let (la, lb) = (luminance(a), luminance(b));
        (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
    }

    #[test]
    fn ligne_claire_colours_are_readable_on_the_paper() {
        let t = Theme::ligne_claire();
        let paper = t.paper.unwrap();
        for (name, color) in [
            ("ink", t.ink),
            ("muted", t.muted),
            ("number", t.number),
            ("off", t.off),
            ("option", t.option),
            ("text", t.text),
            ("commit", t.commit),
        ] {
            let ratio = contrast(color, paper);
            assert!(ratio >= 4.5, "{name} has contrast {ratio:.2} on the paper");
        }
        let selected = t.selected.bg.unwrap();
        assert!(contrast(t.ink, selected) >= 4.5);
    }
}
