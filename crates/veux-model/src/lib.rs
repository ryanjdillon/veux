//! Device-independent model for veux: parameter schemas, typed values,
//! the `veux.toml` config, config validation, and the three-state diff.

pub mod config;
pub mod schema;
pub mod state;
pub mod validate;
pub mod value;

pub use config::{Config, ConfigError, Device, Host, Link, Profile};
pub use schema::{Access, ParamId, ParamSpec, ParamType, Schema};
pub use state::{Drift, ParamState, diff};
pub use validate::{Finding, KindSpec, Severity, validate};
pub use value::{Value, ValueError, Values};
