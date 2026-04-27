//! Manifest authoring + dispatch endpoints (Phase 4 + 5).
//!
//! All filesystem writes are sandboxed to `state.manifests_dir()` — a name
//! supplied by the caller is sanitised down to a single path segment
//! before any join, so a malicious payload cannot escape the workspace.
//!
//! - `GET  /api/schema`                     — full SchemaSection tree
//! - `GET  /api/manifests`                  — list manifests in workspace
//! - `GET  /api/manifests/:name`            — read one manifest's TOML
//! - `POST /api/manifests`                  — save a manifest (create or overwrite)
//! - `POST /api/manifests/validate`         — TOML → ResolvedManifest, return errors
//! - `POST /api/runs`                       — `pitboss dispatch --background <path>`
//! - `POST /api/runs/:id/fork`              — copy a run's snapshot into the workspace

use std::path::{Path, PathBuf};
use std::process::Stdio;

use axum::{
    extract::{Path as AxPath, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use pitboss_cli::manifest::{
    load::load_manifest_from_str, metadata::sections, validate::validate_skip_dir_check,
};
use serde::{Deserialize, Serialize};

use crate::{
    error::{ApiError, ApiResult},
    state::AppState,
};

const MAX_MANIFEST_BYTES: usize = 256 * 1024; // 256 KiB
const MAX_MANIFEST_NAME_LEN: usize = 64;

// ---- /api/schema ---------------------------------------------------------

pub async fn schema() -> impl IntoResponse {
    Json(sections())
}

// ---- /api/manifests -------------------------------------------------------

#[derive(Serialize)]
pub struct ManifestEntry {
    pub name: String,
    pub size: u64,
    pub mtime_unix: i64,
}

pub async fn list(State(state): State<AppState>) -> ApiResult<Json<Vec<ManifestEntry>>> {
    let dir = state.manifests_dir();
    if !dir.exists() {
        // Workspace not yet created — return empty list rather than 404
        // so the SPA can render the "create first manifest" empty state.
        return Ok(Json(Vec::new()));
    }

    let mut entries = Vec::new();
    let mut rd = match tokio::fs::read_dir(dir).await {
        Ok(r) => r,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Json(Vec::new())),
        Err(e) => return Err(e.into()),
    };
    while let Some(de) = rd.next_entry().await? {
        let name = match de.file_name().into_string() {
            Ok(n) => n,
            Err(_) => continue, // skip non-UTF-8 names
        };
        if !name.ends_with(".toml") {
            continue;
        }
        let meta = match de.metadata().await {
            Ok(m) => m,
            Err(_) => continue,
        };
        if !meta.is_file() {
            continue;
        }
        let mtime_unix = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        entries.push(ManifestEntry {
            name,
            size: meta.len(),
            mtime_unix,
        });
    }
    entries.sort_by_key(|e| std::cmp::Reverse(e.mtime_unix));
    Ok(Json(entries))
}

pub async fn read_one(
    State(state): State<AppState>,
    AxPath(name): AxPath<String>,
) -> ApiResult<Response> {
    let path = manifest_path(state.manifests_dir(), &name)?;
    match tokio::fs::read(&path).await {
        Ok(bytes) => Ok(Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "application/toml; charset=utf-8")
            .body(axum::body::Body::from(bytes))
            .expect("toml response")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(ApiError::NotFound),
        Err(e) => Err(e.into()),
    }
}

#[derive(Deserialize)]
pub struct SaveBody {
    pub name: String,
    pub contents: String,
}

#[derive(Serialize)]
pub struct SaveResult {
    pub name: String,
    pub bytes: usize,
}

pub async fn save(
    State(state): State<AppState>,
    Json(body): Json<SaveBody>,
) -> ApiResult<Json<SaveResult>> {
    if body.contents.len() > MAX_MANIFEST_BYTES {
        return Err(ApiError::BadRequest(format!(
            "manifest exceeds {MAX_MANIFEST_BYTES} bytes"
        )));
    }
    let path = manifest_path(state.manifests_dir(), &body.name)?;
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(&path, body.contents.as_bytes()).await?;
    Ok(Json(SaveResult {
        name: sanitize_manifest_name(&body.name)?.to_string(),
        bytes: body.contents.len(),
    }))
}

// ---- /api/manifests/validate ---------------------------------------------

#[derive(Deserialize)]
pub struct ValidateBody {
    pub contents: String,
}

#[derive(Serialize)]
pub struct ValidateResult {
    pub ok: bool,
    pub errors: Vec<String>,
}

pub async fn validate(Json(body): Json<ValidateBody>) -> Json<ValidateResult> {
    if body.contents.len() > MAX_MANIFEST_BYTES {
        return Json(ValidateResult {
            ok: false,
            errors: vec![format!("manifest exceeds {MAX_MANIFEST_BYTES} bytes")],
        });
    }
    let resolved = match load_manifest_from_str(&body.contents) {
        Ok(r) => r,
        Err(e) => {
            return Json(ValidateResult {
                ok: false,
                errors: chain_errors(&e),
            });
        }
    };
    // Skip the dir-existence check: the editor's cwd may not have the
    // worktree dirs and we don't want validation to fail for that reason.
    if let Err(e) = validate_skip_dir_check(&resolved) {
        return Json(ValidateResult {
            ok: false,
            errors: chain_errors(&e),
        });
    }
    Json(ValidateResult {
        ok: true,
        errors: Vec::new(),
    })
}

// ---- /api/runs (dispatch from console) -----------------------------------

#[derive(Deserialize)]
pub struct DispatchBody {
    pub manifest_name: String,
}

#[derive(Serialize)]
pub struct DispatchResult {
    /// Raw JSON descriptor printed by `pitboss dispatch --background`.
    /// Treated as opaque so future dispatcher additions land in the SPA
    /// without backend changes.
    pub descriptor: serde_json::Value,
}

pub async fn dispatch(
    State(state): State<AppState>,
    Json(body): Json<DispatchBody>,
) -> ApiResult<Json<DispatchResult>> {
    let manifest_path = manifest_path(state.manifests_dir(), &body.manifest_name)?;
    if !manifest_path.is_file() {
        return Err(ApiError::NotFound);
    }

    let bin = std::env::var("PITBOSS_BIN").unwrap_or_else(|_| "pitboss".to_string());
    let output = tokio::process::Command::new(&bin)
        .arg("dispatch")
        .arg(&manifest_path)
        .arg("--background")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|e| {
            ApiError::Io(std::io::Error::other(format!(
                "spawn {bin}: {e}; set PITBOSS_BIN if not on PATH"
            )))
        })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(ApiError::BadRequest(format!(
            "dispatch failed (exit {}): {}",
            output.status.code().unwrap_or(-1),
            stderr.trim()
        )));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let line = stdout
        .lines()
        .find(|l| l.trim_start().starts_with('{'))
        .ok_or_else(|| {
            ApiError::Io(std::io::Error::other(
                "dispatcher did not print JSON descriptor",
            ))
        })?;
    let descriptor: serde_json::Value = serde_json::from_str(line)?;
    Ok(Json(DispatchResult { descriptor }))
}

// ---- /api/runs/:id/fork (Phase 5) ----------------------------------------

#[derive(Deserialize)]
pub struct ForkBody {
    pub new_name: String,
}

#[derive(Serialize)]
pub struct ForkResult {
    pub name: String,
}

pub async fn fork_run(
    State(state): State<AppState>,
    AxPath(run_id): AxPath<String>,
    Json(body): Json<ForkBody>,
) -> ApiResult<Json<ForkResult>> {
    let run_seg = sanitize_run_id(&run_id)?;
    let snapshot = state
        .runs_dir()
        .join(run_seg)
        .join("manifest.snapshot.toml");
    if !snapshot.is_file() {
        return Err(ApiError::NotFound);
    }
    let dest = manifest_path(state.manifests_dir(), &body.new_name)?;
    if let Some(parent) = dest.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    if dest.exists() {
        return Err(ApiError::BadRequest(
            "destination manifest already exists".into(),
        ));
    }
    tokio::fs::copy(&snapshot, &dest).await?;
    Ok(Json(ForkResult {
        name: sanitize_manifest_name(&body.new_name)?.to_string(),
    }))
}

// ---- helpers -------------------------------------------------------------

/// Sanitise a manifest name to a single TOML-suffixed path segment.
/// Accepts the name with or without the `.toml` suffix; the returned
/// value always carries the suffix.
fn sanitize_manifest_name(raw: &str) -> ApiResult<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.len() > MAX_MANIFEST_NAME_LEN {
        return Err(ApiError::BadRequest("invalid manifest name length".into()));
    }
    if trimmed.contains('/') || trimmed.contains('\\') || trimmed.contains("..") {
        return Err(ApiError::BadRequest("invalid manifest name chars".into()));
    }
    if !trimmed
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    {
        return Err(ApiError::BadRequest("invalid manifest name chars".into()));
    }
    let stem = trimmed.strip_suffix(".toml").unwrap_or(trimmed);
    if stem.is_empty() || stem == "." || stem == ".." {
        return Err(ApiError::BadRequest("invalid manifest stem".into()));
    }
    Ok(format!("{stem}.toml"))
}

fn manifest_path(base: &Path, name: &str) -> ApiResult<PathBuf> {
    let safe = sanitize_manifest_name(name)?;
    Ok(base.join(safe))
}

fn sanitize_run_id(s: &str) -> ApiResult<&str> {
    if s.is_empty() || s.len() > 128 || s == "." || s == ".." || s.contains('/') || s.contains('\\')
    {
        return Err(ApiError::BadRequest("invalid run id".into()));
    }
    if !s
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    {
        return Err(ApiError::BadRequest("invalid run id chars".into()));
    }
    Ok(s)
}

/// Flatten an anyhow error chain into one error per cause. Mirrors how
/// `pitboss validate` renders failures so the SPA shows the same text.
fn chain_errors(e: &anyhow::Error) -> Vec<String> {
    let mut out = vec![e.to_string()];
    for cause in e.chain().skip(1) {
        out.push(cause.to_string());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_name_roundtrip() {
        assert_eq!(sanitize_manifest_name("foo").unwrap(), "foo.toml");
        assert_eq!(sanitize_manifest_name("foo.toml").unwrap(), "foo.toml");
        assert_eq!(sanitize_manifest_name("my-run_2").unwrap(), "my-run_2.toml");
    }

    #[test]
    fn sanitize_name_rejects_traversal() {
        assert!(sanitize_manifest_name("../etc/passwd").is_err());
        assert!(sanitize_manifest_name("a/b").is_err());
        assert!(sanitize_manifest_name("a\\b").is_err());
        assert!(sanitize_manifest_name("..").is_err());
        assert!(sanitize_manifest_name("").is_err());
        assert!(sanitize_manifest_name(".toml").is_err());
        assert!(sanitize_manifest_name(&"a".repeat(100)).is_err());
        assert!(sanitize_manifest_name("foo bar").is_err());
        assert!(sanitize_manifest_name("foo!").is_err());
    }
}
