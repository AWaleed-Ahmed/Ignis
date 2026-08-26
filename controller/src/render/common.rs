use serde_yaml::Value;

use crate::domain::models::ResourceRef;

pub fn list_resources(yaml: &str) -> Vec<ResourceRef> {
    let mut out = Vec::new();
    for doc in serde_yaml::Deserializer::from_str(yaml) {
        let Ok(value) = Value::deserialize(doc) else {
            continue;
        };
        if value.is_null() {
            continue;
        }
        out.push(ResourceRef {
            kind: value
                .get("kind")
                .and_then(|v| v.as_str())
                .unwrap_or("Unknown")
                .to_string(),
            name: value
                .get("metadata")
                .and_then(|m| m.get("name"))
                .and_then(|v| v.as_str())
                .unwrap_or("unnamed")
                .to_string(),
            api_version: value
                .get("apiVersion")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
        });
    }
    out
}

pub fn extract_images(yaml: &str) -> Vec<String> {
    let mut images = Vec::new();
    for doc in serde_yaml::Deserializer::from_str(yaml) {
        let Ok(value) = Value::deserialize(doc) else {
            continue;
        };
        walk_images(&value, &mut images);
    }
    images.sort();
    images.dedup();
    images
}

fn walk_images(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::Mapping(map) => {
            if let Some(Value::String(img)) = map.get(Value::String("image".into())) {
                out.push(img.clone());
            }
            for v in map.values() {
                walk_images(v, out);
            }
        }
        Value::Sequence(seq) => {
            for v in seq {
                walk_images(v, out);
            }
        }
        _ => {}
    }
}

use serde::Deserialize;
