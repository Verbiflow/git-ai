//! Where the daemon's workers put the metric events they build.
//!
//! Upstream handed these to a telemetry worker that stored them in the local
//! metrics database and uploaded them to git-ai's servers. This fork keeps
//! only the local store: attribution recovery reads agent session events back
//! from it, and the database prunes rows outside its retention window on
//! insert. Nothing is uploaded.

use crate::error::GitAiError;
use crate::metrics::MetricEvent;
use crate::metrics::db::MetricsDatabase;

#[derive(Clone, Debug, Default)]
pub struct LocalMetricsSink;

impl LocalMetricsSink {
    /// Store `events` in the local metrics database.
    pub fn persist_metrics_blocking(&self, events: &[MetricEvent]) -> Result<Vec<i64>, GitAiError> {
        if events.is_empty() {
            return Ok(Vec::new());
        }
        let event_jsons: Vec<String> = events
            .iter()
            .map(serde_json::to_string)
            .collect::<Result<_, _>>()?;
        let db = MetricsDatabase::global()?;
        let mut db_lock = db
            .lock()
            .map_err(|_| GitAiError::Generic("metrics DB lock poisoned".to_string()))?;
        db_lock.insert_events(&event_jsons)
    }

    /// Events waiting to be stored. Storage is synchronous, so never any: the
    /// workers' backpressure (sized for an upload backlog) never engages.
    pub fn metrics_buffer_len(&self) -> usize {
        0
    }

    #[cfg(test)]
    pub fn new_noop() -> Self {
        Self
    }
}
