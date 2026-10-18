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
        assert_additional_bytes(fallback.path());
        assert_minimal_patch(fallback.path());
        drop(fallback);
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
            .args(args)
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
}
