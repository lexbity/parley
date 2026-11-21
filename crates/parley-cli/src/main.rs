use parley_service::Workspace;
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::{
        fs::{OpenOptionsExt, PermissionsExt},
        io::AsRawFd,
    },
    path::PathBuf,
};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|arg| arg == "--version" || arg == "-V") {
        println!("parley {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    let workspace = parse_workspace(&args).unwrap_or_else(default_workspace);
    let port = parse_port(&args).unwrap_or(0);
    if let Err(error) = run(workspace, port) {
        eprintln!("parley: {error}");
        std::process::exit(2);
    }
}
fn parse_port(args: &[String]) -> Option<u16> {
    args.windows(2)
        .find(|pair| pair[0] == "--port")
        .and_then(|pair| pair[1].parse().ok())
}

fn parse_workspace(args: &[String]) -> Option<PathBuf> {
    args.windows(2)
        .find(|pair| pair[0] == "--workspace")
        .map(|pair| PathBuf::from(&pair[1]))
}

fn default_workspace() -> PathBuf {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
        .unwrap_or_else(|| std::env::temp_dir().join("parley-owner-data"));
    base.join("parley")
}

fn run(root: PathBuf, requested_port: u16) -> Result<(), Box<dyn std::error::Error>> {
    fs::create_dir_all(&root)?;
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(root.join("workspace.lock"))?;
    // flock is released by the OS on process exit; the file's existence alone
    // never represents ownership.
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err("workspace is already open by another process".into());
    }
    let token_path = root.join("mutation.token");
    let token = if token_path.exists() {
        let mut value = String::new();
        File::open(&token_path)?.read_to_string(&mut value)?;
        value
    } else {
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).map_err(|error| std::io::Error::other(error.to_string()))?;
        let value = bytes
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&token_path)?;
        file.write_all(value.as_bytes())?;
        file.sync_all()?;
        value
    };
    let db = root.join("workspace.sqlite");
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async move {
        let workspace = Workspace::open(db).await?;
        let listener =
            tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, requested_port)).await?;
        let port = listener.local_addr()?.port();
        println!("http://127.0.0.1:{port}/");
        parley_service::serve(listener, workspace, token, port).await?;
        Ok::<(), Box<dyn std::error::Error>>(())
    })?;
    Ok(())
}
