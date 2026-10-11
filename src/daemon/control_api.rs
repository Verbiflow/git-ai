use crate::authorship::working_log::AgentId;
use crate::commands::checkpoint_agent::bash_tool::StatSnapshot;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "method", content = "params")]
pub enum ControlRequest {
    #[serde(rename = "ping")]
    Ping,
    #[serde(rename = "checkpoint.run")]
    CheckpointRun { body_bytes: u64 },
    #[serde(rename = "sync.family")]
    SyncFamily { repo_working_dir: String },
    #[serde(rename = "status.family")]
    StatusFamily { repo_working_dir: String },
    /// Report ingest loss counters (dropped trace payloads/connections).
    #[serde(rename = "stats.ingest")]
    StatsIngest,
    /// Run one untraced-commit fixup pass now for the repository's family, or
    /// for every family the daemon knows when no repository is given.
    #[serde(rename = "fixup.scan")]
    UntracedFixupScan { repo_working_dir: Option<String> },
    /// Snapshot of the daemon's attribution pipeline: per-family pending work
    /// and fence state, open trace roots, and loss counters.
    #[serde(rename = "status.daemon")]
    StatusDaemon,
    #[serde(rename = "snapshot.watermarks")]
    SnapshotWatermarks { repo_working_dir: String },
    #[serde(rename = "bash_session.start")]
    BashSessionStart {
        repo_work_dir: String,
        original_cwd: Option<String>,
        session_id: String,
        tool_use_id: String,
        agent_id: AgentId,
        metadata: HashMap<String, String>,
        stat_snapshot: Box<StatSnapshot>,
        trace_id: String,
        started_at_ns: u128,
        command: Option<String>,
    },
    #[serde(rename = "bash_session.end")]
    BashSessionEnd {
        repo_work_dir: String,
        original_cwd: Option<String>,
        session_id: String,
        tool_use_id: String,
        agent_id: AgentId,
        metadata: HashMap<String, String>,
        trace_id: String,
        ended_at_ns: u128,
        command: Option<String>,
    },
    #[serde(rename = "bash_session.query")]
    BashSessionQuery { repo_work_dir: String },
    #[serde(rename = "bash_snapshot.query")]
    BashSnapshotQuery {
        session_id: String,
        tool_use_id: String,
    },
    #[serde(rename = "bash_hook_attempt.start")]
    BashHookAttemptStart {
        original_cwd: String,
        discovered_repo_work_dir: Option<String>,
        repo_discovery_error: Option<String>,
        session_id: String,
        tool_use_id: String,
        agent_id: AgentId,
        metadata: HashMap<String, String>,
        trace_id: String,
        started_at_ns: u128,
        command: Option<String>,
    },
    #[serde(rename = "bash_hook_attempt.end")]
    BashHookAttemptEnd {
        original_cwd: String,
        discovered_repo_work_dir: Option<String>,
        repo_discovery_error: Option<String>,
        session_id: String,
        tool_use_id: String,
        agent_id: AgentId,
        metadata: HashMap<String, String>,
        trace_id: String,
        ended_at_ns: u128,
        command: Option<String>,
    },
    /// Wait for the daemon to finish all in-flight work and flush telemetry.
    #[serde(rename = "await")]
    Await { timeout_secs: u64 },
    #[serde(rename = "shutdown")]
    Shutdown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlResponse {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seq: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl ControlResponse {
    pub fn ok(seq: Option<u64>, data: Option<Value>) -> Self {
        Self {
            ok: true,
            seq,
            data,
            error: None,
        }
    }

    pub fn err(msg: impl Into<String>) -> Self {
        Self {
            ok: false,
            seq: None,
            data: None,
            error: Some(msg.into()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BashSessionQueryResponse {
    pub active: bool,
    pub agent_id: Option<AgentId>,
    pub session_id: Option<String>,
    pub tool_use_id: Option<String>,
    pub metadata: Option<HashMap<String, String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BashSnapshotQueryResponse {
    pub found: bool,
    pub stat_snapshot: Option<StatSnapshot>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FamilyStatus {
    pub family_key: String,
    pub latest_seq: u64,
    pub last_error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_daemon_request_uses_stable_wire_shape() {
        assert_eq!(
            serde_json::to_value(ControlRequest::StatusDaemon).unwrap(),
            serde_json::json!({ "method": "status.daemon" })
        );
    }
}
