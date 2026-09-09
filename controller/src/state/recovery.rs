//! Durable, Ignis-internal restart recovery state. No connector-v1 payload uses these records.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionReceipt {
    pub verb: String,
    pub args_hash: String,
    pub args: Value,
    pub frame: Value,
    pub completed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectorJobRecord {
    pub job_id: String,
    pub tenant_id: String,
    pub run_id: String,
    pub job: Value,
    pub workspace_path: Option<String>,
    pub sandbox_id: Option<String>,
    #[serde(default)]
    pub terminal: bool,
    #[serde(default)]
    pub cleanup_failed: bool,
    #[serde(default)]
    pub receipts: HashMap<String, ActionReceipt>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ControllerReceipt {
    args_hash: String,
    response: Value,
}

/// Two durable internal journals. `connector-jobs` belongs to the connector;
/// `controller-actions` is the controller-side idempotency guard.
pub struct RecoveryStore {
    root: Option<PathBuf>,
    lock: Mutex<()>,
    jobs: Mutex<HashMap<String, ConnectorJobRecord>>,
    controller: Mutex<HashMap<String, ControllerReceipt>>,
}

impl RecoveryStore {
    pub fn in_memory() -> Self {
        Self {
            root: None,
            lock: Mutex::new(()),
            jobs: Mutex::new(HashMap::new()),
            controller: Mutex::new(HashMap::new()),
        }
    }

    pub fn open(data_dir: impl AsRef<Path>) -> Result<Self, String> {
        let root = data_dir.as_ref().to_path_buf();
        fs::create_dir_all(root.join("connector-jobs")).map_err(|e| e.to_string())?;
        fs::create_dir_all(root.join("controller-actions")).map_err(|e| e.to_string())?;
        let store = Self {
            root: Some(root),
            lock: Mutex::new(()),
            jobs: Mutex::new(HashMap::new()),
            controller: Mutex::new(HashMap::new()),
        };
        store.load()?;
        Ok(store)
    }

    fn load(&self) -> Result<(), String> {
        let Some(root) = &self.root else {
            return Ok(());
        };
        for entry in fs::read_dir(root.join("connector-jobs")).map_err(|e| e.to_string())? {
            let path = entry.map_err(|e| e.to_string())?.path();
            if path.extension().and_then(|x| x.to_str()) != Some("json") {
                continue;
            }
            let record: ConnectorJobRecord =
                serde_json::from_slice(&fs::read(&path).map_err(|e| e.to_string())?)
                    .map_err(|e| e.to_string())?;
            self.jobs
                .lock()
                .map_err(|_| "recovery jobs lock poisoned".to_string())?
                .insert(record.job_id.clone(), record);
        }
        for entry in fs::read_dir(root.join("controller-actions")).map_err(|e| e.to_string())? {
            let path = entry.map_err(|e| e.to_string())?.path();
            if path.extension().and_then(|x| x.to_str()) != Some("json") {
                continue;
            }
            let key = path
                .file_stem()
                .and_then(|x| x.to_str())
                .ok_or("invalid receipt filename")?
                .to_string();
            let receipt: ControllerReceipt =
                serde_json::from_slice(&fs::read(&path).map_err(|e| e.to_string())?)
                    .map_err(|e| e.to_string())?;
            self.controller
                .lock()
                .map_err(|_| "recovery controller lock poisoned".to_string())?
                .insert(key, receipt);
        }
        Ok(())
    }

    fn persist<T: Serialize>(&self, directory: &str, key: &str, value: &T) -> Result<(), String> {
        let Some(root) = &self.root else {
            return Ok(());
        };
        let _guard = self
            .lock
            .lock()
            .map_err(|_| "recovery file lock poisoned".to_string())?;
        let path = root.join(directory).join(format!("{key}.json"));
        let tmp = path.with_extension("json.tmp");
        fs::write(
            &tmp,
            serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        fs::rename(tmp, path).map_err(|e| e.to_string())
    }

    pub fn save_job(&self, record: ConnectorJobRecord) -> Result<(), String> {
        self.persist("connector-jobs", &record.job_id, &record)?;
        self.jobs
            .lock()
            .map_err(|_| "recovery jobs lock poisoned".to_string())?
            .insert(record.job_id.clone(), record);
        Ok(())
    }
    pub fn job(&self, job_id: &str) -> Option<ConnectorJobRecord> {
        self.jobs.lock().ok()?.get(job_id).cloned()
    }
    pub fn active_jobs(&self) -> Vec<ConnectorJobRecord> {
        self.jobs
            .lock()
            .map(|j| j.values().filter(|x| !x.terminal).cloned().collect())
            .unwrap_or_default()
    }
    pub fn remove_job(&self, job_id: &str) -> Result<(), String> {
        self.jobs
            .lock()
            .map_err(|_| "recovery jobs lock poisoned".to_string())?
            .remove(job_id);
        if let Some(root) = &self.root {
            let path = root.join("connector-jobs").join(format!("{job_id}.json"));
            if path.exists() {
                fs::remove_file(path).map_err(|e| e.to_string())?;
            }
        }
        Ok(())
    }

    fn guard_key(scope: &str, action_id: &str) -> String {
        let digest = Sha256::digest(scope.as_bytes());
        format!(
            "{}-{action_id}",
            digest
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        )
    }
    pub fn controller_receipt(
        &self,
        scope: &str,
        action_id: &str,
        args_hash: &str,
    ) -> Result<Option<Value>, String> {
        let key = Self::guard_key(scope, action_id);
        let receipt = self
            .controller
            .lock()
            .map_err(|_| "recovery controller lock poisoned".to_string())?
            .get(&key)
            .cloned();
        match receipt {
            Some(r) if r.args_hash == args_hash => Ok(Some(r.response)),
            Some(_) => Err("action_id replay payload mismatch".into()),
            None => Ok(None),
        }
    }
    pub fn save_controller_receipt(
        &self,
        scope: &str,
        action_id: &str,
        args_hash: String,
        response: Value,
    ) -> Result<(), String> {
        let key = Self::guard_key(scope, action_id);
        let receipt = ControllerReceipt {
            args_hash,
            response,
        };
        self.persist("controller-actions", &key, &receipt)?;
        self.controller
            .lock()
            .map_err(|_| "recovery controller lock poisoned".to_string())?
            .insert(key, receipt);
        Ok(())
    }
}

pub fn value_hash(value: &Value) -> String {
    let encoded = serde_json::to_vec(value).unwrap_or_default();
    Sha256::digest(encoded)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn job(id: &str) -> ConnectorJobRecord {
        ConnectorJobRecord {
            job_id: id.into(),
            tenant_id: "tenant".into(),
            run_id: "run-1".into(),
            job: serde_json::json!({"job_id": id}),
            workspace_path: Some("/tmp/workspace".into()),
            sandbox_id: Some("sandbox-1".into()),
            terminal: false,
            cleanup_failed: false,
            receipts: HashMap::new(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn recovery_records_and_controller_receipts_survive_reopen() {
        let dir = tempdir().unwrap();
        let store = RecoveryStore::open(dir.path()).unwrap();
        let mut record = job("job-1");
        record.receipts.insert(
            "action-1".into(),
            ActionReceipt {
                verb: "deploy_revision".into(),
                args_hash: "hash".into(),
                args: serde_json::json!({"x": 1}),
                frame: serde_json::json!({"kind":"result"}),
                completed_at: Utc::now(),
            },
        );
        store.save_job(record).unwrap();
        store
            .save_controller_receipt(
                "sandbox-1",
                "action-1",
                "hash".into(),
                serde_json::json!({"sandbox_id":"sandbox-1"}),
            )
            .unwrap();

        let reopened = RecoveryStore::open(dir.path()).unwrap();
        assert_eq!(
            reopened.job("job-1").unwrap().sandbox_id.as_deref(),
            Some("sandbox-1")
        );
        assert!(reopened
            .controller_receipt("sandbox-1", "action-1", "hash")
            .unwrap()
            .is_some());
        assert!(reopened
            .controller_receipt("sandbox-1", "action-1", "other")
            .is_err());
    }
}
