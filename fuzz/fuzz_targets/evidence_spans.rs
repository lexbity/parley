//! Fuzz target for quote verification spans (RFC-0002 S02):
//! "When an accepted interval is nonempty, assert both raw boundaries are
//! UTF-8 boundaries and renormalizing the slice reproduces the matched quote."
#![no_main]

use libfuzzer_sys::fuzz_target;
use parley_domain::evidence::{normalize, verify_quote};

fuzz_target!(|data: &[u8]| {
    if data.len() < 2 {
        return;
    }
    let split = (data.len() / 2).max(1);
    let source = String::from_utf8_lossy(&data[..split]);
    let quote = String::from_utf8_lossy(&data[split..]);
    // verify_quote must never panic on arbitrary bytes.
    let Ok(matched) = verify_quote(&source, &quote, None) else {
        return;
    };
    assert!(source.is_char_boundary(matched.source_start), "raw start is a UTF-8 boundary");
    assert!(source.is_char_boundary(matched.source_end), "raw end is a UTF-8 boundary");
    assert!(matched.source_end > matched.source_start, "accepted interval is nonempty");
    // Renormalizing the matched original slice reproduces the matched quote.
    assert_eq!(
        normalize(&source[matched.source_start..matched.source_end]),
        normalize(&quote),
        "renormalized slice must equal the normalized quote"
    );
    // Canonical chunk-boundary consistency (AR-001 AR06): a quote contained
    // entirely in one sanitized chunk verifies against the full source with
    // the same normalized span bytes.
    let cut = (source.len() / 3).max(1);
    let cut = (0..=cut)
        .rev()
        .find(|&index| source.is_char_boundary(index))
        .unwrap_or(0);
    let slice = &source[..cut];
    if matched.source_end <= cut
        && let Ok(inner) = verify_quote(slice, &quote, None)
    {
        assert_eq!(
            normalize(&slice[inner.source_start..inner.source_end]),
            normalize(&quote),
            "chunk-local span must match the full-source span"
        );
    }
});
