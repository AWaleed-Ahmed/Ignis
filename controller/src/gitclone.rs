//! Clone a repository at an exact commit SHA into a disposable workspace (FR-030).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use crate::domain::errors::DomainError;

/// Clone `clone_url` and check out `commit_sha` into a fresh temp directory.
pub fn clone_at_sha(clone_url: &str, commit_sha: &str) -> Result<PathBuf, DomainError> {
    if clone_url.trim().is_empty() {
        return Err(DomainError::InvalidRequest("clone_url is empty".into()));
    }
    if commit_sha.len() < 7 {
        return Err(DomainError::InvalidRequest(
            "commit_sha must be at least 7 characters".into(),
        ));
    }

    let kept = tempfile::Builder::new()
        .prefix("raphael-clone-")
        .tempdir()
        .map_err(|e| DomainError::Internal(e.to_string()))?;
    let workspace = kept.keep();

    run_git(&workspace, &["init"], Duration::from_secs(30))?;
    run_git(
        &workspace,
        &["remote", "add", "origin", clone_url],
        Duration::from_secs(30),
    )?;

    if run_git(
        &workspace,
        &["fetch", "--depth", "1", "origin", commit_sha],
        Duration::from_secs(120),
    )
    .is_err()
    {
        let _ = std::fs::remove_dir_all(&workspace);
        std::fs::create_dir_all(&workspace).map_err(|e| DomainError::Internal(e.to_string()))?;
        run_git(
            Path::new("/"),
            &[
                "clone",
                "--filter=blob:none",
                "--no-checkout",
                clone_url,
                workspace.to_str().unwrap_or("."),
            ],
            Duration::from_secs(180),
        )?;
        run_git(
            &workspace,
            &["fetch", "origin", commit_sha],
            Duration::from_secs(120),
        )?;
    }

    run_git(
        &workspace,
        &["checkout", "--force", commit_sha],
        Duration::from_secs(60),
    )?;

    let (code, stdout, stderr) =
        run_git_output(&workspace, &["rev-parse", "HEAD"], Duration::from_secs(15))?;
    if code != 0 {
        return Err(DomainError::Internal(format!(
            "git rev-parse failed: {stderr}"
        )));
    }
    let head = stdout.trim();
    let prefix_len = 7.min(commit_sha.len()).min(head.len());
    if !head.starts_with(commit_sha)
        && !commit_sha.starts_with(&head[..prefix_len])
        && !head.starts_with(&commit_sha[..prefix_len])
    {
        return Err(DomainError::Internal(format!(
            "checked out HEAD {head} does not match requested {commit_sha}"
        )));
    }

    tracing::info!(%clone_url, %commit_sha, path = %workspace.display(), "cloned repository at SHA");
    Ok(workspace)
}

fn run_git(cwd: &Path, args: &[&str], timeout: Duration) -> Result<(), DomainError> {
    let (code, _, stderr) = run_git_output(cwd, args, timeout)?;
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
    _timeout: Duration,
) -> Result<(i32, String, String), DomainError> {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                DomainError::Internal("git binary not found; install git for clone-at-SHA".into())
            } else {
                DomainError::Internal(e.to_string())
            }
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

    #[test]
    fn rejects_short_sha() {
        let err = clone_at_sha("https://example.com/repo.git", "abc").unwrap_err();
        assert!(err.to_string().contains("commit_sha"));
    }
}
