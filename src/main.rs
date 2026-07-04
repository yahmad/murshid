//! T13 req 3: thin binary entry point over the `murshid` library crate
//! (`src/lib.rs`) — module declarations and the cross-module helpers
//! (`resolve_slot_key`, `goal_text_now`, `persist_review_digest`,
//! `run_review`) live there now so `tests/` integration tests can reach the
//! real pipeline through the crate's public surface.
//!
//! CLI hygiene batch: `main` is the ONLY place `std::process::exit` is
//! called. Argv parsing and subcommand routing live in `cli::dispatch`,
//! which returns the exit code instead of exiting inline.

use murshid::{cli, config, db};

fn main() {
    // Surfaces the system-config security-check warning (if any) up front,
    // even for subcommands (setup/goal) that don't load config themselves.
    // The resolved config is intentionally discarded here — call sites that
    // need it (review/progress/watch) call `config::load_config()` again.
    let _ = config::load_config();
    if let Some(db_path) = db::get_db_path() {
        if let Err(e) = db::initialize_db(&db_path) {
            eprintln!(
                "[WARNING] Failed to initialize database at {}: {}",
                db_path.display(),
                e
            );
        }
    }

    let args: Vec<String> = std::env::args().collect();
    std::process::exit(cli::dispatch(&args));
}
