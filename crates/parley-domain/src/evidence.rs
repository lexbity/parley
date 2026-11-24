//! Exact, versioned quote verification for captured evidence (RFC-0001 QV-1–4).

use unicode_normalization::UnicodeNormalization;
use unicode_normalization::char::{canonical_combining_class, compose, decompose_compatible};

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

/// A source normalized once, together with the byte map needed to resolve a
/// normalized match back to the exact original UTF-8 span. Build one of these
/// per source when verifying multiple quotes against it.
pub struct SourceIndex<'a> {
    source: &'a str,
    normalized: String,
    /// For every byte of `normalized`, the half-open source-byte range of the
    /// original characters that produced it. A source char that expands to
    /// several normalized chars shares its range across all of them; several
    /// source chars that compose into one normalized char contribute the first
    /// char's start and the last char's end.
    origins: Vec<(usize, usize)>,
}

impl<'a> SourceIndex<'a> {
    /// Normalize `source` and record the origin of every normalized byte.
    pub fn new(source: &'a str) -> Self {
        let (normalized, origins) = normalize_with_origins(source);
        Self {
            source,
            normalized,
            origins,
        }
    }

    /// The normalized source text. Offsets returned by [`Self::verify`] index
    /// into this string.
    pub fn normalized(&self) -> &str {
        &self.normalized
    }

    /// Match a quote exactly against this pre-normalized source. Repeated
    /// occurrences require an explicit zero-based occurrence index; fuzzy
    /// matching is never attempted.
    pub fn verify(&self, quote: &str, occurrence: Option<usize>) -> Result<QuoteMatch, QuoteError> {
        let normalized_quote = normalize(quote);
        if normalized_quote.is_empty() {
            return Err(QuoteError::Empty);
        }
        let mut found = Vec::new();
        let mut search_from = 0;
        while search_from < self.normalized.len() {
            let Some(relative) = self.normalized[search_from..].find(&normalized_quote) else {
                break;
            };
            let start = search_from + relative;
            let end = start + normalized_quote.len();
            found.push((start, end));
            search_from = start
                + self.normalized[start..]
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
        let (normalized_start, normalized_end) = found[index];
        let (source_start, _) = self.origins[normalized_start];
        let (_, source_end) = self.origins[normalized_end - 1];
        // The map is exact, but verify the candidate slice against the
        // normalization again. This preserves the historical semantics for
        // expansions such as a source "ß" against a quote "s", where both
        // normalized occurrences resolve to the same source span and neither
        // re-normalizes to the quote.
        if source_end <= source_start
            || !self.source.is_char_boundary(source_start)
            || !self.source.is_char_boundary(source_end)
            || normalize(&self.source[source_start..source_end]) != normalized_quote
        {
            return Err(QuoteError::NotFound);
        }
        Ok(QuoteMatch {
            source_start,
            source_end,
            normalized_start,
            normalized_end,
        })
    }
}

/// RFC normalization: NFKC, whitespace collapse, trim, case-fold approximation,
/// and typographic quote/dash normalization. Rust's Unicode lowercase mapping
/// is used for case folding; the version is persisted with each occurrence.
pub fn normalize(text: &str) -> String {
    let mut builder = NormalizedBuilder::new(false, text.len());
    for ch in text.nfkc() {
        builder.accept(ch, 0, 0);
    }
    builder.finish().0
}

/// Match a quote exactly after normalization. Repeated occurrences require an
/// explicit zero-based occurrence index; fuzzy matching is never attempted.
pub fn verify_quote(
    source: &str,
    quote: &str,
    occurrence: Option<usize>,
) -> Result<QuoteMatch, QuoteError> {
    SourceIndex::new(source).verify(quote, occurrence)
}

/// Normalize with a byte map. The character mapping is shared with `normalize`;
/// only the NFKC stream differs — it performs a single pass that also carries
/// each character's source-byte origin. Tests assert both paths produce the
/// same normalized text.
fn normalize_with_origins(text: &str) -> (String, Vec<(usize, usize)>) {
    let mut builder = NormalizedBuilder::new(true, text.len());
    for (ch, start, end) in nfkc_with_origins(text) {
        builder.accept(ch, start, end);
    }
    builder.finish()
}

/// NFKD with origins: decompose each source char (recursively), then apply
/// canonical ordering. Every decomposed char carries the source-byte range of
/// the character it came from.
fn nfkd_with_origins(source: &str) -> Vec<(char, usize, usize)> {
    let mut decomposed = Vec::with_capacity(source.len());
    for (start, ch) in source.char_indices() {
        let end = start + ch.len_utf8();
        decompose_compatible(ch, |d| decomposed.push((d, start, end)));
    }
    // Canonical ordering is a stable sort of the combining marks in each
    // sequence by combining class, leaving the sequence's starter in front.
    // This mirrors the buffering done by unicode-normalization's Decompositions.
    let mut i = 0;
    while i < decomposed.len() {
        let mut j = i + 1;
        while j < decomposed.len() && canonical_combining_class(decomposed[j].0) != 0 {
            j += 1;
        }
        let sort_from = if canonical_combining_class(decomposed[i].0) == 0 {
            i + 1
        } else {
            i
        };
        if sort_from + 1 < j {
            decomposed[sort_from..j].sort_by_key(|&(ch, _, _)| canonical_combining_class(ch));
        }
        i = j;
    }
    decomposed
}

/// Canonical composition (NFC) over an NFKD stream that carries origins.
/// Mirrors unicode-normalization's `Recompositions`; a composed character
/// inherits the start of its starter and the end of the last mark it absorbed.
fn recompose_with_origins(
    chars: &[(char, usize, usize)],
    mut emit: impl FnMut(char, usize, usize),
) {
    let mut composee: Option<(char, usize, usize)> = None;
    let mut last_class: Option<u8> = None;
    let mut buffer: Vec<(char, usize, usize)> = Vec::new();
    for &(ch, start, end) in chars {
        let class = canonical_combining_class(ch);
        let (starter, starter_start, starter_end) = match composee {
            None => {
                if class != 0 {
                    emit(ch, start, end);
                } else {
                    composee = Some((ch, start, end));
                }
                continue;
            }
            Some(value) => value,
        };
        match last_class {
            None => match compose(starter, ch) {
                Some(result) => composee = Some((result, starter_start, end)),
                None => {
                    if class == 0 {
                        emit(starter, starter_start, starter_end);
                        composee = Some((ch, start, end));
                    } else {
                        buffer.push((ch, start, end));
                        last_class = Some(class);
                    }
                }
            },
            Some(last) => {
                if last >= class {
                    if class == 0 {
                        emit(starter, starter_start, starter_end);
                        for (buffered, s, e) in buffer.drain(..) {
                            emit(buffered, s, e);
                        }
                        composee = Some((ch, start, end));
                        last_class = None;
                    } else {
                        buffer.push((ch, start, end));
                        last_class = Some(class);
                    }
                } else {
                    match compose(starter, ch) {
                        Some(result) => composee = Some((result, starter_start, end)),
                        None => {
                            buffer.push((ch, start, end));
                            last_class = Some(class);
                        }
                    }
                }
            }
        }
    }
    if let Some((ch, start, end)) = composee {
        emit(ch, start, end);
    }
    for (ch, start, end) in buffer {
        emit(ch, start, end);
    }
}

fn nfkc_with_origins(source: &str) -> Vec<(char, usize, usize)> {
    let decomposed = nfkd_with_origins(source);
    let mut composed = Vec::with_capacity(decomposed.len());
    recompose_with_origins(&decomposed, |ch, start, end| {
        composed.push((ch, start, end))
    });
    composed
}

struct NormalizedBuilder {
    text: String,
    origins: Vec<(usize, usize)>,
    track: bool,
    pending_space: bool,
    pending_start: usize,
    pending_end: usize,
}

impl NormalizedBuilder {
    fn new(track: bool, capacity: usize) -> Self {
        Self {
            text: String::with_capacity(capacity),
            origins: if track {
                Vec::with_capacity(capacity)
            } else {
                Vec::new()
            },
            track,
            pending_space: false,
            pending_start: 0,
            pending_end: 0,
        }
    }

    fn push(&mut self, ch: char, start: usize, end: usize) {
        let from = self.text.len();
        let mut encoded = [0u8; 4];
        self.text.push_str(ch.encode_utf8(&mut encoded));
        if self.track {
            for _ in from..self.text.len() {
                self.origins.push((start, end));
            }
        }
    }

    fn accept(&mut self, ch: char, start: usize, end: usize) {
        if ch.is_whitespace() {
            if !self.pending_space {
                self.pending_start = start;
            }
            self.pending_end = end;
            self.pending_space = !self.text.is_empty();
            return;
        }
        if self.pending_space {
            self.push(' ', self.pending_start, self.pending_end);
            self.pending_space = false;
        }
        match ch {
            '\u{2018}' | '\u{2019}' | '\u{201a}' | '\u{201b}' | '\u{2032}' => {
                self.push('\'', start, end)
            }
            '\u{201c}' | '\u{201d}' | '\u{201e}' | '\u{201f}' | '\u{2033}' => {
                self.push('"', start, end)
            }
            '\u{2010}'..='\u{2015}' | '\u{2212}' => self.push('-', start, end),
            'ß' | 'ẞ' => {
                self.push('s', start, end);
                self.push('s', start, end);
            }
            'ς' => self.push('σ', start, end),
            _ => {
                for lower in ch.to_lowercase() {
                    self.push(lower, start, end);
                }
            }
        }
    }

    fn finish(self) -> (String, Vec<(usize, usize)>) {
        (self.text, self.origins)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;
    use unicode_normalization::UnicodeNormalization;

    /// Reference implementation: the original whole-string `nfkc()` followed
    /// by the same character mapping. The single-pass implementation used by
    /// `normalize` and `SourceIndex` must agree with it exactly.
    fn reference_normalize(text: &str) -> String {
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

    #[test]
    fn single_pass_normalization_matches_whole_string_nfkc() {
        let cases = [
            "",
            "  Ａ  “Hello”—WORLD  ",
            "Hiring: “Ａda” joined—today",
            "Straße",
            "aaa",
            "e\u{0301}",
            "\u{0301}e",
            "a\u{0301}\u{0328}b",
            "A\u{030a}",
            "\u{1100}\u{1161}",
            "\u{ac00}",
            "ﬁnd ① № ㍍",
            "a\u{00a0}\u{2003}b",
            "İ\u{0307}",
            "Σίσυφος ΟΔΥΣΣΕΎΣ",
            "x\u{0315}\u{0300}y\u{0316}",
            "\u{1e0a}\u{0323}",
        ];
        for case in cases {
            assert_eq!(normalize(case), reference_normalize(case), "{case:?}");
            assert_eq!(
                SourceIndex::new(case).normalized(),
                reference_normalize(case),
                "{case:?}"
            );
        }

        // Deterministic sweep over a tricky alphabet, including combining
        // marks, compatibility decompositions and multi-char case folds.
        let alphabet = [
            'a', 'A', 'ß', 'ẞ', 'ς', 'Σ', 'e', '\u{0301}', '\u{0328}', '\u{0315}', '\u{0300}',
            '\u{1100}', '\u{1161}', 'ﬁ', '①', 'Ａ', '“', '”', '—', '\u{00a0}', ' ', '\n',
        ];
        let mut state = 0x2545_f491_4f6c_dd1du64;
        for _ in 0..2_000 {
            let mut value = String::new();
            let len = ((state >> 32) % 24) as usize;
            for _ in 0..len {
                state = state
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                value.push(alphabet[((state >> 33) as usize) % alphabet.len()]);
            }
            assert_eq!(normalize(&value), reference_normalize(&value), "{value:?}");
        }
    }

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

    #[test]
    fn spans_survive_decomposition_composition_and_expansion() {
        let source = "begin e\u{0301} Straße end";
        let found = verify_quote(source, "É strasse", None).unwrap();
        assert_eq!(
            &source[found.source_start..found.source_end],
            "e\u{0301} Straße"
        );
        assert_eq!(
            normalize(&source[found.source_start..found.source_end]),
            "é strasse"
        );

        // A source char that expands to two normalized chars still maps back
        // to its exact byte range.
        let index = SourceIndex::new("x ß y");
        let found = index.verify("ss", None).unwrap();
        assert_eq!(&index.source[found.source_start..found.source_end], "ß");

        // Both normalized occurrences of "s" resolve to the same source span;
        // neither re-normalizes to the quote, so neither is accepted.
        assert_eq!(index.verify("s", None), Err(QuoteError::Ambiguous(2)));
        assert_eq!(index.verify("s", Some(0)), Err(QuoteError::NotFound));
        assert_eq!(index.verify("s", Some(1)), Err(QuoteError::NotFound));
    }

    #[test]
    fn source_index_reuses_one_normalization_for_many_quotes() {
        let source = "Alpha alpha. Beta beta. Gamma gamma.";
        let index = SourceIndex::new(source);
        for (quote, span) in [
            ("Alpha alpha", (0, 11)),
            ("Beta beta", (13, 22)),
            ("Gamma gamma", (24, 35)),
        ] {
            let found = index.verify(quote, None).unwrap();
            assert_eq!((found.source_start, found.source_end), span);
            assert_eq!(&source[span.0..span.1], quote);
        }
    }

    #[test]
    fn arbitrary_bytes_keep_span_invariants() {
        // Mirrors fuzz/fuzz_targets/evidence_spans.rs without requiring cargo-fuzz.
        let alphabet: Vec<u8> =
            b"abcABC012 \n\t\xe2\x80\x9c\xe2\x80\x9d\xc3\x9f\xef\xac\x81\xcc\x81".to_vec();
        let mut state = 0x9e37_79b9_7f4a_7c15u64;
        for _ in 0..2_000 {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let len = (((state >> 33) % 40) as usize).max(2);
            let mut bytes = Vec::with_capacity(len);
            for _ in 0..len {
                state = state
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                bytes.push(alphabet[((state >> 40) as usize) % alphabet.len()]);
            }
            let split = (bytes.len() / 2).max(1);
            let source = String::from_utf8_lossy(&bytes[..split]).into_owned();
            let quote = String::from_utf8_lossy(&bytes[split..]).into_owned();
            if let Ok(matched) = verify_quote(&source, &quote, None) {
                assert!(source.is_char_boundary(matched.source_start));
                assert!(source.is_char_boundary(matched.source_end));
                assert!(matched.source_end > matched.source_start);
                assert_eq!(
                    normalize(&source[matched.source_start..matched.source_end]),
                    normalize(&quote)
                );
            }
        }
    }

    #[test]
    fn verification_is_linear_at_qualification_scale() {
        // The previous implementation was O(chars * quote^2): a quote of this
        // length against a source this size took seconds. The single-pass map
        // must stay far below a generous CI bound.
        let quote = "needle ".repeat(28).trim().to_owned();
        let source = format!("{} {} ", "filler ".repeat(4_000), quote);
        let started = Instant::now();
        let found = verify_quote(&source, &quote, None).unwrap();
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
        assert_eq!(
            &source[found.source_start..found.source_end],
            quote.as_str()
        );
    }
}
