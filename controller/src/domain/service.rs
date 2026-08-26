use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use chrono::{Duration as ChronoDuration, Utc};
use uuid::Uuid;

use crate::domain::errors::DomainError;
use crate::domain::ids::{namespace_for_run, sandbox_id_from_run};
use crate::domain::models::*;
use crate::k8s::{ClusterBackend, DestroyOutcome, NamespaceSpec};
use crate::observe;
use crate::policy;
use crate::render;
use crate::state::registry::{SandboxRecord, SandboxRegistry, SandboxStatus};
use crate::validate;

pub struct SandboxService {
    backend: Arc<dyn ClusterBackend>,
    registry: Arc<SandboxRegistry>,
}

impl SandboxService {
    pub fn new(backend: Arc<dyn ClusterBackend>, registry: Arc<SandboxRegistry>) -> Self {
        Self { backend, registry }
    }

    pub fn backend_name(&self) -> &'static str {
        self.backend.name()
    }

    pub async fn create_sandbox(
        &self,
        req: CreateSandboxRequest,
    ) -> Result<CreateSandboxResponse, DomainError> {
        if req.run_id.trim().is_empty() {
            return Err(DomainError::InvalidRequest("run_id required".into()));
        }
        if req.commit_sha.len() < 7 {
            return Err(DomainError::InvalidRequest(
                "commit_sha must be at least 7 characters".into(),
            ));
        }

        let sandbox_id = sandbox_id_from_run(&req.run_id);
        let namespace = namespace_for_run(&req.run_id)?;
        let now = Utc::now();
        let timeout_minutes = req.timeout_minutes.clamp(1, 120);
        let expires_at = now + ChronoDuration::minutes(timeout_minutes as i64);
        let service_account = "raphael-sandbox-sa".to_string();

        let spec = NamespaceSpec {
            namespace: namespace.clone(),
            sandbox_id: sandbox_id.clone(),
            run_id: req.run_id.clone(),
            tenant_id: req.tenant_id.clone(),
            expires_at,
            service_account: service_account.clone(),
            cpu_limit: "2".into(),
            memory_limit: "2Gi".into(),
        };

        self.backend.create_isolated_namespace(&spec).await?;

        let record = SandboxRecord {
            sandbox_id: sandbox_id.clone(),
            run_id: req.run_id.clone(),
            tenant_id: req.tenant_id,
            namespace: namespace.clone(),
            commit_sha: req.commit_sha,
            repository_owner: req.repository.owner,
            repository_name: req.repository.name,
            clone_url: req.repository.clone_url,
            cloned_workspace: None,
            target_environment: req.target_environment,
            secret_fixture_set: req.secret_fixture_set.clone(),
            status: SandboxStatus::Ready,
            created_at: now,
            expires_at,
            service_account: service_account.clone(),
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
            cluster_backend: self.backend.name().to_string(),
        };

        self.registry
            .insert(record)
            .map_err(DomainError::Conflict)?;

        if let Some(fixture_set) = &req.secret_fixture_set {
            let secrets_yaml = crate::fixtures::load_secret_fixture_yaml(fixture_set)?;
            crate::policy::check_manifest_policy(&secrets_yaml)?;
            self.backend
                .apply_secret_fixtures(&namespace, &secrets_yaml)
                .await?;
            let art = store_artifact(
                &sandbox_id,
                "secret_fixture",
                &format!("fixture_set={fixture_set}"),
            );
            self.registry
                .update(&sandbox_id, |r| {
                    r.artifacts.push(art);
                })
                .map_err(DomainError::Internal)?;
        }

        Ok(CreateSandboxResponse {
            sandbox_id,
            run_id: Some(req.run_id),
            namespace,
            cluster_backend: Some(self.backend.name().to_string()),
            status: "ready".into(),
            service_account: Some(service_account),
            created_at: now,
            expires_at,
        })
    }

    pub async fn deploy_revision(
        &self,
        sandbox_id: &str,
        req: DeployRevisionRequest,
    ) -> Result<DeployRevisionResponse, DomainError> {
        let record = self.require_ready(sandbox_id)?;
        let workspace = resolve_workspace(
            req.workspace_path.as_deref(),
            record.clone_url.as_deref(),
            req.repository_sha.as_str(),
            record.cloned_workspace.as_deref(),
            &record.commit_sha,
        )?;

        // Remember controller-managed clone path for later deploys in this sandbox.
        if req.workspace_path.is_none() && record.cloned_workspace.is_none() {
            let _ = self.registry.update(sandbox_id, |r| {
                r.cloned_workspace = Some(workspace.clone());
            });
        }

        // Apply file patches into a temp workspace copy when provided.
        let effective_workspace = if let Some(patch) = &req.patch {
            apply_patches_to_temp(&workspace, patch)?
        } else {
            workspace.clone()
        };

        let rendered = render::render(&effective_workspace, &req.manifests)?;
        policy::check_manifest_policy(&rendered.yaml)?;

        let apply = self
            .backend
            .apply_manifests(
                &record.namespace,
                &rendered.yaml,
                Duration::from_secs(req.wait_seconds as u64),
            )
            .await?;

        let mut image_refs = apply.image_refs.clone();
        if let Ok(digests) = self.backend.resolve_image_digests(&record.namespace).await {
            for d in digests {
                if !image_refs.iter().any(|x| x == &d) {
                    image_refs.push(d);
                }
            }
        }

        let tool_versions = crate::tools::collect_tool_versions();
        let fidelity = build_fidelity(&record, &req, &rendered.render_path, &image_refs);
        let now = Utc::now();
        let tools_blob = serde_json::to_string_pretty(&tool_versions).unwrap_or_default();
        let manifest_art = store_artifact(sandbox_id, "manifest", &rendered.yaml);
        let tools_art = store_artifact(sandbox_id, "tool_versions", &tools_blob);
        let artifact_id = manifest_art.id.clone();
        let tools_artifact_id = tools_art.id.clone();

        self.registry
            .update(sandbox_id, |r| {
                r.deployed_sha = Some(req.repository_sha.clone());
                r.rendered_yaml = Some(rendered.yaml.clone());
                r.resources = apply.resources.clone();
                r.image_refs = image_refs.clone();
                r.last_fidelity = Some(fidelity.clone());
                r.last_patch = req.patch.clone();
                // A new deploy invalidates any previous finalize for safety.
                r.finalized_result = None;
                r.artifacts.push(manifest_art);
                r.artifacts.push(tools_art);
            })
            .map_err(DomainError::Internal)?;

        Ok(DeployRevisionResponse {
            sandbox_id: sandbox_id.to_string(),
            status: "deployed".into(),
            resources: apply.resources,
            rendered_artifact_ids: vec![artifact_id, tools_artifact_id],
            image_refs,
            fidelity,
            tool_versions: Some(tool_versions),
            message: Some(format!("rendered via {}", rendered.render_path)),
            deployed_at: now,
        })
    }

    pub async fn observe_failure(
        &self,
        sandbox_id: &str,
        req: ObserveFailureRequest,
    ) -> Result<ObserveFailureResponse, DomainError> {
        let record = self.require_ready(sandbox_id)?;
        if record.rendered_yaml.is_none() {
            return Err(DomainError::ObservationFailed(
                "no revision deployed".into(),
            ));
        }
        let obs = self
            .backend
            .observe_workload(
                &record.namespace,
                Duration::from_secs(req.timeout_seconds as u64),
            )
            .await?;

        let signature = observe::observe(&obs, record.rendered_yaml.as_deref());
        let matched = req
            .expected_signature_key
            .as_ref()
            .map(|expected| expected == &signature.key);

        let now = Utc::now();
        let mut artifact_ids: Vec<String> = Vec::new();
        let _ = now;
        if !obs.events.is_empty() {
            let events_blob = obs
                .events
                .iter()
                .map(|e| {
                    format!(
                        "{} {} {}: {}",
                        e.involved_kind, e.involved_name, e.reason, e.message
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            let art = store_artifact(sandbox_id, "k8s_event", &events_blob);
            artifact_ids.push(art.id.clone());
            self.registry
                .update(sandbox_id, |r| {
                    r.artifacts.push(art);
                })
                .map_err(DomainError::Internal)?;
        }

        // Bounded pod logs.
        if let Ok(logs) = self
            .backend
            .collect_pod_logs(&record.namespace, 8_192)
            .await
        {
            for log in logs {
                let content = format!(
                    "pod={} container={}\n{}",
                    log.pod, log.container, log.content
                );
                let art = store_artifact(sandbox_id, "container_log", &content);
                artifact_ids.push(art.id.clone());
                self.registry
                    .update(sandbox_id, |r| {
                        r.artifacts.push(art);
                    })
                    .map_err(DomainError::Internal)?;
            }
        }

        for e in &signature.evidence_refs {
            artifact_ids.push(e.id.clone());
        }

        self.registry
            .update(sandbox_id, |r| {
                r.last_signature = Some(signature.clone());
                if signature.class != "healthy" {
                    r.reproduction_signature = Some(signature.clone());
                } else {
                    r.after_signature = Some(signature.clone());
                }
            })
            .map_err(DomainError::Internal)?;

        Ok(ObserveFailureResponse {
            sandbox_id: sandbox_id.to_string(),
            signature,
            matched_expected: matched,
            artifact_ids,
            fidelity: record.last_fidelity,
        })
    }

    pub async fn run_validation(
        &self,
        sandbox_id: &str,
        req: RunValidationRequest,
    ) -> Result<ValidationResults, DomainError> {
        let record = self.require_ready(sandbox_id)?;

        // Refresh after signature for comparisons.
        let after = match self
            .backend
            .observe_workload(&record.namespace, Duration::from_secs(30))
            .await
        {
            Ok(obs) => Some(observe::observe(&obs, record.rendered_yaml.as_deref())),
            Err(e) => {
                // Mandatory observation failure => fail closed via validate layer messaging
                return Err(DomainError::ValidationUnavailable(e.to_string()));
            }
        };

        let workspace = std::env::var("RAPHAEL_DEFAULT_WORKSPACE").ok();
        let tool_versions = crate::tools::collect_tool_versions();
        let mut results = validate::run_validation(
            self.backend.as_ref(),
            sandbox_id,
            &record.namespace,
            workspace.as_deref(),
            &req.plan,
            record
                .reproduction_signature
                .as_ref()
                .or(record.last_signature.as_ref()),
            after.as_ref(),
            record.last_fidelity.as_ref(),
        )
        .await?;
        results.tool_versions = Some(tool_versions.clone());

        let tools_blob = serde_json::to_string_pretty(&tool_versions).unwrap_or_default();
        let tools_art = store_artifact(sandbox_id, "tool_versions", &tools_blob);

        self.registry
            .update(sandbox_id, |r| {
                if let Some(sig) = after.clone() {
                    r.after_signature = Some(sig);
                    r.last_signature = r.after_signature.clone();
                }
                r.last_validation = Some(results.clone());
                // New validation clears prior finalize so agent must re-freeze.
                r.finalized_result = None;
                r.artifacts.push(tools_art);
            })
            .map_err(DomainError::Internal)?;

        Ok(results)
    }

    pub async fn finalize_result(
        &self,
        sandbox_id: &str,
        req: FinalizeResultRequest,
    ) -> Result<FinalizeResultResponse, DomainError> {
        let record = self.require_ready(sandbox_id)?;

        if let Some(existing) = &record.finalized_result {
            return Ok(FinalizeResultResponse {
                sandbox_id: sandbox_id.to_string(),
                result_id: existing.result_id.clone(),
                status: "already_finalized".into(),
                finalized_at: existing.finalized_at,
                record: existing.clone(),
            });
        }

        let validation = record.last_validation.clone().ok_or_else(|| {
            DomainError::InvalidRequest(
                "cannot finalize: run_validation has not succeeded yet".into(),
            )
        })?;

        if validation.fail_closed || !validation.passed {
            return Err(DomainError::ValidationFailed(
                "cannot finalize: last validation did not pass (fail closed)".into(),
            ));
        }

        if req.require_patch && record.last_patch.is_none() {
            return Err(DomainError::InvalidRequest(
                "require_patch=true but no patch was stored from deploy_revision".into(),
            ));
        }

        let deployed_sha = record.deployed_sha.clone().ok_or_else(|| {
            DomainError::InvalidRequest("cannot finalize: no revision deployed".into())
        })?;

        let rendered_manifest_artifact_id = record
            .artifacts
            .iter()
            .rev()
            .find(|a| a.kind == "manifest")
            .map(|a| a.id.clone());

        let now = Utc::now();
        let content_hash = compute_result_hash(
            &record.last_patch,
            record.rendered_yaml.as_deref(),
            record
                .reproduction_signature
                .as_ref()
                .map(|s| s.key.as_str()),
            record.after_signature.as_ref().map(|s| s.key.as_str()),
            &validation,
        );

        let result_id = format!("res-{}", &content_hash[..16]);
        let artifact_ids: Vec<String> = record.artifacts.iter().map(|a| a.id.clone()).collect();

        let frozen = ValidatedFixRecord {
            result_id: result_id.clone(),
            sandbox_id: sandbox_id.to_string(),
            run_id: record.run_id.clone(),
            repository: RepositoryOwnerName {
                owner: record.repository_owner.clone(),
                name: record.repository_name.clone(),
            },
            base_commit_sha: record.commit_sha.clone(),
            deployed_sha,
            patch: record.last_patch.clone(),
            rendered_manifest_artifact_id,
            before_signature: record.reproduction_signature.clone(),
            after_signature: record.after_signature.clone(),
            validation,
            fidelity: record.last_fidelity.clone(),
            artifact_ids,
            content_hash,
            notes: req.notes,
            finalized_at: now,
        };

        self.registry
            .update(sandbox_id, |r| {
                r.finalized_result = Some(frozen.clone());
            })
            .map_err(DomainError::Internal)?;

        Ok(FinalizeResultResponse {
            sandbox_id: sandbox_id.to_string(),
            result_id,
            status: "finalized".into(),
            finalized_at: now,
            record: frozen,
        })
    }

    pub fn get_result(&self, sandbox_id: &str) -> Result<ValidatedFixRecord, DomainError> {
        let record = self
            .registry
            .get(sandbox_id)
            .ok_or_else(|| DomainError::NotFound(format!("sandbox not found: {sandbox_id}")))?;
        record.finalized_result.ok_or_else(|| {
            DomainError::NotFound(format!("no finalized result for sandbox: {sandbox_id}"))
        })
    }

    pub async fn destroy_sandbox(
        &self,
        sandbox_id: &str,
        _req: DestroySandboxRequest,
    ) -> Result<DestroySandboxResponse, DomainError> {
        let now = Utc::now();
        let existing = self.registry.get(sandbox_id);
        match existing {
            None => Ok(DestroySandboxResponse {
                sandbox_id: sandbox_id.to_string(),
                status: "already_destroyed".into(),
                namespace: None,
                message: Some("sandbox id unknown; treated as already destroyed".into()),
                destroyed_at: now,
            }),
            Some(record) if record.status == SandboxStatus::Destroyed => {
                Ok(DestroySandboxResponse {
                    sandbox_id: sandbox_id.to_string(),
                    status: "already_destroyed".into(),
                    namespace: Some(record.namespace),
                    message: None,
                    destroyed_at: now,
                })
            }
            Some(record) => {
                let outcome = self.backend.destroy_namespace(&record.namespace).await?;
                let _ = crate::artifacts::purge_sandbox_artifacts(sandbox_id);
                self.registry
                    .update(sandbox_id, |r| {
                        r.status = SandboxStatus::Destroyed;
                    })
                    .map_err(DomainError::Internal)?;
                Ok(DestroySandboxResponse {
                    sandbox_id: sandbox_id.to_string(),
                    status: match outcome {
                        DestroyOutcome::Destroyed => "destroyed",
                        DestroyOutcome::AlreadyDestroyed => "already_destroyed",
                    }
                    .into(),
                    namespace: Some(record.namespace),
                    message: None,
                    destroyed_at: now,
                })
            }
        }
    }

    pub async fn reap_expired(&self, now: chrono::DateTime<Utc>) -> Result<(), DomainError> {
        let expired = self.registry.list_expired(now);
        for record in expired {
            tracing::info!(sandbox_id = %record.sandbox_id, "reaping expired sandbox");
            if let Err(e) = self
                .destroy_sandbox(
                    &record.sandbox_id,
                    DestroySandboxRequest {
                        reason: Some("ttl_expired".into()),
                    },
                )
                .await
            {
                tracing::warn!(sandbox_id = %record.sandbox_id, error = %e, "ttl destroy failed");
            }
        }
        // Cluster-side leak hunt: namespaces labeled raphael.managed with expired label.
        self.reconcile_leaked_namespaces(now).await?;
        if let Err(e) = crate::artifacts::purge_expired_artifacts() {
            tracing::warn!(error = %e, "artifact retention purge failed");
        }
        Ok(())
    }

    pub async fn reconcile_leaked_namespaces(
        &self,
        now: chrono::DateTime<Utc>,
    ) -> Result<Vec<String>, DomainError> {
        let managed = self.backend.list_managed_namespaces().await?;
        let ready_ns: std::collections::HashSet<String> = self
            .registry
            .list_ready()
            .into_iter()
            .map(|r| r.namespace)
            .collect();
        let mut destroyed = Vec::new();
        for ns in managed {
            let expired = ns.expires_at.map(|exp| exp <= now).unwrap_or(false);
            let orphan = !ready_ns.contains(&ns.name);
            if expired || (orphan && ns.expires_at.map(|e| e <= now).unwrap_or(false)) {
                tracing::warn!(
                    namespace = %ns.name,
                    sandbox_id = ?ns.sandbox_id,
                    "destroying leaked/expired managed namespace"
                );
                let _ = self.backend.destroy_namespace(&ns.name).await?;
                destroyed.push(ns.name);
                if let Some(sid) = &ns.sandbox_id {
                    let _ = self.registry.update(sid, |r| {
                        r.status = SandboxStatus::Destroyed;
                    });
                    let _ = crate::artifacts::purge_sandbox_artifacts(sid);
                }
            }
        }
        Ok(destroyed)
    }

    pub async fn force_cleanup(
        &self,
        req: ForceCleanupRequest,
    ) -> Result<ForceCleanupResponse, DomainError> {
        let now = Utc::now();
        let mut destroyed_sandboxes = Vec::new();
        let mut destroyed_namespaces = Vec::new();

        if let Some(sid) = &req.sandbox_id {
            let resp = self
                .destroy_sandbox(
                    sid,
                    DestroySandboxRequest {
                        reason: req
                            .reason
                            .clone()
                            .or_else(|| Some("admin_force_cleanup".into())),
                    },
                )
                .await?;
            destroyed_sandboxes.push(sid.clone());
            if let Some(ns) = resp.namespace {
                destroyed_namespaces.push(ns);
            }
        }

        if let Some(ns) = &req.namespace {
            let _ = self.backend.destroy_namespace(ns).await?;
            destroyed_namespaces.push(ns.clone());
            // Mark any matching registry record destroyed.
            for r in self.registry.list_ready() {
                if &r.namespace == ns {
                    let _ = self.registry.update(&r.sandbox_id, |rec| {
                        rec.status = SandboxStatus::Destroyed;
                    });
                    destroyed_sandboxes.push(r.sandbox_id);
                }
            }
        }

        if req.reconcile_leaks {
            let leaked = self.reconcile_leaked_namespaces(now).await?;
            destroyed_namespaces.extend(leaked);
        } else if req.sandbox_id.is_none() && req.namespace.is_none() {
            // No explicit target: reap registry-expired sandboxes.
            let expired = self.registry.list_expired(now);
            for record in expired {
                let _ = self
                    .destroy_sandbox(
                        &record.sandbox_id,
                        DestroySandboxRequest {
                            reason: Some("admin_reap_expired".into()),
                        },
                    )
                    .await;
                destroyed_sandboxes.push(record.sandbox_id);
            }
        }

        destroyed_sandboxes.sort();
        destroyed_sandboxes.dedup();
        destroyed_namespaces.sort();
        destroyed_namespaces.dedup();

        Ok(ForceCleanupResponse {
            destroyed_sandboxes,
            destroyed_namespaces,
            message: Some("admin force-cleanup completed".into()),
            completed_at: now,
        })
    }

    fn require_ready(&self, sandbox_id: &str) -> Result<SandboxRecord, DomainError> {
        let record = self
            .registry
            .get(sandbox_id)
            .ok_or_else(|| DomainError::NotFound(format!("sandbox not found: {sandbox_id}")))?;
        if record.status != SandboxStatus::Ready {
            return Err(DomainError::NotFound(format!(
                "sandbox destroyed: {sandbox_id}"
            )));
        }
        Ok(record)
    }
}

fn store_artifact(sandbox_id: &str, kind: &str, content: &str) -> ArtifactRecord {
    match crate::artifacts::persist_artifact(sandbox_id, kind, content) {
        Ok(rec) => rec,
        Err(e) => {
            tracing::warn!(error = %e, %sandbox_id, kind, "artifact disk persist failed; keeping in-memory only");
            ArtifactRecord {
                id: format!("artifact-{}", Uuid::new_v4()),
                kind: kind.to_string(),
                content: content.chars().take(512).collect(),
                path: None,
                created_at: Utc::now(),
            }
        }
    }
}

fn resolve_workspace(
    path: Option<&str>,
    clone_url: Option<&str>,
    deploy_sha: &str,
    existing_clone: Option<&str>,
    create_sha: &str,
) -> Result<String, DomainError> {
    if let Some(p) = path {
        let pb = PathBuf::from(p);
        if !pb.exists() {
            return Err(DomainError::InvalidRequest(format!(
                "workspace_path not found: {p}"
            )));
        }
        return Ok(p.to_string());
    }
    if let Some(existing) = existing_clone {
        let pb = PathBuf::from(existing);
        if pb.exists() {
            return Ok(existing.to_string());
        }
    }
    if let Some(url) = clone_url {
        // Prefer deploy SHA; fall back to create-time SHA.
        let sha = if deploy_sha.len() >= 7 {
            deploy_sha
        } else {
            create_sha
        };
        let cloned = crate::gitclone::clone_at_sha(url, sha)?;
        return Ok(cloned.to_string_lossy().to_string());
    }
    std::env::var("RAPHAEL_DEFAULT_WORKSPACE").map_err(|_| {
        DomainError::InvalidRequest(
            "workspace_path required, or set repository.clone_url for clone-at-SHA, or RAPHAEL_DEFAULT_WORKSPACE"
                .into(),
        )
    })
}

fn apply_patches_to_temp(workspace: &str, patch: &PatchSpec) -> Result<String, DomainError> {
    let tmp = tempfile::tempdir().map_err(|e| DomainError::Internal(e.to_string()))?;
    copy_dir_recursive(PathBuf::from(workspace), tmp.path().to_path_buf())
        .map_err(|e| DomainError::Internal(e.to_string()))?;

    if let Some(files) = &patch.files {
        for f in files {
            let dest = tmp.path().join(&f.path);
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| DomainError::Internal(e.to_string()))?;
            }
            std::fs::write(&dest, &f.content).map_err(|e| DomainError::Internal(e.to_string()))?;
        }
    }
    // unified_diff application is intentionally not implemented in MVP; require files[] for patches.
    if patch.unified_diff.is_some() && patch.files.as_ref().map(|f| f.is_empty()).unwrap_or(true) {
        return Err(DomainError::InvalidRequest(
            "unified_diff alone not supported yet; provide patch.files".into(),
        ));
    }

    Ok(tmp.keep().to_string_lossy().to_string())
}

fn copy_dir_recursive(from: PathBuf, to: PathBuf) -> std::io::Result<()> {
    std::fs::create_dir_all(&to)?;
    for entry in walkdir::WalkDir::new(&from)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        let rel = path.strip_prefix(&from).unwrap();
        let dest = to.join(rel);
        if path.is_dir() {
            std::fs::create_dir_all(&dest)?;
        } else if path.is_file() {
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(path, &dest)?;
        }
    }
    Ok(())
}

fn build_fidelity(
    record: &SandboxRecord,
    req: &DeployRevisionRequest,
    render_path: &str,
    image_refs: &[String],
) -> FidelityReport {
    let same_commit = record.commit_sha.starts_with(&req.repository_sha)
        || req.repository_sha.starts_with(&record.commit_sha);
    let mut substitutions = Vec::new();
    let mut gaps = Vec::new();
    if let Some(fixture) = &record.secret_fixture_set {
        substitutions.push(FidelitySubstitution {
            name: format!("secret_fixture:{fixture}"),
            reason: "production secrets replaced with synthetic fixtures".into(),
        });
        gaps.push("production secret values not present".into());
    }
    if self_is_mock(record) {
        gaps.push("mock cluster backend; not identical to customer API server".into());
    }
    let has_digest = image_refs.iter().any(|i| i.contains("@sha256:"));
    if !image_refs.is_empty() && !has_digest {
        gaps.push("image digests not resolved; tags only".into());
    }
    let checklist = FidelityChecklist {
        same_commit,
        same_render_path: !render_path.is_empty(),
        same_image_digest_or_tag: !image_refs.is_empty(),
        equivalent_k8s_semantics: !self_is_mock(record),
        equivalent_non_secret_config: true,
        dependencies_available: true,
    };
    let score = {
        let flags = [
            checklist.same_commit,
            checklist.same_render_path,
            checklist.same_image_digest_or_tag,
            checklist.equivalent_non_secret_config,
            checklist.dependencies_available,
            has_digest || image_refs.is_empty(),
        ];
        flags.iter().filter(|x| **x).count() as f64 / flags.len() as f64
    };
    FidelityReport {
        score,
        checklist,
        substitutions,
        material_gaps: gaps,
    }
}

fn compute_result_hash(
    patch: &Option<PatchSpec>,
    rendered_yaml: Option<&str>,
    before_key: Option<&str>,
    after_key: Option<&str>,
    validation: &ValidationResults,
) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    if let Ok(bytes) = serde_json::to_vec(patch) {
        hasher.update(bytes);
    }
    if let Some(yaml) = rendered_yaml {
        hasher.update(yaml.as_bytes());
    }
    hasher.update(before_key.unwrap_or("").as_bytes());
    hasher.update(after_key.unwrap_or("").as_bytes());
    hasher.update(if validation.passed { b"1" } else { b"0" });
    hex::encode(hasher.finalize())
}

fn self_is_mock(record: &SandboxRecord) -> bool {
    record.cluster_backend == "mock"
}
