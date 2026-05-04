/// Thin HTTP client abstraction for testability.
///
/// Returns the HTTP status code on any response (including 4xx/5xx),
/// or an error string for transport-level failures (connection refused,
/// timeout, DNS failure, etc.).
pub trait HttpClient: Send + Sync {
    fn get(&self, url: &str, timeout_ms: u64) -> Result<u16, String>;
}

/// Production implementation using `ureq` — synchronous, no async runtime.
pub struct UreqClient;

impl HttpClient for UreqClient {
    fn get(&self, url: &str, timeout_ms: u64) -> Result<u16, String> {
        let timeout = std::time::Duration::from_millis(timeout_ms);
        match ureq::get(url).timeout(timeout).call() {
            Ok(resp) => Ok(resp.status()),
            // ureq treats 4xx/5xx as Err::Status — extract the code
            Err(ureq::Error::Status(code, _)) => Ok(code),
            Err(e) => Err(e.to_string()),
        }
    }
}

/// Test double: returns a pre-configured sequence of results.
/// Each call pops from the front; panics if more calls than results.
#[cfg(test)]
pub struct MockHttpClient {
    responses: std::sync::Mutex<std::collections::VecDeque<Result<u16, String>>>,
}

#[cfg(test)]
impl MockHttpClient {
    pub fn new(responses: Vec<Result<u16, String>>) -> Self {
        Self { responses: std::sync::Mutex::new(responses.into()) }
    }

    pub fn always_ok(status: u16) -> Self {
        // Pre-load enough responses for any reasonable test
        Self::new(vec![Ok(status); 16])
    }

    pub fn always_err(msg: &str) -> Self {
        Self::new(vec![Err(msg.to_string()); 16])
    }
}

#[cfg(test)]
impl HttpClient for MockHttpClient {
    fn get(&self, _url: &str, _timeout_ms: u64) -> Result<u16, String> {
        self.responses.lock().unwrap().pop_front()
            .expect("MockHttpClient: more calls than configured responses")
    }
}
