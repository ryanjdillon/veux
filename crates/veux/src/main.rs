use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Result, bail};
use clap::{Parser, Subcommand};
use veux_model::{Config, Severity};

mod config_edit;
mod connection;
mod device;
mod profile;
mod remote;
mod tui;
mod workflow;

#[derive(Parser)]
#[command(version, about = "Configure ReSpeaker audio devices")]
struct Cli {
    /// Config file. Default: ./veux.toml, then $XDG_CONFIG_HOME/veux/veux.toml.
    #[arg(long, global = true)]
    config: Option<PathBuf>,

    /// Colour theme of the TUI.
    #[arg(long, value_enum, default_value_t)]
    theme: tui::theme::ThemeName,

    /// No command opens the TUI.
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Validate the config file against the device drivers.
    Check,
    /// List the XVF3800 devices on local USB, with version and values.
    Probe,
    /// Read runtime values of a device. No IDs means all parameters.
    Read { device: String, ids: Vec<String> },
    /// Write runtime values to a device, as ID=VALUE in TOML syntax.
    Write {
        device: String,
        #[arg(required = true)]
        assignments: Vec<String>,
    },
    /// Show where devices differ from their profiles, and what is not
    /// persisted. No device names means all devices.
    Plan { devices: Vec<String> },
    /// Write profile values to the runtime of devices, after a confirmation.
    Apply {
        devices: Vec<String>,
        /// Do not ask for confirmation.
        #[arg(long)]
        yes: bool,
    },
    /// Make the runtime values of devices survive a power cycle.
    Commit {
        devices: Vec<String>,
        /// Do not ask for confirmation.
        #[arg(long)]
        yes: bool,
    },
    /// Print the runtime values of a device as a profile for veux.toml.
    Export { device: String },
    /// Show the firmware of a device, or flash an image to it after a
    /// confirmation.
    Firmware {
        device: String,
        image: Option<PathBuf>,
        /// Have an ESPHome device write the XVF3800 image of its build.
        #[arg(long, conflicts_with = "image")]
        bundled: bool,
        /// Do not ask for confirmation.
        #[arg(long)]
        yes: bool,
    },
    /// Show live microphone levels, and direction and speech where the
    /// device reports them.
    Meter {
        device: String,
        /// Stop after this many seconds.
        #[arg(long, default_value_t = 10)]
        seconds: u64,
    },
    /// Play a sine test tone on the output of a device.
    Tone {
        device: String,
        #[arg(long, value_enum, default_value_t = device::Channel::Both)]
        channel: device::Channel,
        #[arg(long, default_value_t = 440)]
        frequency: u32,
    },
    /// Serve the devices of this host on stdin and stdout. veux starts this
    /// on a remote host over SSH.
    #[command(hide = true)]
    Helper,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<ExitCode> {
    match cli.command {
        Some(Command::Probe) => return device::probe(),
        Some(Command::Helper) => {
            let stdin = std::io::stdin().lock();
            veux_rpc::serve(stdin, std::io::stdout().lock(), &mut veux_rpc::LocalBackend)?;
            return Ok(ExitCode::SUCCESS);
        }
        _ => {}
    }
    let path = config_path(cli.config)?;
    let Some(command) = cli.command else {
        tui::run(
            &Config::load(&path)?,
            &path,
            &tui::theme::Theme::named(cli.theme),
        )?;
        return Ok(ExitCode::SUCCESS);
    };
    match command {
        Command::Check => check(&path),
        Command::Read { device, ids } => device::read(&Config::load(&path)?, &device, &ids),
        Command::Write {
            device,
            assignments,
        } => device::write(&Config::load(&path)?, &device, &assignments),
        Command::Plan { devices } => workflow::plan(&Config::load(&path)?, &devices),
        Command::Apply { devices, yes } => workflow::apply(&Config::load(&path)?, &devices, yes),
        Command::Commit { devices, yes } => workflow::commit(&Config::load(&path)?, &devices, yes),
        Command::Export { device } => workflow::export(&Config::load(&path)?, &device),
        Command::Firmware {
            device,
            image,
            bundled,
            yes,
        } => {
            let flash = match (image.as_deref(), bundled) {
                (Some(path), _) => Some(device::Flash::Image(path)),
                (None, true) => Some(device::Flash::Bundled),
                (None, false) => None,
            };
            device::firmware(&Config::load(&path)?, &device, flash, yes)
        }
        Command::Meter { device, seconds } => {
            device::meter(&Config::load(&path)?, &device, seconds)
        }
        Command::Tone {
            device,
            channel,
            frequency,
        } => device::tone(&Config::load(&path)?, &device, channel, frequency),
        Command::Probe | Command::Helper => unreachable!(),
    }
}

fn check(path: &Path) -> Result<ExitCode> {
    let config = Config::load(path)?;
    let findings = veux_model::validate(&config, &veux_drivers::catalog());
    for finding in &findings {
        println!("{finding}");
    }
    let errors = findings
        .iter()
        .filter(|f| f.severity == Severity::Error)
        .count();
    println!(
        "{}: {} devices, {} errors, {} warnings",
        path.display(),
        config.devices.len(),
        errors,
        findings.len() - errors
    );
    Ok(if errors == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

fn config_path(explicit: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(path) = explicit {
        return Ok(expand_home(path, std::env::var_os("HOME")));
    }
    let local = PathBuf::from("veux.toml");
    if local.exists() {
        return Ok(local);
    }
    let config_home = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")));
    if let Some(dir) = config_home {
        let path = dir.join("veux").join("veux.toml");
        if path.exists() {
            return Ok(path);
        }
    }
    bail!("no config file found: pass --config, or create ./veux.toml")
}

/// Replace a leading `~` with the home directory. A shell does not expand
/// `~` after `=`, as in `--config=~/veux.toml`.
fn expand_home(path: PathBuf, home: Option<std::ffi::OsString>) -> PathBuf {
    match (path.strip_prefix("~"), home) {
        (Ok(rest), Some(home)) => PathBuf::from(home).join(rest),
        _ => path,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_a_leading_tilde_only() {
        let home = Some("/home/u".into());
        assert_eq!(
            expand_home("~/.config/veux.toml".into(), home.clone()),
            PathBuf::from("/home/u/.config/veux.toml")
        );
        assert_eq!(
            expand_home("a/~/b".into(), home.clone()),
            PathBuf::from("a/~/b")
        );
        assert_eq!(expand_home("~x".into(), home), PathBuf::from("~x"));
    }
}
