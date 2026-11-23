//! Fuzz target for notes toggle spans (RFC-0002 S05):
//! "For each valid toggle, compare the output prefix and suffix byte-for-byte
//! outside the selected marker and parse the result again."
#![no_main]

use libfuzzer_sys::fuzz_target;
use parley_domain::notes::{index, toggle};

fuzz_target!(|data: &[u8]| {
    let markdown = String::from_utf8_lossy(data);
    if markdown.contains('\0') {
        return;
    }
    let tasks = index(&markdown);
    let Some(task) = tasks.first() else {
        return;
    };
    // Toggling must never panic on arbitrary markdown; a successful toggle
    // must leave every byte outside the addressed line untouched and must
    // re-parse cleanly. Both requested states are exercised: the checked
    // state and the unchecked state, from the same parsed task.
    for requested in [true, false] {
        if let Ok(result) = toggle(
            &markdown,
            task.byte_start,
            task.byte_end,
            &task.line_sha256,
            requested,
        ) {
            assert!(
                result.markdown.starts_with(&markdown[..task.byte_start]),
                "prefix outside the addressed marker is byte-identical"
            );
            let suffix = &markdown[task.byte_end..];
            assert!(
                result.markdown.ends_with(suffix),
                "suffix outside the addressed marker is byte-identical"
            );
            let reparsed = index(&result.markdown);
            assert!(!reparsed.is_empty(), "toggled markdown still parses as tasks");
            // Re-toggling from the freshly parsed state must succeed: a
            // toggle to a state the task already holds is a byte-level no-op
            // for that line.
            if let Some(first) = reparsed.first() {
                let again = toggle(
                    &result.markdown,
                    first.byte_start,
                    first.byte_end,
                    &first.line_sha256,
                    requested,
                );
                assert!(again.is_ok(), "toggle of the same desired state succeeds");
            }
        }
    }
});
