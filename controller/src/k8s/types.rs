use chrono::{DateTime, Utc};
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct NamespaceSpec {
    pub namespace: String,
    pub sandbox_id: String,
    pub run_id: String,
    pub tenant_id: String,
    pub expires_at: DateTime<Utc>,
    pub service_account: String,
    pub cpu_limit: String,
    pub memory_limit: String,
}

#[derive(Debug, Clone)]
pub struct WorkloadObservation {
    pub events: Vec<ObservedEvent>,
    pub pods: Vec<ObservedPod>,
    pub rendered_hint: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ObservedEvent {
    pub reason: String,
    pub message: String,
    pub involved_kind: String,
    pub involved_name: String,
}

#[derive(Debug, Clone)]
pub struct ObservedPod {
    pub name: String,
    pub phase: String,
    pub container_statuses: Vec<ObservedContainerStatus>,
}

#[derive(Debug, Clone)]
pub struct ObservedContainerStatus {
    pub name: String,
    pub ready: bool,
    pub restart_count: i32,
    pub waiting_reason: Option<String>,
    pub waiting_message: Option<String>,
    pub last_termination_reason: Option<String>,
    pub image: Option<String>,
}

#[derive(Debug, Clone)]
pub struct RolloutStatus {
    pub ready: bool,
    pub message: String,
}

#[derive(Debug, Clone)]
pub struct HttpHealthResult {
    pub ok: bool,
    pub status_code: Option<i32>,
    pub message: String,
}

pub fn default_labels(spec: &NamespaceSpec) -> HashMap<String, String> {
    let mut labels = HashMap::new();
    labels.insert("raphael.managed".into(), "true".into());
    labels.insert("raphael.sandbox_id".into(), spec.sandbox_id.clone());
    labels.insert("raphael.run_id".into(), spec.run_id.clone());
    labels.insert("raphael.tenant_id".into(), spec.tenant_id.clone());
    labels.insert("raphael.expires_at".into(), spec.expires_at.to_rfc3339());
    labels
}
