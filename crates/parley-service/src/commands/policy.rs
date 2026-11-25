//! Command permission model (UCR-001 FR-3).
//!
//! Every command declares exactly one closed permission enum value. The same
//! policy filters discovery and authorizes invocation. U01 registers the
//! owner phase-edit permission; later slices extend the enum.

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub(crate) enum Permission {
    #[serde(rename = "opportunity.create")]
    OpportunityCreate,
    #[serde(rename = "opportunity.phase.edit")]
    OpportunityPhaseEdit,
    #[serde(rename = "party.read")]
    PartyRead,
    #[serde(rename = "opportunity.read")]
    OpportunityRead,
    #[serde(rename = "party.edit")]
    PartyEdit,
    #[serde(rename = "opportunity.task.edit")]
    OpportunityTaskEdit,
    #[serde(rename = "brief.start")]
    BriefStart,
    #[serde(rename = "job.read")]
    JobRead,
    #[serde(rename = "resource.read")]
    ResourceRead,
    #[serde(rename = "resource.write")]
    ResourceWrite,
    #[serde(rename = "resource.retire")]
    ResourceRetire,
    #[serde(rename = "match.start")]
    MatchStart,
    #[serde(rename = "material.compose")]
    MaterialCompose,
    #[serde(rename = "material.gate")]
    MaterialGate,
}

impl Permission {
    pub fn as_str(&self) -> &'static str {
        match self {
            Permission::OpportunityCreate => "opportunity.create",
            Permission::OpportunityPhaseEdit => "opportunity.phase.edit",
            Permission::PartyRead => "party.read",
            Permission::OpportunityRead => "opportunity.read",
            Permission::PartyEdit => "party.edit",
            Permission::OpportunityTaskEdit => "opportunity.task.edit",
            Permission::BriefStart => "brief.start",
            Permission::JobRead => "job.read",
            Permission::ResourceRead => "resource.read",
            Permission::ResourceWrite => "resource.write",
            Permission::ResourceRetire => "resource.retire",
            Permission::MatchStart => "match.start",
            Permission::MaterialCompose => "material.compose",
            Permission::MaterialGate => "material.gate",
        }
    }

    /// Every permission an owner may grant to a delegated run.
    pub fn all() -> &'static [Permission] {
        &[
            Permission::OpportunityCreate,
            Permission::OpportunityPhaseEdit,
            Permission::PartyRead,
            Permission::OpportunityRead,
            Permission::PartyEdit,
            Permission::OpportunityTaskEdit,
            Permission::BriefStart,
            Permission::JobRead,
            Permission::ResourceRead,
            Permission::ResourceWrite,
            Permission::ResourceRetire,
            Permission::MatchStart,
            Permission::MaterialCompose,
            Permission::MaterialGate,
        ]
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::all()
            .iter()
            .copied()
            .find(|permission| permission.as_str() == value)
    }
}

/// Closed set of permission values the registry can declare.
pub(crate) const PERMISSION_ALLOWLIST: &[&str] = &[
    "opportunity.create",
    "opportunity.phase.edit",
    "party.read",
    "opportunity.read",
    "party.edit",
    "opportunity.task.edit",
    "brief.start",
    "job.read",
    "resource.read",
    "resource.write",
    "resource.retire",
    "match.start",
    "material.compose",
    "material.gate",
];

/// Dispatch classification for a registered command. `Read` is dispatched by
/// `commands::read::ReadCommand` without a writer mutation; `Destructive` is
/// routed through the approval pipeline with a frozen intent digest; every
/// other command executes in the shared savepoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Effect {
    Read,
    Write,
    Destructive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Surface {
    Palette,
    Agent,
}

pub(crate) const SURFACE_ALLOWLIST: &[&str] = &["palette", "agent"];
