use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::domain::errors::DomainError;
use crate::domain::models::{FailureSignature, ValidationCheck, ValidationPlan, ValidationResults};
use crate::k8s::ClusterBackend;
use chrono::Utc;

pub async fn run_validation(
    backend: &dyn ClusterBackend,
    sandbox_id: &str,
    namespace: &str,
    workspace: Option<&str>,
    plan: &ValidationPlan,
    before_signature: Option<&FailureSignature>,
    after_signature: Option<&FailureSignature>,
    fidelity: Option<&crate::domain::models::FidelityReport>,
) -> Result<ValidationResults, DomainError> {
    let mut checks = Vec::new();
    let mut fail_closed = false;
    let mut failed = false;

    if let Some(commands) = &plan.commands {
        for cmd in commands {
            let start = Instant::now();
            match run_static_command(workspace, cmd) {
                Ok((code, message)) => {
                    let status = if code == 0 { "passed" } else { "failed" };
                    if code != 0 {
                        failed = true;
                    }
                    checks.push(ValidationCheck {
                        name: cmd.clone(),
                        kind: "static".into(),
                        status: status.into(),
                        mandatory: Some(true),
                        duration_ms: start.elapsed().as_millis() as u64,
                        command: Some(cmd.clone()),
                        exit_code: Some(code),
                        message: Some(message),
                        artifact_refs: vec![],
                    });
                }
                Err(DomainError::ValidationUnavailable(msg)) => {
                    fail_closed = true;
                    failed = true;
                    checks.push(ValidationCheck {
                        name: cmd.clone(),
                        kind: "static".into(),
                        status: "unavailable".into(),
                        mandatory: Some(true),
                        duration_ms: start.elapsed().as_millis() as u64,
                        command: Some(cmd.clone()),
                        exit_code: None,
                        message: Some(msg),
                        artifact_refs: vec![],
                    });
                }
                Err(e) => {
                    failed = true;
                    checks.push(ValidationCheck {
                        name: cmd.clone(),
                        kind: "static".into(),
                        status: "failed".into(),
                        mandatory: Some(true),
                        duration_ms: start.elapsed().as_millis() as u64,
                        command: Some(cmd.clone()),
                        exit_code: None,
                        message: Some(e.to_string()),
                        artifact_refs: vec![],
                    });
                }
            }
        }
    }

    if let Some(health_checks) = &plan.health_checks {
        for hc in health_checks {
            let start = Instant::now();
            let timeout = Duration::from_secs(hc.timeout_seconds.unwrap_or(90) as u64);
            match hc.check_type.as_str() {
                "rollout" => {
                    let resource = hc
                        .resource
                        .clone()
                        .unwrap_or_else(|| "deployment/demo".into());
                    match backend.check_rollout(namespace, &resource, timeout).await {
                        Ok(status) => {
                            if !status.ready {
                                failed = true;
                            }
                            checks.push(ValidationCheck {
                                name: format!("rollout:{resource}"),
                                kind: "rollout".into(),
                                status: if status.ready { "passed" } else { "failed" }.into(),
                                mandatory: Some(hc.mandatory),
                                duration_ms: start.elapsed().as_millis() as u64,
                                command: Some(format!("rollout status {resource}")),
                                exit_code: Some(if status.ready { 0 } else { 1 }),
                                message: Some(status.message),
                                artifact_refs: vec![],
                            });
                        }
                        Err(DomainError::ValidationUnavailable(msg)) => {
                            if hc.mandatory {
                                fail_closed = true;
                            }
                            failed = true;
                            checks.push(ValidationCheck {
                                name: format!("rollout:{resource}"),
                                kind: "rollout".into(),
                                status: "unavailable".into(),
                                mandatory: Some(hc.mandatory),
                                duration_ms: start.elapsed().as_millis() as u64,
                                command: None,
                                exit_code: None,
                                message: Some(msg),
                                artifact_refs: vec![],
                            });
                        }
                        Err(e) => {
                            failed = true;
                            checks.push(ValidationCheck {
                                name: format!("rollout:{resource}"),
                                kind: "rollout".into(),
                                status: "failed".into(),
                                mandatory: Some(hc.mandatory),
                                duration_ms: start.elapsed().as_millis() as u64,
                                command: None,
                                exit_code: None,
                                message: Some(e.to_string()),
                                artifact_refs: vec![],
                            });
                        }
                    }
                }
                "http" => {
                    let url = hc.url.clone().unwrap_or_default();
                    let expected = hc.expected_status.unwrap_or(200);
                    match backend
                        .http_health(namespace, &url, expected, timeout)
                        .await
                    {
                        Ok(result) => {
                            if !result.ok {
                                failed = true;
                            }
                            checks.push(ValidationCheck {
                                name: format!("http:{url}"),
                                kind: "health_http".into(),
                                status: if result.ok { "passed" } else { "failed" }.into(),
                                mandatory: Some(hc.mandatory),
                                duration_ms: start.elapsed().as_millis() as u64,
                                command: Some(format!("GET {url}")),
                                exit_code: Some(if result.ok { 0 } else { 1 }),
                                message: Some(result.message),
                                artifact_refs: vec![],
                            });
                        }
                        Err(DomainError::ValidationUnavailable(msg)) => {
                            if hc.mandatory {
                                fail_closed = true;
                            }
                            failed = true;
                            checks.push(ValidationCheck {
                                name: format!("http:{url}"),
                                kind: "health_http".into(),
                                status: "unavailable".into(),
                                mandatory: Some(hc.mandatory),
                                duration_ms: start.elapsed().as_millis() as u64,
                                command: Some(format!("GET {url}")),
                                exit_code: None,
                                message: Some(msg),
                                artifact_refs: vec![],
                            });
                        }
                        Err(e) => {
                            failed = true;
                            checks.push(ValidationCheck {
                                name: format!("http:{url}"),
                                kind: "health_http".into(),
                                status: "failed".into(),
                                mandatory: Some(hc.mandatory),
                                duration_ms: start.elapsed().as_millis() as u64,
                                command: None,
                                exit_code: None,
                                message: Some(e.to_string()),
                                artifact_refs: vec![],
                            });
                        }
                    }
                }
                "signature_absent" => {
                    let before_key = plan
                        .compare_to_signature_key
                        .clone()
                        .or_else(|| before_signature.map(|s| s.key.clone()));
                    let after_key = after_signature.map(|s| s.key.clone());
                    let cleared = match (&before_key, &after_key) {
                        (Some(b), Some(a)) => a == "healthy" || a != b,
                        _ => false,
                    };
                    if !cleared {
                        failed = true;
                    }
                    checks.push(ValidationCheck {
                        name: "signature_compare".into(),
                        kind: "signature_compare".into(),
                        status: if cleared { "passed" } else { "failed" }.into(),
                        mandatory: Some(hc.mandatory),
                        duration_ms: start.elapsed().as_millis() as u64,
                        command: None,
                        exit_code: Some(if cleared { 0 } else { 1 }),
                        message: Some(format!("before={:?} after={:?}", before_key, after_key)),
                        artifact_refs: vec![],
                    });
                }
                other => {
                    fail_closed = true;
                    failed = true;
                    checks.push(ValidationCheck {
                        name: other.to_string(),
                        kind: "static".into(),
                        status: "unavailable".into(),
                        mandatory: Some(hc.mandatory),
                        duration_ms: start.elapsed().as_millis() as u64,
                        command: None,
                        exit_code: None,
                        message: Some(format!("unknown health check type: {other}")),
                        artifact_refs: vec![],
                    });
                }
            }
        }
    }

    let before_signature_key = plan
        .compare_to_signature_key
        .clone()
        .or_else(|| before_signature.map(|s| s.key.clone()));
    let after_signature_key = after_signature.map(|s| s.key.clone());
    let signature_cleared = match (&before_signature_key, &after_signature_key) {
        (Some(b), Some(a)) => Some(a == "healthy" || a != b),
        _ => None,
    };

    // Material fidelity gaps mean we must not claim full validation (PRD §10.3).
    let material_gaps = fidelity
        .map(|f| f.material_gaps.clone())
        .unwrap_or_default();
    let has_material_gaps = !material_gaps.is_empty();
    if has_material_gaps {
        checks.push(ValidationCheck {
            name: "fidelity_full_claim".into(),
            kind: "fidelity".into(),
            status: "failed".into(),
            mandatory: Some(false),
            duration_ms: 0,
            command: None,
            exit_code: Some(1),
            message: Some(format!(
                "material fidelity gaps prevent full validation claim: {}",
                material_gaps.join("; ")
            )),
            artifact_refs: vec![],
        });
    } else {
        checks.push(ValidationCheck {
            name: "fidelity_full_claim".into(),
            kind: "fidelity".into(),
            status: "passed".into(),
            mandatory: Some(false),
            duration_ms: 0,
            command: None,
            exit_code: Some(0),
            message: Some("no material fidelity gaps".into()),
            artifact_refs: vec![],
        });
    }

    let checks_passed = !failed && !fail_closed;
    Ok(ValidationResults {
        sandbox_id: sandbox_id.to_string(),
        passed: checks_passed,
        fail_closed,
        full_validation: checks_passed && !has_material_gaps,
        checks,
        before_signature_key,
        after_signature_key,
        signature_cleared,
        tool_versions: None,
        completed_at: Utc::now(),
    })
}

fn run_static_command(workspace: Option<&str>, cmd: &str) -> Result<(i32, String), DomainError> {
    // Allowlisted static validators only — no arbitrary shell for agent safety.
    let parts: Vec<&str> = cmd.split_whitespace().collect();
    if parts.is_empty() {
        return Err(DomainError::InvalidRequest(
            "empty validation command".into(),
        ));
    }
    let bin = parts[0];
    let allowed = [
        "true",
        "echo",
        "helm",
        "kubeconform",
        "python",
        "python3",
        "test",
    ];
    if !allowed.contains(&bin) {
        return Err(DomainError::ValidationUnavailable(format!(
            "command `{bin}` is not allowlisted; fail closed"
        )));
    }

    let mut command = std::process::Command::new(bin);
    command.args(&parts[1..]);
    if let Some(ws) = workspace {
        command.current_dir(Path::new(ws));
    }
    let output = command
        .output()
        .map_err(|e| DomainError::ValidationUnavailable(e.to_string()))?;
    let code = output.status.code().unwrap_or(1);
    let mut message = String::from_utf8_lossy(&output.stdout).to_string();
    if message.trim().is_empty() {
        message = String::from_utf8_lossy(&output.stderr).to_string();
    }
    Ok((code, message))
}

#[allow(dead_code)]
pub fn workspace_join(workspace: &str, rel: &str) -> PathBuf {
    PathBuf::from(workspace).join(rel)
}
