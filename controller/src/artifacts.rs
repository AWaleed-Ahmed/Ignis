//! Local filesystem artifact retention (P2). Encryption/object storage later.

use std::fs;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use chrono::Utc;
use uuid::Uuid;

use crate::domain::models::ArtifactRecord;

pub fn artifact_root() -> PathBuf {
    std::env::var_os("RAPHAEL_ARTIFACT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(".raphael-artifacts"))
}

pub fn retention_hours() -> u64 {
    std::env::var("RAPHAEL_ARTIFACT_RETENTION_HOURS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(48)
}

/// Persist artifact body to disk; keep a short in-memory preview in `content`.
pub fn persist_artifact(
    sandbox_id: &str,
    kind: &str,
    content: &str,
) -> Result<ArtifactRecord, String> {
    let id = format!("artifact-{}", Uuid::new_v4());
    let root = artifact_root().join(sandbox_id);
    fs::create_dir_all(&root).map_err(|e| e.to_string())?;
    let path = root.join(format!("{id}.txt"));
    fs::write(&path, content).map_err(|e| e.to_string())?;
    let preview: String = content.chars().take(512).collect();
    Ok(ArtifactRecord {
        id,
        kind: kind.to_string(),
        content: preview,
        path: Some(path.display().to_string()),
        created_at: Utc::now(),
    })
}

pub fn purge_sandbox_artifacts(sandbox_id: &str) -> Result<(), String> {
    let dir = artifact_root().join(sandbox_id);
    if dir.exists() {
        fs::remove_dir_all(&dir).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Delete artifact directories older than retention window.
pub fn purge_expired_artifacts() -> Result<usize, String> {
    let root = artifact_root();
    if !root.exists() {
        return Ok(0);
    }
    let max_age = Duration::from_secs(retention_hours().saturating_mul(3600));
    let now = SystemTime::now();
    let mut removed = 0usize;
    for entry in fs::read_dir(&root).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let meta = entry.metadata().map_err(|e| e.to_string())?;
        if !meta.is_dir() {
            continue;
        }
        let aged_out = meta
            .modified()
            .ok()
            .and_then(|m| now.duration_since(m).ok())
            .map(|d| d > max_age)
            .unwrap_or(false);
        if aged_out {
            let _ = fs::remove_dir_all(entry.path());
            removed += 1;
        }
    }
    Ok(removed)
}

#[allow(dead_code)]
pub fn ensure_root() -> Result<PathBuf, String> {
    let root = artifact_root();
    fs::create_dir_all(&root).map_err(|e| e.to_string())?;
    Ok(root)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persist_and_purge() {
        let sid = "sb-artifact-test";
        let _ = purge_sandbox_artifacts(sid);
        let rec = persist_artifact(sid, "manifest", "kind: Pod\n").unwrap();
        assert!(std::path::Path::new(rec.path.as_ref().unwrap()).exists());
        purge_sandbox_artifacts(sid).unwrap();
        assert!(!artifact_root().join(sid).exists());
    }
}
