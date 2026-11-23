//! Parley local workspace launcher.

use std::{
    fs::{File, OpenOptions},
    io,
    path::{Path, PathBuf},
};

#[cfg(unix)]
use std::os::fd::AsRawFd;

#[cfg(unix)]
unsafe extern "C" {
    fn flock(fd: i32, operation: i32) -> i32;
}

/// How the launcher failed; the process exit code travels with the message.
#[derive(Debug)]
struct CliFailure {
    code: i32,
    message: String,
}

fn usage_failure(message: String) -> CliFailure {
    CliFailure { code: 2, message }
}

/// The launcher decision `prepare` reaches before any long-running work.
#[derive(Debug)]
enum Prepared {
    Version,
    Help,
    Eval {
        report: serde_json::Value,
    },
    /// Another process owns the workspace; hand back its bootstrap URL.
    RelaunchActive {
        url: String,
        headless: bool,
    },
    Launch {
        workspace: PathBuf,
        headless: bool,
        lock: Option<File>,
    },
}

const HELP: &str = "Parley local workspace\n\nUSAGE:\n  parley [--workspace PATH] [--headless]\n  parley eval --role extractor\n\nThe default workspace is $XDG_DATA_HOME/parley/workspace.sqlite. Headless mode prints the one-time local bootstrap URL. Evaluations emit one machine-readable JSON document to stdout.";

fn workspace_path(args: &[String]) -> Result<PathBuf, String> {
    if let Some(index) = args.iter().position(|arg| arg == "--workspace") {
        let value = args.get(index + 1).ok_or("--workspace requires a path")?;
        return Ok(PathBuf::from(value));
    }
    if let Some(value) = std::env::var_os("PARLEY_WORKSPACE") {
        return Ok(PathBuf::from(value));
    }
    let data = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
        .ok_or("cannot determine the owner data directory; set PARLEY_WORKSPACE")?;
    Ok(data.join("parley/workspace.sqlite"))
}

fn acquire_workspace_lock(workspace: &Path) -> Result<Option<File>, String> {
    let control =
        parley_service::Workspace::control_directory(workspace).map_err(|e| e.to_string())?;
    let lock_path = control.join("workspace.lock");
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .map_err(|error| format!("open workspace lock: {error}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&lock_path, std::fs::Permissions::from_mode(0o600))
            .map_err(|error| format!("secure workspace lock: {error}"))?;
        // The descriptor is held for the entire service lifetime. The kernel
        // releases it on process exit, so a stale path never blocks recovery.
        let result = unsafe { flock(lock.as_raw_fd(), 2 | 4) };
        if result != 0 {
            return Ok(None);
        }
    }
    #[cfg(not(unix))]
    {
        return Err("this launcher currently requires an OS lock implementation".to_owned());
    }
    Ok(Some(lock))
}

#[cfg(unix)]
async fn active_workspace_url(parent: &Path) -> Result<String, String> {
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::UnixStream,
    };
    let mut stream = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        UnixStream::connect(parent.join("launcher.sock")),
    )
    .await
    .map_err(|_| "workspace is active but its control socket is unreachable".to_owned())?
    .map_err(|_| "workspace is active but its control channel is unavailable".to_owned())?;
    stream
        .write_all(b"PARLEY-LAUNCH-V1\n")
        .await
        .map_err(|error| error.to_string())?;
    let reply = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let mut reply = String::new();
        stream.read_to_string(&mut reply).await?;
        Ok::<String, io::Error>(reply)
    })
    .await
    .map_err(|_| "active workspace did not answer the launch handshake".to_owned())?
    .map_err(|error| error.to_string())?;
    let mut lines = reply.lines();
    let address = lines
        .next()
        .ok_or("active workspace returned an invalid response")?;
    let secret = lines
        .next()
        .ok_or("active workspace returned an invalid response")?;
    Ok(format!("http://{address}/#bootstrap={secret}"))
}

#[cfg(unix)]
async fn serve_launcher_control(
    path: PathBuf,
    issuer: parley_service::BootstrapIssuer,
) -> Result<tokio::task::JoinHandle<()>, String> {
    use std::os::unix::fs::PermissionsExt;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::UnixListener,
    };
    if path.exists() {
        std::fs::remove_file(&path)
            .map_err(|error| format!("remove stale launcher socket: {error}"))?;
    }
    let listener =
        UnixListener::bind(&path).map_err(|error| format!("bind launcher socket: {error}"))?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
        .map_err(|error| format!("secure launcher socket: {error}"))?;
    Ok(tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            let mut request = Vec::new();
            let mut byte = [0u8; 1];
            while request.len() < 64 {
                match stream.read(&mut byte).await {
                    Ok(0) => break,
                    Ok(_) if byte[0] == b'\n' => break,
                    Ok(_) => request.push(byte[0]),
                    Err(_) => break,
                }
            }
            if request.as_slice() != b"PARLEY-LAUNCH-V1" {
                continue;
            }
            let secret = issuer.issue().await;
            let reply = format!("{}\n{}\n", issuer.address, secret);
            let _ = stream.write_all(reply.as_bytes()).await;
        }
    }))
}

fn open_browser(url: &str) -> io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(url)
            .spawn()
            .map(|_| ())
    }
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("cmd")
            .args(["/C", "start", "", url])
            .spawn()
            .map(|_| ())
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        std::process::Command::new("xdg-open")
            .arg(url)
            .spawn()
            .map(|_| ())
    }
}

/// Resolve the arguments into a launch decision without starting anything.
#[cfg(unix)]
async fn prepare(args: &[String]) -> Result<Prepared, CliFailure> {
    if args.iter().any(|arg| arg == "--version" || arg == "-V") {
        return Ok(Prepared::Version);
    }
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        return Ok(Prepared::Help);
    }
    if args.first().is_some_and(|arg| arg == "eval") {
        if args.get(1).is_none_or(|arg| arg != "--role")
            || args.get(2).is_none_or(|arg| arg != "extractor")
            || args.len() != 3
        {
            return Err(usage_failure(
                "parley eval: expected `--role extractor`".into(),
            ));
        }
        return evaluate_extractor()
            .map(|report| Prepared::Eval { report })
            .map_err(|error| CliFailure {
                code: 1,
                message: format!("parley eval: {error}"),
            });
    }
    let headless = args.iter().any(|arg| arg == "--headless");
    let workspace = workspace_path(args).map_err(usage_failure)?;
    match acquire_workspace_lock(&workspace).map_err(usage_failure)? {
        Some(lock) => Ok(Prepared::Launch {
            workspace,
            headless,
            lock: Some(lock),
        }),
        None => {
            let control = parley_service::Workspace::control_directory(&workspace)
                .map_err(|error| usage_failure(error.to_string()))?;
            let url = active_workspace_url(&control)
                .await
                .map_err(usage_failure)?;
            Ok(Prepared::RelaunchActive { url, headless })
        }
    }
}

/// Start the resolved workspace and serve it until shutdown or restart.
#[cfg(unix)]
async fn launch(workspace: PathBuf, headless: bool, lock: Option<File>) -> i32 {
    let lock = lock.expect("launch requires the held workspace lock");
    let mut running = match parley_service::start_workspace(&workspace).await {
        Ok(service) => service,
        Err(error) => {
            eprintln!("parley: workspace startup failed: {error}");
            return 2;
        }
    };
    let control_path = parley_service::Workspace::control_directory(&workspace)
        .expect("workspace control root")
        .join("launcher.sock");
    let control =
        match serve_launcher_control(control_path.clone(), running.bootstrap_issuer()).await {
            Ok(task) => task,
            Err(error) => {
                eprintln!("parley: launcher control failed: {error}");
                return 2;
            }
        };
    let url = format!(
        "http://{}/#bootstrap={}",
        running.address, running.bootstrap_secret
    );
    println!("Parley workspace ready at http://{}", running.address);
    if headless {
        println!("Bootstrap URL: {url}");
    } else if open_browser(&url).is_err() {
        eprintln!("Open this local URL to continue: {url}");
    }
    let restart_requested = tokio::select! {signal=tokio::signal::ctrl_c()=>{if let Err(error)=signal{eprintln!("parley: shutdown signal: {error}");}false},_ = running.wait_for_restart()=>true};
    running.shutdown().await;
    control.abort();
    let _ = std::fs::remove_file(control_path);
    drop(lock);
    if restart_requested {
        let executable = std::env::current_exe().expect("current executable path");
        let args: Vec<_> = std::env::args_os().skip(1).collect();
        if let Err(error) = std::process::Command::new(executable).args(args).spawn() {
            eprintln!("parley: restart required; relaunch failed: {error}");
        }
    }
    0
}

#[tokio::main]
async fn main() {
    #[cfg(unix)]
    {
        let args: Vec<String> = std::env::args().skip(1).collect();
        let code = match prepare(&args).await {
            Ok(Prepared::Version) => {
                println!("parley {}", env!("CARGO_PKG_VERSION"));
                0
            }
            Ok(Prepared::Help) => {
                println!("{HELP}");
                0
            }
            Ok(Prepared::Eval { report }) => {
                println!("{report}");
                0
            }
            Ok(Prepared::RelaunchActive { url, headless }) => {
                if headless {
                    println!("Bootstrap URL: {url}");
                } else if open_browser(&url).is_err() {
                    eprintln!("Open this local URL to continue: {url}");
                }
                println!("Opened the active Parley workspace");
                0
            }
            Ok(Prepared::Launch {
                workspace,
                headless,
                lock,
            }) => launch(workspace, headless, lock).await,
            Err(failure) => {
                eprintln!("parley: {}", failure.message);
                failure.code
            }
        };
        std::process::exit(code);
    }
    #[cfg(not(unix))]
    {
        eprintln!("parley: this launcher currently requires a unix host");
        std::process::exit(2);
    }
}

fn evaluate_extractor() -> Result<serde_json::Value, String> {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../eval/extractor-v1.json"))
            .map_err(|error| format!("bundled Extractor fixture is invalid: {error}"))?;
    evaluate_extractor_fixture(&fixture)
}

fn evaluate_extractor_fixture(fixture: &serde_json::Value) -> Result<serde_json::Value, String> {
    let cases = fixture["cases"]
        .as_array()
        .ok_or("Extractor fixture has no cases")?;
    let mut correct = 0usize;
    let mut valid_quote = 0usize;
    let mut results = Vec::with_capacity(cases.len());
    for case in cases {
        let id = case["id"]
            .as_str()
            .ok_or("Extractor fixture case has no id")?;
        let source = case["source"]
            .as_str()
            .ok_or("Extractor fixture case has no source")?;
        let quote = case["quote"]
            .as_str()
            .ok_or("Extractor fixture case has no quote")?;
        let expected = case["valid"]
            .as_bool()
            .ok_or("Extractor fixture case has no expected result")?;
        let actual = parley_domain::evidence::verify_quote(source, quote, None).is_ok();
        valid_quote += usize::from(actual);
        correct += usize::from(actual == expected);
        results.push(
            serde_json::json!({"case_id":id,"expected_valid":expected,"actual_valid":actual,"pass":actual==expected}),
        );
    }
    let total = cases.len();
    Ok(serde_json::json!({
        "role":"extractor",
        "dataset_id":fixture["dataset_id"],
        "evaluation_kind":"deterministic_quote_validation",
        "provider_invoked":false,
        "cases":total,
        "correct":correct,
        "quote_valid":valid_quote,
        "accuracy":if total == 0 { 0.0 } else { correct as f64 / total as f64 },
        "results":results
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn temp_dir(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "parley-cli-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ))
    }

    #[test]
    fn s04_extractor_eval_is_machine_readable() {
        let report = evaluate_extractor().unwrap();
        assert_eq!(report["role"], "extractor");
        assert_eq!(report["dataset_id"], "extractor-v1");
        assert_eq!(report["provider_invoked"], false);
        assert_eq!(report["cases"], 4);
        assert_eq!(report["correct"], 4);
        assert_eq!(report["results"].as_array().unwrap().len(), 4);
    }

    #[test]
    fn extractor_fixture_errors_are_named() {
        let err = evaluate_extractor_fixture(&serde_json::json!({})).unwrap_err();
        assert_eq!(err, "Extractor fixture has no cases");
        let err = evaluate_extractor_fixture(&serde_json::json!({"cases": [{}]})).unwrap_err();
        assert_eq!(err, "Extractor fixture case has no id");
        let err =
            evaluate_extractor_fixture(&serde_json::json!({"cases": [{"id": "c"}]})).unwrap_err();
        assert_eq!(err, "Extractor fixture case has no source");
        let err =
            evaluate_extractor_fixture(&serde_json::json!({"cases": [{"id": "c", "source": "s"}]}))
                .unwrap_err();
        assert_eq!(err, "Extractor fixture case has no quote");
        let err = evaluate_extractor_fixture(
            &serde_json::json!({"cases": [{"id": "c", "source": "s", "quote": "q"}]}),
        )
        .unwrap_err();
        assert_eq!(err, "Extractor fixture case has no expected result");
    }

    #[test]
    fn workspace_path_prefers_the_flag_then_the_environment() {
        let guard = ENV_LOCK.lock().unwrap();
        let previous = (
            std::env::var_os("PARLEY_WORKSPACE"),
            std::env::var_os("XDG_DATA_HOME"),
            std::env::var_os("HOME"),
        );
        unsafe {
            std::env::remove_var("PARLEY_WORKSPACE");
            std::env::remove_var("XDG_DATA_HOME");
            std::env::remove_var("HOME");
        }
        // The explicit flag wins over any environment value.
        let args = vec![
            "--workspace".to_owned(),
            "/tmp/flagged.sqlite".to_owned(),
            "--headless".to_owned(),
        ];
        assert_eq!(
            workspace_path(&args).unwrap(),
            PathBuf::from("/tmp/flagged.sqlite")
        );
        assert_eq!(
            workspace_path(&[]).unwrap_err(),
            "cannot determine the owner data directory; set PARLEY_WORKSPACE"
        );
        unsafe {
            std::env::set_var("HOME", "/home/owner");
        }
        assert_eq!(
            workspace_path(&[]).unwrap(),
            PathBuf::from("/home/owner/.local/share/parley/workspace.sqlite")
        );
        unsafe {
            std::env::set_var("XDG_DATA_HOME", "/custom/data");
        }
        assert_eq!(
            workspace_path(&[]).unwrap(),
            PathBuf::from("/custom/data/parley/workspace.sqlite")
        );
        unsafe {
            std::env::set_var("PARLEY_WORKSPACE", "/env/workspace.sqlite");
        }
        assert_eq!(
            workspace_path(&[]).unwrap(),
            PathBuf::from("/env/workspace.sqlite")
        );
        assert_eq!(
            workspace_path(&["--workspace".to_owned()]).unwrap_err(),
            "--workspace requires a path"
        );
        unsafe {
            match previous.0 {
                Some(value) => std::env::set_var("PARLEY_WORKSPACE", value),
                None => std::env::remove_var("PARLEY_WORKSPACE"),
            }
            match previous.1 {
                Some(value) => std::env::set_var("XDG_DATA_HOME", value),
                None => std::env::remove_var("XDG_DATA_HOME"),
            }
            match previous.2 {
                Some(value) => std::env::set_var("HOME", value),
                None => std::env::remove_var("HOME"),
            }
        }
        drop(guard);
    }

    #[test]
    fn workspace_lock_holds_exclusively_or_reports_the_active_holder() {
        let root = temp_dir("lock");
        std::fs::create_dir_all(&root).unwrap();
        let workspace = root.join("workspace.sqlite");
        let control = parley_service::Workspace::control_directory(&workspace).unwrap();
        let first = acquire_workspace_lock(&workspace).unwrap();
        assert!(first.is_some(), "the first holder owns the lock");
        let lock_path = control.join("workspace.lock");
        let mode = std::os::unix::fs::MetadataExt::mode(&std::fs::metadata(&lock_path).unwrap());
        assert_eq!(mode & 0o777, 0o600, "the lock file is owner-only");
        assert!(matches!(acquire_workspace_lock(&workspace), Ok(None)));
        drop(first);
    }

    #[test]
    fn workspace_lock_failure_is_named() {
        let blocked = PathBuf::from("/proc/parley-cannot-exist/workspace.sqlite");
        assert!(acquire_workspace_lock(&blocked).is_err());
    }

    #[tokio::test]
    async fn prepare_resolves_version_help_and_eval_usage() {
        assert!(matches!(
            prepare(&["--version".to_owned()]).await.unwrap(),
            Prepared::Version
        ));
        assert!(matches!(
            prepare(&["-h".to_owned()]).await.unwrap(),
            Prepared::Help
        ));
        let failure = prepare(&["eval".to_owned()]).await.unwrap_err();
        assert_eq!(failure.code, 2);
        assert!(failure.message.contains("--role extractor"));
        let failure = prepare(&["eval".to_owned(), "--role".to_owned(), "other".to_owned()])
            .await
            .unwrap_err();
        assert_eq!(failure.code, 2);
        let report = match prepare(&[
            "eval".to_owned(),
            "--role".to_owned(),
            "extractor".to_owned(),
        ])
        .await
        .unwrap()
        {
            Prepared::Eval { report } => report,
            other => panic!("expected an evaluation report, got {other:?}"),
        };
        assert_eq!(report["role"], "extractor");
    }

    #[tokio::test]
    async fn prepare_reports_an_active_workspace_through_the_control_socket() {
        let workspace = temp_dir("active").join("workspace.sqlite");
        std::fs::create_dir_all(workspace.parent().unwrap()).unwrap();
        // Simulate the other launcher process holding the workspace lock.
        let held = acquire_workspace_lock(&workspace)
            .unwrap()
            .expect("lock acquired");
        let service = parley_service::start_workspace(&workspace).await.unwrap();
        let control = parley_service::Workspace::control_directory(&workspace).unwrap();
        let control_path = control.join("launcher.sock");
        serve_launcher_control(control_path, service.bootstrap_issuer())
            .await
            .unwrap();

        // Address the workspace through the flag so no environment mutation
        // spans an await point.
        let prepared = prepare(&[
            "--workspace".to_owned(),
            workspace.display().to_string(),
            "--headless".to_owned(),
        ])
        .await
        .unwrap();
        let Prepared::RelaunchActive { url, headless } = prepared else {
            panic!("expected the active-workspace handoff");
        };
        assert!(headless);
        assert!(url.contains(service.address.to_string().as_str()), "{url}");
        assert!(url.contains("#bootstrap="));
        drop(held);
        service.shutdown().await;
        let _ = std::fs::remove_file(&workspace);
    }

    #[tokio::test]
    async fn active_workspace_url_names_handshake_failures() {
        // No control socket at all.
        let quiet = temp_dir("quiet");
        std::fs::create_dir_all(&quiet).unwrap();
        let error = active_workspace_url(&quiet).await.unwrap_err();
        assert!(error.contains("control channel is unavailable"), "{error}");

        // A control socket that resets the connection before the handshake
        // completes: the raw transport error is surfaced verbatim.
        let silent = temp_dir("silent");
        std::fs::create_dir_all(&silent).unwrap();
        let listener = tokio::net::UnixListener::bind(silent.join("launcher.sock")).unwrap();
        let drop_connection = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            drop(stream);
        });
        let error = active_workspace_url(&silent).await.unwrap_err();
        assert!(
            error.contains("reset") || error.contains("invalid response"),
            "{error}"
        );
        drop_connection.await.unwrap();

        // A control socket that closes cleanly without ever answering.
        let polite = temp_dir("polite");
        std::fs::create_dir_all(&polite).unwrap();
        let listener = tokio::net::UnixListener::bind(polite.join("launcher.sock")).unwrap();
        let close_after_write = tokio::spawn(async move {
            use tokio::io::AsyncReadExt;
            let (mut stream, _) = listener.accept().await.unwrap();
            // Consume the request line so the close is a clean EOF, not a reset.
            let mut consumed = String::new();
            loop {
                let mut byte = [0u8; 1];
                match stream.read(&mut byte).await {
                    Ok(1) if byte[0] == b'\n' => break,
                    Ok(_) => consumed.push(byte[0] as char),
                    _ => break,
                }
            }
            drop(stream);
        });
        let error = active_workspace_url(&polite).await.unwrap_err();
        assert!(error.contains("invalid response"), "{error}");
        close_after_write.await.unwrap();
    }
}
