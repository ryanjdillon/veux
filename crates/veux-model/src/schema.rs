//! The parameter schema that each driver declares.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

/// A dotted parameter name, for example `aec.enabled`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ParamId(String);

impl ParamId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for ParamId {
    fn from(id: &str) -> Self {
        Self::new(id)
    }
}

impl fmt::Display for ParamId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ParamType {
    Bool,
    Int { min: i64, max: i64 },
    Float { min: f64, max: f64 },
    Enum { variants: Vec<String> },
    Vector { len: usize, min: f64, max: f64 },
    Text { max_len: usize },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Access {
    ReadOnly,
    ReadWrite,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParamSpec {
    pub id: ParamId,
    pub group: String,
    pub description: String,
    pub ty: ParamType,
    pub unit: Option<String>,
    pub access: Access,
    /// True when the device can store the value across a power cycle.
    pub persistent: bool,
}

/// All parameters of one device kind, ordered by ID. On the wire it is a
/// list of parameter specs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(into = "Vec<ParamSpec>", try_from = "Vec<ParamSpec>")]
pub struct Schema {
    params: BTreeMap<ParamId, ParamSpec>,
}

impl Schema {
    /// Panics on a duplicate ID, because that is a defect in the driver.
    pub fn new(params: impl IntoIterator<Item = ParamSpec>) -> Self {
        Self::try_new(params).unwrap_or_else(|e| panic!("{e}"))
    }

    /// Like [`Schema::new`], but returns an error for a schema that comes
    /// from outside the program.
    pub fn try_new(params: impl IntoIterator<Item = ParamSpec>) -> Result<Self, String> {
        let mut map = BTreeMap::new();
        for spec in params {
            let id = spec.id.clone();
            if map.insert(id.clone(), spec).is_some() {
                return Err(format!("duplicate parameter ID in schema: {id}"));
            }
        }
        Ok(Self { params: map })
    }

    pub fn get(&self, id: &ParamId) -> Option<&ParamSpec> {
        self.params.get(id)
    }

    pub fn params(&self) -> impl Iterator<Item = &ParamSpec> {
        self.params.values()
    }
}

impl From<Schema> for Vec<ParamSpec> {
    fn from(schema: Schema) -> Self {
        schema.params.into_values().collect()
    }
}

impl TryFrom<Vec<ParamSpec>> for Schema {
    type Error = String;

    fn try_from(params: Vec<ParamSpec>) -> Result<Self, String> {
        Self::try_new(params)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(id: &str) -> ParamSpec {
        ParamSpec {
            id: id.into(),
            group: "g".into(),
            description: String::new(),
            ty: ParamType::Text {
                max_len: usize::MAX,
            },
            unit: None,
            access: Access::ReadOnly,
            persistent: false,
        }
    }

    #[test]
    fn schema_round_trips_as_json() {
        let schema = Schema::new([spec("g.b"), spec("g.a")]);
        let json = serde_json::to_string(&schema).unwrap();
        assert_eq!(serde_json::from_str::<Schema>(&json).unwrap(), schema);
    }

    #[test]
    fn schema_from_json_rejects_duplicates() {
        let json = serde_json::to_string(&vec![spec("g.a"), spec("g.a")]).unwrap();
        assert!(serde_json::from_str::<Schema>(&json).is_err());
    }
}
