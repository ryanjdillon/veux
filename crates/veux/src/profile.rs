//! A device's profile against its schema: the values that apply, and the
//! entries that do not.

use anyhow::{Result, anyhow};
use veux_model::{ParamId, Profile, Schema, Values};

/// Profile values that pass the schema. `veux check` reports the others.
pub fn profile_values(profile: Option<&Profile>, schema: Option<&Schema>) -> Values {
    let (Some(schema), Some(profile)) = (schema, profile) else {
        return Values::new();
    };
    profile
        .values
        .iter()
        .filter_map(|(id, raw)| {
            let value = schema.get(id)?.parse(raw).ok()?;
            Some((ParamId::clone(id), value))
        })
        .collect()
}

/// Profile entries that the schema rejects. Empty without a schema, because
/// then nothing can be checked.
pub fn profile_issues(profile: Option<&Profile>, schema: Option<&Schema>) -> Vec<String> {
    let (Some(schema), Some(profile)) = (schema, profile) else {
        return Vec::new();
    };
    profile
        .values
        .iter()
        .filter_map(|(id, raw)| match schema.get(id) {
            None => Some(format!("{id}: not on device")),
            Some(spec) => spec.parse(raw).err().map(|e| format!("{id}: {e}")),
        })
        .collect()
}

pub fn parse_toml_value(text: &str) -> Result<toml::Value> {
    let mut table: toml::Table = format!("v = {text}").parse()?;
    table.remove("v").ok_or_else(|| anyhow!("no value"))
}
