//! UCR-001 U02: fuzz the real command wire boundary.
//!
//! The target exercises duplicate-key rejection, bounded nesting, RFC 8785
//! canonical digests, mapped tool names and schema/serde agreement through
//! production code. It never reimplements a parser.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = parley_service::validate_command_wire(data);
});
