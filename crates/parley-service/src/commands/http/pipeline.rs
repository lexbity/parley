use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) async fn pipeline(
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
    args_schema: &'static Value,
    result_schema: &'static Value,
    required_permission: &'static str,
    effect: Effect,
    requires_self_created: bool,
    approval_id: Option<String>,
) -> Result<RunReply, crate::application::writer::Failure> {
    // Recheck the epoch under reservation.
    let current_epoch: Option<String> =
        sqlx::query_scalar("SELECT epoch FROM workspace_meta WHERE singleton=1")
            .fetch_optional(&mut **tx)
            .await
            .map_err(|_| crate::application::writer::Failure::Unavailable)?
            .flatten();
    if current_epoch.as_deref() != Some(epoch) {
        return Ok(RunReply {
            status: 409,
            ok: false,
            inner: json!({"code": "epoch_changed", "message": "Workspace changed; reconnect before continuing", "details": []}),
            replayed: false,
            original_audit_id: None,

            approval_id: None,
        });
    }

    // Authorize every retry against current authority *before* looking up a
    // receipt. A revoked run never replays (Q2).
    if let Some((status, code, message)) =
        agent_scope_denial(tx, actor, required_permission, args, requires_self_created).await?
    {
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
            0,
            None,
        )
        .await
        .map_err(|_| crate::application::writer::Failure::Unavailable)?;
        return Ok(RunReply {
            status,
            ok: false,
            inner: json!({
                "code": code,
                "message": message,
                "details": []
            }),
            replayed: false,
            original_audit_id: None,

            approval_id: None,
        });
    }

    // Replay path: same key, same intent.
    if let Some(record) = lookup_receipt(tx, epoch, &actor.pseudonym, command_id, key_digest)
        .await
        .map_err(|_| crate::application::writer::Failure::Unavailable)?
    {
        if record.command_version == command_version as i64
            && record.mode == mode.as_str()
            && record.wire_args_digest == wire_digest
        {
            finalize_attempt(
                tx,
                epoch,
                audit_id,
                actor,
                command_id,
                command_version,
                mode,
                PolicyResult::Replayed,
                Some("replayed"),
                Some(&record.original_audit_id),
                None,
                None,
                0,
                0,
                None,
            )
            .await
            .map_err(|_| crate::application::writer::Failure::Unavailable)?;
            return Ok(RunReply {
                status: record.status,
                ok: record.status == 200,
                inner: record.body,
                replayed: true,
                original_audit_id: Some(record.original_audit_id),
                approval_id: None,
            });
        }
        // Same key, different intent: terminal mismatch for this key.
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
            0,
            None,
        )
        .await
        .map_err(|_| crate::application::writer::Failure::Unavailable)?;
        return Ok(RunReply {
            status: 422,
            ok: false,
            inner: json!({
                "code": "idempotency_mismatch",
                "message": "Idempotency key was used for a different command",
                "details": []
            }),
            replayed: false,
            original_audit_id: None,

            approval_id: None,
        });
    }

    if let Some((status, code, message)) = effect_allowance_denial(tx, actor, effect, mode).await? {
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
            0,
            None,
        )
        .await
        .map_err(|_| crate::application::writer::Failure::Unavailable)?;
        return Ok(RunReply {
            status,
            ok: false,
            inner: json!({"code": code, "message": message, "details": []}),
            replayed: false,
            original_audit_id: None,

            approval_id: None,
        });
    }

    // One closed-schema gate for every registered command, including reads.
    // Validation-engine messages are never exposed to a caller (§4.1).
    let valid = match validator_for(args_schema) {
        Ok(validator) => validator.validate(args).is_ok(),
        Err(_) => false,
    };
    if !valid {
        let error_body = json!({
            "code": "invalid_args",
            "message": "Arguments do not match the command schema",
            "details": [{"path": "", "problem": "schema_mismatch"}]
        });
        finalize_attempt(
            tx,
            epoch,
            audit_id,
            actor,
            command_id,
            command_version,
            mode,
            PolicyResult::DomainError,
            Some("invalid_args"),
            Some(audit_id),
            Some("invalid_args"),
            Some("Arguments do not match the command schema"),
            0,
            0,
            Some(crate::commands::ledger::ReceiptWrite {
                key_digest,
                wire_args_digest: wire_digest,
                status: 422,
                body: &error_body,
                bytes: error_body.to_string().len() as i64,
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
    charge_invocation(tx, actor).await?;

    // U08: destructive registry commands require human approval (Q5 / FR-8).
    // The planner, approval bindings and the atomic consume+effect handling
    // live in `destructive_pipeline`; dry runs and first requests produce an
    // observational preview instead of an effect.
    if effect == Effect::Destructive {
        return destructive_pipeline::destructive_pipeline(
            tx,
            epoch,
            actor,
            command_id,
            command_version,
            mode,
            key_digest,
            wire_digest,
            canonical_digest,
            audit_id,
            args,
            result_schema,
            approval_id,
        )
        .await;
    }

    // U02/U05 read commands run on the same pipeline but never mutate facts.
    if let Some(read_command) = crate::commands::read::ReadCommand::for_id(command_id) {
        let reply = match crate::commands::read::run(read_command, tx, args, actor).await {
            Ok(value) => RunReply {
                status: 200,
                ok: true,
                inner: value,
                replayed: false,
                original_audit_id: None,

                approval_id: None,
            },
            Err(_) => RunReply {
                status: 404,
                ok: false,
                inner: json!({
                    "code": "target_unavailable",
                    "message": "Target record is not available",
                    "details": []
                }),
                replayed: false,
                original_audit_id: None,

                approval_id: None,
            },
        };
        let bytes = reply.inner.to_string().len() as i64;
        let (policy_result, outcome_code, error_code, error_message) = if reply.ok {
            (PolicyResult::Allowed, "ok", None, None)
        } else {
            (
                PolicyResult::DomainError,
                "target_unavailable",
                Some("target_unavailable"),
                Some("Target record is not available"),
            )
        };
        finalize_attempt(
            tx,
            epoch,
            audit_id,
            actor,
            command_id,
            command_version,
            mode,
            policy_result,
            Some(outcome_code),
            Some(audit_id),
            error_code,
            error_message,
            0,
            bytes,
            Some(crate::commands::ledger::ReceiptWrite {
                key_digest,
                wire_args_digest: wire_digest,
                status: reply.status,
                body: &reply.inner,
                bytes,
            }),
        )
        .await
        .map_err(|_| crate::application::writer::Failure::Unavailable)?;
        return Ok(reply);
    }

    let execution = if command_id == opportunity::CREATE_ID {
        Execution::Create
    } else if let Some(command) = PhaseCommand::for_id(command_id) {
        Execution::Phase(command)
    } else if let Some(command) = PartyCommand::for_id(command_id) {
        Execution::Party(command)
    } else if command_id == task::TASK_DONE_ID {
        Execution::Task
    } else if command_id == jobs::BRIEF_START_ID {
        Execution::Brief
    } else if let Some(command) = ResourceCommand::for_id(command_id) {
        Execution::Resource(command)
    } else if let Some(command) = WritingCommand::for_id(command_id) {
        Execution::Writing(command)
    } else {
        finalize_attempt(
            tx,
            epoch,
            audit_id,
            actor,
            command_id,
            command_version,
            mode,
            PolicyResult::DomainError,
            Some("unknown_command"),
            Some(audit_id),
            Some("unknown_command"),
            Some("Command is not available"),
            0,
            0,
            None,
        )
        .await
        .map_err(|_| crate::application::writer::Failure::Unavailable)?;
        return Ok(RunReply {
            status: 404,
            ok: false,
            inner: json!({
                "code": "unknown_command",
                "message": "Command is not available",
                "details": []
            }),
            replayed: false,
            original_audit_id: None,

            approval_id: None,
        });
    };

    // The typed operation runs inside a savepoint. A domain denial, an
    // invalid output or an oversize result rolls the operation back while
    // the outer transaction still commits its audited terminal outcome
    // and receipt (UCR-001 Q3).
    sqlx::query("SAVEPOINT ucr_operation")
        .execute(&mut **tx)
        .await
        .map_err(|_| crate::application::writer::Failure::Unavailable)?;

    let created_by_run = if actor.kind == ActorKind::Agent {
        actor.run_id.clone()
    } else {
        None
    };
    let executed: Result<crate::commands::RunOutcome, OperationError> = match execution {
        Execution::Create => opportunity::run(tx, args, key_digest, created_by_run)
            .await
            .map_err(OperationError::Create),
        Execution::Phase(command) => run_phase(command, tx, args, key_digest)
            .await
            .map_err(OperationError::Phase),
        Execution::Party(command) => run_party(command, tx, args, key_digest)
            .await
            .map_err(OperationError::Party),
        Execution::Task => run_task(tx, args, key_digest)
            .await
            .map_err(OperationError::Task),
        Execution::Brief => run_jobs(tx, args, key_digest, actor)
            .await
            .map_err(OperationError::Jobs),
        Execution::Resource(command) => {
            crate::commands::resource::run(tx, command, args, key_digest)
                .await
                .map_err(OperationError::Resource)
        }
        Execution::Writing(command) => crate::commands::writing::run(command, tx, args, actor)
            .await
            .map_err(OperationError::Writing),
    };
    // A storage failure is not a domain conflict: abandon the transaction and
    // let admission record the terminal audit outcome with no effect.
    if matches!(
        executed,
        Err(OperationError::Create(CreateRunError::Create(
            CreateError::Database
        )))
    ) {
        return Err(crate::application::writer::Failure::Unavailable);
    }
    let checked: CheckedResult = match executed {
        Ok(outcome) => {
            let value = outcome.result;
            let bytes = serde_json::to_string(&value).unwrap_or_default().len() as i64;
            if bytes > MAX_RESULT_BYTES {
                Err((
                    413,
                    "result_too_large",
                    "Result exceeds the 64 KiB limit".to_string(),
                    vec![],
                ))
            } else {
                let valid = match validator_for(result_schema) {
                    Ok(validator) => validator.validate(&value).is_ok(),
                    Err(_) => false,
                };
                if valid {
                    Ok((value, bytes, outcome.effected))
                } else {
                    Err((
                        500,
                        "internal",
                        "Operation produced an invalid result".to_string(),
                        vec![],
                    ))
                }
            }
        }
        Err(error) => {
            let mapped: (i64, &'static str, String, Vec<Value>) = match error {
                OperationError::Phase(PhaseRunError::InvalidArgs { path, problem }) => (
                    422,
                    "invalid_args",
                    "Arguments do not match the command rules".to_string(),
                    vec![json!({"path": path, "problem": problem})],
                ),
                OperationError::Phase(PhaseRunError::Phase(PhaseError::NotFound)) => (
                    404,
                    "target_unavailable",
                    "Target record is not available".to_string(),
                    vec![],
                ),
                OperationError::Phase(PhaseRunError::Phase(PhaseError::InvalidStage)) => (
                    422,
                    "invalid_args",
                    "Stage is not valid for the selected pack".to_string(),
                    vec![json!({"path": "stage_id", "problem": "unknown_stage"})],
                ),
                OperationError::Phase(PhaseRunError::Phase(PhaseError::TerminalStage)) => (
                    409,
                    "requires_owner_workflow",
                    "Terminal stages require the explicit lifecycle flow".to_string(),
                    vec![],
                ),
                OperationError::Phase(PhaseRunError::Phase(PhaseError::LifecycleBlocked)) => (
                    409,
                    "conflict",
                    "Lifecycle state blocks this command".to_string(),
                    vec![json!({"path": "", "problem": "lifecycle_blocked"})],
                ),
                OperationError::Phase(PhaseRunError::Phase(PhaseError::StaleRevision)) => (
                    409,
                    "version_mismatch",
                    "Expected revision is stale; reload and create a new intent".to_string(),
                    vec![],
                ),
                OperationError::Phase(PhaseRunError::Phase(PhaseError::PackMismatch)) => (
                    409,
                    "version_mismatch",
                    "Pack identity does not match the target".to_string(),
                    vec![],
                ),
                OperationError::Phase(PhaseRunError::Phase(PhaseError::UndoInvalidReference)) => (
                    422,
                    "invalid_args",
                    "Undo reference is not a valid manual phase transition".to_string(),
                    vec![json!({"path": "original_event_id", "problem": "invalid_undo_reference"})],
                ),
                OperationError::Phase(PhaseRunError::Phase(PhaseError::UndoNotLatest)) => (
                    409,
                    "conflict",
                    "The referenced transition is no longer the latest".to_string(),
                    vec![json!({"path": "", "problem": "undo_stale"})],
                ),
                OperationError::Phase(PhaseRunError::Phase(PhaseError::UndoTargetMismatch)) => (
                    422,
                    "invalid_args",
                    "Undo must target the stage recorded before the original transition"
                        .to_string(),
                    vec![json!({"path": "stage_id", "problem": "undo_target_mismatch"})],
                ),
                OperationError::Create(CreateRunError::InvalidArgs) => (
                    422,
                    "invalid_args",
                    "Arguments do not match the command rules".to_string(),
                    vec![json!({"path": "", "problem": "invalid_args"})],
                ),
                OperationError::Create(CreateRunError::Create(CreateError::InvalidTitle)) => (
                    422,
                    "invalid_args",
                    "Opportunity title must contain 1–500 bytes".to_string(),
                    vec![json!({"path": "/title", "problem": "invalid_title"})],
                ),
                OperationError::Create(CreateRunError::Create(CreateError::TargetUnavailable)) => (
                    404,
                    "target_unavailable",
                    "Primary party is not available".to_string(),
                    vec![json!({"path": "/primary_party_id", "problem": "unknown_party"})],
                ),
                OperationError::Create(CreateRunError::Create(CreateError::NoActiveList)) => (
                    409,
                    "conflict",
                    "Workspace must retain an active board list".to_string(),
                    vec![],
                ),
                OperationError::Party(PartyRunError::InvalidArgs) => (
                    422,
                    "invalid_args",
                    "Arguments do not match the command rules".to_string(),
                    vec![json!({"path": "", "problem": "invalid_args"})],
                ),
                OperationError::Party(PartyRunError::Party(PartyEditError::NotFound)) => (
                    404,
                    "target_unavailable",
                    "Target record is not available".to_string(),
                    vec![],
                ),
                OperationError::Party(PartyRunError::Party(PartyEditError::StaleRevision)) => (
                    409,
                    "version_mismatch",
                    "Expected revision is stale; reload and create a new intent".to_string(),
                    vec![],
                ),
                OperationError::Party(PartyRunError::Party(PartyEditError::InvalidName)) => (
                    422,
                    "invalid_args",
                    "Party name must contain 1–500 bytes".to_string(),
                    vec![json!({"path": "/name", "problem": "invalid_name"})],
                ),
                OperationError::Party(PartyRunError::Party(PartyEditError::InvalidFocus)) => (
                    422,
                    "invalid_args",
                    "Focus exceeds 250 characters".to_string(),
                    vec![json!({"path": "/focus", "problem": "invalid_focus"})],
                ),
                OperationError::Party(PartyRunError::Party(PartyEditError::NoFields)) => (
                    422,
                    "invalid_args",
                    "No party fields to update".to_string(),
                    vec![json!({"path": "", "problem": "no_fields"})],
                ),
                OperationError::Task(TaskRunError::InvalidArgs) => (
                    422,
                    "invalid_args",
                    "Arguments do not match the command rules".to_string(),
                    vec![json!({"path": "", "problem": "invalid_args"})],
                ),
                OperationError::Task(TaskRunError::Task(TaskError::NotFound)) => (
                    404,
                    "target_unavailable",
                    "Target record is not available".to_string(),
                    vec![],
                ),
                OperationError::Task(TaskRunError::Task(TaskError::StaleRevision)) => (
                    409,
                    "version_mismatch",
                    "Expected notes revision is stale; reload and create a new intent".to_string(),
                    vec![],
                ),
                OperationError::Task(TaskRunError::Task(TaskError::InvalidSpan(
                    "task line changed; reload notes before toggling",
                ))) => (
                    409,
                    "conflict",
                    "The addressed task line changed; reload notes before toggling".to_string(),
                    vec![json!({"path": "/line_sha256", "problem": "task_line_changed"})],
                ),
                OperationError::Task(TaskRunError::Task(TaskError::InvalidSpan(message))) => (
                    422,
                    "invalid_args",
                    message.to_string(),
                    vec![json!({"path": "/byte_start", "problem": "invalid_span"})],
                ),
                OperationError::Jobs(JobRunError::InvalidArgs) => (
                    422,
                    "invalid_args",
                    "Arguments do not match the command rules".to_string(),
                    vec![json!({"path": "", "problem": "invalid_args"})],
                ),
                OperationError::Jobs(JobRunError::Brief(BriefError::NotFound))
                | OperationError::Jobs(JobRunError::Brief(BriefError::TargetNotGranted)) => (
                    404,
                    "target_unavailable",
                    "Target record is not available".to_string(),
                    vec![],
                ),
                OperationError::Jobs(JobRunError::Brief(BriefError::StaleRevision)) => (
                    409,
                    "version_mismatch",
                    "Expected revision is stale; reload and create a new intent".to_string(),
                    vec![],
                ),
                OperationError::Jobs(JobRunError::Brief(BriefError::InvalidPurpose)) => (
                    422,
                    "invalid_args",
                    "Purpose must be 1–2000 characters".to_string(),
                    vec![json!({"path": "/purpose", "problem": "invalid_purpose"})],
                ),
                OperationError::Jobs(JobRunError::Brief(BriefError::QueueAtCapacity)) => (
                    429,
                    "budget_exceeded",
                    "The job queue is at capacity".to_string(),
                    vec![],
                ),
                OperationError::Jobs(JobRunError::Brief(BriefError::Database)) => {
                    return Err(crate::application::writer::Failure::Unavailable);
                }
                OperationError::Resource(ResourceRunError::InvalidArgs) => (
                    422,
                    "invalid_args",
                    "Arguments do not match the command rules".to_string(),
                    vec![json!({"path": "", "problem": "invalid_args"})],
                ),
                OperationError::Resource(ResourceRunError::Draft(
                    crate::application::resources::ResourceDraftError::InvalidTitle,
                )) => (
                    422,
                    "invalid_args",
                    "Resource title must contain 1–240 characters".to_string(),
                    vec![json!({"path": "/title", "problem": "invalid_title"})],
                ),
                OperationError::Resource(ResourceRunError::Draft(
                    crate::application::resources::ResourceDraftError::InvalidSubtype,
                )) => (
                    422,
                    "invalid_args",
                    "Subtype must be one of the reviewed resource subtypes".to_string(),
                    vec![json!({"path": "/subtype", "problem": "unknown_subtype"})],
                ),
                OperationError::Resource(ResourceRunError::Ready(
                    crate::application::resources::ResourceReadyError::NotFound,
                )) => (
                    404,
                    "target_unavailable",
                    "Target record is not available".to_string(),
                    vec![],
                ),
                OperationError::Resource(ResourceRunError::Ready(
                    crate::application::resources::ResourceReadyError::StaleRevision,
                )) => (
                    409,
                    "version_mismatch",
                    "Expected content revision is stale; reload and create a new intent"
                        .to_string(),
                    vec![],
                ),
                OperationError::Resource(ResourceRunError::Ready(
                    crate::application::resources::ResourceReadyError::NotDraft,
                )) => (
                    422,
                    "invalid_args",
                    "Only a draft resource can transition to ready".to_string(),
                    vec![json!({"path": "", "problem": "not_draft"})],
                ),
                OperationError::Resource(ResourceRunError::Ready(
                    crate::application::resources::ResourceReadyError::MissingFields(fields),
                )) => {
                    let details: Vec<Value> = fields
                        .iter()
                        .map(|field| json!({"path": format!("/{field}"), "problem": "missing_field"}))
                        .collect();
                    (
                        422,
                        "invalid_args",
                        format!("resource is missing required fields: {}", fields.join(", ")),
                        details,
                    )
                }
                OperationError::Resource(ResourceRunError::Ready(
                    crate::application::resources::ResourceReadyError::UnsupportedSubtype,
                )) => (
                    422,
                    "invalid_args",
                    "Subtype is not a material resource subtype".to_string(),
                    vec![json!({"path": "/subtype", "problem": "unknown_subtype"})],
                ),
                OperationError::Resource(ResourceRunError::Retire(retirement_error)) => {
                    match map_retire_error(retirement_error) {
                        Ok(mapped) => mapped,
                        Err(_) => return Err(crate::application::writer::Failure::Unavailable),
                    }
                }
                OperationError::Writing(WritingRunError::InvalidArgs) => (
                    422,
                    "invalid_args",
                    "Arguments do not match the command rules".to_string(),
                    vec![json!({"path": "", "problem": "invalid_args"})],
                ),
                OperationError::Writing(WritingRunError::Match(
                    crate::application::match_start::MatchStartError::NotFound,
                ))
                | OperationError::Writing(WritingRunError::Match(
                    crate::application::match_start::MatchStartError::TargetNotGranted,
                )) => (
                    404,
                    "target_unavailable",
                    "Target record is not available".to_string(),
                    vec![],
                ),
                OperationError::Writing(WritingRunError::Match(
                    crate::application::match_start::MatchStartError::StaleRevision,
                ))
                | OperationError::Writing(WritingRunError::Compose(
                    crate::application::compose_start::ComposeStartError::StaleRevision,
                )) => (
                    409,
                    "version_mismatch",
                    "Expected revision is stale; reload and create a new intent".to_string(),
                    vec![],
                ),
                OperationError::Writing(WritingRunError::Match(
                    crate::application::match_start::MatchStartError::RecipientInvalid,
                ))
                | OperationError::Writing(WritingRunError::Match(
                    crate::application::match_start::MatchStartError::NeedMissing,
                ))
                | OperationError::Writing(WritingRunError::Compose(
                    crate::application::compose_start::ComposeStartError::RecipientInvalid,
                ))
                | OperationError::Writing(WritingRunError::Compose(
                    crate::application::compose_start::ComposeStartError::TemplateInvalid,
                ))
                | OperationError::Writing(WritingRunError::Compose(
                    crate::application::compose_start::ComposeStartError::ExchangeMissing,
                ))
                | OperationError::Writing(WritingRunError::Compose(
                    crate::application::compose_start::ComposeStartError::ExchangeNotReviewed,
                ))
                | OperationError::Writing(WritingRunError::Compose(
                    crate::application::compose_start::ComposeStartError::SlotInvalid,
                ))
                | OperationError::Writing(WritingRunError::Compose(
                    crate::application::compose_start::ComposeStartError::ResourceInvalid,
                ))
                | OperationError::Writing(WritingRunError::Compose(
                    crate::application::compose_start::ComposeStartError::MatchSelectionInvalid,
                )) => (
                    422,
                    "invalid_args",
                    "Arguments do not match the composition rules".to_string(),
                    vec![],
                ),
                OperationError::Writing(WritingRunError::Compose(
                    crate::application::compose_start::ComposeStartError::MatchShortlistMissing,
                )) => (
                    409,
                    "conflict",
                    "Run matching for this recipient before selecting matched resources"
                        .to_string(),
                    vec![],
                ),
                OperationError::Writing(WritingRunError::Compose(
                    crate::application::compose_start::ComposeStartError::TargetNotGranted,
                )) => (
                    404,
                    "target_unavailable",
                    "Target record is not available".to_string(),
                    vec![],
                ),
                OperationError::Writing(WritingRunError::Match(
                    crate::application::match_start::MatchStartError::QueueAtCapacity,
                ))
                | OperationError::Writing(WritingRunError::Compose(
                    crate::application::compose_start::ComposeStartError::QueueAtCapacity,
                )) => (
                    429,
                    "budget_exceeded",
                    "The job queue is at capacity".to_string(),
                    vec![],
                ),
                OperationError::Writing(WritingRunError::Match(
                    crate::application::match_start::MatchStartError::ScopeTooLarge,
                )) => (
                    422,
                    "context_budget",
                    "Matching is bounded to 4,096 eligible resources; narrow the scope".to_string(),
                    vec![],
                ),
                OperationError::Writing(WritingRunError::Match(
                    crate::application::match_start::MatchStartError::Database,
                ))
                | OperationError::Writing(WritingRunError::Compose(
                    crate::application::compose_start::ComposeStartError::Database,
                )) => {
                    return Err(crate::application::writer::Failure::Unavailable);
                }
                OperationError::Create(CreateRunError::Create(CreateError::Database)) => {
                    return Err(crate::application::writer::Failure::Unavailable);
                }
            };
            Err(mapped)
        }
    };

    let (inner, body_bytes) = match checked {
        Ok((value, bytes, effected_flag)) => {
            sqlx::query("RELEASE SAVEPOINT ucr_operation")
                .execute(&mut **tx)
                .await
                .map_err(|_| crate::application::writer::Failure::Unavailable)?;
            // A true no-op consumes zero executed-write allowance (FR-2).
            if effected_flag && effect != Effect::Read && mode == Mode::Execute {
                debit_effect(tx, actor, effect).await?;
            }
            (value, bytes)
        }
        Err((status, code, message, details)) => {
            sqlx::query("ROLLBACK TO SAVEPOINT ucr_operation")
                .execute(&mut **tx)
                .await
                .map_err(|_| crate::application::writer::Failure::Unavailable)?;
            sqlx::query("RELEASE SAVEPOINT ucr_operation")
                .execute(&mut **tx)
                .await
                .map_err(|_| crate::application::writer::Failure::Unavailable)?;
            let error_body = json!({
                "code": code,
                "message": message,
                "details": details
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
            body: &inner,
            bytes: body_bytes,
        }),
    )
    .await
    .map_err(|_| crate::application::writer::Failure::Unavailable)?;

    Ok(RunReply {
        status: 200,
        ok: true,
        inner,
        replayed: false,
        original_audit_id: None,

        approval_id: None,
    })
}
