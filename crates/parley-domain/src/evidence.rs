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
            _ => output.extend(ch.to_lowercase()),
        }
    }
    output
}

fn ascii_source_span(
    source: &str,
    normalized_start: usize,
    normalized_end: usize,
) -> Option<(usize, usize)> {
    if !source.is_ascii() || normalized_start >= normalized_end {
        return None;
    }
    let mut starts = Vec::new();
    let mut ends = Vec::new();
    let mut pending_space: Option<(usize, usize)> = None;
    for (index, byte) in source.bytes().enumerate() {
        if byte.is_ascii_whitespace() {
            if !starts.is_empty() {
                pending_space = Some(match pending_space {
                    Some((start, _)) => (start, index + 1),
                    None => (index, index + 1),
                });
            }
            continue;
        }
        if let Some((start, end)) = pending_space.take() {
            starts.push(start);
            ends.push(end);
        }
        starts.push(index);
        ends.push(index + 1);
    }
    let start = *starts.get(normalized_start)?;
    let end = *ends.get(normalized_end - 1)?;
    Some((start, end))
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
    let mut matches = normalized_source.match_indices(&normalized_quote);
    let found: Vec<(usize, usize)> = matches
        .by_ref()
        .map(|(start, value)| (start, start + value.len()))
        .collect();
    if found.is_empty() {
        return Err(QuoteError::NotFound);
    }
    let index = match occurrence {
        Some(index) if index < found.len() => index,
        Some(_) => return Err(QuoteError::NotFound),
        None if found.len() > 1 => return Err(QuoteError::Ambiguous(found.len())),
        None => 0,
    };
    let (normalized_start, normalized_end) = found[index];
    // Mapping arbitrary NFKC expansions back to source bytes requires retaining
    // an occurrence map. Until that map is available, only accept identity-width
    // source normalization where the verified normalized quote can be recovered
    // at unambiguous UTF-8 boundaries.
    let (source_start, source_end) = ascii_source_span(source, normalized_start, normalized_end)
        .or_else(|| source.find(quote).map(|start| (start, start + quote.len())))
        .ok_or(QuoteError::NotFound)?;
    Ok(QuoteMatch {
        source_start,
        source_end,
        normalized_start,
        normalized_end,
    })
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
}
