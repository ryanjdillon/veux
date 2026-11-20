//! The three states of a parameter: profile, runtime, and persisted.

use crate::schema::{Access, ParamId, Schema};
use crate::value::{Value, Values};

/// One parameter in the three states. `None` means the state has no value.
#[derive(Debug, Clone, PartialEq)]
pub struct ParamState {
    pub id: ParamId,
    pub profile: Option<Value>,
    pub runtime: Option<Value>,
    pub persisted: Option<Value>,
    pub drift: Drift,
}

/// The two actions that remove drift.
///
/// These two flags cover all three pairs of states. A difference between
/// profile and persisted shows as `apply` when the runtime value matches
/// persisted, or as `commit` when the runtime value matches the profile.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Drift {
    /// The profile sets a value that the known runtime value does not match.
    pub apply: bool,
    /// The runtime value is not the persisted value.
    pub commit: bool,
}

impl Drift {
    pub fn any(self) -> bool {
        self.apply || self.commit
    }
}

/// Compare the three states for each parameter in the schema.
///
/// `persisted` is `None` when the device cannot report persisted values.
/// Then no parameter shows commit drift. Likewise, a parameter without a
/// runtime value, for example on a device that is not connected, shows no
/// drift: its state is unknown, not different.
pub fn diff(
    schema: &Schema,
    profile: &Values,
    runtime: &Values,
    persisted: Option<&Values>,
) -> Vec<ParamState> {
    schema
        .params()
        .map(|spec| {
            let p = profile.get(&spec.id).cloned();
            let r = runtime.get(&spec.id).cloned();
            let s = persisted.and_then(|v| v.get(&spec.id)).cloned();
            let writable = spec.access == Access::ReadWrite;
            let drift = Drift {
                apply: writable && p.is_some() && r.is_some() && !same(p.as_ref(), r.as_ref()),
                commit: writable
                    && spec.persistent
                    && persisted.is_some()
                    && r.is_some()
                    && !same(r.as_ref(), s.as_ref()),
            };
            ParamState {
                id: spec.id.clone(),
                profile: p,
                runtime: r,
                persisted: s,
                drift,
            }
        })
        .collect()
}

fn same(a: Option<&Value>, b: Option<&Value>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => a.matches(b),
        (None, None) => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{ParamSpec, ParamType};

    fn param(id: &str, access: Access, persistent: bool) -> ParamSpec {
        ParamSpec {
            id: id.into(),
            group: "g".into(),
            description: String::new(),
            ty: ParamType::Int { min: 0, max: 100 },
            unit: None,
            access,
            persistent,
        }
    }

    fn values(pairs: &[(&str, i64)]) -> Values {
        pairs
            .iter()
            .map(|(id, v)| (ParamId::from(*id), Value::Int(*v)))
            .collect()
    }

    fn drift_of(states: &[ParamState], id: &str) -> Drift {
        states.iter().find(|s| s.id.as_str() == id).unwrap().drift
    }

    #[test]
    fn classifies_each_pair_of_states() {
        let schema = Schema::new([
            param("in_sync", Access::ReadWrite, true),
            param("not_applied", Access::ReadWrite, true),
            param("not_committed", Access::ReadWrite, true),
            param("both", Access::ReadWrite, true),
            param("no_profile", Access::ReadWrite, true),
        ]);
        let profile = values(&[
            ("in_sync", 1),
            ("not_applied", 2),
            ("not_committed", 3),
            ("both", 4),
        ]);
        let runtime = values(&[
            ("in_sync", 1),
            ("not_applied", 0),
            ("not_committed", 3),
            ("both", 0),
            ("no_profile", 5),
        ]);
        let persisted = values(&[
            ("in_sync", 1),
            ("not_applied", 0),
            ("not_committed", 0),
            ("both", 9),
            ("no_profile", 5),
        ]);
        let states = diff(&schema, &profile, &runtime, Some(&persisted));
        let d = |apply, commit| Drift { apply, commit };
        assert_eq!(drift_of(&states, "in_sync"), d(false, false));
        assert_eq!(drift_of(&states, "not_applied"), d(true, false));
        assert_eq!(drift_of(&states, "not_committed"), d(false, true));
        assert_eq!(drift_of(&states, "both"), d(true, true));
        assert_eq!(drift_of(&states, "no_profile"), d(false, false));
    }

    #[test]
    fn read_only_and_volatile_params_do_not_drift() {
        let schema = Schema::new([
            param("doa", Access::ReadOnly, false),
            param("volatile", Access::ReadWrite, false),
        ]);
        let profile = values(&[("doa", 1), ("volatile", 1)]);
        let runtime = values(&[("doa", 2), ("volatile", 1)]);
        let persisted = values(&[("doa", 3), ("volatile", 7)]);
        let states = diff(&schema, &profile, &runtime, Some(&persisted));
        assert!(!drift_of(&states, "doa").any());
        assert!(!drift_of(&states, "volatile").any());
    }

    #[test]
    fn unknown_runtime_state_gives_no_apply_drift() {
        let schema = Schema::new([param("x", Access::ReadWrite, true)]);
        let states = diff(&schema, &values(&[("x", 1)]), &Values::new(), None);
        assert!(!drift_of(&states, "x").any());
    }

    #[test]
    fn unknown_persisted_state_gives_no_commit_drift() {
        let schema = Schema::new([param("x", Access::ReadWrite, true)]);
        let states = diff(&schema, &Values::new(), &values(&[("x", 1)]), None);
        assert!(!drift_of(&states, "x").any());
        assert_eq!(states[0].persisted, None);
    }
}
