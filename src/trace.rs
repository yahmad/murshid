//! T14 req 1: raw-model-I/O trace capture. Persists each stage-1 (screen) /
//! stage-2 (judge) dispatch's raw request prompt and raw response (or
//! dispatch error) under `cli::setup::get_trace_logs_dir()`, so a judge
//! outcome — including a silent drop — is diagnosable from disk instead of
//! requiring source access plus ad hoc SQL against the profile DB (the
//! founder-dogfood incident that motivated this task).
//!
//! Bounding mirrors `specs/AMENDMENT-events-retention.md`'s ratified
//! pattern for the events table: a time-window prune run once at watch
//! startup, plus (here, since a trace entry is far larger than an events
//! row) a total-size backstop.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// T14 req 1: trace entries older than this are pruned at watch startup.
/// Deliberately shorter than the events table's 90/180-day windows (see the
/// events-retention amendment) — a trace entry carries a FULL raw
/// prompt/response pair, not a small JSON payload, so bounding at DB-table
/// timescales would let disk grow much faster under heavy sweep activity.
/// Two weeks is enough to diagnose "why did last week's session drop a
/// card" without becoming a permanent archive.
pub const TRACE_LOG_RETENTION_DAYS: u64 = 14;

/// T14 req 1 backstop: total trace-log directory size cap, enforced
/// (oldest day-file first) even inside the retention window, in case a
/// single day's traces balloon before the age-based prune would remove
/// them.
pub const TRACE_LOG_MAX_TOTAL_BYTES: u64 = 200 * 1024 * 1024; // 200 MiB

/// One stage-1 (screen) or stage-2 (judge) dispatch's raw request + outcome.
#[derive(Debug, Clone, serde::Serialize)]
pub struct TraceEntry {
    pub ts_ms: u128,
    pub session_id: String,
    /// "screen" (stage-1) or "judge" (stage-2).
    pub stage: String,
    pub provider: String,
    pub model: String,
    pub site_hint: String,
    pub request: String,
    pub response: Option<String>,
    pub error: Option<String>,
}

/// Milliseconds since the epoch — the same unit T3's `check_result` events
/// already use for `ts_ms`, so trace timestamps read consistently with the
/// rest of the crate's persisted timestamps.
pub fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn day_index(ts_ms: u128) -> u64 {
    (ts_ms / 86_400_000) as u64
}

fn trace_file_name(ts_ms: u128) -> String {
    format!("trace-{:06}.jsonl", day_index(ts_ms))
}

/// Appends one trace entry as a JSON line under `dir` (created on demand),
/// in the day-bucketed file `trace-<day-index>.jsonl` — bucketing by day
/// keeps [`prune_trace_logs`]'s age check a filename parse, with no per-file
/// mtime read needed.
pub fn write_trace_entry(dir: &Path, entry: &TraceEntry) -> std::io::Result<()> {
    fs::create_dir_all(dir)?;
    let path = dir.join(trace_file_name(entry.ts_ms));
    let mut f = OpenOptions::new().create(true).append(true).open(path)?;
    let line = serde_json::to_string(entry)
        .unwrap_or_else(|e| format!("{{\"error\":\"trace serialize failed: {}\"}}", e));
    writeln!(f, "{}", line)?;
    Ok(())
}

/// T14 req 1: records one dispatch's raw I/O when tracing is enabled (`dir`
/// is `Some`) — a no-op when disabled. Never fatal: a write failure is
/// reported to stderr and otherwise swallowed, exactly like
/// [`crate::db::warn_on_err`]'s discipline for best-effort writes — tracing
/// must never be why a dispatch fails.
#[allow(clippy::too_many_arguments)]
pub fn record_dispatch(
    dir: Option<&Path>,
    session_id: &str,
    stage: &str,
    provider: &str,
    model: &str,
    site_hint: &str,
    request: &str,
    result: &Result<String, String>,
) {
    let Some(dir) = dir else { return };
    let entry = TraceEntry {
        ts_ms: now_ms(),
        session_id: session_id.to_string(),
        stage: stage.to_string(),
        provider: provider.to_string(),
        model: model.to_string(),
        site_hint: site_hint.to_string(),
        request: request.to_string(),
        response: result.as_ref().ok().cloned(),
        error: result.as_ref().err().cloned(),
    };
    if let Err(e) = write_trace_entry(dir, &entry) {
        eprintln!("murshid: trace write failed: {e}");
    }
}

fn parse_day_from_filename(path: &Path) -> Option<u64> {
    let name = path.file_stem()?.to_str()?;
    let day_str = name.strip_prefix("trace-")?;
    day_str.parse::<u64>().ok()
}

/// T14 req 1 / mirrors `db::prune_expired_history`: deletes trace files
/// older than [`TRACE_LOG_RETENTION_DAYS`] (by day-bucketed filename, not
/// mtime), then — as a backstop — deletes the oldest remaining files until
/// the directory is back under [`TRACE_LOG_MAX_TOTAL_BYTES`]. Returns the
/// number of files deleted. Meant to run once per watch-session startup,
/// the same call-site timing as `prune_expired_history`. Never touches a
/// file that doesn't match the `trace-<digits>.jsonl` shape.
pub fn prune_trace_logs(dir: &Path, now_ms: u128) -> std::io::Result<usize> {
    if !dir.exists() {
        return Ok(0);
    }
    let floor_day = day_index(now_ms).saturating_sub(TRACE_LOG_RETENTION_DAYS);
    let mut deleted = 0usize;
    let mut kept: Vec<(PathBuf, u64, u64)> = Vec::new();

    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let Some(day) = parse_day_from_filename(&path) else {
            continue; // not one of ours — never touch unrelated files
        };
        let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
        if day < floor_day {
            if fs::remove_file(&path).is_ok() {
                deleted += 1;
            }
        } else {
            kept.push((path, day, size));
        }
    }

    let mut total: u64 = kept.iter().map(|(_, _, s)| s).sum();
    if total > TRACE_LOG_MAX_TOTAL_BYTES {
        kept.sort_by_key(|(_, day, _)| *day); // oldest first
        for (path, _, size) in kept {
            if total <= TRACE_LOG_MAX_TOTAL_BYTES {
                break;
            }
            if fs::remove_file(&path).is_ok() {
                deleted += 1;
                total = total.saturating_sub(size);
            }
        }
    }

    Ok(deleted)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("murshid_trace_test_{}", tag));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn sample_entry(ts_ms: u128) -> TraceEntry {
        TraceEntry {
            ts_ms,
            session_id: "sess1".to_string(),
            stage: "screen".to_string(),
            provider: "gemini".to_string(),
            model: "gemini-3.1-flash-lite".to_string(),
            site_hint: "src/main.rs".to_string(),
            request: "prompt text".to_string(),
            response: Some("[]".to_string()),
            error: None,
        }
    }

    #[test]
    fn test_write_trace_entry_creates_dir_on_demand() {
        let dir = tmp_dir("create_dir");
        assert!(!dir.exists());
        write_trace_entry(&dir, &sample_entry(1_000)).unwrap();
        assert!(dir.exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_write_trace_entry_appends_json_lines_to_day_bucket() {
        let dir = tmp_dir("append");
        fs::create_dir_all(&dir).unwrap();
        let day_ms = 5 * 86_400_000u128;
        write_trace_entry(&dir, &sample_entry(day_ms)).unwrap();
        write_trace_entry(&dir, &sample_entry(day_ms + 1000)).unwrap();

        let path = dir.join(trace_file_name(day_ms));
        let content = fs::read_to_string(&path).unwrap();
        assert_eq!(content.lines().count(), 2);
        assert!(content.contains("\"stage\":\"screen\""));
        assert!(content.contains("gemini-3.1-flash-lite"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_record_dispatch_noop_when_dir_none() {
        // Must not panic and must not create anything.
        record_dispatch(
            None,
            "sess1",
            "screen",
            "gemini",
            "gemini-3.1-flash-lite",
            "src/main.rs",
            "prompt",
            &Ok("response".to_string()),
        );
    }

    #[test]
    fn test_record_dispatch_writes_error_outcome() {
        let dir = tmp_dir("record_error");
        record_dispatch(
            Some(&dir),
            "sess1",
            "judge",
            "claude",
            "claude-3-5-sonnet-20241022",
            "fn foo",
            "prompt text",
            &Err("provider unreachable".to_string()),
        );
        let entries: Vec<_> = fs::read_dir(&dir).unwrap().collect();
        assert_eq!(entries.len(), 1);
        let content = fs::read_to_string(entries[0].as_ref().unwrap().path()).unwrap();
        assert!(content.contains("provider unreachable"));
        assert!(content.contains("\"stage\":\"judge\""));
        let _ = fs::remove_dir_all(&dir);
    }

    // --- pruning ---

    #[test]
    fn test_prune_trace_logs_deletes_past_retention_floor_keeps_recent() {
        let dir = tmp_dir("prune_floor");
        fs::create_dir_all(&dir).unwrap();
        let now = 1_000 * 86_400_000u128; // day 1000
        let old_day = 1_000 - (TRACE_LOG_RETENTION_DAYS as u128) - 5; // well past floor
        let recent_day = 1_000 - 1; // inside the window

        fs::write(dir.join(trace_file_name(old_day * 86_400_000)), "old\n").unwrap();
        fs::write(
            dir.join(trace_file_name(recent_day * 86_400_000)),
            "recent\n",
        )
        .unwrap();

        let deleted = prune_trace_logs(&dir, now).unwrap();
        assert_eq!(deleted, 1);
        assert!(!dir.join(trace_file_name(old_day * 86_400_000)).exists());
        assert!(dir
            .join(trace_file_name(recent_day * 86_400_000))
            .exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_prune_trace_logs_idempotent_when_nothing_expired() {
        let dir = tmp_dir("prune_idempotent");
        fs::create_dir_all(&dir).unwrap();
        let now = 1_000 * 86_400_000u128;
        fs::write(dir.join(trace_file_name(now)), "today\n").unwrap();
        assert_eq!(prune_trace_logs(&dir, now).unwrap(), 0);
        assert_eq!(prune_trace_logs(&dir, now).unwrap(), 0);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_prune_trace_logs_missing_dir_is_a_noop() {
        let dir = tmp_dir("prune_missing");
        assert!(!dir.exists());
        assert_eq!(prune_trace_logs(&dir, now_ms()).unwrap(), 0);
    }

    #[test]
    fn test_prune_trace_logs_ignores_unrelated_files() {
        let dir = tmp_dir("prune_ignores_unrelated");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("not-a-trace-file.txt"), "hello\n").unwrap();
        let deleted = prune_trace_logs(&dir, now_ms()).unwrap();
        assert_eq!(deleted, 0);
        assert!(dir.join("not-a-trace-file.txt").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_prune_trace_logs_size_backstop_deletes_oldest_first() {
        let dir = tmp_dir("prune_size_backstop");
        fs::create_dir_all(&dir).unwrap();
        let now = 1_000 * 86_400_000u128;

        // Three same-window (recent) day-files, each over 1/2 the cap, so
        // the total exceeds TRACE_LOG_MAX_TOTAL_BYTES and the backstop must
        // trim the oldest even though none are outside the age window.
        let per_file = (TRACE_LOG_MAX_TOTAL_BYTES / 2) + 1;
        let filler = "x".repeat(per_file as usize);
        let day_oldest = now - 2 * 86_400_000;
        let day_mid = now - 86_400_000;
        let day_newest = now;

        fs::write(dir.join(trace_file_name(day_oldest)), &filler).unwrap();
        fs::write(dir.join(trace_file_name(day_mid)), &filler).unwrap();
        fs::write(dir.join(trace_file_name(day_newest)), &filler).unwrap();

        let deleted = prune_trace_logs(&dir, now).unwrap();
        assert!(deleted >= 1, "backstop must delete at least the oldest file");
        assert!(
            !dir.join(trace_file_name(day_oldest)).exists(),
            "oldest file must go first"
        );
        assert!(
            dir.join(trace_file_name(day_newest)).exists(),
            "newest file must survive the backstop"
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
