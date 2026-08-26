//! Inject Pod Security restricted-compatible contexts into workload manifests.

use serde::Deserialize;
use serde_yaml::Value;

use crate::domain::errors::DomainError;

/// Ensure Deployment/Pod/StatefulSet/DaemonSet/Job/CronJob pods can run under PSA restricted.
pub fn inject_restricted_pod_security(yaml: &str) -> Result<String, DomainError> {
    let mut docs: Vec<Value> = Vec::new();
    for doc in serde_yaml::Deserializer::from_str(yaml) {
        let mut value =
            Value::deserialize(doc).map_err(|e| DomainError::RenderFailed(e.to_string()))?;
        inject_doc(&mut value);
        docs.push(value);
    }
    let mut out = String::new();
    for (i, doc) in docs.iter().enumerate() {
        if i > 0 {
            out.push_str("---\n");
        }
        out.push_str(
            &serde_yaml::to_string(doc).map_err(|e| DomainError::RenderFailed(e.to_string()))?,
        );
    }
    Ok(out)
}

fn inject_doc(value: &mut Value) {
    let kind = value
        .get("kind")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    match kind.as_str() {
        "Pod" => {
            if let Some(spec) = map_get_mut(value, "spec") {
                inject_pod_spec(spec);
            }
        }
        "Deployment" | "StatefulSet" | "DaemonSet" | "Job" | "ReplicaSet" => {
            if let Some(spec) = nested_mut(value, &["spec", "template", "spec"]) {
                inject_pod_spec(spec);
            }
        }
        "CronJob" => {
            if let Some(spec) =
                nested_mut(value, &["spec", "jobTemplate", "spec", "template", "spec"])
            {
                inject_pod_spec(spec);
            }
        }
        _ => {}
    }
}

fn map_get_mut<'a>(value: &'a mut Value, key: &str) -> Option<&'a mut Value> {
    match value {
        Value::Mapping(map) => map.get_mut(Value::String(key.into())),
        _ => None,
    }
}

fn nested_mut<'a>(value: &'a mut Value, path: &[&str]) -> Option<&'a mut Value> {
    let mut cur = value;
    for key in path {
        cur = map_get_mut(cur, key)?;
    }
    Some(cur)
}

fn inject_pod_spec(spec: &mut Value) {
    let Value::Mapping(map) = spec else {
        return;
    };
    let pod_sc = map
        .entry(Value::String("securityContext".into()))
        .or_insert_with(|| Value::Mapping(serde_yaml::Mapping::new()));
    if let Value::Mapping(sc) = pod_sc {
        sc.entry(Value::String("runAsNonRoot".into()))
            .or_insert(Value::Bool(true));
        sc.entry(Value::String("runAsUser".into()))
            .or_insert(Value::Number(65534.into()));
        sc.entry(Value::String("runAsGroup".into()))
            .or_insert(Value::Number(65534.into()));
        sc.entry(Value::String("fsGroup".into()))
            .or_insert(Value::Number(65534.into()));
        let seccomp = sc
            .entry(Value::String("seccompProfile".into()))
            .or_insert_with(|| Value::Mapping(serde_yaml::Mapping::new()));
        if let Value::Mapping(sec) = seccomp {
            sec.entry(Value::String("type".into()))
                .or_insert(Value::String("RuntimeDefault".into()));
        }
    }

    for key in ["containers", "initContainers"] {
        let Some(Value::Sequence(containers)) = map.get_mut(Value::String(key.into())) else {
            continue;
        };
        for c in containers.iter_mut() {
            let Value::Mapping(cmap) = c else {
                continue;
            };
            let csc = cmap
                .entry(Value::String("securityContext".into()))
                .or_insert_with(|| Value::Mapping(serde_yaml::Mapping::new()));
            if let Value::Mapping(sc) = csc {
                if sc
                    .get(Value::String("privileged".into()))
                    .and_then(|v| v.as_bool())
                    == Some(true)
                {
                    continue;
                }
                sc.entry(Value::String("allowPrivilegeEscalation".into()))
                    .or_insert(Value::Bool(false));
                sc.entry(Value::String("runAsNonRoot".into()))
                    .or_insert(Value::Bool(true));
                sc.entry(Value::String("runAsUser".into()))
                    .or_insert(Value::Number(65534.into()));
                let caps = sc
                    .entry(Value::String("capabilities".into()))
                    .or_insert_with(|| Value::Mapping(serde_yaml::Mapping::new()));
                if let Value::Mapping(cap) = caps {
                    cap.entry(Value::String("drop".into()))
                        .or_insert(Value::Sequence(vec![Value::String("ALL".into())]));
                }
                let seccomp = sc
                    .entry(Value::String("seccompProfile".into()))
                    .or_insert_with(|| Value::Mapping(serde_yaml::Mapping::new()));
                if let Value::Mapping(sec) = seccomp {
                    sec.entry(Value::String("type".into()))
                        .or_insert(Value::String("RuntimeDefault".into()));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn injects_deployment() {
        let yaml = r#"
apiVersion: apps/v1
kind: Deployment
metadata:
  name: x
spec:
  template:
    spec:
      containers:
        - name: app
          image: hashicorp/http-echo:1.0
"#;
        let out = inject_restricted_pod_security(yaml).unwrap();
        assert!(out.contains("runAsNonRoot"));
        assert!(out.contains("RuntimeDefault"));
        assert!(out.contains("ALL"));
    }
}
