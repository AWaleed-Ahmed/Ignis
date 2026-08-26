use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepositoryRef {
    pub owner: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clone_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateSandboxRequest {
    pub run_id: String,
    pub tenant_id: String,
    pub repository: RepositoryRef,
    pub commit_sha: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_environment: Option<String>,
    #[serde(default = "default_timeout_minutes")]
    pub timeout_minutes: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret_fixture_set: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub labels: Option<std::collections::HashMap<String, String>>,
}

fn default_timeout_minutes() -> u32 {
    20
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateSandboxResponse {
    pub sandbox_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    pub namespace: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cluster_backend: Option<String>,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_account: Option<String>,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestSpec {
    #[serde(rename = "type")]
    pub manifest_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chart: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub values: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overlay: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PatchFile {
    pub path: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PatchSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unified_diff: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files: Option<Vec<PatchFile>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeployRevisionRequest {
    pub repository_sha: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_path: Option<String>,
    pub manifests: ManifestSpec,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub patch: Option<PatchSpec>,
    #[serde(default = "default_wait_seconds")]
    pub wait_seconds: u32,
}

fn default_wait_seconds() -> u32 {
    60
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceRef {
    pub kind: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_version: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FidelityChecklist {
    pub same_commit: bool,
    pub same_render_path: bool,
    pub same_image_digest_or_tag: bool,
    #[serde(default)]
    pub equivalent_k8s_semantics: bool,
    pub equivalent_non_secret_config: bool,
    pub dependencies_available: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FidelitySubstitution {
    pub name: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FidelityReport {
    pub score: f64,
    pub checklist: FidelityChecklist,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub substitutions: Vec<FidelitySubstitution>,
    pub material_gaps: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeployRevisionResponse {
    pub sandbox_id: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub resources: Vec<ResourceRef>,
    pub rendered_artifact_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub image_refs: Vec<String>,
    pub fidelity: FidelityReport,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_versions: Option<std::collections::BTreeMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    pub deployed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObserveFailureRequest {
    #[serde(default = "default_observe_timeout")]
    pub timeout_seconds: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_signature_key: Option<String>,
}

fn default_observe_timeout() -> u32 {
    90
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvidenceRef {
    pub kind: String,
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub excerpt: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NormalizedFailure {
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    pub resource_kind: String,
    pub resource_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attributes: Option<serde_json::Map<String, serde_json::Value>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FailureSignature {
    pub class: String,
    pub key: String,
    pub normalized: NormalizedFailure,
    pub reproduced: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,
    pub evidence_refs: Vec<EvidenceRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    pub observed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObserveFailureResponse {
    pub sandbox_id: String,
    pub signature: FailureSignature,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matched_expected: Option<bool>,
    pub artifact_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fidelity: Option<FidelityReport>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthCheck {
    #[serde(rename = "type")]
    pub check_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_status: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<u32>,
    #[serde(default = "default_true")]
    pub mandatory: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationPlan {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commands: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub health_checks: Option<Vec<HealthCheck>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compare_to_signature_key: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunValidationRequest {
    pub plan: ValidationPlan,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationCheck {
    pub name: String,
    pub kind: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mandatory: Option<bool>,
    pub duration_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_refs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationResults {
    pub sandbox_id: String,
    pub passed: bool,
    pub fail_closed: bool,
    /// True only when checks passed AND there are no material fidelity gaps.
    #[serde(default)]
    pub full_validation: bool,
    pub checks: Vec<ValidationCheck>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before_signature_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_signature_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature_cleared: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_versions: Option<std::collections::BTreeMap<String, String>>,
    pub completed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DestroySandboxRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DestroySandboxResponse {
    pub sandbox_id: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    pub destroyed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FinalizeResultRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    #[serde(default)]
    pub require_patch: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepositoryOwnerName {
    pub owner: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidatedFixRecord {
    pub result_id: String,
    pub sandbox_id: String,
    pub run_id: String,
    pub repository: RepositoryOwnerName,
    pub base_commit_sha: String,
    pub deployed_sha: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub patch: Option<PatchSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rendered_manifest_artifact_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before_signature: Option<FailureSignature>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_signature: Option<FailureSignature>,
    pub validation: ValidationResults,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fidelity: Option<FidelityReport>,
    pub artifact_ids: Vec<String>,
    pub content_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    pub finalized_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FinalizeResultResponse {
    pub sandbox_id: String,
    pub result_id: String,
    pub status: String,
    pub finalized_at: DateTime<Utc>,
    pub record: ValidatedFixRecord,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorBody {
    pub code: String,
    pub message: String,
    pub retryable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorEnvelope {
    pub error: ErrorBody,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactRecord {
    pub id: String,
    pub kind: String,
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForceCleanupRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
    #[serde(default = "default_true")]
    pub reconcile_leaks: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForceCleanupResponse {
    pub destroyed_sandboxes: Vec<String>,
    pub destroyed_namespaces: Vec<String>,
    pub message: Option<String>,
    pub completed_at: DateTime<Utc>,
}
