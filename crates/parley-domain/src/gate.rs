//! Deterministic recipient-facing release policy (RFC-0001 §10.3).
//!
//! This module only evaluates facts prepared by the application layer. It has
//! no storage, clock, transport or model dependencies.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct GateCheck {
    pub id: String,
    pub severity: Severity,
    pub pass: bool,
    pub detail: String,
    pub overridden: bool,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Hard,
    Soft,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct GateResult {
    pub checks: Vec<GateCheck>,
    pub copy_enabled: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct GateInput {
    pub exchange_complete: bool,
    pub fairness_attested: bool,
    /// Every non-generic sentence has an owner-reviewed, eligible evidence binding.
    pub factual_sentences_grounded: bool,
    pub sentence_count: usize,
    pub bound_sentence_count: usize,
    pub slot_fills: Vec<String>,
    pub body: String,
    pub scarcity_claim_valid: bool,
    pub identity_claims_valid: bool,
    pub ask_count: usize,
    pub ask_rung: u32,
    pub highest_rung: u32,
    pub has_sensitive_content: bool,
    pub recipient_open: bool,
    pub recipient_do_not_contact: bool,
    pub references_private_artifact: bool,
    pub gift_in_body: bool,
    pub gift_delivered_before: bool,
    pub anchor_used: bool,
    pub rule_count: usize,
    pub rule_limit: usize,
    pub fluency_ok: bool,
    pub lint_error: bool,
    pub hook_fresh: bool,
    pub cadence_ok: bool,
    pub daily_cap_ok: bool,
    pub body_char_limit: usize,
    pub brief_reviewed: bool,
    pub override_reasons: BTreeMap<String, String>,
}

pub fn evaluate(input: &GateInput) -> GateResult {
    let mut checks = Vec::with_capacity(20);
    let mut push = |id: &'static str, severity: Severity, pass: bool, detail: String| {
        let reason = input
            .override_reasons
            .get(id)
            .map(String::as_str)
            .unwrap_or("");
        let overridden = severity == Severity::Soft && !pass && reason.trim().chars().count() >= 10;
        checks.push(GateCheck {
            id: id.to_owned(),
            severity,
            pass,
            detail,
            overridden,
        });
    };

    for (id, pass, detail) in [
        (
            "H1 EXCH_COMPLETE",
            input.exchange_complete,
            "exchange must state both sides' benefit and link an active interest for each side",
        ),
        (
            "H2 FAIR_ATTESTED",
            input.fairness_attested,
            "current exchange version requires an explicit owner attestation",
        ),
        (
            "H3 GROUNDED",
            input.factual_sentences_grounded && input.bound_sentence_count == input.sentence_count,
            "every factual sentence must map to reviewed, eligible evidence",
        ),
        (
            "H4 SLOTS_PRESERVED",
            input
                .slot_fills
                .iter()
                .all(|slot| !slot.is_empty() && input.body.contains(slot)),
            "every approved slot value must appear verbatim in the body",
        ),
        (
            "H5 SCARCITY_TRUE",
            input.scarcity_claim_valid,
            "capacity statements must match the current ledger exactly",
        ),
        (
            "H6 IDENTITY_TRUE",
            input.identity_claims_valid,
            "shared-identity claims require a verified tag held by both sides",
        ),
        (
            "H7 SINGLE_ASK",
            input.ask_count <= 1 && input.ask_rung <= input.highest_rung.saturating_add(1),
            "at most one ask is allowed, no higher than the next rung",
        ),
        (
            "H8 NO_SENSITIVE",
            !input.has_sensitive_content,
            "recipient-sensitive content is not allowed",
        ),
        (
            "H9 PARTY_OPEN",
            input.recipient_open && !input.recipient_do_not_contact,
            "recipient must be open and not on the do-not-contact list",
        ),
        (
            "H10 NO_PRIVATE_ARTIFACT",
            !input.references_private_artifact,
            "private artifacts cannot be referenced",
        ),
    ] {
        push(id, Severity::Hard, pass, detail.into());
    }

    for (id, pass, detail) in [
        (
            "S1 GIVE_FIRST",
            input.gift_in_body || input.gift_delivered_before,
            "include a real gift or one already delivered to this recipient",
        ),
        (
            "S2 ANCHOR",
            input.anchor_used,
            "include an approved proof, fresh hook or verified identity link",
        ),
        (
            "S3 STACK_LIMIT",
            input.rule_count <= input.rule_limit,
            "message uses more persuasion rules than the pack allows",
        ),
        (
            "S4 FLUENCY",
            input.fluency_ok,
            "every glossary term used must be marked fluent",
        ),
        (
            "S5 TONE_LINT",
            !input.lint_error,
            "deterministic tone lint reports an error",
        ),
        (
            "S6 HOOK_FRESH",
            input.hook_fresh,
            "the selected hook is stale or its date is unknown",
        ),
        (
            "S7 CADENCE",
            input.cadence_ok,
            "contact spacing or touch limit is not met",
        ),
        (
            "S8 DAILY_CAP",
            input.daily_cap_ok,
            "the configured daily send cap has been reached",
        ),
        (
            "S9 LENGTH",
            input.body.chars().count() <= input.body_char_limit,
            "message exceeds the channel character limit",
        ),
        (
            "S10 BRIEF_REVIEWED",
            input.brief_reviewed,
            "a proposed landmine in the brief needs review",
        ),
    ] {
        push(id, Severity::Soft, pass, detail.into());
    }

    let copy_enabled = checks
        .iter()
        .all(|check| check.pass || (check.severity == Severity::Soft && check.overridden));
    GateResult {
        checks,
        copy_enabled,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ready() -> GateInput {
        GateInput {
            exchange_complete: true,
            fairness_attested: true,
            factual_sentences_grounded: true,
            sentence_count: 1,
            bound_sentence_count: 1,
            slot_fills: vec!["Dana".into()],
            body: "Dana, would a checklist help?".into(),
            scarcity_claim_valid: true,
            identity_claims_valid: true,
            ask_count: 1,
            ask_rung: 1,
            highest_rung: 0,
            recipient_open: true,
            gift_in_body: true,
            anchor_used: true,
            rule_count: 2,
            rule_limit: 3,
            fluency_ok: true,
            hook_fresh: true,
            cadence_ok: true,
            daily_cap_ok: true,
            body_char_limit: 1000,
            brief_reviewed: true,
            ..GateInput::default()
        }
    }

    #[test]
    fn any_hard_failure_blocks_copy_even_with_soft_overrides() {
        let mut input = ready();
        input.fairness_attested = false;
        input.override_reasons.insert(
            "S1 GIVE_FIRST".into(),
            "No gift applies to this message.".into(),
        );
        let result = evaluate(&input);
        assert!(!result.copy_enabled);
        assert!(
            !result
                .checks
                .iter()
                .find(|check| check.id == "H2 FAIR_ATTESTED")
                .unwrap()
                .pass
        );
    }

    #[test]
    fn each_soft_failure_needs_a_qualifying_reason() {
        let mut input = ready();
        input.lint_error = true;
        input
            .override_reasons
            .insert("S5 TONE_LINT".into(), "short".into());
        assert!(!evaluate(&input).copy_enabled);
        input
            .override_reasons
            .insert("S5 TONE_LINT".into(), "Owner reviewed this wording.".into());
        let result = evaluate(&input);
        assert!(result.copy_enabled);
        assert!(
            result
                .checks
                .iter()
                .find(|check| check.id == "S5 TONE_LINT")
                .unwrap()
                .overridden
        );
    }

    #[test]
    fn slots_are_exact_and_soft_length_can_be_overridden() {
        let mut input = ready();
        input.body = "Dana, would a checklist help?".into();
        input.slot_fills = vec!["dana".into()];
        input.body_char_limit = 1;
        input
            .override_reasons
            .insert("S9 LENGTH".into(), "Reviewed this channel length.".into());
        let result = evaluate(&input);
        assert!(!result.copy_enabled);
        assert!(
            !result
                .checks
                .iter()
                .find(|check| check.id == "H4 SLOTS_PRESERVED")
                .unwrap()
                .pass
        );
    }
}
