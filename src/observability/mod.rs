//! Telemetry entry points, kept as local no-ops.
//!
//! Upstream git-ai shipped errors, performance timings, messages and metric
//! events from here to its own servers. This fork sends nothing anywhere:
//! callers keep their call sites, and every function only writes a debug
//! trace locally.

use std::collections::HashMap;
use std::time::Duration;

use crate::metrics::MetricEvent;

pub mod performance_targets;

pub fn log_error(error: &dyn std::error::Error, _context: Option<serde_json::Value>) {
    tracing::debug!(%error, "git-ai error");
}

pub fn log_performance(
    operation: &str,
    duration: Duration,
    _context: Option<serde_json::Value>,
    _tags: Option<HashMap<String, String>>,
) {
    tracing::debug!(operation, duration_ms = duration.as_millis() as u64, "git-ai timing");
}

#[allow(dead_code)]
pub fn log_message(message: &str, level: &str, _context: Option<serde_json::Value>) {
    tracing::debug!(message, level, "git-ai message");
}

pub fn log_metrics(_events: Vec<MetricEvent>) {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::time::Duration;

    // Test error logging
    #[test]
    fn test_log_error_no_panic() {
        use std::io;
        let error = io::Error::new(io::ErrorKind::NotFound, "test error");
        log_error(&error, None);
    }

    #[test]
    fn test_log_error_with_context() {
        use serde_json::json;
        use std::io;
        let error = io::Error::new(io::ErrorKind::PermissionDenied, "access denied");
        let context = json!({"file": "test.txt", "operation": "read"});
        log_error(&error, Some(context));
    }

    // Test performance logging
    #[test]
    fn test_log_performance_basic() {
        log_performance("test_operation", Duration::from_millis(100), None, None);
    }

    #[test]
    fn test_log_performance_with_context() {
        use serde_json::json;
        let context = json!({"files": 5, "lines": 100});
        log_performance("test_op", Duration::from_secs(1), Some(context), None);
    }

    #[test]
    fn test_log_performance_with_tags() {
        let mut tags = HashMap::new();
        tags.insert("command".to_string(), "commit".to_string());
        tags.insert("repo".to_string(), "test".to_string());
        log_performance("commit_op", Duration::from_millis(500), None, Some(tags));
    }

    // Test message logging
    #[test]
    fn test_log_message_basic() {
        log_message("test message", "info", None);
    }

    #[test]
    fn test_log_message_with_context() {
        use serde_json::json;
        let context = json!({"user": "test", "action": "login"});
        log_message("user logged in", "info", Some(context));
    }

    #[test]
    fn test_log_message_warning() {
        log_message("warning message", "warning", None);
    }

    // Test metrics logging
    #[test]
    fn test_log_metrics_empty() {
        log_metrics(vec![]);
    }

}
