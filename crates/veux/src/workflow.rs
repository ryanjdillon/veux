//! The declarative workflow on the CLI: plan, apply, commit, and export.
//!
//! - `plan` shows where devices differ from their profiles (apply drift) and
//!   where runtime values are not persisted (commit drift).
//! - `apply` writes profile values to runtime. It never persists.
//! - `commit` makes runtime values survive a power cycle.
//! - `export` prints runtime values as a profile, to paste into veux.toml.

use std::io::{BufRead, Write};
use std::process::ExitCode;

use anyhow::{Result, anyhow, bail};
use veux_drivers::Device;
use veux_model::{Config, Link, ParamState, Schema, Values, diff};

use crate::connection::{Connector, connector};
use crate::profile::{profile_issues, profile_values};

/// One device, read in full.
struct DeviceState {
    device: Box<dyn Device>,
    schema: Schema,
    profile: Values,
    issues: Vec<String>,
    runtime: Values,
    persisted: Option<veux_drivers::Persisted>,
    link: Link,
}

impl DeviceState {
    fn rows(&self) -> Vec<ParamState> {
        diff(
            &self.schema,
            &self.profile,
            &self.runtime,
            self.persisted.as_ref().map(|p| &p.values),
        )
    }

    fn apply_values(&self) -> Values {
        self.rows()
            .into_iter()
            .filter(|r| r.drift.apply)
            .filter_map(|r| Some((r.id, r.profile?)))
            .collect()
    }

    fn commit_count(&self) -> usize {
        self.rows().iter().filter(|r| r.drift.commit).count()
    }
}

/// The named devices, or all devices in the config.
fn selected<'a>(config: &'a Config, names: &'a [String]) -> Result<Vec<&'a str>> {
    if names.is_empty() {
        return Ok(config.devices.keys().map(String::as_str).collect());
    }
    for name in names {
        if !config.devices.contains_key(name) {
            bail!("unknown device {name:?}");
        }
    }
    Ok(names.iter().map(String::as_str).collect())
}

fn load(config: &Config, name: &str, connect: &mut Connector) -> Result<DeviceState> {
    let device_config = &config.devices[name];
    let mut device = connect(name, device_config)?;
    let schema = device.schema();
    let profile_raw = device_config
        .profile
        .as_ref()
        .and_then(|p| config.profiles.get(p));
    let profile = profile_values(profile_raw, Some(&schema));
    let issues = profile_issues(profile_raw, Some(&schema));
    let runtime = device.read_all()?;
    let persisted = device.read_persisted()?;
    Ok(DeviceState {
        device,
        schema,
        profile,
        issues,
        runtime,
        persisted,
        link: device_config.link,
    })
}

fn shown(v: &Option<veux_model::Value>) -> String {
    v.as_ref().map_or("-".to_string(), ToString::to_string)
}

fn confirm(question: &str) -> Result<bool> {
    print!("{question} Type yes: ");
    std::io::stdout().flush()?;
    let mut answer = String::new();
    std::io::stdin().lock().read_line(&mut answer)?;
    Ok(answer.trim() == "yes")
}

pub fn plan(config: &Config, names: &[String]) -> Result<ExitCode> {
    let mut connect = connector(config);
    let (mut to_apply, mut to_commit, mut failed) = (0, 0, 0);
    for name in selected(config, names)? {
        let state = match load(config, name, &mut connect) {
            Ok(state) => state,
            Err(e) => {
                println!("{name}: unreachable: {e:#}");
                failed += 1;
                continue;
            }
        };
        println!("{name}");
        let rows = state.rows();
        let width = rows.iter().map(|r| r.id.as_str().len()).max().unwrap_or(0);
        for r in rows.iter().filter(|r| r.drift.apply) {
            println!(
                "  ≠ {:width$}  profile {}  runtime {}   apply",
                r.id.as_str(),
                shown(&r.profile),
                shown(&r.runtime)
            );
        }
        for r in rows.iter().filter(|r| r.drift.commit) {
            println!(
                "  ● {:width$}  runtime {}  persisted {}   commit",
                r.id.as_str(),
                shown(&r.runtime),
                shown(&r.persisted)
            );
        }
        for issue in &state.issues {
            println!("  ! profile entry not applied: {issue}");
        }
        let apply = rows.iter().filter(|r| r.drift.apply).count();
        let commit = state.commit_count();
        if apply + commit == 0 {
            println!("  in sync: {} parameters", rows.len());
        }
        if state.persisted.is_none() {
            let why = match state.link {
                Link::Esphome => "the device saves by itself",
                Link::Usb => "no snapshot yet; commit records one",
                _ => "the device does not report it",
            };
            println!("  persisted state unknown: {why}");
        } else if state.persisted.as_ref().is_some_and(|p| p.snapshot) {
            println!("  persisted state: the snapshot of the last veux commit");
        }
        to_apply += apply;
        to_commit += commit;
    }
    println!("\n{to_apply} to apply, {to_commit} to commit, {failed} unreachable");
    Ok(ExitCode::SUCCESS)
}

pub fn apply(config: &Config, names: &[String], yes: bool) -> Result<ExitCode> {
    let mut connect = connector(config);
    let mut failed = false;
    for name in selected(config, names)? {
        let mut state = match load(config, name, &mut connect) {
            Ok(state) => state,
            Err(e) => {
                println!("{name}: unreachable: {e:#}");
                failed = true;
                continue;
            }
        };
        let values = state.apply_values();
        if values.is_empty() {
            println!("{name}: nothing to apply");
            continue;
        }
        println!("{name}:");
        for (id, v) in &values {
            println!("  {id}: {} → {v}", shown(&state.runtime.get(id).cloned()));
        }
        if !yes && !confirm(&format!("Apply {} value(s) to {name}?", values.len()))? {
            println!("  skipped");
            continue;
        }
        state.device.write(&values)?;
        let after = state.device.read_all()?;
        let left = values
            .iter()
            .filter(|(id, v)| after.get(*id).is_none_or(|a| !a.matches(v)))
            .count();
        state.runtime = after;
        if left > 0 {
            println!("  applied, but {left} value(s) read back different");
            failed = true;
        } else if state.link == Link::Esphome {
            println!(
                "  applied {} value(s); the device saves them by itself",
                values.len()
            );
        } else if state.persisted.is_some() && state.commit_count() == 0 {
            println!(
                "  applied {} value(s); they match the persisted state",
                values.len()
            );
        } else {
            println!(
                "  applied {} value(s); runtime only, run `veux commit {name}` to keep them",
                values.len()
            );
        }
    }
    Ok(if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

pub fn commit(config: &Config, names: &[String], yes: bool) -> Result<ExitCode> {
    let mut connect = connector(config);
    let mut failed = false;
    for name in selected(config, names)? {
        let mut state = match load(config, name, &mut connect) {
            Ok(state) => state,
            Err(e) => {
                println!("{name}: unreachable: {e:#}");
                failed = true;
                continue;
            }
        };
        if state.link == Link::Esphome {
            println!("{name}: nothing to do; the device saves its values by itself");
            continue;
        }
        let pending = match &state.persisted {
            Some(_) => format!(
                "{} value(s) differ from the persisted state",
                state.commit_count()
            ),
            None => "the persisted state is unknown".to_string(),
        };
        if !yes && !confirm(&format!("{name}: {pending}. Persist the runtime values?"))? {
            println!("  skipped");
            continue;
        }
        state.device.persist()?;
        println!("{name}: persisted");
    }
    Ok(if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

pub fn export(config: &Config, name: &str) -> Result<ExitCode> {
    let device_config = config
        .devices
        .get(name)
        .ok_or_else(|| anyhow!("unknown device {name:?}"))?;
    let mut connect = connector(config);
    let state = load(config, name, &mut connect)?;
    let profile = device_config.profile.as_deref().unwrap_or(name);
    println!("[profiles.{profile}]");
    for spec in state.schema.params() {
        if spec.access != veux_model::Access::ReadWrite {
            continue;
        }
        let Some(value) = state.runtime.get(&spec.id) else {
            continue;
        };
        match crate::config_edit::to_item(value) {
            Ok(item) => println!("{} = {}", spec.id, item.to_string().trim()),
            Err(e) => println!("# {}: {e}", spec.id),
        }
    }
    Ok(ExitCode::SUCCESS)
}
