use super::*;

/// Same-origin fetch-based SSE notification stream (U08 §4.1). Owner auth
/// headers are required; the stream emits `approvals_changed`,
/// `run_changed`, `domain_changed`, `job_changed` and `epoch_changed`
/// signals plus a 15 s heartbeat, and closes on service shutdown.
/// Connections are bounded.
pub(super) async fn notifications(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let (_principal, _registry, _writer) = match auth_owner(&headers, &state).await {
        Ok(value) => value,
        Err(response) => return response,
    };
    let permit = match state.sse_connections.clone().try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => {
            return error403(
                &state,
                "unavailable",
                "Too many notification connections",
                503,
            );
        }
    };
    #[derive(Clone)]
    enum SsePayload {
        Approvals,
        Run,
        Domain(i64),
        Job(String),
        Epoch,
        Heartbeat,
    }
    let (tx, rx) = mpsc::channel::<SsePayload>(64);
    let mut notify_rx = state.notify.subscribe();
    let mut restart_rx = state.restart.subscribe();
    let epoch = state.workspace.epoch().to_owned();
    let _ = tx.send(SsePayload::Epoch).await;
    tokio::spawn(async move {
        let _permit = permit;
        let mut heartbeat = tokio::time::interval(std::time::Duration::from_secs(15));
        loop {
            tokio::select! {
                signal = notify_rx.recv() => {
                    match signal {
                        Ok(CommandSignal::ApprovalsChanged) => {
                            if tx.send(SsePayload::Approvals).await.is_err() { break; }
                        }
                        Ok(CommandSignal::RunChanged) => {
                            if tx.send(SsePayload::Run).await.is_err() { break; }
                        }
                        Ok(CommandSignal::DomainChanged(cursor)) => {
                            if tx.send(SsePayload::Domain(cursor)).await.is_err() { break; }
                        }
                        Ok(CommandSignal::JobChanged(job_id)) => {
                            if tx.send(SsePayload::Job(job_id)).await.is_err() { break; }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
                _ = heartbeat.tick() => {
                    if tx.send(SsePayload::Heartbeat).await.is_err() { break; }
                }
                _ = restart_rx.changed() => break,
            }
        }
    });
    let stream = ReceiverStream::new(rx).map(move |payload| {
        let event = match payload {
            SsePayload::Approvals => Event::default().event("approvals_changed").data("{}"),
            SsePayload::Run => Event::default().event("run_changed").data("{}"),
            SsePayload::Domain(cursor) => Event::default()
                .event("domain_changed")
                .data(json!({"cursor": cursor}).to_string()),
            SsePayload::Job(job_id) => Event::default()
                .event("job_changed")
                .data(json!({"job_id": job_id}).to_string()),
            SsePayload::Epoch => Event::default().event("epoch_changed").data(epoch.clone()),
            SsePayload::Heartbeat => Event::default().comment("heartbeat"),
        };
        Ok::<Event, Infallible>(event)
    });
    let mut response = Sse::new(stream).into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("private, no-store"),
    );
    response.headers_mut().insert(
        header::CONNECTION,
        axum::http::HeaderValue::from_static("keep-alive"),
    );
    response.headers_mut().insert(
        "x-accel-buffering",
        axum::http::HeaderValue::from_static("no"),
    );
    response
}
