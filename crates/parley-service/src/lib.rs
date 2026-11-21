//! Parley application service.
//!
//! Owns commands and transaction execution (RFC-0002 BQ1), SQLite persistence
//! and migrations, the blob store, the durable agent worker and the local
//! Axum transport. Transport parsing, permissions and executable startup stay
//! outside the domain crate; handlers MUST NOT contain SQL or gate overrides.
//!
//! Command, persistence and worker modules land with their owning slices per
//! the RFC-0004 implementation plan.
