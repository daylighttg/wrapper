//! Embeds the built web panel (`web/dist`, or `$WRAPPER_WEB_DIST`) into the
//! binary. Without a build a placeholder page explains how to make one.
//!
//! Also embeds the release signing key from `update-key.pub` (or
//! `$WRAPPER_UPDATE_PUBKEY`) for `wrapper update`.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

fn collect(dir: &Path, base: &Path, out: &mut Vec<(String, PathBuf)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect(&p, base, out);
        } else if let Ok(rel) = p.strip_prefix(base) {
            let rel = rel.to_string_lossy().replace('\\', "/");
            out.push((rel, p));
        }
    }
}

fn main() {
    println!("cargo:rerun-if-env-changed=WRAPPER_WEB_DIST");
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let dist = std::env::var_os("WRAPPER_WEB_DIST")
        .map(PathBuf::from)
        .unwrap_or_else(|| manifest.join("../../web/dist"));
    println!("cargo:rerun-if-changed={}", dist.display());

    embed_update_key(&manifest);

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let mut files = Vec::new();
    collect(&dist, &dist, &mut files);
    files.sort();

    let mut code = String::from("pub static ASSETS: &[(&str, &[u8])] = &[\n");
    if files.iter().any(|(rel, _)| rel == "index.html") {
        for (rel, path) in &files {
            let abs = path.canonicalize().unwrap_or_else(|_| path.clone());
            writeln!(code, "    ({rel:?}, include_bytes!({:?})),", abs.to_string_lossy()).unwrap();
        }
    } else {
        println!("cargo:warning=web panel not built ({}); embedding a placeholder", dist.display());
        let placeholder = out_dir.join("placeholder.html");
        std::fs::write(
            &placeholder,
            "<!doctype html><meta charset=utf-8><title>wrapper</title>\
             <p>The web panel was not built into this binary. Run <code>npm ci &amp;&amp; npm run build</code> \
             in <code>web/</code> and rebuild. The API is available under <code>/api/host</code> and <code>/api/agent</code>.</p>",
        )
        .unwrap();
        writeln!(code, "    (\"index.html\", include_bytes!({:?})),", placeholder.to_string_lossy()).unwrap();
    }
    code.push_str("];\n");
    std::fs::write(out_dir.join("web_assets.rs"), code).unwrap();
}

/// First non-comment line of `update-key.pub`: hex Ed25519 public key; none
/// means releases are not signed and `wrapper update` checks hashes only.
fn embed_update_key(manifest: &Path) {
    println!("cargo:rerun-if-env-changed=WRAPPER_UPDATE_PUBKEY");
    let file = manifest.join("update-key.pub");
    println!("cargo:rerun-if-changed={}", file.display());
    let key = std::env::var("WRAPPER_UPDATE_PUBKEY").ok().or_else(|| {
        let text = std::fs::read_to_string(&file).ok()?;
        text.lines().map(str::trim).find(|l| !l.is_empty() && !l.starts_with('#')).map(str::to_string)
    });
    let key = key.unwrap_or_default().trim().to_ascii_lowercase();
    if !key.is_empty() && (key.len() != 64 || !key.bytes().all(|b| b.is_ascii_hexdigit())) {
        panic!("update signing key must be 64 hex characters, got {key:?}");
    }
    println!("cargo:rustc-env=WRAPPER_UPDATE_PUBKEY={key}");
}
