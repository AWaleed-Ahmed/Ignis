use std::collections::HashMap;
use std::env;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use tokio::sync::{Mutex, Notify};
use tokio::task::JoinHandle;
use tokio::time::sleep;
use tracing::{info, warn};
use uuid::Uuid;

use crate::gitclone::clone_tempdir_at_sha_with_deadline_and_cancel;
use crate::state::recovery::{ActionReceipt, ConnectorJobRecord, RecoveryStore};

const PROTOCOL_VERSION: &str = "1.0";
const MAX_RECONNECT_SECONDS: u64 = 30;
// Match the bounded destroy RPC (70s), with 2s for transport/report work.
const CLEANUP_HEADROOM_SECONDS: u64 = 72;
const ALLOWED_VERBS: [&str; 6] = [
    "create_sandbox",
    "deploy_revision",
    "observe_failure",
    "run_validation",
    "finalize_result",
    "destroy_sandbox",
];

#[derive(Debug, Clone)]
pub struct ConnectorConfig {
    pub dispatch_url: String,
    pub token: String,
    pub controller_url: String,
    pub tenant_id: String,
    pub polling_interval: Duration,
    pub http_timeout: Duration,
}

impl ConnectorConfig {
    pub fn from_env() -> anyhow::Result<Option<Self>> {
        let Some(dispatch_url) = env::var_os("RAPHAEL_CONNECTOR_DISPATCH_URL") else {
            return Ok(None);
        };
        let dispatch_url = dispatch_url
            .into_string()
            .map_err(|_| anyhow::anyhow!("RAPHAEL_CONNECTOR_DISPATCH_URL is not valid UTF-8"))?;
        let token = env::var("RAPHAEL_CONNECTOR_TOKEN").map_err(|_| {
            anyhow::anyhow!("RAPHAEL_CONNECTOR_TOKEN is required when connector is enabled")
        })?;
        if token.trim().is_empty() {
            return Err(anyhow::anyhow!("RAPHAEL_CONNECTOR_TOKEN must not be empty"));
        }
        let parsed_url = dispatch_url
            .parse::<url::Url>()
            .map_err(|_| anyhow::anyhow!("RAPHAEL_CONNECTOR_DISPATCH_URL must be a valid URL"))?;
        if !["http", "https"].contains(&parsed_url.scheme()) {
            return Err(anyhow::anyhow!(
                "RAPHAEL_CONNECTOR_DISPATCH_URL must use http:// or https://"
            ));
        }
        let http_timeout_seconds = env::var("RAPHAEL_CONNECTOR_HTTP_TIMEOUT_SECONDS")
            .ok()
            .map(|value| {
                value.parse::<u64>().map_err(|_| {
                    anyhow::anyhow!(
                        "RAPHAEL_CONNECTOR_HTTP_TIMEOUT_SECONDS must be a positive integer"
                    )
                })
            })
            .transpose()?
            .unwrap_or(30);
        if http_timeout_seconds == 0 {
            return Err(anyhow::anyhow!(
                "RAPHAEL_CONNECTOR_HTTP_TIMEOUT_SECONDS must be a positive integer"
            ));
        }
        Ok(Some(Self {
            dispatch_url,
            token,
            controller_url: env::var("RAPHAEL_CONNECTOR_CONTROLLER_URL")
                .unwrap_or_else(|_| "http://127.0.0.1:8090".to_string()),
            tenant_id: env::var("RAPHAEL_CONNECTOR_TENANT_ID")
                .unwrap_or_else(|_| "connector".to_string()),
            polling_interval: Duration::from_millis(
                env::var("RAPHAEL_CONNECTOR_POLL_INTERVAL_MS")
                    .ok()
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(1000),
            ),
            http_timeout: Duration::from_secs(http_timeout_seconds),
        }))
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    protocol_version: String,
    message_id: String,
    #[serde(default)]
    job_id: Option<String>,
    kind: String,
    sent_at: String,
    payload: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LeaseClaim {
    job_id: String,
    claim_id: String,
    action_id: String,
    renewal_sequence: u64,
    lease_ttl_seconds: u64,
    action_deadline_at: DateTime<Utc>,
    job_deadline_at: DateTime<Utc>,
    lease_expires_at: DateTime<Utc>,
}

struct LeaseHeartbeat {
    task: JoinHandle<()>,
    failed: Arc<AtomicBool>,
    notify: Arc<Notify>,
    confirmed: Arc<Mutex<LeaseClaim>>,
}

impl LeaseHeartbeat {
    async fn lost(&self) {
        loop {
            if self.failed.load(Ordering::SeqCst) {
                return;
            }
            self.notify.notified().await;
        }
    }
}

fn claim_deadline(claim: &LeaseClaim) -> Result<crate::deadline::Deadline, ConnectorError> {
    if claim.lease_ttl_seconds == 0
        || [&claim.job_id, &claim.action_id, &claim.claim_id]
            .iter()
            .any(|id| Uuid::parse_str(id).is_err())
        || claim.lease_expires_at > claim.action_deadline_at.min(claim.job_deadline_at)
    {
        return Err(ConnectorError::Malformed(
            "invalid lease claim bounds or identity".into(),
        ));
    }
    if Utc::now() >= claim.lease_expires_at {
        return Err(ConnectorError::LeaseExpired(claim.job_id.clone()));
    }
    let absolute = claim.action_deadline_at.min(claim.job_deadline_at);
    let remaining = (absolute - Utc::now())
        .to_std()
        .map_err(|_| ConnectorError::LeaseExpired(claim.job_id.clone()))?;
    if remaining.is_zero() {
        return Err(ConnectorError::LeaseExpired(claim.job_id.clone()));
    }
    let execution = remaining
        .checked_sub(Duration::from_secs(CLEANUP_HEADROOM_SECONDS))
        .filter(|budget| !budget.is_zero())
        .ok_or_else(|| ConnectorError::LeaseExpired(claim.job_id.clone()))?;
    Ok(crate::deadline::Deadline::new(execution))
}

impl Drop for LeaseHeartbeat {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn start_lease_heartbeat(
    client: Client,
    config: ConnectorConfig,
    mut claim: LeaseClaim,
    failed: Arc<AtomicBool>,
) -> LeaseHeartbeat {
    let notify = Arc::new(Notify::new());
    let task_notify = notify.clone();
    let task_failed = failed.clone();
    let confirmed = Arc::new(Mutex::new(claim.clone()));
    let task_confirmed = confirmed.clone();
    let task = tokio::spawn(async move {
        let mut absolute_deadline = claim.action_deadline_at.min(claim.job_deadline_at);
        let renewal_interval = Duration::from_secs((claim.lease_ttl_seconds / 3).max(1));
        let mut sequence = claim.renewal_sequence;
        loop {
            let remaining = (absolute_deadline.min(claim.lease_expires_at) - Utc::now())
                .to_std()
                .unwrap_or_default();
            if remaining.is_zero() {
                task_failed.store(true, Ordering::SeqCst);
                task_notify.notify_one();
                return;
            }
            let interval = renewal_interval.min(remaining / 3);
            sleep(interval).await;
            if Utc::now() >= absolute_deadline.min(claim.lease_expires_at) {
                task_failed.store(true, Ordering::SeqCst);
                task_notify.notify_one();
                return;
            }
            sequence = sequence.saturating_add(1);
            let url = format!(
                "{}/v1/tenants/{}/jobs/{}/lease",
                config.dispatch_url.trim_end_matches('/'),
                config.tenant_id,
                claim.job_id
            );
            let body = json!({
                "action_id": claim.action_id,
                "claim_id": claim.claim_id,
                "renewal_sequence": sequence,
            });
            let mut renewed = false;
            for attempt in 0..3 {
                let remaining = (absolute_deadline.min(claim.lease_expires_at) - Utc::now())
                    .to_std()
                    .unwrap_or_default();
                if remaining.is_zero() {
                    break;
                }
                let response = client
                    .post(&url)
                    .bearer_auth(&config.token)
                    .json(&body)
                    .timeout(
                        remaining.min(Duration::from_secs((claim.lease_ttl_seconds / 6).max(1))),
                    )
                    .send()
                    .await;
                match response {
                    Ok(response) if response.status().is_success() => {
                        match response.json::<LeaseClaim>().await {
                            Ok(updated)
                                if updated.job_id == claim.job_id
                                    && updated.action_id == claim.action_id
                                    && updated.claim_id == claim.claim_id
                                    && updated.renewal_sequence == sequence
                                    && updated.lease_ttl_seconds == claim.lease_ttl_seconds
                                    && updated.action_deadline_at == claim.action_deadline_at
                                    && updated.job_deadline_at == claim.job_deadline_at
                                    && updated.lease_expires_at > Utc::now()
                                    && updated.lease_expires_at <= absolute_deadline =>
                            {
                                claim = updated;
                                *task_confirmed.lock().await = claim.clone();
                                renewed = true;
                            }
                            _ => {}
                        }
                        break;
                    }
                    Ok(response) if response.status() == reqwest::StatusCode::CONFLICT => {
                        // A result may have committed while its response is still in transit.
                        // Only an authenticated, explicit handover may change the action.
                        if let Ok(body) = response.json::<Value>().await {
                            if body.get("code").and_then(Value::as_str) == Some("action_completed")
                            {
                                if let Some(updated) =
                                    body.get("claim").cloned().and_then(|value| {
                                        serde_json::from_value::<LeaseClaim>(value).ok()
                                    })
                                {
                                    if updated.job_id == claim.job_id
                                        && updated.action_id != claim.action_id
                                        && updated.job_deadline_at == claim.job_deadline_at
                                        && updated.action_deadline_at <= updated.job_deadline_at
                                        && updated.lease_expires_at > Utc::now()
                                        && updated.lease_expires_at <= updated.action_deadline_at
                                        && updated.lease_ttl_seconds == claim.lease_ttl_seconds
                                    {
                                        claim = updated;
                                        sequence = claim.renewal_sequence;
                                        absolute_deadline =
                                            claim.action_deadline_at.min(claim.job_deadline_at);
                                        *task_confirmed.lock().await = claim.clone();
                                        renewed = true;
                                    }
                                }
                            }
                        }
                        break;
                    }
                    Ok(response)
                        if !response.status().is_server_error()
                            && response.status().as_u16() != 429 =>
                    {
                        break
                    }
                    _ => {}
                }
                let lease_remaining = (absolute_deadline.min(claim.lease_expires_at) - Utc::now())
                    .to_std()
                    .unwrap_or_default();
                if attempt == 2 || lease_remaining.is_zero() || Utc::now() >= absolute_deadline {
                    break;
                }
                sleep(Duration::from_millis(100 * (attempt + 1)).min(lease_remaining)).await;
            }
            if !renewed {
                task_failed.store(true, Ordering::SeqCst);
                task_notify.notify_one();
                return;
            }
        }
    });
    LeaseHeartbeat {
        task,
        failed,
        notify,
        confirmed,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Job {
    job_id: String,
    repository: Repository,
    commit_sha: String,
    narrowed_location: NarrowedLocation,
    #[serde(default)]
    sandbox_profile: Option<String>,
    #[serde(default)]
    lease_ttl_seconds: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Repository {
    clone_url: String,
    #[serde(default)]
    name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NarrowedLocation {
    file_path: String,
    #[serde(default)]
    line_start: Option<u64>,
    #[serde(default)]
    line_end: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Action {
    job_id: String,
    action_id: String,
    verb: String,
    args: Value,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Terminal {
    job_id: String,
    final_status: String,
    #[serde(default = "default_terminal_instruction")]
    instructions: String,
}

fn default_terminal_instruction() -> String {
    "discard_local_copy".to_string()
}

#[derive(Debug, Clone, Serialize)]
struct ResultPayload {
    job_id: String,
    action_id: String,
    verb: String,
    status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<ErrorBody>,
}

#[derive(Debug, Clone)]
struct CachedAction {
    verb: String,
    args: Value,
    frame: String,
}

#[derive(Debug, Clone)]
pub struct LocalJob {
    job: Job,
    workspace_path: PathBuf,
    sandbox_id: Option<String>,
    terminal: bool,
    processed_actions: HashMap<String, CachedAction>,
}

#[derive(Debug, Clone, Serialize)]
struct ErrorBody {
    code: String,
    message: String,
}

#[derive(Debug, thiserror::Error)]
pub enum ConnectorError {
    #[error("malformed connector envelope: {0}")]
    Malformed(String),
    #[error("unsupported connector message: {0}")]
    Unsupported(String),
    #[error("controller execution failed: {0}")]
    Execution(String),
    #[error("controller execution timed out: {0}")]
    Timeout(String),
    #[error("lease expired for job {0}")]
    LeaseExpired(String),
}

#[async_trait]
pub trait WorkspaceCloner: Send + Sync {
    async fn clone_job(
        &self,
        job: &Job,
        deadline: crate::deadline::Deadline,
        cancelled: Arc<AtomicBool>,
    ) -> Result<CloneWorkspace, ConnectorError>;
}

pub struct CloneWorkspace {
    path: PathBuf,
    guard: Option<tempfile::TempDir>,
}

impl CloneWorkspace {
    fn owned(guard: tempfile::TempDir) -> Self {
        Self {
            path: guard.path().to_path_buf(),
            guard: Some(guard),
        }
    }

    fn keep(self) {
        if let Some(guard) = self.guard {
            let _ = guard.keep();
        }
    }
}

#[derive(Clone, Default)]
pub struct GitWorkspaceCloner;

#[async_trait]
impl WorkspaceCloner for GitWorkspaceCloner {
    async fn clone_job(
        &self,
        job: &Job,
        deadline: crate::deadline::Deadline,
        cancelled: Arc<AtomicBool>,
    ) -> Result<CloneWorkspace, ConnectorError> {
        let clone_url = job.repository.clone_url.clone();
        let commit = job.commit_sha.clone();
        let worker_cancelled = cancelled.clone();
        let mut cancellation_guard = CancelOnDrop {
            cancelled,
            armed: true,
        };
        let workspace = tokio::task::spawn_blocking(move || {
            clone_tempdir_at_sha_with_deadline_and_cancel(
                &clone_url,
                &commit,
                deadline,
                &worker_cancelled,
            )
        })
        .await
        .map_err(|e| ConnectorError::Execution(e.to_string()))?
        .map_err(|e| match e {
            crate::domain::errors::DomainError::Timeout(message) => {
                ConnectorError::Timeout(message)
            }
            other => ConnectorError::Execution(other.to_string()),
        })?;
        cancellation_guard.armed = false;
        Ok(CloneWorkspace::owned(workspace))
    }
}

struct CancelOnDrop {
    cancelled: Arc<AtomicBool>,
    armed: bool,
}

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if self.armed {
            self.cancelled.store(true, Ordering::SeqCst);
        }
    }
}

#[async_trait]
pub trait ActionExecutor: Send + Sync {
    async fn execute(
        &self,
        job: &Job,
        local: &LocalJob,
        action: &Action,
        deadline: crate::deadline::Deadline,
    ) -> Result<Value, ConnectorError>;

    async fn destroy(&self, local: &LocalJob) -> Result<(), ConnectorError>;
}

#[derive(Clone)]
pub struct ControllerExecutor {
    client: Client,
    base_url: String,
}

impl ControllerExecutor {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            client: Client::builder()
                .connect_timeout(Duration::from_secs(5))
                .timeout(Duration::from_secs(300))
                .build()
                .expect("local controller HTTP client configuration is valid"),
            base_url: base_url.into().trim_end_matches('/').to_string(),
        }
    }

    async fn post(
        &self,
        path: &str,
        body: Value,
        action_id: Option<&str>,
        budget: Duration,
    ) -> Result<Value, ConnectorError> {
        let mut request = self
            .client
            .post(format!("{}{}", self.base_url, path))
            .json(&body)
            .header(
                "x-raphael-action-deadline-at",
                (Utc::now()
                    + chrono::Duration::from_std(budget)
                        .map_err(|e| ConnectorError::Malformed(e.to_string()))?)
                .to_rfc3339(),
            )
            .timeout(budget);
        if let Some(action_id) = action_id {
            request = request.header("x-raphael-connector-action-id", action_id);
        }
        let response = request.send().await.map_err(controller_http_error)?;
        let status = response.status();
        let payload: Value = response.json().await.map_err(controller_http_error)?;
        if status == reqwest::StatusCode::GATEWAY_TIMEOUT
            || payload.pointer("/error/code").and_then(Value::as_str) == Some("timeout")
        {
            return Err(ConnectorError::Timeout(
                "controller deadline exhausted".into(),
            ));
        }
        if !status.is_success() {
            return Err(ConnectorError::Execution(payload.to_string()));
        }
        Ok(payload)
    }
}

#[async_trait]
impl ActionExecutor for ControllerExecutor {
    async fn execute(
        &self,
        job: &Job,
        local: &LocalJob,
        action: &Action,
        deadline: crate::deadline::Deadline,
    ) -> Result<Value, ConnectorError> {
        validate_action_args(&action.verb, &action.args)?;
        let sandbox_id = local.sandbox_id.as_deref();
        if action.verb != "create_sandbox" && sandbox_id.is_none() {
            return Err(ConnectorError::Execution(
                "sandbox_id is not available".into(),
            ));
        }
        let mut body = match action.verb.as_str() {
            "create_sandbox" => action.args.clone(),
            "deploy_revision" => {
                let mut object = action.args.as_object().cloned().ok_or_else(|| {
                    ConnectorError::Malformed("deploy args must be an object".into())
                })?;
                object.insert(
                    "workspace_path".into(),
                    Value::String(local.workspace_path.to_string_lossy().to_string()),
                );
                Value::Object(object)
            }
            "observe_failure" => action.args.clone(),
            "run_validation" => action.args.clone(),
            "finalize_result" => action.args.clone(),
            "destroy_sandbox" => action.args.clone(),
            _ => return Err(ConnectorError::Unsupported(action.verb.clone())),
        };
        let path = if action.verb == "create_sandbox" {
            "/v1/sandboxes".to_string()
        } else {
            let sandbox_id = sandbox_id.expect("checked above");
            format!(
                "/v1/sandboxes/{sandbox_id}/{}",
                endpoint_suffix(&action.verb)?
            )
        };
        let budget = if action.verb == "deploy_revision" {
            let seconds = action
                .args
                .get("deploy_timeout_seconds")
                .and_then(Value::as_u64)
                .unwrap_or(crate::domain::models::default_deploy_timeout_seconds() as u64);
            if !(1..=600).contains(&seconds) {
                return Err(ConnectorError::Malformed(
                    "deploy_timeout_seconds must be 1..600".into(),
                ));
            }
            Duration::from_secs(seconds + 10)
        } else {
            Duration::from_secs(300)
        };
        let remaining = deadline
            .remaining()
            .map_err(|error| ConnectorError::Timeout(error.to_string()))?;
        cap_action_timeouts(&action.verb, &mut body, remaining)?;
        let budget = deadline
            .cap(budget)
            .map_err(|error| ConnectorError::Timeout(error.to_string()))?;
        let result = self
            .post(&path, body, Some(&action.action_id), budget)
            .await?;
        validate_response(&action.verb, &result)?;
        let _ = job;
        Ok(result)
    }

    async fn destroy(&self, local: &LocalJob) -> Result<(), ConnectorError> {
        let Some(sandbox_id) = &local.sandbox_id else {
            return Ok(());
        };
        let path = format!("/v1/sandboxes/{sandbox_id}/destroy");
        let result = self
            .post(
                &path,
                json!({"reason": "connector_terminal"}),
                None,
                Duration::from_secs(70),
            )
            .await?;
        validate_response("destroy_sandbox", &result)
    }
}

fn cap_action_timeouts(
    verb: &str,
    body: &mut Value,
    remaining: Duration,
) -> Result<(), ConnectorError> {
    let seconds = remaining.as_secs();
    if seconds == 0 {
        return Err(ConnectorError::Timeout(
            "absolute action deadline exhausted".into(),
        ));
    }
    let cap = |object: &mut serde_json::Map<String, Value>, key: &str, default: u64| {
        let requested = object.get(key).and_then(Value::as_u64).unwrap_or(default);
        object.insert(
            key.to_string(),
            Value::from(
                requested
                    .min(seconds)
                    .max(if key == "wait_seconds" { 0 } else { 1 }),
            ),
        );
    };
    match verb {
        "deploy_revision" => {
            if let Some(object) = body.as_object_mut() {
                cap(object, "deploy_timeout_seconds", 120);
                cap(object, "wait_seconds", 60);
            }
        }
        "observe_failure" => {
            if let Some(object) = body.as_object_mut() {
                cap(object, "timeout_seconds", 90);
            }
        }
        "run_validation" => {
            if let Some(plan) = body.get_mut("plan").and_then(Value::as_object_mut) {
                for key in ["health_checks"] {
                    if let Some(items) = plan.get_mut(key).and_then(Value::as_array_mut) {
                        for item in items {
                            if let Some(object) = item.as_object_mut() {
                                cap(object, "timeout_seconds", 90);
                            }
                        }
                    }
                }
            }
        }
        _ => {}
    }
    Ok(())
}

pub struct Connector<E = ControllerExecutor, C = GitWorkspaceCloner> {
    config: ConnectorConfig,
    executor: Arc<E>,
    cloner: Arc<C>,
    jobs: Arc<Mutex<HashMap<String, LocalJob>>>,
    recovery: Arc<RecoveryStore>,
}

impl Connector<ControllerExecutor, GitWorkspaceCloner> {
    pub fn from_config(config: ConnectorConfig, recovery: Arc<RecoveryStore>) -> Self {
        let executor = ControllerExecutor::new(config.controller_url.clone());
        Self::with_recovery(config, executor, GitWorkspaceCloner, recovery)
    }
}

impl<E> Connector<E, GitWorkspaceCloner>
where
    E: ActionExecutor + 'static,
{
    pub fn new(config: ConnectorConfig, executor: E) -> Self {
        Self::with_recovery(
            config,
            executor,
            GitWorkspaceCloner,
            Arc::new(RecoveryStore::in_memory()),
        )
    }
}

impl<E, C> Connector<E, C>
where
    E: ActionExecutor + 'static,
    C: WorkspaceCloner + 'static,
{
    pub fn with_cloner(config: ConnectorConfig, executor: E, cloner: C) -> Self {
        Self::with_recovery(
            config,
            executor,
            cloner,
            Arc::new(RecoveryStore::in_memory()),
        )
    }

    pub fn with_recovery(
        config: ConnectorConfig,
        executor: E,
        cloner: C,
        recovery: Arc<RecoveryStore>,
    ) -> Self {
        Self {
            config,
            executor: Arc::new(executor),
            cloner: Arc::new(cloner),
            jobs: Arc::new(Mutex::new(HashMap::new())),
            recovery,
        }
    }

    pub async fn rehydrate(&self) -> Result<(), ConnectorError> {
        for record in self.recovery.cleanup_jobs() {
            let local = LocalJob {
                job: serde_json::from_value(record.job.clone())
                    .map_err(|e| ConnectorError::Malformed(e.to_string()))?,
                workspace_path: PathBuf::from(record.workspace_path.as_deref().unwrap_or("")),
                sandbox_id: record.sandbox_id.clone(),
                terminal: true,
                processed_actions: HashMap::new(),
            };
            if self.cleanup_local_job(&local).await {
                self.recovery
                    .remove_job(&record.job_id)
                    .map_err(ConnectorError::Execution)?;
            }
        }
        for record in self.recovery.active_jobs() {
            self.restore_job(&record.job_id).await?;
        }
        Ok(())
    }

    async fn restore_job(&self, job_id: &str) -> Result<bool, ConnectorError> {
        if self.jobs.lock().await.contains_key(job_id) {
            return Ok(true);
        }
        let Some(record) = self.recovery.job(job_id) else {
            return Ok(false);
        };
        if record.terminal {
            return Ok(false);
        }
        let workspace = record.workspace_path.as_ref().map(PathBuf::from);
        if workspace.as_ref().is_none_or(|path| !path.exists()) {
            self.handle_terminal(Terminal {
                job_id: job_id.to_string(),
                final_status: "failed".into(),
                instructions: "discard_local_copy".into(),
            })
            .await?;
            return Ok(false);
        }
        let workspace = workspace.expect("checked above");
        let job: Job = serde_json::from_value(record.job.clone())
            .map_err(|e| ConnectorError::Malformed(e.to_string()))?;
        let mut processed_actions = HashMap::new();
        for (action_id, receipt) in record.receipts {
            processed_actions.insert(
                action_id,
                CachedAction {
                    verb: receipt.verb,
                    args: receipt.args,
                    frame: receipt.frame.to_string(),
                },
            );
        }
        self.jobs.lock().await.insert(
            job_id.to_string(),
            LocalJob {
                job,
                workspace_path: workspace,
                sandbox_id: record.sandbox_id,
                terminal: false,
                processed_actions,
            },
        );
        Ok(true)
    }

    pub async fn run_forever(self: Arc<Self>) {
        let mut delay = 1;
        loop {
            match self.run_once().await {
                Ok(()) => delay = 1,
                Err(error) => {
                    warn!(error = %error, delay_seconds = delay, "connector session ended; reconnecting");
                    sleep(Duration::from_secs(delay)).await;
                    delay = (delay * 2).min(MAX_RECONNECT_SECONDS);
                }
            }
        }
    }

    pub async fn run_once(&self) -> Result<(), ConnectorError> {
        let client = Client::builder()
            .timeout(self.config.http_timeout)
            .build()
            .map_err(|e| ConnectorError::Execution(e.to_string()))?;
        let url = format!(
            "{}/v1/tenants/{}/jobs/next",
            self.config.dispatch_url.trim_end_matches('/'),
            self.config.tenant_id
        );
        let response = client
            .get(url)
            .bearer_auth(&self.config.token)
            .send()
            .await
            .map_err(|e| ConnectorError::Execution(e.to_string()))?;
        if response.status() == reqwest::StatusCode::UNAUTHORIZED
            || response.status() == reqwest::StatusCode::FORBIDDEN
        {
            return Err(ConnectorError::Execution(format!(
                "dispatch authentication failed: {}",
                response.status()
            )));
        }
        if !response.status().is_success() {
            return Err(ConnectorError::Execution(format!(
                "dispatch returned {}",
                response.status()
            )));
        }
        let body: Value = response
            .json()
            .await
            .map_err(|e| ConnectorError::Malformed(e.to_string()))?;
        let messages = body
            .get("messages")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                ConnectorError::Malformed("dispatch response missing messages array".into())
            })?;
        if messages.is_empty() {
            sleep(self.config.polling_interval).await;
            return Ok(());
        }
        let mut claim = body
            .get("claim")
            .cloned()
            .map(serde_json::from_value::<LeaseClaim>)
            .transpose()
            .map_err(|e| ConnectorError::Malformed(format!("invalid lease claim: {e}")))?;
        for message in messages {
            self.process_http_message(&client, message, &mut claim)
                .await?;
        }
        Ok(())
    }

    #[cfg(test)]
    async fn run_reconnect_attempts(&self, attempts: usize) -> Result<(), ConnectorError> {
        let mut delay = 1;
        let mut last_error = None;
        for attempt in 0..attempts {
            match self.run_once().await {
                Ok(()) => return Ok(()),
                Err(error) => {
                    last_error = Some(error);
                    if attempt + 1 < attempts {
                        sleep(Duration::from_millis(delay * 100)).await;
                        delay = (delay * 2).min(MAX_RECONNECT_SECONDS);
                    }
                }
            }
        }
        Err(last_error
            .unwrap_or_else(|| ConnectorError::Execution("no reconnect attempts requested".into())))
    }

    async fn process_http_message(
        &self,
        client: &Client,
        value: &Value,
        claim: &mut Option<LeaseClaim>,
    ) -> Result<(), ConnectorError> {
        let mut pending = vec![value.clone()];
        while let Some(value) = pending.pop() {
            let envelope = parse_envelope(&value)?;
            match envelope.kind.as_str() {
                "action" => {
                    let action: Action = serde_json::from_value(envelope.payload)
                        .map_err(|e| ConnectorError::Malformed(e.to_string()))?;
                    if claim.as_ref().is_some_and(|claim| {
                        claim.action_id != action.action_id || claim.job_id != action.job_id
                    }) {
                        return Err(ConnectorError::Malformed(
                            "claim does not match the action".into(),
                        ));
                    }
                    // A redelivered claim can outlive local cleanup after renewal
                    // denial. Never post a new result for work we no longer own.
                    if claim.is_some()
                        && action.verb != "create_sandbox"
                        && !self.restore_job(&action.job_id).await?
                    {
                        return Err(ConnectorError::LeaseExpired(action.job_id.clone()));
                    }
                    let lease_failed = Arc::new(AtomicBool::new(false));
                    let active_claim = claim
                        .as_ref()
                        .filter(|claim| claim.action_id == action.action_id);
                    let deadline = match active_claim.map(claim_deadline).transpose() {
                        Ok(deadline) => deadline.unwrap_or_else(|| {
                            crate::deadline::Deadline::new(Duration::from_secs(300))
                        }),
                        Err(error) => {
                            self.handle_terminal(Terminal {
                                job_id: action.job_id.clone(),
                                final_status: "failed".into(),
                                instructions: "discard_local_copy".into(),
                            })
                            .await?;
                            return Err(error);
                        }
                    };
                    let _heartbeat = active_claim.cloned().map(|claim| {
                        start_lease_heartbeat(
                            client.clone(),
                            self.config.clone(),
                            claim.clone(),
                            lease_failed.clone(),
                        )
                    });
                    let job_id = action.job_id.clone();
                    if action.verb == "create_sandbox" {
                        let args = action.args.as_object().ok_or_else(|| {
                            ConnectorError::Malformed(
                                "create_sandbox args must be an object".into(),
                            )
                        })?;
                        let repository = args
                            .get("repository")
                            .and_then(Value::as_object)
                            .ok_or_else(|| {
                                ConnectorError::Malformed(
                                    "create_sandbox args missing repository".into(),
                                )
                            })?;
                        let commit_sha = args
                            .get("commit_sha")
                            .and_then(Value::as_str)
                            .ok_or_else(|| {
                                ConnectorError::Malformed(
                                    "create_sandbox args missing commit_sha".into(),
                                )
                            })?;
                        let clone_url = repository
                            .get("clone_url")
                            .and_then(Value::as_str)
                            .ok_or_else(|| {
                                ConnectorError::Malformed(
                                    "create_sandbox repository missing clone_url".into(),
                                )
                            })?;
                        let name = repository
                            .get("name")
                            .and_then(Value::as_str)
                            .map(str::to_string);
                        let job = Job {
                            job_id: job_id.clone(),
                            repository: Repository {
                                clone_url: clone_url.into(),
                                name,
                            },
                            commit_sha: commit_sha.into(),
                            narrowed_location: NarrowedLocation {
                                file_path: "deploy/app.yaml".into(),
                                line_start: None,
                                line_end: None,
                            },
                            sandbox_profile: None,
                            lease_ttl_seconds: None,
                        };
                        let run_id =
                            args.get("run_id").and_then(Value::as_str).ok_or_else(|| {
                                ConnectorError::Malformed(
                                    "create_sandbox args missing run_id".into(),
                                )
                            })?;
                        let tenant_id =
                            args.get("tenant_id")
                                .and_then(Value::as_str)
                                .ok_or_else(|| {
                                    ConnectorError::Malformed(
                                        "create_sandbox args missing tenant_id".into(),
                                    )
                                })?;
                        let clone_cancelled = Arc::new(AtomicBool::new(false));
                        let job_result = if let Some(heartbeat) = &_heartbeat {
                            tokio::select! {
                                result = self.handle_job_until(job, run_id, tenant_id, deadline, clone_cancelled.clone()) => result,
                                _ = heartbeat.lost() => Err(ConnectorError::LeaseExpired(job_id.clone())),
                            }
                        } else {
                            self.handle_job_until(job, run_id, tenant_id, deadline, clone_cancelled)
                                .await
                        };
                        if job_result.is_err() {
                            self.handle_terminal(Terminal {
                                job_id: job_id.clone(),
                                final_status: "failed".into(),
                                instructions: "discard_local_copy".into(),
                            })
                            .await?;
                        }
                        job_result?;
                    }
                    let action_result = if let Some(heartbeat) = &_heartbeat {
                        tokio::select! {
                            result = self.handle_action_until(action, deadline) => result,
                            _ = heartbeat.lost() => Err(ConnectorError::LeaseExpired(job_id.clone())),
                        }
                    } else {
                        self.handle_action_until(action, deadline).await
                    };
                    if matches!(action_result, Err(ConnectorError::LeaseExpired(_))) {
                        self.handle_terminal(Terminal {
                            job_id: job_id.clone(),
                            final_status: "failed".into(),
                            instructions: "discard_local_copy".into(),
                        })
                        .await?;
                    }
                    let frames = action_result?;
                    let result = frames
                        .into_iter()
                        .find_map(|frame| serde_json::from_str::<Value>(&frame).ok())
                        .ok_or_else(|| {
                            ConnectorError::Malformed("action produced no result".into())
                        })?;
                    if lease_failed.load(Ordering::SeqCst) {
                        self.handle_terminal(Terminal {
                            job_id: job_id.clone(),
                            final_status: "failed".into(),
                            instructions: "discard_local_copy".into(),
                        })
                        .await?;
                        return Err(ConnectorError::LeaseExpired(job_id));
                    }
                    let url = format!(
                        "{}/v1/results",
                        self.config.dispatch_url.trim_end_matches('/')
                    );
                    let delivery = async {
                        let mut failures = 0;
                        loop {
                            let budget = deadline
                                .remaining()
                                .map_err(|e| ConnectorError::Timeout(e.to_string()))?;
                            let exchange = async {
                                let response = client
                                    .post(&url)
                                    .bearer_auth(&self.config.token)
                                    .json(&result)
                                    .timeout(budget.min(self.config.http_timeout))
                                    .send()
                                    .await
                                    .map_err(controller_http_error)?;
                                let status = response.status();
                                // Error responses need not have a JSON body.
                                if !status.is_success() {
                                    return Ok((status, Value::Null));
                                }
                                let body = response
                                    .json::<Value>()
                                    .await
                                    .map_err(controller_http_error)?;
                                Ok::<_, ConnectorError>((status, body))
                            };
                            let error = match exchange.await {
                                Ok((status, body)) if status.is_success() => {
                                    let next_claim = body
                                        .get("claim")
                                        .cloned()
                                        .map(serde_json::from_value::<LeaseClaim>)
                                        .transpose()
                                        .map_err(|e| {
                                            ConnectorError::Malformed(format!(
                                                "invalid lease claim: {e}"
                                            ))
                                        })?;
                                    let messages = body
                                        .get("messages")
                                        .and_then(Value::as_array)
                                        .ok_or_else(|| {
                                            ConnectorError::Malformed(
                                                "result response missing messages array".into(),
                                            )
                                        })?
                                        .clone();
                                    return Ok((next_claim, messages));
                                }
                                Ok((reqwest::StatusCode::CONFLICT, _)) => None,
                                Ok((status, _)) => {
                                    let error = ConnectorError::Execution(format!(
                                        "dispatch returned {status}"
                                    ));
                                    if !status.is_server_error() && status.as_u16() != 429 {
                                        return Err(error);
                                    }
                                    Some(error)
                                }
                                Err(error) => Some(error),
                            };
                            if let Some(error) = error {
                                failures += 1;
                                if failures >= 3 {
                                    return Err(error);
                                }
                            }
                            // Retry the cached result, never execute the action again.
                            let remaining = deadline
                                .remaining()
                                .map_err(|e| ConnectorError::Timeout(e.to_string()))?;
                            sleep(self.config.polling_interval.min(remaining)).await;
                        }
                    };
                    let delivered = if let Some(heartbeat) = &_heartbeat {
                        tokio::select! {
                            response = delivery => response,
                            _ = heartbeat.lost() => Err(ConnectorError::LeaseExpired(job_id.clone())),
                        }
                    } else {
                        delivery.await
                    };
                    let (next_claim, messages) = match delivered {
                        Ok(response) => response,
                        Err(error) => {
                            self.handle_terminal(Terminal {
                                job_id: job_id.clone(),
                                final_status: "failed".into(),
                                instructions: "discard_local_copy".into(),
                            })
                            .await?;
                            return Err(error);
                        }
                    };
                    *claim = next_claim;
                    if let (Some(heartbeat), Some(next_claim)) = (&_heartbeat, claim.as_mut()) {
                        let confirmed = heartbeat.confirmed.lock().await;
                        if confirmed.action_id == next_claim.action_id
                            && confirmed.claim_id == next_claim.claim_id
                            && confirmed.renewal_sequence > next_claim.renewal_sequence
                        {
                            *next_claim = confirmed.clone();
                        }
                    }
                    pending.extend(messages);
                }
                "terminal" => {
                    let terminal: Terminal = serde_json::from_value(envelope.payload)
                        .map_err(|e| ConnectorError::Malformed(e.to_string()))?;
                    self.handle_terminal(terminal).await?;
                }
                _ => return Err(ConnectorError::Unsupported(envelope.kind)),
            }
        }
        Ok(())
    }

    #[cfg(test)]
    async fn handle_job(
        &self,
        job: Job,
        run_id: &str,
        tenant_id: &str,
    ) -> Result<Vec<String>, ConnectorError> {
        self.handle_job_until(
            job,
            run_id,
            tenant_id,
            crate::deadline::Deadline::new(Duration::from_secs(180)),
            Arc::new(AtomicBool::new(false)),
        )
        .await
    }

    async fn handle_job_until(
        &self,
        job: Job,
        run_id: &str,
        tenant_id: &str,
        deadline: crate::deadline::Deadline,
        cancelled: Arc<AtomicBool>,
    ) -> Result<Vec<String>, ConnectorError> {
        validate_job(&job)?;
        let job_id = job.job_id.clone();
        {
            let jobs = self.jobs.lock().await;
            if jobs.contains_key(&job_id) {
                return Ok(Vec::new());
            }
        }
        if self.restore_job(&job_id).await? {
            return Ok(Vec::new());
        }
        if self
            .recovery
            .job(&job_id)
            .is_some_and(|record| record.terminal)
        {
            return Err(ConnectorError::Execution(
                "job cleanup is still pending".into(),
            ));
        }
        self.recovery
            .save_job(ConnectorJobRecord {
                job_id: job_id.clone(),
                tenant_id: tenant_id.to_string(),
                run_id: run_id.to_string(),
                job: serde_json::to_value(&job)
                    .map_err(|e| ConnectorError::Malformed(e.to_string()))?,
                workspace_path: None,
                sandbox_id: None,
                terminal: false,
                cleanup_failed: false,
                receipts: HashMap::new(),
                updated_at: Utc::now(),
            })
            .map_err(ConnectorError::Execution)?;
        let workspace_guard = self.cloner.clone_job(&job, deadline, cancelled).await?;
        let workspace_path = workspace_guard.path.clone();
        let mut record = self
            .recovery
            .job(&job_id)
            .expect("recovery record was just saved");
        record.workspace_path = Some(workspace_path.to_string_lossy().to_string());
        record.updated_at = Utc::now();
        self.recovery
            .save_job(record)
            .map_err(ConnectorError::Execution)?;
        let local = LocalJob {
            job,
            workspace_path,
            sandbox_id: None,
            terminal: false,
            processed_actions: HashMap::new(),
        };
        let mut jobs = self.jobs.lock().await;
        jobs.insert(job_id.clone(), local);
        workspace_guard.keep();
        drop(jobs);
        info!(job_id = %job_id, "connector accepted job");
        Ok(Vec::new())
    }

    #[cfg(test)]
    async fn handle_action(&self, action: Action) -> Result<Vec<String>, ConnectorError> {
        self.handle_action_until(
            action,
            crate::deadline::Deadline::new(Duration::from_secs(300)),
        )
        .await
    }

    async fn handle_action_until(
        &self,
        action: Action,
        deadline: crate::deadline::Deadline,
    ) -> Result<Vec<String>, ConnectorError> {
        validate_action(&action)?;
        if !self.jobs.lock().await.contains_key(&action.job_id) {
            let _ = self.restore_job(&action.job_id).await?;
        }
        let mut jobs = self.jobs.lock().await;
        let Some(local) = jobs.get_mut(&action.job_id) else {
            return Ok(vec![self.result_frame(
                &action,
                "failed",
                None,
                Some(ErrorBody {
                    code: "internal_error".into(),
                    message: "action references unknown job".into(),
                }),
            )]);
        };
        if local.terminal {
            return Ok(vec![self.result_frame(
                &action,
                "failed",
                None,
                Some(ErrorBody {
                    code: "job_lease_expired".into(),
                    message: "job is already terminal".into(),
                }),
            )]);
        }
        if let Some(cached) = local.processed_actions.get(&action.action_id) {
            if cached.verb != action.verb || cached.args != action.args {
                return Err(ConnectorError::Malformed(
                    "action_id replay payload mismatch".into(),
                ));
            }
            return Ok(vec![cached.frame.clone()]);
        }
        if action.verb == "create_sandbox" {
            let run_id = action
                .args
                .get("run_id")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    ConnectorError::Malformed("create_sandbox.run_id is required".into())
                })?;
            let mut record = self.recovery.job(&action.job_id).ok_or_else(|| {
                ConnectorError::Execution("create job has no recovery record".into())
            })?;
            if record.run_id != run_id {
                return Err(ConnectorError::Malformed(
                    "create_sandbox.run_id does not match the accepted job".into(),
                ));
            }
            let sandbox_id = crate::domain::ids::sandbox_id_from_run(run_id);
            record.sandbox_id = Some(sandbox_id.clone());
            record.updated_at = Utc::now();
            self.recovery
                .save_job(record)
                .map_err(ConnectorError::Execution)?;
            local.sandbox_id = Some(sandbox_id);
        }
        let result = match action.verb.as_str() {
            "create_sandbox" => match self
                .executor
                .execute(&local.job, local, &action, deadline)
                .await
            {
                Ok(value) => {
                    validate_response("create_sandbox", &value)?;
                    local.sandbox_id = value
                        .get("sandbox_id")
                        .and_then(Value::as_str)
                        .map(str::to_string);
                    if let Some(mut record) = self.recovery.job(&action.job_id) {
                        record.sandbox_id = local.sandbox_id.clone();
                        record.updated_at = Utc::now();
                        self.recovery
                            .save_job(record)
                            .map_err(ConnectorError::Execution)?;
                    }
                    Ok(value)
                }
                Err(error) => Err(error),
            },
            _ => {
                self.executor
                    .execute(&local.job, local, &action, deadline)
                    .await
            }
        };
        let frame = match result {
            Ok(value) => self.result_frame(&action, "ok", Some(value), None),
            Err(ConnectorError::Timeout(message)) => self.result_frame(
                &action,
                "timeout",
                None,
                Some(ErrorBody {
                    code: "controller_timeout".into(),
                    message,
                }),
            ),
            Err(ConnectorError::LeaseExpired(message)) => self.result_frame(
                &action,
                "timeout",
                None,
                Some(ErrorBody {
                    code: "job_lease_expired".into(),
                    message,
                }),
            ),
            Err(error) => self.result_frame(
                &action,
                "failed",
                None,
                Some(ErrorBody {
                    code: error_code(&error).into(),
                    message: error.to_string(),
                }),
            ),
        };
        local.processed_actions.insert(
            action.action_id.clone(),
            CachedAction {
                verb: action.verb.clone(),
                args: action.args.clone(),
                frame: frame.clone(),
            },
        );
        if let Some(mut record) = self.recovery.job(&action.job_id) {
            record.receipts.insert(
                action.action_id.clone(),
                ActionReceipt {
                    verb: action.verb.clone(),
                    args_hash: args_hash(&action.args),
                    args: action.args.clone(),
                    frame: serde_json::from_str(&frame)
                        .map_err(|e| ConnectorError::Malformed(e.to_string()))?,
                    completed_at: Utc::now(),
                },
            );
            record.updated_at = Utc::now();
            self.recovery
                .save_job(record)
                .map_err(ConnectorError::Execution)?;
        }
        Ok(vec![frame])
    }

    async fn handle_terminal(&self, terminal: Terminal) -> Result<Vec<String>, ConnectorError> {
        if !["fix_finalized", "escalated", "cancelled", "failed"]
            .contains(&terminal.final_status.as_str())
        {
            return Err(ConnectorError::Malformed(
                "invalid terminal final_status".into(),
            ));
        }
        let mut local = self.jobs.lock().await.remove(&terminal.job_id);
        if local.is_none() {
            if let Some(record) = self.recovery.job(&terminal.job_id) {
                local = Some(LocalJob {
                    job: serde_json::from_value(record.job)
                        .map_err(|e| ConnectorError::Malformed(e.to_string()))?,
                    workspace_path: PathBuf::from(record.workspace_path.unwrap_or_default()),
                    sandbox_id: record.sandbox_id,
                    terminal: true,
                    processed_actions: HashMap::new(),
                });
            }
        }
        let Some(mut local) = local else {
            return Ok(Vec::new());
        };
        local.terminal = true;
        if terminal.instructions == "discard_local_copy" {
            // Journal cleanup intent before the HTTP call or file deletion. A
            // restart must retry cleanup rather than restore terminal work.
            let mut record = self.recovery.job(&terminal.job_id).ok_or_else(|| {
                ConnectorError::Execution("terminal job has no recovery record".into())
            })?;
            record.terminal = true;
            record.cleanup_failed = true;
            record.updated_at = Utc::now();
            self.recovery
                .save_job(record)
                .map_err(ConnectorError::Execution)?;
            if self.cleanup_local_job(&local).await {
                self.recovery
                    .remove_job(&terminal.job_id)
                    .map_err(ConnectorError::Execution)?;
            }
        }
        Ok(Vec::new())
    }

    async fn cleanup_local_job(&self, local: &LocalJob) -> bool {
        let mut succeeded = true;
        if let Err(error) = self.executor.destroy(local).await {
            warn!(job_id = %local.job.job_id, error = %error, "sandbox cleanup failed; retaining recovery record");
            succeeded = false;
        }
        if !local.workspace_path.as_os_str().is_empty() {
            if let Err(error) = tokio::fs::remove_dir_all(&local.workspace_path).await {
                if error.kind() != std::io::ErrorKind::NotFound {
                    warn!(job_id = %local.job.job_id, error = %error, "workspace cleanup failed");
                    succeeded = false;
                }
            }
        }
        succeeded
    }

    fn result_frame(
        &self,
        action: &Action,
        status: &str,
        result: Option<Value>,
        error: Option<ErrorBody>,
    ) -> String {
        envelope(
            "result",
            Some(action.job_id.clone()),
            serde_json::to_value(ResultPayload {
                job_id: action.job_id.clone(),
                action_id: action.action_id.clone(),
                verb: action.verb.clone(),
                status: status.to_string(),
                result,
                error,
            })
            .unwrap_or_else(|_| json!({})),
        )
        .to_string()
    }
}

fn args_hash(args: &Value) -> String {
    let encoded = serde_json::to_vec(args).unwrap_or_default();
    Sha256::digest(encoded)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn envelope(kind: &str, job_id: Option<String>, payload: Value) -> Value {
    let mut object = Map::new();
    object.insert(
        "protocol_version".into(),
        Value::String(PROTOCOL_VERSION.into()),
    );
    object.insert(
        "message_id".into(),
        Value::String(Uuid::new_v4().to_string()),
    );
    if let Some(job_id) = job_id {
        object.insert("job_id".into(), Value::String(job_id));
    }
    object.insert("kind".into(), Value::String(kind.into()));
    object.insert("sent_at".into(), Value::String(Utc::now().to_rfc3339()));
    object.insert("payload".into(), payload);
    Value::Object(object)
}

fn parse_envelope(value: &Value) -> Result<Envelope, ConnectorError> {
    let envelope: Envelope = serde_json::from_value(value.clone())
        .map_err(|e| ConnectorError::Malformed(e.to_string()))?;
    if envelope.protocol_version != PROTOCOL_VERSION {
        return Err(ConnectorError::Malformed(
            "unsupported_protocol_version".into(),
        ));
    }
    Uuid::parse_str(&envelope.message_id)
        .map_err(|_| ConnectorError::Malformed("message_id must be a UUID".into()))?;
    if let Some(job_id) = &envelope.job_id {
        Uuid::parse_str(job_id)
            .map_err(|_| ConnectorError::Malformed("job_id must be a UUID".into()))?;
    }
    if !["job", "action", "result", "ack", "terminal", "error"].contains(&envelope.kind.as_str()) {
        return Err(ConnectorError::Unsupported(envelope.kind));
    }
    if envelope.sent_at.parse::<DateTime<Utc>>().is_err() {
        return Err(ConnectorError::Malformed("sent_at must be RFC3339".into()));
    }
    match envelope.kind.as_str() {
        "job" => {
            let job: Job = serde_json::from_value(envelope.payload.clone())
                .map_err(|e| ConnectorError::Malformed(e.to_string()))?;
            validate_job(&job)?;
            ensure_job_correlation(envelope.job_id.as_deref(), &job.job_id)?;
        }
        "action" => {
            let action: Action = serde_json::from_value(envelope.payload.clone())
                .map_err(|e| ConnectorError::Malformed(e.to_string()))?;
            validate_action(&action)?;
            ensure_job_correlation(envelope.job_id.as_deref(), &action.job_id)?;
        }
        "terminal" => {
            let terminal: Terminal = serde_json::from_value(envelope.payload.clone())
                .map_err(|e| ConnectorError::Malformed(e.to_string()))?;
            ensure_job_correlation(envelope.job_id.as_deref(), &terminal.job_id)?;
        }
        "ack" => {
            let ack: Value = envelope.payload.clone();
            if ack
                .get("acked_message_id")
                .and_then(Value::as_str)
                .and_then(|v| Uuid::parse_str(v).ok())
                .is_none()
            {
                return Err(ConnectorError::Malformed(
                    "acked_message_id must be a UUID".into(),
                ));
            }
        }
        "error" => {
            if envelope
                .payload
                .get("code")
                .and_then(Value::as_str)
                .is_none()
                || envelope
                    .payload
                    .get("message")
                    .and_then(Value::as_str)
                    .is_none()
            {
                return Err(ConnectorError::Malformed(
                    "error requires code and message".into(),
                ));
            }
        }
        "result" => {}
        _ => unreachable!(),
    }
    Ok(envelope)
}

fn validate_job(job: &Job) -> Result<(), ConnectorError> {
    Uuid::parse_str(&job.job_id)
        .map_err(|_| ConnectorError::Malformed("job_id must be a UUID".into()))?;
    if job.repository.clone_url.trim().is_empty() {
        return Err(ConnectorError::Malformed(
            "repository.clone_url is required".into(),
        ));
    }
    let clone_url = url::Url::parse(&job.repository.clone_url)
        .map_err(|_| ConnectorError::Malformed("repository.clone_url must be a URI".into()))?;
    if !clone_url.username().is_empty()
        || clone_url.password().is_some()
        || clone_url.query_pairs().any(|(key, _)| {
            ["token", "access_token", "auth", "password", "secret"].contains(&key.as_ref())
        })
    {
        return Err(ConnectorError::Malformed(
            "repository.clone_url must not contain credentials".into(),
        ));
    }
    if job.commit_sha.len() < 7
        || job.commit_sha.len() > 40
        || !job.commit_sha.chars().all(|c| c.is_ascii_hexdigit())
    {
        return Err(ConnectorError::Malformed(
            "commit_sha must be 7-40 hexadecimal characters".into(),
        ));
    }
    if job.narrowed_location.file_path.trim().is_empty() {
        return Err(ConnectorError::Malformed(
            "narrowed_location.file_path is required".into(),
        ));
    }
    Ok(())
}

fn ensure_job_correlation(
    envelope_job_id: Option<&str>,
    payload_job_id: &str,
) -> Result<(), ConnectorError> {
    if let Some(envelope_job_id) = envelope_job_id {
        if envelope_job_id != payload_job_id {
            return Err(ConnectorError::Malformed(
                "top-level job_id does not match payload job_id".into(),
            ));
        }
    }
    Ok(())
}

fn validate_action(action: &Action) -> Result<(), ConnectorError> {
    Uuid::parse_str(&action.job_id)
        .map_err(|_| ConnectorError::Malformed("action.job_id must be a UUID".into()))?;
    Uuid::parse_str(&action.action_id)
        .map_err(|_| ConnectorError::Malformed("action.action_id must be a UUID".into()))?;
    if !ALLOWED_VERBS.contains(&action.verb.as_str()) {
        return Err(ConnectorError::Unsupported(action.verb.clone()));
    }
    validate_action_args(&action.verb, &action.args)
}

fn object<'a>(value: &'a Value, name: &str) -> Result<&'a Map<String, Value>, ConnectorError> {
    value
        .as_object()
        .ok_or_else(|| ConnectorError::Malformed(format!("{name} must be an object")))
}

fn allowed_keys(
    value: &Map<String, Value>,
    allowed: &[&str],
    name: &str,
) -> Result<(), ConnectorError> {
    if let Some(key) = value.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(ConnectorError::Malformed(format!(
            "{name} contains unsupported field {key}"
        )));
    }
    Ok(())
}

fn required_string(
    value: &Map<String, Value>,
    key: &str,
    name: &str,
) -> Result<(), ConnectorError> {
    if value
        .get(key)
        .and_then(Value::as_str)
        .filter(|v| !v.trim().is_empty())
        .is_none()
    {
        return Err(ConnectorError::Malformed(format!(
            "{name}.{key} is required"
        )));
    }
    Ok(())
}

fn validate_action_args(verb: &str, args: &Value) -> Result<(), ConnectorError> {
    let map = object(args, "action.args")?;
    match verb {
        "create_sandbox" => {
            allowed_keys(
                map,
                &[
                    "run_id",
                    "tenant_id",
                    "repository",
                    "commit_sha",
                    "target_environment",
                    "timeout_minutes",
                    "secret_fixture_set",
                    "labels",
                ],
                verb,
            )?;
            for key in ["run_id", "tenant_id", "commit_sha"] {
                required_string(map, key, verb)?;
            }
            let repo = object(
                map.get("repository").ok_or_else(|| {
                    ConnectorError::Malformed("create_sandbox.repository is required".into())
                })?,
                "create_sandbox.repository",
            )?;
            allowed_keys(
                repo,
                &["owner", "name", "clone_url"],
                "create_sandbox.repository",
            )?;
            required_string(repo, "owner", "create_sandbox.repository")?;
            required_string(repo, "name", "create_sandbox.repository")?;
        }
        "deploy_revision" => {
            allowed_keys(
                map,
                &[
                    "repository_sha",
                    "workspace_path",
                    "manifests",
                    "patch",
                    "wait_seconds",
                    "deploy_timeout_seconds",
                ],
                verb,
            )?;
            required_string(map, "repository_sha", verb)?;
            let manifests = object(
                map.get("manifests").ok_or_else(|| {
                    ConnectorError::Malformed("deploy_revision.manifests is required".into())
                })?,
                "deploy_revision.manifests",
            )?;
            allowed_keys(
                manifests,
                &["type", "path", "chart", "values", "overlay", "release_name"],
                "deploy_revision.manifests",
            )?;
            required_string(manifests, "type", "deploy_revision.manifests")?;
            if !["yaml", "helm", "kustomize"]
                .contains(&manifests["type"].as_str().unwrap_or_default())
            {
                return Err(ConnectorError::Malformed(
                    "deploy_revision.manifests.type is invalid".into(),
                ));
            }
            if let Some(patch) = map.get("patch") {
                let patch = object(patch, "deploy_revision.patch")?;
                allowed_keys(patch, &["unified_diff", "files"], "deploy_revision.patch")?;
            }
        }
        "observe_failure" => {
            allowed_keys(map, &["timeout_seconds", "expected_signature_key"], verb)?;
        }
        "run_validation" => {
            allowed_keys(map, &["plan"], verb)?;
            let plan = object(
                map.get("plan").ok_or_else(|| {
                    ConnectorError::Malformed("run_validation.plan is required".into())
                })?,
                "run_validation.plan",
            )?;
            allowed_keys(
                plan,
                &["commands", "health_checks", "compare_to_signature_key"],
                "run_validation.plan",
            )?;
        }
        "finalize_result" => allowed_keys(map, &["notes", "require_patch"], verb)?,
        "destroy_sandbox" => allowed_keys(map, &["reason"], verb)?,
        _ => return Err(ConnectorError::Unsupported(verb.into())),
    }
    Ok(())
}

fn endpoint_suffix(verb: &str) -> Result<&'static str, ConnectorError> {
    match verb {
        "deploy_revision" => Ok("deploy"),
        "observe_failure" => Ok("observe"),
        "run_validation" => Ok("validate"),
        "finalize_result" => Ok("finalize"),
        "destroy_sandbox" => Ok("destroy"),
        _ => Err(ConnectorError::Unsupported(verb.into())),
    }
}

fn validate_response(verb: &str, value: &Value) -> Result<(), ConnectorError> {
    let map = object(value, "controller response")?;
    required_string(map, "sandbox_id", verb)?;
    match verb {
        "create_sandbox" => {
            for key in ["namespace", "status", "created_at", "expires_at"] {
                required_string(map, key, verb)?;
            }
            if !["ready", "creating", "failed"]
                .contains(&map["status"].as_str().unwrap_or_default())
            {
                return Err(ConnectorError::Malformed(
                    "create_sandbox response status is invalid".into(),
                ));
            }
        }
        "deploy_revision" => {
            for key in ["status", "deployed_at"] {
                required_string(map, key, verb)?;
            }
            if map
                .get("rendered_artifact_ids")
                .and_then(Value::as_array)
                .is_none()
                || map.get("fidelity").is_none()
            {
                return Err(ConnectorError::Malformed(
                    "deploy_revision response is incomplete".into(),
                ));
            }
            // rendered_files is optional (added in contracts-v1.1.0). When present it must be
            // an array of {path, content} objects; when absent, accept for backward compatibility.
            if let Some(rendered_files) = map.get("rendered_files") {
                let arr = rendered_files.as_array().ok_or_else(|| {
                    ConnectorError::Malformed(
                        "deploy_revision.rendered_files must be an array".into(),
                    )
                })?;
                for (i, item) in arr.iter().enumerate() {
                    let file = item.as_object().ok_or_else(|| {
                        ConnectorError::Malformed(format!(
                            "deploy_revision.rendered_files[{i}] must be an object"
                        ))
                    })?;
                    if file.get("path").and_then(Value::as_str).is_none()
                        || file.get("content").and_then(Value::as_str).is_none()
                    {
                        return Err(ConnectorError::Malformed(format!(
                            "deploy_revision.rendered_files[{i}] requires string path and content"
                        )));
                    }
                }
            }
        }
        "observe_failure" => {
            if map.get("signature").is_none()
                || map.get("artifact_ids").and_then(Value::as_array).is_none()
            {
                return Err(ConnectorError::Malformed(
                    "observe_failure response is incomplete".into(),
                ));
            }
        }
        "run_validation" => {
            if map.get("passed").and_then(Value::as_bool).is_none()
                || map.get("fail_closed").and_then(Value::as_bool).is_none()
                || map.get("checks").and_then(Value::as_array).is_none()
            {
                return Err(ConnectorError::Malformed(
                    "run_validation response is incomplete".into(),
                ));
            }
        }
        "finalize_result" => {
            for key in ["result_id", "status", "finalized_at", "record"] {
                if map.get(key).is_none() {
                    return Err(ConnectorError::Malformed(format!(
                        "finalize_result response missing {key}"
                    )));
                }
            }
        }
        "destroy_sandbox" => {
            for key in ["status", "destroyed_at"] {
                required_string(map, key, verb)?;
            }
        }
        _ => return Err(ConnectorError::Unsupported(verb.into())),
    }
    Ok(())
}

fn controller_http_error(error: reqwest::Error) -> ConnectorError {
    if error.is_timeout() {
        ConnectorError::Timeout("local controller request deadline exhausted".into())
    } else {
        ConnectorError::Execution(error.to_string())
    }
}

fn error_code(error: &ConnectorError) -> &'static str {
    match error {
        ConnectorError::Malformed(_) => "malformed_envelope",
        ConnectorError::Unsupported(_) => "internal_error",
        ConnectorError::Execution(_) => "internal_error",
        ConnectorError::Timeout(_) => "controller_timeout",
        ConnectorError::LeaseExpired(_) => "job_lease_expired",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn stalled_result_body_is_cancelled_on_lease_denial() {
        use axum::{
            body::{Body, Bytes},
            response::Response,
            routing::post,
            Router,
        };
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new()
            .route(
                "/v1/results",
                post(|| async {
                    Response::builder()
                        .header("content-type", "application/json")
                        .body(Body::from_stream(futures_util::stream::pending::<
                            Result<Bytes, std::convert::Infallible>,
                        >()))
                        .unwrap()
                }),
            )
            .route(
                "/v1/tenants/connector/jobs/{job}/lease",
                post(|| async { axum::http::StatusCode::CONFLICT }),
            );
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let directory = tempdir().unwrap();
        let workspace = directory.path().join("workspace");
        let mut settings = config();
        settings.dispatch_url = format!("http://{address}");
        let executor = FakeExecutor::default();
        let connector = Connector::with_cloner(
            settings,
            executor.clone(),
            FakeCloner {
                workspace: workspace.clone(),
            },
        );
        let job = job();
        let job_id = job.job_id.clone();
        connector.handle_job(job, "run", "connector").await.unwrap();
        let action_id = Uuid::new_v4().to_string();
        let now = Utc::now();
        let mut claim = Some(LeaseClaim {
            job_id: job_id.clone(),
            claim_id: Uuid::new_v4().to_string(),
            action_id: action_id.clone(),
            renewal_sequence: 0,
            lease_ttl_seconds: 3,
            lease_expires_at: now + chrono::Duration::seconds(3),
            action_deadline_at: now + chrono::Duration::seconds(200),
            job_deadline_at: now + chrono::Duration::seconds(300),
        });
        let message = action(&job_id, &action_id, "observe_failure", json!({}));
        let result = tokio::time::timeout(
            Duration::from_secs(2),
            connector.process_http_message(&Client::new(), &message, &mut claim),
        )
        .await
        .unwrap();
        assert!(matches!(result, Err(ConnectorError::LeaseExpired(_))));
        assert!(!workspace.exists());
        assert!(connector.recovery.job(&job_id).is_none());
        assert_eq!(*executor.destroyed.lock().await, 1);
        server.abort();
    }

    #[tokio::test]
    async fn committed_result_adopts_next_claim_before_delayed_response() {
        use axum::{http::StatusCode, routing::post, Json, Router};
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let directory = tempdir().unwrap();
        let workspace = directory.path().join("workspace");
        let mut settings = config();
        settings.dispatch_url = format!("http://{address}");
        let executor = FakeExecutor::default();
        let connector = Connector::with_cloner(
            settings,
            executor.clone(),
            FakeCloner {
                workspace: workspace.clone(),
            },
        );
        let job = job();
        let job_id = job.job_id.clone();
        connector.handle_job(job, "run", "connector").await.unwrap();
        let action_id = Uuid::new_v4().to_string();
        let next_id = Uuid::new_v4().to_string();
        let now = Utc::now();
        let initial = LeaseClaim {
            job_id: job_id.clone(),
            claim_id: Uuid::new_v4().to_string(),
            action_id: action_id.clone(),
            renewal_sequence: 0,
            lease_ttl_seconds: 3,
            lease_expires_at: now + chrono::Duration::seconds(3),
            action_deadline_at: now + chrono::Duration::seconds(200),
            job_deadline_at: now + chrono::Duration::seconds(300),
        };
        let next = serde_json::to_value(LeaseClaim {
            action_id: next_id.clone(),
            claim_id: Uuid::new_v4().to_string(),
            ..initial.clone()
        })
        .unwrap();
        let reply = next.clone();
        let updated = next.clone();
        let old_id = action_id.clone();
        let app = Router::new()
            .route(
                "/v1/results",
                post(move || {
                    let reply = reply.clone();
                    async move {
                        sleep(Duration::from_millis(2200)).await;
                        Json(json!({"messages":[], "claim":reply}))
                    }
                }),
            )
            .route(
                "/v1/tenants/connector/jobs/{job}/lease",
                post(move |Json(request): Json<Value>| {
                    let mut updated = updated.clone();
                    let old_id = old_id.clone();
                    async move {
                        if request["action_id"] == old_id {
                            (
                                StatusCode::CONFLICT,
                                Json(json!({"code":"action_completed", "claim":updated})),
                            )
                        } else {
                            assert_eq!(request["action_id"], updated["action_id"]);
                            assert_eq!(request["claim_id"], updated["claim_id"]);
                            updated["renewal_sequence"] = request["renewal_sequence"].clone();
                            updated["lease_expires_at"] =
                                json!((Utc::now() + chrono::Duration::seconds(3)).to_rfc3339());
                            (StatusCode::OK, Json(updated))
                        }
                    }
                }),
            );
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let mut claim = Some(initial);
        let message = action(&job_id, &action_id, "observe_failure", json!({}));
        tokio::time::timeout(
            Duration::from_secs(4),
            connector.process_http_message(&Client::new(), &message, &mut claim),
        )
        .await
        .unwrap()
        .unwrap();
        let confirmed = claim.unwrap();
        assert_eq!(confirmed.action_id, next_id);
        assert!(
            confirmed.renewal_sequence >= 1,
            "response must not reset an already renewed claim sequence"
        );
        assert!(workspace.exists());
        assert_eq!(*executor.destroyed.lock().await, 0);
        server.abort();
    }

    #[tokio::test]
    async fn failed_clone_preparation_is_cleaned_and_does_not_block_rehydration() {
        struct FailingCloner;
        #[async_trait]
        impl WorkspaceCloner for FailingCloner {
            async fn clone_job(
                &self,
                _job: &Job,
                _deadline: crate::deadline::Deadline,
                _cancelled: Arc<AtomicBool>,
            ) -> Result<CloneWorkspace, ConnectorError> {
                Err(ConnectorError::Timeout("fixture clone deadline".into()))
            }
        }
        let connector = Connector::with_cloner(config(), FakeExecutor::default(), FailingCloner);
        let job = job();
        let job_id = job.job_id.clone();
        let args = json!({"run_id":job_id,"tenant_id":"connector", "repository": {"owner":"fixture", "name":"fixture", "clone_url":job.repository.clone_url}, "commit_sha":job.commit_sha});
        let message = action(&job_id, &Uuid::new_v4().to_string(), "create_sandbox", args);
        let result = connector
            .process_http_message(&Client::new(), &message, &mut None)
            .await;
        assert!(matches!(result, Err(ConnectorError::Timeout(_))));
        assert!(connector.recovery.job(&job_id).is_none());
        // A process may crash after journaling preparation but before any clone result.
        let _ = connector.handle_job(job, "run", "connector").await;
        assert!(connector.recovery.job(&job_id).is_some());
        connector.rehydrate().await.unwrap();
        assert!(connector.recovery.job(&job_id).is_none());
    }

    #[tokio::test]
    async fn renewal_denial_cancels_execution_cleans_workspace_and_posts_no_result() {
        struct SlowExecutor(FakeExecutor);
        #[async_trait]
        impl ActionExecutor for SlowExecutor {
            async fn execute(
                &self,
                job: &Job,
                local: &LocalJob,
                action: &Action,
                deadline: crate::deadline::Deadline,
            ) -> Result<Value, ConnectorError> {
                sleep(Duration::from_secs(3)).await;
                self.0.execute(job, local, action, deadline).await
            }
            async fn destroy(&self, local: &LocalJob) -> Result<(), ConnectorError> {
                self.0.destroy(local).await
            }
        }
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut bytes = [0; 4096];
            let count = stream.read(&mut bytes).await.unwrap();
            let request = String::from_utf8_lossy(&bytes[..count]).to_string();
            stream
                .write_all(
                    b"HTTP/1.1 409 Conflict\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
                )
                .await
                .unwrap();
            assert!(
                tokio::time::timeout(Duration::from_millis(200), listener.accept())
                    .await
                    .is_err(),
                "unexpected result posted after denial"
            );
            request
        });
        let fake = FakeExecutor::default();
        let mut settings = config();
        settings.dispatch_url = format!("http://{address}");
        let directory = tempdir().unwrap();
        let workspace = directory.path().join("workspace");
        let connector = Connector::with_cloner(
            settings,
            SlowExecutor(fake.clone()),
            FakeCloner {
                workspace: workspace.clone(),
            },
        );
        let job = job();
        let job_id = job.job_id.clone();
        connector.handle_job(job, "run", "connector").await.unwrap();
        let action_id = Uuid::new_v4().to_string();
        let now = Utc::now();
        let mut claim = Some(LeaseClaim {
            job_id: job_id.clone(),
            claim_id: Uuid::new_v4().to_string(),
            action_id: action_id.clone(),
            renewal_sequence: 0,
            lease_ttl_seconds: 3,
            lease_expires_at: now + chrono::Duration::seconds(3),
            action_deadline_at: now + chrono::Duration::seconds(200),
            job_deadline_at: now + chrono::Duration::seconds(300),
        });
        let message = action(&job_id, &action_id, "observe_failure", json!({}));
        let result = tokio::time::timeout(
            Duration::from_secs(2),
            connector.process_http_message(&Client::new(), &message, &mut claim),
        )
        .await
        .unwrap();
        assert!(matches!(result, Err(ConnectorError::LeaseExpired(_))));
        assert!(fake.calls.lock().await.is_empty());
        assert_eq!(*fake.destroyed.lock().await, 1);
        assert!(!workspace.exists());
        assert!(connector.recovery.job(&job_id).is_none());
        // Dispatch can redeliver the still-unexpired claim after reconnect. Local
        // cleanup revoked this work; do not emit an unknown-job failure result.
        let replay = connector
            .process_http_message(&Client::new(), &message, &mut claim)
            .await;
        assert!(matches!(replay, Err(ConnectorError::LeaseExpired(_))));
        let request = server.await.unwrap();
        assert!(request.lines().next().unwrap().contains("/lease"));
    }

    #[tokio::test]
    async fn resumed_claim_stops_at_confirmed_expiry() {
        let now = Utc::now();
        let claim = LeaseClaim {
            job_id: Uuid::new_v4().to_string(),
            claim_id: Uuid::new_v4().to_string(),
            action_id: Uuid::new_v4().to_string(),
            renewal_sequence: 0,
            lease_ttl_seconds: 30,
            lease_expires_at: now + chrono::Duration::milliseconds(80),
            action_deadline_at: now + chrono::Duration::seconds(60),
            job_deadline_at: now + chrono::Duration::seconds(90),
        };
        let mut settings = config();
        settings.dispatch_url = "http://127.0.0.1:9".into();
        let failed = Arc::new(AtomicBool::new(false));
        let heartbeat = start_lease_heartbeat(Client::new(), settings, claim, failed.clone());
        tokio::time::timeout(Duration::from_millis(250), heartbeat.lost())
            .await
            .unwrap();
        assert!(failed.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn mismatched_claim_never_executes_as_legacy_unclaimed_work() {
        let directory = tempdir().unwrap();
        let workspace = directory.path().join("workspace");
        let executor = FakeExecutor::default();
        let connector =
            Connector::with_cloner(config(), executor.clone(), FakeCloner { workspace });
        let job_id = Uuid::new_v4().to_string();
        let action_id = Uuid::new_v4().to_string();
        let now = Utc::now();
        let mut claim = Some(LeaseClaim {
            job_id: Uuid::new_v4().to_string(),
            claim_id: Uuid::new_v4().to_string(),
            action_id: action_id.clone(),
            renewal_sequence: 0,
            lease_ttl_seconds: 30,
            lease_expires_at: now + chrono::Duration::seconds(30),
            action_deadline_at: now + chrono::Duration::seconds(200),
            job_deadline_at: now + chrono::Duration::seconds(300),
        });
        let message = action(&job_id, &action_id, "observe_failure", json!({}));
        let result = connector
            .process_http_message(&Client::new(), &message, &mut claim)
            .await;
        assert!(matches!(result, Err(ConnectorError::Malformed(_))));
        assert!(executor.calls.lock().await.is_empty());
    }

    #[test]
    fn lease_deadline_caps_execution_and_rejects_expired_claim() {
        let now = Utc::now();
        let claim = LeaseClaim {
            job_id: Uuid::new_v4().to_string(),
            claim_id: Uuid::new_v4().to_string(),
            action_id: Uuid::new_v4().to_string(),
            renewal_sequence: 0,
            lease_ttl_seconds: 30,
            action_deadline_at: now + chrono::Duration::seconds(140),
            job_deadline_at: now + chrono::Duration::seconds(150),
            lease_expires_at: now + chrono::Duration::seconds(20),
        };
        let deadline = claim_deadline(&claim).unwrap();
        assert!(deadline.cap(Duration::from_secs(300)).unwrap() <= Duration::from_secs(68));

        let expired = LeaseClaim {
            lease_expires_at: now - chrono::Duration::seconds(1),
            ..claim
        };
        assert!(matches!(
            claim_deadline(&expired),
            Err(ConnectorError::LeaseExpired(_))
        ));
    }

    #[test]
    fn clone_workspace_guard_removes_unjournaled_workspace() {
        let workspace = tempfile::tempdir().unwrap();
        let path = workspace.path().to_path_buf();
        drop(CloneWorkspace::owned(workspace));
        assert!(!path.exists());
    }

    #[test]
    fn controller_action_timeouts_are_capped_to_absolute_budget() {
        let mut deploy = json!({"manifests": {"path": "deploy"}});
        cap_action_timeouts("deploy_revision", &mut deploy, Duration::from_secs(12)).unwrap();
        assert_eq!(deploy["deploy_timeout_seconds"], 12);
        assert_eq!(deploy["wait_seconds"], 12);

        let mut validation =
            json!({"plan": {"commands": ["true"], "health_checks": [{"timeout_seconds": 90}]}});
        cap_action_timeouts("run_validation", &mut validation, Duration::from_secs(7)).unwrap();
        assert_eq!(validation["plan"]["commands"][0], "true");
        assert_eq!(validation["plan"]["health_checks"][0]["timeout_seconds"], 7);
    }

    #[derive(Clone, Default)]
    struct FakeExecutor {
        calls: Arc<Mutex<Vec<String>>>,
        destroyed: Arc<Mutex<usize>>,
        fail_destroy: Arc<AtomicBool>,
    }

    #[derive(Clone)]
    struct FakeCloner {
        workspace: PathBuf,
    }

    #[async_trait]
    impl WorkspaceCloner for FakeCloner {
        async fn clone_job(
            &self,
            _job: &Job,
            _deadline: crate::deadline::Deadline,
            _cancelled: Arc<AtomicBool>,
        ) -> Result<CloneWorkspace, ConnectorError> {
            tokio::fs::create_dir_all(&self.workspace)
                .await
                .map_err(|e| ConnectorError::Execution(e.to_string()))?;
            Ok(CloneWorkspace {
                path: self.workspace.clone(),
                guard: None,
            })
        }
    }

    #[async_trait]
    impl ActionExecutor for FakeExecutor {
        async fn execute(
            &self,
            _job: &Job,
            _local: &LocalJob,
            action: &Action,
            _deadline: crate::deadline::Deadline,
        ) -> Result<Value, ConnectorError> {
            self.calls.lock().await.push(action.action_id.clone());
            match action.verb.as_str() {
                "create_sandbox" => Ok(
                    json!({"sandbox_id":"sb-test","namespace":"ns","status":"ready","created_at":"2026-08-27T00:00:00Z","expires_at":"2026-08-27T01:00:00Z"}),
                ),
                "deploy_revision" => Ok(
                    json!({"sandbox_id":"sb-test","status":"deployed","rendered_artifact_ids":[],"fidelity":{},"deployed_at":"2026-08-27T00:00:00Z"}),
                ),
                "observe_failure" => {
                    Ok(json!({"sandbox_id":"sb-test","signature":{},"artifact_ids":[]}))
                }
                "run_validation" => Ok(
                    json!({"sandbox_id":"sb-test","passed":true,"fail_closed":false,"checks":[]}),
                ),
                "finalize_result" => Ok(
                    json!({"sandbox_id":"sb-test","result_id":"res-test","status":"finalized","finalized_at":"2026-08-27T00:00:00Z","record":{}}),
                ),
                "destroy_sandbox" => Ok(
                    json!({"sandbox_id":"sb-test","status":"destroyed","destroyed_at":"2026-08-27T00:00:00Z"}),
                ),
                _ => Err(ConnectorError::Unsupported(action.verb.clone())),
            }
        }
        async fn destroy(&self, _local: &LocalJob) -> Result<(), ConnectorError> {
            *self.destroyed.lock().await += 1;
            if self.fail_destroy.load(Ordering::SeqCst) {
                return Err(ConnectorError::Execution(
                    "synthetic destroy failure".into(),
                ));
            }
            Ok(())
        }
    }

    fn config() -> ConnectorConfig {
        ConnectorConfig {
            dispatch_url: "http://127.0.0.1:1".into(),
            token: "test-token".into(),
            controller_url: "http://127.0.0.1:8090".into(),
            tenant_id: "connector".into(),
            polling_interval: Duration::from_millis(1),
            http_timeout: Duration::from_secs(30),
        }
    }

    async fn http_fixture(responses: Vec<Value>) -> (String, tokio::task::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let mut seen = Vec::new();
            let mut responses = responses.into_iter();
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut buffer = vec![0; 65536];
                let size = stream.read(&mut buffer).await.unwrap();
                if size == 0 {
                    continue;
                }
                let request = String::from_utf8_lossy(&buffer[..size]).to_string();
                seen.push(request.clone());
                let body = responses
                    .next()
                    .unwrap_or_else(|| json!({"messages":[],"pending":false}))
                    .to_string();
                let reply = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body);
                stream.write_all(reply.as_bytes()).await.unwrap();
                if responses.len() == 0 {
                    break;
                }
            }
            seen
        });
        (format!("http://{}", address), server)
    }

    fn job() -> Job {
        Job {
            job_id: Uuid::new_v4().to_string(),
            repository: Repository {
                clone_url: "https://example.com/service.git".into(),
                name: Some("service".into()),
            },
            commit_sha: "0123456789abcdef0123456789abcdef01234567".into(),
            narrowed_location: NarrowedLocation {
                file_path: "deploy/app.yaml".into(),
                line_start: Some(12),
                line_end: None,
            },
            sandbox_profile: None,
            lease_ttl_seconds: Some(60),
        }
    }

    fn action(job_id: &str, action_id: &str, verb: &str, args: Value) -> Value {
        envelope(
            "action",
            Some(job_id.into()),
            json!({"job_id":job_id,"action_id":action_id,"verb":verb,"args":args}),
        )
    }

    fn action_from_envelope(value: Value) -> Action {
        let envelope = parse_envelope(&value).unwrap();
        serde_json::from_value(envelope.payload).unwrap()
    }

    fn terminal_from_envelope(value: Value) -> Terminal {
        let envelope = parse_envelope(&value).unwrap();
        serde_json::from_value(envelope.payload).unwrap()
    }

    fn create_action(job_id: &str, action_id: &str) -> Action {
        action_from_envelope(action(
            job_id,
            action_id,
            "create_sandbox",
            json!({
                "run_id":"recovery-run", "tenant_id":"connector",
                "repository":{"owner":"example","name":"service","clone_url":"https://example.com/service.git"},
                "commit_sha":"0123456789abcdef0123456789abcdef01234567"
            }),
        ))
    }

    #[tokio::test]
    async fn recovered_matching_action_replays_without_second_execution() {
        let dir = tempdir().unwrap();
        let workspace = dir.path().join("workspace");
        let durable = Arc::new(RecoveryStore::open(dir.path()).unwrap());
        let first = FakeExecutor::default();
        let connector = Connector::with_recovery(
            config(),
            first.clone(),
            FakeCloner {
                workspace: workspace.clone(),
            },
            durable,
        );
        let job = job();
        let action = create_action(&job.job_id, &Uuid::new_v4().to_string());
        connector
            .handle_job(job.clone(), "recovery-run", "connector")
            .await
            .unwrap();
        connector.handle_action(action.clone()).await.unwrap();
        assert_eq!(first.calls.lock().await.len(), 1);

        let second = FakeExecutor::default();
        let recovered = Connector::with_recovery(
            config(),
            second.clone(),
            FakeCloner { workspace },
            Arc::new(RecoveryStore::open(dir.path()).unwrap()),
        );
        recovered.rehydrate().await.unwrap();
        recovered.handle_action(action).await.unwrap();
        assert_eq!(
            second.calls.lock().await.len(),
            0,
            "recovered receipt must replay without executing"
        );
    }

    #[tokio::test]
    async fn recovered_replay_with_different_args_is_malformed() {
        let dir = tempdir().unwrap();
        let workspace = dir.path().join("workspace");
        let durable = Arc::new(RecoveryStore::open(dir.path()).unwrap());
        let connector = Connector::with_recovery(
            config(),
            FakeExecutor::default(),
            FakeCloner {
                workspace: workspace.clone(),
            },
            durable,
        );
        let job = job();
        let action_id = Uuid::new_v4().to_string();
        let first_action = create_action(&job.job_id, &action_id);
        connector
            .handle_job(job.clone(), "recovery-run", "connector")
            .await
            .unwrap();
        connector.handle_action(first_action).await.unwrap();
        let recovered = Connector::with_recovery(
            config(),
            FakeExecutor::default(),
            FakeCloner { workspace },
            Arc::new(RecoveryStore::open(dir.path()).unwrap()),
        );
        recovered.rehydrate().await.unwrap();
        let mismatch = action_from_envelope(action(
            &job.job_id,
            &action_id,
            "create_sandbox",
            json!({"run_id":"different","tenant_id":"connector","repository":{"owner":"example","name":"service","clone_url":"https://example.com/service.git"},"commit_sha":"0123456789abcdef0123456789abcdef01234567"}),
        ));
        assert!(matches!(
            recovered.handle_action(mismatch).await,
            Err(ConnectorError::Malformed(_))
        ));
    }

    #[tokio::test]
    async fn recovered_terminal_cleans_workspace_without_memory_state() {
        let dir = tempdir().unwrap();
        let workspace = dir.path().join("workspace");
        let durable = Arc::new(RecoveryStore::open(dir.path()).unwrap());
        let connector = Connector::with_recovery(
            config(),
            FakeExecutor::default(),
            FakeCloner {
                workspace: workspace.clone(),
            },
            durable,
        );
        let job = job();
        let create = create_action(&job.job_id, &Uuid::new_v4().to_string());
        connector
            .handle_job(job.clone(), "recovery-run", "connector")
            .await
            .unwrap();
        connector.handle_action(create).await.unwrap();
        let second = FakeExecutor::default();
        let recovered = Connector::with_recovery(
            config(),
            second.clone(),
            FakeCloner {
                workspace: workspace.clone(),
            },
            Arc::new(RecoveryStore::open(dir.path()).unwrap()),
        );
        recovered.handle_terminal(terminal_from_envelope(envelope("terminal", Some(job.job_id.clone()), json!({"job_id":job.job_id,"final_status":"fix_finalized","instructions":"discard_local_copy"})))).await.unwrap();
        assert_eq!(*second.destroyed.lock().await, 1);
        assert!(!workspace.exists());
    }

    #[test]
    fn rejects_credentialed_repository_urls() {
        for clone_url in [
            "https://user:secret@example.com/service.git",
            "https://example.com/service.git?access_token=secret",
        ] {
            let mut candidate = job();
            candidate.repository.clone_url = clone_url.into();
            let error = validate_job(&candidate).unwrap_err();
            assert!(error.to_string().contains("must not contain credentials"));
        }
    }

    #[test]
    fn rejects_unknown_verbs_and_extra_action_fields() {
        let id = Uuid::new_v4().to_string();
        let action = json!({"protocol_version":"1.0","message_id":Uuid::new_v4().to_string(),"kind":"action","sent_at":Utc::now().to_rfc3339(),"payload":{"job_id":id,"action_id":Uuid::new_v4().to_string(),"verb":"shell","args":{},"extra":true}});
        assert!(parse_envelope(&action).is_err());
    }

    #[tokio::test]
    async fn duplicate_action_id_returns_cached_result_without_second_execution() {
        let fake = FakeExecutor::default();
        let connector = Connector::new(config(), fake.clone());
        let job = job();
        let job_id = job.job_id.clone();
        let dir = tempdir().unwrap();
        connector.jobs.lock().await.insert(
            job_id.clone(),
            LocalJob {
                job,
                workspace_path: dir.path().to_path_buf(),
                sandbox_id: Some("sb-test".into()),
                terminal: false,
                processed_actions: HashMap::new(),
            },
        );
        let action_id = Uuid::new_v4().to_string();
        let frame = action(&job_id, &action_id, "observe_failure", json!({}));
        let first = connector
            .handle_action(action_from_envelope(frame.clone()))
            .await
            .unwrap();
        let second = connector
            .handle_action(action_from_envelope(frame))
            .await
            .unwrap();
        assert_eq!(fake.calls.lock().await.len(), 1);
        assert_eq!(first[0], second[0]);
    }

    #[tokio::test]
    async fn replayed_action_id_with_different_payload_is_rejected() {
        let fake = FakeExecutor::default();
        let connector = Connector::new(config(), fake.clone());
        let job = job();
        let job_id = job.job_id.clone();
        let dir = tempdir().unwrap();
        connector.jobs.lock().await.insert(
            job_id.clone(),
            LocalJob {
                job,
                workspace_path: dir.path().to_path_buf(),
                sandbox_id: Some("sb-test".into()),
                terminal: false,
                processed_actions: HashMap::new(),
            },
        );
        let action_id = Uuid::new_v4().to_string();
        let first = connector
            .handle_action(action_from_envelope(action(
                &job_id,
                &action_id,
                "observe_failure",
                json!({}),
            )))
            .await
            .unwrap();
        let second = connector
            .handle_action(action_from_envelope(action(
                &job_id,
                &action_id,
                "observe_failure",
                json!({"timeout_seconds": 30}),
            )))
            .await
            .unwrap_err();
        assert_eq!(fake.calls.lock().await.len(), 1);
        assert_eq!(first.len(), 1);
        assert!(matches!(second, ConnectorError::Malformed(_)));
    }

    #[tokio::test]
    async fn terminal_discards_sandbox_and_workspace() {
        let fake = FakeExecutor::default();
        let job = job();
        let job_id = job.job_id.clone();
        let dir = tempdir().unwrap();
        let workspace = dir.path().join("workspace");
        let connector = Connector::with_cloner(
            config(),
            fake.clone(),
            FakeCloner {
                workspace: workspace.clone(),
            },
        );
        connector
            .handle_job(job, "recovery-run", "connector")
            .await
            .unwrap();
        connector
            .jobs
            .lock()
            .await
            .get_mut(&job_id)
            .unwrap()
            .sandbox_id = Some("sb-test".into());
        let terminal = envelope(
            "terminal",
            Some(job_id.clone()),
            json!({"job_id":job_id,"final_status":"failed","instructions":"discard_local_copy"}),
        );
        connector
            .handle_terminal(terminal_from_envelope(terminal))
            .await
            .unwrap();
        assert_eq!(*fake.destroyed.lock().await, 1);
        assert!(!workspace.exists());
    }

    #[tokio::test]
    async fn http_empty_queue_is_safe() {
        let connector = Connector::new(config(), FakeExecutor::default());
        let error = connector.run_once().await.unwrap_err();
        assert!(matches!(error, ConnectorError::Execution(_)));
    }

    #[tokio::test]
    async fn http_empty_queue_is_polled_again_by_run_forever() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let mut requests = Vec::new();
            while requests.len() < 2 {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut buffer = [0; 4096];
                let size = stream.read(&mut buffer).await.unwrap();
                requests.push(String::from_utf8_lossy(&buffer[..size]).to_string());
                let body = r#"{"messages":[],"pending":false}"#;
                let reply = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body);
                stream.write_all(reply.as_bytes()).await.unwrap();
            }
            requests
        });
        let mut cfg = config();
        cfg.dispatch_url = format!("http://{}", address);
        cfg.polling_interval = Duration::from_millis(1);
        let connector = Arc::new(Connector::new(cfg, FakeExecutor::default()));
        let running = tokio::spawn(connector.clone().run_forever());
        let requests = tokio::time::timeout(Duration::from_secs(3), server)
            .await
            .unwrap()
            .unwrap();
        running.abort();
        assert_eq!(requests.len(), 2);
        assert!(requests
            .iter()
            .all(|request| request.starts_with("GET /v1/tenants/connector/jobs/next")));
    }

    #[tokio::test]
    async fn http_action_chain_posts_results_and_cleans_up_terminal() {
        let fake = FakeExecutor::default();
        let dir = tempdir().unwrap();
        let workspace = dir.path().join("workspace");
        let job = job();
        let job_id = job.job_id.clone();
        let create = envelope(
            "action",
            Some(job_id.clone()),
            json!({"job_id":job_id,"action_id":Uuid::new_v4().to_string(),"verb":"create_sandbox","args":{"run_id":"run-1","tenant_id":"connector","repository":{"owner":"example","name":"service","clone_url":"https://example.com/service.git"},"commit_sha":job.commit_sha}}),
        );
        let deploy = envelope(
            "action",
            Some(job_id.clone()),
            json!({"job_id":job_id,"action_id":Uuid::new_v4().to_string(),"verb":"deploy_revision","args":{"repository_sha":job.commit_sha,"manifests":{"type":"yaml","path":"deploy/app.yaml"}}}),
        );
        let observe = envelope(
            "action",
            Some(job_id.clone()),
            json!({"job_id":job_id,"action_id":Uuid::new_v4().to_string(),"verb":"observe_failure","args":{"timeout_seconds":1}}),
        );
        let patch_deploy = envelope(
            "action",
            Some(job_id.clone()),
            json!({"job_id":job_id,"action_id":Uuid::new_v4().to_string(),"verb":"deploy_revision","args":{"repository_sha":job.commit_sha,"manifests":{"type":"yaml","path":"deploy/app.yaml"},"patch":{"files":[]}}}),
        );
        let validation = envelope(
            "action",
            Some(job_id.clone()),
            json!({"job_id":job_id,"action_id":Uuid::new_v4().to_string(),"verb":"run_validation","args":{"plan":{"commands":[],"health_checks":[]}}}),
        );
        let finalize = envelope(
            "action",
            Some(job_id.clone()),
            json!({"job_id":job_id,"action_id":Uuid::new_v4().to_string(),"verb":"finalize_result","args":{"notes":"validated","require_patch":true}}),
        );
        let terminal = envelope(
            "terminal",
            Some(job_id.clone()),
            json!({"job_id":job_id,"final_status":"fix_finalized","instructions":"discard_local_copy"}),
        );
        let (url, server) = http_fixture(vec![
            json!({"messages":[create],"pending":true}),
            json!({"messages":[deploy],"idempotent_replay":false}),
            json!({"messages":[observe],"idempotent_replay":false}),
            json!({"messages":[patch_deploy],"idempotent_replay":false}),
            json!({"messages":[validation],"idempotent_replay":false}),
            json!({"messages":[finalize],"idempotent_replay":false}),
            json!({"messages":[terminal],"idempotent_replay":false}),
        ])
        .await;
        let mut cfg = config();
        cfg.dispatch_url = url;
        let connector = Connector::with_cloner(
            cfg,
            fake.clone(),
            FakeCloner {
                workspace: workspace.clone(),
            },
        );
        connector.run_once().await.unwrap();
        let requests = server.await.unwrap();
        assert_eq!(requests.len(), 7);
        assert!(requests[0].starts_with("GET /v1/tenants/connector/jobs/next"));
        assert!(requests[1].starts_with("POST /v1/results"));
        assert!(requests[1].contains("Bearer test-token"));
        assert!(requests[2].contains("POST /v1/results"));
        assert_eq!(*fake.destroyed.lock().await, 1);
        assert!(!workspace.exists());
    }

    #[tokio::test]
    async fn http_unknown_action_posts_attributable_failed_result() {
        let fake = FakeExecutor::default();
        let job_id = Uuid::new_v4().to_string();
        let action_id = Uuid::new_v4().to_string();
        let unknown_action = action(
            &job_id,
            &action_id,
            "deploy_revision",
            json!({
                "repository_sha": "0123456789abcdef0123456789abcdef01234567",
                "manifests": {"type": "yaml", "path": "deploy/app.yaml"}
            }),
        );
        let (url, server) = http_fixture(vec![
            json!({"messages":[unknown_action],"pending":true}),
            json!({"messages":[],"idempotent_replay":false}),
        ])
        .await;
        let mut cfg = config();
        cfg.dispatch_url = url;
        Connector::new(cfg, fake).run_once().await.unwrap();

        let requests = server.await.unwrap();
        assert_eq!(requests.len(), 2);
        let posted = requests[1]
            .split("\r\n\r\n")
            .nth(1)
            .expect("POST request body must be present");
        let result: Value = serde_json::from_str(posted).unwrap();
        assert_eq!(result["kind"], "result");
        assert_eq!(result["job_id"], job_id);
        assert_eq!(result["payload"]["job_id"], job_id);
        assert_eq!(result["payload"]["action_id"], action_id);
        assert_eq!(result["payload"]["verb"], "deploy_revision");
        assert_eq!(result["payload"]["status"], "failed");
        assert_eq!(result["payload"]["error"]["code"], "internal_error");
    }

    #[tokio::test]
    async fn http_auth_and_malformed_responses_fail_safely() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = [0; 4096];
            let _ = stream.read(&mut buf).await.unwrap();
            let body = "not-json";
            let reply = format!(
                "HTTP/1.1 401 Unauthorized\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(reply.as_bytes()).await.unwrap();
        });
        let mut cfg = config();
        cfg.dispatch_url = format!("http://{}", address);
        cfg.token = "bad".into();
        let error = Connector::new(cfg, FakeExecutor::default())
            .run_once()
            .await
            .unwrap_err();
        assert!(error.to_string().contains("authentication"));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn http_escalation_terminal_also_cleans_up() {
        let fake = FakeExecutor::default();
        let dir = tempdir().unwrap();
        let workspace = dir.path().join("workspace");
        let job = job();
        let job_id = job.job_id.clone();
        let create = envelope(
            "action",
            Some(job_id.clone()),
            json!({"job_id":job_id,"action_id":Uuid::new_v4().to_string(),"verb":"create_sandbox","args":{"run_id":"run-2","tenant_id":"connector","repository":{"owner":"example","name":"service","clone_url":"https://example.com/service.git"},"commit_sha":job.commit_sha}}),
        );
        let terminal = envelope(
            "terminal",
            Some(job_id.clone()),
            json!({"job_id":job_id,"final_status":"escalated","instructions":"discard_local_copy"}),
        );
        let (url, server) = http_fixture(vec![
            json!({"messages":[create],"pending":true}),
            json!({"messages":[terminal],"idempotent_replay":false}),
        ])
        .await;
        let mut cfg = config();
        cfg.dispatch_url = url;
        let connector = Connector::with_cloner(
            cfg,
            fake.clone(),
            FakeCloner {
                workspace: workspace.clone(),
            },
        );
        connector.run_once().await.unwrap();
        let requests = server.await.unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(*fake.destroyed.lock().await, 1);
        assert!(!workspace.exists());
    }

    #[tokio::test]
    async fn http_forbidden_and_malformed_success_bodies_are_rejected() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = [0; 4096];
            let _ = stream.read(&mut buf).await.unwrap();
            let body = "{}";
            let reply = format!(
                "HTTP/1.1 403 Forbidden\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(reply.as_bytes()).await.unwrap();
        });
        let mut cfg = config();
        cfg.dispatch_url = format!("http://{}", address);
        let error = Connector::new(cfg, FakeExecutor::default())
            .run_once()
            .await
            .unwrap_err();
        assert!(error.to_string().contains("authentication"));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn http_dispatch_failure_is_bounded_by_reconnect_attempts() {
        let connector = Connector::new(config(), FakeExecutor::default());
        let started = std::time::Instant::now();
        let result = connector.run_reconnect_attempts(2).await;
        assert!(result.is_err());
        assert!(started.elapsed() >= Duration::from_millis(100));
    }

    #[tokio::test]
    async fn http_malformed_response_is_rejected() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = [0; 4096];
            let _ = stream.read(&mut buf).await.unwrap();
            let body = "{}";
            let reply = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(reply.as_bytes()).await.unwrap();
        });
        let mut cfg = config();
        cfg.dispatch_url = format!("http://{}", address);
        let error = Connector::new(cfg, FakeExecutor::default())
            .run_once()
            .await
            .unwrap_err();
        assert!(matches!(error, ConnectorError::Malformed(_)));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn http_request_timeout_is_enforced() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = [0; 4096];
            let _ = stream.read(&mut buf).await.unwrap();
            sleep(Duration::from_millis(200)).await;
        });
        let mut cfg = config();
        cfg.dispatch_url = format!("http://{}", address);
        cfg.http_timeout = Duration::from_millis(25);
        let started = std::time::Instant::now();
        let error = Connector::new(cfg, FakeExecutor::default())
            .run_once()
            .await
            .unwrap_err();
        assert!(matches!(error, ConnectorError::Execution(_)));
        assert!(started.elapsed() < Duration::from_millis(150));
        server.await.unwrap();
    }
    async fn check_result_delivery_cleanup(mode: &str, fail_cleanup: bool) {
        use axum::{
            body::{Body, Bytes},
            response::Response,
            routing::post,
            Router,
        };
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let terminal_job = Arc::new(Mutex::new(String::new()));
        let route_attempts = attempts.clone();
        let route_requests = requests.clone();
        let route_job = terminal_job.clone();
        let route_mode = mode.to_string();
        let app = Router::new().route("/v1/results", post(move |axum::Json(body): axum::Json<Value>| {
            let attempts = route_attempts.clone();
            let requests = route_requests.clone();
            let job = route_job.clone();
            let mode = route_mode.clone();
            async move {
                requests.lock().await.push(body);
                let attempt = attempts.fetch_add(1, Ordering::SeqCst);
                if mode == "retry" && attempt > 0 {
                    let job_id = job.lock().await.clone();
                    return Response::builder().header("content-type", "application/json")
                        .body(Body::from(json!({"messages":[envelope("terminal", Some(job_id.clone()), json!({"job_id":job_id,"final_status":"failed","instructions":"discard_local_copy"}))],"idempotent_replay":true}).to_string())).unwrap();
                }
                if mode == "budget" {
                    return Response::builder().status(409).header("content-type", "application/json")
                        .body(Body::from("{}" )).unwrap();
                }
                Response::builder().header("content-type", "application/json")
                    .body(Body::from_stream(futures_util::stream::pending::<Result<Bytes, std::convert::Infallible>>())).unwrap()
            }
        }));
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let directory = tempdir().unwrap();
        let workspace = directory.path().join("workspace");
        let journal = directory.path().join("recovery");
        let mut settings = config();
        settings.dispatch_url = format!("http://{address}");
        settings.http_timeout = Duration::from_millis(30);
        let executor = FakeExecutor::default();
        executor.fail_destroy.store(fail_cleanup, Ordering::SeqCst);
        let connector = Connector::with_recovery(
            settings.clone(),
            executor.clone(),
            FakeCloner {
                workspace: workspace.clone(),
            },
            Arc::new(RecoveryStore::open(&journal).unwrap()),
        );
        let job = job();
        let job_id = job.job_id.clone();
        *terminal_job.lock().await = job_id.clone();
        connector.handle_job(job, "run", "connector").await.unwrap();
        let action_id = Uuid::new_v4().to_string();
        let now = Utc::now();
        let action_budget = if mode == "budget" {
            chrono::Duration::milliseconds(72_150)
        } else {
            chrono::Duration::seconds(200)
        };
        let mut claim = Some(LeaseClaim {
            job_id: job_id.clone(),
            claim_id: Uuid::new_v4().to_string(),
            action_id: action_id.clone(),
            renewal_sequence: 0,
            lease_ttl_seconds: 30,
            lease_expires_at: now + chrono::Duration::seconds(30),
            action_deadline_at: now + action_budget,
            job_deadline_at: now + chrono::Duration::seconds(300),
        });
        let message = action(&job_id, &action_id, "observe_failure", json!({}));
        let started = std::time::Instant::now();
        let result = tokio::time::timeout(
            Duration::from_secs(2),
            connector.process_http_message(&Client::new(), &message, &mut claim),
        )
        .await
        .unwrap();
        server.abort();
        assert_eq!(result.is_ok(), mode == "retry");
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(!workspace.exists(), "delivery failure abandoned workspace");
        assert_eq!(*executor.destroyed.lock().await, 1);
        let delivered = requests.lock().await;
        assert!(delivered.len() > 1, "transient failure was not retried");
        assert!(
            delivered.iter().all(|body| body == &delivered[0]),
            "retry changed cached result"
        );
        assert_eq!(
            executor.calls.lock().await.len(),
            1,
            "retry reexecuted action"
        );
        if fail_cleanup {
            let record = connector.recovery.job(&job_id).unwrap();
            assert!(record.terminal && record.cleanup_failed);
            executor.fail_destroy.store(false, Ordering::SeqCst);
            let restarted = Connector::with_recovery(
                settings,
                executor.clone(),
                FakeCloner { workspace },
                Arc::new(RecoveryStore::open(&journal).unwrap()),
            );
            restarted.rehydrate().await.unwrap();
            assert!(restarted.recovery.job(&job_id).is_none());
            assert_eq!(*executor.destroyed.lock().await, 2);
        } else {
            assert!(connector.recovery.job(&job_id).is_none());
        }
    }

    #[tokio::test]
    async fn result_delivery_timeout_cleans_workspace_and_journal() {
        check_result_delivery_cleanup("timeout", false).await;
    }

    #[tokio::test]
    async fn result_delivery_retries_cached_result_and_recovers_terminal() {
        check_result_delivery_cleanup("retry", false).await;
    }

    #[tokio::test]
    async fn result_delivery_processing_conflicts_end_at_budget_and_clean_up() {
        check_result_delivery_cleanup("budget", false).await;
    }

    #[tokio::test]
    async fn result_delivery_failed_cleanup_is_retried_after_restart() {
        check_result_delivery_cleanup("timeout", true).await;
    }
}
