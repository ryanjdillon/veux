//! The terminal UI.

pub mod app;
pub mod theme;
mod ui;

use std::time::Duration;

use anyhow::Result;
use ratatui::crossterm::event::{self, Event, KeyEventKind};
use veux_model::Config;

use app::App;
use theme::Theme;

pub fn run(config: &Config, path: &std::path::Path, theme: &Theme) -> Result<()> {
    let catalog = veux_drivers::catalog();
    let schema_of = |kind: &str| catalog.iter().find(|k| k.id == kind)?.schema.clone();
    let mut app = App::new(
        config,
        Some(path.to_path_buf()),
        schema_of,
        crate::connection::connector(config),
        Box::new(crate::connection::ConfigFirmware(config.clone())),
    );
    let mut terminal = ratatui::init();
    let result = (|| -> Result<()> {
        while !app.quit {
            terminal.draw(|frame| ui::draw(frame, &app, theme))?;
            // Without work to tick, wait for a key; else check for one and
            // tick, so the monitor updates and a tone plays after it shows.
            let key_ready = !app.wants_tick() || event::poll(Duration::from_millis(50))?;
            if key_ready
                && let Event::Key(key) = event::read()?
                && key.kind == KeyEventKind::Press
            {
                app.handle_key(key);
            }
            app.tick();
        }
        Ok(())
    })();
    ratatui::restore();
    result
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;

    #[test]
    fn renders_devices_and_schema_rows() {
        let config = Config::parse(
            r#"
            [devices.office-array]
            kind = "xvf3800"
            link = "usb"
            profile = "p"
            [devices.kitchen-hat]
            kind = "respeaker-2mic-v1"
            link = "alsa"
            [profiles.p]
            led.effect = 4
            "#,
        )
        .unwrap();
        let catalog = veux_drivers::catalog();
        let mut app = App::new(
            &config,
            None,
            |kind| catalog.iter().find(|k| k.id == kind)?.schema.clone(),
            Box::new(|_, _| anyhow::bail!("no hardware in tests")),
            Box::new(crate::connection::ConfigFirmware(config.clone())),
        );
        let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
        // Devices are ordered by name, so kitchen-hat is first.
        terminal
            .draw(|f| ui::draw(f, &app, &Theme::ligne_claire()))
            .unwrap();
        let screen = format!("{:?}", terminal.backend().buffer());
        assert!(screen.contains("kitchen-hat"));
        assert!(screen.contains("office-array"));
        assert!(screen.contains("mixer.3d_"));
        // The detail pane shows the highlighted parameter.
        assert!(screen.contains("Detail"));
        assert!(screen.contains("accepts"));

        app.selected = 1;
        terminal
            .draw(|f| ui::draw(f, &app, &Theme::ligne_claire()))
            .unwrap();
        let screen = format!("{:?}", terminal.backend().buffer());
        assert!(screen.contains("aec.aecconverged"));
        assert!(screen.contains("Persisted"));
    }
}
