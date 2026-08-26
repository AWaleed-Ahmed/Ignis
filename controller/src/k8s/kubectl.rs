use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use tokio::process::Command;
use tokio::time::timeout;

use crate::domain::errors::DomainError;
use crate::domain::models::ResourceRef;
use crate::k8s::types::default_labels;
use crate::k8s::{
    ApplyResult, ClusterBackend, DestroyOutcome, HttpHealthResult, LogArtifact, NamespaceSpec,
    ObservedContainerStatus, ObservedEvent, ObservedPod, RolloutStatus, WorkloadObservation,
};

pub struct KubectlCluster {
    kubeconfig: Option<PathBuf>,
    context: Option<String>,
}

impl KubectlCluster {
    pub fn from_env() -> anyhow::Result<Self> {
        Ok(Self {
            kubeconfig: std::env::var_os("KUBECONFIG").map(PathBuf::from),
            context: std::env::var("RAPHAEL_KUBE_CONTEXT").ok(),
        })
    }

    fn base_cmd(&self) -> Command {
        let mut cmd = Command::new("kubectl");
        if let Some(cfg) = &self.kubeconfig {
            cmd.arg("--kubeconfig").arg(cfg);
        }
        if let Some(ctx) = &self.context {
            cmd.arg("--context").arg(ctx);
        }
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
        cmd
    }

    async fn run(
        &self,
        args: &[&str],
        max: Duration,
    ) -> Result<(i32, String, String), DomainError> {
        let mut cmd = self.base_cmd();
        cmd.args(args);
        let fut = cmd.output();
        let output = timeout(max, fut)
            .await
            .map_err(|_| DomainError::Timeout(format!("kubectl {} timed out", args.join(" "))))?
            .map_err(|e| DomainError::ClusterUnavailable(e.to_string()))?;
        let code = output.status.code().unwrap_or(1);
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        Ok((code, stdout, stderr))
    }
}

#[async_trait]
impl ClusterBackend for KubectlCluster {
    fn name(&self) -> &'static str {
        "kubectl"
    }

    async fn create_isolated_namespace(&self, spec: &NamespaceSpec) -> Result<(), DomainError> {
        let labels = default_labels(spec);
        let label_args: Vec<String> = labels.iter().map(|(k, v)| format!("{k}={v}")).collect();

        let (code, _, stderr) = self
            .run(
                &["create", "namespace", &spec.namespace],
                Duration::from_secs(30),
            )
            .await?;
        if code != 0 && !stderr.contains("AlreadyExists") {
            return Err(DomainError::ClusterUnavailable(stderr));
        }

        for label in &label_args {
            let _ = self
                .run(
                    &["label", "namespace", &spec.namespace, label, "--overwrite"],
                    Duration::from_secs(15),
                )
                .await?;
        }
        // Pod Security Admission labels (best-effort; ignore errors on older clusters)
        // Pod Security Admission labels
        let enforce = std::env::var("RAPHAEL_PSA_ENFORCE").unwrap_or_else(|_| "restricted".into());
        for psa in [
            format!("pod-security.kubernetes.io/enforce={enforce}"),
            "pod-security.kubernetes.io/enforce-version=latest".into(),
            "pod-security.kubernetes.io/warn=restricted".into(),
        ] {
            let _ = self
                .run(
                    &["label", "namespace", &spec.namespace, &psa, "--overwrite"],
                    Duration::from_secs(10),
                )
                .await;
        }

        let isolation = isolation_manifest(spec);
        apply_yaml(self, &spec.namespace, &isolation).await?;
        Ok(())
    }

    async fn destroy_namespace(&self, namespace: &str) -> Result<DestroyOutcome, DomainError> {
        let (code, _, stderr) = self
            .run(
                &[
                    "delete",
                    "namespace",
                    namespace,
                    "--wait=false",
                    "--ignore-not-found=true",
                ],
                Duration::from_secs(60),
            )
            .await?;
        if code != 0 {
            return Err(DomainError::ClusterUnavailable(stderr));
        }
        if stderr.contains("NotFound") {
            Ok(DestroyOutcome::AlreadyDestroyed)
        } else {
            Ok(DestroyOutcome::Destroyed)
        }
    }

    async fn apply_manifests(
        &self,
        namespace: &str,
        rendered_yaml: &str,
        timeout_d: Duration,
    ) -> Result<ApplyResult, DomainError> {
        let yaml = if std::env::var("RAPHAEL_INJECT_RESTRICTED_SC").unwrap_or_else(|_| "1".into())
            != "0"
        {
            crate::security_context::inject_restricted_pod_security(rendered_yaml)?
        } else {
            rendered_yaml.to_string()
        };
        apply_yaml(self, namespace, &yaml).await?;
        let resources = list_resources_from_yaml(&yaml);
        let image_refs = crate::render::common::extract_images(&yaml);
        // Best-effort wait
        let _ = timeout_d;
        Ok(ApplyResult {
            resources,
            image_refs,
        })
    }

    async fn observe_workload(
        &self,
        namespace: &str,
        max: Duration,
    ) -> Result<WorkloadObservation, DomainError> {
        let (code, stdout, stderr) = self
            .run(&["get", "events", "-n", namespace, "-o", "json"], max)
            .await?;
        if code != 0 {
            return Err(DomainError::ObservationFailed(stderr));
        }
        let events = parse_events_json(&stdout);

        let (code, pods_json, stderr) = self
            .run(&["get", "pods", "-n", namespace, "-o", "json"], max)
            .await?;
        if code != 0 {
            return Err(DomainError::ObservationFailed(stderr));
        }
        let pods = parse_pods_json(&pods_json);

        Ok(WorkloadObservation {
            events,
            pods,
            rendered_hint: None,
        })
    }

    async fn check_rollout(
        &self,
        namespace: &str,
        resource: &str,
        max: Duration,
    ) -> Result<RolloutStatus, DomainError> {
        let timeout_s = max.as_secs().max(1).to_string();
        let (code, stdout, stderr) = self
            .run(
                &[
                    "rollout",
                    "status",
                    resource,
                    "-n",
                    namespace,
                    "--timeout",
                    &format!("{timeout_s}s"),
                ],
                max + Duration::from_secs(5),
            )
            .await?;
        Ok(RolloutStatus {
            ready: code == 0,
            message: if code == 0 { stdout } else { stderr },
        })
    }

    async fn http_health(
        &self,
        namespace: &str,
        url: &str,
        expected_status: i32,
        max: Duration,
    ) -> Result<HttpHealthResult, DomainError> {
        let (fetch_url, mut pf) = prepare_http_target(self, namespace, url).await?;
        let result = curl_status(&fetch_url, expected_status, max).await;
        if let Some(mut child) = pf.take() {
            let _ = child.kill().await;
            let _ = child.wait().await;
        }
        result
    }

    async fn apply_secret_fixtures(
        &self,
        namespace: &str,
        secrets_yaml: &str,
    ) -> Result<(), DomainError> {
        apply_yaml(self, namespace, secrets_yaml).await
    }

    async fn collect_pod_logs(
        &self,
        namespace: &str,
        max_bytes_per_pod: usize,
    ) -> Result<Vec<LogArtifact>, DomainError> {
        let (code, pods_json, stderr) = self
            .run(
                &["get", "pods", "-n", namespace, "-o", "json"],
                Duration::from_secs(30),
            )
            .await?;
        if code != 0 {
            return Err(DomainError::ObservationFailed(stderr));
        }
        let pods = parse_pods_json(&pods_json);
        let mut out = Vec::new();
        for pod in pods {
            let container = pod
                .container_statuses
                .first()
                .map(|c| c.name.clone())
                .unwrap_or_else(|| "app".into());
            let (code, stdout, stderr) = self
                .run(
                    &[
                        "logs",
                        "-n",
                        namespace,
                        &pod.name,
                        "-c",
                        &container,
                        "--tail=200",
                        "--timestamps=true",
                    ],
                    Duration::from_secs(30),
                )
                .await?;
            let mut content = if code == 0 {
                stdout
            } else {
                format!("log_unavailable: {stderr}")
            };
            if content.len() > max_bytes_per_pod {
                content.truncate(max_bytes_per_pod);
            }
            out.push(LogArtifact {
                pod: pod.name,
                container,
                content,
            });
        }
        Ok(out)
    }

    async fn resolve_image_digests(&self, namespace: &str) -> Result<Vec<String>, DomainError> {
        let (code, pods_json, _) = self
            .run(
                &["get", "pods", "-n", namespace, "-o", "json"],
                Duration::from_secs(20),
            )
            .await?;
        if code != 0 {
            return Ok(vec![]);
        }
        Ok(parse_image_digests(&pods_json))
    }

    async fn list_managed_namespaces(
        &self,
    ) -> Result<Vec<crate::k8s::ManagedNamespace>, DomainError> {
        let (code, stdout, stderr) = self
            .run(
                &[
                    "get",
                    "namespaces",
                    "-l",
                    "raphael.managed=true",
                    "-o",
                    "json",
                ],
                Duration::from_secs(30),
            )
            .await?;
        if code != 0 {
            return Err(DomainError::ClusterUnavailable(stderr));
        }
        Ok(parse_managed_namespaces(&stdout))
    }
}

/// Parse `svc/name:port/path` or `service://name:port/path` into port-forward target.
fn parse_service_url(url: &str) -> Option<(String, u16, String)> {
    let rest = url
        .strip_prefix("service://")
        .or_else(|| url.strip_prefix("svc/"))?;
    let (name_port, path) = match rest.split_once('/') {
        Some((np, p)) => (np, format!("/{p}")),
        None => (rest, "/".to_string()),
    };
    let (name, port_s) = name_port.split_once(':')?;
    let port: u16 = port_s.parse().ok()?;
    if name.is_empty() {
        return None;
    }
    Some((name.to_string(), port, path))
}

async fn prepare_http_target(
    cluster: &KubectlCluster,
    namespace: &str,
    url: &str,
) -> Result<(String, Option<tokio::process::Child>), DomainError> {
    if url.contains("127.0.0.1") || url.contains("localhost") {
        return Ok((url.to_string(), None));
    }
    let Some((svc, port, path)) = parse_service_url(url) else {
        return Err(DomainError::ValidationUnavailable(format!(
            "http health URL must be localhost or svc/name:port/path (got {url})"
        )));
    };
    let local_port = 18080 + (std::process::id() % 1000) as u16;
    let mut cmd = cluster.base_cmd();
    cmd.args([
        "port-forward",
        "-n",
        namespace,
        &format!("svc/{svc}"),
        &format!("{local_port}:{port}"),
    ]);
    cmd.stdout(Stdio::null());
    cmd.stderr(Stdio::null());
    let child = cmd
        .spawn()
        .map_err(|e| DomainError::ValidationUnavailable(format!("port-forward spawn: {e}")))?;
    // Give port-forward a moment to bind.
    tokio::time::sleep(Duration::from_millis(800)).await;
    Ok((format!("http://127.0.0.1:{local_port}{path}"), Some(child)))
}

async fn curl_status(
    url: &str,
    expected_status: i32,
    max: Duration,
) -> Result<HttpHealthResult, DomainError> {
    let client = timeout(max, async {
        let mut cmd = Command::new("curl");
        cmd.args([
            "-s",
            "-o",
            "/dev/null",
            "-w",
            "%{http_code}",
            "--max-time",
            "10",
            url,
        ]);
        cmd.output().await
    })
    .await
    .map_err(|_| DomainError::Timeout(format!("curl {url}")))?
    .map_err(|e| DomainError::ValidationUnavailable(e.to_string()))?;

    let code_str = String::from_utf8_lossy(&client.stdout).trim().to_string();
    let status_code = code_str.parse::<i32>().ok();
    let ok = status_code == Some(expected_status);
    Ok(HttpHealthResult {
        ok,
        status_code,
        message: format!("curl {url} => {code_str}"),
    })
}

fn parse_image_digests(raw: &str) -> Vec<String> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(raw) else {
        return vec![];
    };
    let mut out = Vec::new();
    let Some(items) = v.get("items").and_then(|i| i.as_array()) else {
        return out;
    };
    for item in items {
        let statuses = item
            .pointer("/status/containerStatuses")
            .and_then(|x| x.as_array())
            .cloned()
            .unwrap_or_default();
        for c in statuses {
            let image = c.get("image").and_then(|x| x.as_str()).unwrap_or("");
            let image_id = c.get("imageID").and_then(|x| x.as_str()).unwrap_or("");
            if let Some(digest) = image_id_to_digest(image, image_id) {
                out.push(digest);
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

fn image_id_to_digest(image: &str, image_id: &str) -> Option<String> {
    // imageID forms: docker-pullable://repo@sha256:..., or sha256:...
    let digest = if let Some(idx) = image_id.find("sha256:") {
        Some(&image_id[idx..])
    } else {
        None
    }?;
    let digest = digest.split_whitespace().next()?.to_string();
    if image.is_empty() {
        return Some(digest);
    }
    // Prefer repo@sha256:...
    let repo = image.split('@').next()?.split(':').next()?;
    Some(format!("{repo}@{digest}"))
}

fn parse_managed_namespaces(raw: &str) -> Vec<crate::k8s::ManagedNamespace> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(raw) else {
        return vec![];
    };
    let Some(items) = v.get("items").and_then(|i| i.as_array()) else {
        return vec![];
    };
    items
        .iter()
        .filter_map(|item| {
            let name = item.pointer("/metadata/name")?.as_str()?.to_string();
            let labels = item.pointer("/metadata/labels");
            let sandbox_id = labels
                .and_then(|l| l.get("raphael.sandbox_id"))
                .and_then(|x| x.as_str())
                .map(|s| s.to_string());
            let run_id = labels
                .and_then(|l| l.get("raphael.run_id"))
                .and_then(|x| x.as_str())
                .map(|s| s.to_string());
            let expires_at = labels
                .and_then(|l| l.get("raphael.expires_at"))
                .and_then(|x| x.as_str())
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                .map(|dt| dt.with_timezone(&chrono::Utc));
            Some(crate::k8s::ManagedNamespace {
                name,
                sandbox_id,
                run_id,
                expires_at,
            })
        })
        .collect()
}

async fn apply_yaml(
    cluster: &KubectlCluster,
    namespace: &str,
    yaml: &str,
) -> Result<(), DomainError> {
    let tmp = tempfile::NamedTempFile::new().map_err(|e| DomainError::Internal(e.to_string()))?;
    std::fs::write(tmp.path(), yaml).map_err(|e| DomainError::Internal(e.to_string()))?;
    let path = tmp.path().to_string_lossy().to_string();
    let (code, _, stderr) = cluster
        .run(
            &["apply", "-n", namespace, "-f", &path],
            Duration::from_secs(60),
        )
        .await?;
    if code != 0 {
        return Err(DomainError::DeployFailed(stderr));
    }
    Ok(())
}

fn isolation_manifest(spec: &NamespaceSpec) -> String {
    format!(
        r#"apiVersion: v1
kind: ResourceQuota
metadata:
  name: raphael-quota
  namespace: {ns}
spec:
  hard:
    requests.cpu: "2"
    requests.memory: 2Gi
    limits.cpu: "2"
    limits.memory: 2Gi
    pods: "20"
---
apiVersion: v1
kind: LimitRange
metadata:
  name: raphael-limits
  namespace: {ns}
spec:
  limits:
    - type: Container
      default:
        cpu: 250m
        memory: 256Mi
      defaultRequest:
        cpu: 50m
        memory: 64Mi
---
apiVersion: v1
kind: ServiceAccount
metadata:
  name: {sa}
  namespace: {ns}
---
apiVersion: networking.k8s.io/v1
kind: NetworkPolicy
metadata:
  name: default-deny-egress
  namespace: {ns}
spec:
  podSelector: {{}}
  # Egress-only deny: keeps isolation without risking kubelet probe quirks on kind.
  policyTypes:
    - Egress
---
# DNS egress so pods can resolve (image pulls are node-side; this helps app traffic later).
apiVersion: networking.k8s.io/v1
kind: NetworkPolicy
metadata:
  name: allow-dns
  namespace: {ns}
spec:
  podSelector: {{}}
  policyTypes:
    - Egress
  egress:
    - ports:
        - protocol: UDP
          port: 53
        - protocol: TCP
          port: 53
"#,
        ns = spec.namespace,
        sa = spec.service_account
    )
}

fn list_resources_from_yaml(yaml: &str) -> Vec<ResourceRef> {
    crate::render::common::list_resources(yaml)
}

fn parse_events_json(raw: &str) -> Vec<ObservedEvent> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(raw) else {
        return vec![];
    };
    v.get("items")
        .and_then(|i| i.as_array())
        .map(|items| {
            items
                .iter()
                .map(|item| ObservedEvent {
                    reason: item
                        .get("reason")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string(),
                    message: item
                        .get("message")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string(),
                    involved_kind: item
                        .pointer("/involvedObject/kind")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string(),
                    involved_name: item
                        .pointer("/involvedObject/name")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string(),
                })
                .collect()
        })
        .unwrap_or_default()
}

fn parse_pods_json(raw: &str) -> Vec<ObservedPod> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(raw) else {
        return vec![];
    };
    v.get("items")
        .and_then(|i| i.as_array())
        .map(|items| {
            items
                .iter()
                .map(|item| {
                    let name = item
                        .pointer("/metadata/name")
                        .and_then(|x| x.as_str())
                        .unwrap_or("pod")
                        .to_string();
                    let phase = item
                        .pointer("/status/phase")
                        .and_then(|x| x.as_str())
                        .unwrap_or("Unknown")
                        .to_string();
                    let container_statuses = item
                        .pointer("/status/containerStatuses")
                        .and_then(|x| x.as_array())
                        .map(|arr| {
                            arr.iter()
                                .map(|c| ObservedContainerStatus {
                                    name: c
                                        .get("name")
                                        .and_then(|x| x.as_str())
                                        .unwrap_or("app")
                                        .to_string(),
                                    ready: c
                                        .get("ready")
                                        .and_then(|x| x.as_bool())
                                        .unwrap_or(false),
                                    restart_count: c
                                        .get("restartCount")
                                        .and_then(|x| x.as_i64())
                                        .unwrap_or(0)
                                        as i32,
                                    waiting_reason: c
                                        .pointer("/state/waiting/reason")
                                        .and_then(|x| x.as_str())
                                        .map(|s| s.to_string()),
                                    waiting_message: c
                                        .pointer("/state/waiting/message")
                                        .and_then(|x| x.as_str())
                                        .map(|s| s.to_string()),
                                    last_termination_reason: c
                                        .pointer("/lastState/terminated/reason")
                                        .and_then(|x| x.as_str())
                                        .map(|s| s.to_string()),
                                    image: c
                                        .get("image")
                                        .and_then(|x| x.as_str())
                                        .map(|s| s.to_string()),
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    ObservedPod {
                        name,
                        phase,
                        container_statuses,
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}
