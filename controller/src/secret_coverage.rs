use serde::Deserialize;
use serde_yaml::Value;

use crate::domain::models::{
    AppliedSecretFixtureInventory, SecretCoverageReport, SecretCoverageStatus,
    SecretReferenceCoverage,
};

const FORMAT_VERSION: u32 = 1;
const MAX_REFERENCES: usize = 256;
const MAX_REPORT_BYTES: usize = 65_536;

pub fn evaluate_secret_coverage(
    yaml: &str,
    sandbox_namespace: &str,
    inventory: Option<&AppliedSecretFixtureInventory>,
) -> SecretCoverageReport {
    let mut references = Vec::new();
    let mut parse_complete = true;
    let mut truncated = false;
    for document in serde_yaml::Deserializer::from_str(yaml) {
        let value = match Value::deserialize(document) {
            Ok(value) => value,
            Err(_) => {
                parse_complete = false;
                break;
            }
        };
        if value.is_null() {
            continue;
        }
        collect_workload_references(
            &value,
            sandbox_namespace,
            &mut references,
            &mut parse_complete,
            &mut truncated,
        );
    }

    let inventory_complete = inventory.is_some_and(|saved| {
        saved.namespace == sandbox_namespace && saved.complete && !saved.truncated
    });
    for reference in &mut references {
        reference.status = if !parse_complete || truncated || !inventory_complete {
            SecretCoverageStatus::Unknown
        } else {
            match &reference.secret_name {
                None => SecretCoverageStatus::Unknown,
                Some(_) if reference.source == "secretKeyRef" && reference.key.is_none() => {
                    SecretCoverageStatus::Unknown
                }
                Some(secret_name) => {
                    let secret = inventory
                        .and_then(|saved| (saved.namespace == reference.namespace).then_some(saved))
                        .and_then(|saved| {
                            saved.secrets.iter().find(|item| item.name == *secret_name)
                        });
                    match secret {
                        None => SecretCoverageStatus::MissingObject,
                        Some(_) if reference.key.is_none() => SecretCoverageStatus::Covered,
                        Some(secret)
                            if secret
                                .keys
                                .iter()
                                .any(|item| Some(item) == reference.key.as_ref()) =>
                        {
                            SecretCoverageStatus::Covered
                        }
                        Some(_) => SecretCoverageStatus::MissingKey,
                    }
                }
            }
        };
    }
    let complete = parse_complete
        && !truncated
        && (inventory_complete || references.is_empty())
        && references
            .iter()
            .all(|item| item.status != SecretCoverageStatus::Unknown);
    let mut report = SecretCoverageReport {
        format_version: FORMAT_VERSION,
        complete,
        truncated,
        references,
    };
    bound_report_bytes(&mut report);
    report
}

fn collect_workload_references(
    document: &Value,
    sandbox_namespace: &str,
    references: &mut Vec<SecretReferenceCoverage>,
    complete: &mut bool,
    truncated: &mut bool,
) {
    let kind = string_at(document, &["kind"]).unwrap_or("");
    let pod_spec = match kind {
        "Pod" => document.get("spec"),
        "Deployment" | "StatefulSet" | "DaemonSet" | "Job" => document
            .get("spec")
            .and_then(|spec| spec.get("template"))
            .and_then(|template| template.get("spec")),
        "CronJob" => document
            .get("spec")
            .and_then(|spec| spec.get("jobTemplate"))
            .and_then(|job| job.get("spec"))
            .and_then(|spec| spec.get("template"))
            .and_then(|template| template.get("spec")),
        "List" => {
            *complete = false;
            return;
        }
        _ if document
            .get("spec")
            .and_then(|spec| spec.get("template"))
            .and_then(|template| template.get("spec"))
            .is_some() =>
        {
            *complete = false;
            return;
        }
        _ => return,
    };
    let Some(pod_spec) = pod_spec.and_then(Value::as_mapping) else {
        *complete = false;
        return;
    };
    let workload_kind = kind.to_string();
    let workload_name = string_at(document, &["metadata", "name"])
        .unwrap_or("")
        .to_string();
    let namespace = string_at(document, &["metadata", "namespace"])
        .unwrap_or(sandbox_namespace)
        .to_string();
    if workload_name.is_empty() {
        *complete = false;
    }

    for (field, owner_kind) in [
        ("containers", "container"),
        ("initContainers", "init_container"),
    ] {
        if has(pod_spec, field) {
            let Some(containers) = array(pod_spec, field) else {
                *complete = false;
                continue;
            };
            for container in containers {
                let owner_name = string_at(container, &["name"]).map(str::to_string);
                if owner_name.is_none() {
                    *complete = false;
                }
                collect_container_references(
                    container,
                    &workload_kind,
                    &workload_name,
                    &namespace,
                    owner_kind,
                    owner_name,
                    references,
                    complete,
                    truncated,
                );
            }
        }
    }
    if has(pod_spec, "ephemeralContainers") {
        *complete = false;
    }
    if has(pod_spec, "volumes") {
        let Some(volumes) = array(pod_spec, "volumes") else {
            *complete = false;
            return;
        };
        for volume in volumes {
            collect_volume_references(
                volume,
                &workload_kind,
                &workload_name,
                &namespace,
                references,
                complete,
                truncated,
            );
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn collect_container_references(
    container: &Value,
    workload_kind: &str,
    workload_name: &str,
    namespace: &str,
    owner_kind: &str,
    owner_name: Option<String>,
    references: &mut Vec<SecretReferenceCoverage>,
    complete: &mut bool,
    truncated: &mut bool,
) {
    if has_value(container, "env") {
        let Some(env) = array_value(container, "env") else {
            *complete = false;
            return;
        };
        for variable in env {
            let Some(source) = path(variable, &["valueFrom", "secretKeyRef"]) else {
                continue;
            };
            add_reference(
                references,
                truncated,
                SecretReferenceCoverage {
                    workload_kind: workload_kind.to_string(),
                    workload_name: workload_name.to_string(),
                    namespace: namespace.to_string(),
                    source: "secretKeyRef".into(),
                    owner_kind: Some(owner_kind.to_string()),
                    owner_name: owner_name.clone(),
                    secret_name: string_at(source, &["name"]).map(str::to_string),
                    key: string_at(source, &["key"]).map(str::to_string),
                    optional: bool_at(source, "optional").unwrap_or(false),
                    status: SecretCoverageStatus::Unknown,
                },
            );
            if string_at(source, &["name"]).is_none() || string_at(source, &["key"]).is_none() {
                *complete = false;
            }
        }
    }
    if has_value(container, "envFrom") {
        let Some(env_from) = array_value(container, "envFrom") else {
            *complete = false;
            return;
        };
        for source in env_from {
            let Some(secret_ref) = path(source, &["secretRef"]) else {
                continue;
            };
            add_reference(
                references,
                truncated,
                SecretReferenceCoverage {
                    workload_kind: workload_kind.to_string(),
                    workload_name: workload_name.to_string(),
                    namespace: namespace.to_string(),
                    source: "envFrom.secretRef".into(),
                    owner_kind: Some(owner_kind.to_string()),
                    owner_name: owner_name.clone(),
                    secret_name: string_at(secret_ref, &["name"]).map(str::to_string),
                    key: None,
                    optional: bool_at(secret_ref, "optional").unwrap_or(false),
                    status: SecretCoverageStatus::Unknown,
                },
            );
            if string_at(secret_ref, &["name"]).is_none() {
                *complete = false;
            }
        }
    }
}

fn collect_volume_references(
    volume: &Value,
    workload_kind: &str,
    workload_name: &str,
    namespace: &str,
    references: &mut Vec<SecretReferenceCoverage>,
    complete: &mut bool,
    truncated: &mut bool,
) {
    let volume_name = string_at(volume, &["name"]).map(str::to_string);
    if let Some(secret) = path(volume, &["secret"]) {
        add_volume_secret(
            secret,
            "volumes.secret",
            workload_kind,
            workload_name,
            namespace,
            volume_name.clone(),
            references,
            complete,
            truncated,
        );
    }
    if let Some(projected) = path(volume, &["projected"]) {
        let Some(sources) = array_value(projected, "sources") else {
            *complete = false;
            return;
        };
        for source in sources {
            if let Some(secret) = path(source, &["secret"]) {
                add_volume_secret(
                    secret,
                    "volumes.projected.secret",
                    workload_kind,
                    workload_name,
                    namespace,
                    volume_name.clone(),
                    references,
                    complete,
                    truncated,
                );
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn add_volume_secret(
    secret: &Value,
    source_kind: &str,
    workload_kind: &str,
    workload_name: &str,
    namespace: &str,
    volume_name: Option<String>,
    references: &mut Vec<SecretReferenceCoverage>,
    complete: &mut bool,
    truncated: &mut bool,
) {
    let name_field = if source_kind == "volumes.secret" {
        "secretName"
    } else {
        "name"
    };
    let secret_name = string_at(secret, &[name_field]).map(str::to_string);
    let optional = bool_at(secret, "optional").unwrap_or(false);
    let keys = if has_value(secret, "items") {
        let Some(items) = array_value(secret, "items") else {
            *complete = false;
            return;
        };
        if items.is_empty() {
            vec![None]
        } else {
            items
                .iter()
                .map(|item| string_at(item, &["key"]).map(str::to_string))
                .collect()
        }
    } else {
        vec![None]
    };
    for key in keys {
        if secret_name.is_none()
            || has_value(secret, "items")
                && key.is_none()
                && !array_value(secret, "items").is_some_and(|items| items.is_empty())
        {
            *complete = false;
        }
        add_reference(
            references,
            truncated,
            SecretReferenceCoverage {
                workload_kind: workload_kind.to_string(),
                workload_name: workload_name.to_string(),
                namespace: namespace.to_string(),
                source: source_kind.to_string(),
                owner_kind: Some("volume".into()),
                owner_name: volume_name.clone(),
                secret_name: secret_name.clone(),
                key,
                optional,
                status: SecretCoverageStatus::Unknown,
            },
        );
    }
}

fn add_reference(
    references: &mut Vec<SecretReferenceCoverage>,
    truncated: &mut bool,
    reference: SecretReferenceCoverage,
) {
    if references.len() >= MAX_REFERENCES {
        *truncated = true;
    } else {
        references.push(reference);
    }
}

fn bound_report_bytes(report: &mut SecretCoverageReport) {
    while serde_json::to_vec(report).map_or(true, |bytes| bytes.len() > MAX_REPORT_BYTES) {
        let Some(_) = report.references.pop() else {
            report.complete = false;
            report.truncated = true;
            return;
        };
        report.complete = false;
        report.truncated = true;
        for reference in &mut report.references {
            reference.status = SecretCoverageStatus::Unknown;
        }
    }
}

fn path<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a Value> {
    keys.iter()
        .try_fold(value, |current, key| current.get(*key))
}

fn string_at<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a str> {
    path(value, keys).and_then(Value::as_str)
}

fn bool_at(value: &Value, key: &str) -> Option<bool> {
    value.get(key).and_then(Value::as_bool)
}

fn has(mapping: &serde_yaml::Mapping, key: &str) -> bool {
    mapping.contains_key(Value::String(key.to_string()))
}

fn array<'a>(mapping: &'a serde_yaml::Mapping, key: &str) -> Option<&'a [Value]> {
    mapping
        .get(Value::String(key.to_string()))
        .and_then(Value::as_sequence)
        .map(Vec::as_slice)
}

fn has_value(value: &Value, key: &str) -> bool {
    value.get(key).is_some()
}

fn array_value<'a>(value: &'a Value, key: &str) -> Option<&'a [Value]> {
    value
        .get(key)
        .and_then(Value::as_sequence)
        .map(Vec::as_slice)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::models::{AppliedSecretFixture, AppliedSecretFixtureInventory};

    #[test]
    fn no_references_need_no_inventory_but_lists_remain_unknown() {
        let yaml = deployment("      containers:\n      - name: app\n        image: busybox\n");
        assert!(evaluate(&yaml, None).complete);
        assert!(!evaluate("kind: List\nitems: []\n", Some(&inventory(vec![]))).complete);
    }

    fn inventory(secrets: Vec<AppliedSecretFixture>) -> AppliedSecretFixtureInventory {
        AppliedSecretFixtureInventory {
            namespace: "sandbox-a".into(),
            fixture_set: Some("payments-test".into()),
            complete: true,
            truncated: false,
            secrets,
        }
    }

    fn secret(name: &str, keys: &[&str]) -> AppliedSecretFixture {
        AppliedSecretFixture {
            name: name.into(),
            keys: keys.iter().map(|key| (*key).into()).collect(),
        }
    }

    fn deployment(pod_spec: &str) -> String {
        format!(
            "apiVersion: apps/v1\nkind: Deployment\nmetadata:\n  name: api\nspec:\n  template:\n    spec:\n{pod_spec}"
        )
    }

    fn evaluate(
        yaml: &str,
        inventory: Option<&AppliedSecretFixtureInventory>,
    ) -> SecretCoverageReport {
        evaluate_secret_coverage(yaml, "sandbox-a", inventory)
    }

    #[test]
    fn identifies_covered_missing_object_and_missing_key() {
        let yaml = deployment(
            "      containers:\n      - name: api\n        env:\n        - name: DB\n          valueFrom:\n            secretKeyRef:\n              name: db\n              key: URL\n        - name: TOKEN\n          valueFrom:\n            secretKeyRef:\n              name: absent\n              key: token\n        - name: PASS\n          valueFrom:\n            secretKeyRef:\n              name: db\n              key: PASSWORD\n",
        );
        let saved = inventory(vec![secret("db", &["URL"])]);
        let report = evaluate(&yaml, Some(&saved));
        assert!(report.complete, "{report:?}");
        assert_eq!(
            report
                .references
                .iter()
                .map(|reference| &reference.status)
                .collect::<Vec<_>>(),
            vec![
                &SecretCoverageStatus::Covered,
                &SecretCoverageStatus::MissingObject,
                &SecretCoverageStatus::MissingKey,
            ]
        );
    }

    #[test]
    fn handles_env_from_secret_and_optional_absence() {
        let yaml = deployment(
            "      containers:\n      - name: api\n        envFrom:\n        - secretRef:\n            name: optional-secret\n            optional: true\n",
        );
        let report = evaluate(&yaml, Some(&inventory(vec![])));
        assert!(report.complete);
        assert_eq!(
            report.references[0].status,
            SecretCoverageStatus::MissingObject
        );
        assert!(report.references[0].optional);
    }

    #[test]
    fn evaluates_secret_volumes_and_projected_items() {
        let yaml = deployment(
            "      containers:\n      - name: api\n      volumes:\n      - name: direct\n        secret:\n          secretName: tls\n          items:\n          - key: cert\n          - key: key\n      - name: projected\n        projected:\n          sources:\n          - secret:\n              name: tls\n              items:\n              - key: cert\n",
        );
        let report = evaluate(&yaml, Some(&inventory(vec![secret("tls", &["cert"])])));
        assert!(report.complete);
        assert_eq!(report.references.len(), 3);
        assert_eq!(report.references[0].status, SecretCoverageStatus::Covered);
        assert_eq!(
            report.references[1].status,
            SecretCoverageStatus::MissingKey
        );
        assert_eq!(report.references[2].source, "volumes.projected.secret");
        assert_eq!(report.references[2].status, SecretCoverageStatus::Covered);
    }

    #[test]
    fn includes_init_container_references_and_owner_identity() {
        let yaml = deployment(
            "      initContainers:\n      - name: migrate\n        env:\n        - name: TOKEN\n          valueFrom:\n            secretKeyRef:\n              name: db\n              key: URL\n      containers:\n      - name: api\n",
        );
        let report = evaluate(&yaml, Some(&inventory(vec![secret("db", &["URL"])])));
        assert!(report.complete);
        assert_eq!(
            report.references[0].owner_kind.as_deref(),
            Some("init_container")
        );
        assert_eq!(report.references[0].owner_name.as_deref(), Some("migrate"));
    }

    #[test]
    fn old_incomplete_or_wrong_namespace_inventory_is_unknown() {
        let yaml = deployment(
            "      containers:\n      - name: api\n        env:\n        - name: DB\n          valueFrom:\n            secretKeyRef:\n              name: db\n              key: URL\n",
        );
        assert_eq!(
            evaluate(&yaml, None).references[0].status,
            SecretCoverageStatus::Unknown
        );
        let mut saved = inventory(vec![secret("db", &["URL"])]);
        saved.complete = false;
        assert_eq!(
            evaluate(&yaml, Some(&saved)).references[0].status,
            SecretCoverageStatus::Unknown
        );
        saved.complete = true;
        saved.namespace = "other".into();
        assert_eq!(
            evaluate(&yaml, Some(&saved)).references[0].status,
            SecretCoverageStatus::Unknown
        );
    }

    #[test]
    fn honors_manifest_namespace_in_fixture_matching() {
        let yaml = deployment(
            "      containers:\n      - name: api\n        envFrom:\n        - secretRef:\n            name: db\n",
        )
        .replace(
            "metadata:\n  name: api",
            "metadata:\n  name: api\n  namespace: other",
        );
        let report = evaluate(&yaml, Some(&inventory(vec![secret("db", &[])])));
        assert!(report.complete, "{report:?}");
        assert_eq!(report.references[0].namespace, "other");
        assert_eq!(
            report.references[0].status,
            SecretCoverageStatus::MissingObject
        );
    }

    #[test]
    fn unsupported_shapes_and_invalid_yaml_are_incomplete() {
        let unsupported =
            deployment("      ephemeralContainers:\n      - name: debug\n        image: busybox\n");
        assert!(!evaluate(&unsupported, Some(&inventory(vec![]))).complete);
        assert!(!evaluate("kind: Deployment\nspec: [", Some(&inventory(vec![]))).complete);
    }

    #[test]
    fn reports_truncation_at_reference_and_serialized_size_limits() {
        let mut env = String::new();
        for index in 0..=MAX_REFERENCES {
            env.push_str(&format!(
                "        - name: ITEM{index}\n          valueFrom:\n            secretKeyRef:\n              name: db\n              key: URL\n"
            ));
        }
        let yaml = deployment(&format!(
            "      containers:\n      - name: api\n        env:\n{env}"
        ));
        let saved = inventory(vec![secret("db", &["URL"])]);
        let report = evaluate(&yaml, Some(&saved));
        assert!(report.truncated);
        assert!(!report.complete);
        assert_eq!(report.references.len(), MAX_REFERENCES);
        assert!(report
            .references
            .iter()
            .all(|reference| reference.status == SecretCoverageStatus::Unknown));

        let long_name = "n".repeat(MAX_REPORT_BYTES + 1);
        let huge_yaml = deployment(&format!(
            "      containers:\n      - name: api\n        envFrom:\n        - secretRef:\n            name: {long_name}\n"
        ));
        let report = evaluate(&huge_yaml, Some(&inventory(vec![])));
        assert!(report.truncated);
        assert!(!report.complete);
        assert!(serde_json::to_vec(&report).unwrap().len() <= MAX_REPORT_BYTES);
    }
}
