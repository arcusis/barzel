use std::io;
use std::path::Path;

/// The result of spawning a subprocess.
#[derive(Debug, Clone)]
pub struct ProcessOutput {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

impl ProcessOutput {
    pub fn combined(&self) -> String {
        format!("{}\n{}", self.stdout, self.stderr)
    }
}

/// Trait over subprocess execution — injectable for testing.
pub trait SubprocessRunner: Send + Sync {
    fn run(&self, cmd: &str, args: &[&str], cwd: &Path) -> io::Result<ProcessOutput>;

    /// Run a command with a wall-clock timeout (milliseconds).
    /// Returns `Err` with `ErrorKind::TimedOut` if the process does not finish in time.
    /// Default implementation ignores the timeout and delegates to `run()`.
    fn run_timed(&self, cmd: &str, args: &[&str], cwd: &Path, timeout_ms: u64) -> io::Result<ProcessOutput> {
        let _ = timeout_ms;
        self.run(cmd, args, cwd)
    }

    /// Convenience: check whether a command is available and exits successfully.
    fn is_available(&self, cmd: &str, args: &[&str]) -> bool {
        self.run(cmd, args, Path::new("."))
            .map(|o| o.success)
            .unwrap_or(false)
    }
}

/// Production implementation — delegates to std::process::Command.
pub struct OsProcessRunner;

impl OsProcessRunner {
    fn spawn_and_collect(cmd: &str, args: &[&str], cwd: &Path) -> io::Result<ProcessOutput> {
        let output = std::process::Command::new(cmd)
            .args(args)
            .current_dir(cwd)
            .output()?;
        Ok(ProcessOutput {
            success: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        })
    }
}

impl SubprocessRunner for OsProcessRunner {
    fn run(&self, cmd: &str, args: &[&str], cwd: &Path) -> io::Result<ProcessOutput> {
        Self::spawn_and_collect(cmd, args, cwd)
    }

    fn run_timed(&self, cmd: &str, args: &[&str], cwd: &Path, timeout_ms: u64) -> io::Result<ProcessOutput> {
        use std::io::Read;
        use std::process::Stdio;
        use wait_timeout::ChildExt;

        let mut child = std::process::Command::new(cmd)
            .args(args)
            .current_dir(cwd)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        // Drain stdout/stderr in background threads to prevent deadlock when
        // the child produces output that fills the pipe buffer.
        let mut stdout_pipe = child.stdout.take().expect("stdout piped");
        let mut stderr_pipe = child.stderr.take().expect("stderr piped");
        let (tx_out, rx_out) = std::sync::mpsc::channel::<Vec<u8>>();
        let (tx_err, rx_err) = std::sync::mpsc::channel::<Vec<u8>>();
        std::thread::spawn(move || { let mut b = Vec::new(); stdout_pipe.read_to_end(&mut b).ok(); let _ = tx_out.send(b); });
        std::thread::spawn(move || { let mut b = Vec::new(); stderr_pipe.read_to_end(&mut b).ok(); let _ = tx_err.send(b); });

        let timeout = std::time::Duration::from_millis(timeout_ms);
        match child.wait_timeout(timeout)? {
            Some(status) => {
                let stdout = rx_out.recv().unwrap_or_default();
                let stderr = rx_err.recv().unwrap_or_default();
                Ok(ProcessOutput {
                    success: status.success(),
                    stdout: String::from_utf8_lossy(&stdout).into_owned(),
                    stderr: String::from_utf8_lossy(&stderr).into_owned(),
                })
            }
            None => {
                // Timed out — kill the child so it does not leak.
                child.kill().ok();
                child.wait().ok();
                Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("command timed out after {}ms", timeout_ms),
                ))
            }
        }
    }
}

/// Test double: returns a fixed response for every command invocation.
#[cfg(test)]
#[derive(Clone)]
pub struct MockProcessRunner {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
    spawn_error: Option<String>,
}

#[cfg(test)]
impl MockProcessRunner {
    /// Every call returns Ok with success=true and the given stdout.
    pub fn passing(stdout: impl Into<String>) -> Self {
        Self { success: true, stdout: stdout.into(), stderr: String::new(), spawn_error: None }
    }

    /// Every call returns Ok with success=false and the given stdout.
    /// Simulates a process that spawned successfully but exited non-zero.
    pub fn failing(stdout: impl Into<String>) -> Self {
        Self { success: false, stdout: stdout.into(), stderr: "error output".into(), spawn_error: None }
    }

    /// Every call returns Ok with success=false and empty output.
    /// Use `spawn_error()` if you need to simulate a missing binary.
    pub fn unavailable() -> Self {
        Self { success: false, stdout: String::new(), stderr: "not found".into(), spawn_error: None }
    }

    /// Every call returns Err(io::Error::NotFound) — simulates binary not on PATH.
    pub fn spawn_error(msg: impl Into<String>) -> Self {
        Self { success: false, stdout: String::new(), stderr: String::new(), spawn_error: Some(msg.into()) }
    }

    /// Returns a sequence of responses, one per call (in order).
    pub fn sequence(responses: Vec<ProcessOutput>) -> SequentialMock {
        SequentialMock { responses: std::sync::Mutex::new(std::collections::VecDeque::from(responses)) }
    }
}

#[cfg(test)]
impl SubprocessRunner for MockProcessRunner {
    fn run(&self, _cmd: &str, _args: &[&str], _cwd: &Path) -> io::Result<ProcessOutput> {
        if let Some(ref msg) = self.spawn_error {
            return Err(io::Error::new(io::ErrorKind::NotFound, msg.clone()));
        }
        Ok(ProcessOutput {
            success: self.success,
            stdout: self.stdout.clone(),
            stderr: self.stderr.clone(),
        })
    }
}

/// Sequential test double — returns a different response per call.
#[cfg(test)]
pub struct SequentialMock {
    responses: std::sync::Mutex<std::collections::VecDeque<ProcessOutput>>,
}

#[cfg(test)]
impl SubprocessRunner for SequentialMock {
    fn run(&self, _cmd: &str, _args: &[&str], _cwd: &Path) -> io::Result<ProcessOutput> {
        self.responses.lock().unwrap().pop_front()
            .map(Ok)
            .unwrap_or_else(|| Err(io::Error::new(io::ErrorKind::Other, "MockProcessRunner: no more responses")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_passing_is_available() {
        let mock = MockProcessRunner::passing("");
        assert!(mock.is_available("any", &[]));
    }

    #[test]
    fn mock_unavailable_is_not_available() {
        let mock = MockProcessRunner::unavailable();
        assert!(!mock.is_available("any", &[]));
    }

    #[test]
    fn process_output_combined_joins_stdout_and_stderr() {
        let out = ProcessOutput {
            success: true,
            stdout: "hello".into(),
            stderr: "world".into(),
        };
        assert!(out.combined().contains("hello"));
        assert!(out.combined().contains("world"));
    }
}
