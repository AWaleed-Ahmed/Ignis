use serde_yaml::Value;

use crate::domain::errors::DomainError;

/// Reject privileged / host access patterns before apply.
pub fn check_manifest_policy(yaml: &str) -> Result<(), DomainError> {
    for doc in serde_yaml::Deserializer::from_str(yaml) {
        let value =
            Value::deserialize(doc).map_err(|e| DomainError::RenderFailed(e.to_string()))?;
        if value.is_null() {
            continue;
        }
        if let Some(msg) = find_violation(&value) {
            return Err(DomainError::PolicyBlocked(msg));
        }
    }
    Ok(())
}

fn find_violation(value: &Value) -> Option<String> {
    match value {
        Value::Mapping(map) => {
            if let Some(Value::Bool(true)) = map.get(Value::String("privileged".into())) {
                return Some("privileged containers are blocked".into());
            }
            if let Some(Value::Bool(true)) = map.get(Value::String("hostNetwork".into())) {
                return Some("hostNetwork is blocked".into());
            }
            if let Some(Value::Bool(true)) = map.get(Value::String("hostPID".into())) {
                return Some("hostPID is blocked".into());
            }
            if map.contains_key(Value::String("hostPath".into())) {
                return Some("hostPath volumes are blocked".into());
            }
            // Secret kind payloads should never be applied from production copies
            if map.get(Value::String("kind".into())) == Some(&Value::String("Secret".into())) {
                // Allow only explicitly labeled synthetic fixtures
                let synthetic = map
                    .get(Value::String("metadata".into()))
                    .and_then(|m| m.get("labels"))
                    .and_then(|l| l.get("raphael.secret_fixture"))
                    .and_then(|v| v.as_str())
                    == Some("true");
                if !synthetic {
                    return Some("Secret objects require raphael.secret_fixture=true label".into());
                }
            }
            for v in map.values() {
                if let Some(msg) = find_violation(v) {
                    return Some(msg);
                }
            }
            None
        }
        Value::Sequence(seq) => {
            for v in seq {
                if let Some(msg) = find_violation(v) {
                    return Some(msg);
                }
            }
            None
        }
        _ => None,
    }
}

use serde::Deserialize;
