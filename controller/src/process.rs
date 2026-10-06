//! Bounded execution of owned tools. Unix children use a separate process group
//! so timeout/cancellation also terminates descendants launched by wrappers.

use std::io;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

struct ProcessGroup(u32);

impl Drop for ProcessGroup {
    fn drop(&mut self) {
        #[cfg(unix)]
        // SAFETY: the child was created in its own process group. A negative PID
        // targets that group only, never the controller's process group.
        unsafe {
            libc::kill(-(self.0 as i32), libc::SIGKILL);
        }
    }
}

pub fn output(command: &mut Command, budget: Duration) -> io::Result<Output> {
    if budget.is_zero() {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "tool deadline exhausted",
        ));
    }
    // Files avoid pipe backpressure while polling the child, including wrappers
    // whose descendants inherit stdout/stderr.
    let stdout = tempfile::tempfile()?;
    let stderr = tempfile::tempfile()?;
    command.stdout(Stdio::from(stdout.try_clone()?));
    command.stderr(Stdio::from(stderr.try_clone()?));
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let deadline = Instant::now() + budget;
    let mut child = command.spawn()?;
    let group = ProcessGroup(child.id());
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(
                    Duration::from_millis(10)
                        .min(deadline.saturating_duration_since(Instant::now())),
                );
            }
            result => {
                drop(group);
                let _ = child.kill();
                let _ = child.wait();
                return Err(match result {
                    Err(error) => error,
                    _ => io::Error::new(io::ErrorKind::TimedOut, "tool deadline exhausted"),
                });
            }
        }
    };
    // Also stop descendants that outlive a successfully completed wrapper.
    drop(group);
    use std::io::{Read, Seek};
    let mut stdout = stdout;
    let mut stderr = stderr;
    stdout.rewind()?;
    stderr.rewind()?;
    let mut out = Vec::new();
    let mut err = Vec::new();
    stdout.read_to_end(&mut out)?;
    stderr.read_to_end(&mut err)?;
    Ok(Output {
        status,
        stdout: out,
        stderr: err,
    })
}

pub async fn output_async(
    command: &mut tokio::process::Command,
    budget: Duration,
) -> io::Result<Output> {
    if budget.is_zero() {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "tool deadline exhausted",
        ));
    }
    command.kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);
    let stdout = tempfile::tempfile()?;
    let stderr = tempfile::tempfile()?;
    command.stdout(Stdio::from(stdout.try_clone()?));
    command.stderr(Stdio::from(stderr.try_clone()?));
    let mut child = command.spawn()?;
    let group = ProcessGroup(
        child
            .id()
            .ok_or_else(|| io::Error::other("tool has no process ID"))?,
    );
    let status = match tokio::time::timeout(budget, child.wait()).await {
        Ok(Ok(status)) => status,
        result => {
            drop(group);
            let _ = child.kill().await;
            let _ = child.wait().await;
            return Err(match result {
                Ok(Err(error)) => error,
                _ => io::Error::new(io::ErrorKind::TimedOut, "tool deadline exhausted"),
            });
        }
    };
    drop(group);
    use std::io::{Read, Seek};
    let mut stdout = stdout;
    let mut stderr = stderr;
    stdout.rewind()?;
    stderr.rewind()?;
    let mut out = Vec::new();
    let mut err = Vec::new();
    stdout.read_to_end(&mut out)?;
    stderr.read_to_end(&mut err)?;
    Ok(Output {
        status,
        stdout: out,
        stderr: err,
    })
}
