//! Daemon-free library seam for Enkidu's Agent Git tool (VER-83).
//!
//! Enkidu embeds git-ai as a *library* and must never run the git-ai
//! background service, its control sockets, its sqlite metrics db, or any
//! telemetry. This module exposes the one checkpoint entry point Enkidu needs,
//! composed from already-daemon-free internals:
//!
//! 1. `commands::checkpoint_agent::orchestrator::execute_preset_checkpoint`
//!    parses an agent hook payload into `CheckpointRequest`s (pure).
//! 2. `daemon::apply_checkpoint_side_effect` discovers the repo, resolves, and
//!    appends to `.git/ai/working_logs/<HEAD sha>/checkpoints.jsonl`. Despite
//!    the module name it performs no IPC; the metrics calls on this path are a
//!    silent no-op unless a git-ai daemon handle is initialized (Enkidu never
//!    initializes one).
//!
//! Commit → `refs/notes/ai` is served by the already-`pub`
//! `authorship::post_commit::post_commit`; no patch is needed for it.

use crate::commands::checkpoint_agent::orchestrator::execute_preset_checkpoint;
use crate::error::GitAiError;

/// Parse an agent hook payload for `preset` (`"claude"` or `"codex"`) and apply
/// it to the local working log, fully in-process. Returns the number of
/// per-repo checkpoint requests applied.
///
/// No background daemon, no sockets, no sqlite, no network. The only external
/// process used is the `git` binary git-ai shells out to for repo discovery and
/// diffing.
pub fn checkpoint_from_hook(preset: &str, hook_input: &str) -> Result<usize, GitAiError> {
    let requests = execute_preset_checkpoint(preset, hook_input)?;
    let applied = requests.len();
    for request in requests {
        crate::daemon::apply_checkpoint_side_effect(request)?;
    }
    Ok(applied)
}
