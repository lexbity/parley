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

/// Resolvers implemented by the deterministic S03 authoring path. Other
/// declared slot kinds remain invalid until their evidence bank and resolver
/// are part of the same release path.
pub const REGISTERED_SLOT_TYPES: &[&str] = &[
    "their_name",
    "sender_name",
    "gift",
    "proof",
    "artifact",
    "ask",
    "custom",
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TemplateValidation {
    pub slots: Vec<String>,
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

fn first_unresolved_proper_noun(fixed: &str) -> Option<String> {
    let mut sentence_start = true;
    let mut seen = BTreeSet::new();
    for token in fixed.split_whitespace() {
        let trimmed = token.trim_matches(|ch: char| !ch.is_alphanumeric() && ch != '\'');
        if trimmed.is_empty() {
            continue;
        }
        let is_sentence_start = sentence_start;
        sentence_start = token.ends_with('.') || token.ends_with('!') || token.ends_with('?');
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
}
