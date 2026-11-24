//! Actor-independent typed application operations (UCR-001).
//!
//! Each module here implements one application operation: typed inputs,
//! typed errors, revision preconditions and bounded repository queries. The
//! operation receives a transaction and a checked actor context from the
//! writer executor; it never sees HTTP types, keys or transport envelopes.

pub(crate) mod brief;
pub(crate) mod coach;
pub(crate) mod compose_start;
pub(crate) mod job_status;
pub(crate) mod match_start;
pub(crate) mod material_gate;
pub(crate) mod opportunity;
pub(crate) mod opportunity_summary;
pub(crate) mod opportunity_task;
pub(crate) mod party_edit;
pub(crate) mod party_summary;
pub(crate) mod phase;
pub(crate) mod resource_retirement;
pub(crate) mod resources;
pub(crate) mod writer;
