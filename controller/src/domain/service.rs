use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio::time::{sleep, timeout, Instant};

use chrono::{Duration as ChronoDuration, Utc};
use uuid::Uuid;

use crate::domain::errors::DomainError;
use crate::domain::ids::{namespace_for_run, sandbox_id_from_run};
use crate::domain::models::*;
use crate::k8s::{ClusterBackend, DestroyOutcome, ImageDigests, NamespaceSpec};
use crate::observe;
use crate::policy;
use crate::render;
use crate::state::recovery::RecoveryStore;
use crate::state::registry::{SandboxRecord, SandboxRegistry, SandboxStatus};
use crate::validate;

pub struct SandboxService {
    backend: Arc<dyn ClusterBackend>,
    registry: Arc<SandboxRegistry>,
    recovery: Arc<RecoveryStore>,
}

impl SandboxService {
    pub fn new(backend: Arc<dyn ClusterBackend>, registry: Arc<SandboxRegistry>) -> Self {
        Self {
            backend,
            registry,
            recovery: Arc::new(RecoveryStore::in_memory()),
        }
    }

    pub fn with_recovery(
        backend: Arc<dyn ClusterBackend>,
        registry: Arc<SandboxRegistry>,
        recovery: Arc<RecoveryStore>,
    ) -> Self {
        Self {
            backend,
            registry,
            recovery,
        }
    }

    pub fn recovery(&self) -> Arc<RecoveryStore> {
        self.recovery.clone()
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

        let applied_secret_fixtures = Some(AppliedSecretFixtureInventory {
            namespace: namespace.clone(),
            fixture_set: req.secret_fixture_set.clone(),
            complete: req.secret_fixture_set.is_none(),
            truncated: false,
            secrets: vec![],
        });

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
            applied_secret_fixtures,
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
            let (secrets_yaml, fixture_metadata) =
                crate::fixtures::load_secret_fixture(fixture_set)?;
            crate::policy::check_manifest_policy(&secrets_yaml)?;
            self.backend
                .apply_secret_fixtures(&namespace, &secrets_yaml)
                .await?;
            let (secrets, truncated) = bounded_fixture_metadata(fixture_metadata);
            self.registry
                .update(&sandbox_id, |r| {
                    r.applied_secret_fixtures = Some(AppliedSecretFixtureInventory {
                        namespace: namespace.clone(),
                        fixture_set: Some(fixture_set.clone()),
                        complete: !truncated,
                        truncated,
                        secrets: secrets.clone(),
                    });
                })
                .map_err(DomainError::Internal)?;
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
        if !(1..=600).contains(&req.deploy_timeout_seconds) || req.wait_seconds > 600 {
            return Err(DomainError::InvalidRequest(
                "deploy_timeout_seconds must be 1..600 and wait_seconds 0..600".into(),
            ));
        }
        let deadline =
            crate::deadline::Deadline::new(Duration::from_secs(req.deploy_timeout_seconds as u64));
        let record = self.require_ready(sandbox_id)?;
        let workspace = resolve_workspace(
            req.workspace_path.as_deref(),
            record.clone_url.as_deref(),
            req.repository_sha.as_str(),
            record.cloned_workspace.as_deref(),
            &record.commit_sha,
            deadline,
        )?;

        // Remember controller-managed clone path for later deploys in this sandbox.
        if req.workspace_path.is_none() && record.cloned_workspace.is_none() {
            let _ = self.registry.update(sandbox_id, |r| {
                r.cloned_workspace = Some(workspace.clone());
            });
        }

        // Apply file patches into a temp workspace copy when provided.
        let effective_workspace = if let Some(patch) = &req.patch {
            apply_patches_to_temp(&workspace, patch, deadline)?
        } else {
            workspace.clone()
        };

        let rendered =
            render::render_with_deadline(&effective_workspace, &req.manifests, deadline)?;
        policy::check_manifest_policy(&rendered.yaml)?;

        deadline.remaining()?;
        let apply = self
            .backend
            .apply_manifests(&record.namespace, &rendered.yaml, deadline.remaining()?)
            .await?;

        let mut image_refs = apply.image_refs.clone();
        // The real API server may accept a Deployment before any Pod has an imageID.
        // Bound the entire best-effort lookup by the request's existing wait_seconds.
        // Mock has no runtime digests: preserve its immediate tags-only behavior.
        deadline.remaining()?;
        // Leave response/report headroom after optional runtime evidence gathering.
        let digest_budget = deadline
            .remaining()?
            .saturating_sub(Duration::from_secs(2))
            .min(Duration::from_secs(req.wait_seconds as u64));
        let digests = if self.backend.name() == "mock" {
            timeout(
                digest_budget,
                self.backend.resolve_image_digests(&record.namespace),
            )
            .await
            .ok()
            .and_then(Result::ok)
            .unwrap_or_default()
        } else {
            poll_image_digests(
                &apply.image_refs,
                digest_budget,
                Duration::from_millis(500),
                || self.backend.resolve_image_digests(&record.namespace),
            )
            .await
        };
        for (image, digest) in &digests {
            if apply.image_refs.contains(image) && !image_refs.contains(digest) {
                image_refs.push(digest.clone());
            }
        }

        let tool_versions =
            crate::tools::collect_tool_versions_with_deadline(crate::deadline::Deadline::new(
                deadline
                    .remaining()?
                    .saturating_sub(Duration::from_secs(1))
                    .min(Duration::from_secs(5)),
            ));
        deadline.remaining()?;
        let fidelity = build_fidelity(
            &record,
            &req,
            &rendered.render_path,
            &apply.image_refs,
            &digests,
            &rendered.yaml,
        );
        let now = Utc::now();
        let tools_blob = serde_json::to_string_pretty(&tool_versions).unwrap_or_default();
        let manifest_art = store_artifact(sandbox_id, "manifest", &rendered.yaml);
        let tools_art = store_artifact(sandbox_id, "tool_versions", &tools_blob);
        let artifact_id = manifest_art.id.clone();
        let tools_artifact_id = tools_art.id.clone();

        deadline.remaining()?;
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
            rendered_files: rendered.files,
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
                        r.applied_secret_fixtures = None;
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
                        r.applied_secret_fixtures = None;
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
                        rec.applied_secret_fixtures = None;
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
    deadline: crate::deadline::Deadline,
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
        let cloned = crate::gitclone::clone_at_sha_with_deadline(url, sha, deadline)?;
        return Ok(cloned.to_string_lossy().to_string());
    }
    std::env::var("RAPHAEL_DEFAULT_WORKSPACE").map_err(|_| {
        DomainError::InvalidRequest(
            "workspace_path required, or set repository.clone_url for clone-at-SHA, or RAPHAEL_DEFAULT_WORKSPACE"
                .into(),
        )
    })
}

fn apply_patches_to_temp(
    workspace: &str,
    patch: &PatchSpec,
    deadline: crate::deadline::Deadline,
) -> Result<String, DomainError> {
    let tmp = tempfile::tempdir().map_err(|e| DomainError::Internal(e.to_string()))?;
    copy_dir_recursive(PathBuf::from(workspace), tmp.path().to_path_buf(), deadline)?;

    if let Some(files) = &patch.files {
        for f in files {
            deadline.remaining()?;
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

fn copy_dir_recursive(
    from: PathBuf,
    to: PathBuf,
    deadline: crate::deadline::Deadline,
) -> Result<(), DomainError> {
    std::fs::create_dir_all(&to).map_err(|e| DomainError::Internal(e.to_string()))?;
    for entry in walkdir::WalkDir::new(&from)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        deadline.remaining()?;
        let path = entry.path();
        let rel = path.strip_prefix(&from).unwrap();
        let dest = to.join(rel);
        if path.is_dir() {
            std::fs::create_dir_all(&dest).map_err(|e| DomainError::Internal(e.to_string()))?;
        } else if path.is_file() {
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| DomainError::Internal(e.to_string()))?;
            }
            std::fs::copy(path, &dest).map_err(|e| DomainError::Internal(e.to_string()))?;
        }
    }
    Ok(())
}

async fn poll_image_digests<F, Fut>(
    expected_images: &[String],
    budget: Duration,
    interval: Duration,
    mut fetch: F,
) -> ImageDigests
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<ImageDigests, DomainError>>,
{
    let deadline = Instant::now() + budget;
    let mut observed = ImageDigests::new();
    if unresolved_images(expected_images, &observed).is_empty() {
        return observed;
    }
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return observed;
        }
        if let Ok(Ok(digests)) = timeout(remaining, fetch()).await {
            // Use the latest successful snapshot, not a union of historical
            // snapshots that could hide an image no longer resolved now.
            observed = digests;
            if unresolved_images(expected_images, &observed).is_empty() {
                return observed;
            }
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return observed;
        }
        sleep(interval.min(remaining)).await;
    }
}

fn unresolved_images<'a>(expected: &'a [String], digests: &ImageDigests) -> Vec<&'a str> {
    expected
        .iter()
        .filter(|image| {
            !is_digest_pinned(image)
                && !digests
                    .get(*image)
                    .is_some_and(|digest| digest.contains("@sha256:"))
        })
        .map(String::as_str)
        .collect()
}

fn is_digest_pinned(image: &str) -> bool {
    let Some((repository, digest)) = image.rsplit_once("@sha256:") else {
        return false;
    };
    !repository.is_empty()
        && digest.len() == 64
        && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn image_digest_gaps(expected: &[String], digests: &ImageDigests) -> Vec<String> {
    unresolved_images(expected, digests)
        .into_iter()
        .map(|image| format!("image digests not resolved; tags only: {image}"))
        .collect()
}

#[cfg(test)]
mod digest_poll_tests {
    use super::{build_fidelity, poll_image_digests};
    use crate::domain::errors::DomainError;
    use crate::domain::models::DeployRevisionRequest;
    use crate::k8s::ImageDigests;
    use crate::state::registry::SandboxRecord;
    use std::cell::Cell;
    use std::time::Duration;

    fn expected() -> Vec<String> {
        vec!["busybox:1.37.0".into(), "busybox:1.36.1".into()]
    }

    fn resolved(both: bool) -> ImageDigests {
        let mut map = ImageDigests::from([("busybox:1.37.0".into(), "busybox@sha256:aaa".into())]);
        if both {
            map.insert("busybox:1.36.1".into(), "busybox@sha256:bbb".into());
        }
        map
    }

    fn fidelity_gaps(images: &[String], digests: &ImageDigests) -> Vec<String> {
        let record: SandboxRecord = serde_json::from_value(serde_json::json!({
            "sandbox_id": "sb-test", "run_id": "test", "tenant_id": "test",
            "namespace": "test", "commit_sha": "abc", "repository_owner": "test",
            "repository_name": "test", "status": "ready",
            "created_at": "2026-10-01T00:00:00Z", "expires_at": "2026-10-01T01:00:00Z",
            "service_account": "test", "resources": [], "image_refs": [],
            "artifacts": [], "cluster_backend": "kubectl"
        }))
        .unwrap();
        let req: DeployRevisionRequest = serde_json::from_value(serde_json::json!({
            "repository_sha": "abc", "manifests": {"type": "yaml", "path": "app.yaml"}
        }))
        .unwrap();
        build_fidelity(&record, &req, "yaml", images, digests, "").material_gaps
    }

    #[tokio::test]
    async fn waits_for_late_status_then_returns_digest() {
        let calls = Cell::new(0);
        let result = poll_image_digests(
            &expected()[..1],
            Duration::from_millis(200),
            Duration::from_millis(1),
            || {
                let call = calls.get() + 1;
                calls.set(call);
                async move {
                    if call < 3 {
                        Ok(ImageDigests::new())
                    } else {
                        Ok(resolved(false))
                    }
                }
            },
        )
        .await;
        assert_eq!(calls.get(), 3);
        assert_eq!(result, resolved(false));
    }

    #[tokio::test]
    async fn times_out_to_tags_when_image_id_never_appears() {
        let calls = Cell::new(0);
        let result = poll_image_digests(
            &expected(),
            Duration::from_millis(20),
            Duration::from_millis(1),
            || {
                calls.set(calls.get() + 1);
                async { Ok::<_, DomainError>(ImageDigests::new()) }
            },
        )
        .await;
        assert!(calls.get() > 1);
        assert!(result.is_empty());
    }

    #[tokio::test]
    async fn a_stalled_lookup_cannot_exceed_the_existing_budget() {
        let result = poll_image_digests(
            &expected(),
            Duration::from_millis(20),
            Duration::from_millis(1),
            || std::future::pending::<Result<ImageDigests, DomainError>>(),
        )
        .await;
        assert!(result.is_empty());
    }

    #[tokio::test]
    async fn waits_for_second_image_even_when_first_has_a_digest() {
        let calls = Cell::new(0);
        let result = poll_image_digests(
            &expected(),
            Duration::from_millis(200),
            Duration::from_millis(1),
            || {
                let call = calls.get() + 1;
                calls.set(call);
                async move { Ok(resolved(call >= 3)) }
            },
        )
        .await;
        assert_eq!(calls.get(), 3);
        assert_eq!(result, resolved(true));
        assert!(fidelity_gaps(&expected(), &result).is_empty());
    }

    #[tokio::test]
    async fn timeout_preserves_partial_digests_and_names_only_unresolved_image() {
        let result = poll_image_digests(
            &expected(),
            Duration::from_millis(20),
            Duration::from_millis(1),
            || async { Ok(resolved(false)) },
        )
        .await;
        assert_eq!(result, resolved(false));
        assert_eq!(
            fidelity_gaps(&expected(), &result),
            vec!["image digests not resolved; tags only: busybox:1.36.1"]
        );
    }

    #[tokio::test]
    async fn all_images_resolved_exits_immediately_without_gap() {
        let calls = Cell::new(0);
        let result = poll_image_digests(
            &expected(),
            Duration::from_secs(10),
            Duration::from_secs(5),
            || {
                calls.set(calls.get() + 1);
                async { Ok(resolved(true)) }
            },
        )
        .await;
        assert_eq!(calls.get(), 1);
        assert!(fidelity_gaps(&expected(), &result).is_empty());
    }

    #[tokio::test]
    async fn kubectl_error_mid_poll_recovers_without_failing_deploy() {
        let calls = Cell::new(0);
        let result = poll_image_digests(
            &expected(),
            Duration::from_millis(200),
            Duration::from_millis(1),
            || {
                let call = calls.get() + 1;
                calls.set(call);
                async move {
                    match call {
                        1 => Ok(resolved(false)),
                        2 => Err(DomainError::ClusterUnavailable(
                            "temporary kubectl failure".into(),
                        )),
                        _ => Ok(resolved(true)),
                    }
                }
            },
        )
        .await;
        assert_eq!(calls.get(), 3);
        assert_eq!(result, resolved(true));
        assert!(fidelity_gaps(&expected(), &result).is_empty());
    }

    #[tokio::test]
    async fn zero_wait_skips_lookup_and_discloses_each_unresolved_image() {
        let calls = Cell::new(0);
        let result = poll_image_digests(
            &expected(),
            Duration::ZERO,
            Duration::from_millis(1),
            || {
                calls.set(calls.get() + 1);
                async { Ok(resolved(true)) }
            },
        )
        .await;
        assert_eq!(calls.get(), 0);
        assert!(result.is_empty());
        assert_eq!(
            fidelity_gaps(&expected(), &result),
            vec![
                "image digests not resolved; tags only: busybox:1.37.0",
                "image digests not resolved; tags only: busybox:1.36.1"
            ]
        );
    }

    #[tokio::test]
    async fn digest_pinned_images_skip_runtime_lookup_without_gaps() {
        let images = vec![format!(
            "registry.example:5000/app@sha256:{}",
            "a".repeat(64)
        )];
        let calls = Cell::new(0);
        let result = poll_image_digests(
            &images,
            Duration::from_secs(60),
            Duration::from_millis(500),
            || {
                calls.set(calls.get() + 1);
                async { Ok(ImageDigests::new()) }
            },
        )
        .await;
        assert_eq!(calls.get(), 0);
        assert!(result.is_empty());
        assert!(fidelity_gaps(&images, &result).is_empty());
    }

    #[tokio::test]
    async fn mixed_pinned_and_tagged_images_only_wait_for_tagged_image() {
        let images = vec![
            format!("app@sha256:{}", "a".repeat(64)),
            "busybox:1.36.1".into(),
        ];
        assert_eq!(
            fidelity_gaps(&images, &ImageDigests::new()),
            vec!["image digests not resolved; tags only: busybox:1.36.1"]
        );
        let calls = Cell::new(0);
        let result = poll_image_digests(
            &images,
            Duration::from_secs(60),
            Duration::from_millis(500),
            || {
                calls.set(calls.get() + 1);
                async { Ok(resolved(true)) }
            },
        )
        .await;
        assert_eq!(calls.get(), 1);
        assert!(!result.contains_key(&images[0]));
        assert!(fidelity_gaps(&images, &result).is_empty());
    }

    #[tokio::test]
    async fn malformed_digest_pin_still_discloses_a_gap() {
        let images = vec!["app@sha256:abc".into()];
        let result = poll_image_digests(
            &images,
            Duration::ZERO,
            Duration::from_millis(1),
            || async { Ok(ImageDigests::new()) },
        )
        .await;
        assert_eq!(
            fidelity_gaps(&images, &result),
            vec!["image digests not resolved; tags only: app@sha256:abc"]
        );
    }
}

fn build_fidelity(
    record: &SandboxRecord,
    req: &DeployRevisionRequest,
    render_path: &str,
    expected_images: &[String],
    digests: &ImageDigests,
    rendered_yaml: &str,
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
    let all_resolved = if self_is_mock(record) {
        // Preserve the mock backend's existing global tags-only disclosure.
        let has_digest = expected_images.iter().any(|i| i.contains("@sha256:"));
        if !expected_images.is_empty() && !has_digest {
            gaps.push("image digests not resolved; tags only".into());
        }
        has_digest || expected_images.is_empty()
    } else {
        let image_gaps = image_digest_gaps(expected_images, digests);
        let complete = image_gaps.is_empty();
        gaps.extend(image_gaps);
        complete
    };
    let secret_coverage = crate::secret_coverage::evaluate_secret_coverage(
        rendered_yaml,
        &record.namespace,
        record.applied_secret_fixtures.as_ref(),
    );
    if secret_coverage.references.iter().any(|reference| {
        !reference.optional
            && matches!(
                reference.status,
                SecretCoverageStatus::MissingObject | SecretCoverageStatus::MissingKey
            )
    }) {
        gaps.push(
            "required Secret references are not covered by applied synthetic fixtures".into(),
        );
    } else if !secret_coverage.complete {
        gaps.push("Secret fixture coverage is incomplete or unknown".into());
    }
    let checklist = FidelityChecklist {
        same_commit,
        same_render_path: !render_path.is_empty(),
        same_image_digest_or_tag: !expected_images.is_empty(),
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
            all_resolved,
        ];
        flags.iter().filter(|x| **x).count() as f64 / flags.len() as f64
    };
    FidelityReport {
        score,
        checklist,
        substitutions,
        material_gaps: gaps,
        secret_coverage: Some(secret_coverage),
    }
}

fn bounded_fixture_metadata(
    metadata: Vec<crate::fixtures::FixtureSecretMetadata>,
) -> (Vec<AppliedSecretFixture>, bool) {
    const MAX_SECRETS: usize = 128;
    const MAX_KEYS: usize = 1_024;

    let mut secrets = Vec::new();
    let mut key_count = 0;
    let mut truncated = metadata.len() > MAX_SECRETS;
    for secret in metadata.into_iter().take(MAX_SECRETS) {
        if key_count == MAX_KEYS && !secret.keys.is_empty() {
            truncated = true;
            break;
        }
        let remaining = MAX_KEYS - key_count;
        if secret.keys.len() > remaining {
            truncated = true;
        }
        let keys = secret.keys.into_iter().take(remaining).collect::<Vec<_>>();
        key_count += keys.len();
        secrets.push(AppliedSecretFixture {
            name: secret.name,
            keys,
        });
    }
    (secrets, truncated)
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

#[cfg(test)]
mod secret_inventory_tests {
    use super::*;
    use crate::k8s::mock::MockCluster;
    use crate::state::sqlite::SqliteStore;

    fn create_request(run_id: &str, fixture: Option<&str>) -> CreateSandboxRequest {
        serde_json::from_value(serde_json::json!({
            "run_id": run_id, "tenant_id": "test", "repository": {"owner":"test", "name":"test"},
            "commit_sha": "abcdef0123456789", "secret_fixture_set": fixture
        }))
        .unwrap()
    }

    #[tokio::test]
    async fn applied_inventory_survives_registry_restart_and_is_cleared_at_destroy() {
        let data = tempfile::tempdir().unwrap();
        let store = Arc::new(SqliteStore::open(data.path().join("records")).unwrap());
        let backend = Arc::new(MockCluster::with_store(data.path()).unwrap());
        let registry = Arc::new(SandboxRegistry::with_store(store.clone()).unwrap());
        let service = SandboxService::new(backend.clone(), registry.clone());
        let created = service
            .create_sandbox(create_request("coverage-restart", Some("payments-test")))
            .await
            .unwrap();
        let saved = registry
            .get(&created.sandbox_id)
            .unwrap()
            .applied_secret_fixtures
            .unwrap();
        assert!(saved.complete);
        assert!(saved
            .secrets
            .iter()
            .any(|secret| secret.name == "payments-db"
                && secret.keys.contains(&"DATABASE_URL".into())));
        let restored = Arc::new(SandboxRegistry::with_store(store.clone()).unwrap());
        assert_eq!(
            restored
                .get(&created.sandbox_id)
                .unwrap()
                .applied_secret_fixtures,
            Some(saved)
        );
        let restarted = SandboxService::new(backend, restored.clone());
        restarted
            .destroy_sandbox(&created.sandbox_id, DestroySandboxRequest { reason: None })
            .await
            .unwrap();
        assert!(restored
            .get(&created.sandbox_id)
            .unwrap()
            .applied_secret_fixtures
            .is_none());
        assert!(store.load_all().unwrap()[0]
            .applied_secret_fixtures
            .is_none());
    }

    #[tokio::test]
    async fn no_selection_is_known_empty_but_failed_selection_stays_incomplete() {
        let registry = Arc::new(SandboxRegistry::new());
        let service = SandboxService::new(Arc::new(MockCluster::new()), registry.clone());
        let empty = service
            .create_sandbox(create_request("coverage-empty", None))
            .await
            .unwrap();
        let inventory = registry
            .get(&empty.sandbox_id)
            .unwrap()
            .applied_secret_fixtures
            .unwrap();
        assert!(inventory.complete && inventory.secrets.is_empty());
        assert!(service
            .create_sandbox(create_request(
                "coverage-failed",
                Some("nonexistent-coverage-fixture")
            ))
            .await
            .is_err());
        let failed = registry
            .get(&sandbox_id_from_run("coverage-failed"))
            .unwrap()
            .applied_secret_fixtures
            .unwrap();
        assert!(!failed.complete && failed.secrets.is_empty());
    }

    #[test]
    fn inventory_limits_preserve_only_bounded_metadata_and_disclose_truncation() {
        let metadata = (0..129)
            .map(|index| crate::fixtures::FixtureSecretMetadata {
                name: format!("secret-{index}"),
                keys: vec!["KEY".into()],
            })
            .collect();
        let (secrets, truncated) = bounded_fixture_metadata(metadata);
        assert_eq!(secrets.len(), 128);
        assert!(truncated);
        let (secrets, truncated) =
            bounded_fixture_metadata(vec![crate::fixtures::FixtureSecretMetadata {
                name: "db".into(),
                keys: (0..1025).map(|index| format!("KEY{index}")).collect(),
            }]);
        assert_eq!(secrets[0].keys.len(), 1024);
        assert!(truncated);
    }
}
