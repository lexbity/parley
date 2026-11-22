//! Crash-safe workspace generation selection used for restore and erasure.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug)]
pub(crate) struct WorkspaceRoot {
    identity: PathBuf,
    control: PathBuf,
    generations: PathBuf,
    selector: PathBuf,
}
#[derive(Clone, Debug)]
pub(crate) struct ActiveGeneration {
    pub id: String,
    pub database: PathBuf,
    pub needs_publish: bool,
}
#[derive(Serialize, Deserialize)]
struct Selector {
    format: u32,
    generation: String,
    sha256: String,
}
impl WorkspaceRoot {
    pub fn new(identity: &Path) -> Result<Self, String> {
        let parent = identity.parent().ok_or("workspace path requires parent")?;
        let name = identity
            .file_name()
            .and_then(|v| v.to_str())
            .ok_or("workspace filename must be UTF-8")?;
        let control = parent.join(format!(".{name}.parley"));
        Ok(Self {
            identity: identity.to_path_buf(),
            generations: control.join("generations"),
            selector: control.join("CURRENT"),
            control,
        })
    }
    pub fn control_path(&self) -> &Path {
        &self.control
    }
    #[cfg(test)]
    pub fn generations_path(&self) -> &Path {
        &self.generations
    }
    pub fn ensure_control(&self) -> Result<(), String> {
        fs::create_dir_all(&self.control).map_err(|e| e.to_string())?;
        secure_dir(&self.control)?;
        Ok(())
    }
    fn path(&self, id: &str) -> Result<PathBuf, String> {
        if id.is_empty() || !id.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-') {
            return Err("invalid generation id".into());
        }
        Ok(self.generations.join(id))
    }
    pub fn database_path(&self, id: &str) -> Result<PathBuf, String> {
        Ok(self.path(id)?.join("workspace.sqlite"))
    }
    pub fn prepare_generation(&self, id: &str) -> Result<PathBuf, String> {
        self.ensure_control()?;
        fs::create_dir_all(&self.generations).map_err(|e| e.to_string())?;
        let p = self.path(id)?;
        fs::create_dir(&p).map_err(|e| e.to_string())?;
        secure_dir(&p)?;
        Ok(p)
    }
    pub fn blob_path(&self, id: &str) -> Result<PathBuf, String> {
        Ok(self.path(id)?.join("blobs"))
    }
    pub fn read_selector(&self) -> Result<String, String> {
        let s: Selector =
            serde_json::from_slice(&fs::read(&self.selector).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
        let expected = selector_hash(s.format, &s.generation);
        if s.format != 1 || s.sha256 != expected {
            return Err("unsupported or corrupt CURRENT selector".into());
        }
        Ok(s.generation)
    }
    pub fn publish(&self, id: &str) -> Result<(), String> {
        if !self.database_path(id)?.is_file() {
            return Err("generation database missing".into());
        }
        self.ensure_control()?;
        let temp = self
            .control
            .join(format!(".CURRENT-{}.tmp", std::process::id()));
        let data = serde_json::to_vec(&Selector {
            format: 1,
            generation: id.into(),
            sha256: selector_hash(1, id),
        })
        .map_err(|e| e.to_string())?;
        let mut f = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
            .map_err(|e| e.to_string())?;
        f.write_all(&data).map_err(|e| e.to_string())?;
        f.sync_all().map_err(|e| e.to_string())?;
        drop(f);
        #[cfg(test)]
        stop_at_test_boundary("before_rename", &self.control)?;
        fs::rename(&temp, &self.selector).map_err(|e| e.to_string())?;
        sync_dir(&self.control)?;
        #[cfg(test)]
        stop_at_test_boundary("after_rename", &self.control)?;
        Ok(())
    }
    pub async fn prepare_active(&self) -> Result<ActiveGeneration, String> {
        self.ensure_control()?;
        fs::create_dir_all(&self.generations).map_err(|e| e.to_string())?;
        if self.selector.exists() {
            let id = self.read_selector()?;
            let database = self.database_path(&id)?;
            if !database.is_file() {
                return Err("CURRENT database missing".into());
            }
            Ok(ActiveGeneration {
                id,
                database,
                needs_publish: false,
            })
        } else {
            let id = format!("gen-{}", std::process::id());
            let p = self.prepare_generation(&id)?;
            let db = p.join("workspace.sqlite");
            if self.identity.is_file() {
                fs::copy(&self.identity, &db)
                    .map_err(|e| format!("legacy database snapshot failed: {e}"))?;
            }
            Ok(ActiveGeneration {
                id,
                database: db,
                needs_publish: true,
            })
        }
    }
    pub fn publish_initial(&self, id: &str) -> Result<(), String> {
        if self.selector.exists() {
            return Err("selector already exists".into());
        }
        self.publish(id)?;
        if self.identity.is_file() {
            fs::remove_file(&self.identity).map_err(|e| e.to_string())?;
        }
        Ok(())
    }
    pub fn reconcile_unselected(&self) -> Result<u64, String> {
        let selected = self.read_selector()?;
        let mut removed = 0;
        for e in fs::read_dir(&self.generations).map_err(|e| e.to_string())? {
            let e = e.map_err(|e| e.to_string())?;
            if e.file_name() == std::ffi::OsStr::new(&selected) {
                continue;
            }
            let meta = fs::symlink_metadata(e.path()).map_err(|e| e.to_string())?;
            if meta.file_type().is_symlink() {
                return Err("generation symlink rejected".into());
            }
            removed += dir_size(&e.path())?;
            if meta.is_dir() {
                fs::remove_dir_all(e.path()).map_err(|e| e.to_string())?
            } else {
                fs::remove_file(e.path()).map_err(|e| e.to_string())?
            }
        }
        sync_dir(&self.generations)?;
        Ok(removed)
    }
    pub fn default_backup_path(&self) -> PathBuf {
        self.control.with_extension("backups")
    }
}

#[cfg(test)]
fn stop_at_test_boundary(boundary: &str, control: &Path) -> Result<(), String> {
    if std::env::var("PARLEY_GENERATION_PAUSE").ok().as_deref() != Some(boundary) {
        return Ok(());
    }
    let marker = control.join(format!("pause-{boundary}"));
    fs::write(marker, b"ready").map_err(|e| e.to_string())?;
    loop {
        std::thread::park_timeout(std::time::Duration::from_millis(20));
    }
}
fn selector_hash(version: u32, id: &str) -> String {
    hex::encode(Sha256::digest(format!("{version}\0{id}").as_bytes()))
}
fn dir_size(path: &Path) -> Result<u64, String> {
    let mut sum = 0;
    for e in fs::read_dir(path).map_err(|e| e.to_string())? {
        let e = e.map_err(|e| e.to_string())?;
        let m = fs::symlink_metadata(e.path()).map_err(|e| e.to_string())?;
        if m.file_type().is_symlink() {
            return Err("symlink in generation".into());
        }
        sum += if m.is_dir() {
            dir_size(&e.path())?
        } else {
            m.len()
        };
    }
    Ok(sum)
}
fn sync_dir(p: &Path) -> Result<(), String> {
    File::open(p)
        .and_then(|f| f.sync_all())
        .map_err(|e| e.to_string())
}
#[cfg(unix)]
fn secure_dir(p: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(p, fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())
}
#[cfg(not(unix))]
fn secure_dir(_: &Path) -> Result<(), String> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        process::{Command, Stdio},
        thread,
        time::{Duration, Instant},
    };

    fn make_root() -> (PathBuf, WorkspaceRoot) {
        let parent = std::env::temp_dir().join(format!(
            "parley-gen-{}-{}",
            std::process::id(),
            random_test_suffix()
        ));
        fs::create_dir_all(&parent).unwrap();
        let root = WorkspaceRoot::new(&parent.join("workspace.sqlite")).unwrap();
        root.ensure_control().unwrap();
        fs::create_dir_all(root.generations_path()).unwrap();
        (parent, root)
    }
    fn make_generation(root: &WorkspaceRoot, id: &str) {
        let path = root.prepare_generation(id).unwrap();
        fs::write(path.join("workspace.sqlite"), b"db").unwrap();
    }
    fn random_test_suffix() -> String {
        format!(
            "{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        )
    }

    #[test]
    fn child_publish_pause() {
        let Ok(parent) = std::env::var("PARLEY_GENERATION_ROOT") else {
            return;
        };
        let root = WorkspaceRoot::new(&PathBuf::from(parent).join("workspace.sqlite")).unwrap();
        let _ = root.publish("new");
    }

    #[test]
    fn s11_process_interruption_at_selector_publication_boundary() {
        for (boundary, expected) in [("before_rename", "old"), ("after_rename", "new")] {
            let (parent, root) = make_root();
            make_generation(&root, "old");
            root.publish("old").unwrap();
            make_generation(&root, "new");
            let executable = std::env::current_exe().unwrap();
            let mut child = Command::new(executable)
                .arg("--exact")
                .arg("generation::tests::child_publish_pause")
                .arg("--nocapture")
                .env("PARLEY_GENERATION_ROOT", &parent)
                .env("PARLEY_GENERATION_PAUSE", boundary)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            let marker = root.control_path().join(format!("pause-{boundary}"));
            let until = Instant::now() + Duration::from_secs(5);
            while !marker.exists() && Instant::now() < until {
                thread::sleep(Duration::from_millis(10));
            }
            assert!(marker.exists(), "child missed selector boundary {boundary}");
            child.kill().unwrap();
            let _ = child.wait();
            assert_eq!(root.read_selector().unwrap(), expected);
            let _ = fs::remove_dir_all(parent);
        }
    }
}
