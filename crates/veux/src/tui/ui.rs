//! Draws the app state. The layout is generic: it reads only the schema.
//! All colours come from the theme.

use ratatui::Frame;
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Cell, Clear, List, ListItem, ListState, Paragraph, Row, Table,
    TableState,
};
use veux_model::{Access, ParamSpec, ParamType};

use super::app::{App, Edit, Focus, Prompt, Status};
use super::theme::Theme;

pub fn draw(frame: &mut Frame, app: &App, theme: &Theme) {
    let area = frame.area();
    // Paint the paper first, so every cell has the theme background.
    frame.render_widget(Block::new().style(theme.base()), area);
    let banner = if theme.banner { 3 } else { 0 };
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(banner),
        Constraint::Min(3),
        Constraint::Length(2),
    ])
    .areas(area);
    let [left, right] =
        Layout::horizontal([Constraint::Length(34), Constraint::Min(20)]).areas(body);
    if theme.banner {
        draw_banner(frame, app, theme, header);
    }
    draw_devices(frame, app, theme, left);
    draw_params(frame, app, theme, right);
    draw_footer(frame, app, theme, footer);
    if let Some(Edit::Choice { options, index }) = &app.edit {
        draw_choice(frame, app, theme, options, *index, right);
    }
    match &app.prompt {
        Some(Prompt::Quit) => draw_quit_prompt(frame, app, theme, area),
        Some(Prompt::Apply(values)) => draw_apply_prompt(frame, app, theme, values, area),
        Some(Prompt::Commit) => draw_commit_prompt(frame, app, theme, area),
        Some(Prompt::Tone) => draw_tone_prompt(frame, app, theme, area),
        Some(Prompt::Image { buffer }) => draw_image_prompt(frame, app, theme, buffer, area),
        Some(Prompt::Flash { path, size, sha256 }) => {
            draw_flash_prompt(frame, app, theme, (path, *size, sha256), area);
        }
        Some(Prompt::FlashBundled) => draw_bundled_flash_prompt(frame, app, theme, area),
        None => {}
    }
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let [area] = Layout::horizontal([Constraint::Length(width.min(area.width))])
        .flex(Flex::Center)
        .areas(area);
    let [area] = Layout::vertical([Constraint::Length(height.min(area.height))])
        .flex(Flex::Center)
        .areas(area);
    area
}

fn draw_popup(frame: &mut Frame, theme: &Theme, title: &str, lines: Vec<Line>, area: Rect) {
    let width = lines.iter().map(Line::width).max().unwrap_or(40) as u16 + 4;
    let popup = centered(area, width, lines.len() as u16 + 2);
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(lines).block(panel(theme, title, true)),
        popup,
    );
}

fn key_line(keys: &[(&str, &str)]) -> Line<'static> {
    let mut spans = Vec::new();
    for (key, what) in keys {
        spans.push(Span::styled(
            key.to_string(),
            Style::new().add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::raw(format!(" {what}   ")));
    }
    Line::from(spans)
}

fn draw_apply_prompt(
    frame: &mut Frame,
    app: &App,
    theme: &Theme,
    values: &veux_model::Values,
    area: Rect,
) {
    let Some(entry) = app.current() else {
        return;
    };
    let mut lines = vec![
        Line::from(format!(
            "Write {} profile value(s) to {}:",
            values.len(),
            entry.name
        )),
        Line::default(),
    ];
    for (id, v) in values.iter().take(10) {
        let now = entry
            .runtime
            .get(id)
            .map_or("-".to_string(), ToString::to_string);
        lines.push(Line::from(vec![
            Span::styled(format!("  {id}  "), Style::new().fg(theme.focus)),
            Span::styled(now, Style::new().fg(theme.muted)),
            Span::raw(" → "),
            Span::raw(v.to_string()),
        ]));
    }
    if values.len() > 10 {
        lines.push(Line::styled(
            format!("  … and {} more", values.len() - 10),
            Style::new().fg(theme.muted),
        ));
    }
    lines.push(Line::default());
    lines.push(Line::styled(
        "Runtime only: press c afterwards to persist.",
        Style::new().fg(theme.muted),
    ));
    lines.push(key_line(&[("y", "apply"), ("esc", "cancel")]));
    draw_popup(frame, theme, " Apply profile ", lines, area);
}

fn draw_commit_prompt(frame: &mut Frame, app: &App, theme: &Theme, area: Rect) {
    let Some(entry) = app.current() else {
        return;
    };
    let pending = if let Some(persisted) = &entry.persisted {
        format!(
            "{} value(s) differ from the {}.",
            entry.rows().iter().filter(|r| r.drift.commit).count(),
            if persisted.snapshot {
                "snapshot of the last commit"
            } else {
                "persisted state"
            }
        )
    } else {
        "The persisted state is unknown.".to_string()
    };
    let lines = vec![
        Line::from(format!(
            "Persist the runtime values of {} on the device?",
            entry.name
        )),
        Line::styled(pending, Style::new().fg(theme.muted)),
        Line::default(),
        key_line(&[("y", "persist"), ("esc", "cancel")]),
    ];
    draw_popup(frame, theme, " Commit ", lines, area);
}

fn draw_tone_prompt(frame: &mut Frame, app: &App, theme: &Theme, area: Rect) {
    let Some(entry) = app.current() else {
        return;
    };
    let lines = vec![
        Line::from(format!(
            "Play a {} Hz test tone on {}?",
            super::app::TONE_HZ,
            entry.name
        )),
        Line::styled(
            "Its level follows the playback volumes.",
            Style::new().fg(theme.muted),
        ),
        Line::default(),
        key_line(&[
            ("l", "left"),
            ("r", "right"),
            ("b", "both"),
            ("esc", "cancel"),
        ]),
    ];
    draw_popup(frame, theme, " Test tone ", lines, area);
}

fn draw_image_prompt(frame: &mut Frame, app: &App, theme: &Theme, buffer: &str, area: Rect) {
    let Some(entry) = app.current() else {
        return;
    };
    let lines = vec![
        Line::from(format!("The firmware image for {}:", entry.name)),
        Line::from(vec![
            Span::styled("> ", Style::new().fg(theme.focus)),
            Span::raw(format!("{buffer}█{}", " ".repeat(40))),
        ]),
        Line::default(),
        key_line(&[("enter", "check"), ("esc", "cancel")]),
    ];
    draw_popup(frame, theme, " Flash firmware ", lines, area);
}

fn draw_flash_prompt(
    frame: &mut Frame,
    app: &App,
    theme: &Theme,
    (path, size, sha256): (&str, usize, &str),
    area: Rect,
) {
    let Some(entry) = app.current() else {
        return;
    };
    let muted = Style::new().fg(theme.muted);
    let lines = vec![
        Line::from(format!(
            "Write this image to the upgrade slot of {}?",
            entry.name
        )),
        Line::default(),
        Line::from(format!("  {path}")),
        Line::from(format!("  {size} bytes")),
        Line::from(format!("  sha256 {sha256}")),
        Line::default(),
        Line::styled(
            "The device resets after the flash. An I2S image leaves the USB bus.",
            muted,
        ),
        Line::default(),
        key_line(&[("y", "flash"), ("esc", "cancel")]),
    ];
    draw_popup(frame, theme, " Flash firmware ", lines, area);
}

fn draw_bundled_flash_prompt(frame: &mut Frame, app: &App, theme: &Theme, area: Rect) {
    let Some(entry) = app.current() else {
        return;
    };
    let lines = vec![
        Line::from(format!(
            "Have the ESP32 of {} write the XVF3800 image of its ESPHome build?",
            entry.name
        )),
        Line::default(),
        Line::styled(
            "It writes the image even when the versions are the same. It takes some minutes.",
            Style::new().fg(theme.muted),
        ),
        Line::default(),
        key_line(&[("y", "flash"), ("esc", "cancel")]),
    ];
    draw_popup(frame, theme, " Flash firmware ", lines, area);
}

fn draw_firmware(frame: &mut Frame, app: &App, theme: &Theme, area: Rect) {
    let muted = Style::new().fg(theme.muted);
    let Some(pane) = &app.firmware else {
        return;
    };
    let mut lines = Vec::new();
    match &pane.info {
        Err(why) => lines.push(Line::from(why.clone())),
        Ok(info) => {
            let running = match (&info.version, info.dfu_mode) {
                (_, true) => "DFU only (Safe Mode or bootloader)".to_string(),
                (Some(v), false) => v.clone(),
                (None, false) => "version unknown".to_string(),
            };
            lines.push(Line::from(vec![
                Span::styled("running  ", muted),
                Span::styled(running, Style::new().fg(theme.number)),
            ]));
            lines.push(if info.bundled_image {
                Line::from(vec![
                    Span::styled("image    ", muted),
                    Span::raw(
                        "the image of the ESPHome build; an OTA update of the ESP32 changes it",
                    ),
                ])
            } else {
                Line::from(vec![
                    Span::styled("slots    ", muted),
                    Span::raw(info.slots.join(", ")),
                ])
            });
        }
    }
    if let Some(flash) = &pane.flash {
        let percent = (flash.sent * 100).checked_div(flash.total).unwrap_or(0);
        let state = match &flash.result {
            None if flash.image == super::app::BUNDLED_IMAGE => {
                Span::styled(format!("{percent:>3}%"), Style::new().fg(theme.focus))
            }
            None => Span::styled(
                format!("{percent:>3}%  {}/{} bytes", flash.sent, flash.total),
                Style::new().fg(theme.focus),
            ),
            Some(Ok(())) => Span::styled(
                "done; the device boots the new image",
                Style::new().fg(theme.online),
            ),
            Some(Err(e)) => Span::styled(format!("failed: {e}"), Style::new().fg(theme.failed)),
        };
        lines.push(Line::from(vec![
            Span::styled("flash    ", muted),
            Span::raw(format!("{}  ", flash.image)),
            state,
        ]));
    }
    lines.push(Line::styled(
        if pane.info.is_ok() {
            "F flash an image · f close"
        } else {
            "f close"
        },
        muted,
    ));
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(ratatui::widgets::Wrap { trim: false })
            .block(panel(theme, " Firmware ", false)),
        area,
    );
}

/// The colour of a level: clear below -18 dBFS, a warning up to -6 dBFS,
/// and near clipping above.
fn level_colour(theme: &Theme, dbfs: f64) -> ratatui::style::Color {
    if dbfs >= -6.0 {
        theme.failed
    } else if dbfs >= -18.0 {
        theme.drift
    } else {
        theme.online
    }
}

/// One meter row: name, a bar coloured by zone with the peak hold, and
/// the numbers.
fn meter_line(theme: &Theme, level: &veux_drivers::metrics::Level, width: u16) -> Line<'static> {
    use crate::device::{BAR_FLOOR_DBFS, bar_cells};
    const NAME: usize = 14;
    let numbers = format!(
        " rms {:>5.1}  peak {:>5.1} dBFS",
        level.rms_dbfs, level.peak_hold_dbfs
    );
    let bar = (width as usize)
        .saturating_sub(NAME + numbers.len() + 1)
        .max(8);
    let fill = bar_cells(level.rms_dbfs, bar);
    let hold = bar_cells(level.peak_hold_dbfs, bar).saturating_sub(1);
    let mut spans = vec![Span::styled(
        format!("{:NAME$.NAME$} ", level.name),
        Style::new().fg(theme.ink),
    )];
    for i in 0..bar {
        let at = BAR_FLOOR_DBFS * (1.0 - (i + 1) as f64 / bar as f64);
        let colour = level_colour(theme, at);
        spans.push(if i < fill {
            Span::styled("█", Style::new().fg(colour))
        } else if i == hold && level.peak_hold_dbfs > BAR_FLOOR_DBFS {
            Span::styled("▌", Style::new().fg(colour))
        } else {
            Span::styled("·", Style::new().fg(theme.muted))
        });
    }
    spans.push(Span::styled(numbers, Style::new().fg(theme.number)));
    Line::from(spans)
}

fn draw_monitor(frame: &mut Frame, app: &App, theme: &Theme, area: Rect) {
    let width = area.width.saturating_sub(2);
    let mut lines = Vec::new();
    match app.monitor.as_ref().and_then(|m| m.metrics.as_ref()) {
        None => lines.push(Line::styled("reading…", Style::new().fg(theme.muted))),
        Some(m) => {
            if let Some(note) = &m.note {
                lines.push(Line::styled(note.clone(), Style::new().fg(theme.muted)));
            }
            for level in &m.levels {
                lines.push(meter_line(theme, level, width));
            }
            let mut status = Vec::new();
            if let Some(speech) = m.speech {
                status.push(if speech {
                    Span::styled("speech", Style::new().fg(theme.on))
                } else {
                    Span::styled("quiet", Style::new().fg(theme.muted))
                });
            }
            if let Some(deg) = m.direction_deg {
                status.push(Span::raw(format!("   direction {deg:.0}°")));
            }
            if !status.is_empty() {
                lines.push(Line::from(status));
            }
        }
    }
    lines.push(Line::styled(
        "scale -60 … 0 dBFS · the bar is RMS, the mark is the peak of the last 1.5 s",
        Style::new().fg(theme.muted),
    ));
    frame.render_widget(
        Paragraph::new(lines).block(panel(theme, " Monitor ", false)),
        area,
    );
}

fn draw_quit_prompt(frame: &mut Frame, app: &App, theme: &Theme, area: Rect) {
    let names = app.unsaved_names();
    let mut lines = vec![
        Line::from(format!(
            "{} edited value(s) are not in veux.toml:",
            names.len()
        )),
        Line::default(),
    ];
    let shown = names.len().min(8);
    for name in &names[..shown] {
        lines.push(Line::styled(
            format!("  {name}"),
            Style::new().fg(theme.focus),
        ));
    }
    if names.len() > shown {
        lines.push(Line::styled(
            format!("  … and {} more", names.len() - shown),
            Style::new().fg(theme.muted),
        ));
    }
    lines.push(Line::default());
    lines.push(Line::from(vec![
        Span::styled("s", Style::new().add_modifier(Modifier::BOLD)),
        Span::raw(" save and quit   "),
        Span::styled("q", Style::new().add_modifier(Modifier::BOLD)),
        Span::raw(" quit without saving   "),
        Span::styled("esc", Style::new().add_modifier(Modifier::BOLD)),
        Span::raw(" stay"),
    ]));
    let width = lines.iter().map(Line::width).max().unwrap_or(40) as u16 + 4;
    let popup = centered(area, width, lines.len() as u16 + 2);
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(lines).block(panel(theme, " Unsaved edits ", true)),
        popup,
    );
}

/// A URL short enough for one line: the host and the last path segment.
/// The full URL is in the explanations file.
fn short_url(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let parts: Vec<&str> = rest.split('/').filter(|p| !p.is_empty()).collect();
    match parts.as_slice() {
        [] => String::new(),
        [only] => (*only).to_string(),
        [host, last] => format!("{host}/{last}"),
        [host, .., last] => format!("{host}/…/{last}"),
    }
}

/// How a parameter keeps its value, in words.
fn persistence(spec: &ParamSpec, link: veux_model::Link) -> &'static str {
    match (spec.access, link, spec.persistent) {
        (Access::ReadOnly, _, _) => "read-only: the device reports it",
        (_, veux_model::Link::Esphome, _) => {
            "the device saves it to flash by itself, about a minute after a change"
        }
        (_, _, true) => "runtime until a commit saves it on the device",
        (_, _, false) => "runtime only: lost at power-off",
    }
}

/// The highlighted parameter in full, at the bottom of the parameter pane.
fn draw_detail(frame: &mut Frame, app: &App, theme: &Theme, area: Rect) {
    let muted = Style::new().fg(theme.muted);
    let bold = Style::new().add_modifier(Modifier::BOLD);
    let (Some(entry), Some(row), Some(spec)) =
        (app.current(), app.selected_row(), selected_spec(app))
    else {
        frame.render_widget(
            Paragraph::new("").block(panel(theme, " Detail ", false)),
            area,
        );
        return;
    };
    let shown =
        |v: &Option<veux_model::Value>| v.as_ref().map_or("-".to_string(), ToString::to_string);
    let mut values = vec![
        Span::styled("profile ", muted),
        Span::styled(
            shown(&row.profile),
            theme.value(Some(&spec), row.profile.as_ref()),
        ),
        Span::styled("   runtime ", muted),
        Span::styled(
            shown(&row.runtime),
            theme.value(Some(&spec), row.runtime.as_ref()),
        ),
    ];
    if row.drift.apply {
        values.push(Span::styled(
            "   ≠ differs from the profile",
            Style::new().fg(theme.drift),
        ));
    }
    if app.unsaved.contains(&(app.selected, row.id.clone())) {
        values.push(Span::styled(
            "   edited, not saved (s)",
            Style::new().fg(theme.commit),
        ));
    }
    let explanation =
        veux_drivers::explain::explain(&entry.device.kind, entry.device.link, &row.id);
    let mut lines = vec![Line::from(vec![
        Span::styled(row.id.to_string(), bold),
        Span::styled(format!("  ·  {}", spec.group), muted),
        Span::styled(format!("  ·  {}", spec.description), muted),
    ])];
    if let Some(e) = explanation {
        lines.extend(e.text.lines().map(|l| Line::from(l.to_string())));
    }
    lines.extend([
        Line::from(vec![
            Span::styled("accepts ", muted),
            Span::raw(type_hint(&spec)),
        ]),
        Line::from(vec![
            Span::styled("keeps   ", muted),
            Span::raw(persistence(&spec, entry.device.link)),
        ]),
        Line::from(values),
    ]);
    if let Some(e) = explanation {
        lines.push(Line::styled(
            format!("source  {}", short_url(&e.source)),
            muted,
        ));
    }
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(ratatui::widgets::Wrap { trim: true })
            .block(panel(theme, " Detail ", false)),
        area,
    );
}

/// What a parameter accepts, in words: options, range, length, and unit.
pub fn type_hint(spec: &ParamSpec) -> String {
    // Limits at the size of the wire type say nothing useful.
    let limit = |v: f64| v.abs() < 1e9;
    let range = |min: f64, max: f64| {
        if limit(min) && limit(max) {
            format!("{min} … {max}")
        } else {
            "any".into()
        }
    };
    let mut hint = match &spec.ty {
        ParamType::Bool => "true | false".into(),
        ParamType::Int { min, max } => format!("integer {}", range(*min as f64, *max as f64)),
        ParamType::Float { min, max } => format!("number {}", range(*min, *max)),
        ParamType::Enum { variants } => variants.join(" | "),
        ParamType::Vector { len, min, max } => {
            format!("{len} numbers, each {}, as [a, b, …]", range(*min, *max))
        }
        ParamType::Text { max_len } if *max_len < 1 << 16 => {
            format!("text, up to {max_len} characters")
        }
        ParamType::Text { .. } => "text".into(),
    };
    if let Some(unit) = &spec.unit {
        hint = format!("{hint} {unit}");
    }
    if spec.access == Access::ReadOnly {
        hint = format!("read-only · {hint}");
    }
    hint
}

fn selected_spec(app: &App) -> Option<ParamSpec> {
    let row = app.selected_row()?;
    app.current()?.schema.as_ref()?.get(&row.id).cloned()
}

fn panel<'a>(theme: &Theme, title: &'a str, focused: bool) -> Block<'a> {
    let color = if focused { theme.focus } else { theme.frame };
    let title_style = if focused {
        Style::new().fg(theme.focus).add_modifier(Modifier::BOLD)
    } else {
        Style::new().fg(theme.ink)
    };
    Block::new()
        .borders(Borders::ALL)
        .border_type(theme.border)
        .border_style(Style::new().fg(color))
        .title(Span::styled(title, title_style))
        .style(theme.base())
}

/// A title card in the manner of a 1930s album cover: double rule, name,
/// and a fleuron.
fn draw_banner(frame: &mut Frame, app: &App, theme: &Theme, area: Rect) {
    let (name, status, color) = match app.current() {
        Some(d) => {
            let (text, color) = match &d.status {
                Status::Online(detail) => (format!("● {detail}"), theme.online),
                Status::Failed(_) => ("✗ unreachable".to_string(), theme.failed),
                Status::Offline => ("○ not connected".to_string(), theme.muted),
            };
            (d.name.clone(), text, color)
        }
        None => (String::new(), String::new(), theme.muted),
    };
    let line = Line::from(vec![
        Span::styled(" VEUX ", Style::new().add_modifier(Modifier::BOLD)),
        Span::styled("❦  ", Style::new().fg(theme.drift)),
        Span::styled(name, Style::new().add_modifier(Modifier::BOLD)),
        Span::raw("   "),
        Span::styled(status, Style::new().fg(color)),
    ]);
    let card = Paragraph::new(line).block(
        Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Double)
            .border_style(Style::new().fg(theme.ink))
            .style(theme.base()),
    );
    frame.render_widget(card, area);
}

fn draw_choice(
    frame: &mut Frame,
    app: &App,
    theme: &Theme,
    options: &[String],
    index: usize,
    area: Rect,
) {
    let title = app
        .selected_row()
        .map(|r| format!(" {} ", r.id))
        .unwrap_or_default();
    let widest = options
        .iter()
        .map(|o| o.chars().count())
        .chain([title.chars().count()])
        .max()
        .unwrap_or(0);
    let width = (widest as u16 + 6).min(area.width);
    let height = (options.len() as u16 + 2).min(area.height);
    let [popup] = Layout::horizontal([Constraint::Length(width)])
        .flex(Flex::Center)
        .areas(area);
    let [popup] = Layout::vertical([Constraint::Length(height)])
        .flex(Flex::Center)
        .areas(popup);
    let spec = selected_spec(app);
    let items: Vec<ListItem> = options
        .iter()
        .map(|o| {
            let value = match spec.as_ref().map(|s| &s.ty) {
                Some(ParamType::Bool) => veux_model::Value::Bool(o == "true"),
                _ => veux_model::Value::Enum(o.clone()),
            };
            ListItem::new(o.as_str()).style(theme.value(spec.as_ref(), Some(&value)))
        })
        .collect();
    let list = List::new(items)
        .block(panel(theme, &title, true))
        .highlight_style(theme.selected)
        .highlight_symbol("› ");
    let mut state = ListState::default().with_selected(Some(index));
    frame.render_widget(Clear, popup);
    frame.render_stateful_widget(list, popup, &mut state);
}

fn draw_devices(frame: &mut Frame, app: &App, theme: &Theme, area: Rect) {
    let items: Vec<ListItem> = app
        .devices
        .iter()
        .map(|d| {
            let (mark, color) = match d.status {
                Status::Offline => ("○", theme.muted),
                Status::Online(_) => ("●", theme.online),
                Status::Failed(_) => ("✗", theme.failed),
            };
            let drift = match d.drift_count() {
                0 => Span::raw(""),
                n => Span::styled(format!(" Δ{n}"), Style::new().fg(theme.drift)),
            };
            ListItem::new(vec![
                Line::from(vec![
                    Span::styled(format!("{mark} "), Style::new().fg(color)),
                    Span::styled(d.name.clone(), Style::new().add_modifier(Modifier::BOLD)),
                    drift,
                ]),
                Line::styled(
                    format!("  {} · {}", d.device.kind, d.location()),
                    Style::new().fg(theme.muted),
                ),
            ])
        })
        .collect();
    let list = List::new(items)
        .block(panel(theme, "Devices", app.focus == Focus::Devices))
        .highlight_style(theme.selected);
    let mut state = ListState::default().with_selected(Some(app.selected));
    frame.render_stateful_widget(list, area, &mut state);
}

fn draw_params(frame: &mut Frame, app: &App, theme: &Theme, area: Rect) {
    let focused = app.focus == Focus::Params;
    let Some(entry) = app.current() else {
        frame.render_widget(
            Paragraph::new("No devices in the config.").block(panel(theme, "Parameters", focused)),
            area,
        );
        return;
    };
    let title = match &entry.status {
        Status::Online(detail) => format!("{} · {detail}", entry.name),
        Status::Failed(error) => format!("{} · {error}", entry.name),
        Status::Offline => entry.name.clone(),
    };
    let Some(schema) = &entry.schema else {
        let text = format!("Kind {} has no schema yet.", entry.device.kind);
        frame.render_widget(
            Paragraph::new(text).block(panel(theme, &title, focused)),
            area,
        );
        return;
    };
    // An explanation needs room: the pane grows from 7 to 14 rows.
    let explained = app
        .selected_row()
        .and_then(|r| veux_drivers::explain::explain(&entry.device.kind, entry.device.link, &r.id))
        .is_some();
    let monitored = app
        .monitor
        .as_ref()
        .is_some_and(|m| m.device == app.selected);
    let firmware = app
        .firmware
        .as_ref()
        .is_some_and(|f| f.device == app.selected);
    let detail_rows = if explained && !monitored && !firmware {
        14
    } else {
        7
    };
    let [area, detail] =
        Layout::vertical([Constraint::Min(5), Constraint::Length(detail_rows)]).areas(area);
    if monitored {
        draw_monitor(frame, app, theme, detail);
    } else if firmware {
        draw_firmware(frame, app, theme, detail);
    } else {
        draw_detail(frame, app, theme, detail);
    }
    let dash = || "-".to_string();
    let rows: Vec<Row> = entry
        .rows()
        .into_iter()
        .map(|r| {
            let spec = schema.get(&r.id);
            let read_only = spec.is_none_or(|s| s.access == Access::ReadOnly);
            let marker = match (r.drift.apply, r.drift.commit) {
                (true, _) => Span::styled("≠", Style::new().fg(theme.drift)),
                (false, true) => Span::styled("●", Style::new().fg(theme.commit)),
                _ => Span::raw(" "),
            };
            let name_style = Style::new().fg(if read_only { theme.muted } else { theme.ink });
            let value_cell = |v: Option<veux_model::Value>| {
                let style = theme.value(spec, v.as_ref());
                Cell::from(Span::styled(v.map_or_else(dash, |v| v.to_string()), style))
            };
            Row::new(vec![
                Cell::from(marker),
                Cell::from(Span::styled(r.id.to_string(), name_style)),
                value_cell(r.profile),
                value_cell(r.runtime),
                value_cell(r.persisted),
                Cell::from(Span::styled(
                    spec.and_then(|s| s.unit.clone()).unwrap_or_default(),
                    Style::new().fg(theme.muted),
                )),
            ])
        })
        .collect();
    // The XVF3800 cannot report its flash: the column shows what veux
    // recorded at the last commit.
    let persisted = if entry.persisted.as_ref().is_some_and(|p| p.snapshot) {
        "Snapshot"
    } else {
        "Persisted"
    };
    let header = Row::new(["", "Parameter", "Profile", "Runtime", persisted, "Unit"]).style(
        Style::new()
            .fg(theme.ink)
            .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
    );
    let widths = [
        Constraint::Length(1),
        Constraint::Length(30),
        Constraint::Fill(1),
        Constraint::Fill(2),
        Constraint::Fill(1),
        Constraint::Length(8),
    ];
    let table = Table::new(rows, widths)
        .header(header)
        .block(panel(theme, &title, focused))
        .row_highlight_style(if focused {
            theme.selected
        } else {
            Style::new()
        });
    let mut state = TableState::default().with_selected(Some(app.param));
    frame.render_stateful_widget(table, area, &mut state);
}

fn draw_footer(frame: &mut Frame, app: &App, theme: &Theme, area: Rect) {
    let muted = Style::new().fg(theme.muted);
    let hint = selected_spec(app)
        .map(|s| type_hint(&s))
        .unwrap_or_default();
    let id = app
        .selected_row()
        .map(|r| r.id.to_string())
        .unwrap_or_default();
    let first = match &app.edit {
        Some(Edit::Text { buffer }) => Line::from(vec![
            Span::styled(format!("{id} = "), Style::new().fg(theme.focus)),
            Span::raw(buffer.clone()),
            Span::raw("█   "),
            Span::styled(hint, muted),
        ]),
        Some(Edit::Choice { .. }) => Line::styled(
            format!("{id}: choose a value"),
            Style::new().fg(theme.focus),
        ),
        None if !app.message.is_empty() => Line::from(app.message.clone()),
        None => Line::default(),
    };
    let hints = match app.edit {
        Some(Edit::Choice { .. }) => "j/k choose · enter write · esc cancel",
        Some(Edit::Text { .. }) => "enter write · esc cancel",
        None => {
            "j/k move · r connect · enter edit · u revert · a apply · c commit · s save · S save all · m monitor · t tone · f firmware · q quit"
        }
    };
    frame.render_widget(
        Paragraph::new(vec![first, Line::styled(hints, muted)]).style(theme.base()),
        area,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(ty: ParamType, unit: Option<&str>, access: Access) -> ParamSpec {
        ParamSpec {
            id: "g.x".into(),
            group: "g".into(),
            description: String::new(),
            ty,
            unit: unit.map(Into::into),
            access,
            persistent: true,
        }
    }

    #[test]
    fn short_url_keeps_host_and_file() {
        assert_eq!(
            short_url("https://github.com/a/b/blob/542a05d/x/y.cpp#L4-L9"),
            "github.com/…/y.cpp#L4-L9"
        );
        assert_eq!(
            short_url("https://www.cirrus.com/wm8960"),
            "www.cirrus.com/wm8960"
        );
    }

    #[test]
    fn type_hint_names_range_options_and_unit() {
        let rw = Access::ReadWrite;
        let float = ParamType::Float { min: 1.0, max: 8.0 };
        assert_eq!(type_hint(&spec(float, Some("dB"), rw)), "number 1 … 8 dB");
        let wide = ParamType::Int {
            min: i32::MIN.into(),
            max: i32::MAX.into(),
        };
        assert_eq!(type_hint(&spec(wide, None, rw)), "integer any");
        let options = ParamType::Enum {
            variants: vec!["low".into(), "high".into()],
        };
        assert_eq!(type_hint(&spec(options, None, rw)), "low | high");
        let text = ParamType::Text {
            max_len: usize::MAX,
        };
        assert_eq!(
            type_hint(&spec(text, None, Access::ReadOnly)),
            "read-only · text"
        );
    }
}
