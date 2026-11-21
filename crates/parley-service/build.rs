use serde_json::Value;
use std::{
    env, fs,
    path::{Path, PathBuf},
};

fn main() {
    let root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("manifest dir"));
    let dist = root.join("../../web/dist");
    let manifest_path = dist.join(".vite/manifest.json");
    println!("cargo:rerun-if-changed={}", manifest_path.display());
    let output = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR")).join("web_assets.rs");
    let generated = match fs::read_to_string(&manifest_path) {
        Ok(text) => embed_manifest(&dist, &text),
        Err(_) => {
            "pub static INDEX: &[u8] = b\"\";\npub static ASSETS: &[(&str, &str, &[u8])] = &[];\n"
                .to_owned()
        }
    };
    fs::write(output, generated).expect("write embedded asset table");
}

fn embed_manifest(dist: &Path, text: &str) -> String {
    let manifest: Value = serde_json::from_str(text).expect("valid Vite manifest");
    let mut out = format!(
        "pub static INDEX: &[u8] = include_bytes!({:?});\n",
        dist.join("index.html").to_string_lossy()
    );
    out.push_str("pub static ASSETS: &[(&str, &str, &[u8])] = &[\n");
    for entry in manifest
        .as_object()
        .into_iter()
        .flatten()
        .map(|(_, value)| value)
    {
        let mut files = Vec::new();
        if let Some(file) = entry.get("file").and_then(Value::as_str) {
            files.push(file.to_owned());
        }
        if let Some(css) = entry.get("css").and_then(Value::as_array) {
            files.extend(css.iter().filter_map(Value::as_str).map(str::to_owned));
        }
        for file in files {
            let mime = match PathBuf::from(&file)
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
            {
                "js" => "text/javascript; charset=utf-8",
                "css" => "text/css; charset=utf-8",
                "svg" => "image/svg+xml",
                "woff2" => "font/woff2",
                _ => "application/octet-stream",
            };
            out.push_str(&format!(
                "({file:?}, {mime:?}, include_bytes!({:?})),\n",
                dist.join(&file).to_string_lossy()
            ));
        }
    }
    out.push_str("];\n");
    out
}
