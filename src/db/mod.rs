//! The persistence layer: SQLite (bundled rusqlite) connection setup,
//! schema migrations, and every data-access function for the C5 tables
//! (`events`, `cards`, `concept_memory`, `threads`, `suppressions`). The
//! `events` table is the append-only spine from which noise and struggle
//! state are recomputed at session start rather than stored.
//!
//! Split (2026-07 clarity pass) from a single 3.3k-line file into per-
//! domain submodules; this module retains the connection/retry/observability
//! primitives and re-exports every submodule so the public path stays
//! `db::<name>` for all callers.

pub(crate) use rusqlite::{Connection, OptionalExtension, Result};
pub(crate) use std::fs;
pub(crate) use std::path::{Path, PathBuf};
pub(crate) use std::thread::sleep;
pub(crate) use std::time::Duration;

mod cards;
mod concept_memory;
mod events;
mod history;
mod migrations;
mod suppressions;
mod threads;

pub use cards::*;
pub use concept_memory::*;
pub use events::*;
pub use history::*;
pub use migrations::*;
pub use suppressions::*;
pub use threads::*;

pub fn get_db_path() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        crate::config::get_home_dir()
            .map(|h| h.join("Library/Application Support/murshid/profile.db"))
    }
    #[cfg(target_os = "windows")]
    {
        std::env::var("APPDATA")
            .ok()
            .map(|a| PathBuf::from(a).join("murshid\\profile.db"))
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        crate::config::get_home_dir().map(|h| h.join(".local/share/murshid/profile.db"))
    }
}

pub fn open_connection<P: AsRef<Path>>(path: P) -> Result<Connection> {
    let conn = Connection::open(path)?;

    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.busy_timeout(Duration::from_millis(5000))?;

    // Catch early database corruption by running integrity check
    let check: String = conn.query_row("PRAGMA integrity_check(1);", [], |row| row.get(0))?;
    if check != "ok" {
        return Err(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CORRUPT),
            Some(format!("Integrity check failed: {}", check)),
        ));
    }

    Ok(conn)
}

pub fn execute_with_retry<F, T>(mut f: F) -> Result<T, rusqlite::Error>
where
    F: FnMut() -> Result<T, rusqlite::Error>,
{
    let mut backoff = Duration::from_millis(50);
    let max_backoff = Duration::from_millis(5000);

    loop {
        match f() {
            Ok(val) => return Ok(val),
            Err(e) => {
                if is_busy_error(&e) {
                    if backoff >= max_backoff {
                        return Err(e);
                    }

                    let jitter_ms = (rand_jitter() % 21) as i64 - 10; // +/-10ms
                    let sleep_duration = Duration::from_millis(
                        (backoff.as_millis() as i64 + jitter_ms).max(1) as u64,
                    );

                    sleep(sleep_duration);
                    backoff *= 2;
                } else {
                    return Err(e);
                }
            }
        }
    }
}

fn is_busy_error(err: &rusqlite::Error) -> bool {
    if let rusqlite::Error::SqliteFailure(ffi_err, _) = err {
        ffi_err.code == rusqlite::ErrorCode::DatabaseBusy
            || ffi_err.code == rusqlite::ErrorCode::DatabaseLocked
    } else {
        false
    }
}

/// A classified persistence-boundary error (ROADMAP item 2). `rusqlite::Error`
/// is opaque about *why* a call failed; this splits out the three outcomes a
/// caller might plausibly treat differently — a transient `Busy`/`Locked`
/// (retry), a `Corrupt` store (restore from backup), and a `NotFound` (a query
/// that returned no rows, made explicit instead of folded into `Ok(None)`) —
/// from everything else (`Backend`, carried verbatim). Classification reuses the
/// existing [`is_busy_error`]/[`migrations::is_corrupt_error`] predicates so the
/// retry/recovery paths and this boundary can never disagree.
///
/// Intentionally not yet threaded through every db signature: nothing branches
/// on it today, so adoption is per-caller as a need arises — this is the type
/// and its (tested) classification, ready to use.
#[derive(Debug)]
pub enum Error {
    /// The database was busy or locked — the retry helpers treat this as
    /// transient and re-run.
    Busy,
    /// The database file is corrupt or not a database — recover from backup.
    Corrupt,
    /// A query that expected a row found none.
    NotFound,
    /// Any other backend error, carried verbatim.
    Backend(rusqlite::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Busy => write!(f, "database busy or locked"),
            Error::Corrupt => write!(f, "database corrupt or not a database"),
            Error::NotFound => write!(f, "no matching row"),
            Error::Backend(e) => write!(f, "{}", e),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Backend(e) => Some(e),
            _ => None,
        }
    }
}

impl From<rusqlite::Error> for Error {
    fn from(err: rusqlite::Error) -> Self {
        if matches!(err, rusqlite::Error::QueryReturnedNoRows) {
            Error::NotFound
        } else if is_busy_error(&err) {
            Error::Busy
        } else if migrations::is_corrupt_error(&err) {
            Error::Corrupt
        } else {
            Error::Backend(err)
        }
    }
}

fn rand_jitter() -> u32 {
    use std::time::SystemTime;
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u32)
        .unwrap_or(42)
}

/// A `cards`-mutation + `log_event` pair (or any other multi-statement write)
/// is otherwise two independent autocommit statements with a window between
/// them; since noise/throttle/BKT state is *derived from the event log*, a
/// partial failure there desyncs `cards` from `events`. `with_tx` closes that
/// window: `f` runs inside one SQLite transaction that either commits every
/// statement it makes or rolls all of them back.
///
/// `with_tx` OWNS the retry — the *whole* closure re-runs (opening a fresh
/// transaction each attempt) on a busy error, mirroring
/// [`history::restore_db_from_backup`]'s proven pattern. Because of that, `f`
/// must call PLAIN (non-retrying) statement fns internally — a per-statement
/// retry nested inside a still-held transaction would retry against a
/// transaction that may itself be about to roll back, which is wrong. Use
/// the `_stmt` variants of the write helpers (e.g. [`update_card_status_stmt`])
/// inside `f`, not the public retrying fns.
pub fn with_tx<T, F>(conn: &Connection, f: F) -> Result<T, rusqlite::Error>
where
    F: Fn(&Connection) -> Result<T, rusqlite::Error>,
{
    execute_with_retry(|| {
        let tx = conn.unchecked_transaction()?;
        let out = f(&tx)?;
        tx.commit()?;
        Ok(out)
    })
}

/// Records a discarded write failure to stderr instead of dropping it
/// silently. State-mutating writes (card status/rung transitions, suppression
/// inserts, requeues) are best-effort at their call sites — the watch loop
/// must not abort on a transient busy-timeout — but a *silent* drop lets the
/// dedup ledger and BKT mastery evidence diverge from what the user actually
/// did (advice re-raises, mastery is lost) with no diagnostic. Routing those
/// writes through this helper keeps them non-fatal while making a real failure
/// observable, matching the `eprintln!`-on-recovery-failure discipline already
/// used inside `initialize_db`. `context` should name the write, e.g.
/// `"update_card_status(applied)"`.
pub fn warn_on_err<T, E: std::fmt::Display>(result: Result<T, E>, context: &str) {
    if let Err(e) = result {
        eprintln!("murshid: db write failed [{context}]: {e}");
    }
}

#[cfg(test)]
mod test_support;

#[cfg(test)]
mod tests {
    use super::*;

    fn sqlite_failure(code: rusqlite::ErrorCode) -> rusqlite::Error {
        rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error {
                code,
                extended_code: 0,
            },
            None,
        )
    }

    #[test]
    fn test_error_classifies_no_rows_as_not_found() {
        assert!(matches!(
            Error::from(rusqlite::Error::QueryReturnedNoRows),
            Error::NotFound
        ));
    }

    #[test]
    fn test_error_classifies_busy_and_locked() {
        assert!(matches!(
            Error::from(sqlite_failure(rusqlite::ErrorCode::DatabaseBusy)),
            Error::Busy
        ));
        assert!(matches!(
            Error::from(sqlite_failure(rusqlite::ErrorCode::DatabaseLocked)),
            Error::Busy
        ));
    }

    #[test]
    fn test_error_classifies_corrupt_and_not_a_database() {
        assert!(matches!(
            Error::from(sqlite_failure(rusqlite::ErrorCode::DatabaseCorrupt)),
            Error::Corrupt
        ));
        assert!(matches!(
            Error::from(sqlite_failure(rusqlite::ErrorCode::NotADatabase)),
            Error::Corrupt
        ));
    }

    #[test]
    fn test_error_classifies_everything_else_as_backend() {
        // A constraint violation is a real backend error, not busy/corrupt/none.
        assert!(matches!(
            Error::from(sqlite_failure(rusqlite::ErrorCode::ConstraintViolation)),
            Error::Backend(_)
        ));
    }
}
