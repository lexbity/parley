//! Exact, versioned quote verification for captured evidence (RFC-0001 QV-1–4).

use unicode_normalization::UnicodeNormalization;

pub const NORMALIZATION_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuoteMatch {
    /// Half-open UTF-8 byte offsets into the original source.
    pub source_start: usize,
    pub source_end: usize,
    /// Half-open offsets into the normalized source.
    pub normalized_start: usize,
    pub normalized_end: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuoteError {
    Empty,
    NotFound,
    Ambiguous(usize),
}

/// RFC normalization: NFKC, whitespace collapse, trim, case-fold approximation,
/// and typographic quote/dash normalization. Rust's Unicode lowercase mapping
/// is used for case folding; the version is persisted with each occurrence.
pub fn normalize(text: &str) -> String {
    let nfkc: String = text.nfkc().collect();
    let mut output = String::with_capacity(nfkc.len());
    let mut pending_space = false;
    for ch in nfkc.chars() {
        if ch.is_whitespace() {
            pending_space = !output.is_empty();
            continue;
        }
        if pending_space {
            output.push(' ');
            pending_space = false;
        }
        match ch {
            '\u{2018}' | '\u{2019}' | '\u{201a}' | '\u{201b}' | '\u{2032}' => output.push('\''),
            '\u{201c}' | '\u{201d}' | '\u{201e}' | '\u{201f}' | '\u{2033}' => output.push('"'),
            '\u{2010}'..='\u{2015}' | '\u{2212}' => output.push('-'),
            'ß' | 'ẞ' => output.push_str("ss"),
            'ς' => output.push('σ'),
            _ => output.extend(ch.to_lowercase()),
        }
    }
    output
}

/// Match a quote exactly after normalization. Repeated occurrences require an
/// explicit zero-based occurrence index; fuzzy matching is never attempted.
pub fn verify_quote(
    source: &str,
    quote: &str,
    occurrence: Option<usize>,
) -> Result<QuoteMatch, QuoteError> {
    let normalized_source = normalize(source);
    let normalized_quote = normalize(quote);
    if normalized_quote.is_empty() {
        return Err(QuoteError::Empty);
    }
    let mut found = Vec::new();
    let mut search_from = 0;
    while search_from < normalized_source.len() {
        let Some(relative) = normalized_source[search_from..].find(&normalized_quote) else {
            break;
        };
        let start = search_from + relative;
        let end = start + normalized_quote.len();
        found.push((start, end));
        search_from = start
            + normalized_source[start..]
                .chars()
                .next()
                .expect("match starts at a character boundary")
                .len_utf8();
    }
    if found.is_empty() {
        return Err(QuoteError::NotFound);
    }
    let index = match occurrence {
        Some(index) if index < found.len() => index,
        Some(_) => return Err(QuoteError::NotFound),
        None if found.len() > 1 => return Err(QuoteError::Ambiguous(found.len())),
        None => 0,
    };
    let source_matches = source_quote_spans(source, &normalized_quote);
    if source_matches.len() <= index {
        return Err(QuoteError::NotFound);
    }
    let (source_start, source_end) = source_matches[index];
    let (normalized_start, normalized_end) = found[index];
    if normalize(&source[source_start..source_end]) != normalized_quote {
        return Err(QuoteError::NotFound);
    }
    Ok(QuoteMatch {
        source_start,
        source_end,
        normalized_start,
        normalized_end,
    })
}

fn source_quote_spans(source: &str, quote: &str) -> Vec<(usize, usize)> {
    let boundaries: Vec<usize> = source
        .char_indices()
        .map(|(offset, _)| offset)
        .chain([source.len()])
        .collect();
    let mut spans = Vec::new();
    for (start_index, &start) in boundaries
        .iter()
        .enumerate()
        .take(boundaries.len().saturating_sub(1))
    {
        for &end in boundaries.iter().skip(start_index + 1) {
            let candidate = normalize(&source[start..end]);
            if candidate == quote {
                let trimmed_start = start
                    + source[start..end]
                        .char_indices()
                        .find(|(_, ch)| !ch.is_whitespace())
                        .map_or(0, |(offset, _)| offset);
                let trimmed_end = start
                    + source[start..end]
                        .char_indices()
                        .rev()
                        .find(|(_, ch)| !ch.is_whitespace())
                        .map_or(0, |(offset, ch)| offset + ch.len_utf8());
                spans.push((trimmed_start, trimmed_end));
                break;
            }
            if candidate.len() > quote.len().saturating_add(8) {
                break;
            }
        }
    }
    spans.sort_unstable();
    spans.dedup();
    spans
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalization_handles_unicode_whitespace_quotes_and_dashes() {
        assert_eq!(normalize("  Ａ  “Hello”—WORLD  "), "a \"hello\"-world");
    }

    #[test]
    fn quote_requires_exact_normalized_substring_and_disambiguates_repeats() {
        assert!(matches!(
            verify_quote("Alpha alpha", "alpha", None),
            Err(QuoteError::Ambiguous(2))
        ));
        assert_eq!(
            verify_quote("Alpha alpha", "alpha", Some(1))
                .unwrap()
                .source_start,
            6
        );
        assert_eq!(
            verify_quote("Alpha alpha", "alpha", Some(0))
                .unwrap()
                .source_start,
            0
        );
        assert_eq!(
            verify_quote("a cat", "a car", None),
            Err(QuoteError::NotFound)
        );
    }

    #[test]
    fn quote_span_maps_unicode_variant_to_original_utf8_bytes() {
        let source = "Hiring: “Ａda” joined—today";
        let found = verify_quote(source, "\"ada\" joined-today", None).unwrap();
        assert_eq!(
            &source[found.source_start..found.source_end],
            "“Ａda” joined—today"
        );
        assert_eq!(
            normalize(&source[found.source_start..found.source_end]),
            "\"ada\" joined-today"
        );
    }

    #[test]
    fn casefolding_and_overlapping_quote_occurrences_are_exact() {
        assert_eq!(normalize("Straße"), "strasse");
        assert!(matches!(
            verify_quote("aaa", "aa", None),
            Err(QuoteError::Ambiguous(2))
        ));
        let second = verify_quote("aaa", "aa", Some(1)).unwrap();
        assert_eq!((second.source_start, second.source_end), (1, 3));
    }
}
