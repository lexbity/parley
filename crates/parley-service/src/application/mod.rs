//! Actor-independent typed application operations (UCR-001).
//!
//! Each module here implements one application operation: typed inputs,
//! typed errors, revision preconditions and bounded repository queries. The
//! operation receives a transaction and a checked actor context from the
//! writer executor; it never sees HTTP types, keys or transport envelopes.

pub(crate) mod opportunity_summary;
pub(crate) mod party_summary;
pub(crate) mod phase;
pub(crate) mod writer;
