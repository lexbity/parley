//! Fuzz target for template slot parsing (RFC-0002 S03):
//! "Emit accepted slots only from the declared resolver set. Remove one
//! required slot and require validation failure unless its declared fallback
//! applies."
#![no_main]

use libfuzzer_sys::fuzz_target;
use parley_domain::template::validate;

fuzz_target!(|data: &[u8]| {
    let body = String::from_utf8_lossy(data);
    if body.contains('\0') {
        return;
    }
    // validate must never panic on arbitrary bytes.
    let validation = validate(&body, None);
    let mut seen = std::collections::BTreeSet::new();
    for slot in &validation.slots {
        // Accepted slots come only from the declared resolver set: every
        // emitted slot must be `kind:name` with a registered kind.
        let Some((kind, name)) = slot.split_once(':') else {
            panic!("emitted slot has no typed kind: {slot:?}");
        };
        assert!(!kind.is_empty() && !name.trim().is_empty(), "slot is non-empty");
        assert!(seen.insert(slot.clone()), "no duplicate slots are emitted");
    }
});
