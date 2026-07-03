//! Session lifecycle, session-start snapshot, and session diff (C2).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// C2/C12: an idle gap greater than this splits a new session.
pub const DEFAULT_IDLE_SPLIT: Duration = Duration::from_secs(4 * 3600);

const CROCKFORD_ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// Generates a ULID-like session id: a 48-bit millisecond timestamp encoded
/// as 10 Crockford-base32 chars, followed by 16 chars of pseudo-random
/// Crockford-base32 (80 bits), for 26 chars total — matching ULID's shape
/// without pulling in a dependency for it.
pub fn generate_session_id() -> String {
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    generate_session_id_at(now_ms)
}

fn generate_session_id_at(now_ms: u64) -> String {
    let mut out = String::with_capacity(26);

    // 48-bit timestamp -> 10 base32 chars.
    let ts = now_ms & 0xFFFF_FFFF_FFFF;
    for i in (0..10).rev() {
        let shift = i * 5;
        let idx = ((ts >> shift) & 0x1F) as usize;
        out.push(CROCKFORD_ALPHABET[idx] as char);
    }

    // 80 bits of randomness, seeded from a xorshift PRNG so we don't need a
    // `rand` dependency; entropy source mixes wall-clock nanos with a
    // process-local counter.
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let counter = COUNTER.fetch_add(1, Ordering::SeqCst);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64;
    let mut state =
        nanos ^ counter.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (now_ms << 1) ^ 0xD1B5_4A32_D192_ED03;
    if state == 0 {
        state = 0xA5A5_A5A5_A5A5_A5A5;
    }

    let mut rand_words = [0u64; 2];
    for word in rand_words.iter_mut() {
        // xorshift64*
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        *word = state.wrapping_mul(0x2545_F491_4F6C_DD1D);
    }

    let rand_bits: u128 = ((rand_words[0] as u128) << 64) | (rand_words[1] as u128);
    for i in (0..16).rev() {
        let shift = i * 5;
        let idx = ((rand_bits >> shift) & 0x1F) as usize;
        out.push(CROCKFORD_ALPHABET[idx] as char);
    }

    out
}

/// Returns true when the gap since the last file event exceeds `idle_limit`
/// (C2: idle gap > 4h splits a new session id).
pub fn should_start_new_session(
    last_event_at: SystemTime,
    now: SystemTime,
    idle_limit: Duration,
) -> bool {
    match now.duration_since(last_event_at) {
        Ok(gap) => gap > idle_limit,
        Err(_) => false, // clock went backwards; don't spuriously split
    }
}

/// Tracks the current session id and rotates it on idle-gap splits.
pub struct SessionManager {
    pub session_id: String,
    pub started_at: SystemTime,
    last_event_at: SystemTime,
    idle_limit: Duration,
}

impl SessionManager {
    pub fn new(now: SystemTime) -> Self {
        Self::with_idle_limit(now, DEFAULT_IDLE_SPLIT)
    }

    pub fn with_idle_limit(now: SystemTime, idle_limit: Duration) -> Self {
        Self {
            session_id: generate_session_id(),
            started_at: now,
            last_event_at: now,
            idle_limit,
        }
    }

    /// Records a file event at `now`. Returns true if a new session was
    /// started as a result (idle gap exceeded).
    pub fn on_file_event(&mut self, now: SystemTime) -> bool {
        let split = should_start_new_session(self.last_event_at, now, self.idle_limit);
        if split {
            self.session_id = generate_session_id();
            self.started_at = now;
        }
        self.last_event_at = now;
        split
    }
}

/// A session-start snapshot entry: the worktree content of a tracked-and-
/// modified file at session start, plus its SHA-256 content hash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileSnapshot {
    pub content: String,
    pub content_hash: String,
}

/// C2 session-start snapshot: content hashes (and the content needed to diff
/// against) of every tracked-and-modified file at watcher start.
#[derive(Debug, Clone, Default)]
pub struct SessionSnapshot {
    pub files: HashMap<PathBuf, FileSnapshot>,
}

fn parse_git_status_porcelain(output: &str) -> Vec<String> {
    let mut paths = Vec::new();
    for line in output.lines() {
        if line.len() < 4 {
            continue;
        }
        let status = &line[0..2];
        if status == "??" || status == "!!" {
            continue; // untracked / ignored — not "tracked-and-modified"
        }
        let rest = &line[3..];
        let path = if let Some(idx) = rest.find(" -> ") {
            &rest[idx + 4..]
        } else {
            rest
        };
        let path = path.trim().trim_matches('"');
        if !path.is_empty() {
            paths.push(path.to_string());
        }
    }
    paths
}

/// Runs `git status --porcelain` (I2's shell-out convention — no libgit2) to
/// find tracked-and-modified files relative to `project_root`.
pub fn tracked_and_modified_files(project_root: &Path) -> Result<Vec<PathBuf>, String> {
    let output = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(project_root)
        .output()
        .map_err(|e| format!("Failed to run git status: {}", e))?;

    if !output.status.success() {
        return Err(format!(
            "git status failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(parse_git_status_porcelain(&stdout)
        .into_iter()
        .map(PathBuf::from)
        .collect())
}

/// Snapshots content hashes (C2) of all tracked-and-modified files at
/// session/watcher start.
pub fn snapshot_session_start(project_root: &Path) -> Result<SessionSnapshot, String> {
    let files = tracked_and_modified_files(project_root)?;
    let mut snapshot = SessionSnapshot::default();
    for rel in files {
        let abs = project_root.join(&rel);
        if let Ok(content) = std::fs::read_to_string(&abs) {
            let content_hash = crate::sha256::sha256_hex(content.as_bytes());
            snapshot.files.insert(
                rel,
                FileSnapshot {
                    content,
                    content_hash,
                },
            );
        }
    }
    Ok(snapshot)
}

/// Returns the committed HEAD content of `rel_path`, or `None` if the file
/// has no HEAD blob (e.g. it's new/untracked).
fn get_head_content(project_root: &Path, rel_path: &Path) -> Option<String> {
    let spec = format!("HEAD:{}", rel_path.to_string_lossy().replace('\\', "/"));
    let output = Command::new("git")
        .args(["show", &spec])
        .current_dir(project_root)
        .output()
        .ok()?;
    if output.status.success() {
        Some(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        None
    }
}

/// Resolves the diff baseline for a changed file per C2/T1#3: the session-
/// start snapshot content if the file was tracked-and-modified at session
/// start, otherwise its HEAD content (empty string if it has none).
pub fn baseline_content(
    project_root: &Path,
    rel_path: &Path,
    snapshot: &SessionSnapshot,
) -> String {
    if let Some(entry) = snapshot.files.get(rel_path) {
        entry.content.clone()
    } else {
        get_head_content(project_root, rel_path).unwrap_or_default()
    }
}

/// Computes the session diff for one changed file at a quiescence moment:
/// current worktree content vs the resolved baseline, as unified hunks.
pub fn compute_session_diff(
    project_root: &Path,
    rel_path: &Path,
    snapshot: &SessionSnapshot,
) -> Result<Vec<crate::diff::Hunk>, String> {
    let abs = project_root.join(rel_path);
    let current = std::fs::read_to_string(&abs).map_err(|e| e.to_string())?;
    let baseline = baseline_content(project_root, rel_path, snapshot);
    Ok(crate::diff::diff_lines(&baseline, &current))
}

/// T4 req 12 / D18: "offered ... at commit detection" — the current HEAD
/// commit hash, for spotting a commit as it happens (I2's shell-out
/// convention, no libgit2).
pub fn current_head_commit(project_root: &Path) -> Option<String> {
    let output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(project_root)
        .output()
        .ok()?;
    if output.status.success() {
        let hash = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if hash.is_empty() { None } else { Some(hash) }
    } else {
        None
    }
}

/// req 12: whether a commit just happened between two HEAD reads. `None`
/// (no prior read yet, e.g. the very first sweep) never counts as a commit.
pub fn head_commit_changed(previous: Option<&str>, current: Option<&str>) -> bool {
    match (previous, current) {
        (Some(p), Some(c)) => p != c,
        _ => false,
    }
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

    fn git_add_commit(dir: &Path, msg: &str) {
        Command::new("git")
            .args(["add", "-A"])
            .current_dir(dir)
            .output()
            .unwrap();
        Command::new("git")
            .args([
                "-c",
                "user.email=test@test.com",
                "-c",
                "user.name=Test",
                "commit",
                "-q",
                "-m",
                msg,
            ])
            .current_dir(dir)
            .output()
            .unwrap();
    }

    // --- session id / split logic ---

    #[test]
    fn test_session_id_is_ulid_like() {
        let id = generate_session_id();
        assert_eq!(id.len(), 26);
        for c in id.chars() {
            assert!(
                CROCKFORD_ALPHABET.contains(&(c as u8)),
                "unexpected char {} in session id {}",
                c,
                id
            );
        }
    }

    #[test]
    fn test_session_ids_are_unique() {
        let a = generate_session_id();
        let b = generate_session_id();
        assert_ne!(a, b);
    }

    #[test]
    fn test_session_split_on_idle_gap() {
        let t0 = UNIX_EPOCH + Duration::from_secs(1_000_000);
        let mut mgr = SessionManager::with_idle_limit(t0, DEFAULT_IDLE_SPLIT);
        let first_id = mgr.session_id.clone();

        // Small gap: same session.
        let t1 = t0 + Duration::from_secs(60);
        let split1 = mgr.on_file_event(t1);
        assert!(!split1);
        assert_eq!(mgr.session_id, first_id);

        // Gap > 4h: new session.
        let t2 = t1 + Duration::from_secs(4 * 3600 + 1);
        let split2 = mgr.on_file_event(t2);
        assert!(split2);
        assert_ne!(mgr.session_id, first_id);

        // Gap of exactly 4h: not a split (strictly greater than per C2).
        let t3 = t2 + Duration::from_secs(4 * 3600);
        let id_before = mgr.session_id.clone();
        let split3 = mgr.on_file_event(t3);
        assert!(!split3);
        assert_eq!(mgr.session_id, id_before);
    }

    // --- snapshot / session diff ---

    #[test]
    fn test_snapshot_captures_uncommitted_worktree_state() {
        let temp_dir = std::env::temp_dir();
        let root = temp_dir.join("murshid_test_session_snapshot");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        init_git_repo(&root);

        let file = root.join("src_lib.rs");
        fs::write(&file, "fn a() {}\n").unwrap();
        git_add_commit(&root, "add file");

        // Uncommitted edit present before session start (C2: "belongs to the window").
        fs::write(&file, "fn a() {}\nfn b() {}\n").unwrap();

        let snapshot = snapshot_session_start(&root).unwrap();
        let rel = PathBuf::from("src_lib.rs");
        let entry = snapshot.files.get(&rel).expect("expected snapshot entry");
        assert_eq!(entry.content, "fn a() {}\nfn b() {}\n");
        assert_eq!(
            entry.content_hash,
            crate::sha256::sha256_hex(entry.content.as_bytes())
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn test_session_diff_against_snapshot() {
        let temp_dir = std::env::temp_dir();
        let root = temp_dir.join("murshid_test_session_diff_snapshot");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        init_git_repo(&root);

        let file = root.join("lib.rs");
        fs::write(&file, "fn a() {}\n").unwrap();
        git_add_commit(&root, "add file");

        // Modify before session start.
        fs::write(&file, "fn a() {}\nfn b() {}\n").unwrap();
        let snapshot = snapshot_session_start(&root).unwrap();

        // Further edit after session start.
        fs::write(&file, "fn a() {}\nfn b() {}\nfn c() {}\n").unwrap();

        let hunks = compute_session_diff(&root, Path::new("lib.rs"), &snapshot).unwrap();
        let changed = crate::diff::changed_line_numbers(&hunks);
        // Only the line added after session start should show as changed —
        // not the fn b() line that predates the session snapshot.
        assert_eq!(changed, vec![3]);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn test_session_diff_falls_back_to_head_for_file_unmodified_at_start() {
        let temp_dir = std::env::temp_dir();
        let root = temp_dir.join("murshid_test_session_diff_head");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        init_git_repo(&root);

        let file = root.join("lib.rs");
        fs::write(&file, "fn a() {}\n").unwrap();
        git_add_commit(&root, "add file");

        // Clean at session start: not in tracked-and-modified snapshot.
        let snapshot = snapshot_session_start(&root).unwrap();
        assert!(!snapshot.files.contains_key(Path::new("lib.rs")));

        // Edit made during the session.
        fs::write(&file, "fn a() {}\nfn b() {}\n").unwrap();

        let hunks = compute_session_diff(&root, Path::new("lib.rs"), &snapshot).unwrap();
        let changed = crate::diff::changed_line_numbers(&hunks);
        assert_eq!(changed, vec![2]);

        let _ = fs::remove_dir_all(&root);
    }

    // --- T4 req 12: commit detection ---

    #[test]
    fn test_current_head_commit_returns_a_hash_after_a_commit() {
        let temp_dir = std::env::temp_dir();
        let root = temp_dir.join("murshid_test_head_commit");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        init_git_repo(&root);

        let hash1 = current_head_commit(&root).unwrap();
        assert_eq!(hash1.len(), 40);

        fs::write(root.join("a.rs"), "fn a() {}\n").unwrap();
        git_add_commit(&root, "add a");

        let hash2 = current_head_commit(&root).unwrap();
        assert_ne!(hash1, hash2);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn test_head_commit_changed_detects_transition() {
        assert!(head_commit_changed(Some("abc"), Some("def")));
        assert!(!head_commit_changed(Some("abc"), Some("abc")));
    }

    #[test]
    fn test_head_commit_changed_never_fires_without_a_prior_read() {
        assert!(!head_commit_changed(None, Some("abc")));
        assert!(!head_commit_changed(None, None));
    }
}
