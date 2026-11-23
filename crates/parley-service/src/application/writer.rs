//! Bounded writer executor (UCR-001 Q3, §5).
//!
//! Every adopted operation executes on one dedicated SQLite connection. A
//! job closure owns its transaction: it begins `BEGIN IMMEDIATE`, runs the
//! whole adapter-level pipeline (ledger checks, application operation,
//! receipt and audit writes) and commits through [`commit_tx`], which
//! enforces the monotonic deadline and applies test faults. Requests queue
//! through a bounded 64-entry FIFO reservation queue. The executor holds a
//! shared generation read-gate; a generation publisher holds the exclusive
//! gate and therefore drains queued adopted work before switching epochs.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use sqlx::{Sqlite, Transaction};
use tokio::sync::{RwLock, mpsc, oneshot};

/// Reservation queue depth (NFR-3: pending writer admissions).
pub(crate) const RESERVATION_DEPTH: usize = 64;
/// Admission wait cap (NFR-3: 2s).
pub(crate) const ADMISSION_WAIT: Duration = Duration::from_secs(2);
/// Short execution including planning cap (NFR-3: 30s).
pub(crate) const EXECUTION_DEADLINE: Duration = Duration::from_secs(30);

/// A job submitted to the writer executor.
pub(crate) struct WriterJob {
    /// Monotonic deadline for the whole operation, including its commit.
    pub deadline: Instant,
    /// Generation token observed at admission. If the workspace generation
    /// has advanced (a replacement was published), the job fails closed.
    pub generation: u64,
    #[allow(clippy::type_complexity)] // erased job closure: one writer slot
    pub run: Box<
        dyn for<'a> FnOnce(
                &'a mut sqlx::sqlite::SqliteConnection,
                Instant,
            ) -> Pin<Box<dyn Future<Output = JobResult> + Send + 'a>>
            + Send,
    >,
    pub reply: oneshot::Sender<WriteOutcome>,
}

/// What a job produces.
pub(crate) struct JobResult {
    /// `true` when the transaction committed and `body` is the response.
    pub commit: bool,
    /// The response body to return to the caller on commit.
    pub body: serde_json::Value,
    /// When `commit` is false, the failure variant for the transport.
    pub failure: Option<Failure>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Failure {
    /// Admission saturation or an uncertain commit: retryable, same key.
    Unavailable,
    /// The execution deadline expired before the operation completed.
    Timeout,
    /// The workspace generation advanced while the job was queued.
    GenerationChanged,
    /// The job waited too long for a writer slot.
    AdmissionTimeout,
}

pub(crate) enum WriteOutcome {
    Committed(serde_json::Value),
    Rejected(Failure),
}

#[derive(Clone)]
pub(crate) struct Writer {
    tx: mpsc::Sender<WriterJob>,
}

impl Writer {
    pub async fn run(&self, mut job: WriterJob) -> WriteOutcome {
        let (reply_tx, reply_rx) = oneshot::channel();
        job.reply = reply_tx;
        match tokio::time::timeout(ADMISSION_WAIT, self.tx.send(job)).await {
            Err(_) => WriteOutcome::Rejected(Failure::AdmissionTimeout),
            Ok(Err(_)) => WriteOutcome::Rejected(Failure::Unavailable),
            Ok(Ok(())) => match reply_rx.await {
                Ok(outcome) => outcome,
                Err(_) => WriteOutcome::Rejected(Failure::Unavailable),
            },
        }
    }
}

/// Spawn the writer task. Returns the writer handle plus the task.
pub(crate) fn start(
    mut connection: sqlx::sqlite::SqliteConnection,
    generation: Arc<RwLock<u64>>,
) -> (Writer, tokio::task::JoinHandle<()>) {
    let (tx, mut rx) = mpsc::channel::<WriterJob>(RESERVATION_DEPTH);
    let writer = Writer { tx };
    let task = tokio::spawn(async move {
        while let Some(job) = rx.recv().await {
            let (reply, outcome) = run_job(&mut connection, job, &generation).await;
            let _ = reply.send(outcome);
        }
    });
    (writer, task)
}

async fn run_job(
    connection: &mut sqlx::sqlite::SqliteConnection,
    job: WriterJob,
    generation: &Arc<RwLock<u64>>,
) -> (oneshot::Sender<WriteOutcome>, WriteOutcome) {
    if Instant::now() >= job.deadline {
        return (job.reply, WriteOutcome::Rejected(Failure::Timeout));
    }
    // Read-gate: hold the shared lock for the whole operation so a generation
    // publisher (which takes the exclusive lock) cannot publish a replacement
    // while an adopted write is in flight. A job admitted before the fence
    // closes carries an older token and fails closed after the drain.
    let generation_guard = generation.read().await;
    if *generation_guard != job.generation {
        return (
            job.reply,
            WriteOutcome::Rejected(Failure::GenerationChanged),
        );
    }
    let result = (job.run)(connection, job.deadline).await;
    let outcome = match result {
        JobResult {
            commit: true, body, ..
        } => WriteOutcome::Committed(body),
        JobResult {
            commit: false,
            failure: Some(failure),
            ..
        } => WriteOutcome::Rejected(failure),
        JobResult {
            commit: false,
            failure: None,
            ..
        } => WriteOutcome::Rejected(Failure::Unavailable),
    };
    (job.reply, outcome)
}

/// Commit a pipeline transaction under the deadline, with crash fault
/// injection. Returns `Err` when the transaction must be rolled back
/// (deadline exceeded, simulated pre-commit crash, or commit failure).
pub(crate) async fn commit_tx(
    tx: Transaction<'_, Sqlite>,
    deadline: Instant,
    fault: &str,
) -> Result<(), Failure> {
    if Instant::now() >= deadline {
        // Drop rolls the transaction back.
        return Err(Failure::Timeout);
    }
    if fault == "precommit_crash" {
        // Simulated process death before commit: the effect is lost.
        return Err(Failure::Unavailable);
    }
    tx.commit().await.map_err(|_| Failure::Unavailable)?;
    if fault == "postcommit_crash" {
        // Simulated process death after commit before the reply: the durable
        // effect exists and a same-key retry replays it.
        return Err(Failure::Unavailable);
    }
    Ok(())
}

/// Fault injection is bound to the request, never to process-global state,
/// so concurrent tests cannot affect each other. Release binaries ignore the
/// hook entirely: only debug/test builds derive a fault from the key.
pub(crate) fn fault_from_key(key: &str) -> &'static str {
    #[cfg(debug_assertions)]
    {
        use std::collections::HashMap;
        use std::sync::{Mutex, OnceLock};
        static REMAINING: OnceLock<Mutex<HashMap<&'static str, usize>>> = OnceLock::new();
        let kind = if key.starts_with("fault-precommit-") {
            "precommit_crash"
        } else if key.starts_with("fault-postcommit-") {
            "postcommit_crash"
        } else {
            return "";
        };
        let mut remaining = REMAINING
            .get_or_init(|| {
                Mutex::new(HashMap::from([
                    ("precommit_crash", 1),
                    ("postcommit_crash", 1),
                ]))
            })
            .lock()
            .expect("fault counter poisoned");
        if let Some(count) = remaining.get_mut(kind)
            && *count > 0
        {
            *count -= 1;
            return kind;
        }
    }
    #[cfg(not(debug_assertions))]
    {
        let _ = key;
    }
    ""
}
