pub mod aggregate;
pub mod backup;
pub mod budget;
pub mod card;
pub mod compiler;
pub mod config;
pub mod context;
pub mod credentials;
pub mod db;
pub mod diff;
pub mod judge;
pub mod noise;
pub mod pack;
pub mod pipeline;
pub mod provider;
pub mod queue;
pub mod quiescence;
pub mod response;
pub mod sanitizer;
pub mod session;
pub mod sha256;
pub mod site;
pub mod suppression;
pub mod throttle;
pub mod watcher;
pub mod watcher_coordinator;

#[path = "cli/goal.rs"]
pub mod cli_goal;
#[path = "cli/setup.rs"]
pub mod cli_setup;

fn print_usage() {
    println!("Murshid — Local-First Socratic AI Coding Mentor");
    println!("\nUsage:");
    println!("  murshid <command> [args]");
    println!("\nCommands:");
    println!(
        "  setup [path]                           Onboard a new project (auto-adds .murshid/ to .gitignore and parses .env)"
    );
    println!(
        "  watch [path]                           Watch a directory for code updates to trigger Socratic mentor feedback"
    );
    println!(
        "  goal <command> [args]                  Manage project active goals (set, get, complete, list)"
    );
}

/// Resolves a model slot's key using the existing keyring/env flow (C6:
/// "Keys via existing keyring/env flow"). Ollama needs no key.
fn resolve_slot_key(provider: &str, keys: &Option<credentials::CachedKeys>) -> Option<String> {
    match provider {
        "claude" => keys.as_ref().and_then(|k| k.claude_api_key.clone()),
        "gemini" => keys.as_ref().and_then(|k| k.gemini_api_key.clone()),
        _ => None, // ollama (local) and anything else: no key
    }
}

/// State pinned to the single card currently on screen (T1: at most one),
/// awaiting a `g`/`u`/`n` response (req 10). T2 req 8 extends this with the
/// fields the tiered-snooze `n` handler needs (advice_fp/concept_name).
#[derive(Clone)]
struct PendingCard {
    card_id: i64,
    session_id: String,
    concept_id: String,
    concept_name: String,
    advice_fp: String,
}

/// T2 req 1/10: the four taxonomy categories throttle/floor decisions key
/// on (C4/C8).
const CATEGORIES: [&str; 4] = ["bug", "idiom", "best-practice", "architecture"];

/// T2 req 10 / C5: computes each category's throttle state fresh from
/// `cards`/`events` history (never stored), applies the config-key undo
/// override (T2 req 10's "config key" undo path), and logs a
/// `throttle_change` event for every actual transition. Returns the set of
/// currently-throttled categories.
fn compute_throttle_state(
    conn: &rusqlite::Connection,
    session_id: &str,
    unthrottle: &[String],
) -> std::collections::HashSet<String> {
    let mut throttled = std::collections::HashSet::new();
    for category in CATEGORIES {
        let statuses =
            db::recent_card_statuses_for_category(conn, category, throttle::THROTTLE_WINDOW)
                .unwrap_or_default();
        let unthrottled_by_config = unthrottle.iter().any(|c| c == category);
        let prior_throttled = db::latest_throttle_action(conn, category)
            .unwrap_or(None)
            .as_deref()
            == Some("throttled");

        let (computed_throttled, transition) =
            throttle::decide_throttle_transition(&statuses, unthrottled_by_config, prior_throttled);

        if let Some(action) = transition {
            let _ = db::log_event(
                conn,
                &db::EventRecord {
                    id: None,
                    session_id: session_id.to_string(),
                    kind: "throttle_change".to_string(),
                    payload_json: serde_json::json!({
                        "category": category,
                        "action": action,
                    })
                    .to_string(),
                    ts: None,
                },
            );
            if computed_throttled {
                println!(
                    "  {} is quiet lately \u{2014} queue-only for now (undo: [dial] unthrottle)",
                    category
                );
            }
        }

        if computed_throttled {
            throttled.insert(category.to_string());
        }
    }
    throttled
}

/// T2 review fix / CD-1 BYOK-cost mandate: caps stage-1 (screen) dispatches
/// per sweep pass so a burst of saves across many files can't burn the
/// screen model unboundedly in one pass. C12-style tunable — raise/lower
/// per BYOK cost tolerance; nothing else depends on this exact value.
const MAX_STAGE1_DISPATCHES_PER_PASS: usize = 4;

/// Splits an ordered candidate list into the prefix this pass may consider
/// (at most `cap` items — an upper bound on stage-1 dispatches, since some
/// considered files may still short-circuit without dispatching at all,
/// e.g. empty hunks or an unchanged-since-last-dispatch signature) and the
/// remainder that must stay pending for the next pass. Mirrors the T1 sweep
/// fix's retain semantics: nothing in the remainder is ever dropped, only
/// deferred — the caller simply never removes it from `pending_files`.
fn cap_dispatch_batch<T>(mut items: Vec<T>, cap: usize) -> (Vec<T>, Vec<T>) {
    if items.len() <= cap {
        (items, Vec::new())
    } else {
        let tail = items.split_off(cap);
        (items, tail)
    }
}

/// T2 review fix (req 7): when a card for `concept_id` ships — auto-push or
/// pull, either path calls this — removes every sibling entry for the same
/// concept still sitting in the queue, marks their `cards` rows `collapsed`
/// (additive status, same precedent as `queued`), logs one
/// `card_aggregated` event per collapsed entry, and returns their (file,
/// line) anchors — including any they'd already folded in themselves — so
/// the caller can fold them into the just-shipped card's aggregation via
/// [`fold_anchors_into_card`].
fn collapse_queued_siblings(
    conn: &rusqlite::Connection,
    queue_state: &std::sync::Mutex<Vec<queue::QueueEntry>>,
    session_id: &str,
    concept_id: &str,
) -> Vec<(String, usize)> {
    let siblings: Vec<queue::QueueEntry> = {
        let mut q = queue_state.lock().unwrap();
        let (siblings, rest): (Vec<_>, Vec<_>) = q
            .drain(..)
            .partition(|e| e.finding.concept_id == concept_id);
        *q = rest;
        siblings
    };

    let mut anchors = Vec::new();
    for sib in siblings {
        let _ = db::update_card_status(conn, sib.card_id, "collapsed");
        let _ = db::log_event(
            conn,
            &db::EventRecord {
                id: None,
                session_id: session_id.to_string(),
                kind: "card_aggregated".to_string(),
                payload_json: serde_json::json!({
                    "concept": concept_id,
                    "collapsed_card_id": sib.card_id,
                })
                .to_string(),
                ts: None,
            },
        );
        anchors.push((sib.finding.card.file.clone(), sib.finding.card.line));
        anchors.extend(sib.finding.card.additional_anchors.iter().cloned());
    }
    anchors
}

/// T2 review fix (req 6/7): folds `extra` (file, line) anchors into `card`'s
/// aggregation, up to the 3-anchor cap (`aggregate::MAX_ANCHORS`); anything
/// beyond the cap tallies into `overflow_site_count` instead and is
/// returned so the caller can extend the event payload's `remaining_sites`.
fn fold_anchors_into_card(
    card: &mut card::Card,
    extra: Vec<(String, usize)>,
) -> Vec<(String, usize)> {
    let mut overflow = Vec::new();
    for anchor in extra {
        if card.additional_anchors.len() < aggregate::MAX_ANCHORS - 1 {
            card.additional_anchors.push(anchor);
        } else {
            card.overflow_site_count += 1;
            overflow.push(anchor);
        }
    }
    overflow
}

/// T2 review fix (req 7): whether a queued entry may still be shown once
/// pulled via `m` — re-checked right before showing (not just at enqueue
/// time), because a sibling entry for the same concept can ship (auto-push)
/// or get snoozed to concept-scope in the time between queuing and pulling.
/// Without this re-check a user could pull two cards for one concept.
fn pull_is_blocked(
    conn: &rusqlite::Connection,
    session_id: &str,
    concept_id: &str,
    advice_fp: &str,
) -> bool {
    let already_shipped =
        db::concept_shown_this_session(conn, session_id, concept_id).unwrap_or(false);
    let suppressed = db::is_suppressed(conn, session_id, concept_id, advice_fp).unwrap_or(false);
    already_shipped || suppressed
}

type ShutdownCleanup = std::sync::Mutex<Option<Box<dyn Fn() + Send>>>;
static SHUTDOWN_CLEANUP: std::sync::OnceLock<ShutdownCleanup> = std::sync::OnceLock::new();

#[cfg(unix)]
fn setup_sigint_handler() {
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| unsafe {
        libc::signal(libc::SIGINT, sigint_handler as usize);
    });
}

#[cfg(unix)]
extern "C" fn sigint_handler(_sig: libc::c_int) {
    std::thread::spawn(|| {
        if let Some(mutex) = SHUTDOWN_CLEANUP.get() {
            if let Ok(guard) = mutex.lock() {
                if let Some(ref cleanup) = *guard {
                    cleanup();
                }
            }
        }
        std::process::exit(0);
    });
}

fn main() {
    let _cfg = config::load_config();
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
    if args.len() > 1 {
        match args[1].as_str() {
            "setup" => {
                let project_root = if args.len() > 2 {
                    std::path::PathBuf::from(&args[2])
                } else {
                    std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
                };
                match cli_setup::run_setup(&project_root) {
                    Ok(_) => {
                        println!(
                            "Setup completed successfully for {}",
                            project_root.display()
                        );
                        std::process::exit(0);
                    }
                    Err(e) => {
                        eprintln!("Setup failed: {}", e);
                        std::process::exit(1);
                    }
                }
            }
            "goal" => {
                let db_path = match db::get_db_path() {
                    Some(p) => p,
                    None => {
                        eprintln!("Error: Database path not found");
                        std::process::exit(1);
                    }
                };
                let conn = match db::open_connection(&db_path) {
                    Ok(c) => c,
                    Err(e) => {
                        eprintln!("Failed to open database: {}", e);
                        std::process::exit(1);
                    }
                };
                match cli_goal::run_goal_cli(&conn, &args[2..]) {
                    Ok(_) => {
                        std::process::exit(0);
                    }
                    Err(e) => {
                        eprintln!("Goal command failed: {}", e);
                        std::process::exit(1);
                    }
                }
            }
            "watch" => {
                let project_root = if args.len() > 2 {
                    std::path::PathBuf::from(&args[2])
                } else {
                    std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
                };
                let project_root = match project_root.canonicalize() {
                    Ok(p) => p,
                    Err(e) => {
                        eprintln!("Error: Invalid project path: {}", e);
                        std::process::exit(1);
                    }
                };

                println!("Starting Murshid watcher on {}...", project_root.display());

                // --- T1 vertical slice state (C2 session, C12 budget) ---
                let taxonomy = pack::load_taxonomy(&pack::default_pack_dir()).unwrap_or_default();
                let canon = pack::load_canon(&pack::default_pack_dir()).unwrap_or_default();

                // req 5: two-slot [models] config (screen cheap/fast, judge strong).
                let cfg = config::load_config();

                // req 1: the frequency knob (default `quiet`, I7 ship-chill)
                // sets the (budget, floor) pair for this run.
                let detent = noise::detent_for(&cfg.dial.frequency);
                println!(
                    "[murshid] frequency: {} (budget {} min, floor {:?})",
                    cfg.dial.frequency,
                    detent.refill_period.as_secs() / 60,
                    detent.floor
                );

                let now0 = std::time::SystemTime::now();
                let session_mgr =
                    std::sync::Arc::new(std::sync::Mutex::new(session::SessionManager::new(now0)));
                let snapshot = std::sync::Arc::new(std::sync::Mutex::new(
                    session::snapshot_session_start(&project_root).unwrap_or_default(),
                ));
                let bucket = std::sync::Arc::new(std::sync::Mutex::new(
                    budget::TokenBucket::for_detent(&detent, now0),
                ));
                let last_event_at = std::sync::Arc::new(std::sync::Mutex::new(now0));
                // req 4/req 6: files touched since they were last swept/judged.
                let pending_files: std::sync::Arc<
                    std::sync::Mutex<std::collections::HashSet<std::path::PathBuf>>,
                > = std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashSet::new()));
                // req 10: the single on-screen card awaiting a response.
                let pending_card: std::sync::Arc<std::sync::Mutex<Option<PendingCard>>> =
                    std::sync::Arc::new(std::sync::Mutex::new(None));
                // req 3/4: the single pull queue (in-memory; C2 — dies at
                // session end). `queue_seq` is the C7 "age" tie-break.
                let queue_state: std::sync::Arc<std::sync::Mutex<Vec<queue::QueueEntry>>> =
                    std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
                let queue_seq = std::sync::Arc::new(std::sync::Mutex::new(0u64));
                // req 10: per-category throttle state, computed fresh at
                // session start (C5: "never stored").
                let throttled_categories: std::sync::Arc<
                    std::sync::Mutex<std::collections::HashSet<String>>,
                > = std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashSet::new()));
                // Review fix / C6 "unchanged... never re-judged": per-file
                // signature of the session-diff hunks last dispatched to
                // stage-1 this session, so an unchanged re-sweep can skip
                // re-dispatching. Session-scoped: cleared on session split.
                let dispatched_hunk_signatures: std::sync::Arc<
                    std::sync::Mutex<std::collections::HashMap<std::path::PathBuf, String>>,
                > = std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()));

                {
                    let sid = session_mgr.lock().unwrap().session_id.clone();
                    if let Some(dp) = db::get_db_path() {
                        if let Ok(conn) = db::open_connection(&dp) {
                            let _ = db::log_event(
                                &conn,
                                &db::EventRecord {
                                    id: None,
                                    session_id: sid.clone(),
                                    kind: "session_start".to_string(),
                                    payload_json: "{}".to_string(),
                                    ts: None,
                                },
                            );
                            *throttled_categories.lock().unwrap() =
                                compute_throttle_state(&conn, &sid, &cfg.dial.unthrottle);
                        }
                    }
                }

                let keys = credentials::get_api_keys();
                let screen_provider = cfg.models.screen.provider.clone();
                let screen_model = cfg.models.screen.model.clone();
                let screen_key = resolve_slot_key(&screen_provider, &keys);
                let judge_provider = cfg.models.judge.provider.clone();
                let judge_model = cfg.models.judge.model.clone();
                let judge_key = resolve_slot_key(&judge_provider, &keys);

                // C6 degraded mode: no key for a non-Ollama slot -> observe-only.
                let mode = judge::determine_judge_mode(
                    &screen_provider,
                    screen_key.as_deref(),
                    &judge_provider,
                    judge_key.as_deref(),
                );
                if let judge::JudgeMode::Degraded { ref reason } = mode {
                    println!("{}", judge::degraded_status_line(reason));
                }

                // req 10: best-effort session-end cleanup on Ctrl+C — marks any
                // still-`shown` card `expired` and logs `session_end`, mirroring
                // credentials.rs's SIGHUP-reload pattern (spawn-a-thread handler).
                {
                    let session_mgr_for_shutdown = session_mgr.clone();
                    let db_path_for_shutdown = db::get_db_path();
                    // req 3/8: the queue and its snoozes die at session end
                    // (C2) — the T3 bookend's stand-in prints the unshown
                    // count (req 3) here, on process exit.
                    let queue_for_shutdown = queue_state.clone();
                    let cleanup: Box<dyn Fn() + Send> = Box::new(move || {
                        let sid = session_mgr_for_shutdown.lock().unwrap().session_id.clone();
                        if let Some(ref dp) = db_path_for_shutdown {
                            if let Ok(conn) = db::open_connection(dp) {
                                let expired = db::expire_unresolved_cards(&conn, &sid).unwrap_or(0);
                                let _ = db::log_event(
                                    &conn,
                                    &db::EventRecord {
                                        id: None,
                                        session_id: sid.clone(),
                                        kind: "session_end".to_string(),
                                        payload_json:
                                            serde_json::json!({ "expired_cards": expired })
                                                .to_string(),
                                        ts: None,
                                    },
                                );
                                let _ = db::purge_suppressions_for_session(&conn, &sid);
                            }
                        }
                        let queue_len = queue_for_shutdown.lock().unwrap().len();
                        if let Some(line) = queue::unshown_count_line(queue_len) {
                            println!("{}", line);
                        }
                    });
                    let _ = SHUTDOWN_CLEANUP.set(std::sync::Mutex::new(Some(cleanup)));
                    #[cfg(unix)]
                    setup_sigint_handler();
                }

                // req 3/8/10: non-blocking (relative to the watcher) stdin
                // reader — g/u/n resolve the single pending card; `m` browses
                // the pull queue; a number selects a queued item into the slot.
                {
                    let pending_card_for_stdin = pending_card.clone();
                    let queue_for_stdin = queue_state.clone();
                    std::thread::spawn(move || {
                        use std::io::BufRead;
                        let stdin = std::io::stdin();
                        for line in stdin.lock().lines().map_while(Result::ok) {
                            let trimmed = line.trim();

                            if trimmed.eq_ignore_ascii_case("m") {
                                let mut q = queue_for_stdin.lock().unwrap();
                                queue::sort_queue(&mut q);
                                if q.is_empty() {
                                    println!("  (queue is empty)");
                                } else {
                                    print!("{}", queue::render_queue_list(&q));
                                }
                                continue;
                            }

                            if let Ok(choice) = trimmed.parse::<usize>() {
                                if choice == 0 {
                                    continue;
                                }
                                if pending_card_for_stdin.lock().unwrap().is_some() {
                                    println!(
                                        "  finish the current card first (g/u/n), then pick again"
                                    );
                                    continue;
                                }
                                let mut q = queue_for_stdin.lock().unwrap();
                                queue::sort_queue(&mut q);
                                if choice > q.len() {
                                    println!("  no such item \u{2014} press m to see the list");
                                    continue;
                                }
                                let entry = q.remove(choice - 1);
                                drop(q);

                                let Some(dp) = db::get_db_path() else {
                                    continue;
                                };
                                let Ok(conn) = db::open_connection(&dp) else {
                                    continue;
                                };

                                // req 7 fix: re-check cooldown/suppression
                                // right before showing — a sibling entry for
                                // the same concept could have shipped (auto-
                                // push) or been snoozed to concept-scope
                                // since this one was queued. Without this, a
                                // user could pull two cards for one concept.
                                if pull_is_blocked(
                                    &conn,
                                    &entry.session_id,
                                    &entry.finding.concept_id,
                                    &entry.finding.advice_fp,
                                ) {
                                    let _ =
                                        db::update_card_status(&conn, entry.card_id, "collapsed");
                                    let _ = db::log_event(
                                        &conn,
                                        &db::EventRecord {
                                            id: None,
                                            session_id: entry.session_id.clone(),
                                            kind: "card_aggregated".to_string(),
                                            payload_json: serde_json::json!({
                                                "concept": entry.finding.concept_id,
                                                "collapsed_card_id": entry.card_id,
                                            })
                                            .to_string(),
                                            ts: None,
                                        },
                                    );
                                    println!(
                                        "  that one's already settled \u{2014} press m to see what's left"
                                    );
                                    continue;
                                }

                                // req 7 fix: this concept is shipping now —
                                // collapse any remaining siblings still in
                                // the queue into it.
                                let mut shown_card = entry.finding.card.clone();
                                let extra_anchors = collapse_queued_siblings(
                                    &conn,
                                    &queue_for_stdin,
                                    &entry.session_id,
                                    &entry.finding.concept_id,
                                );
                                if !extra_anchors.is_empty() {
                                    let _ = fold_anchors_into_card(&mut shown_card, extra_anchors);
                                }

                                let _ = db::update_card_status(&conn, entry.card_id, "shown");
                                let _ = db::log_event(
                                    &conn,
                                    &db::EventRecord {
                                        id: None,
                                        session_id: entry.session_id.clone(),
                                        kind: "card_shown".to_string(),
                                        payload_json: serde_json::json!({
                                            "concept": entry.finding.concept_id,
                                            "pulled_from_queue": true,
                                        })
                                        .to_string(),
                                        ts: None,
                                    },
                                );
                                println!("{}", card::render_card(&shown_card, 0));
                                *pending_card_for_stdin.lock().unwrap() = Some(PendingCard {
                                    card_id: entry.card_id,
                                    session_id: entry.session_id,
                                    concept_id: entry.finding.concept_id,
                                    concept_name: shown_card.concept_name,
                                    advice_fp: entry.finding.advice_fp,
                                });
                                continue;
                            }

                            let Some(verb) = response::response_verb_for_key(trimmed) else {
                                continue;
                            };
                            let maybe_pc = pending_card_for_stdin.lock().unwrap().take();
                            let Some(pc) = maybe_pc else { continue };
                            let Some(dp) = db::get_db_path() else {
                                continue;
                            };
                            let Ok(conn) = db::open_connection(&dp) else {
                                continue;
                            };
                            let _ = db::update_card_status(&conn, pc.card_id, verb);

                            // req 8 / D11(c): tiered snooze on `not_now`.
                            let mut widened = false;
                            if verb == "not_now" {
                                let prior = db::count_instance_snoozes_for_concept(
                                    &conn,
                                    &pc.session_id,
                                    &pc.concept_id,
                                )
                                .unwrap_or(0);
                                match suppression::tiered_snooze_scope(prior) {
                                    suppression::SnoozeScope::Instance => {
                                        let _ = db::insert_suppression(
                                            &conn,
                                            &pc.session_id,
                                            &pc.concept_id,
                                            &pc.advice_fp,
                                            "instance",
                                        );
                                    }
                                    suppression::SnoozeScope::Concept => {
                                        let _ = db::insert_suppression(
                                            &conn,
                                            &pc.session_id,
                                            &pc.concept_id,
                                            &pc.concept_id,
                                            "concept",
                                        );
                                        widened = true;
                                        println!(
                                            "  {}",
                                            suppression::widening_notice(&pc.concept_name)
                                        );
                                    }
                                }
                                let _ = db::enforce_suppression_cap(&conn, &pc.session_id);
                            }

                            let _ = db::log_event(
                                &conn,
                                &db::EventRecord {
                                    id: None,
                                    session_id: pc.session_id.clone(),
                                    kind: "card_response".to_string(),
                                    payload_json: serde_json::json!({
                                        "verb": verb,
                                        "concept": pc.concept_id,
                                        "widened": widened,
                                    })
                                    .to_string(),
                                    ts: None,
                                },
                            );
                        }
                    });
                }

                let project_root_cb = project_root.clone();

                let _watcher = match watcher::start_watching(project_root.clone(), move |path| {
                    println!("File saved: {}", path.display());
                    let db_path = db::get_db_path();
                    let conn_opt = db_path.as_ref().and_then(|dp| db::open_connection(dp).ok());
                    let project_root_str = project_root_cb.to_string_lossy().to_string();
                    let file_path_str = path.to_string_lossy().to_string();

                    if let Some(ref conn) = conn_opt {
                        let edit_event = db::HistoryEvent {
                            id: None,
                            event_type: "file_edit".to_string(),
                            project_root: project_root_str.clone(),
                            file_path: file_path_str.clone(),
                            success: None,
                            error_code: None,
                            error_message: None,
                            line_number: None,
                            created_at: None,
                        };
                        let _ = db::log_history_event(conn, &edit_event);
                    }

                    let now = std::time::SystemTime::now();
                    *last_event_at.lock().unwrap() = now;

                    // C2 session split on idle gap > 4h: expire the OLD session's
                    // unresolved cards (req 10 / C3 "no interaction by session end
                    // ⇒ expired") before rotating the snapshot to the new one.
                    let old_session_id = session_mgr.lock().unwrap().session_id.clone();
                    let split = session_mgr.lock().unwrap().on_file_event(now);
                    let session_id_now = session_mgr.lock().unwrap().session_id.clone();
                    if split {
                        if let Some(ref conn) = conn_opt {
                            let expired =
                                db::expire_unresolved_cards(conn, &old_session_id).unwrap_or(0);
                            let _ = db::log_event(
                                conn,
                                &db::EventRecord {
                                    id: None,
                                    session_id: old_session_id.clone(),
                                    kind: "session_end".to_string(),
                                    payload_json: serde_json::json!({ "expired_cards": expired })
                                        .to_string(),
                                    ts: None,
                                },
                            );
                            // req 3/8 / C2: the pull queue and all snoozes die
                            // at session end.
                            let _ = db::purge_suppressions_for_session(conn, &old_session_id);
                        }
                        let old_queue_len = queue_state.lock().unwrap().len();
                        if let Some(line) = queue::unshown_count_line(old_queue_len) {
                            println!("{}", line);
                        }
                        queue_state.lock().unwrap().clear();
                        dispatched_hunk_signatures.lock().unwrap().clear();
                        *pending_card.lock().unwrap() = None;
                        *snapshot.lock().unwrap() =
                            session::snapshot_session_start(&project_root_cb).unwrap_or_default();
                        if let Some(ref conn) = conn_opt {
                            let _ = db::log_event(
                                conn,
                                &db::EventRecord {
                                    id: None,
                                    session_id: session_id_now.clone(),
                                    kind: "session_start".to_string(),
                                    payload_json: "{}".to_string(),
                                    ts: None,
                                },
                            );
                            // req 10 / C5: throttle state is recomputed fresh
                            // at each session start, never carried over.
                            *throttled_categories.lock().unwrap() =
                                compute_throttle_state(conn, &session_id_now, &cfg.dial.unthrottle);
                        }
                    }

                    let rel_path = path
                        .strip_prefix(&project_root_cb)
                        .unwrap_or(path.as_path())
                        .to_path_buf();
                    pending_files.lock().unwrap().insert(rel_path.clone());

                    // D8/C12 quiescence gate: wait out the pause, then bail if a
                    // newer file event superseded this one (its own timer will
                    // handle the latest state).
                    std::thread::sleep(
                        quiescence::QUIESCENCE_PAUSE + std::time::Duration::from_millis(100),
                    );
                    if *last_event_at.lock().unwrap() != now {
                        return;
                    }

                    let content = match std::fs::read_to_string(&path) {
                        Ok(c) => c,
                        Err(_) => return,
                    };
                    if !site::parses_without_errors(&content) {
                        return; // parse errors: wait (D8)
                    }

                    // cargo check (D3 supporting signal / catch-up sweep trigger);
                    // diagnostics stay visible as plain lines in both modes (C6
                    // degraded-mode requirement).
                    let interceptor = compiler::CompilerInterceptor::new();
                    if let Ok(output) = interceptor.run_check(&project_root_cb, &path) {
                        if let Some(ref conn) = conn_opt {
                            let check_event = db::HistoryEvent {
                                id: None,
                                event_type: "compiler_check".to_string(),
                                project_root: project_root_str.clone(),
                                file_path: file_path_str.clone(),
                                success: Some(output.success),
                                error_code: None,
                                error_message: None,
                                line_number: None,
                                created_at: None,
                            };
                            let _ = db::log_history_event(conn, &check_event);
                        }
                        for diag in &output.diagnostics {
                            let code_str =
                                diag.code.clone().unwrap_or_else(|| "unknown".to_string());
                            let line_num = diag.spans.first().map(|s| s.line_start).unwrap_or(1);
                            println!(
                                "[ERROR {}] in {} at line {}",
                                code_str, file_path_str, line_num
                            );
                            println!("Message: {}", diag.message);
                        }
                    }

                    if let judge::JudgeMode::Degraded { .. } = &mode {
                        // Observe-only: events above are already recorded; no LLM call.
                        pending_files.lock().unwrap().clear();
                        return;
                    }

                    // req 8/req 4/req 6: dedup checked against the DB, computed
                    // fresh per advice-fp — never touches `bucket`/`session_mgr`/
                    // etc. from inside the dispatch closures below (mutex-
                    // poisoning safety: catch_unwind only ever wraps the pure
                    // provider call).
                    let already_judged = |fp: &str| -> bool {
                        conn_opt
                            .as_ref()
                            .map(|c| {
                                db::card_exists_with_advice_fp(c, &session_id_now, fp)
                                    .unwrap_or(false)
                            })
                            .unwrap_or(false)
                    };
                    let dispatch_stage1 = |prompt: &str| -> Result<String, String> {
                        judge::safe_dispatch(|| {
                            provider::dispatch_debounced_with_model(
                                &screen_provider,
                                Some(&screen_model),
                                prompt,
                                screen_key.as_deref(),
                            )
                        })
                        .map_err(|m| match m {
                            judge::JudgeMode::Degraded { reason } => reason,
                            judge::JudgeMode::Active => "degraded".to_string(),
                        })
                    };
                    let dispatch_stage2 = |prompt: &str| -> Result<String, String> {
                        judge::safe_dispatch(|| {
                            provider::dispatch_debounced_with_model(
                                &judge_provider,
                                Some(&judge_model),
                                prompt,
                                judge_key.as_deref(),
                            )
                        })
                        .map_err(|m| match m {
                            judge::JudgeMode::Degraded { reason } => reason,
                            judge::JudgeMode::Active => "degraded".to_string(),
                        })
                    };

                    // req 4/req 6 catch-up sweep: re-diff every file touched since
                    // it was last swept, not just the file that triggered this
                    // save. Unlike T1, every touched file is judged this pass
                    // (never stopped early) so req 6 can see every site the same
                    // concept was found at before deciding what's shown vs
                    // queued — "at most one card on screen" (T1 req 9) is
                    // enforced afterwards, over the aggregated results.
                    //
                    // Review fix / CD-1 BYOK-cost mandate: capped to at most
                    // MAX_STAGE1_DISPATCHES_PER_PASS candidate files per
                    // pass; anything past the cap is never removed from
                    // `pending_files` (same retain semantics as the T1 sweep
                    // fix), so it's simply picked up on the next pass.
                    let all_pending: Vec<std::path::PathBuf> =
                        pending_files.lock().unwrap().iter().cloned().collect();
                    let (files_to_sweep, _retained_for_next_pass) =
                        cap_dispatch_batch(all_pending, MAX_STAGE1_DISPATCHES_PER_PASS);
                    let mut findings: Vec<aggregate::SweepFinding> = Vec::new();

                    for rel in files_to_sweep {
                        let abs = project_root_cb.join(&rel);
                        let sweep_content = match std::fs::read_to_string(&abs) {
                            Ok(c) => c,
                            Err(_) => {
                                // Unreadable/deleted: nothing to sweep, ever.
                                pending_files.lock().unwrap().remove(&rel);
                                continue;
                            }
                        };
                        if !site::parses_without_errors(&sweep_content) {
                            continue; // still broken: stays pending for the next pass
                        }
                        // Actually sweeping this file now — only here does it
                        // leave the pending set.
                        pending_files.lock().unwrap().remove(&rel);

                        let snap = snapshot.lock().unwrap().clone();
                        let hunks =
                            match session::compute_session_diff(&project_root_cb, &rel, &snap) {
                                Ok(h) => h,
                                Err(_) => continue,
                            };
                        if hunks.is_empty() {
                            continue;
                        }

                        // Review fix / C6 "unchanged... never re-judged":
                        // this exact hunk set was already dispatched to
                        // stage-1 this session with nothing new to learn —
                        // skip re-burning the screen model on it.
                        let hunk_sig = diff::hunks_signature(&hunks);
                        let unchanged_since_last_dispatch = dispatched_hunk_signatures
                            .lock()
                            .unwrap()
                            .get(&rel)
                            .is_some_and(|prev| prev == &hunk_sig);
                        if unchanged_since_last_dispatch {
                            continue;
                        }
                        dispatched_hunk_signatures
                            .lock()
                            .unwrap()
                            .insert(rel.clone(), hunk_sig);

                        let rel_str = rel.to_string_lossy().to_string();
                        let outcome = pipeline::judge_hunks(
                            &rel_str,
                            &hunks,
                            &sweep_content,
                            &taxonomy,
                            &canon,
                            already_judged,
                            dispatch_stage1,
                            dispatch_stage2,
                        );

                        match outcome {
                            Ok(o) => {
                                if let Some(reason) = &o.drop_reason {
                                    if let Some(ref conn) = conn_opt {
                                        let payload = judge::judge_drop_payload(reason, &rel_str);
                                        let _ = db::log_event(
                                            conn,
                                            &db::EventRecord {
                                                id: None,
                                                session_id: session_id_now.clone(),
                                                kind: "judge_drop".to_string(),
                                                payload_json: payload.to_string(),
                                                ts: None,
                                            },
                                        );
                                    }
                                }

                                if let (Some(card), Some(stage2)) = (o.card, o.stage2) {
                                    if let Some(site) =
                                        site::compute_site(&rel_str, &sweep_content, card.line)
                                    {
                                        let advice_fp =
                                            site::advice_fingerprint(&stage2.concept, &site);

                                        // req 8: snoozed (instance or concept
                                        // scope) this session -> skip entirely.
                                        let suppressed = conn_opt
                                            .as_ref()
                                            .map(|c| {
                                                db::is_suppressed(
                                                    c,
                                                    &session_id_now,
                                                    &stage2.concept,
                                                    &advice_fp,
                                                )
                                                .unwrap_or(false)
                                            })
                                            .unwrap_or(false);
                                        // T1 req 8/C2 same-session dedup — a
                                        // `queued` row counts too (T2), so a
                                        // site already sitting in the queue is
                                        // never re-judged/re-added.
                                        let already_known = conn_opt
                                            .as_ref()
                                            .map(|c| {
                                                db::card_exists_with_advice_fp(
                                                    c,
                                                    &session_id_now,
                                                    &advice_fp,
                                                )
                                                .unwrap_or(false)
                                            })
                                            .unwrap_or(false);

                                        if !suppressed && !already_known {
                                            findings.push(aggregate::SweepFinding {
                                                concept_id: stage2.concept.clone(),
                                                category: stage2.category.clone(),
                                                advice_fp,
                                                file: rel_str.clone(),
                                                line: card.line,
                                                card,
                                                likely_bug: stage2.likely_bug,
                                                strict_mode_passed: o.strict_mode_passed,
                                            });
                                        }
                                    }
                                }
                            }
                            Err(e) => {
                                eprintln!("[WARNING] Judge pipeline error: {}", e);
                            }
                        }
                    }

                    // req 6: same-concept sites found in this sweep fold into
                    // one card each (up to 3 anchors).
                    let aggregated = aggregate::aggregate_by_concept(findings);
                    let mut shown_this_pass = false;

                    // req 3/10: persists a not-shown-this-pass finding as a
                    // `queued` card row and adds it to the in-memory pull
                    // queue (req 11: `card_queued` transition event).
                    let enqueue_finding =
                        |conn: &rusqlite::Connection,
                         agg: &aggregate::AggregatedFinding,
                         regresses_card_id: Option<i64>,
                         throttled_flag: bool| {
                            let Ok(card_id) = db::insert_card(
                                conn,
                                &db::CardRecord {
                                    id: None,
                                    session_id: session_id_now.clone(),
                                    concept_id: agg.concept_id.clone(),
                                    category: agg.category.clone(),
                                    rung_shown: "R2".to_string(),
                                    advice_fp: agg.advice_fp.clone(),
                                    finding_fp: None,
                                    status: "queued".to_string(),
                                    created_ts: None,
                                    resolved_ts: None,
                                    worked_diff: Some(agg.card.worked_diff.clone()),
                                    regresses_card_id,
                                },
                            ) else {
                                return;
                            };
                            let _ = db::log_event(
                                conn,
                                &db::EventRecord {
                                    id: None,
                                    session_id: session_id_now.clone(),
                                    kind: "card_queued".to_string(),
                                    payload_json: serde_json::json!({
                                        "concept": agg.concept_id,
                                        "category": agg.category,
                                        "site_count": agg.site_count,
                                        "throttled": throttled_flag,
                                    })
                                    .to_string(),
                                    ts: None,
                                },
                            );
                            let seq = {
                                let mut s = queue_seq.lock().unwrap();
                                let v = *s;
                                *s += 1;
                                v
                            };
                            queue_state.lock().unwrap().push(queue::QueueEntry {
                                finding: agg.clone(),
                                seq,
                                throttled: throttled_flag,
                                card_id,
                                session_id: session_id_now.clone(),
                            });
                        };

                    for mut agg in aggregated {
                        let Some(ref conn) = conn_opt else { continue };

                        // req 5/9: cross-session ledger dedup; a regression
                        // (misuse of previously applied/resolved advice) is
                        // the one exception, and re-opens a new card row
                        // referencing the old one.
                        let mut regresses_card_id: Option<i64> = None;
                        if let Ok(Some((old_id, status))) =
                            db::find_ledger_card(conn, &agg.advice_fp)
                        {
                            if db::is_regression_eligible(&status) {
                                regresses_card_id = Some(old_id);
                            } else {
                                continue; // permanently suppressed (req 5)
                            }
                        }

                        // req 7 / C8 concept cooldown: a concept that already
                        // shipped a card this session collapses further sites
                        // into that card's aggregation instead of queuing.
                        if db::concept_shown_this_session(conn, &session_id_now, &agg.concept_id)
                            .unwrap_or(false)
                        {
                            let _ = db::log_event(
                                conn,
                                &db::EventRecord {
                                    id: None,
                                    session_id: session_id_now.clone(),
                                    kind: "card_aggregated".to_string(),
                                    payload_json: serde_json::json!({
                                        "concept": agg.concept_id,
                                        "site_count": agg.site_count,
                                    })
                                    .to_string(),
                                    ts: None,
                                },
                            );
                            continue;
                        }

                        let throttled =
                            throttled_categories.lock().unwrap().contains(&agg.category);
                        let floor_excluded = noise::floor_excludes(&detent, &agg.category);

                        // Review fix: single-slot guard — never show a
                        // second card while an earlier one (this pass OR an
                        // earlier pass) is still awaiting a response. Checked
                        // fresh every iteration (not a one-time snapshot) so
                        // a concurrent pull via `m` is also respected.
                        let pending_card_present = pending_card.lock().unwrap().is_some();
                        let gate = noise::gate_sweep_finding(
                            shown_this_pass,
                            pending_card_present,
                            throttled,
                            floor_excluded,
                        );
                        if gate == noise::SweepAction::Enqueue {
                            // A strict-mode likely_bug candidate still only
                            // preempts the *budget*, never the slot — it
                            // queues like everything else blocked here (C7's
                            // category-rank ordering puts it at the head).
                            enqueue_finding(conn, &agg, regresses_card_id, throttled);
                            continue;
                        }

                        let candidate = budget::PushCandidate {
                            likely_bug: agg.likely_bug,
                            strict_mode_passed: agg.strict_mode_passed,
                        };
                        let decision = {
                            let mut b = bucket.lock().unwrap();
                            budget::decide_push(&mut b, &candidate, now)
                        };
                        match decision {
                            budget::PushDecision::Shown => {
                                // req 7 fix: this concept is shipping now —
                                // collapse any sibling queued entries for it
                                // into this card's aggregation.
                                let extra_anchors = collapse_queued_siblings(
                                    conn,
                                    &queue_state,
                                    &session_id_now,
                                    &agg.concept_id,
                                );
                                if !extra_anchors.is_empty() {
                                    let collapsed_count = extra_anchors.len();
                                    let overflow =
                                        fold_anchors_into_card(&mut agg.card, extra_anchors);
                                    agg.site_count += collapsed_count;
                                    agg.remaining_sites.extend(overflow);
                                }

                                println!("{}", card::render_card(&agg.card, 0));
                                if let Ok(card_id) = db::insert_card(
                                    conn,
                                    &db::CardRecord {
                                        id: None,
                                        session_id: session_id_now.clone(),
                                        concept_id: agg.concept_id.clone(),
                                        category: agg.category.clone(),
                                        rung_shown: "R2".to_string(),
                                        advice_fp: agg.advice_fp.clone(),
                                        finding_fp: None,
                                        status: "shown".to_string(),
                                        created_ts: None,
                                        resolved_ts: None,
                                        worked_diff: Some(agg.card.worked_diff.clone()),
                                        regresses_card_id,
                                    },
                                ) {
                                    let _ = db::log_event(
                                        conn,
                                        &db::EventRecord {
                                            id: None,
                                            session_id: session_id_now.clone(),
                                            kind: "card_shown".to_string(),
                                            payload_json: serde_json::json!({
                                                "concept": agg.concept_id,
                                                "site_count": agg.site_count,
                                                "remaining_sites": agg.remaining_sites,
                                                "regresses_card_id": regresses_card_id,
                                            })
                                            .to_string(),
                                            ts: None,
                                        },
                                    );
                                    *pending_card.lock().unwrap() = Some(PendingCard {
                                        card_id,
                                        session_id: session_id_now.clone(),
                                        concept_id: agg.concept_id.clone(),
                                        concept_name: agg.card.concept_name.clone(),
                                        advice_fp: agg.advice_fp.clone(),
                                    });
                                }
                                shown_this_pass = true;
                            }
                            budget::PushDecision::Queued => {
                                enqueue_finding(conn, &agg, regresses_card_id, false);
                            }
                        }
                    }

                    // req 3: the one-line presence indicator, printed once
                    // per sweep when anything is sitting in the queue.
                    let queue_len = queue_state.lock().unwrap().len();
                    if let Some(line) = queue::presence_indicator(queue_len) {
                        println!("{}", line);
                    }
                }) {
                    Ok(w) => w,
                    Err(e) => {
                        eprintln!("Failed to start watcher: {}", e);
                        std::process::exit(1);
                    }
                };

                // Keep watcher running
                loop {
                    std::thread::sleep(std::time::Duration::from_secs(60));
                }
            }
            _ => {
                print_usage();
                std::process::exit(1);
            }
        }
    } else {
        print_usage();
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- review fix: per-pass dispatch cap retains the tail ---

    #[test]
    fn test_cap_dispatch_batch_under_cap_is_a_no_op() {
        let items = vec![1, 2, 3];
        let (batch, retained) = cap_dispatch_batch(items, MAX_STAGE1_DISPATCHES_PER_PASS);
        assert_eq!(batch, vec![1, 2, 3]);
        assert!(retained.is_empty());
    }

    #[test]
    fn test_cap_dispatch_batch_over_cap_retains_tail() {
        let items = vec![1, 2, 3, 4, 5, 6, 7];
        let (batch, retained) = cap_dispatch_batch(items, MAX_STAGE1_DISPATCHES_PER_PASS);
        assert_eq!(
            batch,
            vec![1, 2, 3, 4],
            "only the cap's worth is dispatched this pass"
        );
        assert_eq!(
            retained,
            vec![5, 6, 7],
            "everything past the cap stays pending for the next pass, never dropped"
        );
    }

    #[test]
    fn test_cap_dispatch_batch_exactly_at_cap_is_a_no_op() {
        let items = vec![1, 2, 3, 4];
        let (batch, retained) = cap_dispatch_batch(items, MAX_STAGE1_DISPATCHES_PER_PASS);
        assert_eq!(batch.len(), 4);
        assert!(retained.is_empty());
    }

    // --- review fix: anchor folding on concept collapse ---

    fn sample_card() -> card::Card {
        card::Card {
            concept_name: "borrow-vs-clone".to_string(),
            file: "a.rs".to_string(),
            line: 1,
            grounding_quote: "q".to_string(),
            why: "why".to_string(),
            rule: "rule".to_string(),
            doc_ref: "ref".to_string(),
            worked_diff: "diff".to_string(),
            additional_anchors: Vec::new(),
            overflow_site_count: 0,
        }
    }

    #[test]
    fn test_fold_anchors_into_card_under_cap() {
        let mut card = sample_card();
        let overflow = fold_anchors_into_card(
            &mut card,
            vec![("b.rs".to_string(), 2), ("c.rs".to_string(), 3)],
        );
        assert!(overflow.is_empty());
        assert_eq!(
            card.additional_anchors,
            vec![("b.rs".to_string(), 2), ("c.rs".to_string(), 3)]
        );
        assert_eq!(card.overflow_site_count, 0);
    }

    #[test]
    fn test_fold_anchors_into_card_beyond_cap_overflows() {
        let mut card = sample_card();
        let overflow = fold_anchors_into_card(
            &mut card,
            vec![
                ("b.rs".to_string(), 2),
                ("c.rs".to_string(), 3),
                ("d.rs".to_string(), 4),
            ],
        );
        assert_eq!(
            card.additional_anchors.len(),
            2,
            "primary + 2 = 3 anchors max"
        );
        assert_eq!(card.overflow_site_count, 1);
        assert_eq!(overflow, vec![("d.rs".to_string(), 4)]);
    }

    // --- review fix (req 7): concept-collapse on ship ---

    fn sample_finding(concept_id: &str, file: &str, line: usize) -> aggregate::AggregatedFinding {
        let mut c = sample_card();
        c.concept_name = concept_id.to_string();
        c.file = file.to_string();
        c.line = line;
        aggregate::AggregatedFinding {
            concept_id: concept_id.to_string(),
            category: "idiom".to_string(),
            advice_fp: format!("{}-{}-{}", concept_id, file, line),
            card: c,
            likely_bug: false,
            strict_mode_passed: false,
            site_count: 1,
            remaining_sites: Vec::new(),
        }
    }

    fn make_queued_entry(
        conn: &rusqlite::Connection,
        session_id: &str,
        concept_id: &str,
        file: &str,
        line: usize,
    ) -> queue::QueueEntry {
        let finding = sample_finding(concept_id, file, line);
        let card_id = db::insert_card(
            conn,
            &db::CardRecord {
                id: None,
                session_id: session_id.to_string(),
                concept_id: concept_id.to_string(),
                category: finding.category.clone(),
                rung_shown: "R2".to_string(),
                advice_fp: finding.advice_fp.clone(),
                finding_fp: None,
                status: "queued".to_string(),
                created_ts: None,
                resolved_ts: None,
                worked_diff: None,
                regresses_card_id: None,
            },
        )
        .unwrap();
        queue::QueueEntry {
            finding,
            seq: card_id as u64,
            throttled: false,
            card_id,
            session_id: session_id.to_string(),
        }
    }

    #[test]
    fn test_collapse_queued_siblings_removes_marks_collapsed_and_returns_anchors() {
        let conn = db::initialize_db(":memory:").unwrap();
        let session_id = "sess1";

        let sib1 = make_queued_entry(&conn, session_id, "borrow-vs-clone", "b.rs", 2);
        let sib2 = make_queued_entry(&conn, session_id, "borrow-vs-clone", "c.rs", 3);
        let other = make_queued_entry(&conn, session_id, "string-vs-str", "d.rs", 4);

        let queue_mutex = std::sync::Mutex::new(vec![sib1.clone(), sib2.clone(), other.clone()]);

        let anchors = collapse_queued_siblings(&conn, &queue_mutex, session_id, "borrow-vs-clone");

        assert_eq!(anchors.len(), 2);
        assert!(anchors.contains(&("b.rs".to_string(), 2)));
        assert!(anchors.contains(&("c.rs".to_string(), 3)));

        // Siblings removed from the queue; the unrelated concept stays.
        let remaining = queue_mutex.lock().unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].finding.concept_id, "string-vs-str");
        drop(remaining);

        for id in [sib1.card_id, sib2.card_id] {
            let status: String = conn
                .query_row(
                    "SELECT status FROM cards WHERE id = ?1",
                    rusqlite::params![id],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(status, "collapsed");
        }
        let other_status: String = conn
            .query_row(
                "SELECT status FROM cards WHERE id = ?1",
                rusqlite::params![other.card_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(other_status, "queued", "unrelated concept untouched");

        let events = db::get_events_for_session(&conn, session_id).unwrap();
        let collapse_events = events
            .iter()
            .filter(|e| e.kind == "card_aggregated")
            .count();
        assert_eq!(
            collapse_events, 2,
            "one card_aggregated event per collapsed sibling"
        );
    }

    #[test]
    fn test_collapse_queued_siblings_empty_queue_is_a_no_op() {
        let conn = db::initialize_db(":memory:").unwrap();
        let queue_mutex = std::sync::Mutex::new(Vec::new());
        let anchors = collapse_queued_siblings(&conn, &queue_mutex, "sess1", "borrow-vs-clone");
        assert!(anchors.is_empty());
    }

    // --- review fix (req 7): pull-path cooldown rejection ---

    #[test]
    fn test_pull_is_blocked_when_concept_already_shipped() {
        let conn = db::initialize_db(":memory:").unwrap();
        db::insert_card(
            &conn,
            &db::CardRecord {
                id: None,
                session_id: "sess1".to_string(),
                concept_id: "borrow-vs-clone".to_string(),
                category: "idiom".to_string(),
                rung_shown: "R2".to_string(),
                advice_fp: "fp-shown".to_string(),
                finding_fp: None,
                status: "shown".to_string(),
                created_ts: None,
                resolved_ts: None,
                worked_diff: None,
                regresses_card_id: None,
            },
        )
        .unwrap();

        assert!(pull_is_blocked(
            &conn,
            "sess1",
            "borrow-vs-clone",
            "fp-other-site"
        ));
    }

    #[test]
    fn test_pull_is_blocked_when_concept_suppressed() {
        let conn = db::initialize_db(":memory:").unwrap();
        db::insert_suppression(
            &conn,
            "sess1",
            "borrow-vs-clone",
            "borrow-vs-clone",
            "concept",
        )
        .unwrap();
        assert!(pull_is_blocked(&conn, "sess1", "borrow-vs-clone", "fp-x"));
    }

    #[test]
    fn test_pull_is_not_blocked_when_clear() {
        let conn = db::initialize_db(":memory:").unwrap();
        assert!(!pull_is_blocked(&conn, "sess1", "borrow-vs-clone", "fp-x"));
    }
}
