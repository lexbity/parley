//! Deterministic decision core for Parley.
//!
//! This crate owns product policy only: gates (RFC-0001 §10.3), quote
//! verification (§11.2), grounding, matching (§10.4), queue ordering (§10.5),
//! lifecycle transitions, notes parsing and pack policy (§9). Per RFC-0002's
//! module ownership table it MUST NOT depend on HTTP, SQL, file I/O, clocks or
//! model clients; `ci/trace.py` enforces that boundary with an import gate on
//! every run.
//!
pub mod analytics;
pub mod evidence;
pub mod gate;
pub mod hmac;
pub mod jcs;
pub mod lifecycle;
pub mod lint;
pub mod matching;
pub mod notes;
pub mod queue;
pub mod resources;
pub mod template;
