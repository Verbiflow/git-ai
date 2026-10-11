//! VER-84 on the daemon path: each agent checkpoint credits only the lines its
//! own tool call wrote, a hook firing mid-call does not take a call's lines,
//! and credits for uncommitted work survive a commit of other work.
//!
//! Hooks go through `git-ai checkpoint <preset> --hook-input`, as Enkidu's
//! per-pane hooks send them; the daemon applies them.

use crate::repos::test_repo::TestRepo;
use git_ai::authorship::authorship_log_serialization::AuthorshipLog;
use serde_json::json;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

struct Agents<'a> {
    repo: &'a TestRepo,
    root: std::path::PathBuf,
}

impl<'a> Agents<'a> {
    fn new(repo: &'a TestRepo) -> Self {
        Self {
            repo,
            root: repo.canonical_path(),
        }
    }

    fn hook(&self, event: &str, session: &str, tool_use: &str, tool: &str, input: serde_json::Value) {
        let payload = json!({
            "hook_event_name": event,
            "tool_name": tool,
            "session_id": session,
            "tool_use_id": tool_use,
            "cwd": self.root.to_string_lossy(),
            "transcript_path": self.root.join(format!("{session}.jsonl")).to_string_lossy(),
            "tool_input": input,
        })
        .to_string();
        self.repo
            .git_ai(&["checkpoint", "claude", "--hook-input", &payload])
            .expect("hook runs");
    }

    fn edit_input(&self, file: &str, new_string: &str) -> serde_json::Value {
        json!({
            "file_path": self.root.join(file).to_string_lossy(),
            "old_string": "",
            "new_string": new_string,
        })
    }

    fn pre_edit(&self, session: &str, tool_use: &str, file: &str) {
        self.hook("PreToolUse", session, tool_use, "Edit", self.edit_input(file, ""));
    }

    fn post_edit(&self, session: &str, tool_use: &str, file: &str, new_string: &str) {
        self.hook("PostToolUse", session, tool_use, "Edit", self.edit_input(file, new_string));
    }

    fn pre_bash(&self, session: &str, tool_use: &str) {
        self.hook("PreToolUse", session, tool_use, "Bash", json!({"command": "fmt"}));
    }

    fn post_bash(&self, session: &str, tool_use: &str) {
        self.hook("PostToolUse", session, tool_use, "Bash", json!({"command": "fmt"}));
    }

    fn append(&self, file: &str, line: &str) {
        let path = self.root.join(file);
        let mut content = fs::read_to_string(&path).unwrap_or_default();
        content.push_str(line);
        content.push('\n');
        // Distinct mtimes for the shell stat-diff.
        std::thread::sleep(std::time::Duration::from_millis(20));
        fs::write(&path, content).expect("write");
    }
}

/// Owner of each line of `file` in `log`: the session id ("A", "B") of the
/// agent credited with it, or "human" for a line no agent is credited with.
fn owners(log: &AuthorshipLog, root: &Path, file: &str) -> BTreeMap<String, String> {
    let content = fs::read_to_string(root.join(file)).expect("read");
    let lines: Vec<&str> = content.lines().collect();
    let mut owners: BTreeMap<String, String> = lines
        .iter()
        .map(|l| (l.to_string(), "human".to_string()))
        .collect();
    let Some(attestation) = log.attestations.iter().find(|a| a.file_path == file) else {
        return owners;
    };
    for entry in &attestation.entries {
        let session_key = entry.hash.split("::").next().unwrap_or(&entry.hash);
        let Some(session) = log.metadata.sessions.get(session_key) else {
            continue;
        };
        for range in &entry.line_ranges {
            for line in range.expand() {
                if let Some(text) = lines.get(line as usize - 1) {
                    owners.insert(text.to_string(), session.agent_id.id.clone());
                }
            }
        }
    }
    owners
}

fn expect(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(l, o)| (l.to_string(), o.to_string()))
        .collect()
}

fn setup() -> TestRepo {
    let repo = TestRepo::new();
    fs::write(repo.canonical_path().join("f.txt"), "base\n").unwrap();
    fs::write(repo.canonical_path().join("g.txt"), "base\n").unwrap();
    repo.stage_all_and_commit("init").unwrap();
    repo
}

#[test]
fn daemon_b_writes_before_a_posts_and_a_does_not_take_bs_line() {
    let repo = setup();
    let a = Agents::new(&repo);
    a.pre_edit("A", "a1", "f.txt");
    a.append("f.txt", "alpha line");
    a.pre_edit("B", "b1", "f.txt");
    a.append("f.txt", "beta line");
    a.post_edit("A", "a1", "f.txt", "alpha line\n");
    a.post_edit("B", "b1", "f.txt", "beta line\n");
    let commit = repo.stage_all_and_commit("work").unwrap();
    assert_eq!(
        owners(&commit.authorship_log, &a.root, "f.txt"),
        expect(&[("base", "human"), ("alpha line", "A"), ("beta line", "B")])
    );
}

#[test]
fn daemon_b_pre_between_a_write_and_a_post_does_not_lose_as_line() {
    let repo = setup();
    let a = Agents::new(&repo);
    a.pre_edit("A", "a1", "f.txt");
    a.append("f.txt", "alpha line");
    a.pre_edit("B", "b1", "f.txt");
    a.post_edit("A", "a1", "f.txt", "alpha line\n");
    a.append("f.txt", "beta line");
    a.post_edit("B", "b1", "f.txt", "beta line\n");
    let commit = repo.stage_all_and_commit("work").unwrap();
    assert_eq!(
        owners(&commit.authorship_log, &a.root, "f.txt"),
        expect(&[("base", "human"), ("alpha line", "A"), ("beta line", "B")])
    );
}

#[test]
fn daemon_long_shell_command_keeps_its_lines_when_another_hook_fires_mid_command() {
    let repo = setup();
    let a = Agents::new(&repo);
    a.pre_bash("A", "a1");
    a.append("f.txt", "formatted line");
    a.pre_edit("B", "b1", "g.txt");
    a.post_bash("A", "a1");
    let commit = repo.stage_all_and_commit("work").unwrap();
    assert_eq!(
        owners(&commit.authorship_log, &a.root, "f.txt"),
        expect(&[("base", "human"), ("formatted line", "A")])
    );
}

#[test]
fn daemon_unhooked_writers_line_stays_unattributed() {
    let repo = setup();
    let a = Agents::new(&repo);
    a.pre_edit("A", "a1", "f.txt");
    a.append("f.txt", "alpha line");
    a.append("f.txt", "someone else");
    a.post_edit("A", "a1", "f.txt", "alpha line\n");
    let commit = repo.stage_all_and_commit("work").unwrap();
    assert_eq!(
        owners(&commit.authorship_log, &a.root, "f.txt"),
        expect(&[("base", "human"), ("alpha line", "A"), ("someone else", "human")])
    );
}

#[test]
fn credits_for_uncommitted_work_survive_a_commit_of_other_work() {
    let repo = setup();
    let a = Agents::new(&repo);
    a.pre_edit("A", "a1", "f.txt");
    a.append("f.txt", "committed now");
    a.post_edit("A", "a1", "f.txt", "committed now\n");
    a.pre_edit("B", "b1", "g.txt");
    a.append("g.txt", "committed later");
    a.post_edit("B", "b1", "g.txt", "committed later\n");

    // Someone commits only f.txt; g.txt stays uncommitted across the commit.
    repo.git(&["add", "f.txt"]).unwrap();
    let first = repo.commit("first").unwrap();
    assert_eq!(
        owners(&first.authorship_log, &a.root, "f.txt"),
        expect(&[("base", "human"), ("committed now", "A")])
    );

    let second = repo.stage_all_and_commit("second").unwrap();
    assert_eq!(
        owners(&second.authorship_log, &a.root, "g.txt"),
        expect(&[("base", "human"), ("committed later", "B")])
    );
}
