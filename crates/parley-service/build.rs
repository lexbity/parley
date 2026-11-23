use serde_json::Value;
use std::{
    collections::BTreeSet,
    env, fs,
    path::{Path, PathBuf},
};

fn main() {
    let root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("manifest dir"));
    let dist = root.join("../../web/dist");
    let manifest_path = dist.join(".vite/manifest.json");
    println!("cargo:rerun-if-env-changed=PARLEY_REQUIRE_WEB_ASSETS");
    println!("cargo:rerun-if-changed={}", manifest_path.display());
    let output = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR")).join("web_assets.rs");
    let generated = match fs::read_to_string(&manifest_path) {
        Ok(text) => embed_manifest(&dist, &text),
        Err(error)
            if error.kind() == std::io::ErrorKind::NotFound
                && env::var_os("PARLEY_REQUIRE_WEB_ASSETS").is_none() =>
        {
            "pub static INDEX: &[u8] = b\"\";\npub static ASSETS: &[(&str, &str, &[u8])] = &[];\n"
                .to_owned()
        }
        Err(error) => panic!(
            "cannot embed {}: {error}. Run pnpm --dir web run build first.",
            manifest_path.display()
        ),
    };
    fs::write(output, generated).expect("write embedded asset table");
}

fn embed_manifest(dist: &Path, text: &str) -> String {
    let manifest: Value = serde_json::from_str(text).expect("valid Vite manifest");
    let entries = manifest.as_object().expect("Vite manifest object");
    assert!(!entries.is_empty(), "Vite manifest is empty");
    let index = dist.join("index.html");
    println!("cargo:rerun-if-changed={}", index.display());
    assert!(
        index.metadata().is_ok_and(|file| file.len() > 0),
        "Vite index.html is missing or empty"
    );
    let mut out = format!(
        "pub static INDEX: &[u8] = include_bytes!({:?});\n",
        index.to_string_lossy()
    );
    out.push_str("pub static ASSETS: &[(&str, &str, &[u8])] = &[\n");
    let mut files = BTreeSet::new();
    for entry in entries.values() {
        files.insert(entry["file"].as_str().expect("Vite asset file"));
        for field in ["css", "assets"] {
            let Some(assets) = entry.get(field).and_then(Value::as_array) else {
                continue;
            };
            files.extend(
                assets
                    .iter()
                    .map(|value| value.as_str().expect("Vite asset path")),
            );
        }
    }
    for file in files {
        assert!(
            file.starts_with("assets/")
                && Path::new(file)
                    .components()
                    .all(|part| matches!(part, std::path::Component::Normal(_))),
            "invalid embedded asset path: {file}"
        );
        let path = dist.join(file);
        println!("cargo:rerun-if-changed={}", path.display());
        assert!(
            path.metadata().is_ok_and(|file| file.len() > 0),
            "Vite asset is missing or empty: {file}"
        );
        let mime = match path.extension().and_then(|e| e.to_str()).unwrap_or("") {
            "js" => "text/javascript; charset=utf-8",
            "css" => "text/css; charset=utf-8",
            "svg" => "image/svg+xml",
            "woff2" => "font/woff2",
            _ => "application/octet-stream",
        };
        out.push_str(&format!(
            "({file:?}, {mime:?}, include_bytes!({:?})),\n",
            path.to_string_lossy()
        ));
    }
    out.push_str("];\n");
    out
}
