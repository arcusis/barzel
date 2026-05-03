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

    /// Convenience: check whether a command is available and exits successfully.
    fn is_available(&self, cmd: &str, args: &[&str]) -> bool {
        self.run(cmd, args, Path::new("."))
            .map(|o| o.success)
            .unwrap_or(false)
    }
}

/// Production implementation — delegates to std::process::Command.
pub struct OsProcessRunner;

impl SubprocessRunner for OsProcessRunner {
    fn run(&self, cmd: &str, args: &[&str], cwd: &Path) -> io::Result<ProcessOutput> {
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

/// Test double: returns fixed output for any command invocation.
#[cfg(test)]
#[derive(Clone)]
pub struct MockProcessRunner {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

#[cfg(test)]
impl MockProcessRunner {
    pub fn passing(stdout: impl Into<String>) -> Self {
        Self { success: true, stdout: stdout.into(), stderr: String::new() }
    }

    pub fn failing(stdout: impl Into<String>) -> Self {
        Self { success: false, stdout: stdout.into(), stderr: "error output".into() }
    }

    pub fn unavailable() -> Self {
        Self { success: false, stdout: String::new(), stderr: "not found".into() }
    }
}

#[cfg(test)]
impl SubprocessRunner for MockProcessRunner {
    fn run(&self, _cmd: &str, _args: &[&str], _cwd: &Path) -> io::Result<ProcessOutput> {
        Ok(ProcessOutput {
            success: self.success,
            stdout: self.stdout.clone(),
            stderr: self.stderr.clone(),
        })
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
