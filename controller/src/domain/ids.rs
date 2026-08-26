use regex::Regex;
use sha2::{Digest, Sha256};

use super::errors::DomainError;

pub fn sandbox_id_from_run(run_id: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(run_id.as_bytes());
    let digest = hex::encode(hasher.finalize());
    format!("sb-{}", &digest[..12])
}

pub fn namespace_for_run(run_id: &str) -> Result<String, DomainError> {
    let raw = format!("raphael-run-{}", sanitize_dns_label(run_id));
    let ns = if raw.len() > 63 {
        let mut hasher = Sha256::new();
        hasher.update(run_id.as_bytes());
        let digest = hex::encode(hasher.finalize());
        format!("raphael-run-{}", &digest[..20])
    } else {
        raw
    };
    if ns.is_empty() {
        return Err(DomainError::InvalidRequest(
            "run_id produced empty namespace".into(),
        ));
    }
    Ok(ns)
}

fn sanitize_dns_label(input: &str) -> String {
    let re = Regex::new(r"[^a-z0-9-]").expect("static regex");
    let lower = input.to_ascii_lowercase();
    let cleaned = re.replace_all(&lower, "-");
    let trimmed = cleaned.trim_matches('-');
    if trimmed.is_empty() {
        "run".to_string()
    } else {
        trimmed.chars().take(48).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn namespace_is_dns1123_safe() {
        let ns = namespace_for_run("Run_ABC.123").unwrap();
        assert!(ns.starts_with("raphael-run-"));
        assert!(ns
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'));
    }
}
