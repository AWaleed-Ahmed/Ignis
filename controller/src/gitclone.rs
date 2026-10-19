//! Clone a repository at an exact commit SHA into a disposable workspace (FR-030).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use crate::domain::errors::DomainError;

/// Clone `clone_url` and check out `commit_sha` into a fresh temp directory.
pub fn clone_at_sha(clone_url: &str, commit_sha: &str) -> Result<PathBuf, DomainError> {
    clone_at_sha_with_deadline(
        clone_url,
        commit_sha,
        crate::deadline::Deadline::new(Duration::from_secs(180)),
    )
}

pub fn clone_at_sha_with_deadline(
    clone_url: &str,
    commit_sha: &str,
    deadline: crate::deadline::Deadline,
) -> Result<PathBuf, DomainError> {
    clone_at_sha_internal(clone_url, commit_sha, deadline, false, None)
        .map(|workspace| workspace.keep())
}

pub fn clone_tempdir_at_sha_with_deadline_and_cancel(
    clone_url: &str,
    commit_sha: &str,
    deadline: crate::deadline::Deadline,
    cancelled: &AtomicBool,
) -> Result<tempfile::TempDir, DomainError> {
    clone_at_sha_internal(clone_url, commit_sha, deadline, false, Some(cancelled))
}

fn clone_at_sha_internal(
    clone_url: &str,
    commit_sha: &str,
    deadline: crate::deadline::Deadline,
    force_fallback: bool,
    cancelled: Option<&AtomicBool>,
) -> Result<tempfile::TempDir, DomainError> {
    if clone_url.trim().is_empty() {
        return Err(DomainError::InvalidRequest("clone_url is empty".into()));
    }
    if !(7..=40).contains(&commit_sha.len()) || !commit_sha.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(DomainError::InvalidRequest(
            "commit_sha must be 7..40 hexadecimal characters".into(),
        ));
    }

    let kept = tempfile::Builder::new()
        .prefix("raphael-clone-")
        .tempdir()
        .map_err(|e| DomainError::Internal(e.to_string()))?;
    let workspace = kept.path().to_path_buf();

    run_git(
        &workspace,
        &["init", "--template="],
        deadline.cap(Duration::from_secs(30))?,
        cancelled,
    )?;
    configure_checkout(&workspace, deadline, cancelled)?;
    run_git(
        &workspace,
        &["remote", "add", "origin", clone_url],
        deadline.cap(Duration::from_secs(30))?,
        cancelled,
    )?;

    let fetch = if force_fallback {
        Err(DomainError::Internal(
            "test requested fallback clone".into(),
        ))
    } else {
        run_git(
            &workspace,
            &["fetch", "--depth", "1", "origin", commit_sha],
            deadline.cap(Duration::from_secs(120))?,
            cancelled,
        )
    };
    let fetch = match fetch {
        Err(error @ DomainError::Timeout(_)) => return Err(error),
        result => result,
    };
    if fetch.is_err() {
        let _ = std::fs::remove_dir_all(&workspace);
        std::fs::create_dir_all(&workspace).map_err(|e| DomainError::Internal(e.to_string()))?;
        run_git(
            Path::new("/"),
            &[
                "clone",
                "--template=",
                "--filter=blob:none",
                "--no-checkout",
                clone_url,
                workspace.to_str().unwrap_or("."),
            ],
            deadline.cap(Duration::from_secs(180))?,
            cancelled,
        )?;
        configure_checkout(&workspace, deadline, cancelled)?;
        run_git(
            &workspace,
            &["fetch", "origin", commit_sha],
            deadline.cap(Duration::from_secs(120))?,
            cancelled,
        )?;
    }

    run_git(
        &workspace,
        &["checkout", "--force", commit_sha],
        deadline.cap(Duration::from_secs(60))?,
        cancelled,
    )?;

    let (code, stdout, stderr) = run_git_output(
        &workspace,
        &["rev-parse", "HEAD"],
        deadline.cap(Duration::from_secs(15))?,
        cancelled,
    )?;
    if code != 0 {
        return Err(DomainError::Internal(format!(
            "git rev-parse failed: {stderr}"
        )));
    }
    let head = stdout.trim();
    if !head.starts_with(&commit_sha.to_ascii_lowercase()) {
        return Err(DomainError::Internal(format!(
            "checked out HEAD {head} does not match requested {commit_sha}"
        )));
    }

    tracing::info!(%clone_url, %commit_sha, path = %workspace.display(), "cloned repository at SHA");
    deadline.remaining()?;
    if cancelled.is_some_and(|flag| flag.load(std::sync::atomic::Ordering::SeqCst)) {
        return Err(DomainError::Timeout("clone was cancelled".into()));
    }
    Ok(kept)
}

/// Keep checkout conversion independent of host Git preferences. Repository
/// attributes still control paths with explicit text/eol rules.
fn configure_checkout(
    workspace: &Path,
    deadline: crate::deadline::Deadline,
    cancelled: Option<&AtomicBool>,
) -> Result<(), DomainError> {
    run_git(
        workspace,
        &["config", "core.autocrlf", "false"],
        deadline.cap(Duration::from_secs(15))?,
        cancelled,
    )?;
    run_git(
        workspace,
        &["config", "core.eol", "lf"],
        deadline.cap(Duration::from_secs(15))?,
        cancelled,
    )?;
    let attributes_path = workspace.join(".git/empty-attributes");
    let attributes_path = attributes_path.to_string_lossy().to_string();
    run_git(
        workspace,
        &["config", "core.attributesFile", &attributes_path],
        deadline.cap(Duration::from_secs(15))?,
        cancelled,
    )?;
    std::fs::write(workspace.join(".git/empty-attributes"), "")
        .map_err(|e| DomainError::Internal(e.to_string()))
}

fn run_git(
    cwd: &Path,
    args: &[&str],
    timeout: Duration,
    cancelled: Option<&AtomicBool>,
) -> Result<(), DomainError> {
    let (code, _, stderr) = run_git_output(cwd, args, timeout, cancelled)?;
    if code != 0 {
        return Err(DomainError::Internal(format!(
            "git {} failed: {stderr}",
            args.join(" ")
        )));
    }
    Ok(())
}

fn run_git_output(
    cwd: &Path,
    args: &[&str],
    timeout: Duration,
    cancelled: Option<&AtomicBool>,
) -> Result<(i32, String, String), DomainError> {
    let output = crate::process::output_with_cancel(
        Command::new("git")
            .args(args)
            .current_dir(cwd)
            .env("GIT_ATTR_NOSYSTEM", "1")
            .env_remove("GIT_TEMPLATE_DIR"),
        timeout,
        cancelled,
    )
    .map_err(|e| match e.kind() {
        std::io::ErrorKind::TimedOut => DomainError::Timeout("git command timed out".into()),
        std::io::ErrorKind::NotFound => {
            DomainError::Internal("git binary not found; install git for clone-at-SHA".into())
        }
        _ => DomainError::Internal(e.to_string()),
    })?;
    Ok((
        output.status.code().unwrap_or(1),
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;

    #[test]
    fn preserves_global_authentication_and_transport_configuration() {
        if std::env::var_os("IGNIS_GIT_CONFIG_CHILD").is_some() {
            let directory = tempfile::tempdir().unwrap();
            for (key, expected) in [
                ("credential.helper", "fixture-helper"),
                ("http.proxy", "http://proxy.invalid"),
                ("http.sslCAInfo", "/fixture/ca.pem"),
            ] {
                let (status, value, _) = run_git_output(
                    directory.path(),
                    &["config", "--get", key],
                    Duration::from_secs(5),
                    None,
                )
                .unwrap();
                assert_eq!(status, 0);
                assert_eq!(value.trim(), expected);
            }
            return;
        }
        let directory = tempfile::tempdir().unwrap();
        let config = directory.path().join("gitconfig");
        fs::write(&config, "[credential]\n helper = fixture-helper\n[http]\n proxy = http://proxy.invalid\n sslCAInfo = /fixture/ca.pem\n").unwrap();
        let status = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "gitclone::tests::preserves_global_authentication_and_transport_configuration",
            ])
            .env("IGNIS_GIT_CONFIG_CHILD", "1")
            .env("GIT_CONFIG_GLOBAL", config)
            .status()
            .unwrap();
        assert!(status.success());
    }

    #[test]
    fn rejects_refs_and_option_like_commit_values() {
        for value in [
            "--upload-pack=tool",
            "main~123",
            "123456g",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ] {
            assert!(matches!(
                clone_at_sha("https://example.com/repo.git", value),
                Err(DomainError::InvalidRequest(_))
            ));
        }
    }

    #[test]
    fn rejects_short_sha() {
        let err = clone_at_sha("https://example.com/repo.git", "abc").unwrap_err();
        assert!(err.to_string().contains("commit_sha"));
    }

    #[test]
    fn checkout_policy_preserves_repository_attributes_and_binary_bytes() {
        let source = tempfile::tempdir().unwrap();
        git(source.path(), &["init", "--template="]);
        git(source.path(), &["config", "user.email", "test@example.com"]);
        git(source.path(), &["config", "user.name", "Test"]);
        git(source.path(), &["config", "core.autocrlf", "true"]);
        fs::write(
            source.path().join(".gitattributes"),
            "windows.txt text eol=crlf\nmixed.txt -text\n",
        )
        .unwrap();
        fs::create_dir(source.path().join("nested")).unwrap();
        fs::write(
            source.path().join("nested/.gitattributes"),
            "*.txt text eol=lf\n",
        )
        .unwrap();
        fs::write(source.path().join("plain.txt"), b"one\ntwo\n").unwrap();
        fs::write(source.path().join("windows.txt"), b"one\ntwo\n").unwrap();
        fs::write(source.path().join("mixed.txt"), b"one\r\ntwo\nthree\r\n").unwrap();
        fs::write(source.path().join("nested/lf.txt"), b"one\r\ntwo\n").unwrap();
        fs::write(source.path().join("binary.bin"), b"one\r\ntwo\0\n").unwrap();
        git(source.path(), &["add", "."]);
        fs::write(
            source.path().join("authored-crlf.txt"),
            b"original\r\nbytes\r\n",
        )
        .unwrap();
        fs::write(source.path().join("unicode.txt"), "café\n").unwrap();
        fs::write(source.path().join("no-newline.txt"), "no final newline").unwrap();
        git(
            source.path(),
            &[
                "-c",
                "core.autocrlf=false",
                "add",
                "authored-crlf.txt",
                "unicode.txt",
                "no-newline.txt",
            ],
        );
        git(source.path(), &["commit", "-m", "fixture"]);
        let sha = String::from_utf8(
            Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(source.path())
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_string();

        let workspace = clone_at_sha(source.path().to_str().unwrap(), &sha).unwrap();
        assert_blob_and_checkout_hashes(&workspace);
        assert_eq!(
            fs::read(workspace.join("plain.txt")).unwrap(),
            b"one\ntwo\n"
        );
        assert_eq!(
            fs::read(workspace.join("windows.txt")).unwrap(),
            b"one\r\ntwo\r\n"
        );
        assert_eq!(
            fs::read(workspace.join("mixed.txt")).unwrap(),
            b"one\r\ntwo\nthree\r\n"
        );
        assert_eq!(
            fs::read(workspace.join("nested/lf.txt")).unwrap(),
            b"one\ntwo\n"
        );
        assert_eq!(
            fs::read(workspace.join("binary.bin")).unwrap(),
            b"one\r\ntwo\0\n"
        );
        assert_additional_bytes(&workspace);
        assert_minimal_patch(&workspace);
        fs::remove_dir_all(workspace).unwrap();

        let fallback = clone_at_sha_internal(
            source.path().to_str().unwrap(),
            &sha,
            crate::deadline::Deadline::new(Duration::from_secs(180)),
            true,
            None,
        )
        .unwrap();
        assert_eq!(
            fs::read(fallback.path().join("plain.txt")).unwrap(),
            b"one\ntwo\n"
        );
        assert_eq!(
            fs::read(fallback.path().join("windows.txt")).unwrap(),
            b"one\r\ntwo\r\n"
        );
        assert_eq!(
            fs::read(fallback.path().join("mixed.txt")).unwrap(),
            b"one\r\ntwo\nthree\r\n"
        );
        assert_eq!(
            fs::read(fallback.path().join("nested/lf.txt")).unwrap(),
            b"one\ntwo\n"
        );
        assert_eq!(
            fs::read(fallback.path().join("binary.bin")).unwrap(),
            b"one\r\ntwo\0\n"
        );
        assert_blob_and_checkout_hashes(fallback.path());
        assert_additional_bytes(fallback.path());
        assert_minimal_patch(fallback.path());
        drop(fallback);
    }

    fn assert_blob_and_checkout_hashes(workspace: &Path) {
        use sha2::{Digest, Sha256};
        for (path, blob, checkout) in [
            (
                "plain.txt",
                b"one\ntwo\n".as_slice(),
                b"one\ntwo\n".as_slice(),
            ),
            ("windows.txt", b"one\ntwo\n", b"one\r\ntwo\r\n"),
            (
                "mixed.txt",
                b"one\r\ntwo\nthree\r\n",
                b"one\r\ntwo\nthree\r\n",
            ),
            ("nested/lf.txt", b"one\ntwo\n", b"one\ntwo\n"),
            ("binary.bin", b"one\r\ntwo\0\n", b"one\r\ntwo\0\n"),
            (
                "authored-crlf.txt",
                b"original\r\nbytes\r\n",
                b"original\r\nbytes\r\n",
            ),
            ("unicode.txt", "café\n".as_bytes(), "café\n".as_bytes()),
            ("no-newline.txt", b"no final newline", b"no final newline"),
        ] {
            let output = Command::new("git")
                .args(["show", &format!("HEAD:{path}")])
                .current_dir(workspace)
                .output()
                .unwrap();
            assert!(output.status.success());
            assert_eq!(output.stdout, blob, "blob bytes for {path}");
            assert_eq!(
                Sha256::digest(&output.stdout),
                Sha256::digest(blob),
                "blob hash for {path}"
            );
            let actual = fs::read(workspace.join(path)).unwrap();
            assert_eq!(actual, checkout, "checkout bytes for {path}");
            assert_eq!(
                Sha256::digest(&actual),
                Sha256::digest(checkout),
                "checkout hash for {path}"
            );
        }
    }

    fn assert_additional_bytes(workspace: &Path) {
        assert_eq!(
            fs::read(workspace.join("authored-crlf.txt")).unwrap(),
            b"original\r\nbytes\r\n"
        );
        assert_eq!(
            fs::read(workspace.join("unicode.txt")).unwrap(),
            "café\n".as_bytes()
        );
        assert_eq!(
            fs::read(workspace.join("no-newline.txt")).unwrap(),
            b"no final newline"
        );
    }

    fn assert_minimal_patch(workspace: &Path) {
        let head = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(workspace)
            .output()
            .unwrap();
        let head = String::from_utf8(head.stdout).unwrap();
        fs::write(
            workspace.join("windows.txt"),
            [
                b'o', b'n', b'e', 13, 10, b'c', b'h', b'a', b'n', b'g', b'e', b'd', 13, 10,
            ],
        )
        .unwrap();
        let diff = Command::new("git")
            .args(["diff", "--numstat"])
            .env("GIT_ATTR_NOSYSTEM", "1")
            .current_dir(workspace)
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8(diff.stdout).unwrap(),
            "1\t1\twindows.txt\n"
        );
        let after = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(workspace)
            .output()
            .unwrap();
        assert_eq!(String::from_utf8(after.stdout).unwrap(), head);
    }

    fn git(cwd: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(["-c", "core.attributesFile=/dev/null"])
            .args(args)
            .env("GIT_ATTR_NOSYSTEM", "1")
            .current_dir(cwd)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    #[cfg(unix)]
    #[test]
    fn authenticated_http_clone_uses_retained_credential_helper_in_both_paths() {
        if std::env::var_os("IGNIS_AUTH_HTTP_CHILD").is_some() {
            let url = std::env::var("IGNIS_AUTH_HTTP_URL").unwrap();
            let sha = std::env::var("IGNIS_AUTH_HTTP_SHA").unwrap();
            for fallback in [false, true] {
                let workspace = clone_at_sha_internal(
                    &url,
                    &sha,
                    crate::deadline::Deadline::new(Duration::from_secs(15)),
                    fallback,
                    None,
                )
                .unwrap();
                assert_eq!(
                    fs::read(workspace.path().join("file.txt")).unwrap(),
                    b"owned fixture\n"
                );
                assert_eq!(
                    workspace.path().join(".git/shallow").exists(),
                    !fallback,
                    "test must exercise shallow fetch and full fallback independently"
                );
            }
            return;
        }
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("repo.git");
        fs::create_dir(&source).unwrap();
        git(&source, &["init", "--template="]);
        fs::write(source.join("file.txt"), "owned fixture\n").unwrap();
        git(&source, &["add", "."]);
        git(
            &source,
            &[
                "-c",
                "user.email=fixture@example.invalid",
                "-c",
                "user.name=Fixture",
                "commit",
                "-m",
                "owned",
            ],
        );
        let sha = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&source)
            .output()
            .unwrap();
        let sha = String::from_utf8(sha.stdout).unwrap().trim().to_string();
        let status_path = directory.path().join("http-status.json");
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../harness/git_http_fixture.py");
        let child = Command::new("python3")
            .arg(fixture)
            .arg(directory.path())
            .arg(&status_path)
            .spawn()
            .unwrap();
        struct Server(std::process::Child);
        impl Drop for Server {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let _server = Server(child);
        let started = std::time::Instant::now();
        let status: serde_json::Value = loop {
            if let Ok(bytes) = fs::read(&status_path) {
                if let Ok(value) = serde_json::from_slice(&bytes) {
                    break value;
                }
            }
            assert!(
                started.elapsed() < Duration::from_secs(5),
                "HTTP fixture startup timed out"
            );
            std::thread::sleep(Duration::from_millis(10));
        };
        let helper = directory.path().join("credential-helper");
        let helper_calls = directory.path().join("helper-calls");
        let mut file = fs::File::create(&helper).unwrap();
        writeln!(file, "#!/bin/sh\nprintf '%s\\n' \"$1\" >> '{}'\nif [ \"$1\" = get ]; then printf 'username=fixture-user\\npassword=fixture-pass\\n'; fi", helper_calls.display()).unwrap();
        drop(file);
        fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).unwrap();
        let config = directory.path().join("gitconfig");
        fs::write(
            &config,
            format!(
                "[credential]\n helper = {}\n[http]\n proxy =\n",
                helper.display()
            ),
        )
        .unwrap();
        let url = format!(
            "http://127.0.0.1:{}/repo.git",
            status["port"].as_u64().unwrap()
        );
        let status = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "gitclone::tests::authenticated_http_clone_uses_retained_credential_helper_in_both_paths"])
            .env("IGNIS_AUTH_HTTP_CHILD", "1").env("IGNIS_AUTH_HTTP_URL", url)
            .env("IGNIS_AUTH_HTTP_SHA", sha).env("GIT_CONFIG_GLOBAL", config)
            .env("GIT_TERMINAL_PROMPT", "0").status().unwrap();
        assert!(status.success());
        assert!(fs::read_to_string(helper_calls).unwrap().contains("get"));
        let stats: serde_json::Value =
            serde_json::from_slice(&fs::read(status_path).unwrap()).unwrap();
        assert!(stats["unauthorized"].as_u64().unwrap() > 0);
        assert!(stats["authorized"].as_u64().unwrap() > 0);
        assert!(stats["uploads"].as_u64().unwrap() >= 2);
    }
}
