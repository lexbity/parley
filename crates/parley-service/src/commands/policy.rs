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
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Effect {
    #[allow(dead_code)] // reserved for later slices
    Read,
    Write,
    #[allow(dead_code)] // reserved for the approval slice
    Destructive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Surface {
    Palette,
    Agent,
}

pub(crate) const SURFACE_ALLOWLIST: &[&str] = &["palette", "agent"];
