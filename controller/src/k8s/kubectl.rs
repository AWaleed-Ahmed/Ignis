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
    ApplyResult, ClusterBackend, DestroyOutcome, HttpHealthResult, ImageDigests, LogArtifact,
    NamespaceSpec, ObservedContainerStatus, ObservedEvent, ObservedPod, RolloutStatus,
    WorkloadObservation,
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
        let output = crate::process::output_async(&mut cmd, max)
            .await
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::TimedOut {
                    DomainError::Timeout(format!("kubectl {} timed out", args.join(" ")))
                } else {
                    DomainError::ClusterUnavailable(e.to_string())
                }
            })?;
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
        apply_yaml(self, &spec.namespace, &isolation, Duration::from_secs(60)).await?;
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
        apply_yaml(self, namespace, &yaml, timeout_d).await?;
        let resources = list_resources_from_yaml(&yaml);
        let image_refs = crate::render::common::extract_images(&yaml);
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
        let deadline = crate::deadline::Deadline::new(max);
        let (code, stdout, stderr) = self
            .run(
                &["get", "events", "-n", namespace, "-o", "json"],
                deadline.remaining()?,
            )
            .await?;
        if code != 0 {
            return Err(DomainError::ObservationFailed(stderr));
        }
        let events = parse_events_json(&stdout);
        let (code, pods_json, stderr) = self
            .run(
                &["get", "pods", "-n", namespace, "-o", "json"],
                deadline.remaining()?,
            )
            .await?;
        if code != 0 {
            return Err(DomainError::ObservationFailed(stderr));
        }
        let mut pods = parse_pods_json(&pods_json);
        if pods.iter().any(|pod| {
            pod.container_statuses.iter().any(|c| {
                matches!(
                    c.waiting_reason.as_deref(),
                    Some("ImagePullBackOff" | "ErrImagePull")
                )
            })
        }) {
            if let Ok(remaining) = deadline.cap(Duration::from_secs(5)) {
                if let Ok((0, owners, _)) = self
                    .run(
                        &[
                            "get",
                            "replicasets,deployments",
                            "-n",
                            namespace,
                            "-o",
                            "json",
                        ],
                        remaining,
                    )
                    .await
                {
                    resolve_deployment_owners(&mut pods, &owners, namespace);
                }
            }
        }

        Ok(WorkloadObservation {
            source: crate::k8s::ObservationSource::Runtime,
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
        apply_yaml(self, namespace, secrets_yaml, Duration::from_secs(60)).await
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

    async fn resolve_image_digests(&self, namespace: &str) -> Result<ImageDigests, DomainError> {
        let (code, pods_json, _) = self
            .run(
                &["get", "pods", "-n", namespace, "-o", "json"],
                Duration::from_secs(20),
            )
            .await?;
        if code != 0 {
            return Ok(ImageDigests::new());
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

fn parse_image_digests(raw: &str) -> ImageDigests {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(raw) else {
        return ImageDigests::new();
    };
    let mut out = ImageDigests::new();
    let Some(items) = v.get("items").and_then(|i| i.as_array()) else {
        return out;
    };
    for item in items {
        // Status image names can be registry-expanded. Match status to spec by
        // container name and keep the original spec image as the lookup key.
        for (spec_path, status_path) in [
            ("/spec/containers", "/status/containerStatuses"),
            ("/spec/initContainers", "/status/initContainerStatuses"),
        ] {
            let Some(containers) = item.pointer(spec_path).and_then(|x| x.as_array()) else {
                continue;
            };
            let Some(statuses) = item.pointer(status_path).and_then(|x| x.as_array()) else {
                continue;
            };
            for c in containers {
                let Some(image) = c.get("image").and_then(|x| x.as_str()) else {
                    continue;
                };
                let Some(name) = c.get("name").and_then(|x| x.as_str()) else {
                    continue;
                };
                let Some(status) = statuses
                    .iter()
                    .find(|s| s.get("name").and_then(|x| x.as_str()) == Some(name))
                else {
                    continue;
                };
                let image_id = status.get("imageID").and_then(|x| x.as_str()).unwrap_or("");
                if let Some(digest) = image_id_to_digest(image, image_id) {
                    out.insert(image.to_string(), digest);
                }
            }
        }
    }
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
    let reference = image.split('@').next()?;
    let repo = match reference.rfind(':') {
        Some(colon) if !reference[colon..].contains('/') => &reference[..colon],
        _ => reference,
    };
    Some(format!("{repo}@{digest}"))
}

#[cfg(test)]
mod digest_lookup_tests {
    use super::{image_id_to_digest, parse_image_digests};
    use serde_json::json;

    #[test]
    fn matches_status_by_name_and_keeps_distinct_tags_of_same_image() {
        let pods = json!({"items": [{
            "spec": {"containers": [
                {"name": "app", "image": "busybox:1.37.0"},
                {"name": "sidecar", "image": "busybox:1.36.1"}
            ]},
            "status": {"containerStatuses": [
                {"name": "sidecar", "image": "docker.io/library/busybox:1.36.1", "imageID": ""},
                {"name": "app", "image": "docker.io/library/busybox:1.37.0", "imageID": "containerd://sha256:aaa"}
            ]}
        }]});
        let digests = parse_image_digests(&pods.to_string());
        assert_eq!(digests.len(), 1);
        assert_eq!(digests["busybox:1.37.0"], "busybox@sha256:aaa");
        assert!(!digests.contains_key("busybox:1.36.1"));
    }

    #[test]
    fn includes_init_container_images_and_ignores_unmatched_status() {
        let pods = json!({"items": [{
            "spec": {"initContainers": [{"name": "setup", "image": "setup:v1"}]},
            "status": {"initContainerStatuses": [
                {"name": "setup", "imageID": "sha256:aaa"},
                {"name": "unrelated", "image": "other:v1", "imageID": "sha256:bbb"}
            ]}
        }]});
        let digests = parse_image_digests(&pods.to_string());
        assert_eq!(digests.len(), 1);
        assert_eq!(digests["setup:v1"], "setup@sha256:aaa");
    }

    #[test]
    fn digest_reference_preserves_registry_port() {
        assert_eq!(
            image_id_to_digest("registry.example:5000/app:v1", "sha256:aaa"),
            Some("registry.example:5000/app@sha256:aaa".into())
        );
    }
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
    budget: Duration,
) -> Result<(), DomainError> {
    let tmp = tempfile::NamedTempFile::new().map_err(|e| DomainError::Internal(e.to_string()))?;
    std::fs::write(tmp.path(), yaml).map_err(|e| DomainError::Internal(e.to_string()))?;
    let path = tmp.path().to_string_lossy().to_string();
    let (code, _, stderr) = cluster
        .run(&["apply", "-n", namespace, "-f", &path], budget)
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

fn json_string(value: &serde_json::Value, path: &str) -> Option<String> {
    value
        .pointer(path)?
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn json_time(value: &serde_json::Value, path: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(&json_string(value, path)?)
        .ok()
        .map(|t| t.with_timezone(&chrono::Utc))
}

fn parse_events_json(raw: &str) -> Vec<ObservedEvent> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(raw) else {
        return vec![];
    };
    value
        .get("items")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
        .map(|item| ObservedEvent {
            reason: json_string(item, "/reason").unwrap_or_default(),
            message: crate::observe::image_pull::safe_excerpt(
                &json_string(item, "/message").unwrap_or_default(),
            ),
            involved_kind: json_string(item, "/involvedObject/kind").unwrap_or_default(),
            involved_name: json_string(item, "/involvedObject/name").unwrap_or_default(),
            involved_uid: json_string(item, "/involvedObject/uid"),
            observed_at: json_time(item, "/eventTime")
                .or_else(|| json_time(item, "/lastTimestamp")),
        })
        .collect()
}

fn controller_owner(item: &serde_json::Value, kind: &str) -> Option<(String, String)> {
    let owners = item.pointer("/metadata/ownerReferences")?.as_array()?;
    let controlling: Vec<_> = owners
        .iter()
        .filter(|owner| owner.get("controller").and_then(|v| v.as_bool()) == Some(true))
        .collect();
    if controlling.len() != 1 || controlling[0].get("kind")?.as_str()? != kind {
        return None;
    }
    Some((
        json_string(controlling[0], "/name")?,
        json_string(controlling[0], "/uid")?,
    ))
}

fn parse_pods_json(raw: &str) -> Vec<ObservedPod> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(raw) else {
        return vec![];
    };
    value
        .get("items")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
        .map(|item| {
            let mut statuses = Vec::new();
            for (spec_path, status_path, is_init) in [
                ("/spec/containers", "/status/containerStatuses", false),
                (
                    "/spec/initContainers",
                    "/status/initContainerStatuses",
                    true,
                ),
            ] {
                let specs = item.pointer(spec_path).and_then(|v| v.as_array());
                for c in item
                    .pointer(status_path)
                    .and_then(|v| v.as_array())
                    .into_iter()
                    .flatten()
                {
                    let name = json_string(c, "/name").unwrap_or_default();
                    let spec_image = specs
                        .and_then(|items| {
                            items.iter().find(|spec| {
                                spec.get("name").and_then(|v| v.as_str()) == Some(name.as_str())
                            })
                        })
                        .and_then(|spec| json_string(spec, "/image"));
                    statuses.push(ObservedContainerStatus {
                        name,
                        is_init,
                        ready: c.get("ready").and_then(|v| v.as_bool()).unwrap_or(false),
                        restart_count: c.get("restartCount").and_then(|v| v.as_i64()).unwrap_or(0)
                            as i32,
                        waiting_reason: json_string(c, "/state/waiting/reason"),
                        waiting_message: json_string(c, "/state/waiting/message")
                            .map(|message| crate::observe::image_pull::safe_excerpt(&message)),
                        last_termination_reason: json_string(c, "/lastState/terminated/reason"),
                        image: spec_image,
                    });
                }
            }
            ObservedPod {
                name: json_string(item, "/metadata/name").unwrap_or_default(),
                uid: json_string(item, "/metadata/uid"),
                created_at: json_time(item, "/metadata/creationTimestamp"),
                replica_set: controller_owner(item, "ReplicaSet"),
                deployment: None,
                phase: json_string(item, "/status/phase").unwrap_or_else(|| "Unknown".into()),
                container_statuses: statuses,
            }
        })
        .collect()
}

fn resolve_deployment_owners(pods: &mut [ObservedPod], raw: &str, namespace: &str) {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(raw) else {
        return;
    };
    let Some(items) = value.get("items").and_then(|v| v.as_array()) else {
        return;
    };
    for pod in pods {
        let Some((rs_name, rs_uid)) = &pod.replica_set else {
            continue;
        };
        let Some(rs) = items.iter().find(|item| {
            item.get("kind").and_then(|v| v.as_str()) == Some("ReplicaSet")
                && json_string(item, "/metadata/name").as_ref() == Some(rs_name)
                && json_string(item, "/metadata/uid").as_ref() == Some(rs_uid)
                && json_string(item, "/metadata/namespace").as_deref() == Some(namespace)
        }) else {
            continue;
        };
        let Some((dep_name, dep_uid)) = controller_owner(rs, "Deployment") else {
            continue;
        };
        if items.iter().any(|item| {
            item.get("kind").and_then(|v| v.as_str()) == Some("Deployment")
                && json_string(item, "/metadata/name").as_deref() == Some(dep_name.as_str())
                && json_string(item, "/metadata/uid").as_deref() == Some(dep_uid.as_str())
                && json_string(item, "/metadata/namespace").as_deref() == Some(namespace)
        }) {
            pod.deployment = Some(dep_name);
        }
    }
}
