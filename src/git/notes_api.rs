//! Centralized notes I/O API.
//!
//! All authorship-note reads and writes flow through this module. This fork
//! keeps notes only in git (`refs/notes/ai`); upstream's HTTP notes backend,
//! which stored them on a remote server, is removed.

use crate::authorship::authorship_log_serialization::AuthorshipLog;
use crate::error::GitAiError;
use crate::git::repository::Repository;
use std::collections::{HashMap, HashSet};

// Re-export CommitAuthorship so callers don't need to import from refs directly.
pub use crate::git::refs::CommitAuthorship;

// --- Writes ---

pub fn write_note(repo: &Repository, commit_sha: &str, content: &str) -> Result<(), GitAiError> {
    crate::git::refs::notes_add(repo, commit_sha, content)
}

pub fn write_notes_batch(
    repo: &Repository,
    entries: &[(String, String)],
) -> Result<(), GitAiError> {
    if entries.is_empty() {
        return Ok(());
    }
    crate::git::refs::notes_add_batch(repo, entries)
}

// --- Reads ---

pub fn read_note(repo: &Repository, commit_sha: &str) -> Option<String> {
    crate::git::refs::show_authorship_note(repo, commit_sha)
}

/// Read note contents for multiple commits in O(1) git process calls.
/// Returns a map of commit_sha → note_content for commits that have notes.
pub fn read_notes_batch(
    repo: &Repository,
    commit_shas: &[String],
) -> Result<HashMap<String, String>, GitAiError> {
    crate::git::refs::notes_for_commits(repo, commit_shas)
}

pub fn read_authorship(repo: &Repository, commit_sha: &str) -> Option<AuthorshipLog> {
    crate::git::refs::get_authorship(repo, commit_sha)
}

pub fn read_authorship_v3(
    repo: &Repository,
    commit_sha: &str,
) -> Result<AuthorshipLog, GitAiError> {
    crate::git::refs::get_reference_as_authorship_log_v3(repo, commit_sha)
}

/// Return a map of commit SHA → note-blob OID for the given commits.
pub fn read_note_blob_oids(
    repo: &Repository,
    commit_shas: &[String],
) -> Result<HashMap<String, String>, GitAiError> {
    crate::git::refs::note_blob_oids_for_commits(repo, commit_shas)
}

pub fn commits_with_notes(
    repo: &Repository,
    commit_shas: &[String],
) -> Result<HashSet<String>, GitAiError> {
    crate::git::refs::commits_with_authorship_notes(repo, commit_shas)
}

pub fn filter_commits_with_notes(
    repo: &Repository,
    commit_shas: &[String],
) -> Result<Vec<CommitAuthorship>, GitAiError> {
    crate::git::refs::get_commits_with_notes_from_list(repo, commit_shas)
}

// --- Search ---

/// Search authorship-note content for a literal substring and return matching
/// commit SHAs, newest first.
pub fn search_notes(repo: &Repository, pattern: &str) -> Result<Vec<String>, GitAiError> {
    crate::git::refs::grep_ai_notes(repo, pattern)
}
