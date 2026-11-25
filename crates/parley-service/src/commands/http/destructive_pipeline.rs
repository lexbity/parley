use super::*;

/// U08 destructive pipeline: planner preview, approval record, reservation-
/// time binding checks, and the atomic consume+effect inside one savepoint.
///
/// The effect and the approval consumption share the savepoint (Q5): any
/// failed effect rolls both back, so a grant survives a failed attempt and
/// remains usable on the original key. A dry run returns the observational
/// preview with zero domain effects and records its own dry-run receipt.
#[allow(clippy::too_many_arguments)]
pub(super) async fn destructive_pipeline(
    tx: &mut sqlx::Transaction<'_, Sqlite>,
    epoch: &str,
    actor: &Actor,
    command_id: &str,
    command_version: u32,
    mode: Mode,
    key_digest: &str,
    wire_digest: &str,
    canonical_digest: &str,
    audit_id: &str,
    args: &Value,
    result_schema: &'static Value,
    approval_id: Option<String>,
) -> Result<RunReply, crate::application::writer::Failure> {
    let execution = if command_id == crate::commands::resource::RETIRE_ID {
        Execution::Resource(ResourceCommand::Retire)
    } else {
        return Err(crate::application::writer::Failure::Unavailable);
    };
    let preview_schema: &'static Value = crate::commands::resource::retire_preview_schema();

    // Plan the current impact with SQL reads only (§5.3). Planner denials
    // are terminal, receipted rejections (they do not create an approval).
    let plan = match plan_for_execution(tx, &execution, args).await {
        Ok(plan) => plan,
        Err(Err(failure)) => return Err(failure),
        Err(Ok((status, code, message, details))) => {
            let error_body = json!({"code": code, "message": message, "details": details});
            let bytes = error_body.to_string().len() as i64;
            finalize_attempt(
                tx,
                epoch,
                audit_id,
                actor,
                command_id,
                command_version,
                mode,
                PolicyResult::DomainError,
                Some(code),
                Some(audit_id),
                Some(code),
                Some(&message),
                0,
                bytes,
                Some(crate::commands::ledger::ReceiptWrite {
                    key_digest,
                    wire_args_digest: wire_digest,
                    status,
                    body: &error_body,
                    bytes,
                }),
            )
            .await
            .map_err(|_| crate::application::writer::Failure::Unavailable)?;
            return Ok(RunReply {
                status,
                ok: false,
                inner: error_body,
                replayed: false,
                original_audit_id: None,
                approval_id: None,
            });
        }
    };
    let preview = plan.preview();
    let preview_digest = match parley_domain::jcs::canonicalize_value(&preview) {
        Ok(canonical) => crate::digest(canonical.as_bytes()),
        Err(_) => return Err(crate::application::writer::Failure::Unavailable),
    };
    let fingerprint = plan.fingerprint();

    // Dry run: same access checks and planner, zero domain effects, and its
    // own dry-run receipt (§5 "Approval previews also pass admission...").
    if mode == Mode::DryRun {
        let bytes = preview.to_string().len() as i64;
        if bytes > MAX_RESULT_BYTES {
            let error_body = json!({
                "code": "result_too_large",
                "message": "Preview exceeds the 64 KiB limit",
                "details": []
            });
            let bytes = error_body.to_string().len() as i64;
            finalize_attempt(
                tx,
                epoch,
                audit_id,
                actor,
                command_id,
                command_version,
                mode,
                PolicyResult::DomainError,
                Some("result_too_large"),
                Some(audit_id),
                Some("result_too_large"),
                Some("Preview exceeds the 64 KiB limit"),
                0,
                bytes,
                Some(crate::commands::ledger::ReceiptWrite {
                    key_digest,
                    wire_args_digest: wire_digest,
                    status: 413,
                    body: &error_body,
                    bytes,
                }),
            )
            .await
            .map_err(|_| crate::application::writer::Failure::Unavailable)?;
            return Ok(RunReply {
                status: 413,
                ok: false,
                inner: error_body,
                replayed: false,
                original_audit_id: None,
                approval_id: None,
            });
        }
        let valid = match validator_for(preview_schema) {
            Ok(validator) => validator.validate(&preview).is_ok(),
            Err(_) => false,
        };
        if !valid {
            return Err(crate::application::writer::Failure::Unavailable);
        }
        finalize_attempt(
            tx,
            epoch,
            audit_id,
            actor,
            command_id,
            command_version,
            mode,
            PolicyResult::Allowed,
            Some("ok"),
            Some(audit_id),
            None,
            None,
            0,
            bytes,
            Some(crate::commands::ledger::ReceiptWrite {
                key_digest,
                wire_args_digest: wire_digest,
                status: 200,
                body: &preview,
                bytes,
            }),
        )
        .await
        .map_err(|_| crate::application::writer::Failure::Unavailable)?;
        return Ok(RunReply {
            status: 200,
            ok: true,
            inner: preview,
            replayed: false,
            original_audit_id: None,
            approval_id: None,
        });
    }

    // Execute path: resolve the durable approval for this key.
    let approval = approval::lookup_by_key(tx, epoch, &actor.pseudonym, command_id, key_digest)
        .await
        .map_err(|_| crate::application::writer::Failure::Unavailable)?;
    let Some(approval) = approval else {
        if approval_id.is_some() {
            let error_body = json!({
                "code": "approval_invalid",
                "message": "The presented approval does not match this invocation",
                "details": []
            });
            let bytes = error_body.to_string().len() as i64;
            finalize_attempt(
                tx,
                epoch,
                audit_id,
                actor,
                command_id,
                command_version,
                mode,
                PolicyResult::DomainError,
                Some("approval_invalid"),
                Some(audit_id),
                Some("approval_invalid"),
                Some("The presented approval does not match this invocation"),
                0,
                bytes,
                Some(crate::commands::ledger::ReceiptWrite {
                    key_digest,
                    wire_args_digest: wire_digest,
                    status: 403,
                    body: &error_body,
                    bytes,
                }),
            )
            .await
            .map_err(|_| crate::application::writer::Failure::Unavailable)?;
            return Ok(RunReply {
                status: 403,
                ok: false,
                inner: error_body,
                replayed: false,
                original_audit_id: None,
                approval_id: None,
            });
        }
        // First destructive execution request without a grant: record the
        // pending approval with the observational preview and dependency
        // fingerprint, then return the waiting outcome (FR-8).
        let owner = approval_owner(tx, actor).await?;
        let normalized_args =
            crate::commands::resource::canonical_args(args).unwrap_or_else(|| args.clone());
        let args_json = serde_json::to_string(&normalized_args)
            .map_err(|_| crate::application::writer::Failure::Unavailable)?;
        let id = approval::create_pending(
            tx,
            &approval::NewApproval {
                epoch,
                owner_pseudonym: &owner,
                principal_pseudonym: &actor.pseudonym,
                run_pseudonym: actor.run_id.as_deref(),
                command_id,
                command_version: command_version as i64,
                key_digest,
                wire_args_digest: wire_digest,
                canonical_args_digest: canonical_digest,
                dependency_fingerprint: &fingerprint,
                preview_digest: &preview_digest,
                args_json,
                preview_json: preview.to_string(),
            },
        )
        .await
        .map_err(|_| crate::application::writer::Failure::Unavailable)?;
        let error_body = json!({
            "code": "approval_required",
            "message": "Review the requested retirement in Approvals.",
            "details": [{"path": "", "problem": "approval_pending"}]
        });
        let bytes = error_body.to_string().len() as i64;
        finalize_attempt(
            tx,
            epoch,
            audit_id,
            actor,
            command_id,
            command_version,
            mode,
            PolicyResult::DomainError,
            Some("approval_required"),
            Some(audit_id),
            Some("approval_required"),
            Some("Review the requested retirement in Approvals."),
            0,
            bytes,
            None,
        )
        .await
        .map_err(|_| crate::application::writer::Failure::Unavailable)?;
        return Ok(RunReply {
            status: 403,
            ok: false,
            inner: error_body,
            replayed: false,
            original_audit_id: None,
            approval_id: Some(id),
        });
    };

    // Same key, different intent is terminal for that key (Q2).
    if approval.wire_args_digest != wire_digest
        || approval.canonical_args_digest != canonical_digest
    {
        let error_body = json!({
            "code": "idempotency_mismatch",
            "message": "Idempotency key was used for a different command",
            "details": []
        });
        let bytes = error_body.to_string().len() as i64;
        finalize_attempt(
            tx,
            epoch,
            audit_id,
            actor,
            command_id,
            command_version,
            mode,
            PolicyResult::DomainError,
            Some("idempotency_mismatch"),
            Some(audit_id),
            Some("idempotency_mismatch"),
            Some("Idempotency key was used for a different command"),
            0,
            bytes,
            Some(crate::commands::ledger::ReceiptWrite {
                key_digest,
                wire_args_digest: wire_digest,
                status: 422,
                body: &error_body,
                bytes,
            }),
        )
        .await
        .map_err(|_| crate::application::writer::Failure::Unavailable)?;
        return Ok(RunReply {
            status: 422,
            ok: false,
            inner: error_body,
            replayed: false,
            original_audit_id: None,
            approval_id: None,
        });
    }

    match approval.status {
        ApprovalStatus::Pending => {
            // Pending approvals also carry a TTL; an expired pending intent
            // fails closed instead of returning the same waiting location.
            if approval_expired(&approval.expires_at) {
                approval::expire_one(tx, &approval.id)
                    .await
                    .map_err(|_| crate::application::writer::Failure::Unavailable)?;
                return approval_deny_reply(
                    tx,
                    epoch,
                    audit_id,
                    actor,
                    command_id,
                    command_version,
                    mode,
                    key_digest,
                    wire_digest,
                    403,
                    "approval_invalid",
                    "The approval has expired",
                )
                .await;
            }
            // Repeated requests for the same pending intent return the same
            // location (§4.1).
            let error_body = json!({
                "code": "approval_required",
                "message": "Review the requested retirement in Approvals.",
                "details": [{"path": "", "problem": "approval_pending"}]
            });
            let bytes = error_body.to_string().len() as i64;
            finalize_attempt(
                tx,
                epoch,
                audit_id,
                actor,
                command_id,
                command_version,
                mode,
                PolicyResult::DomainError,
                Some("approval_required"),
                Some(audit_id),
                Some("approval_required"),
                Some("Review the requested retirement in Approvals."),
                0,
                bytes,
                None,
            )
            .await
            .map_err(|_| crate::application::writer::Failure::Unavailable)?;
            Ok(RunReply {
                status: 403,
                ok: false,
                inner: error_body,
                replayed: false,
                original_audit_id: None,
                approval_id: Some(approval.id),
            })
        }
        ApprovalStatus::Granted => {
            if approval_expired(&approval.expires_at) {
                approval::expire_one(tx, &approval.id)
                    .await
                    .map_err(|_| crate::application::writer::Failure::Unavailable)?;
                return approval_deny_reply(
                    tx,
                    epoch,
                    audit_id,
                    actor,
                    command_id,
                    command_version,
                    mode,
                    key_digest,
                    wire_digest,
                    403,
                    "approval_invalid",
                    "The approval has expired",
                )
                .await;
            }
            // Execution recomputes the fingerprint under reservation before
            // consuming the grant (Q5): a changed dependency is a stale
            // denial, never a silently broadened authorization.
            if approval.dependency_fingerprint != fingerprint {
                approval::mark_stale(tx, &approval.id)
                    .await
                    .map_err(|_| crate::application::writer::Failure::Unavailable)?;
                return approval_deny_reply(
                    tx,
                    epoch,
                    audit_id,
                    actor,
                    command_id,
                    command_version,
                    mode,
                    key_digest,
                    wire_digest,
                    409,
                    "approval_stale",
                    "The intent dependencies changed; request a fresh preview with a new key",
                )
                .await;
            }
            if let Some(presented) = approval_id.as_deref()
                && presented != approval.id
            {
                return approval_deny_reply(
                    tx,
                    epoch,
                    audit_id,
                    actor,
                    command_id,
                    command_version,
                    mode,
                    key_digest,
                    wire_digest,
                    403,
                    "approval_invalid",
                    "The presented approval does not match this invocation",
                )
                .await;
            }
            let _ = approval;
            // Atomic consume + effect inside one savepoint. A domain denial, an
            // invalid output or an oversize result rolls the consumption back
            // with the operation, so the grant stays usable on the original
            // key (Q5 / §5.3).
            sqlx::query("SAVEPOINT ucr_operation")
                .execute(&mut **tx)
                .await
                .map_err(|_| crate::application::writer::Failure::Unavailable)?;
            let consumed = approval::consume(tx, &approval.id, approval.revision, audit_id)
                .await
                .map_err(|_| crate::application::writer::Failure::Unavailable)?;
            if !consumed {
                sqlx::query("ROLLBACK TO SAVEPOINT ucr_operation")
                    .execute(&mut **tx)
                    .await
                    .map_err(|_| crate::application::writer::Failure::Unavailable)?;
                sqlx::query("RELEASE SAVEPOINT ucr_operation")
                    .execute(&mut **tx)
                    .await
                    .map_err(|_| crate::application::writer::Failure::Unavailable)?;
                return approval_deny_reply(
                    tx,
                    epoch,
                    audit_id,
                    actor,
                    command_id,
                    command_version,
                    mode,
                    key_digest,
                    wire_digest,
                    403,
                    "approval_invalid",
                    "The approval is no longer actionable",
                )
                .await;
            }
            let executed = match execution {
                Execution::Resource(ResourceCommand::Retire) => {
                    crate::commands::resource::run(tx, ResourceCommand::Retire, args, key_digest)
                        .await
                        .map_err(OperationError::Resource)
                }
                _ => unreachable!("destructive command validated above"),
            };
            match executed {
                Ok(outcome) => {
                    let value = outcome.result;
                    let body_bytes = serde_json::to_string(&value).unwrap_or_default().len() as i64;
                    if body_bytes > MAX_RESULT_BYTES {
                        sqlx::query("ROLLBACK TO SAVEPOINT ucr_operation")
                            .execute(&mut **tx)
                            .await
                            .map_err(|_| crate::application::writer::Failure::Unavailable)?;
                        sqlx::query("RELEASE SAVEPOINT ucr_operation")
                            .execute(&mut **tx)
                            .await
                            .map_err(|_| crate::application::writer::Failure::Unavailable)?;
                        return approval_deny_reply(
                            tx,
                            epoch,
                            audit_id,
                            actor,
                            command_id,
                            command_version,
                            mode,
                            key_digest,
                            wire_digest,
                            413,
                            "result_too_large",
                            "Result exceeds the 64 KiB limit",
                        )
                        .await;
                    }
                    let valid = match validator_for(result_schema) {
                        Ok(validator) => validator.validate(&value).is_ok(),
                        Err(_) => false,
                    };
                    if !valid {
                        sqlx::query("ROLLBACK TO SAVEPOINT ucr_operation")
                            .execute(&mut **tx)
                            .await
                            .map_err(|_| crate::application::writer::Failure::Unavailable)?;
                        sqlx::query("RELEASE SAVEPOINT ucr_operation")
                            .execute(&mut **tx)
                            .await
                            .map_err(|_| crate::application::writer::Failure::Unavailable)?;
                        return approval_deny_reply(
                            tx,
                            epoch,
                            audit_id,
                            actor,
                            command_id,
                            command_version,
                            mode,
                            key_digest,
                            wire_digest,
                            500,
                            "internal",
                            "Operation produced an invalid result",
                        )
                        .await;
                    }
                    sqlx::query("RELEASE SAVEPOINT ucr_operation")
                        .execute(&mut **tx)
                        .await
                        .map_err(|_| crate::application::writer::Failure::Unavailable)?;
                    if outcome.effected {
                        debit_effect(tx, actor, Effect::Destructive).await?;
                    }
                    finalize_attempt(
                        tx,
                        epoch,
                        audit_id,
                        actor,
                        command_id,
                        command_version,
                        mode,
                        PolicyResult::Allowed,
                        Some("ok"),
                        Some(audit_id),
                        None,
                        None,
                        0,
                        body_bytes,
                        Some(crate::commands::ledger::ReceiptWrite {
                            key_digest,
                            wire_args_digest: wire_digest,
                            status: 200,
                            body: &value,
                            bytes: body_bytes,
                        }),
                    )
                    .await
                    .map_err(|_| crate::application::writer::Failure::Unavailable)?;
                    Ok(RunReply {
                        status: 200,
                        ok: true,
                        inner: value,
                        replayed: false,
                        original_audit_id: None,
                        approval_id: None,
                    })
                }
                Err(error) => {
                    sqlx::query("ROLLBACK TO SAVEPOINT ucr_operation")
                        .execute(&mut **tx)
                        .await
                        .map_err(|_| crate::application::writer::Failure::Unavailable)?;
                    sqlx::query("RELEASE SAVEPOINT ucr_operation")
                        .execute(&mut **tx)
                        .await
                        .map_err(|_| crate::application::writer::Failure::Unavailable)?;
                    let mapped: (i64, &'static str, String, Vec<Value>) = match error {
                        OperationError::Resource(ResourceRunError::Retire(retirement_error)) => {
                            map_retire_error(retirement_error)?
                        }
                        _ => unreachable!("destructive command validated above"),
                    };
                    let (status, code, message, details) = mapped;
                    let error_body = json!({"code": code, "message": message, "details": details});
                    let bytes = error_body.to_string().len() as i64;
                    finalize_attempt(
                        tx,
                        epoch,
                        audit_id,
                        actor,
                        command_id,
                        command_version,
                        mode,
                        PolicyResult::DomainError,
                        Some(code),
                        Some(audit_id),
                        Some(code),
                        Some(&message),
                        0,
                        bytes,
                        Some(crate::commands::ledger::ReceiptWrite {
                            key_digest,
                            wire_args_digest: wire_digest,
                            status,
                            body: &error_body,
                            bytes,
                        }),
                    )
                    .await
                    .map_err(|_| crate::application::writer::Failure::Unavailable)?;
                    Ok(RunReply {
                        status,
                        ok: false,
                        inner: error_body,
                        replayed: false,
                        original_audit_id: None,
                        approval_id: None,
                    })
                }
            }
        }
        ApprovalStatus::Consumed => {
            approval_deny_reply(
                tx,
                epoch,
                audit_id,
                actor,
                command_id,
                command_version,
                mode,
                key_digest,
                wire_digest,
                403,
                "approval_invalid",
                "The approval was already consumed",
            )
            .await
        }
        ApprovalStatus::Denied => {
            approval_deny_reply(
                tx,
                epoch,
                audit_id,
                actor,
                command_id,
                command_version,
                mode,
                key_digest,
                wire_digest,
                403,
                "approval_invalid",
                "The approval was denied",
            )
            .await
        }
        ApprovalStatus::Revoked => {
            approval_deny_reply(
                tx,
                epoch,
                audit_id,
                actor,
                command_id,
                command_version,
                mode,
                key_digest,
                wire_digest,
                403,
                "approval_invalid",
                "The approval was revoked",
            )
            .await
        }
        ApprovalStatus::Expired => {
            approval_deny_reply(
                tx,
                epoch,
                audit_id,
                actor,
                command_id,
                command_version,
                mode,
                key_digest,
                wire_digest,
                403,
                "approval_invalid",
                "The approval has expired",
            )
            .await
        }
        ApprovalStatus::Stale => {
            approval_deny_reply(
                tx,
                epoch,
                audit_id,
                actor,
                command_id,
                command_version,
                mode,
                key_digest,
                wire_digest,
                409,
                "approval_stale",
                "The intent dependencies changed; request a fresh preview with a new key",
            )
            .await
        }
    }
}
