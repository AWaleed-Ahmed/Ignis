use std::collections::HashMap;
use std::env;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use tokio::sync::Mutex;
use tokio::time::sleep;
use tracing::{info, warn};
use uuid::Uuid;

use crate::gitclone::clone_at_sha;

const PROTOCOL_VERSION: &str = "1.0";
const MAX_RECONNECT_SECONDS: u64 = 30;
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

#[derive(Debug, Clone, Serialize)]
struct ErrorPayload {
    code: String,
    message: String,
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
    last_activity: DateTime<Utc>,
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
    #[error("lease expired for job {0}")]
    LeaseExpired(String),
}

#[async_trait]
pub trait WorkspaceCloner: Send + Sync {
    async fn clone_job(&self, job: &Job) -> Result<PathBuf, ConnectorError>;
}

#[derive(Clone, Default)]
pub struct GitWorkspaceCloner;

#[async_trait]
impl WorkspaceCloner for GitWorkspaceCloner {
    async fn clone_job(&self, job: &Job) -> Result<PathBuf, ConnectorError> {
        let clone_url = job.repository.clone_url.clone();
        let commit = job.commit_sha.clone();
        tokio::task::spawn_blocking(move || clone_at_sha(&clone_url, &commit))
            .await
            .map_err(|e| ConnectorError::Execution(e.to_string()))?
            .map_err(|e| ConnectorError::Execution(e.to_string()))
    }
}

#[async_trait]
pub trait ActionExecutor: Send + Sync {
    async fn execute(
        &self,
        job: &Job,
        local: &LocalJob,
        action: &Action,
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
            client: Client::new(),
            base_url: base_url.into().trim_end_matches('/').to_string(),
        }
    }

    async fn post(&self, path: &str, body: Value) -> Result<Value, ConnectorError> {
        let response = self
            .client
            .post(format!("{}{}", self.base_url, path))
            .json(&body)
            .send()
            .await
            .map_err(|e| ConnectorError::Execution(e.to_string()))?;
        let status = response.status();
        let payload: Value = response
            .json()
            .await
            .map_err(|e| ConnectorError::Execution(e.to_string()))?;
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
    ) -> Result<Value, ConnectorError> {
        validate_action_args(&action.verb, &action.args)?;
        let sandbox_id = local.sandbox_id.as_deref();
        if action.verb != "create_sandbox" && sandbox_id.is_none() {
            return Err(ConnectorError::Execution(
                "sandbox_id is not available".into(),
            ));
        }
        let body = match action.verb.as_str() {
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
        let result = self.post(&path, body).await?;
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
            .post(&path, json!({"reason": "connector_terminal"}))
            .await?;
        validate_response("destroy_sandbox", &result)
    }
}

pub struct Connector<E = ControllerExecutor, C = GitWorkspaceCloner> {
    config: ConnectorConfig,
    executor: Arc<E>,
    cloner: Arc<C>,
    jobs: Arc<Mutex<HashMap<String, LocalJob>>>,
}

impl Connector<ControllerExecutor, GitWorkspaceCloner> {
    pub fn from_config(config: ConnectorConfig) -> Self {
        let executor = ControllerExecutor::new(config.controller_url.clone());
        Self::new(config, executor)
    }
}

impl<E> Connector<E, GitWorkspaceCloner>
where
    E: ActionExecutor + 'static,
{
    pub fn new(config: ConnectorConfig, executor: E) -> Self {
        Self::with_cloner(config, executor, GitWorkspaceCloner)
    }
}

impl<E, C> Connector<E, C>
where
    E: ActionExecutor + 'static,
    C: WorkspaceCloner + 'static,
{
    pub fn with_cloner(config: ConnectorConfig, executor: E, cloner: C) -> Self {
        Self {
            config,
            executor: Arc::new(executor),
            cloner: Arc::new(cloner),
            jobs: Arc::new(Mutex::new(HashMap::new())),
        }
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
        let client = Client::new();
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
        for message in messages {
            self.process_http_message(&client, message).await?;
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
    ) -> Result<(), ConnectorError> {
        let mut pending = vec![value.clone()];
        while let Some(value) = pending.pop() {
            let envelope = parse_envelope(&value)?;
            match envelope.kind.as_str() {
                "action" => {
                    let action: Action = serde_json::from_value(envelope.payload)
                        .map_err(|e| ConnectorError::Malformed(e.to_string()))?;
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
                        self.handle_job(job).await?;
                    }
                    let frames = self.handle_action(action).await?;
                    let result = frames
                        .into_iter()
                        .find_map(|frame| serde_json::from_str::<Value>(&frame).ok())
                        .ok_or_else(|| {
                            ConnectorError::Malformed("action produced no result".into())
                        })?;
                    let url = format!(
                        "{}/v1/results",
                        self.config.dispatch_url.trim_end_matches('/')
                    );
                    let response = client
                        .post(url)
                        .bearer_auth(&self.config.token)
                        .json(&result)
                        .send()
                        .await
                        .map_err(|e| ConnectorError::Execution(e.to_string()))?;
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
                    let next = body
                        .get("messages")
                        .and_then(Value::as_array)
                        .ok_or_else(|| {
                            ConnectorError::Malformed(
                                "result response missing messages array".into(),
                            )
                        })?;
                    pending.extend(next.iter().cloned());
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

    async fn handle_text(&self, text: &str) -> Vec<String> {
        let parsed = serde_json::from_str::<Value>(text)
            .map_err(|e| ConnectorError::Malformed(e.to_string()));
        match parsed {
            Ok(value) => match self.handle_value(value).await {
                Ok(values) => values,
                Err(error) => vec![self.error_frame(None, error_code(&error), error.to_string())],
            },
            Err(error) => vec![self.error_frame(None, "malformed_envelope", error.to_string())],
        }
    }

    async fn handle_value(&self, value: Value) -> Result<Vec<String>, ConnectorError> {
        let envelope = parse_envelope(&value)?;
        let ack = self.ack_frame(&envelope.message_id);
        let mut responses = vec![ack];
        match envelope.kind.as_str() {
            "job" => {
                let job: Job = serde_json::from_value(envelope.payload)
                    .map_err(|e| ConnectorError::Malformed(e.to_string()))?;
                responses.extend(self.handle_job(job).await?);
            }
            "action" => {
                let action: Action = serde_json::from_value(envelope.payload)
                    .map_err(|e| ConnectorError::Malformed(e.to_string()))?;
                responses.extend(self.handle_action(action).await?);
            }
            "terminal" => {
                let terminal: Terminal = serde_json::from_value(envelope.payload)
                    .map_err(|e| ConnectorError::Malformed(e.to_string()))?;
                responses.extend(self.handle_terminal(terminal).await?);
            }
            "ack" => {}
            "error" => {
                warn!(payload = %envelope.payload, "dispatch sent connector error");
            }
            _ => {
                return Err(ConnectorError::Unsupported(envelope.kind));
            }
        }
        Ok(responses)
    }

    async fn handle_job(&self, job: Job) -> Result<Vec<String>, ConnectorError> {
        validate_job(&job)?;
        let job_id = job.job_id.clone();
        {
            let jobs = self.jobs.lock().await;
            if jobs.contains_key(&job_id) {
                return Ok(Vec::new());
            }
        }
        let workspace = self.cloner.clone_job(&job).await?;
        let local = LocalJob {
            job,
            workspace_path: workspace,
            sandbox_id: None,
            last_activity: Utc::now(),
            terminal: false,
            processed_actions: HashMap::new(),
        };
        self.jobs.lock().await.insert(job_id.clone(), local);
        info!(job_id = %job_id, "connector accepted job");
        Ok(Vec::new())
    }

    async fn handle_action(&self, action: Action) -> Result<Vec<String>, ConnectorError> {
        validate_action(&action)?;
        let mut jobs = self.jobs.lock().await;
        let Some(local) = jobs.get_mut(&action.job_id) else {
            return Ok(vec![self.error_frame(
                Some(action.job_id),
                "internal_error",
                "action references unknown job".into(),
            )]);
        };
        if local.terminal {
            return Ok(vec![self.error_frame(
                Some(action.job_id),
                "job_lease_expired",
                "job is already terminal".into(),
            )]);
        }
        if let Some(cached) = local.processed_actions.get(&action.action_id) {
            if cached.verb != action.verb || cached.args != action.args {
                return Ok(vec![self.error_frame(
                    Some(action.job_id.clone()),
                    "malformed_envelope",
                    "action_id replay payload mismatch".into(),
                )]);
            }
            return Ok(vec![cached.frame.clone()]);
        }
        local.last_activity = Utc::now();
        let result = match action.verb.as_str() {
            "create_sandbox" => {
                let value = self.executor.execute(&local.job, local, &action).await?;
                validate_response("create_sandbox", &value)?;
                local.sandbox_id = value
                    .get("sandbox_id")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                Ok(value)
            }
            _ => self.executor.execute(&local.job, local, &action).await,
        };
        let frame = match result {
            Ok(value) => self.result_frame(&action, "ok", Some(value), None),
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
            action.action_id,
            CachedAction {
                verb: action.verb.clone(),
                args: action.args.clone(),
                frame: frame.clone(),
            },
        );
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
        let local = self.jobs.lock().await.remove(&terminal.job_id);
        let Some(mut local) = local else {
            return Ok(Vec::new());
        };
        local.terminal = true;
        if terminal.instructions == "discard_local_copy" {
            if let Err(error) = self.executor.destroy(&local).await {
                warn!(job_id = %terminal.job_id, error = %error, "sandbox cleanup failed");
            }
            if let Err(error) = tokio::fs::remove_dir_all(&local.workspace_path).await {
                warn!(job_id = %terminal.job_id, error = %error, "workspace cleanup failed");
            }
        }
        Ok(Vec::new())
    }

    async fn reap_expired(&self) -> Vec<String> {
        let now = Utc::now();
        let mut expired = Vec::new();
        let mut jobs = self.jobs.lock().await;
        for (job_id, local) in jobs.iter_mut() {
            let ttl = local.job.lease_ttl_seconds.unwrap_or(0);
            if ttl >= 30 && (now - local.last_activity).num_seconds() > ttl as i64 {
                local.terminal = true;
                expired.push(self.error_frame(
                    Some(job_id.clone()),
                    "job_lease_expired",
                    "connector abandoned expired job".into(),
                ));
            }
        }
        let ids: Vec<String> = jobs
            .iter()
            .filter_map(|(id, local)| local.terminal.then_some(id.clone()))
            .collect();
        for id in ids {
            if let Some(local) = jobs.remove(&id) {
                if let Err(error) = self.executor.destroy(&local).await {
                    warn!(job_id = %id, error = %error, "expired sandbox cleanup failed");
                }
                if let Err(error) = tokio::fs::remove_dir_all(&local.workspace_path).await {
                    warn!(job_id = %id, error = %error, "expired workspace cleanup failed");
                }
            }
        }
        expired
    }

    fn ack_frame(&self, message_id: &str) -> String {
        envelope("ack", None, json!({"acked_message_id": message_id})).to_string()
    }

    fn error_frame(&self, job_id: Option<String>, code: &str, message: String) -> String {
        envelope(
            "error",
            job_id,
            serde_json::to_value(ErrorPayload {
                code: code.to_string(),
                message,
            })
            .unwrap_or_else(
                |_| json!({"code": "internal_error", "message": "error serialization failed"}),
            ),
        )
        .to_string()
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

fn error_code(error: &ConnectorError) -> &'static str {
    match error {
        ConnectorError::Malformed(_) => "malformed_envelope",
        ConnectorError::Unsupported(_) => "internal_error",
        ConnectorError::Execution(_) => "internal_error",
        ConnectorError::LeaseExpired(_) => "job_lease_expired",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[derive(Clone, Default)]
    struct FakeExecutor {
        calls: Arc<Mutex<Vec<String>>>,
        destroyed: Arc<Mutex<usize>>,
    }

    #[derive(Clone)]
    struct FakeCloner {
        workspace: PathBuf,
    }

    #[async_trait]
    impl WorkspaceCloner for FakeCloner {
        async fn clone_job(&self, _job: &Job) -> Result<PathBuf, ConnectorError> {
            tokio::fs::create_dir_all(&self.workspace)
                .await
                .map_err(|e| ConnectorError::Execution(e.to_string()))?;
            Ok(self.workspace.clone())
        }
    }

    #[async_trait]
    impl ActionExecutor for FakeExecutor {
        async fn execute(
            &self,
            _job: &Job,
            _local: &LocalJob,
            action: &Action,
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
                last_activity: Utc::now(),
                terminal: false,
                processed_actions: HashMap::new(),
            },
        );
        let action_id = Uuid::new_v4().to_string();
        let frame = action(&job_id, &action_id, "observe_failure", json!({}));
        let first = connector.handle_value(frame.clone()).await.unwrap();
        let second = connector.handle_value(frame).await.unwrap();
        assert_eq!(fake.calls.lock().await.len(), 1);
        assert_eq!(first[1], second[1]);
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
                last_activity: Utc::now(),
                terminal: false,
                processed_actions: HashMap::new(),
            },
        );
        let action_id = Uuid::new_v4().to_string();
        let first = connector
            .handle_value(action(&job_id, &action_id, "observe_failure", json!({})))
            .await
            .unwrap();
        let second = connector
            .handle_value(action(
                &job_id,
                &action_id,
                "observe_failure",
                json!({"timeout_seconds": 30}),
            ))
            .await
            .unwrap();
        assert_eq!(fake.calls.lock().await.len(), 1);
        assert_eq!(first.len(), 2);
        assert_eq!(second.len(), 2);
        let error: Value = serde_json::from_str(&second[1]).unwrap();
        assert_eq!(error["kind"], "error");
        assert_eq!(error["payload"]["code"], "malformed_envelope");
    }

    #[tokio::test]
    async fn terminal_discards_sandbox_and_workspace() {
        let fake = FakeExecutor::default();
        let connector = Connector::new(config(), fake.clone());
        let job = job();
        let job_id = job.job_id.clone();
        let dir = tempdir().unwrap();
        let workspace = dir.path().join("workspace");
        tokio::fs::create_dir_all(&workspace).await.unwrap();
        connector.jobs.lock().await.insert(
            job_id.clone(),
            LocalJob {
                job,
                workspace_path: workspace.clone(),
                sandbox_id: Some("sb-test".into()),
                last_activity: Utc::now(),
                terminal: false,
                processed_actions: HashMap::new(),
            },
        );
        let terminal = envelope(
            "terminal",
            Some(job_id.clone()),
            json!({"job_id":job_id,"final_status":"failed","instructions":"discard_local_copy"}),
        );
        connector.handle_value(terminal).await.unwrap();
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
    async fn malformed_envelope_returns_error_frame() {
        let connector = Connector::new(config(), FakeExecutor::default());
        let frames = connector.handle_text("{not-json").await;
        assert_eq!(frames.len(), 1);
        let value: Value = serde_json::from_str(&frames[0]).unwrap();
        assert_eq!(value["kind"], "error");
        assert_eq!(value["payload"]["code"], "malformed_envelope");
    }
}
