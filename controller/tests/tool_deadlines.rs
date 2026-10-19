//! Owned CLI fixtures, not Kubernetes or registry runtime evidence.
#![cfg(target_os = "linux")]

use raphael_sandbox_controller::{
    deadline::Deadline,
    domain::errors::DomainError,
    domain::models::ManifestSpec,
    gitclone::{clone_at_sha_with_deadline, clone_tempdir_at_sha_with_deadline_and_cancel},
    k8s::ClusterBackend,
};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    process::Command,
    time::{Duration, Instant},
};

const WRAPPER: &str = r#"#!/usr/bin/python3
import json, os, pathlib, subprocess, sys, time
root = pathlib.Path(os.environ['BOUND_ROOT'])
mode = os.environ['BOUND_CASE']
tool = pathlib.Path(sys.argv[0]).name
args = sys.argv[1:]
with (root / 'calls').open('a') as f:
    f.write(json.dumps([tool, args, os.getcwd()]) + '\n')
stall = ((tool == 'git' and args[0] == 'fetch') or
         (mode == 'git-fallback' and tool == 'git' and args[0] == 'clone') or
         (mode == 'helm' and tool == 'helm' and args[0] == 'template') or
         (mode == 'kustomize' and tool == 'kustomize') or
         (mode == 'apply' and tool == 'kubectl') or mode == 'versions')
if mode == 'git-fallback' and tool == 'git' and args[0] == 'fetch':
    time.sleep(0.6)
    sys.exit(1)
if stall:
    child = subprocess.Popen(['/bin/sleep', '30'])
    with (root / 'pids').open('a') as f:
        f.write(f'{os.getpid()} {child.pid}\n')
    time.sleep(30)
    (root / 'late').write_text('unexpected completion')
if tool == 'git':
    os.execv(os.environ['BOUND_GIT'], ['git'] + args)
if mode == 'helm' and args[0] == 'lint':
    time.sleep(0.6)
    sys.exit(0)
if mode == 'kustomize' and tool == 'kubectl':
    time.sleep(0.6)
    sys.exit(1)
sys.exit(0)
"#;

#[test]
fn owned_tools_share_deadline_and_kill_descendants() {
    if let Ok(case) = std::env::var("BOUND_CASE") {
        exercise(&case);
        return;
    }
    let git = Command::new("sh")
        .args(["-c", "command -v git"])
        .output()
        .unwrap();
    assert!(git.status.success());
    let git = String::from_utf8(git.stdout).unwrap();
    for case in [
        "git-fetch",
        "git-cancel",
        "git-fallback",
        "helm",
        "kustomize",
        "apply",
        "versions",
    ] {
        let root = tempfile::tempdir().unwrap();
        let bin = root.path().join("bin");
        fs::create_dir(&bin).unwrap();
        for tool in ["git", "helm", "kubectl", "kustomize"] {
            let path = bin.join(tool);
            fs::write(&path, WRAPPER).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "owned_tools_share_deadline_and_kill_descendants",
                "--nocapture",
            ])
            .env("BOUND_CASE", case)
            .env("BOUND_ROOT", root.path())
            .env("BOUND_GIT", git.trim())
            .env(
                "PATH",
                format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
            )
            .env_remove("KUBECONFIG")
            .env_remove("RAPHAEL_KUBE_CONTEXT")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{case}: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            !root.path().join("late").exists(),
            "{case}: late completion"
        );
        let pids = fs::read_to_string(root.path().join("pids")).unwrap();
        for pid in pids.split_whitespace() {
            let stat = fs::read_to_string(format!("/proc/{pid}/stat"));
            assert!(
                stat.is_err() || stat.unwrap().split_whitespace().nth(2) == Some("Z"),
                "{case}: process {pid} survived"
            );
        }
        let calls = fs::read_to_string(root.path().join("calls")).unwrap();
        if case.starts_with("git") {
            for line in calls.lines() {
                let call: serde_json::Value = serde_json::from_str(line).unwrap();
                let cwd = call[2].as_str().unwrap();
                if cwd != "/" {
                    assert!(
                        !std::path::Path::new(cwd).exists(),
                        "clone workspace leaked"
                    );
                }
            }
            assert_eq!(calls.contains("\"clone\""), case == "git-fallback");
        }
        if case == "helm" {
            assert!(calls.contains("\"lint\"") && calls.contains("\"template\""));
        }
        if case == "kustomize" {
            assert!(calls.contains("\"kubectl\"") && calls.contains("\"build\""));
        }
    }
}

fn exercise(case: &str) {
    let root = std::env::var("BOUND_ROOT").unwrap();
    let start = Instant::now();
    let budget = Duration::from_millis(1000);
    let deadline = Deadline::new(budget);
    let result = match case {
        "git-cancel" => {
            let cancelled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let flag = cancelled.clone();
            let marker = format!("{root}/pids");
            let signal = std::thread::spawn(move || {
                let until = Instant::now() + Duration::from_secs(2);
                while !std::path::Path::new(&marker).exists() && Instant::now() < until {
                    std::thread::sleep(Duration::from_millis(5));
                }
                flag.store(true, std::sync::atomic::Ordering::SeqCst);
            });
            let result = clone_tempdir_at_sha_with_deadline_and_cancel(
                "file:///unused-owned-fixture",
                "1234567",
                deadline,
                &cancelled,
            )
            .map(|_| ());
            signal.join().unwrap();
            result
        }
        "git-fetch" | "git-fallback" => {
            clone_at_sha_with_deadline("file:///unused-owned-fixture", "1234567", deadline)
                .map(|_| ())
        }
        "helm" | "kustomize" => {
            fs::create_dir(format!("{root}/chart")).unwrap();
            let manifest: ManifestSpec = serde_json::from_value(
                serde_json::json!({"type":case,"chart":"chart","overlay":"chart"}),
            )
            .unwrap();
            raphael_sandbox_controller::render::render_with_deadline(&root, &manifest, deadline)
                .map(|_| ())
        }
        "apply" => tokio::runtime::Runtime::new().unwrap().block_on(async {
            raphael_sandbox_controller::k8s::kubectl::KubectlCluster::from_env()
                .unwrap()
                .apply_manifests(
                    "owned-proof",
                    "apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: proof\n",
                    budget,
                )
                .await
                .map(|_| ())
        }),
        "versions" => {
            assert!(
                raphael_sandbox_controller::tools::collect_tool_versions_with_deadline(deadline)
                    .is_empty()
            );
            Err(DomainError::Timeout("expected best-effort timeout".into()))
        }
        _ => unreachable!(),
    };
    assert!(
        matches!(result, Err(DomainError::Timeout(_))),
        "{case}: expected timeout"
    );
    if case != "git-cancel" {
        assert!(start.elapsed() >= Duration::from_millis(900));
    }
    assert!(
        start.elapsed() < Duration::from_millis(1400),
        "{case}: budget reset: {:?}",
        start.elapsed()
    );
}
