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
        for field in ["party_id", "opportunity_id", "resource_id", "material_id"] {
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
