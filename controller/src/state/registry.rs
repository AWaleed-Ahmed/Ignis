use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::domain::models::{
    ArtifactRecord, FailureSignature, FidelityReport, PatchSpec, ResourceRef, ValidatedFixRecord,
    ValidationResults,
};
use crate::state::sqlite::SqliteStore;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxStatus {
    Ready,
    Destroyed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxRecord {
    pub sandbox_id: String,
    pub run_id: String,
    pub tenant_id: String,
    pub namespace: String,
    pub commit_sha: String,
    pub repository_owner: String,
    pub repository_name: String,
    pub clone_url: Option<String>,
    /// Local path produced by clone-at-SHA (controller-managed).
    pub cloned_workspace: Option<String>,
    pub target_environment: Option<String>,
    pub secret_fixture_set: Option<String>,
    pub status: SandboxStatus,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub service_account: String,
    pub deployed_sha: Option<String>,
    pub rendered_yaml: Option<String>,
    pub resources: Vec<ResourceRef>,
    pub image_refs: Vec<String>,
    pub last_signature: Option<FailureSignature>,
    /// Signature observed while the workload was still failing (reproduction).
    pub reproduction_signature: Option<FailureSignature>,
    /// Signature after the candidate fix deploy (usually healthy).
    pub after_signature: Option<FailureSignature>,
    pub last_fidelity: Option<FidelityReport>,
    pub last_patch: Option<PatchSpec>,
    pub last_validation: Option<ValidationResults>,
    pub finalized_result: Option<ValidatedFixRecord>,
    pub artifacts: Vec<ArtifactRecord>,
    pub cluster_backend: String,
}

pub struct SandboxRegistry {
    inner: RwLock<HashMap<String, SandboxRecord>>,
    store: Option<Arc<SqliteStore>>,
}

impl SandboxRegistry {
    pub fn new() -> Self {
        Self {
            inner: RwLock::new(HashMap::new()),
            store: None,
        }
    }

    pub fn with_store(store: Arc<SqliteStore>) -> Result<Self, String> {
        let reg = Self {
            inner: RwLock::new(HashMap::new()),
            store: Some(store.clone()),
        };
        for record in store.load_all()? {
            let mut guard = reg
                .inner
                .write()
                .map_err(|_| "registry lock poisoned".to_string())?;
            guard.insert(record.sandbox_id.clone(), record);
        }
        Ok(reg)
    }

    fn persist(&self, record: &SandboxRecord) {
        if let Some(store) = &self.store {
            if let Err(e) = store.upsert(record) {
                tracing::warn!(error = %e, sandbox_id = %record.sandbox_id, "sqlite upsert failed");
            }
        }
    }

    pub fn store_path(&self) -> Option<std::path::PathBuf> {
        self.store.as_ref().map(|s| s.path().to_path_buf())
    }

    pub fn insert(&self, record: SandboxRecord) -> Result<(), String> {
        let mut guard = self
            .inner
            .write()
            .map_err(|_| "registry lock poisoned".to_string())?;
        if guard.contains_key(&record.sandbox_id) {
            return Err(format!("sandbox already exists: {}", record.sandbox_id));
        }
        // Also reject duplicate active run namespaces
        if guard
            .values()
            .any(|r| r.run_id == record.run_id && r.status == SandboxStatus::Ready)
        {
            return Err(format!(
                "active sandbox already exists for run_id={}",
                record.run_id
            ));
        }
        self.persist(&record);
        guard.insert(record.sandbox_id.clone(), record);
        Ok(())
    }

    pub fn get(&self, sandbox_id: &str) -> Option<SandboxRecord> {
        self.inner
            .read()
            .ok()
            .and_then(|g| g.get(sandbox_id).cloned())
    }

    pub fn update<F>(&self, sandbox_id: &str, mutator: F) -> Result<SandboxRecord, String>
    where
        F: FnOnce(&mut SandboxRecord),
    {
        let mut guard = self
            .inner
            .write()
            .map_err(|_| "registry lock poisoned".to_string())?;
        let record = guard
            .get_mut(sandbox_id)
            .ok_or_else(|| format!("sandbox not found: {sandbox_id}"))?;
        mutator(record);
        let cloned = record.clone();
        drop(guard);
        self.persist(&cloned);
        Ok(cloned)
    }

    pub fn list_expired(&self, now: DateTime<Utc>) -> Vec<SandboxRecord> {
        self.inner
            .read()
            .map(|g| {
                g.values()
                    .filter(|r| r.status == SandboxStatus::Ready && r.expires_at <= now)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn list_ready(&self) -> Vec<SandboxRecord> {
        self.inner
            .read()
            .map(|g| {
                g.values()
                    .filter(|r| r.status == SandboxStatus::Ready)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }
}

impl Default for SandboxRegistry {
    fn default() -> Self {
        Self::new()
    }
}
