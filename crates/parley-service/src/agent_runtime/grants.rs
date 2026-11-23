//! Immutable run grants and private principal construction (UCR-001 Q4).
//!
//! A grant is fixed when the owner starts a run. It is the intersection of
//! owner authority, an explicit permission subset and an explicit target set.
//! Tool input, provider text and claimed roles never contribute authority.
//! The only constructor of an agent principal lives here and requires a
//! server-created, durable grant.

use serde::{Deserialize, Serialize};

use crate::commands::policy::Permission;

/// Maximum explicitly granted target records per run (Q4).
pub(crate) const MAX_TARGETS: usize = 32;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GrantTarget {
    pub kind: String,
    pub id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Grant {
    pub permissions: Vec<String>,
    pub targets: Vec<GrantTarget>,
    pub allow_created_opportunities: bool,
}

/// A private, server-derived actor. No public HTTP handler can construct one:
/// the constructor takes the durable run row values as arguments.
#[derive(Clone, Debug)]
pub(crate) struct AgentPrincipal {
    /// Opaque pseudonym distinct from the owner's session pseudonym.
    pub pseudonym: String,
    pub run_id: String,
    pub epoch: String,
    pub lease_token: String,
}

impl AgentPrincipal {
    pub(crate) fn from_grant(run_id: &str, epoch: &str, lease_token: &str, secret: &[u8]) -> Self {
        let digest = parley_domain::hmac::sign_hex(
            secret,
            b"agent-principal",
            format!("{epoch}\0{run_id}").as_bytes(),
        );
        Self {
            pseudonym: format!("agent_{digest}"),
            run_id: run_id.to_owned(),
            epoch: epoch.to_owned(),
            lease_token: lease_token.to_owned(),
        }
    }
}

/// Validate a client-supplied run request into a closed, deduplicated grant.
pub(crate) fn build_grant(
    permissions: Vec<String>,
    targets: Vec<GrantTarget>,
    allow_created_opportunities: bool,
) -> Result<Grant, String> {
    if permissions.is_empty() || permissions.len() > Permission::all().len() {
        return Err("permissions must be a non-empty subset of the supported set".into());
    }
    let mut seen_permissions = std::collections::BTreeSet::new();
    for permission in &permissions {
        if Permission::parse(permission).is_none() {
            return Err(format!("unsupported permission {permission:?}"));
        }
        if !seen_permissions.insert(permission.clone()) {
            return Err("duplicate permission".into());
        }
    }
    if targets.is_empty() || targets.len() > MAX_TARGETS {
        return Err(format!("targets must contain 1..={MAX_TARGETS} records"));
    }
    let mut seen_targets = std::collections::BTreeSet::new();
    for target in &targets {
        if target.id.is_empty() || target.id.len() > 200 {
            return Err("target id is out of bounds".into());
        }
        if !matches!(
            target.kind.as_str(),
            "party" | "opportunity" | "resource" | "material" | "segment"
        ) {
            return Err(format!("unsupported target kind {:?}", target.kind));
        }
        if !seen_targets.insert((target.kind.clone(), target.id.clone())) {
            return Err("duplicate target".into());
        }
    }
    Ok(Grant {
        permissions,
        targets,
        allow_created_opportunities,
    })
}

// Scope helpers are carried for the write slices that extend this grant.
#[allow(dead_code)]
impl Grant {
    pub(crate) fn allows(&self, permission: Permission) -> bool {
        self.permissions
            .iter()
            .any(|candidate| candidate == permission.as_str())
    }

    /// Resolve the target id named by a command's arguments, if any. Read
    /// commands declare their target field in the descriptor's palette
    /// context; a selector or relationship inside args never widens scope.
    pub(crate) fn target_field<'a>(
        &self,
        args: &'a serde_json::Value,
    ) -> Option<(&'static str, &'a str)> {
        for field in [
            "party_id",
            "primary_party_id",
            "opportunity_id",
            "resource_id",
            "material_id",
        ] {
            if let Some(value) = args.get(field).and_then(serde_json::Value::as_str) {
                return Some((field, value));
            }
        }
        None
    }

    pub(crate) fn contains_target(&self, kind: &str, id: &str) -> bool {
        self.targets
            .iter()
            .any(|target| target.kind == kind && target.id == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::policy::Permission;

    fn target(kind: &str, id: &str) -> GrantTarget {
        GrantTarget { kind: kind.to_owned(), id: id.to_owned() }
    }

    fn valid_targets() -> Vec<GrantTarget> {
        vec![target("party", "p-1")]
    }

    #[test]
    fn build_grant_accepts_a_deduplicated_subset() {
        let permission = Permission::all()[0].as_str().to_owned();
        let grant = build_grant(
            vec![permission.clone(), permission.clone()],
            vec![target("party", "p-1"), target("party", "p-2")],
            true,
        );
        let err = build_grant(vec![permission.clone(), permission], valid_targets(), true)
            .expect_err("duplicate permissions must be rejected");
        assert!(err.contains("duplicate permission"), "{err}");
        let _ = grant;
    }

    #[test]
    fn build_grant_rejects_out_of_bounds_input() {
        let permission = Permission::all()[0].as_str().to_owned();
        assert!(build_grant(vec![], valid_targets(), false).is_err());
        assert!(
            build_grant(vec!["not-a-permission".into()], valid_targets(), false).is_err()
        );
        assert!(build_grant(vec![permission], vec![], false).is_err());
        let too_many: Vec<GrantTarget> = (0..=MAX_TARGETS)
            .map(|index| target("party", &format!("p-{index}")))
            .collect();
        assert!(build_grant(vec![Permission::all()[0].as_str().into()], too_many, false).is_err());
        assert!(
            build_grant(vec![Permission::all()[0].as_str().into()], vec![target("party", "")], false)
                .is_err()
        );
        let long_id = "p".repeat(201);
        assert!(
            build_grant(vec![Permission::all()[0].as_str().into()], vec![target("party", &long_id)], false)
                .is_err()
        );
        assert!(
            build_grant(vec![Permission::all()[0].as_str().into()], vec![target("vehicle", "p-1")], false)
                .is_err()
        );
        assert!(
            build_grant(
                vec![Permission::all()[0].as_str().into()],
                vec![target("party", "p-1"), target("party", "p-1")],
                false
            )
            .is_err()
        );
    }

    #[test]
    fn principal_pseudonym_is_stable_per_run_and_epoch() {
        let first = AgentPrincipal::from_grant("run-1", "epoch-1", "lease-1", b"secret");
        let again = AgentPrincipal::from_grant("run-1", "epoch-1", "lease-2", b"secret");
        let other_run = AgentPrincipal::from_grant("run-2", "epoch-1", "lease-1", b"secret");
        assert_eq!(first.pseudonym, again.pseudonym);
        assert_ne!(first.pseudonym, other_run.pseudonym);
        assert!(first.pseudonym.starts_with("agent_"));
        assert_eq!(first.run_id, "run-1");
        assert_eq!(first.lease_token, "lease-1");
    }

    #[test]
    fn allows_and_target_fields_read_the_grant() {
        let grant = build_grant(
            vec![Permission::all()[0].as_str().into()],
            vec![target("party", "p-1"), target("opportunity", "o-1")],
            false,
        )
        .unwrap();
        assert!(grant.allows(Permission::parse(Permission::all()[0].as_str()).unwrap()));
        let others = Permission::all()
            .iter()
            .find(|permission| permission.as_str() != Permission::all()[0].as_str())
            .copied();
        if let Some(other) = others {
            assert!(!grant.allows(other));
        }
        assert!(grant.contains_target("party", "p-1"));
        assert!(!grant.contains_target("party", "p-9"));

        let args = serde_json::json!({ "opportunity_id": "o-1", "nested": { "party_id": "x" } });
        assert_eq!(grant.target_field(&args), Some(("opportunity_id", "o-1")));
        let party_args = serde_json::json!({ "party_id": "p-1" });
        assert_eq!(grant.target_field(&party_args), Some(("party_id", "p-1")));
        let empty = serde_json::json!({});
        assert_eq!(grant.target_field(&empty), None);
    }
}
