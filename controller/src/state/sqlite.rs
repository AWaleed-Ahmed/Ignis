//! Durable sandbox metadata store (P2).
//! JSON-document files today (no extra crates); swap implementation for SQLite/Postgres later.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::state::registry::SandboxRecord;

pub struct SqliteStore {
    /// Kept name for API stability; backend is a JSON document directory.
    root: PathBuf,
    lock: Mutex<()>,
}

impl SqliteStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, String> {
        // If path ends with .db, use sibling `sandboxes/` dir for JSON docs.
        let path = path.as_ref();
        let root = if path.extension().and_then(|e| e.to_str()) == Some("db") {
            path.parent()
                .unwrap_or_else(|| Path::new("."))
                .join("sandboxes")
        } else {
            path.to_path_buf()
        };
        fs::create_dir_all(&root).map_err(|e| e.to_string())?;
        // Marker file notes intended SQLite migration.
        let marker = root.join("README.txt");
        if !marker.exists() {
            let _ = fs::write(
                &marker,
                "Raphael sandbox durable store (JSON documents). Replace with SQLite/Postgres via RAPHAEL_SQLITE_PATH when available.\n",
            );
        }
        Ok(Self {
            root,
            lock: Mutex::new(()),
        })
    }

    pub fn path(&self) -> &Path {
        &self.root
    }

    fn file_for(&self, sandbox_id: &str) -> PathBuf {
        self.root.join(format!("{sandbox_id}.json"))
    }

    pub fn upsert(&self, record: &SandboxRecord) -> Result<(), String> {
        let _g = self
            .lock
            .lock()
            .map_err(|_| "store lock poisoned".to_string())?;
        let path = self.file_for(&record.sandbox_id);
        let payload = serde_json::to_vec_pretty(record).map_err(|e| e.to_string())?;
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, payload).map_err(|e| e.to_string())?;
        fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
        // Index by result_id when present
        if let Some(result) = &record.finalized_result {
            let idx = self.root.join("by-result");
            fs::create_dir_all(&idx).map_err(|e| e.to_string())?;
            let link = idx.join(format!("{}.json", result.result_id));
            let _ = fs::write(&link, record.sandbox_id.as_bytes());
        }
        Ok(())
    }

    pub fn load_all(&self) -> Result<Vec<SandboxRecord>, String> {
        let _g = self
            .lock
            .lock()
            .map_err(|_| "store lock poisoned".to_string())?;
        let mut out = Vec::new();
        if !self.root.exists() {
            return Ok(out);
        }
        for entry in fs::read_dir(&self.root).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let bytes = fs::read(&path).map_err(|e| e.to_string())?;
            let record: SandboxRecord =
                serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
            out.push(record);
        }
        Ok(out)
    }

    pub fn get_by_result_id(&self, result_id: &str) -> Result<Option<SandboxRecord>, String> {
        let idx = self
            .root
            .join("by-result")
            .join(format!("{result_id}.json"));
        if !idx.exists() {
            return Ok(None);
        }
        let sid = fs::read_to_string(&idx).map_err(|e| e.to_string())?;
        let path = self.file_for(sid.trim());
        if !path.exists() {
            return Ok(None);
        }
        let bytes = fs::read(&path).map_err(|e| e.to_string())?;
        let record: SandboxRecord = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        Ok(Some(record))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::registry::{SandboxRecord, SandboxStatus};
    use chrono::Utc;

    #[test]
    fn upsert_and_reload() {
        let dir = tempfile::tempdir().unwrap();
        let store = SqliteStore::open(dir.path().join("sandboxes.db")).unwrap();
        let mut rec = SandboxRecord {
            sandbox_id: "sb-test".into(),
            run_id: "run-1".into(),
            tenant_id: "t".into(),
            namespace: "ns".into(),
            commit_sha: "abcdef0".into(),
            repository_owner: "o".into(),
            repository_name: "n".into(),
            clone_url: None,
            cloned_workspace: None,
            target_environment: None,
            secret_fixture_set: None,
            status: SandboxStatus::Ready,
            created_at: Utc::now(),
            expires_at: Utc::now(),
            service_account: "sa".into(),
            deployed_sha: None,
            rendered_yaml: None,
            resources: vec![],
            image_refs: vec![],
            last_signature: None,
            reproduction_signature: None,
            after_signature: None,
            last_fidelity: None,
            last_patch: None,
            last_validation: None,
            finalized_result: None,
            artifacts: vec![],
            cluster_backend: "mock".into(),
        };
        store.upsert(&rec).unwrap();
        rec.status = SandboxStatus::Destroyed;
        store.upsert(&rec).unwrap();
        let all = store.load_all().unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].status, SandboxStatus::Destroyed);
    }
}
