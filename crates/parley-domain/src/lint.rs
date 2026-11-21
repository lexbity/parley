//! Deterministic layer of the Composer Linter (RFC-0001 §12.6).

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Error,
    Warn,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    pub code: String,
    pub level: Level,
    pub message: String,
    pub start_byte: usize,
    pub end_byte: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub max_chars: usize,
    pub ask_count: usize,
    pub highest_allowed_rung: u32,
    pub ask_rungs: Vec<u32>,
    pub banned_phrases: Vec<String>,
    pub stiffness_terms: Vec<String>,
    pub unfluent_terms: Vec<String>,
    pub owner_name: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            max_chars: 1200,
            ask_count: 0,
            highest_allowed_rung: 5,
            ask_rungs: Vec::new(),
            banned_phrases: Vec::new(),
            stiffness_terms: [
                "i hope this finds you well",
                "synergy",
                "circle back",
                "touch base",
                "leverage",
                "reach out",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect(),
            unfluent_terms: Vec::new(),
            owner_name: String::new(),
        }
    }
}

pub fn lint(body: &str, config: &Config) -> Vec<Finding> {
    let mut findings = Vec::new();
    let body_chars = body.chars().count();
    if body_chars > config.max_chars {
        findings.push(Finding {
            code: "L-LEN".into(),
            level: Level::Error,
            message: format!(
                "message has {body_chars} characters; limit is {}",
                config.max_chars
            ),
            start_byte: 0,
            end_byte: body.len(),
        });
    }
    let question_marks = body.chars().filter(|ch| *ch == '?').count();
    let rung_failure = config
        .ask_rungs
        .iter()
        .any(|rung| *rung == 0 || *rung > config.highest_allowed_rung);
    if question_marks > 1 || config.ask_count > 1 || rung_failure {
        findings.push(Finding {
            code: "L-ASK".into(),
            level: Level::Error,
            message: "message has multiple asks or exceeds the permitted ask rung".into(),
            start_byte: 0,
            end_byte: body.len(),
        });
    }
    append_phrase_findings(
        body,
        &config.banned_phrases,
        "L-BANNED",
        Level::Error,
        "contains a disallowed phrase",
        &mut findings,
    );
    append_phrase_findings(
        body,
        &config.stiffness_terms,
        "L-STIFF",
        Level::Warn,
        "contains a corporate-sounding phrase",
        &mut findings,
    );
    append_phrase_findings(
        body,
        &config.unfluent_terms,
        "L-FLUENCY",
        Level::Warn,
        "uses terminology not marked fluent",
        &mut findings,
    );
    let exclamations = body.chars().filter(|ch| *ch == '!').count();
    if exclamations > 1 {
        findings.push(Finding {
            code: "L-EXCL".into(),
            level: Level::Warn,
            message: "use at most one exclamation mark".into(),
            start_byte: 0,
            end_byte: body.len(),
        });
    }
    for (start_byte, end_byte, sentence) in sentence_spans(body) {
        let word_count = sentence.split_whitespace().count();
        if word_count > 28 {
            findings.push(Finding {
                code: "L-READ".into(),
                level: Level::Warn,
                message: format!("sentence has {word_count} words; keep it to 28 or fewer"),
                start_byte,
                end_byte,
            });
        }
    }
    let trimmed = body.trim_start();
    let lower = trimmed.to_lowercase();
    let owner_open = ["i am ", "i'm ", "my name is ", "i work as "]
        .iter()
        .any(|prefix| lower.starts_with(prefix));
    if owner_open {
        findings.push(Finding {
            code: "L-OPEN".into(),
            level: Level::Warn,
            message: "opens with the owner's self-introduction rather than the recipient's context"
                .into(),
            start_byte: body.len() - trimmed.len(),
            end_byte: body.len() - trimmed.len() + trimmed.lines().next().unwrap_or_default().len(),
        });
    }
    findings
}

pub fn preserves_protected_tokens(original: &str, proposed: &str) -> bool {
    protected_tokens(original)
        .into_iter()
        .all(|token| proposed.contains(&token))
}

fn protected_tokens(text: &str) -> Vec<String> {
    let mut result = Vec::new();
    let mut token = String::new();
    let mut has_digit = false;
    for ch in text.chars().chain(std::iter::once(' ')) {
        if ch.is_alphanumeric() || matches!(ch, '.' | '/' | ':' | '-' | '_' | '@') {
            token.push(ch);
            has_digit |= ch.is_ascii_digit();
        } else {
            if has_digit && !token.is_empty() {
                result.push(token.clone());
            }
            token.clear();
            has_digit = false;
        }
    }
    result
}

fn append_phrase_findings(
    body: &str,
    phrases: &[String],
    code: &str,
    level: Level,
    message: &str,
    findings: &mut Vec<Finding>,
) {
    // ASCII casing preserves byte offsets. Seed phrases and the deterministic
    // lexicon are ASCII; non-ASCII source bytes remain untouched.
    let lower = body.to_ascii_lowercase();
    for phrase in phrases.iter().filter(|p| !p.trim().is_empty()) {
        let needle = phrase.to_ascii_lowercase();
        let mut offset = 0;
        while let Some(relative) = lower[offset..].find(&needle) {
            let start = offset + relative;
            let end = start + needle.len();
            findings.push(Finding {
                code: code.to_owned(),
                level: level.clone(),
                message: message.to_owned(),
                start_byte: start,
                end_byte: end,
            });
            offset = end;
            if offset >= lower.len() {
                break;
            }
        }
    }
}

fn sentence_spans(body: &str) -> Vec<(usize, usize, &str)> {
    let mut spans = Vec::new();
    let mut start = 0;
    for (index, ch) in body.char_indices() {
        if matches!(ch, '.' | '?' | '!' | '\n') {
            let end = index + ch.len_utf8();
            let sentence = &body[start..end];
            if !sentence.trim().is_empty() {
                spans.push((start, end, sentence));
            }
            start = end;
        }
    }
    if start < body.len() && !body[start..].trim().is_empty() {
        spans.push((start, body.len(), &body[start..]));
    }
    spans
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_findings_have_byte_spans_and_authoritative_errors() {
        let config = Config {
            max_chars: 30,
            ask_count: 2,
            highest_allowed_rung: 1,
            ask_rungs: vec![2],
            banned_phrases: vec!["fake urgency".into()],
            unfluent_terms: vec!["synergy".into()],
            owner_name: "Owner".into(),
            ..Config::default()
        };
        let body = "I'm Owner. Fake urgency. Synergy!";
        let findings = lint(body, &config);
        for code in ["L-LEN", "L-ASK", "L-BANNED", "L-FLUENCY", "L-OPEN"] {
            assert!(
                findings.iter().any(|finding| finding.code == code),
                "missing {code}: {findings:?}"
            );
        }
        assert!(
            findings
                .iter()
                .all(|finding| body.is_char_boundary(finding.start_byte)
                    && body.is_char_boundary(finding.end_byte))
        );
    }

    #[test]
    fn protected_numbers_and_identifiers_must_survive_advisory_rewrites() {
        assert!(preserves_protected_tokens(
            "I reduced 9 days to 3 days.",
            "It took 9 days, now 3 days."
        ));
        assert!(!preserves_protected_tokens(
            "I reduced 9 days to 3 days.",
            "It took 9 days, now 2 days."
        ));
    }
}
