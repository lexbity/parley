//! Canonical resource policy (UCR-001 U06).
//!
//! Pure resource-subtype validation used by the readiness gate shared by the
//! REST adapter and the registry `resource.mark_ready` command. Field names
//! and bounds are product policy; this module touches no I/O, so the same
//! gate runs inside the writer transaction without transport dependencies.

use serde_json::Value;

/// The closed set of resource subtypes that participate in material
/// eligibility and readiness. Skills, interests and notes are board records
/// outside this set (UCR-001 U06).
pub const MATERIAL_SUBTYPES: &[&str] = &["gift", "proof", "artifact"];

/// Resource subtypes a palette `resource.create_draft` prompt may offer.
pub const ALL_SUBTYPES: &[&str] = &["gift", "proof", "artifact", "skill", "interest", "note"];

/// Validate a resource body against its subtype's required fields.
///
/// Returns the missing-field names (bounded, in a stable order) when the body
/// is not ready; an empty list means the body satisfies every required field.
/// `None` is returned for an unsupported subtype.
pub fn validate_resource(subtype: &str, value: &Value) -> Option<Vec<String>> {
    let string = |name: &str, required: bool| -> bool {
        value
            .get(name)
            .and_then(Value::as_str)
            .is_some_and(|v| !v.trim().is_empty())
            || (!required && value.get(name).is_none())
    };
    let missing = match subtype {
        "gift" => {
            let mut fields = Vec::new();
            for key in ["what", "format", "delivery", "cost_to_me"] {
                if !string(key, true) {
                    fields.push(key.to_string());
                }
            }
            match value.get("delivery").and_then(Value::as_str) {
                Some("on_request") => {
                    if !value
                        .get("time_to_produce_minutes")
                        .and_then(Value::as_u64)
                        .is_some()
                    {
                        fields.push("time_to_produce_minutes".into());
                    }
                }
                Some("exists") => {
                    if !string("evidence_ref", true) {
                        fields.push("evidence_ref".into());
                    }
                    if value.get("permission_to_share").and_then(Value::as_bool) != Some(true) {
                        fields.push("permission_to_share".into());
                    }
                }
                _ => fields.push("delivery (exists or on_request)".into()),
            }
            if !matches!(
                value.get("cost_to_me").and_then(Value::as_str),
                Some("low" | "medium" | "high")
            ) {
                fields.push("cost_to_me (low, medium or high)".into());
            }
            if !matches!(
                value.get("format").and_then(Value::as_str),
                Some(
                    "teardown"
                        | "intro"
                        | "resource"
                        | "audit"
                        | "bug report"
                        | "template"
                        | "other"
                )
            ) {
                fields.push("format".into());
            }
            fields
        }
        "proof" => {
            let mut fields = Vec::new();
            for key in [
                "what_i_did",
                "for_whom",
                "problem",
                "result_unit",
                "period",
                "my_role",
            ] {
                if !string(key, true) {
                    fields.push(key.to_string());
                }
            }
            if !value
                .get("permission_to_name")
                .is_some_and(Value::is_boolean)
            {
                fields.push("permission_to_name".into());
            }
            if value
                .get("for_whom")
                .and_then(Value::as_str)
                .is_some_and(|who| who.split_whitespace().count() < 2)
            {
                fields.push("for_whom (named recipient or a two-attribute descriptor)".into());
            }
            if !value.get("result_value").is_some_and(|v| v.is_number()) {
                fields.push("result_value".into());
            }
            if value.get("result_unit").and_then(Value::as_str) == Some("%")
                && !string("baseline", true)
            {
                fields.push("baseline".into());
            }
            if value
                .get("what_i_did")
                .and_then(Value::as_str)
                .is_some_and(|text| text.split_whitespace().count() < 6)
            {
                fields.push("what_i_did (at least six words)".into());
            }
            fields
        }
        "artifact" => {
            let mut fields = Vec::new();
            for key in ["title", "location", "what_it_shows"] {
                if !string(key, true) {
                    fields.push(key.to_string());
                }
            }
            if !value.get("public").is_some_and(Value::is_boolean) {
                fields.push("public".into());
            }
            if value
                .get("location")
                .and_then(Value::as_str)
                .is_some_and(|location| location.trim().is_empty())
            {
                fields.push("location (URL or local path)".into());
            }
            fields
        }
        _ => return None,
    };
    Some(missing)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn subtypes_are_a_closed_set() {
        assert_eq!(MATERIAL_SUBTYPES, ["gift", "proof", "artifact"]);
        assert_eq!(
            ALL_SUBTYPES,
            ["gift", "proof", "artifact", "skill", "interest", "note"]
        );
        for subtype in ["skill", "interest", "note", "unknown", ""] {
            assert_eq!(validate_resource(subtype, &json!({})), None);
        }
    }

    #[test]
    fn gift_reports_every_missing_field_in_a_stable_order() {
        assert_eq!(
            validate_resource("gift", &json!({})).unwrap(),
            [
                "what",
                "format",
                "delivery",
                "cost_to_me",
                "delivery (exists or on_request)",
                "cost_to_me (low, medium or high)",
                "format",
            ]
        );
    }

    #[test]
    fn gift_on_request_requires_a_production_time() {
        let body = json!({
            "what": "A teardown",
            "format": "teardown",
            "delivery": "on_request",
            "cost_to_me": "low",
        });
        assert_eq!(
            validate_resource("gift", &body).unwrap(),
            ["time_to_produce_minutes"]
        );
        let mut ready = body;
        ready["time_to_produce_minutes"] = json!(30);
        assert!(validate_resource("gift", &ready).unwrap().is_empty());
    }

    #[test]
    fn gift_exists_requires_evidence_and_sharing_permission() {
        let body = json!({
            "what": "A proof",
            "format": "resource",
            "delivery": "exists",
            "cost_to_me": "medium",
        });
        assert_eq!(
            validate_resource("gift", &body).unwrap(),
            ["evidence_ref", "permission_to_share"]
        );
        let mut ready = body;
        ready["evidence_ref"] = json!("https://example.test/proof");
        ready["permission_to_share"] = json!(true);
        assert!(validate_resource("gift", &ready).unwrap().is_empty());
    }

    #[test]
    fn gift_rejects_unknown_delivery_cost_and_format() {
        let body = json!({
            "what": "Thing",
            "format": "mystery",
            "delivery": "later",
            "cost_to_me": "free",
        });
        assert_eq!(
            validate_resource("gift", &body).unwrap(),
            [
                "delivery (exists or on_request)",
                "cost_to_me (low, medium or high)",
                "format",
            ]
        );
    }

    #[test]
    fn proof_requires_all_fields_and_typed_values() {
        assert_eq!(
            validate_resource("proof", &json!({})).unwrap(),
            [
                "what_i_did",
                "for_whom",
                "problem",
                "result_unit",
                "period",
                "my_role",
                "permission_to_name",
                "result_value",
            ]
        );
        let body = json!({
            "what_i_did": "I built the parser for the team",
            "for_whom": "Acme hiring",
            "problem": "Slow triage",
            "result_unit": "%",
            "period": "2025",
            "my_role": "Author",
            "permission_to_name": true,
            "result_value": 40,
        });
        assert_eq!(validate_resource("proof", &body).unwrap(), ["baseline"]);
        let mut ready = body;
        ready["baseline"] = json!("10%");
        assert!(validate_resource("proof", &ready).unwrap().is_empty());
    }

    #[test]
    fn proof_enforces_word_counts_and_value_types() {
        let body = json!({
            "what_i_did": "I did",
            "for_whom": "Acme",
            "problem": "Slow triage",
            "result_unit": "days",
            "period": "2025",
            "my_role": "Author",
            "permission_to_name": "yes",
            "result_value": "40",
        });
        assert_eq!(
            validate_resource("proof", &body).unwrap(),
            [
                "permission_to_name",
                "for_whom (named recipient or a two-attribute descriptor)",
                "result_value",
                "what_i_did (at least six words)",
            ]
        );
    }

    #[test]
    fn artifact_requires_fields_public_and_a_real_location() {
        assert_eq!(
            validate_resource("artifact", &json!({})).unwrap(),
            ["title", "location", "what_it_shows", "public"]
        );
        let mut body = json!({
            "title": "Demo",
            "location": "   ",
            "what_it_shows": "The flow",
            "public": true,
        });
        assert_eq!(
            validate_resource("artifact", &body).unwrap(),
            ["location", "location (URL or local path)"]
        );
        body["location"] = json!("https://example.test/demo");
        assert!(validate_resource("artifact", &body).unwrap().is_empty());
        body["public"] = json!("yes");
        assert_eq!(validate_resource("artifact", &body).unwrap(), ["public"]);
    }
}
