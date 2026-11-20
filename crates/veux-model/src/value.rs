//! Typed parameter values, and their conversion from TOML.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::schema::{Access, ParamId, ParamSpec, ParamType};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Value {
    Bool(bool),
    Int(i64),
    Float(#[serde(with = "non_finite")] f64),
    Enum(String),
    Vector(#[serde(with = "non_finite::vec")] Vec<f64>),
    Text(String),
}

/// JSON has no NaN or infinity, and `serde_json` writes them as `null`.
/// Devices report NaN, for example the XVF3800 direction without speech. So
/// a non-finite float travels as `null` and comes back as NaN.
mod non_finite {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(v: &f64, s: S) -> Result<S::Ok, S::Error> {
        v.is_finite().then_some(*v).serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<f64, D::Error> {
        Ok(Option::<f64>::deserialize(d)?.unwrap_or(f64::NAN))
    }

    pub mod vec {
        use serde::{Deserialize, Deserializer, Serialize, Serializer};

        pub fn serialize<S: Serializer>(v: &[f64], s: S) -> Result<S::Ok, S::Error> {
            let items: Vec<Option<f64>> = v.iter().map(|x| x.is_finite().then_some(*x)).collect();
            items.serialize(s)
        }

        pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<f64>, D::Error> {
            let items = Vec::<Option<f64>>::deserialize(d)?;
            Ok(items.into_iter().map(|x| x.unwrap_or(f64::NAN)).collect())
        }
    }
}

pub type Values = BTreeMap<ParamId, Value>;

impl Value {
    /// Compare two values. Floats use a relative tolerance, because devices
    /// round values to their own fixed-point formats.
    pub fn matches(&self, other: &Value) -> bool {
        match (self, other) {
            (Value::Float(a), Value::Float(b)) => close(*a, *b),
            (Value::Vector(a), Value::Vector(b)) => {
                a.len() == b.len() && a.iter().zip(b).all(|(x, y)| close(*x, *y))
            }
            _ => self == other,
        }
    }
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-6 * a.abs().max(b.abs()).max(1.0)
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Bool(v) => write!(f, "{v}"),
            Value::Int(v) => write!(f, "{v}"),
            Value::Float(v) => write!(f, "{}", short(*v)),
            Value::Enum(v) | Value::Text(v) => write!(f, "{v}"),
            Value::Vector(v) => {
                let items: Vec<String> = v.iter().map(|x| short(*x)).collect();
                write!(f, "[{}]", items.join(", "))
            }
        }
    }
}

/// A float with 6 significant digits. Devices send f32, and the full f64
/// shows the f32 rounding as noise: `0.8999999761581421` shows as `0.9`.
fn short(v: f64) -> String {
    if v == 0.0 || !v.is_finite() {
        return v.to_string();
    }
    let exp = v.abs().log10().floor() as i32;
    if !(-4..15).contains(&exp) {
        let s = format!("{v:.5e}");
        let (mantissa, exp) = s.split_once('e').unwrap_or((&s, "0"));
        return format!("{}e{exp}", trim_zeros(mantissa));
    }
    let decimals = (5 - exp).max(0) as usize;
    trim_zeros(&format!("{v:.decimals$}")).to_string()
}

fn trim_zeros(s: &str) -> &str {
    if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.')
    } else {
        s
    }
}

#[derive(Debug, Clone, PartialEq, Error)]
pub enum ValueError {
    #[error("expected {expected}, found {found}")]
    WrongType { expected: String, found: String },
    #[error("{value} is out of range {min}..={max}")]
    OutOfRange {
        value: String,
        min: String,
        max: String,
    },
    #[error("{value:?} is not one of: {}", variants.join(", "))]
    UnknownVariant {
        value: String,
        variants: Vec<String>,
    },
    #[error("expected {expected} elements, found {found}")]
    WrongLength { expected: usize, found: usize },
    #[error("text is {found} bytes, the maximum is {max}")]
    TooLong { found: usize, max: usize },
    #[error("parameter is read-only")]
    ReadOnly,
}

impl ParamSpec {
    /// Convert a TOML value from a profile into a typed value for this
    /// parameter.
    pub fn parse(&self, raw: &toml::Value) -> Result<Value, ValueError> {
        if self.access == Access::ReadOnly {
            return Err(ValueError::ReadOnly);
        }
        match &self.ty {
            ParamType::Bool => match raw {
                toml::Value::Boolean(v) => Ok(Value::Bool(*v)),
                _ => Err(wrong_type("a boolean", raw)),
            },
            ParamType::Int { min, max } => {
                let toml::Value::Integer(v) = raw else {
                    return Err(wrong_type("an integer", raw));
                };
                check_range(*v, *min, *max)?;
                Ok(Value::Int(*v))
            }
            ParamType::Float { min, max } => {
                let v = number(raw).ok_or_else(|| wrong_type("a number", raw))?;
                check_range(v, *min, *max)?;
                Ok(Value::Float(v))
            }
            ParamType::Enum { variants } => {
                let toml::Value::String(v) = raw else {
                    return Err(wrong_type("a string", raw));
                };
                if !variants.contains(v) {
                    return Err(ValueError::UnknownVariant {
                        value: v.clone(),
                        variants: variants.clone(),
                    });
                }
                Ok(Value::Enum(v.clone()))
            }
            ParamType::Vector { len, min, max } => {
                let toml::Value::Array(items) = raw else {
                    return Err(wrong_type("an array", raw));
                };
                if items.len() != *len {
                    return Err(ValueError::WrongLength {
                        expected: *len,
                        found: items.len(),
                    });
                }
                let mut out = Vec::with_capacity(items.len());
                for item in items {
                    let v = number(item).ok_or_else(|| wrong_type("a number", item))?;
                    check_range(v, *min, *max)?;
                    out.push(v);
                }
                Ok(Value::Vector(out))
            }
            ParamType::Text { max_len } => {
                let toml::Value::String(v) = raw else {
                    return Err(wrong_type("a string", raw));
                };
                if v.len() > *max_len {
                    return Err(ValueError::TooLong {
                        found: v.len(),
                        max: *max_len,
                    });
                }
                Ok(Value::Text(v.clone()))
            }
        }
    }
}

fn number(raw: &toml::Value) -> Option<f64> {
    match raw {
        toml::Value::Float(v) => Some(*v),
        toml::Value::Integer(v) => Some(*v as f64),
        _ => None,
    }
}

fn check_range<T: PartialOrd + ToString>(v: T, min: T, max: T) -> Result<(), ValueError> {
    if v < min || v > max {
        return Err(ValueError::OutOfRange {
            value: v.to_string(),
            min: min.to_string(),
            max: max.to_string(),
        });
    }
    Ok(())
}

fn wrong_type(expected: &str, found: &toml::Value) -> ValueError {
    ValueError::WrongType {
        expected: expected.to_string(),
        found: found.type_str().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn floats_show_six_significant_digits() {
        let shown = |v: f64| Value::Float(v).to_string();
        assert_eq!(shown(f64::from(0.9f32)), "0.9");
        assert_eq!(shown(f64::from(1.234_567_8f32)), "1.23457");
        assert_eq!(shown(f64::from(1e-9f32)), "1e-9");
        assert_eq!(shown(48000.0), "48000");
        assert_eq!(shown(-0.25), "-0.25");
        assert_eq!(shown(0.0), "0");
        assert_eq!(shown(f64::NAN), "NaN");
        assert_eq!(
            Value::Vector(vec![0.0, f64::from(0.1f32)]).to_string(),
            "[0, 0.1]"
        );
    }

    fn spec(ty: ParamType) -> ParamSpec {
        ParamSpec {
            id: "test.param".into(),
            group: "test".into(),
            description: String::new(),
            ty,
            unit: None,
            access: Access::ReadWrite,
            persistent: true,
        }
    }

    #[test]
    fn float_accepts_integer() {
        let s = spec(ParamType::Float {
            min: 0.0,
            max: 10.0,
        });
        assert_eq!(s.parse(&toml::Value::Integer(3)), Ok(Value::Float(3.0)));
    }

    #[test]
    fn int_rejects_out_of_range() {
        let s = spec(ParamType::Int { min: 0, max: 10 });
        assert!(matches!(
            s.parse(&toml::Value::Integer(11)),
            Err(ValueError::OutOfRange { .. })
        ));
    }

    #[test]
    fn enum_rejects_unknown_variant() {
        let s = spec(ParamType::Enum {
            variants: vec!["off".into(), "on".into()],
        });
        assert!(matches!(
            s.parse(&toml::Value::String("auto".into())),
            Err(ValueError::UnknownVariant { .. })
        ));
    }

    #[test]
    fn vector_checks_length_and_elements() {
        let s = spec(ParamType::Vector {
            len: 2,
            min: -1.0,
            max: 1.0,
        });
        let ok = toml::Value::Array(vec![toml::Value::Float(0.5), toml::Value::Integer(1)]);
        assert_eq!(s.parse(&ok), Ok(Value::Vector(vec![0.5, 1.0])));
        let short = toml::Value::Array(vec![toml::Value::Float(0.5)]);
        assert!(matches!(
            s.parse(&short),
            Err(ValueError::WrongLength { .. })
        ));
    }

    #[test]
    fn read_only_rejects_any_value() {
        let mut s = spec(ParamType::Bool);
        s.access = Access::ReadOnly;
        assert_eq!(
            s.parse(&toml::Value::Boolean(true)),
            Err(ValueError::ReadOnly)
        );
    }

    #[test]
    fn nan_survives_json() {
        let values = vec![Value::Float(f64::NAN), Value::Vector(vec![1.5, f64::NAN])];
        let json = serde_json::to_string(&values).unwrap();
        assert_eq!(json, r#"[{"Float":null},{"Vector":[1.5,null]}]"#);
        let back: Vec<Value> = serde_json::from_str(&json).unwrap();
        assert!(matches!(back[0], Value::Float(v) if v.is_nan()));
        assert!(matches!(&back[1], Value::Vector(v) if v[0] == 1.5 && v[1].is_nan()));
    }

    #[test]
    fn floats_match_within_tolerance() {
        assert!(Value::Float(1.0).matches(&Value::Float(1.0 + 1e-9)));
        assert!(!Value::Float(1.0).matches(&Value::Float(1.01)));
    }
}
