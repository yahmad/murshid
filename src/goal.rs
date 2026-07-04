//! T3 reqs 1-4 — session-goal capture: inference, file storage, the banner,
//! explicit-goal-wins, and drift detection (D13/D14, I8, C7 forward-refs).
//!
//! Goal storage is a plain user-editable file (D13(c)): `.murshid/goal`, one
//! line of goal text plus optional trailing notes lines. A sibling marker
//! file (`.murshid/goal.inferred_at`) records the epoch-ms of the engine's
//! own last auto-write, so a later run can tell "the user hand-edited this
//! since we last wrote it" (explicit wins, req 3) from "nobody touched it,
//! safe to re-infer" — a bare file-mtime-vs-now comparison can't make that
//! distinction across process restarts.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

// --- file storage (D13(c)) ---

pub fn goal_file_path(project_root: &Path) -> PathBuf {
    project_root.join(".murshid").join("goal")
}

fn goal_marker_path(project_root: &Path) -> PathBuf {
    project_root.join(".murshid").join("goal.inferred_at")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredGoal {
    pub text: String,
    pub notes: Vec<String>,
}

/// Reads `.murshid/goal`: line 1 is the goal text, remaining lines are
/// user notes. `None` when the file doesn't exist or has no goal line.
pub fn read_goal_file(project_root: &Path) -> Option<StoredGoal> {
    let content = std::fs::read_to_string(goal_file_path(project_root)).ok()?;
    let mut lines = content.lines();
    let text = lines.next().unwrap_or("").trim().to_string();
    if text.is_empty() {
        return None;
    }
    let notes = lines.map(|l| l.to_string()).collect();
    Some(StoredGoal { text, notes })
}

pub fn write_goal_file(project_root: &Path, text: &str) -> Result<(), String> {
    let path = goal_file_path(project_root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(&path, format!("{}\n", text.trim())).map_err(|e| e.to_string())
}

pub fn goal_file_mtime(project_root: &Path) -> Option<SystemTime> {
    std::fs::metadata(goal_file_path(project_root))
        .and_then(|m| m.modified())
        .ok()
}

fn read_last_inferred_at(project_root: &Path) -> Option<SystemTime> {
    let raw = std::fs::read_to_string(goal_marker_path(project_root)).ok()?;
    let millis: u64 = raw.trim().parse().ok()?;
    Some(UNIX_EPOCH + Duration::from_millis(millis))
}

fn write_last_inferred_at(project_root: &Path, at: SystemTime) -> Result<(), String> {
    let millis = at
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let path = goal_marker_path(project_root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(&path, millis.to_string()).map_err(|e| e.to_string())
}

// --- banner (req 1) ---

pub fn goal_banner(text: &str) -> String {
    format!("goal: {} (g to edit)", text)
}

// --- branch/commit inference (req 1) ---

const NON_INFORMATIVE_BRANCHES: &[&str] = &[
    "main", "master", "dev", "develop", "trunk", "release", "staging", "wip",
];

/// req 1: "main/master/dev/wip-like names are non-informative".
pub fn is_informative_branch(branch: &str) -> bool {
    let trimmed = branch.trim();
    if trimmed.is_empty() {
        return false;
    }
    let lower = trimmed.to_lowercase();
    if NON_INFORMATIVE_BRANCHES.contains(&lower.as_str()) {
        return false;
    }
    if lower.starts_with("wip-") || lower.starts_with("wip/") {
        return false;
    }
    true
}

/// req 1: parses issue-id + slug branch naming, e.g.
/// `feature/AUTH-123-fix-timeout` -> "fix timeout (AUTH-123)", or
/// `123-fix-timeout` -> "fix timeout (#123)". Falls back to the slug alone
/// when no issue id is present, and to `None` for non-informative branches.
pub fn infer_goal_from_branch(branch: &str) -> Option<String> {
    if !is_informative_branch(branch) {
        return None;
    }
    let last_segment = branch.trim().rsplit('/').next().unwrap_or(branch);
    let tokens: Vec<&str> = last_segment
        .split(['-', '_'])
        .filter(|t| !t.is_empty())
        .collect();
    if tokens.is_empty() {
        return None;
    }

    let (issue_id, idx) = if tokens.len() >= 2
        && tokens[0].len() >= 2
        && tokens[0].chars().all(|c| c.is_ascii_uppercase())
        && tokens[1].chars().all(|c| c.is_ascii_digit())
    {
        (Some(format!("{}-{}", tokens[0], tokens[1])), 2)
    } else if tokens[0].chars().all(|c| c.is_ascii_digit()) {
        (Some(format!("#{}", tokens[0])), 1)
    } else {
        (None, 0)
    };

    let slug_tokens = &tokens[idx..];
    if slug_tokens.is_empty() {
        return issue_id;
    }
    let slug = slug_tokens.join(" ");
    match issue_id {
        Some(id) => Some(format!("{} ({})", slug, id)),
        None => Some(slug),
    }
}

/// req 1 fallback 2: last 3 commit subjects, joined.
pub fn infer_goal_from_commits(subjects: &[String]) -> Option<String> {
    let take: Vec<&str> = subjects.iter().take(3).map(|s| s.as_str()).collect();
    if take.is_empty() {
        None
    } else {
        Some(take.join("; "))
    }
}

/// req 1 fallback 3: first session-diff file cluster — the most common
/// leading directory among the changed files, or the file names themselves
/// when nothing shares a directory (e.g. top-level files only).
pub fn infer_goal_from_file_cluster(files: &[PathBuf]) -> Option<String> {
    if files.is_empty() {
        return None;
    }
    let dirs: Vec<String> = files
        .iter()
        .filter_map(|f| f.parent().map(|p| p.to_string_lossy().to_string()))
        .filter(|d| !d.is_empty())
        .collect();
    if dirs.is_empty() {
        let names: Vec<String> = files
            .iter()
            .take(3)
            .map(|f| f.to_string_lossy().to_string())
            .collect();
        return Some(format!("work in {}", names.join(", ")));
    }
    let mut counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for d in &dirs {
        *counts.entry(d.as_str()).or_insert(0) += 1;
    }
    let top = counts
        .into_iter()
        .max_by_key(|(_, c)| *c)
        .map(|(d, _)| d.to_string())?;
    Some(format!("work in {}", top))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoalSource {
    Branch,
    Commits,
    FileCluster,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InferredGoal {
    pub text: String,
    pub source: GoalSource,
}

/// req 1: branch -> last-3-commit-subjects -> first session-diff file
/// cluster, in that order.
pub fn infer_goal(
    branch: Option<&str>,
    commit_subjects: &[String],
    changed_files: &[PathBuf],
) -> Option<InferredGoal> {
    if let Some(b) = branch {
        if let Some(text) = infer_goal_from_branch(b) {
            return Some(InferredGoal {
                text,
                source: GoalSource::Branch,
            });
        }
    }
    if let Some(text) = infer_goal_from_commits(commit_subjects) {
        return Some(InferredGoal {
            text,
            source: GoalSource::Commits,
        });
    }
    if let Some(text) = infer_goal_from_file_cluster(changed_files) {
        return Some(InferredGoal {
            text,
            source: GoalSource::FileCluster,
        });
    }
    None
}

// --- git shell-outs (mirrors session.rs's Command convention) ---

pub fn current_branch(project_root: &Path) -> Option<String> {
    let output = Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(project_root)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let name = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if name.is_empty() || name == "HEAD" {
        None
    } else {
        Some(name)
    }
}

pub fn recent_commit_subjects(project_root: &Path, count: usize) -> Vec<String> {
    let output = Command::new("git")
        .args(["log", &format!("-{}", count), "--pretty=%s"])
        .current_dir(project_root)
        .output();
    match output {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout)
            .lines()
            .map(|l| l.to_string())
            .collect(),
        _ => Vec::new(),
    }
}

// --- explicit-goal-wins (req 3) ---

/// req 3: "file mtime > inference time wins". `reference_time` is the
/// timestamp of the engine's own last auto-write (or the moment inference
/// is about to run, for a file that predates any known auto-write) — a
/// human edit lands after that reference, an untouched auto-written file
/// never does.
pub fn explicit_wins(file_mtime: Option<SystemTime>, reference_time: SystemTime) -> bool {
    match file_mtime {
        Some(m) => m > reference_time,
        None => false,
    }
}

/// req 1-3: resolves this session's goal text — infers fresh from
/// branch/commits/file-cluster, but never clobbers a goal file a human
/// edited since the engine's own last auto-write. Writes the file (and the
/// auto-write marker) only when it actually infers and adopts a new value.
/// Returns `(goal_text, was_freshly_inferred)`.
pub fn resolve_session_goal(
    project_root: &Path,
    branch: Option<&str>,
    commit_subjects: &[String],
    changed_files: &[PathBuf],
) -> (Option<String>, bool) {
    // Review fix: an empty/whitespace-only file (no actual goal text —
    // `read_goal_file` already returns `None` for it) is never "explicit".
    // There's nothing to protect, and protecting it anyway would
    // permanently wedge inference behind an accidental empty save (e.g. a
    // `g`-opened $EDITOR quit-without-saving that still left a zero-byte
    // stub). Only a file that actually holds goal text is a candidate for
    // protection.
    let stored = read_goal_file(project_root);
    if let Some(stored) = &stored {
        let file_mtime = goal_file_mtime(project_root);
        let last_inferred_at = read_last_inferred_at(project_root);
        let protected = match last_inferred_at {
            Some(reference) => explicit_wins(file_mtime, reference),
            // Non-empty goal text, no record of ever writing it ourselves:
            // conservatively treat it as user-authored.
            None => true,
        };
        if protected {
            return (Some(stored.text.clone()), false);
        }
    }

    match infer_goal(branch, commit_subjects, changed_files) {
        Some(g) => {
            let now = SystemTime::now();
            let _ = write_goal_file(project_root, &g.text);
            let _ = write_last_inferred_at(project_root, now);
            (Some(g.text), true)
        }
        None => (stored.map(|g| g.text), false),
    }
}

// --- drift (req 4) ---

pub const DRIFT_WINDOW: Duration = Duration::from_secs(30 * 60);
pub const DRIFT_THRESHOLD: f64 = 0.70;
pub const DRIFT_NOTICE: &str = "looks like you've moved on \u{2014} g to update";

fn file_matches_cluster(cluster_dirs: &HashSet<String>, file: &str) -> bool {
    cluster_dirs
        .iter()
        .any(|d| file == d.as_str() || file.starts_with(&format!("{}/", d)))
}

/// req 4: fraction of `recent_files` (deduped) whose directory falls
/// outside `cluster_dirs`. An unknown/empty cluster never counts as drift
/// (nothing to compare against).
pub fn drift_ratio(cluster_dirs: &HashSet<String>, recent_files: &[String]) -> f64 {
    if recent_files.is_empty() || cluster_dirs.is_empty() {
        return 0.0;
    }
    let unique: HashSet<&String> = recent_files.iter().collect();
    let outside = unique
        .iter()
        .filter(|f| !file_matches_cluster(cluster_dirs, f))
        .count();
    outside as f64 / unique.len() as f64
}

pub fn drift_threshold_exceeded(ratio: f64) -> bool {
    ratio >= DRIFT_THRESHOLD
}

/// req 4: "surface ONE line per session... never repeats".
pub fn should_fire_drift(already_fired: bool, ratio: f64) -> bool {
    !already_fired && drift_threshold_exceeded(ratio)
}

/// The goal's file cluster: directories of the files known to be part of
/// the goal's working set (session-start snapshot files, per T3 implementer
/// note — see main.rs wiring).
pub fn cluster_dirs_from_files(files: &[PathBuf]) -> HashSet<String> {
    files
        .iter()
        .filter_map(|f| f.parent().map(|p| p.to_string_lossy().to_string()))
        .filter(|d| !d.is_empty())
        .collect()
}

// --- goal relevance (req 5 — consumed by queue::sort_queue) ---

pub fn file_in_goal_cluster(cluster_dirs: &HashSet<String>, file: &str) -> bool {
    file_matches_cluster(cluster_dirs, file)
}

/// Lowercases and collapses punctuation to spaces so "Borrow vs. clone"
/// matches goal text that drops the period ("borrow vs clone overhead").
fn normalize_for_matching(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last_was_space = false;
    for c in text.to_lowercase().chars() {
        if c.is_alphanumeric() {
            out.push(c);
            last_was_space = false;
        } else if !last_was_space {
            out.push(' ');
            last_was_space = true;
        }
    }
    out.trim().to_string()
}

pub fn concept_referenced_in_goal(goal_text: &str, concept_name: &str) -> bool {
    let concept = normalize_for_matching(concept_name);
    if concept.is_empty() {
        return false;
    }
    normalize_for_matching(goal_text).contains(&concept)
}

/// req 5: "advice whose site's file is in the goal cluster (or whose
/// concept was referenced in the goal text) ranks first."
pub fn is_goal_relevant(
    cluster_dirs: &HashSet<String>,
    goal_text: &str,
    file: &str,
    concept_name: &str,
) -> bool {
    file_in_goal_cluster(cluster_dirs, file) || concept_referenced_in_goal(goal_text, concept_name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn init_git_repo(dir: &Path) {
        let run = |args: &[&str]| {
            Command::new("git")
                .args(args)
                .current_dir(dir)
                .output()
                .unwrap()
        };
        run(&["init", "-q"]);
        run(&[
            "-c",
            "user.email=test@test.com",
            "-c",
            "user.name=Test",
            "commit",
            "--allow-empty",
            "-q",
            "-m",
            "init",
        ]);
    }

    // --- branch-name inference incl. non-informative fallbacks ---

    #[test]
    fn test_non_informative_branches_return_none() {
        for b in ["main", "master", "dev", "develop", "wip", "WIP", "trunk"] {
            assert_eq!(infer_goal_from_branch(b), None, "branch: {}", b);
        }
    }

    #[test]
    fn test_wip_prefixed_branch_is_non_informative() {
        assert_eq!(infer_goal_from_branch("wip-scratch"), None);
        assert_eq!(infer_goal_from_branch("wip/scratch"), None);
    }

    #[test]
    fn test_infer_goal_from_branch_with_jira_style_issue_id() {
        assert_eq!(
            infer_goal_from_branch("feature/AUTH-123-fix-timeout"),
            Some("fix timeout (AUTH-123)".to_string())
        );
    }

    #[test]
    fn test_infer_goal_from_branch_with_numeric_issue_id() {
        assert_eq!(
            infer_goal_from_branch("123-fix-timeout"),
            Some("fix timeout (#123)".to_string())
        );
    }

    #[test]
    fn test_infer_goal_from_branch_without_issue_id() {
        assert_eq!(
            infer_goal_from_branch("fix-timeout"),
            Some("fix timeout".to_string())
        );
    }

    #[test]
    fn test_infer_goal_from_branch_empty_is_none() {
        assert_eq!(infer_goal_from_branch(""), None);
        assert_eq!(infer_goal_from_branch("   "), None);
    }

    // --- commit-subject fallback ---

    #[test]
    fn test_infer_goal_from_commits_takes_last_three() {
        let subjects = vec![
            "fix auth timeout".to_string(),
            "add retry".to_string(),
            "cleanup".to_string(),
            "unrelated fourth".to_string(),
        ];
        assert_eq!(
            infer_goal_from_commits(&subjects),
            Some("fix auth timeout; add retry; cleanup".to_string())
        );
    }

    #[test]
    fn test_infer_goal_from_commits_empty_is_none() {
        assert_eq!(infer_goal_from_commits(&[]), None);
    }

    // --- file-cluster fallback ---

    #[test]
    fn test_infer_goal_from_file_cluster_common_directory() {
        let files = vec![
            PathBuf::from("src/goal.rs"),
            PathBuf::from("src/queue.rs"),
            PathBuf::from("src/main.rs"),
        ];
        assert_eq!(
            infer_goal_from_file_cluster(&files),
            Some("work in src".to_string())
        );
    }

    #[test]
    fn test_infer_goal_from_file_cluster_top_level_files_named_directly() {
        let files = vec![PathBuf::from("Cargo.toml")];
        assert_eq!(
            infer_goal_from_file_cluster(&files),
            Some("work in Cargo.toml".to_string())
        );
    }

    #[test]
    fn test_infer_goal_from_file_cluster_empty_is_none() {
        assert_eq!(infer_goal_from_file_cluster(&[]), None);
    }

    // --- overall precedence: branch -> commits -> file cluster ---

    #[test]
    fn test_infer_goal_precedence_branch_wins_over_commits() {
        let g = infer_goal(
            Some("feature/AUTH-123-fix-timeout"),
            &["some commit".to_string()],
            &[],
        )
        .unwrap();
        assert_eq!(g.source, GoalSource::Branch);
        assert_eq!(g.text, "fix timeout (AUTH-123)");
    }

    #[test]
    fn test_infer_goal_falls_back_to_commits_when_branch_non_informative() {
        let g = infer_goal(Some("main"), &["fix auth timeout".to_string()], &[]).unwrap();
        assert_eq!(g.source, GoalSource::Commits);
    }

    #[test]
    fn test_infer_goal_falls_back_to_file_cluster_when_nothing_else() {
        let g = infer_goal(Some("main"), &[], &[PathBuf::from("src/lib.rs")]).unwrap();
        assert_eq!(g.source, GoalSource::FileCluster);
    }

    #[test]
    fn test_infer_goal_none_when_nothing_available() {
        assert!(infer_goal(Some("main"), &[], &[]).is_none());
    }

    // --- banner ---

    #[test]
    fn test_goal_banner_format() {
        assert_eq!(
            goal_banner("fix timeout (AUTH-123)"),
            "goal: fix timeout (AUTH-123) (g to edit)"
        );
    }

    // --- explicit-goal-wins rule ---

    #[test]
    fn test_explicit_wins_when_file_newer_than_reference() {
        let t0 = UNIX_EPOCH + Duration::from_secs(1000);
        let t1 = t0 + Duration::from_secs(10);
        assert!(explicit_wins(Some(t1), t0));
    }

    #[test]
    fn test_explicit_does_not_win_when_file_older_or_equal() {
        let t0 = UNIX_EPOCH + Duration::from_secs(1000);
        assert!(!explicit_wins(Some(t0), t0));
        assert!(!explicit_wins(Some(t0 - Duration::from_secs(5)), t0));
    }

    #[test]
    fn test_explicit_does_not_win_with_no_file() {
        let t0 = UNIX_EPOCH + Duration::from_secs(1000);
        assert!(!explicit_wins(None, t0));
    }

    #[test]
    fn test_resolve_session_goal_infers_fresh_on_first_run() {
        let temp_dir = std::env::temp_dir();
        let root = temp_dir.join("murshid_test_goal_infer_first_run");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        init_git_repo(&root);

        let (text, was_inferred) = resolve_session_goal(&root, Some("main"), &[], &[]);
        // "main" is non-informative and there's nothing else to infer from.
        assert_eq!(text, None);
        assert!(!was_inferred);

        let (text2, was_inferred2) =
            resolve_session_goal(&root, Some("feature/AUTH-123-fix-timeout"), &[], &[]);
        assert_eq!(text2, Some("fix timeout (AUTH-123)".to_string()));
        assert!(was_inferred2);
        assert_eq!(
            read_goal_file(&root).unwrap().text,
            "fix timeout (AUTH-123)"
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn test_resolve_session_goal_never_overwrites_a_hand_edit() {
        let temp_dir = std::env::temp_dir();
        let root = temp_dir.join("murshid_test_goal_explicit_wins");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        init_git_repo(&root);

        // First run: auto-inferred and written.
        let (text, was_inferred) =
            resolve_session_goal(&root, Some("feature/AUTH-123-fix-timeout"), &[], &[]);
        assert_eq!(text, Some("fix timeout (AUTH-123)".to_string()));
        assert!(was_inferred);

        // Simulate a human hand-edit: sleep a hair so mtime advances, then
        // write directly to the file (bypassing the marker update).
        std::thread::sleep(Duration::from_millis(20));
        write_goal_file(&root, "actually fixing the parser").unwrap();

        // Next run must NOT clobber the hand edit, even with a fresh,
        // different inference available.
        let (text2, was_inferred2) =
            resolve_session_goal(&root, Some("feature/AUTH-999-other-thing"), &[], &[]);
        assert_eq!(text2, Some("actually fixing the parser".to_string()));
        assert!(!was_inferred2);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn test_resolve_session_goal_protects_preexisting_file_with_no_marker() {
        let temp_dir = std::env::temp_dir();
        let root = temp_dir.join("murshid_test_goal_preexisting_no_marker");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        init_git_repo(&root);

        // A goal file exists (e.g. hand-authored before ever running the
        // watcher) with no auto-write marker at all.
        write_goal_file(&root, "hand-authored goal").unwrap();

        let (text, was_inferred) =
            resolve_session_goal(&root, Some("feature/AUTH-123-fix-timeout"), &[], &[]);
        assert_eq!(text, Some("hand-authored goal".to_string()));
        assert!(!was_inferred);

        let _ = fs::remove_dir_all(&root);
    }

    /// Review fix: an unedited empty/whitespace-only stub (e.g. left behind
    /// by a `g`-opened $EDITOR that was quit without saving) must never
    /// permanently block inference — there's no actual goal text to
    /// protect, regardless of whether an auto-write marker exists.
    #[test]
    fn test_resolve_session_goal_unedited_empty_stub_still_infers() {
        let temp_dir = std::env::temp_dir();
        let root = temp_dir.join("murshid_test_goal_empty_stub_reinfers");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        init_git_repo(&root);

        // Simulate a pre-created-but-never-written stub, no marker.
        fs::create_dir_all(goal_file_path(&root).parent().unwrap()).unwrap();
        fs::write(goal_file_path(&root), "\n").unwrap();
        assert!(read_goal_file(&root).is_none(), "empty stub holds no goal");

        let (text, was_inferred) =
            resolve_session_goal(&root, Some("feature/AUTH-123-fix-timeout"), &[], &[]);
        assert_eq!(text, Some("fix timeout (AUTH-123)".to_string()));
        assert!(was_inferred, "an empty stub must not block inference");

        let _ = fs::remove_dir_all(&root);
    }

    // --- drift: fires once only, correct ratio ---

    #[test]
    fn test_drift_ratio_all_inside_cluster_is_zero() {
        let mut dirs = HashSet::new();
        dirs.insert("src".to_string());
        let recent = vec!["src/a.rs".to_string(), "src/b.rs".to_string()];
        assert_eq!(drift_ratio(&dirs, &recent), 0.0);
    }

    #[test]
    fn test_drift_ratio_all_outside_cluster_is_one() {
        let mut dirs = HashSet::new();
        dirs.insert("src".to_string());
        let recent = vec!["docs/a.md".to_string(), "docs/b.md".to_string()];
        assert_eq!(drift_ratio(&dirs, &recent), 1.0);
    }

    #[test]
    fn test_drift_ratio_mixed() {
        let mut dirs = HashSet::new();
        dirs.insert("src".to_string());
        // 3 outside, 1 inside -> 0.75
        let recent = vec![
            "src/a.rs".to_string(),
            "docs/a.md".to_string(),
            "docs/b.md".to_string(),
            "tests/c.rs".to_string(),
        ];
        assert!((drift_ratio(&dirs, &recent) - 0.75).abs() < 1e-9);
    }

    #[test]
    fn test_drift_threshold_boundary() {
        assert!(drift_threshold_exceeded(0.70));
        assert!(!drift_threshold_exceeded(0.69));
    }

    #[test]
    fn test_drift_unknown_cluster_never_drifts() {
        let dirs = HashSet::new();
        let recent = vec!["docs/a.md".to_string()];
        assert_eq!(drift_ratio(&dirs, &recent), 0.0);
    }

    #[test]
    fn test_should_fire_drift_once_only() {
        assert!(should_fire_drift(false, 0.75));
        assert!(!should_fire_drift(true, 0.90), "never repeats once fired");
        assert!(!should_fire_drift(false, 0.50), "below threshold");
    }

    // --- goal relevance (req 5) ---

    #[test]
    fn test_file_in_goal_cluster() {
        let mut dirs = HashSet::new();
        dirs.insert("src/auth".to_string());
        assert!(file_in_goal_cluster(&dirs, "src/auth/login.rs"));
        assert!(!file_in_goal_cluster(&dirs, "src/parser/lex.rs"));
    }

    #[test]
    fn test_concept_referenced_in_goal_text() {
        assert!(concept_referenced_in_goal(
            "fix the borrow vs clone overhead in auth",
            "Borrow vs. clone"
        ));
        assert!(!concept_referenced_in_goal(
            "fix auth timeout",
            "Iterator chains"
        ));
    }

    #[test]
    fn test_is_goal_relevant_either_condition() {
        let mut dirs = HashSet::new();
        dirs.insert("src/auth".to_string());
        assert!(is_goal_relevant(
            &dirs,
            "fix auth timeout",
            "src/auth/login.rs",
            "unrelated concept"
        ));
        assert!(is_goal_relevant(
            &dirs,
            "fix the clone overhead",
            "src/parser/lex.rs",
            "clone"
        ));
        assert!(!is_goal_relevant(
            &dirs,
            "fix auth timeout",
            "src/parser/lex.rs",
            "unrelated concept"
        ));
    }
}
