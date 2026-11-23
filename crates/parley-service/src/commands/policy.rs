//! Command permission model (UCR-001 FR-3).
//!
//! Every command declares exactly one closed permission enum value. The same
//! policy filters discovery and authorizes invocation. U01 registers the
//! owner phase-edit permission; later slices extend the enum.

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub(crate) enum Permission {
    #[serde(rename = "opportunity.phase.edit")]
    OpportunityPhaseEdit,
}

impl Permission {
    pub fn as_str(&self) -> &'static str {
        match self {
            Permission::OpportunityPhaseEdit => "opportunity.phase.edit",
        }
    }
}

/// Closed set of permission values the registry can declare.
pub(crate) const PERMISSION_ALLOWLIST: &[&str] = &["opportunity.phase.edit"];

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
