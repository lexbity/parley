//! Deterministic validation for immutable, manually authored templates.

use std::collections::BTreeSet;

pub const SLOT_TYPES: &[&str] = &[
    "their_name",
    "their_org",
    "sender_name",
    "hook",
    "gift",
    "proof",
    "artifact",
    "ask",
    "jargon_term",
    "identity_link",
    "capacity_line",
    "custom",
];

/// Resolvers implemented by the deterministic authoring path. Other declared
/// slot kinds remain invalid until their evidence bank and resolver are part
/// of the same release path. The R5 hook resolver lands with AR03: its typed
/// fill is an accepted, current claim revision scoped to the selected
/// recipient and opportunity.
pub const REGISTERED_SLOT_TYPES: &[&str] = &[
    "their_name",
    "sender_name",
    "gift",
    "proof",
    "artifact",
    "ask",
    "custom",
    "capacity_line",
    "hook",
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TemplateValidation {
    pub slots: Vec<String>,
    pub errors: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SentenceRuleValidation {
    pub sentence_index: usize,
    pub declared: Vec<String>,
    pub derived: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DetailedTemplateValidation {
    pub base: TemplateValidation,
    pub sentences: Vec<SentenceRuleValidation>,
    pub errors: Vec<String>,
}

pub fn validate(body: &str, proof_fallback: Option<&str>) -> TemplateValidation {
    let mut slots = Vec::new();
    let mut errors = Vec::new();
    let mut fixed = String::with_capacity(body.len());
    let mut rest = body;
    while let Some(open) = rest.find("{{") {
        fixed.push_str(&rest[..open]);
        let after = &rest[open + 2..];
        let Some(close) = after.find("}}") else {
            errors.push("unclosed template slot".into());
            rest = "";
            break;
        };
        let slot = &after[..close];
        let (kind, name) = match slot.split_once(':') {
            Some(parts) if !parts.0.is_empty() && !parts.1.trim().is_empty() => parts,
            _ => {
                errors.push(format!("invalid typed slot {{{{{slot}}}}}"));
                rest = &after[close + 2..];
                continue;
            }
        };
        if !SLOT_TYPES.contains(&kind) || !REGISTERED_SLOT_TYPES.contains(&kind) {
            errors.push(format!("slot type '{kind}' has no registered resolver"));
        }
        let full = format!("{kind}:{name}");
        if slots.contains(&full) {
            errors.push(format!("duplicate slot '{full}'"));
        } else {
            slots.push(full);
        }
        fixed.push(' ');
        rest = &after[close + 2..];
    }
    fixed.push_str(rest);
    if body.trim().is_empty() {
        errors.push("template body is empty".into());
    }
    if fixed.chars().any(|ch| ch.is_ascii_digit()) {
        errors.push("fixed template text cannot contain digits".into());
    }
    if fixed.contains("://") || fixed.to_ascii_lowercase().contains("www.") {
        errors.push("fixed template text cannot contain URLs".into());
    }
    if let Some(token) = first_unresolved_proper_noun(&fixed) {
        errors.push(format!(
            "fixed template text contains proper noun '{token}'"
        ));
    }
    let asks = slots.iter().filter(|slot| slot.starts_with("ask:")).count();
    if asks > 1 {
        errors.push("template can contain at most one ask slot".into());
    }
    if slots.iter().any(|slot| slot.starts_with("proof:"))
        && !matches!(
            proof_fallback,
            Some("omit_sentence" | "use_alternative_template")
        )
    {
        errors
            .push("proof slot requires omit_sentence or use_alternative_template fallback".into());
    }
    TemplateValidation { slots, errors }
}

/// Validate Draft-00 TPL-5 rule annotations against the slot types that each
/// sentence actually contains. Every non-empty sentence must have exactly one
/// declaration; a template cannot advertise a persuasion rule that its slots
/// do not support.
pub fn validate_detailed(
    body: &str,
    proof_fallback: Option<&str>,
    sentence_rules: &[Vec<String>],
) -> DetailedTemplateValidation {
    let base = validate(body, proof_fallback);
    let parts = sentences(body);
    let mut errors = base.errors.clone();
    if parts.len() != sentence_rules.len() {
        errors.push(format!(
            "template has {} sentences but {} rule annotations",
            parts.len(),
            sentence_rules.len()
        ));
    }
    let mut annotations = Vec::new();
    for (index, sentence) in parts.iter().enumerate() {
        let declared = sentence_rules.get(index).cloned().unwrap_or_default();
        let derived = rules_for_slots(sentence);
        if declared.iter().any(|rule| {
            !matches!(
                rule.as_str(),
                "R1" | "R2" | "R3" | "R4" | "R5" | "R6" | "R7"
            )
        }) {
            errors.push(format!("sentence {} declares an unknown rule", index + 1));
        }
        let mut declared_sorted = declared.clone();
        declared_sorted.sort();
        declared_sorted.dedup();
        if declared_sorted.len() != declared.len() {
            errors.push(format!("sentence {} repeats a rule tag", index + 1));
        }
        let mut derived_sorted = derived.clone();
        derived_sorted.sort();
        if declared_sorted != derived_sorted {
            errors.push(format!(
                "sentence {} rule tags do not match its typed slots (declared {:?}, derived {:?})",
                index + 1,
                declared_sorted,
                derived_sorted
            ));
        }
        annotations.push(SentenceRuleValidation {
            sentence_index: index,
            declared,
            derived,
        });
    }
    DetailedTemplateValidation {
        base,
        sentences: annotations,
        errors,
    }
}

pub fn rules_for_slots(sentence: &str) -> Vec<String> {
    let mappings = [
        ("gift", "R1"),
        ("ask", "R2"),
        ("proof", "R3"),
        ("artifact", "R4"),
        ("hook", "R5"),
        ("capacity_line", "R6"),
        ("identity_link", "R7"),
    ];
    let mut found = BTreeSet::new();
    for (slot_type, rule) in mappings {
        if sentence.contains(&format!("{{{{{slot_type}:")) {
            found.insert(rule.to_owned());
        }
    }
    found.into_iter().collect()
}

fn sentences(body: &str) -> Vec<&str> {
    body.split_inclusive(['.', '!', '?', '\n'])
        .map(str::trim)
        .filter(|sentence| !sentence.is_empty())
        .collect()
}

/// Public sentence splitter used by the candidate renderer and the typed
/// preview generator. Sentences are split inclusively on sentence-final
/// punctuation and newlines, matching the validator's parser.
pub fn split_sentences(body: &str) -> Vec<&str> {
    sentences(body)
}

/// Apply a declared optional omission to a candidate skeleton. When a rule is
/// explicitly optional and the skeleton declares the matching fallback, the
/// entire dependent sentence is removed. The returned body is the skeleton
/// with omitted sentences removed; the returned tags are the surviving
/// per-sentence rule annotations.
pub fn apply_optional_omission(
    body: &str,
    sentence_rules: &[Vec<String>],
    optional_rule: &str,
    fallback: &str,
) -> Option<(String, Vec<Vec<String>>)> {
    let parts = sentences(body);
    if parts.len() != sentence_rules.len() {
        return None;
    }
    let mut kept = Vec::with_capacity(parts.len());
    let mut tags = Vec::with_capacity(parts.len());
    for (sentence, rules) in parts.into_iter().zip(sentence_rules) {
        if rules.iter().any(|rule| rule == optional_rule) {
            if fallback != "omit_sentence" {
                return None;
            }
            continue;
        }
        kept.push(sentence);
        tags.push(rules.clone());
    }
    Some((kept.join(" "), tags))
}

fn first_unresolved_proper_noun(fixed: &str) -> Option<String> {
    let mut sentence_start = true;
    let mut seen = BTreeSet::new();
    for token in fixed.split_whitespace() {
        let is_sentence_start = sentence_start;
        sentence_start = token.ends_with('.') || token.ends_with('!') || token.ends_with('?');
        let trimmed = token.trim_matches(|ch: char| !ch.is_alphanumeric() && ch != '\'');
        if trimmed.is_empty() {
            continue;
        }
        let first = trimmed.chars().next().unwrap_or_default();
        let is_all_caps = trimmed
            .chars()
            .filter(|ch| ch.is_alphabetic())
            .all(char::is_uppercase)
            && trimmed.chars().any(char::is_alphabetic)
            && trimmed.len() > 1;
        if trimmed != "I"
            && !is_sentence_start
            && ((first.is_uppercase() && trimmed.chars().skip(1).any(char::is_lowercase))
                || is_all_caps)
            && seen.insert(trimmed.to_owned())
        {
            return Some(trimmed.to_owned());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_slots_are_removed_before_fixed_text_checks() {
        let valid = validate(
            "Hi {{their_name:name}},\nI made a {{gift:gift}} for this.",
            None,
        );
        assert!(valid.errors.is_empty());
        assert_eq!(valid.slots.len(), 2);
    }

    #[test]
    fn fixed_numbers_proper_nouns_and_invalid_slots_are_rejected() {
        let invalid = validate("Hi Dana, see 3 examples using {{unknown:x}}.", None);
        assert!(invalid.errors.iter().any(|error| error.contains("digits")));
        assert!(invalid.errors.iter().any(|error| error.contains("Dana")));
        assert!(
            invalid
                .errors
                .iter()
                .any(|error| error.contains("resolver"))
        );
    }

    #[test]
    fn proof_fallback_and_single_ask_are_enforced() {
        assert!(
            validate("{{proof:p}}", None)
                .errors
                .iter()
                .any(|error| error.contains("fallback"))
        );
        assert!(
            validate("{{ask:a}} {{ask:b}}", None)
                .errors
                .iter()
                .any(|error| error.contains("one ask"))
        );
        assert!(
            validate("{{proof:p}}", Some("omit_sentence"))
                .errors
                .is_empty()
        );
    }

    #[test]
    fn sentence_rule_tags_must_match_typed_slots() {
        let ok = validate_detailed(
            "I can share {{gift:guide}}. Would a look help {{ask:question}}?",
            None,
            &[vec!["R1".into()], vec!["R2".into()]],
        );
        assert!(ok.errors.is_empty(), "{:?}", ok.errors);
        let wrong = validate_detailed("I can share {{gift:guide}}.", None, &[vec!["R3".into()]]);
        assert!(
            wrong
                .errors
                .iter()
                .any(|error| error.contains("do not match"))
        );
    }

    #[test]
    fn hook_slot_is_registered_and_urls_are_rejected() {
        let valid = validate("Hi {{their_name:name}}, see {{hook:reason}}.", None);
        assert!(valid.errors.is_empty(), "{:?}", valid.errors);
        assert!(valid.slots.iter().any(|slot| slot == "hook:reason"));
        let url = validate("Look at https://example.com for details.", None);
        assert!(url.errors.iter().any(|error| error.contains("URL")));
        let www = validate("See www.example.com for details.", None);
        assert!(www.errors.iter().any(|error| error.contains("URL")));
    }

    #[test]
    fn optional_omission_removes_only_the_dependent_sentence() {
        let body = "I can share {{proof:p}} with you. Would {{ask:question}} help?";
        let rules: Vec<Vec<String>> = vec![vec!["R3".into()], vec!["R2".into()]];
        let (omitted, tags) = apply_optional_omission(body, &rules, "R3", "omit_sentence").unwrap();
        assert_eq!(omitted, "Would {{ask:question}} help?");
        assert_eq!(tags, vec![vec!["R2".to_owned()]]);
        assert!(apply_optional_omission(body, &rules, "R3", "use_alternative_template").is_none());
        assert!(apply_optional_omission(body, &rules[..1], "R3", "omit_sentence").is_none());
    }
}
