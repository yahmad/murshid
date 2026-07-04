//! `murshid progress` — renders the open skill meter (I24): per-concept
//! mastery, help level, and staleness read straight from concept memory.

use crate::{config, db, pack, progress, throttle};

/// T5 req 9 / I24: the open per-concept skill meter — plain text, no live
/// session needed.
/// T10 req 1: resolved dynamically (config override -> project marker ->
/// rust fallback) instead of a hardcoded pack, same as every other command
/// that loads a pack. No path arg for this command, so the project root is
/// cwd (same convention `goal` uses).
pub fn run() {
    let project_root = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let cfg = config::load_config();
    let pack_dir = pack::resolve_pack_dir(&project_root, &cfg);
    let taxonomy = pack::load_or_notice(pack::load_taxonomy(&pack_dir), "taxonomy", &pack_dir);
    let Some(dp) = db::get_db_path() else {
        eprintln!("progress: could not resolve the database path");
        std::process::exit(1);
    };
    let Ok(conn) = db::open_connection(&dp) else {
        eprintln!("progress: could not open the database");
        std::process::exit(1);
    };
    let memory_rows = db::list_concept_memory(&conn).unwrap_or_default();

    // req 9's "throttled category flag from T2" — read-only:
    // this command never logs a `throttle_change` transition
    // (that belongs to a live session), it only reports the
    // CURRENTLY computed state.
    let mut throttled_categories = std::collections::HashSet::new();
    for category in ["bug", "idiom", "best-practice", "architecture"] {
        let statuses =
            db::recent_card_statuses_for_category(&conn, category, throttle::THROTTLE_WINDOW)
                .unwrap_or_default();
        let unthrottled_by_config = cfg.dial.unthrottle.iter().any(|c| c == category);
        let is_throttled = throttle::action_rate(&statuses)
            .map(throttle::is_throttled)
            .unwrap_or(false)
            && !unthrottled_by_config;
        if is_throttled {
            throttled_categories.insert(category.to_string());
        }
    }

    let now_epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let rows = progress::build_rows(&memory_rows, &taxonomy, &throttled_categories, now_epoch);
    print!("{}", progress::render_progress(&rows));
    std::process::exit(0);
}
