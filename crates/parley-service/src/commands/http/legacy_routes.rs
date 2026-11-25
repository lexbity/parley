//! TD-10 W4: the consolidated legacy transport surface.
//!
//! Every non-exempt `/api/v1` route registers here, and only here,
//! dispatching to the owning module's implementation. `ci/routes.py`
//! enforces this placement plus the inventory/openapi mapping so the
//! legacy transport cannot reappear outside `commands/http`.

use super::*;
use crate::{
    agent_runtime, assistant, blob, board, http, inference::activity, jobs, maintenance, materials,
    packs, parties, queue_sheet, queueing, relations, release, resource_board, thread, writing,
};

pub(super) fn legacy_routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/health", get(http::health))
        .route("/api/v1/session/bootstrap", post(http::bootstrap))
        .route("/api/v1/session", get(http::session))
        .route(
            "/api/v1/entities",
            get(http::list_entities).post(http::create_entity),
        )
        .route(
            "/api/v1/entities/{id}",
            get(http::get_entity).patch(http::update_entity),
        )
        .route(
            "/api/v1/entities/{id}/notes",
            get(http::get_entity_notes).put(http::update_entity_notes),
        )
        .route(
            "/api/v1/entities/{id}/dossier",
            get(http::get_entity_dossier),
        )
        .route("/api/v1/events", get(http::get_events))
        .route(
            "/api/v1/captures",
            get(http::list_captures).post(http::save_capture),
        )
        .route("/api/v1/graph", get(http::get_graph))
        .route("/api/v1/graph/layout", put(http::update_graph_layout))
        .route("/api/v1/graph/related", post(relations::related))
        .route(
            "/api/v1/claims/{id}/revisions",
            post(relations::revise_claim),
        )
        .route("/api/v1/edges", post(relations::link))
        .route(
            "/api/v1/edges/{id}",
            patch(relations::correct).delete(relations::remove),
        )
        .route("/api/v1/board", get(board::get_board))
        .route("/api/v1/board/lists", post(board::create_list))
        .route("/api/v1/board/lists/reorder", post(board::reorder_lists))
        .route("/api/v1/board/lists/{list_id}", patch(board::rename_list))
        .route(
            "/api/v1/board/lists/{list_id}/archive",
            post(board::archive_list),
        )
        .route(
            "/api/v1/opportunities/{id}/placement",
            post(board::move_card),
        )
        .route(
            "/api/v1/opportunities/{id}/notes",
            get(board::get_notes).patch(board::update_notes),
        )
        .route(
            "/api/v1/opportunities/{id}/notes/tasks/toggle",
            post(board::toggle_task),
        )
        .route(
            "/api/v1/opportunities",
            get(materials::list_opportunities).post(materials::create_opportunity),
        )
        .route(
            "/api/v1/opportunities/{id}/tags",
            put(materials::set_opportunity_tags),
        )
        .route(
            "/api/v1/opportunities/{id}",
            get(materials::get_opportunity),
        )
        .route(
            "/api/v1/opportunities/{id}/participants",
            post(materials::add_participant),
        )
        .route(
            "/api/v1/opportunities/{id}/sources/{source_id}",
            post(materials::link_source).delete(materials::unlink_source),
        )
        .route(
            "/api/v1/opportunities/{id}/interests",
            get(materials::get_interests).post(materials::add_interest),
        )
        .route(
            "/api/v1/opportunities/{id}/exchange",
            get(materials::get_exchange).post(materials::save_exchange),
        )
        .route(
            "/api/v1/opportunities/{id}/exchange/{recipient_id}/attest",
            post(materials::attest_exchange),
        )
        .route(
            "/api/v1/templates",
            post(materials::create_template).get(materials::list_templates),
        )
        .route(
            "/api/v1/templates/{id}/approve",
            post(materials::approve_template),
        )
        .route(
            "/api/v1/templates/{id}/activate",
            post(materials::activate_template),
        )
        .route(
            "/api/v1/opportunities/{id}/materials",
            post(materials::create_material).get(materials::list_materials),
        )
        .route("/api/v1/materials/{id}", get(materials::get_material))
        .route(
            "/api/v1/materials/{id}/gate",
            post(materials::evaluate_material_gate),
        )
        .route(
            "/api/v1/materials/{id}/versions",
            post(materials::edit_material),
        )
        .route(
            "/api/v1/materials/{id}/approve",
            post(materials::approve_material),
        )
        .route(
            "/api/v1/materials/{id}/release",
            post(materials::release_material),
        )
        .route("/api/v1/releases/{id}/ack", post(materials::ack_release))
        .route("/api/v1/outputs", get(materials::list_outputs))
        .route(
            "/api/v1/resources/{id}/ready",
            post(materials::mark_resource_ready),
        )
        .route(
            "/api/v1/materials/{id}/download",
            get(release::download_material).head(release::head_not_allowed),
        )
        .route(
            "/api/v1/uploads",
            post(blob::upload)
                .get(blob::list_uploads)
                .layer(axum::extract::DefaultBodyLimit::max(
                    crate::blob::MAX_BLOB_SIZE as usize,
                )),
        )
        .route("/api/v1/uploads/{id}/review", post(blob::review_upload))
        .route("/api/v1/directory", get(parties::directory))
        .route("/api/v1/entities/{id}/usage", get(parties::usage))
        .route("/api/v1/entities/{id}/retire", post(parties::retire))
        .route("/api/v1/party-imports", post(parties::stage))
        .route("/api/v1/party-imports/{id}/preview", post(parties::preview))
        .route("/api/v1/party-imports/{id}/commit", post(parties::commit))
        .route("/api/v1/entities/{id}/attachments", post(parties::attach))
        .route(
            "/api/v1/entities/{id}/attachments/{reference}",
            delete(parties::unlink),
        )
        .route(
            "/api/v1/entities/{id}/attachments/{reference}/content",
            get(parties::content),
        )
        .route("/api/v1/threads", get(thread::list_threads))
        .route("/api/v1/threads/{id}/send", post(thread::mark_sent))
        .route("/api/v1/threads/{id}/reopen", post(thread::reopen_thread))
        .route("/api/v1/sends/{id}/outcome", post(thread::record_outcome))
        .route(
            "/api/v1/entities/{id}/clear-dnc",
            post(thread::clear_party_dnc),
        )
        .route("/api/v1/sends", get(thread::list_sends))
        .route(
            "/api/v1/sends/{id}/outcome/correction",
            post(thread::correct_outcome),
        )
        .route(
            "/api/v1/opportunities/{id}/resolution",
            post(thread::record_resolution),
        )
        .route(
            "/api/v1/capacity",
            get(thread::list_capacity).post(thread::set_capacity),
        )
        .route(
            "/api/v1/agent/provider",
            get(jobs::get_provider).put(jobs::update_provider),
        )
        .route("/api/v1/agent/provider/health", post(jobs::provider_health))
        .route(
            "/api/v1/jobs/{id}",
            get(jobs::get_job).post(jobs::cancel_job),
        )
        .route(
            "/api/v1/captures/{occurrence_id}/proposals",
            get(jobs::get_capture_proposals),
        )
        .route(
            "/api/v1/proposals/{batch_id}/resolve",
            post(jobs::resolve_proposals),
        )
        .route("/api/v1/agent/activity", get(activity::list))
        .route(
            "/api/v1/agent/chats",
            get(assistant::list_chats).post(assistant::create_chat),
        )
        .route("/api/v1/agent/chats/{id}", get(assistant::get_chat))
        .route("/api/v1/agent/chats/{id}/turns", post(assistant::add_turn))
        .route(
            "/api/v1/agent/chats/{id}/questions",
            post(assistant::question_action),
        )
        .route(
            "/api/v1/agent/candidates/{id}/confirm",
            post(assistant::confirm_candidate),
        )
        .route(
            "/api/v1/agent/candidates/{id}/dismiss",
            post(assistant::dismiss_candidate),
        )
        .route(
            "/api/v1/agent/question-bank",
            get(assistant::get_question_bank).put(assistant::update_question_bank),
        )
        .route(
            "/api/v1/agent/prompts/{role}",
            get(assistant::get_prompt).put(assistant::update_prompt),
        )
        .route(
            "/api/v1/segments",
            get(assistant::list_segments).post(assistant::create_segment),
        )
        .route(
            "/api/v1/segments/{id}/parties",
            post(assistant::update_segment_party),
        )
        .route(
            "/api/v1/briefs/{scope_kind}/{scope_id}/items",
            get(assistant::list_brief_items).post(assistant::generate_brief),
        )
        .route(
            "/api/v1/brief-items/{id}/decision",
            post(assistant::decide_brief_item),
        )
        .route("/api/v1/fluency", get(assistant::list_fluency))
        .route("/api/v1/fluency/{term}", put(assistant::update_fluency))
        .route(
            "/api/v1/opportunities/{id}/matches",
            post(writing::start_match).get(writing::get_match),
        )
        .route("/api/v1/templates/build", post(writing::build_templates))
        .route(
            "/api/v1/templates/build-candidates/{job_id}",
            get(writing::build_candidates),
        )
        .route(
            "/api/v1/template-families",
            get(writing::list_template_families).post(writing::create_template_family),
        )
        .route(
            "/api/v1/matching/calibrations",
            get(writing::get_calibrations).post(writing::reject_client_calibration),
        )
        .route(
            "/api/v1/embedding-model",
            get(writing::get_embedding_model).put(writing::set_embedding_model),
        )
        .route(
            "/api/v1/voice-samples",
            get(writing::list_voice_samples).post(writing::create_voice_sample),
        )
        .route(
            "/api/v1/voice-samples/{id}/retire",
            post(writing::retire_voice_sample),
        )
        .route(
            "/api/v1/templates/{family_id}/versions",
            post(writing::create_template_version),
        )
        .route(
            "/api/v1/opportunities/{id}/materials/compose",
            post(writing::compose_material),
        )
        .route(
            "/api/v1/opportunities/{id}/materials/compose/{job_id}/save",
            post(writing::save_composer_candidate),
        )
        .route("/api/v1/materials/{id}/lint", post(writing::lint_material))
        .route("/api/v1/queue", get(queueing::queue))
        .route("/api/v1/insights", get(queueing::insights))
        .route("/api/v1/coach/questions", get(queueing::questions))
        .route(
            "/api/v1/coach/context",
            post(queueing::generate_context_coach),
        )
        .route(
            "/api/v1/coach/questions/{id}/answer",
            post(queueing::answer_question),
        )
        .route(
            "/api/v1/coach/questions/{id}/dismiss",
            post(queueing::dismiss_question),
        )
        .route(
            "/api/v1/coach/questions/{id}/defer",
            post(queueing::defer_question),
        )
        .route(
            "/api/v1/coach/weekly",
            get(queueing::get_coach_report).post(queueing::generate_coach_report),
        )
        .route("/api/v1/identity-tags", post(queueing::save_identity_tag))
        .route("/api/v1/claims/{id}/hook", post(queueing::save_hook))
        .route(
            "/api/v1/opportunities/{id}/phase",
            patch(queue_sheet::set_phase),
        )
        .route(
            "/api/v1/resource-board",
            get(resource_board::http::get_resource_board),
        )
        .route(
            "/api/v1/resource-board/columns",
            post(resource_board::http::create_resource_column),
        )
        .route(
            "/api/v1/resource-board/columns/{id}",
            delete(resource_board::http::remove_resource_column)
                .patch(resource_board::http::rename_resource_column),
        )
        .route(
            "/api/v1/resource-cards/{id}",
            get(resource_board::http::get_resource_card),
        )
        .route(
            "/api/v1/resource-cards/{id}/deliveries",
            get(resource_board::http::get_resource_card_deliveries),
        )
        .route(
            "/api/v1/resource-cards/{id}/retire",
            post(resource_board::http::retire_resource_card),
        )
        .route(
            "/api/v1/resource-cards/{id}/erase",
            post(resource_board::http::erase_resource_card),
        )
        .route(
            "/api/v1/resource-cards/{id}/deliveries/{send_id}/{material_id}/{version}",
            get(resource_board::http::get_delivered_resource_version),
        )
        .route(
            "/api/v1/resource-card-commits",
            post(resource_board::http::commit_resource_card),
        )
        .route(
            "/api/v1/resource-board/moves",
            post(resource_board::http::move_resource_card),
        )
        .route(
            "/api/v1/entities/{id}/attachments",
            get(resource_board::http::list_attachments),
        )
        .route("/api/v1/settings", get(packs::get_pack_settings))
        .route("/api/v1/packs/validate", post(packs::validate_pack))
        .route("/api/v1/packs/activate", post(packs::activate_pack))
        .route(
            "/api/v1/settings/maintenance",
            get(maintenance::get_settings).put(maintenance::update_settings),
        )
        .route(
            "/api/v1/backups",
            get(maintenance::list_backups).post(maintenance::create_backup),
        )
        .route("/api/v1/exports", post(maintenance::create_export))
        .route("/api/v1/imports/preview", post(maintenance::preview_import))
        .route("/api/v1/imports/commit", post(maintenance::commit_import))
        .route(
            "/api/v1/restore/preview",
            post(maintenance::preview_restore),
        )
        .route("/api/v1/restore/commit", post(maintenance::commit_restore))
        .route(
            "/api/v1/parties/{id}/erase/preview",
            post(maintenance::preview_erasure),
        )
        .route(
            "/api/v1/parties/{id}/erase",
            post(maintenance::commit_erasure),
        )
        .route(
            "/api/v1/maintenance/operations/{id}",
            get(maintenance::get_operation),
        )
        .route(
            "/api/v1/agent/runs",
            post(agent_runtime::http::start).get(agent_runtime::http::list),
        )
        .route("/api/v1/agent/runs/{id}", get(agent_runtime::http::status))
        .route(
            "/api/v1/agent/runs/{id}/cancel",
            post(agent_runtime::http::cancel),
        )
        .route(
            "/api/v1/agent/runs/{id}/transcript",
            get(agent_runtime::http::transcript),
        )
}
