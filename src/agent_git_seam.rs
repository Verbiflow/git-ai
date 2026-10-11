//! Daemon-free library seam for Enkidu's Agent Git tool (VER-83).
//!
//! Enkidu embeds git-ai as a *library* and must never run the git-ai
//! background service, its control sockets, its sqlite metrics db, or any
//! telemetry. This module exposes the one checkpoint entry point Enkidu needs,
//! composed from already-daemon-free internals:
//!
//! 1. The agent's preset parses the hook payload into events, and
//!    `commands::checkpoint_agent::orchestrator::execute_event` turns each
//!    into `CheckpointRequest`s (pure).
//! 2. `daemon::apply_checkpoint_side_effect` discovers the repo, resolves, and
//!    appends to `.git/ai/working_logs/<HEAD sha>/checkpoints.jsonl`. Despite
//!    the module name it performs no IPC; the metrics calls on this path are a
//!    silent no-op unless a git-ai daemon handle is initialized (Enkidu never
//!    initializes one).
//!
//! Commit → `refs/notes/ai` is served by the already-`pub`
//! `authorship::post_commit::post_commit`; no patch is needed for it.

use crate::authorship::authorship_log_serialization::generate_trace_id;
use crate::commands::checkpoint_agent::orchestrator::execute_event;
use crate::commands::checkpoint_agent::presets::{ParsedHookEvent, resolve_preset};
use crate::commands::checkpoint_agent::tool_claims::{self, ToolClaim};
use crate::error::GitAiError;
use std::path::Path;

/// Parse an agent hook payload for `preset` (`"claude"` or `"codex"`) and apply
/// it to the local working log, fully in-process. Returns the number of
/// per-repo checkpoint requests applied.
///
/// No background daemon, no sockets, no sqlite, no network. The only external
/// process used is the `git` binary git-ai shells out to for repo discovery and
/// diffing.
///
/// `state_dir` holds what a tool call's pre hook leaves for its post hook (the
/// daemon that normally holds it never runs here): the Bash tool's stat
/// snapshot, so files an agent changes through shell commands are credited to
/// it, and every call's baseline, so its post checkpoint credits only the
/// lines the call wrote (VER-84, see `tool_claims`).
pub fn checkpoint_from_hook(
    preset: &str,
    hook_input: &str,
    state_dir: &Path,
) -> Result<usize, GitAiError> {
    crate::commands::checkpoint_agent::bash_tool::enable_embedded_bash_sessions(
        state_dir.to_path_buf(),
    );
    let trace_id = generate_trace_id();
    let events = resolve_preset(preset)?.parse(hook_input, &trace_id)?;
    let data: serde_json::Value = serde_json::from_str(hook_input).unwrap_or_default();

    let mut applied = 0;
    for event in events {
        let call = ToolCall::of(&event);
        let claim = match &call {
            Some(call) if !call.is_pre => Some(ToolClaim {
                written: if call.is_bash {
                    None
                } else {
                    tool_claims::written_text(preset, &data)
                },
                baseline: tool_claims::take_baseline(
                    state_dir,
                    &call.session_id,
                    &call.tool_use_id,
                ),
            }),
            _ => None,
        };
        for request in execute_event(event, preset)? {
            crate::daemon::apply_checkpoint_side_effect_with_claim(request, claim.as_ref())?;
            applied += 1;
        }
        if let Some(call) = call.filter(|call| call.is_pre)
            && let Err(error) = tool_claims::record_baseline(
                state_dir,
                &call.session_id,
                &call.tool_use_id,
                &call.cwd,
            )
        {
            tracing::debug!("agent git: no baseline for {}: {}", call.tool_use_id, error);
        }
    }
    Ok(applied)
}

/// The tool call a hook event belongs to.
struct ToolCall {
    session_id: String,
    tool_use_id: String,
    cwd: std::path::PathBuf,
    is_pre: bool,
    is_bash: bool,
}

impl ToolCall {
    fn of(event: &ParsedHookEvent) -> Option<Self> {
        let (context, tool_use_id, is_pre, is_bash) = match event {
            ParsedHookEvent::PreFileEdit(e) => (&e.context, e.tool_use_id.clone()?, true, false),
            ParsedHookEvent::PostFileEdit(e) => (&e.context, e.tool_use_id.clone()?, false, false),
            ParsedHookEvent::PreBashCall(e) => (&e.context, e.tool_use_id.clone(), true, true),
            ParsedHookEvent::PostBashCall(e) => (&e.context, e.tool_use_id.clone(), false, true),
            ParsedHookEvent::KnownHumanEdit(_) | ParsedHookEvent::UntrackedEdit(_) => return None,
        };
        Some(Self {
            session_id: context.external_session_id.clone(),
            tool_use_id,
            cwd: context.cwd.clone(),
            is_pre,
            is_bash,
        })
    }
}

#[cfg(test)]
mod race_tests {
    //! The orderings from the VER-84 spike, driven through the seam the way
    //! Enkidu's hooks drive it: one call per hook, hook payloads as JSON.

    use super::checkpoint_from_hook;
    use crate::authorship::authorship_log_serialization::generate_session_id;
    use serde_json::json;
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    struct Repo {
        _dir: tempfile::TempDir,
        root: PathBuf,
        state: PathBuf,
    }

    fn git(root: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(root)
            .status()
            .expect("git runs");
        assert!(status.success(), "git {args:?}");
    }

    fn repo() -> Repo {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("repo").canonicalize_or_create();
        git(&root, &["init", "-q"]);
        git(&root, &["config", "user.name", "Test"]);
        git(&root, &["config", "user.email", "test@example.com"]);
        std::fs::write(root.join("f.txt"), "base\n").expect("write");
        git(&root, &["add", "."]);
        git(&root, &["commit", "-qm", "init"]);
        let state = dir.path().join("state");
        Repo {
            _dir: dir,
            root,
            state,
        }
    }

    trait CanonicalizeOrCreate {
        fn canonicalize_or_create(self) -> PathBuf;
    }

    impl CanonicalizeOrCreate for PathBuf {
        fn canonicalize_or_create(self) -> PathBuf {
            std::fs::create_dir_all(&self).expect("mkdir");
            self.canonicalize().expect("canonicalize")
        }
    }

    impl Repo {
        fn hook(
            &self,
            event: &str,
            session: &str,
            tool_use: &str,
            tool: &str,
            input: serde_json::Value,
        ) {
            let payload = json!({
                "hook_event_name": event,
                "tool_name": tool,
                "session_id": session,
                "tool_use_id": tool_use,
                "cwd": self.root,
                "transcript_path": self.root.join(format!("../{session}.jsonl")),
                "tool_input": input,
            });
            checkpoint_from_hook("claude", &payload.to_string(), &self.state).expect("hook");
        }

        fn edit_input(&self, new_string: &str) -> serde_json::Value {
            json!({"file_path": self.root.join("f.txt"), "old_string": "", "new_string": new_string})
        }

        fn pre_edit(&self, session: &str, tool_use: &str) {
            self.hook("PreToolUse", session, tool_use, "Edit", self.edit_input(""));
        }

        fn post_edit(&self, session: &str, tool_use: &str, new_string: &str) {
            self.hook(
                "PostToolUse",
                session,
                tool_use,
                "Edit",
                self.edit_input(new_string),
            );
        }

        fn pre_bash(&self, session: &str, tool_use: &str) {
            self.hook(
                "PreToolUse",
                session,
                tool_use,
                "Bash",
                json!({"command": "fmt"}),
            );
        }

        fn post_bash(&self, session: &str, tool_use: &str) {
            self.hook(
                "PostToolUse",
                session,
                tool_use,
                "Bash",
                json!({"command": "fmt"}),
            );
        }

        /// Append a line to f.txt, as an agent's tool (or anyone) would.
        fn append(&self, line: &str) {
            let path = self.root.join("f.txt");
            let mut content = std::fs::read_to_string(&path).expect("read");
            content.push_str(line);
            content.push('\n');
            // Keep mtimes distinct for the shell stat-diff.
            std::thread::sleep(std::time::Duration::from_millis(20));
            std::fs::write(&path, content).expect("write");
        }

        /// Owner of each line of f.txt in the latest checkpoint that has it:
        /// "A"/"B" for the test sessions, "human" otherwise.
        fn owners(&self) -> BTreeMap<String, String> {
            let logs = self.root.join(".git/ai/working_logs");
            let dir = std::fs::read_dir(&logs)
                .expect("working logs")
                .flatten()
                .next()
                .expect("one working log")
                .path();
            let checkpoints = std::fs::read_to_string(dir.join("checkpoints.jsonl")).expect("log");
            let mut latest = None;
            for line in checkpoints.lines() {
                let checkpoint: serde_json::Value = serde_json::from_str(line).expect("json");
                for entry in checkpoint["entries"].as_array().into_iter().flatten() {
                    if entry["file"] == "f.txt" {
                        latest = Some(entry.clone());
                    }
                }
            }
            let latest = latest.expect("an entry for f.txt");
            let a = generate_session_id("A", "claude");
            let b = generate_session_id("B", "claude");
            let content = std::fs::read_to_string(self.root.join("f.txt")).expect("read");
            let mut owners: BTreeMap<String, String> = content
                .lines()
                .map(|l| (l.to_string(), "human".to_string()))
                .collect();
            let lines: Vec<&str> = content.lines().collect();
            for attr in latest["line_attributions"].as_array().into_iter().flatten() {
                let author = attr["author_id"].as_str().unwrap_or_default();
                let who = if author.starts_with(&format!("{a}::")) {
                    "A"
                } else if author.starts_with(&format!("{b}::")) {
                    "B"
                } else {
                    "human"
                };
                let start = attr["start_line"].as_u64().unwrap_or(1) as usize;
                let end = attr["end_line"].as_u64().unwrap_or(0) as usize;
                for line in start..=end {
                    owners.insert(lines[line - 1].to_string(), who.to_string());
                }
            }
            owners
        }
    }

    fn expect(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(l, o)| (l.to_string(), o.to_string()))
            .collect()
    }

    #[test]
    fn b_writes_before_a_posts_and_a_does_not_take_bs_line() {
        let r = repo();
        r.pre_edit("A", "a1");
        r.append("alpha line");
        r.pre_edit("B", "b1");
        r.append("beta line");
        r.post_edit("A", "a1", "alpha line\n");
        r.post_edit("B", "b1", "beta line\n");
        assert_eq!(
            r.owners(),
            expect(&[("base", "human"), ("alpha line", "A"), ("beta line", "B")])
        );
    }

    #[test]
    fn b_pre_between_a_write_and_a_post_does_not_lose_as_line() {
        let r = repo();
        r.pre_edit("A", "a1");
        r.append("alpha line");
        r.pre_edit("B", "b1");
        r.post_edit("A", "a1", "alpha line\n");
        r.append("beta line");
        r.post_edit("B", "b1", "beta line\n");
        assert_eq!(
            r.owners(),
            expect(&[("base", "human"), ("alpha line", "A"), ("beta line", "B")])
        );
    }

    #[test]
    fn a_long_shell_command_keeps_its_lines_when_another_hook_fires_mid_command() {
        let r = repo();
        r.pre_bash("A", "a1");
        r.append("formatted line");
        // Another session's hook fires while the command is still running.
        r.pre_edit("B", "b1");
        r.post_bash("A", "a1");
        assert_eq!(
            r.owners(),
            expect(&[("base", "human"), ("formatted line", "A")])
        );
    }

    #[test]
    fn an_unhooked_writers_line_stays_unattributed() {
        let r = repo();
        r.pre_edit("A", "a1");
        r.append("alpha line");
        r.append("someone else");
        r.post_edit("A", "a1", "alpha line\n");
        assert_eq!(
            r.owners(),
            expect(&[
                ("base", "human"),
                ("alpha line", "A"),
                ("someone else", "human")
            ])
        );
    }
}
