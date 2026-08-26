use chrono::Utc;
use serde_yaml::Value;

use crate::domain::models::{EvidenceRef, FailureSignature, NormalizedFailure};
use crate::k8s::{ObservedPod, WorkloadObservation};

/// Deterministic analyzer used by both mock synthesis and observe path.
#[derive(Debug, Clone)]
pub struct AnalyzedSignature {
    pub class: String,
    pub key: String,
    pub reason: String,
    pub message: Option<String>,
    pub resource_kind: String,
    pub resource_name: String,
    pub container: Option<String>,
    pub attributes: serde_json::Map<String, serde_json::Value>,
    pub summary: Option<String>,
    pub confidence: f64,
}

pub fn analyze_rendered_yaml(yaml: &str) -> Option<AnalyzedSignature> {
    let mut deployments = Vec::new();
    let mut services = Vec::new();
    let mut configmaps = Vec::new();

    for doc in serde_yaml::Deserializer::from_str(yaml) {
        let Ok(value) = Value::deserialize(doc) else {
            continue;
        };
        let kind = value.get("kind").and_then(|v| v.as_str()).unwrap_or("");
        match kind {
            "Deployment" => deployments.push(value),
            "Service" => services.push(value),
            "ConfigMap" => configmaps.push(value),
            _ => {}
        }
    }

    for dep in &deployments {
        let name = meta_name(dep).unwrap_or("deployment").to_string();
        if let Some(sig) = liveness_too_early(dep, &name) {
            return Some(sig);
        }
        if let Some(sig) = probe_port_mismatch(dep, &name) {
            return Some(sig);
        }
        if let Some(sig) = bad_image(dep, &name) {
            return Some(sig);
        }
        if let Some(sig) = missing_configmap_key(dep, &configmaps, &name) {
            return Some(sig);
        }
        if let Some(sig) = oom_limit(dep, &name) {
            return Some(sig);
        }
    }

    if let Some(sig) = service_port_mismatch(&deployments, &services) {
        return Some(sig);
    }

    // Explicit healthy marker used by fixed scenarios
    if yaml.contains("raphael.scenario/state: healthy") {
        return Some(AnalyzedSignature {
            class: "healthy".into(),
            key: "healthy".into(),
            reason: "Healthy".into(),
            message: Some("workload marked healthy".into()),
            resource_kind: "Deployment".into(),
            resource_name: deployments
                .first()
                .and_then(meta_name)
                .unwrap_or("app")
                .to_string(),
            container: None,
            attributes: serde_json::Map::new(),
            summary: Some("healthy".into()),
            confidence: 1.0,
        });
    }

    None
}

pub fn analyze_observation(
    obs: &WorkloadObservation,
    rendered_yaml: Option<&str>,
) -> FailureSignature {
    if let Some(yaml) = rendered_yaml.or(obs.rendered_hint.as_deref()) {
        if let Some(analyzed) = analyze_rendered_yaml(yaml) {
            return to_signature(analyzed, true, evidence_from_obs(obs));
        }
    }

    // Fall back to live pod waiting reasons
    for pod in &obs.pods {
        for c in &pod.container_statuses {
            if let Some(reason) = &c.waiting_reason {
                if reason == "ImagePullBackOff" || reason == "ErrImagePull" {
                    let mut attrs = serde_json::Map::new();
                    if let Some(img) = &c.image {
                        attrs.insert("image".into(), serde_json::Value::String(img.clone()));
                    }
                    return to_signature(
                        AnalyzedSignature {
                            class: "bad_image_reference".into(),
                            key: format!("bad_image:{}:{}", pod.name, reason),
                            reason: reason.clone(),
                            message: c.waiting_message.clone(),
                            resource_kind: "Pod".into(),
                            resource_name: pod.name.clone(),
                            container: Some(c.name.clone()),
                            attributes: attrs,
                            summary: Some("image pull failure".into()),
                            confidence: 0.9,
                        },
                        true,
                        evidence_from_obs(obs),
                    );
                }
                if reason == "CreateContainerConfigError" {
                    return to_signature(
                        AnalyzedSignature {
                            class: "invalid_missing_config".into(),
                            key: format!("missing_config:{}", pod.name),
                            reason: reason.clone(),
                            message: c.waiting_message.clone(),
                            resource_kind: "Pod".into(),
                            resource_name: pod.name.clone(),
                            container: Some(c.name.clone()),
                            attributes: serde_json::Map::new(),
                            summary: Some("container config error".into()),
                            confidence: 0.9,
                        },
                        true,
                        evidence_from_obs(obs),
                    );
                }
            }
            if c.last_termination_reason.as_deref() == Some("OOMKilled") {
                return to_signature(
                    AnalyzedSignature {
                        class: "resource_constraint".into(),
                        key: format!("oom:{}", pod.name),
                        reason: "OOMKilled".into(),
                        message: Some("container OOMKilled".into()),
                        resource_kind: "Pod".into(),
                        resource_name: pod.name.clone(),
                        container: Some(c.name.clone()),
                        attributes: serde_json::Map::new(),
                        summary: Some("memory limit too low".into()),
                        confidence: 0.85,
                    },
                    true,
                    evidence_from_obs(obs),
                );
            }
        }
    }

    if pods_ready(&obs.pods) {
        return to_signature(
            AnalyzedSignature {
                class: "healthy".into(),
                key: "healthy".into(),
                reason: "Healthy".into(),
                message: Some("all containers ready".into()),
                resource_kind: "Pod".into(),
                resource_name: obs
                    .pods
                    .first()
                    .map(|p| p.name.clone())
                    .unwrap_or_else(|| "unknown".into()),
                container: None,
                attributes: serde_json::Map::new(),
                summary: Some("healthy".into()),
                confidence: 0.8,
            },
            false,
            evidence_from_obs(obs),
        );
    }

    to_signature(
        AnalyzedSignature {
            class: "unknown".into(),
            key: "unknown".into(),
            reason: "Unknown".into(),
            message: Some("could not classify failure".into()),
            resource_kind: "Pod".into(),
            resource_name: obs
                .pods
                .first()
                .map(|p| p.name.clone())
                .unwrap_or_else(|| "unknown".into()),
            container: None,
            attributes: serde_json::Map::new(),
            summary: Some("unknown failure".into()),
            confidence: 0.2,
        },
        false,
        evidence_from_obs(obs),
    )
}

fn pods_ready(pods: &[ObservedPod]) -> bool {
    !pods.is_empty()
        && pods
            .iter()
            .all(|p| p.phase == "Running" && p.container_statuses.iter().all(|c| c.ready))
}

fn to_signature(
    analyzed: AnalyzedSignature,
    reproduced: bool,
    evidence_refs: Vec<EvidenceRef>,
) -> FailureSignature {
    FailureSignature {
        class: analyzed.class,
        key: analyzed.key,
        normalized: NormalizedFailure {
            reason: analyzed.reason,
            message: analyzed.message,
            resource_kind: analyzed.resource_kind,
            resource_name: analyzed.resource_name,
            container: analyzed.container,
            attributes: if analyzed.attributes.is_empty() {
                None
            } else {
                Some(analyzed.attributes)
            },
        },
        reproduced,
        confidence: Some(analyzed.confidence),
        evidence_refs,
        summary: analyzed.summary,
        observed_at: Utc::now(),
    }
}

fn evidence_from_obs(obs: &WorkloadObservation) -> Vec<EvidenceRef> {
    let mut refs = Vec::new();
    for (i, ev) in obs.events.iter().enumerate() {
        refs.push(EvidenceRef {
            kind: "k8s_event".into(),
            id: format!("event-{i}"),
            path: None,
            excerpt: Some(format!("{}: {}", ev.reason, ev.message)),
        });
    }
    for (i, pod) in obs.pods.iter().enumerate() {
        refs.push(EvidenceRef {
            kind: "pod_status".into(),
            id: format!("pod-{i}"),
            path: None,
            excerpt: Some(format!("{} phase={}", pod.name, pod.phase)),
        });
    }
    refs
}

fn meta_name(value: &Value) -> Option<&str> {
    value
        .get("metadata")
        .and_then(|m| m.get("name"))
        .and_then(|v| v.as_str())
}

fn yaml_get<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    let mut cur = value;
    for part in path.trim_start_matches('/').split('/') {
        if part.is_empty() {
            continue;
        }
        cur = match cur {
            Value::Mapping(map) => map.get(Value::String(part.to_string()))?,
            Value::Sequence(seq) => {
                let idx: usize = part.parse().ok()?;
                seq.get(idx)?
            }
            _ => return None,
        };
    }
    Some(cur)
}

fn containers(dep: &Value) -> Vec<&Value> {
    yaml_get(dep, "/spec/template/spec/containers")
        .and_then(|v| v.as_sequence())
        .map(|s| s.iter().collect())
        .unwrap_or_default()
}

fn liveness_too_early(dep: &Value, name: &str) -> Option<AnalyzedSignature> {
    for c in containers(dep) {
        let marked = c
            .get("env")
            .and_then(|e| e.as_sequence())
            .map(|envs| {
                envs.iter().any(|env| {
                    env.get("name").and_then(|v| v.as_str()) == Some("RAPHAEL_LIVENESS_EARLY")
                        && env.get("value").and_then(|v| v.as_str()) == Some("true")
                })
            })
            .unwrap_or(false);

        let initial = yaml_get(c, "/livenessProbe/initialDelaySeconds").and_then(|v| v.as_i64());
        let failure = yaml_get(c, "/livenessProbe/failureThreshold").and_then(|v| v.as_i64());
        let period = yaml_get(c, "/livenessProbe/periodSeconds").and_then(|v| v.as_i64());

        let aggressive = matches!(initial, Some(i) if i <= 0)
            && matches!(failure, Some(f) if f <= 1)
            && matches!(period, Some(p) if p <= 1);

        if marked || aggressive {
            let delay = initial.unwrap_or(0);
            let mut attrs = serde_json::Map::new();
            attrs.insert("initial_delay_seconds".into(), delay.into());
            if let Some(f) = failure {
                attrs.insert("failure_threshold".into(), f.into());
            }
            if let Some(p) = period {
                attrs.insert("period_seconds".into(), p.into());
            }
            return Some(AnalyzedSignature {
                class: "probe_misconfiguration".into(),
                key: format!("liveness_too_early:{name}:delay={delay}"),
                reason: "LivenessProbeTooEarly".into(),
                message: Some(format!(
                    "liveness probe initialDelaySeconds={delay} is too aggressive"
                )),
                resource_kind: "Deployment".into(),
                resource_name: name.to_string(),
                container: c
                    .get("name")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
                attributes: attrs,
                summary: Some("liveness probe starts too early".into()),
                confidence: 0.9,
            });
        }
    }
    None
}

fn probe_port_mismatch(dep: &Value, name: &str) -> Option<AnalyzedSignature> {
    for c in containers(dep) {
        let container_port = c
            .get("ports")
            .and_then(|p| p.as_sequence())
            .and_then(|s| s.first())
            .and_then(|p| p.get("containerPort"))
            .and_then(|v| v.as_i64());
        let probe_port = yaml_get(c, "/readinessProbe/httpGet/port").and_then(|v| match v {
            Value::Number(n) => n.as_i64(),
            Value::String(s) => s.parse().ok(),
            _ => None,
        });
        if let (Some(cp), Some(pp)) = (container_port, probe_port) {
            if cp != pp {
                let mut attrs = serde_json::Map::new();
                attrs.insert("container_port".into(), cp.into());
                attrs.insert("probe_port".into(), pp.into());
                return Some(AnalyzedSignature {
                    class: "probe_misconfiguration".into(),
                    key: format!("probe_port_mismatch:{name}:{cp}!={pp}"),
                    reason: "ReadinessProbePortMismatch".into(),
                    message: Some(format!(
                        "readiness probe port {pp} does not match containerPort {cp}"
                    )),
                    resource_kind: "Deployment".into(),
                    resource_name: name.to_string(),
                    container: c
                        .get("name")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string()),
                    attributes: attrs,
                    summary: Some("readiness probe uses wrong port".into()),
                    confidence: 0.95,
                });
            }
        }
    }
    None
}

fn bad_image(dep: &Value, name: &str) -> Option<AnalyzedSignature> {
    for c in containers(dep) {
        let image = c.get("image").and_then(|v| v.as_str()).unwrap_or("");
        if image.contains("does-not-exist")
            || image.ends_with(":missing")
            || image.contains("invalid.tag")
        {
            let mut attrs = serde_json::Map::new();
            attrs.insert("image".into(), image.into());
            return Some(AnalyzedSignature {
                class: "bad_image_reference".into(),
                key: format!("bad_image:{name}:{image}"),
                reason: "ImagePullBackOff".into(),
                message: Some(format!("bad image reference: {image}")),
                resource_kind: "Deployment".into(),
                resource_name: name.to_string(),
                container: c
                    .get("name")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
                attributes: attrs,
                summary: Some("image tag/repository does not exist".into()),
                confidence: 0.95,
            });
        }
    }
    None
}

fn missing_configmap_key(
    dep: &Value,
    configmaps: &[Value],
    name: &str,
) -> Option<AnalyzedSignature> {
    for c in containers(dep) {
        let env_from_keys: Vec<(String, String)> = c
            .get("env")
            .and_then(|e| e.as_sequence())
            .map(|seq| {
                seq.iter()
                    .filter_map(|env| {
                        let key = yaml_get(env, "/valueFrom/configMapKeyRef/key")?
                            .as_str()?
                            .to_string();
                        let cm = yaml_get(env, "/valueFrom/configMapKeyRef/name")?
                            .as_str()?
                            .to_string();
                        Some((cm, key))
                    })
                    .collect()
            })
            .unwrap_or_default();

        for (cm_name, key) in env_from_keys {
            let present = configmaps.iter().any(|cm| {
                meta_name(cm) == Some(cm_name.as_str())
                    && cm
                        .get("data")
                        .and_then(|d| d.as_mapping())
                        .map(|m| m.contains_key(Value::String(key.clone())))
                        .unwrap_or(false)
            });
            if !present {
                let mut attrs = serde_json::Map::new();
                attrs.insert("configmap".into(), cm_name.clone().into());
                attrs.insert("key".into(), key.clone().into());
                return Some(AnalyzedSignature {
                    class: "invalid_missing_config".into(),
                    key: format!("missing_configmap_key:{cm_name}:{key}"),
                    reason: "CreateContainerConfigError".into(),
                    message: Some(format!("configmap {cm_name} missing key {key}")),
                    resource_kind: "Deployment".into(),
                    resource_name: name.to_string(),
                    container: c
                        .get("name")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string()),
                    attributes: attrs,
                    summary: Some("missing ConfigMap key".into()),
                    confidence: 0.95,
                });
            }
        }
    }
    None
}

fn oom_limit(dep: &Value, name: &str) -> Option<AnalyzedSignature> {
    for c in containers(dep) {
        let limit = yaml_get(c, "/resources/limits/memory")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        // Demo heuristic: extremely low memory like 8Mi / 16Mi flags resource constraint scenario
        if limit == "8Mi" || limit == "16Mi" {
            let mut attrs = serde_json::Map::new();
            attrs.insert("memory_limit".into(), limit.into());
            return Some(AnalyzedSignature {
                class: "resource_constraint".into(),
                key: format!("oom_risk:{name}:{limit}"),
                reason: "OOMKilled".into(),
                message: Some(format!("memory limit {limit} below startup requirement")),
                resource_kind: "Deployment".into(),
                resource_name: name.to_string(),
                container: c
                    .get("name")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
                attributes: attrs,
                summary: Some("memory limit too low".into()),
                confidence: 0.9,
            });
        }
    }
    None
}

fn service_port_mismatch(deployments: &[Value], services: &[Value]) -> Option<AnalyzedSignature> {
    let mut container_ports = Vec::new();
    for dep in deployments {
        for c in containers(dep) {
            if let Some(port) = c
                .get("ports")
                .and_then(|p| p.as_sequence())
                .and_then(|s| s.first())
                .and_then(|p| p.get("containerPort"))
                .and_then(|v| v.as_i64())
            {
                container_ports.push(port);
            }
        }
    }
    for svc in services {
        let name = meta_name(svc).unwrap_or("service").to_string();
        let target = yaml_get(svc, "/spec/ports/0/targetPort").and_then(|v| match v {
            Value::Number(n) => n.as_i64(),
            Value::String(s) => s.parse().ok(),
            _ => None,
        });
        if let Some(tp) = target {
            if !container_ports.is_empty() && !container_ports.contains(&tp) {
                let mut attrs = serde_json::Map::new();
                attrs.insert("target_port".into(), tp.into());
                attrs.insert(
                    "container_ports".into(),
                    serde_json::Value::Array(
                        container_ports
                            .iter()
                            .map(|p| serde_json::json!(p))
                            .collect(),
                    ),
                );
                return Some(AnalyzedSignature {
                    class: "service_port_mismatch".into(),
                    key: format!("service_port_mismatch:{name}:{tp}"),
                    reason: "TargetPortMismatch".into(),
                    message: Some(format!(
                        "service targetPort {tp} does not match container ports"
                    )),
                    resource_kind: "Service".into(),
                    resource_name: name,
                    container: None,
                    attributes: attrs,
                    summary: Some("service targetPort mismatch".into()),
                    confidence: 0.9,
                });
            }
        }
    }
    None
}

use serde::Deserialize;
