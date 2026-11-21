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
    let parent = workspace
        .parent()
        .ok_or("workspace path needs a parent directory")?;
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("create workspace directory: {error}"))?;
    let lock_path = parent.join("workspace.lock");
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
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))
            .map_err(|error| format!("secure workspace directory: {error}"))?;
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
async fn open_existing_workspace(parent: &Path, headless: bool) -> Result<(), String> {
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::UnixStream,
    };
    let mut stream = UnixStream::connect(parent.join("launcher.sock"))
        .await
        .map_err(|_| "workspace is active but its control channel is unavailable".to_owned())?;
    stream
        .write_all(b"PARLEY-LAUNCH-V1\n")
        .await
        .map_err(|error| error.to_string())?;
    let mut reply = String::new();
    stream
        .read_to_string(&mut reply)
        .await
        .map_err(|error| error.to_string())?;
    let mut lines = reply.lines();
    let address = lines
        .next()
        .ok_or("active workspace returned an invalid response")?;
    let secret = lines
        .next()
        .ok_or("active workspace returned an invalid response")?;
    let url = format!("http://{address}/#bootstrap={secret}");
    if headless {
        println!("Bootstrap URL: {url}");
    } else {
        open_browser(&url).map_err(|_| format!("Open this local URL to continue: {url}"))?;
    }
    println!("Opened the active Parley workspace at http://{address}");
    Ok(())
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

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|arg| arg == "--version" || arg == "-V") {
        println!("parley {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        println!(
            "Parley local workspace\n\nUSAGE:\n  parley [--workspace PATH] [--headless]\n  parley eval --role extractor\n\nThe default workspace is $XDG_DATA_HOME/parley/workspace.sqlite. Headless mode prints the one-time local bootstrap URL. Evaluations emit one machine-readable JSON document to stdout."
        );
        return;
    }
    if args.first().is_some_and(|arg| arg == "eval") {
        if args.get(1).is_none_or(|arg| arg != "--role")
            || args.get(2).is_none_or(|arg| arg != "extractor")
            || args.len() != 3
        {
            eprintln!("parley eval: expected `--role extractor`");
            std::process::exit(2);
        }
        match evaluate_extractor() {
            Ok(report) => println!("{}", report),
            Err(error) => {
                eprintln!("parley eval: {error}");
                std::process::exit(1);
            }
        }
        return;
    }
    let headless = args.iter().any(|arg| arg == "--headless");
    let workspace = match workspace_path(&args) {
        Ok(path) => path,
        Err(error) => {
            eprintln!("parley: {error}");
            std::process::exit(2);
        }
    };
    let lock = match acquire_workspace_lock(&workspace) {
        Ok(Some(lock)) => lock,
        Ok(None) => {
            #[cfg(unix)]
            if let Some(parent) = workspace.parent() {
                if let Err(error) = open_existing_workspace(parent, headless).await {
                    eprintln!("parley: {error}");
                    std::process::exit(2);
                }
                return;
            }
            eprintln!("parley: workspace is already active");
            std::process::exit(2);
        }
        Err(error) => {
            eprintln!("parley: {error}");
            std::process::exit(2);
        }
    };
    let running = match parley_service::start_workspace(&workspace).await {
        Ok(service) => service,
        Err(error) => {
            eprintln!("parley: workspace startup failed: {error}");
            std::process::exit(2);
        }
    };
    #[cfg(unix)]
    let control_path = workspace.parent().unwrap().join("launcher.sock");
    #[cfg(unix)]
    let control =
        match serve_launcher_control(control_path.clone(), running.bootstrap_issuer()).await {
            Ok(task) => task,
            Err(error) => {
                eprintln!("parley: launcher control failed: {error}");
                std::process::exit(2);
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
    if let Err(error) = tokio::signal::ctrl_c().await {
        eprintln!("parley: shutdown signal: {error}");
    }
    running.shutdown().await;
    #[cfg(unix)]
    {
        control.abort();
        let _ = std::fs::remove_file(control_path);
    }
    drop(lock);
}

fn evaluate_extractor() -> Result<serde_json::Value, String> {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../eval/extractor-v1.json"))
            .map_err(|error| format!("bundled Extractor fixture is invalid: {error}"))?;
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
        results.push(serde_json::json!({"case_id":id,"expected_valid":expected,"actual_valid":actual,"pass":actual==expected}));
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
    #[test]
    fn s04_extractor_eval_is_machine_readable() {
        let report = super::evaluate_extractor().unwrap();
        assert_eq!(report["role"], "extractor");
        assert_eq!(report["dataset_id"], "extractor-v1");
        assert_eq!(report["provider_invoked"], false);
        assert_eq!(report["cases"], 4);
        assert_eq!(report["correct"], 4);
        assert_eq!(report["results"].as_array().unwrap().len(), 4);
    }
}
