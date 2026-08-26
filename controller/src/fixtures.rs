//! Load and render synthetic Kubernetes Secret fixtures (never production secrets).

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::domain::errors::DomainError;

#[derive(Debug, Deserialize)]
struct FixtureFile {
    name: String,
    #[serde(default)]
    secrets: Vec<FixtureSecret>,
}

#[derive(Debug, Deserialize)]
struct FixtureSecret {
    name: String,
    data: std::collections::HashMap<String, String>,
    #[serde(default)]
    labels: std::collections::HashMap<String, String>,
}

/// Resolve fixtures directory: RAPHAEL_FIXTURES_DIR or <repo>/sandbox/fixtures/secret_fixtures
pub fn fixtures_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("RAPHAEL_FIXTURES_DIR") {
        return PathBuf::from(dir);
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../fixtures/secret_fixtures")
}

pub fn load_secret_fixture_yaml(set_name: &str) -> Result<String, DomainError> {
    let path = fixtures_dir().join(format!("{set_name}.json"));
    load_secret_fixture_yaml_from_path(&path, set_name)
}

fn load_secret_fixture_yaml_from_path(path: &Path, set_name: &str) -> Result<String, DomainError> {
    let raw = std::fs::read_to_string(path).map_err(|e| {
        DomainError::InvalidRequest(format!(
            "secret fixture set `{set_name}` not found at {}: {e}",
            path.display()
        ))
    })?;
    let file: FixtureFile = serde_json::from_str(&raw)
        .map_err(|e| DomainError::InvalidRequest(format!("invalid fixture JSON: {e}")))?;

    let mut docs = Vec::new();
    for secret in file.secrets {
        let mut labels = secret.labels;
        labels
            .entry("raphael.secret_fixture".into())
            .or_insert_with(|| "true".into());
        labels.insert("raphael.fixture_set".into(), file.name.clone());

        let mut label_yaml = String::new();
        for (k, v) in &labels {
            label_yaml.push_str(&format!("    {k}: \"{v}\"\n"));
        }
        let mut data_yaml = String::new();
        for (k, v) in &secret.data {
            let escaped = v.replace('\\', "\\\\").replace('"', "\\\"");
            data_yaml.push_str(&format!("  {k}: \"{escaped}\"\n"));
        }
        docs.push(format!(
            r#"apiVersion: v1
kind: Secret
metadata:
  name: {name}
  labels:
{labels}type: Opaque
stringData:
{data}"#,
            name = secret.name,
            labels = label_yaml,
            data = data_yaml
        ));
    }

    if docs.is_empty() {
        return Err(DomainError::InvalidRequest(format!(
            "fixture set `{set_name}` contains no secrets"
        )));
    }
    Ok(docs.join("---\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_payments_test_fixture() {
        let yaml = load_secret_fixture_yaml("payments-test").expect("load");
        assert!(yaml.contains("kind: Secret"));
        assert!(yaml.contains("raphael.secret_fixture: \"true\""));
        assert!(yaml.contains("payments-db"));
        assert!(yaml.contains("DATABASE_URL"));
    }
}
