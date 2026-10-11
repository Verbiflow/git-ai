//! Scoping an agent checkpoint to the lines its own tool call wrote (VER-84).
//!
//! A checkpoint credits every line that changed since the previous checkpoint
//! to its author. With several agents in one checkout that goes wrong two ways:
//!
//! * Agent B writes a file before agent A's post hook runs, so A's checkpoint
//!   takes B's lines.
//! * B's pre hook runs between A's write and A's post hook, so B's checkpoint
//!   files A's lines as "human", and A's checkpoint then finds nothing changed.
//!
//! Each hook's checkpoint request therefore carries a [`ToolCallMarker`]. The
//! pre hook's marker records a baseline once its checkpoint is applied (which
//! content every file had, as working-log blobs, under `.git/ai/`), and the
//! post hook's marker becomes a [`ToolClaim`]: the text the call wrote, from
//! its own payload, plus that baseline. The post checkpoint then
//!
//! 1. gives back to "human" any line it took that the call's text does not
//!    contain, so a concurrent writer's lines are not taken, and
//! 2. takes any "human" line that changed since its baseline and matches its
//!    text, so lines another hook filed as human in between come back.
//!
//! It never takes a line from another agent. Shell commands carry no text, so
//! their claim is every line that changed since the baseline (the stat-diff
//! already limits it to files the command touched); that is lower confidence,
//! which the checkpoint's `edit_kind = bash` already says.

use crate::authorship::attribution_tracker::{
    Attribution, attributions_to_line_attributions_for_checkpoint,
};
use crate::authorship::imara_diff_utils::{LineChangeTag, compute_line_changes};
use crate::authorship::working_log::CheckpointKind;
use crate::commands::checkpoint_agent::presets::ParsedHookEvent;
use crate::error::GitAiError;
use crate::git::repository::Repository;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Baselines older than this belong to a tool call whose post hook never ran.
const BASELINE_STALE: Duration = Duration::from_secs(2 * 60 * 60);

/// What a post hook may claim.
#[derive(Debug, Clone, Default)]
pub struct ToolClaim {
    /// The text the tool call wrote, from its own payload. `None` for shell
    /// commands, whose writes are only known from the file system.
    pub written: Option<Vec<String>>,
    /// The files' content when the call started; `None` when its pre hook did
    /// not record one.
    pub baseline: Option<Baseline>,
}

impl ToolClaim {
    /// Whether this claim can change anything the plain diff decided.
    pub fn is_active(&self) -> bool {
        self.written.is_some() || self.baseline.is_some()
    }
}

/// The working-log blob each file had when a tool call started.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Baseline {
    pub base_commit: String,
    /// Repo-relative path → blob sha of the file's latest checkpoint entry.
    /// A file absent here was as it is at `base_commit`.
    pub blobs: HashMap<String, String>,
}

/// Which tool call a hook's checkpoint request belongs to, and which side of
/// it the hook fired on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCallMarker {
    pub is_pre: bool,
    pub session_id: String,
    pub tool_use_id: String,
    /// The repo the call runs in; a pre hook with nothing to checkpoint still
    /// records its baseline here.
    pub repo_work_dir: PathBuf,
    /// Post hooks of file edits: the text the call wrote (see
    /// [`written_text`]). `None` for shell commands and pre hooks.
    #[serde(default)]
    pub written: Option<Vec<String>>,
}

impl ToolCallMarker {
    /// The marker for `event`, or `None` for events outside a tool call (or
    /// outside a repo). `data` is the hook payload, for the written text.
    pub fn for_event(
        event: &ParsedHookEvent,
        preset: &str,
        data: &serde_json::Value,
    ) -> Option<Self> {
        let (context, tool_use_id, is_pre, is_bash) = match event {
            ParsedHookEvent::PreFileEdit(e) => (&e.context, e.tool_use_id.clone()?, true, false),
            ParsedHookEvent::PostFileEdit(e) => {
                (&e.context, e.tool_use_id.clone()?, false, false)
            }
            ParsedHookEvent::PreBashCall(e) => (&e.context, e.tool_use_id.clone(), true, true),
            ParsedHookEvent::PostBashCall(e) => (&e.context, e.tool_use_id.clone(), false, true),
            ParsedHookEvent::KnownHumanEdit(_) | ParsedHookEvent::UntrackedEdit(_) => return None,
        };
        let repo_work_dir =
            crate::git::repository::discover_repository_in_path_no_git_exec(&context.cwd)
                .and_then(|repo| repo.workdir())
                .ok()?;
        Some(Self {
            is_pre,
            session_id: context.external_session_id.clone(),
            tool_use_id,
            repo_work_dir,
            written: if is_pre || is_bash {
                None
            } else {
                written_text(preset, data)
            },
        })
    }
}

fn baseline_path(repo: &Repository, session_id: &str, tool_use_id: &str) -> PathBuf {
    use sha2::{Digest, Sha256};
    let key = Sha256::digest(format!("{session_id}\0{tool_use_id}").as_bytes());
    repo.storage
        .ai_dir
        .join("tool-baselines")
        .join(format!("{key:x}.json"))
}

/// Record, at a pre hook, which content every file of `repo` has. Called
/// after the pre hook's own checkpoint, so files it just checkpointed are in
/// the baseline at their current content.
pub fn record_baseline(
    repo: &Repository,
    session_id: &str,
    tool_use_id: &str,
) -> Result<(), GitAiError> {
    let workdir = repo.workdir()?;
    let base_commit = crate::git::repo_state::read_head_state_for_worktree(&workdir)
        .and_then(|state| state.head)
        .unwrap_or_else(|| "initial".to_string());

    let mut blobs = HashMap::new();
    if repo.storage.has_working_log(&base_commit) {
        let working_log = repo.storage.working_log_for_base_commit(&base_commit)?;
        for checkpoint in working_log.read_all_checkpoints()? {
            for entry in checkpoint.entries {
                blobs.insert(entry.file, entry.blob_sha);
            }
        }
    }

    let path = baseline_path(repo, session_id, tool_use_id);
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    fs::create_dir_all(parent)?;
    prune_stale(parent);
    let bytes = serde_json::to_vec(&Baseline { base_commit, blobs })
        .map_err(|e| GitAiError::Generic(format!("serialize tool baseline: {e}")))?;
    let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
    fs::write(&tmp, bytes)?;
    fs::rename(&tmp, &path)?;
    Ok(())
}

/// Read and remove the baseline a pre hook recorded for this tool call.
pub fn take_baseline(repo: &Repository, session_id: &str, tool_use_id: &str) -> Option<Baseline> {
    let path = baseline_path(repo, session_id, tool_use_id);
    let bytes = fs::read(&path).ok()?;
    let _ = fs::remove_file(&path);
    serde_json::from_slice(&bytes).ok()
}

fn prune_stale(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let stale = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > BASELINE_STALE);
        if stale {
            let _ = fs::remove_file(entry.path());
        }
    }
}

/// The text a file-edit tool call wrote, from its hook payload: Claude's
/// `new_string` (Edit), `edits[].new_string` (MultiEdit) and `content`
/// (Write), and the added lines of a Codex `apply_patch`. `None` when the
/// payload carries none, which leaves the checkpoint unscoped.
pub fn written_text(preset: &str, data: &serde_json::Value) -> Option<Vec<String>> {
    let input = data.get("tool_input").or_else(|| data.get("toolInput"))?;
    let mut written = Vec::new();
    match preset {
        "claude" => {
            for key in ["new_string", "content"] {
                if let Some(text) = input.get(key).and_then(|v| v.as_str()) {
                    written.push(text.to_string());
                }
            }
            if let Some(edits) = input.get("edits").and_then(|v| v.as_array()) {
                for edit in edits {
                    if let Some(text) = edit.get("new_string").and_then(|v| v.as_str()) {
                        written.push(text.to_string());
                    }
                }
            }
        }
        "codex" => {
            let patch = match input {
                serde_json::Value::String(patch) => Some(patch.as_str()),
                _ => ["patch", "command", "input"]
                    .iter()
                    .find_map(|key| input.get(*key).and_then(|v| v.as_str())),
            }?;
            let added: Vec<&str> = patch
                .lines()
                .filter(|line| line.starts_with('+') && !line.starts_with("+++"))
                .map(|line| &line[1..])
                .collect();
            if !added.is_empty() {
                written.push(added.join("\n"));
            }
        }
        _ => return None,
    }
    (!written.is_empty()).then_some(written)
}

/// 1-based line numbers of `new` that differ from `old`.
fn changed_lines(old: &str, new: &str) -> HashSet<u32> {
    let mut changed = HashSet::new();
    let mut line = 0u32;
    for change in compute_line_changes(old, new) {
        match change.tag() {
            LineChangeTag::Equal => line += 1,
            LineChangeTag::Insert => {
                line += 1;
                changed.insert(line);
            }
            LineChangeTag::Delete => {}
        }
    }
    changed
}

/// Byte span of each line, including its terminator.
fn line_spans(content: &str) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut start = 0;
    for (idx, byte) in content.bytes().enumerate() {
        if byte == b'\n' {
            spans.push((start, idx + 1));
            start = idx + 1;
        }
    }
    if start < content.len() {
        spans.push((start, content.len()));
    }
    spans
}

/// The written text, as trimmed lines.
struct WrittenLines {
    lines: Vec<String>,
    has_blank: bool,
}

impl WrittenLines {
    fn new(written: &[String]) -> Self {
        let mut lines = Vec::new();
        let mut has_blank = false;
        for text in written {
            for line in text.lines() {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    has_blank = true;
                } else {
                    lines.push(trimmed.to_string());
                }
            }
        }
        Self { lines, has_blank }
    }

    /// Whether `line` holds text the call wrote: a written line is part of it
    /// (an edit inside a line), or it is part of a written line (a formatter
    /// split one).
    fn matches(&self, line: &str) -> bool {
        let line = line.trim();
        if line.is_empty() {
            return self.has_blank;
        }
        self.lines
            .iter()
            .any(|w| line.contains(w.as_str()) || w.contains(line))
    }
}

/// Rewrite `attributions` so `[start, end)` belongs to `author` alone.
fn reassign_span(
    attributions: &mut Vec<Attribution>,
    start: usize,
    end: usize,
    author: &str,
    ts: u128,
) {
    let mut kept = Vec::with_capacity(attributions.len() + 2);
    for attr in attributions.drain(..) {
        let inside = if attr.start == attr.end {
            attr.start >= start && attr.start < end
        } else {
            attr.start < end && attr.end > start
        };
        if !inside {
            kept.push(attr);
            continue;
        }
        if attr.start < start {
            kept.push(Attribution::new(
                attr.start,
                start,
                attr.author_id.clone(),
                attr.ts,
            ));
        }
        if attr.end > end {
            kept.push(Attribution::new(
                end,
                attr.end,
                attr.author_id.clone(),
                attr.ts,
            ));
        }
    }
    kept.push(Attribution::new(start, end, author.to_string(), ts));
    kept.sort_by_key(|a| (a.start, a.end));
    *attributions = kept;
}

/// Apply `claim` to the attributions an AI checkpoint by `author_id` computed
/// for one file. `previous_content` is the file at the previous checkpoint and
/// `baseline_content` the file when the tool call started. Returns the new
/// attributions, or `None` when nothing changes.
pub fn apply_claim(
    attributions: &[Attribution],
    previous_content: &str,
    content: &str,
    baseline_content: Option<&str>,
    written: Option<&[String]>,
    author_id: &str,
    ts: u128,
) -> Option<Vec<Attribution>> {
    if written.is_none() && baseline_content.is_none() {
        return None;
    }
    let human = CheckpointKind::Human.to_str();
    let written = written.map(WrittenLines::new);
    let changed_now = changed_lines(previous_content, content);
    let changed_since_baseline = baseline_content.map(|b| changed_lines(b, content));

    let mut owner: HashMap<u32, String> = HashMap::new();
    for attr in attributions_to_line_attributions_for_checkpoint(attributions, content, true) {
        for line in attr.start_line..=attr.end_line {
            owner.insert(line, attr.author_id.clone());
        }
    }

    let spans = line_spans(content);
    let mut updated = attributions.to_vec();
    let mut changed = false;
    for (idx, &(start, end)) in spans.iter().enumerate() {
        let line = idx as u32 + 1;
        let text = &content[start..end];
        let is_mine = written.as_ref().is_none_or(|w| w.matches(text));
        let current = owner
            .get(&line)
            .map(String::as_str)
            .unwrap_or(human.as_str());

        if current == author_id && changed_now.contains(&line) && !is_mine {
            reassign_span(&mut updated, start, end, &human, ts);
            changed = true;
        } else if current == human
            && is_mine
            && changed_since_baseline
                .as_ref()
                .is_some_and(|lines| lines.contains(&line))
        {
            reassign_span(&mut updated, start, end, author_id, ts);
            changed = true;
        }
    }
    changed.then_some(updated)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn owners(attrs: &[Attribution], content: &str) -> Vec<(u32, String)> {
        let mut out = Vec::new();
        for attr in attributions_to_line_attributions_for_checkpoint(attrs, content, true) {
            for line in attr.start_line..=attr.end_line {
                out.push((line, attr.author_id.clone()));
            }
        }
        out
    }

    #[test]
    fn scoping_gives_back_lines_the_call_did_not_write() {
        let previous = "a\nb\n";
        let content = "a\nmine\ntheirs\nb\n";
        // "mine\ntheirs\n": both lines this checkpoint took.
        let attrs = vec![Attribution::new(2, 14, "agent".into(), 5)];
        let written = vec!["mine\n".to_string()];
        let out = apply_claim(&attrs, previous, content, None, Some(&written), "agent", 6)
            .expect("scoped");
        assert_eq!(owners(&out, content), vec![(2, "agent".to_string())]);
    }

    #[test]
    fn reclaims_human_lines_changed_since_the_baseline() {
        let baseline = "a\nb\n";
        let content = "a\nmine\nb\n";
        // Another hook filed the line as human; this checkpoint saw no change.
        let attrs = vec![Attribution::new(0, content.len(), "human".into(), 5)];
        let written = vec!["mine".to_string()];
        let out = apply_claim(
            &attrs,
            content,
            content,
            Some(baseline),
            Some(&written),
            "agent",
            6,
        )
        .expect("reclaimed");
        assert_eq!(owners(&out, content), vec![(2, "agent".to_string())]);
    }

    #[test]
    fn never_takes_another_agents_line() {
        let baseline = "a\n";
        let content = "a\nmine\n";
        let attrs = vec![Attribution::new(2, 7, "other".into(), 5)];
        let written = vec!["mine".to_string()];
        assert!(
            apply_claim(
                &attrs,
                content,
                content,
                Some(baseline),
                Some(&written),
                "agent",
                6
            )
            .is_none()
        );
    }

    #[test]
    fn shell_claims_every_line_changed_since_the_baseline() {
        let baseline = "a\n";
        let content = "a\nfmt\n";
        let attrs = vec![];
        let out = apply_claim(&attrs, content, content, Some(baseline), None, "agent", 6)
            .expect("reclaimed");
        assert_eq!(owners(&out, content), vec![(2, "agent".to_string())]);
    }

    #[test]
    fn written_text_reads_claude_and_codex_payloads() {
        let edit = json!({"tool_input": {"new_string": "x"}});
        assert_eq!(written_text("claude", &edit), Some(vec!["x".to_string()]));
        let multi = json!({"tool_input": {"edits": [{"new_string": "a"}, {"new_string": "b"}]}});
        assert_eq!(
            written_text("claude", &multi),
            Some(vec!["a".to_string(), "b".to_string()])
        );
        let write = json!({"tool_input": {"content": "c"}});
        assert_eq!(written_text("claude", &write), Some(vec!["c".to_string()]));
        let patch = json!({"tool_input": {"patch": "*** Update File: f\n@@\n-old\n+new\n+more\n"}});
        assert_eq!(
            written_text("codex", &patch),
            Some(vec!["new\nmore".to_string()])
        );
        let bash = json!({"tool_input": {"command": "ls"}});
        assert_eq!(written_text("claude", &bash), None);
    }
}
