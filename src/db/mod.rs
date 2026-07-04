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
