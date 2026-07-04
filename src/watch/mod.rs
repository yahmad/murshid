//! The watch loop wiring: `watch::run` owns the terminal pane, loads config
//! and the language pack, builds the shared `WatchSession`, and spawns the
//! stdin, offer-poll, and file-event threads. The file-event path proper
//! lives in `sweep`, keystrokes in `keys`, the offer poll in `offers`.

pub mod keys;
pub mod offers;
pub mod sweep;

use crate::sync_ext::LockExt;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::{
    bookend, budget, card, credentials, db, goal, judge, ladder, memory, noise, offer, pack, queue,
    retrieval, session, site, struggle, throttle,
};

/// State pinned to the single card currently on screen (T1: at most one),
/// awaiting a `g`/`u`/`n` response (req 10). T2 req 8 extends this with the
/// fields the tiered-snooze `n` handler needs (advice_fp/concept_name). T4
/// extends it further with the ladder rung, category (for slot-contention
/// re-queue), full card content (for rung re-render + thread anchor), and
/// the STORED (enclosing_item, anchor_hash) site identity — req 1's
/// mechanical applied-detection re-check relocates by this identity rather
/// than trusting `card.line` (gating review fix: a line-pinned recompute
/// falsely reads "applied" whenever an edit ABOVE the site shifts it).
#[derive(Clone)]
pub struct PendingCard {
    pub card_id: i64,
    pub session_id: String,
    pub concept_id: String,
    pub concept_name: String,
    pub advice_fp: String,
    pub category: String,
    pub rung: ladder::Rung,
    pub card: card::Card,
    pub site_enclosing_item: Option<String>,
    pub site_anchor_hash: Option<String>,
}

/// T3 reqs 11-13: the single struggle offer awaiting a y/[anything-else]
/// response — mirrors `PendingCard`'s "at most one" shape, but per req 11
/// this never occupies the card slot; it's tracked separately.
#[derive(Clone)]
pub struct PendingOffer {
    pub key: (&'static str, String),
    pub site_file: std::path::PathBuf,
    pub fired_at: std::time::SystemTime,
    /// The `cards(category='struggle-offer')` row backing this offer for
    /// EFP/throttle accounting (req 12).
    pub card_id: i64,
}

/// T3 reqs 7-10: per-session struggle-signal state, gathered by the sweep
/// callback and read by the offer-poll thread.
#[derive(Default)]
pub struct StruggleTracking {
    pub error_streak: struggle::ErrorStreak,
    pub red_streak: struggle::RedStreak,
    /// req 8: the user's own 75th-pct time-to-green (C12 cold start when
    /// there's no history yet); recomputed per session start.
    pub baseline_ms: u128,
    /// req 11: an offer never fires while the last check was green.
    pub last_check_success: Option<bool>,
    /// The file behind the active red streak — accepting an inferred-pair
    /// offer runs the judge here (req 11).
    pub struggle_site: Option<std::path::PathBuf>,
    /// A still-live signal-3 candidate: (file, fresh help-flavored comment).
    pub help_candidate: Option<(std::path::PathBuf, String)>,
    /// req 13: (signal, key) pairs already offered this session — never
    /// re-fire regardless of outcome.
    pub already_offered: std::collections::HashSet<(&'static str, String)>,
}

/// T3 req 4: goal-drift tracking, session-scoped.
#[derive(Default)]
pub struct DriftTracking {
    pub fired: bool,
    /// (rel-path string, touch time) — pruned to the last
    /// [`goal::DRIFT_WINDOW`] on every check.
    pub touches: Vec<(String, std::time::SystemTime)>,
}

/// T2 req 1/10: the four taxonomy categories throttle/floor decisions key
/// on (C4/C8), plus T3 req 12's `struggle-offer` — offers share the same
/// D12 auto-throttle machinery (action rate < 15% over the last 20 counted
/// "cards" for that category, `cards` rows and all).
const CATEGORIES: [&str; 5] = [
    "bug",
    "idiom",
    "best-practice",
    "architecture",
    offer::OFFER_CATEGORY,
];

/// T2 req 10 / C5: computes each category's throttle state fresh from
/// `cards`/`events` history (never stored), applies the config-key undo
/// override (T2 req 10's "config key" undo path), and logs a
/// `throttle_change` event for every actual transition. Returns the set of
/// currently-throttled categories.
pub fn compute_throttle_state(
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

/// T2 review fix (req 7): when a card for `concept_id` ships — auto-push or
/// pull, either path calls this — removes every sibling entry for the same
/// concept still sitting in the queue, marks their `cards` rows `collapsed`
/// (additive status, same precedent as `queued`), logs one
/// `card_aggregated` event per collapsed entry, and returns their (file,
/// line) anchors — including any they'd already folded in themselves — so
/// the caller can fold them into the just-shipped card's aggregation via
/// [`fold_anchors_into_card`].
pub fn collapse_queued_siblings(
    conn: &rusqlite::Connection,
    queue_state: &std::sync::Mutex<Vec<queue::QueueEntry>>,
    session_id: &str,
    concept_id: &str,
) -> Vec<(String, usize)> {
    let siblings: Vec<queue::QueueEntry> = {
        let mut q = queue_state.lock_poison_safe();
        let (siblings, rest): (Vec<_>, Vec<_>) = q
            .drain(..)
            .partition(|e| e.finding.concept_id == concept_id);
        *q = rest;
        siblings
    };

    let mut anchors = Vec::new();
    db::warn_on_err(
        db::with_tx(conn, |tx| {
            for sib in &siblings {
                db::update_card_status_stmt(tx, sib.card_id, db::CardStatus::Collapsed)?;
                db::log_event_stmt(
                    tx,
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
                )?;
            }
            Ok(())
        }),
        "update_card_status+log_event(collapse_queued_siblings)",
    );
    for sib in siblings {
        anchors.push((sib.finding.card.file.clone(), sib.finding.card.line));
        anchors.extend(sib.finding.card.additional_anchors.iter().cloned());
    }
    anchors
}

/// T2 review fix (req 6/7): folds `extra` (file, line) anchors into `card`'s
/// aggregation, up to the 3-anchor cap (`aggregate::MAX_ANCHORS`); anything
/// beyond the cap tallies into `overflow_site_count` instead and is
/// returned so the caller can extend the event payload's `remaining_sites`.
pub fn fold_anchors_into_card(
    card: &mut card::Card,
    extra: Vec<(String, usize)>,
) -> Vec<(String, usize)> {
    let mut overflow = Vec::new();
    for anchor in extra {
        if card.additional_anchors.len() < crate::aggregate::MAX_ANCHORS - 1 {
            card.additional_anchors.push(anchor);
        } else {
            card.overflow_site_count += 1;
            overflow.push(anchor);
        }
    }
    overflow
}

/// T3 req 6: gathers the session-end bookend from the DB, the still-live
/// pull queue, and the current goal — shared by the SIGINT-cleanup and
/// C2-session-split "session end" moments.
#[allow(clippy::too_many_arguments)]
pub fn assemble_session_bookend(
    conn: &rusqlite::Connection,
    session_id: &str,
    project_root: &std::path::Path,
    taxonomy: &[pack::TaxonomyConcept],
    queue_state: &std::sync::Mutex<Vec<queue::QueueEntry>>,
    goal_cluster_dirs: &std::sync::Mutex<std::collections::HashSet<String>>,
    throttled_categories: &std::sync::Mutex<std::collections::HashSet<String>>,
) -> bookend::Bookend {
    let goal_text = crate::goal_text_now(project_root);
    let counts = bookend::BookendCounts {
        shown: db::bookend_shown_count(conn, session_id).unwrap_or(0),
        applied: db::bookend_applied_count(conn, session_id).unwrap_or(0),
        queued_unshown: db::bookend_queued_unshown_count(conn, session_id).unwrap_or(0),
    };
    let concept_slugs = db::concepts_taught_this_session(conn, session_id).unwrap_or_default();
    // req 6: "concepts taught (names only)" — map slug -> pack taxonomy name.
    let slug_to_name = |slug: &str| -> String {
        taxonomy
            .iter()
            .find(|c| c.slug == slug)
            .map(|c| c.name.clone())
            .unwrap_or_else(|| slug.to_string())
    };
    let concepts_taught: Vec<String> = concept_slugs.iter().map(|s| slug_to_name(s)).collect();
    // T4 req 7/10: unresolved threads / murshid-comments, names only (I21).
    let unresolved_threads: Vec<String> = db::unresolved_thread_concepts(conn, session_id)
        .unwrap_or_default()
        .iter()
        .map(|s| slug_to_name(s))
        .collect();
    let unresolved_comments: Vec<String> = db::unresolved_comment_ask_concepts(conn, session_id)
        .unwrap_or_default()
        .iter()
        .map(|s| slug_to_name(s))
        .collect();
    let mut throttled: Vec<String> = throttled_categories
        .lock_poison_safe()
        .iter()
        .cloned()
        .collect();
    throttled.sort();
    let queue_last_call: Vec<String> = {
        let mut q = queue_state.lock_poison_safe().clone();
        let cluster = goal_cluster_dirs.lock_poison_safe().clone();
        queue::sort_queue(&mut q, &cluster, &goal_text);
        q.iter()
            .take(3)
            .map(|e| {
                format!(
                    "{} \u{2014} {}:{}",
                    e.finding.card.concept_name, e.finding.card.file, e.finding.card.line
                )
            })
            .collect()
    };
    let goal_opt = if goal_text.trim().is_empty() {
        None
    } else {
        Some(goal_text.as_str())
    };
    bookend::assemble_bookend(
        goal_opt,
        counts,
        concepts_taught,
        throttled,
        queue_last_call,
        unresolved_threads,
        unresolved_comments,
    )
}

/// T3 req 6: "Bookend is an event" — folded into the existing `session_end`
/// event payload (an enumerated C5 kind) rather than inventing a new one.
pub fn bookend_event_payload(b: &bookend::Bookend, expired_cards: usize) -> serde_json::Value {
    serde_json::json!({
        "expired_cards": expired_cards,
        "goal_line": b.goal_line,
        "shown": b.counts.shown,
        "applied": b.counts.applied,
        "queued_unshown": b.counts.queued_unshown,
        "concepts_taught": b.concepts_taught,
        "throttled_categories": b.throttled_categories,
        "queue_last_call": b.queue_last_call,
    })
}

/// T5 req 4 / C4: resolves a concept's current entry rung from the memory
/// model (BKT band -> Wood shift -> knob offset, clamped) — replaces T4's
/// static `R2 + knob` default. Silence (mastered, `None`) and any DB error
/// both fall back to R2 (T4's old default) here: this helper backs paths
/// where an interaction is already committed to happening (a direct ask,
/// an accepted struggle offer, a queue pull) — the user engaged, so
/// SOMETHING renders. The one path that must honor silence as "no card at
/// all" is the sweep's auto-push decision, which calls
/// [`memory::entry_rung_for`] directly instead of this wrapper.
pub fn resolve_entry_rung(
    conn: &rusqlite::Connection,
    concept_id: &str,
    category: &str,
    directness: ladder::Directness,
) -> ladder::Rung {
    memory::entry_rung_for(conn, concept_id, category, directness)
        .ok()
        .flatten()
        .unwrap_or(ladder::Rung::R2)
}

/// T9 req 7: re-derives the STORED (enclosing_item, anchor_hash) site
/// identity for a card about to occupy the pending slot (T4 req 1) — the
/// queue-pull path (stdin thread) and the sweep-show path ran verbatim
/// copies of this read → compute_site → identity-pair chain.
pub fn derive_site_identity(
    project_root: &std::path::Path,
    rel_file: &str,
    line: usize,
    grammar: &pack::GrammarSpec,
) -> (Option<String>, Option<String>) {
    std::fs::read_to_string(project_root.join(rel_file))
        .ok()
        .and_then(|c| site::compute_site(rel_file, &c, line, grammar))
        .map(|s| (Some(s.enclosing_item), Some(s.anchor_hash)))
        .unwrap_or((None, None))
}

/// T9 req 7: goal (re-)resolution ceremony shared by session start and the
/// idle-gap session split (T3 reqs 1/3): resolve → refresh cluster dirs →
/// banner → log `goal_inferred` via the caller's `log_inferred` (each call
/// site sources its DB connection/session id differently, and session start
/// only opens a connection when something was actually inferred).
/// `announce_empty`: session start prints the "(none yet — g to set)" hint;
/// a mid-session split stays quiet when no goal resolves.
pub fn resolve_and_announce_goal(
    project_root: &std::path::Path,
    changed_files: &[std::path::PathBuf],
    goal_cluster_dirs: &std::sync::Mutex<std::collections::HashSet<String>>,
    announce_empty: bool,
    log_inferred: impl FnOnce(&str),
) {
    let branch = goal::current_branch(project_root);
    let commit_subjects = goal::recent_commit_subjects(project_root, 3);
    let (goal_text, was_inferred) = goal::resolve_session_goal(
        project_root,
        branch.as_deref(),
        &commit_subjects,
        changed_files,
    );
    *goal_cluster_dirs.lock_poison_safe() = goal::cluster_dirs_from_files(changed_files);
    match &goal_text {
        Some(t) => println!("[murshid] {}", goal::goal_banner(t)),
        None if announce_empty => println!("[murshid] goal: (none yet — g to set)"),
        None => {}
    }
    if was_inferred {
        if let Some(t) = goal_text.as_deref() {
            log_inferred(t);
        }
    }
}

/// T5 req 7 / C12: reads one line from stdin, or `None` if `timeout`
/// elapses first — req 7's "skippable by keypress or 30s timeout". The
/// reader thread may outlive the timeout (a CLI process reaps it on exit);
/// there is no other stdin reader running yet at session start.
fn read_line_with_timeout(timeout: std::time::Duration) -> Option<String> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        use std::io::BufRead;
        let mut line = String::new();
        if std::io::stdin().lock().read_line(&mut line).is_ok() {
            let _ = tx.send(line);
        }
    });
    rx.recv_timeout(timeout).ok()
}

/// T5 reqs 7-8 / D22, C12: fires at most [`retrieval::MAX_PER_SESSION`]
/// one-line recall questions for stale, eligible concepts. Wired ONLY at
/// the watcher's initial startup boundary, not the C2 idle-gap session
/// SPLIT (a background file-event callback) nor the bookend: both of those
/// moments either already have a dedicated stdin-reading thread running
/// (a blocking read here would race it for the user's next keystroke) or
/// are mid-teardown with no interactive turn left. Never during the work
/// session, pull-priced, one judge call per answered question — a
/// documented scoping choice, not a silent gap.
fn run_retrieval_questions(
    conn: &rusqlite::Connection,
    session_id: &str,
    taxonomy: &[pack::TaxonomyConcept],
    canon: &[pack::CanonEntry],
    judge_slot: &crate::ResolvedSlot,
) {
    let already_asked = db::retrieval_questions_asked_this_session(conn, session_id).unwrap_or(0);
    let cap_remaining = retrieval::MAX_PER_SESSION.saturating_sub(already_asked);
    if cap_remaining == 0 {
        return;
    }

    let memory_rows = db::list_concept_memory(conn).unwrap_or_default();
    let rows_with_category: Vec<(db::ConceptMemoryRow, String)> = memory_rows
        .into_iter()
        .filter_map(|row| {
            taxonomy
                .iter()
                .find(|c| c.slug == row.concept_id)
                .map(|c| (row, c.category.as_str().to_string()))
        })
        .collect();
    let now_epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    // T13 req 1 / T5 req 7: skip concepts naturally encountered last session.
    // Fails OPEN on DB error (empty set → gate off for this pass): worst case
    // is one unnecessary recall question, matching the degraded-gracefully
    // unwrap_or/unwrap_or_default reads surrounding it.
    let encountered_last_session =
        db::concepts_encountered_last_session(conn, session_id).unwrap_or_default();
    let candidates = retrieval::select_stale_concepts(
        &rows_with_category,
        now_epoch,
        cap_remaining,
        &encountered_last_session,
    );

    for candidate in candidates {
        let Some(entry) = pack::find_canon_for_concept(canon, &candidate.concept_id) else {
            continue;
        };
        let question = retrieval::build_recall_question(entry);
        println!("[murshid] {}", question);
        println!("  (30s to answer, or press enter to skip)");

        let Some(answer_line) = read_line_with_timeout(std::time::Duration::from_secs(30)) else {
            // Timeout: skip = no observation (I23), backoff only.
            let _ = memory::record_retrieval_skip(
                conn,
                session_id,
                &candidate.concept_id,
                &candidate.category,
            );
            println!("  (timed out \u{2014} skipped)");
            continue;
        };
        let answer = answer_line.trim().to_string();
        if answer.is_empty() {
            let _ = memory::record_retrieval_skip(
                conn,
                session_id,
                &candidate.concept_id,
                &candidate.category,
            );
            println!("  (skipped)");
            continue;
        }

        let prompt = retrieval::build_grading_prompt(&question, entry, &answer);
        // T11 req 2/4: recall grading is user-initiated — Interactive lane.
        let Ok(raw) = judge_slot.dispatch(crate::provider::Lane::Interactive, &prompt) else {
            continue;
        };
        let Ok(graded) = retrieval::parse_grading_response(&raw) else {
            continue;
        };
        if let Ok(enc) = memory::record_encounter(
            conn,
            session_id,
            &candidate.concept_id,
            &candidate.category,
            graded.grade,
            memory::EvidenceSource::Retrieval,
        ) {
            println!("  {}", graded.feedback);
            if enc.crossed_into_mastery {
                let name = taxonomy
                    .iter()
                    .find(|c| c.slug == candidate.concept_id)
                    .map(|c| c.name.clone())
                    .unwrap_or_else(|| candidate.concept_id.clone());
                println!(
                    "[murshid] backing off on {} \u{2014} applied {} times straight",
                    name, enc.row.pass_streak
                );
            }
            if enc.leveled_down {
                let name = taxonomy
                    .iter()
                    .find(|c| c.slug == candidate.concept_id)
                    .map(|c| c.name.clone())
                    .unwrap_or_else(|| candidate.concept_id.clone());
                println!("  {} needs another look \u{2014} cards are back", name);
            }
        }
    }
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

/// T9 req 3: the ONLY thing the signal handler may do is set this flag —
/// spawning threads / locking mutexes / I/O inside a handler is not
/// async-signal-safe (a signal landing mid-malloc can deadlock). The
/// keep-alive loop at the bottom of the watch arm polls it (~200ms) and
/// runs the session-end cleanup on a normal thread.
static SHUTDOWN_REQUESTED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

#[cfg(unix)]
extern "C" fn sigint_handler(_sig: libc::c_int) {
    SHUTDOWN_REQUESTED.store(true, std::sync::atomic::Ordering::SeqCst);
}

/// T12: the watch loop's shared mutable state — one `Arc<WatchSession>`
/// clone per thread (stdin reader, offer-poll, file-event sweep) replaces
/// the ~16 individually-cloned `Arc<Mutex<...>>` handles (`_for_stdin`/
/// `_for_poll`/`_cb` suffixes) main.rs used to carry. Every `Mutex` here is
/// the SAME one the pre-T12 code locked — only the access path changed
/// (`ws.field` instead of a per-closure clone variable); lock granularity
/// is unchanged (req 1: no coarsening).
pub struct WatchSession {
    pub session_mgr: Mutex<session::SessionManager>,
    pub snapshot: Mutex<session::SessionSnapshot>,
    pub bucket: Mutex<budget::TokenBucket>,
    pub last_event_at: Mutex<std::time::SystemTime>,
    /// req 4/req 6: files touched since they were last swept/judged.
    pub pending_files: Mutex<HashSet<PathBuf>>,
    /// req 10: the single on-screen card awaiting a response.
    pub pending_card: Mutex<Option<PendingCard>>,
    /// req 3/4: the single pull queue (in-memory; C2 — dies at session
    /// end). `queue_seq` is the C7 "age" tie-break.
    pub queue_state: Mutex<Vec<queue::QueueEntry>>,
    pub queue_seq: Mutex<u64>,
    /// req 10: per-category throttle state, computed fresh at session
    /// start (C5: "never stored").
    pub throttled_categories: Mutex<HashSet<String>>,
    /// Review fix / C6 "unchanged... never re-judged": per-file signature
    /// of the session-diff hunks last dispatched to stage-1 this session.
    pub dispatched_hunk_signatures: Mutex<HashMap<PathBuf, String>>,
    /// T3 req 5: the goal's file cluster, recomputed on every session
    /// split.
    pub goal_cluster_dirs: Mutex<HashSet<String>>,
    /// T3 req 4: drift tracking, session-scoped.
    pub drift_tracking: Mutex<DriftTracking>,
    /// T3 reqs 7-10: struggle-signal state, session-scoped.
    pub struggle_tracking: Mutex<StruggleTracking>,
    /// T3 reqs 11-13: the single struggle offer awaiting a response.
    pub pending_offer: Mutex<Option<PendingOffer>>,
    /// T4 req 8 / C6 BYOK consent: whether the session's first thread
    /// turn has already been confirmed under `ask`.
    pub thread_consent_confirmed: Mutex<bool>,
    /// T4 req 12 / D18: the last-seen HEAD commit hash.
    pub last_head_commit: Mutex<Option<String>>,
}

impl WatchSession {
    fn new(project_root: &Path, now0: std::time::SystemTime, detent: &noise::Detent) -> Self {
        WatchSession {
            session_mgr: Mutex::new(session::SessionManager::new(now0)),
            snapshot: Mutex::new(session::snapshot_session_start(project_root).unwrap_or_default()),
            bucket: Mutex::new(budget::TokenBucket::for_detent(detent, now0)),
            last_event_at: Mutex::new(now0),
            pending_files: Mutex::new(HashSet::new()),
            pending_card: Mutex::new(None),
            queue_state: Mutex::new(Vec::new()),
            queue_seq: Mutex::new(0u64),
            throttled_categories: Mutex::new(HashSet::new()),
            dispatched_hunk_signatures: Mutex::new(HashMap::new()),
            goal_cluster_dirs: Mutex::new(HashSet::new()),
            drift_tracking: Mutex::new(DriftTracking::default()),
            struggle_tracking: Mutex::new(StruggleTracking::default()),
            pending_offer: Mutex::new(None),
            thread_consent_confirmed: Mutex::new(false),
            last_head_commit: Mutex::new(session::current_head_commit(project_root)),
        }
    }
}

/// Command dispatch's `watch` arm, in full: resolves the project root,
/// loads config/pack, builds the shared [`WatchSession`], runs the
/// session-start ceremony (goal resolution, baseline, throttle, retrieval
/// questions, SIGINT-cleanup registration), spawns the stdin/offer-poll
/// threads and the file-event watcher (T12: closure BODIES live in
/// [`keys`], [`offers`], [`sweep`] — these are thin, `ws`-carrying calls),
/// then runs the keep-alive/shutdown-poll loop.
pub fn run(args: &[String]) {
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

    // req 5: two-slot [models] config (screen cheap/fast, judge strong).
    let cfg = crate::config::load_config();

    // --- T1 vertical slice state (C2 session, C12 budget) ---
    // T10 req 1/5: resolved dynamically (config override -> project
    // marker -> rust fallback) — everything below this point (watched
    // extensions, comment token, quiescence gating) already flowed from
    // the loaded pack; only the resolution of `pack_dir` itself used to
    // be hardcoded.
    let pack_dir = pack::resolve_pack_dir(&project_root, &cfg);
    let taxonomy = pack::load_or_notice(pack::load_taxonomy(&pack_dir), "taxonomy", &pack_dir);
    let canon = pack::load_or_notice(pack::load_canon(&pack_dir), "canon", &pack_dir);
    // T6: grammar reference (payload 6) + judge-prompt fragments
    // (payload 4) — engine loses all language-shaped literals.
    // T6 review defect 3: any payload fallback prints a
    // degraded-mode notice instead of silently assuming Rust.
    let grammar = pack::load_or_notice(pack::load_grammar(&pack_dir), "grammar", &pack_dir);
    let prompts =
        pack::load_or_notice(pack::load_prompt_fragments(&pack_dir), "prompts", &pack_dir);

    // req 1: the frequency knob (default `quiet`, I7 ship-chill)
    // sets the (budget, floor) pair for this run.
    let detent = noise::detent_for(&cfg.dial.frequency);
    println!(
        "[murshid] frequency: {} (budget {} min, floor {:?})",
        cfg.dial.frequency,
        detent.refill_period.as_secs() / 60,
        detent.floor
    );

    // T5 req 4 / C4: entry rung is now memory-driven per
    // concept (BKT band -> Wood shift -> knob offset); only the
    // knob setting itself is a fixed, session-wide value.
    let directness = ladder::directness_from_config(&cfg.dial.directness);
    println!("[murshid] directness: {}", cfg.dial.directness);

    let now0 = std::time::SystemTime::now();
    // T3 req 10: pack-seeded help-comment pattern lists. Falls
    // back to the bundled pack's own defaults (T6: the fallback
    // literal lives in `pack::SurfaceConfig::default`, the pack
    // loader — never inline in this engine module) and prints a
    // degraded-mode notice when it does (review defect 3).
    let surface = pack::load_or_notice(pack::load_surface(&pack_dir), "surface", &pack_dir);

    let ws = Arc::new(WatchSession::new(&project_root, now0, &detent));

    // T3 req 1: infer the goal (branch -> commits -> file
    // cluster), never overwriting an explicit hand-edit (req 3),
    // and render the banner. NEVER prompts for input (req 1).
    {
        let changed_files: Vec<std::path::PathBuf> = ws
            .snapshot
            .lock_poison_safe()
            .files
            .keys()
            .cloned()
            .collect();
        resolve_and_announce_goal(
            &project_root,
            &changed_files,
            &ws.goal_cluster_dirs,
            true,
            |t| {
                if let Some(dp) = db::get_db_path() {
                    if let Ok(conn) = db::open_connection(&dp) {
                        let sid = ws.session_mgr.lock_poison_safe().session_id.clone();
                        let _ = db::log_event(
                            &conn,
                            &db::EventRecord {
                                id: None,
                                session_id: sid,
                                kind: "goal_inferred".to_string(),
                                payload_json: serde_json::json!({ "text": t }).to_string(),
                                ts: None,
                            },
                        );
                    }
                }
            },
        );
    }

    {
        let sid = ws.session_mgr.lock_poison_safe().session_id.clone();
        if let Some(dp) = db::get_db_path() {
            if let Ok(conn) = db::open_connection(&dp) {
                // ROADMAP item 3: rolling-window retention — prune
                // events/context_history older than the widest read window
                // once per session, reclaiming space. Runs before the baseline
                // recompute so a wedged toolchain's old rows are already gone.
                match db::prune_expired_history(&conn) {
                    Ok(pruned) if pruned > 0 => println!(
                        "[murshid] pruned {} history row(s) older than the {}-day retention window.",
                        pruned,
                        db::RETENTION_PRUNE_FLOOR_DAYS
                    ),
                    _ => {}
                }

                // T3 req 8: recompute the user's own baseline
                // fresh at every session start (C12).
                let points = db::all_check_result_points(&conn).unwrap_or_default();
                let durations = struggle::time_to_green_durations_ms(&points);
                ws.struggle_tracking.lock_poison_safe().baseline_ms =
                    struggle::percentile_75_ms(&durations);

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
                *ws.throttled_categories.lock_poison_safe() =
                    compute_throttle_state(&conn, &sid, &cfg.dial.unthrottle);
            }
        }
    }

    let keys = credentials::get_api_keys();
    let models = crate::Models::resolve(&cfg.models, &keys);
    for warning in models.config_warnings() {
        eprintln!("[WARNING] model slot misconfigured: {warning}");
    }

    // C6 degraded mode: no usable key for a non-Ollama slot -> observe-only.
    let mode = judge::determine_judge_mode(
        &models.screen.provider,
        judge::KeyStatus::resolve(models.screen.key.as_deref(), models.screen.key_unreadable),
        &models.judge.provider,
        judge::KeyStatus::resolve(models.judge.key.as_deref(), models.judge.key_unreadable),
    );
    if let judge::JudgeMode::Degraded { ref reason } = mode {
        println!("{}", judge::degraded_status_line(reason));
    } else if let Some(dp) = db::get_db_path() {
        // T5 reqs 7-8 / D22: session-start recall questions —
        // never during the work session, never in degraded mode
        // (grading needs a live judge call).
        if let Ok(conn) = db::open_connection(&dp) {
            let sid = ws.session_mgr.lock_poison_safe().session_id.clone();
            run_retrieval_questions(&conn, &sid, &taxonomy, &canon, &models.judge);
        }
    }

    // req 10: best-effort session-end cleanup on Ctrl+C — marks any
    // still-`shown` card `expired`, logs `session_end` (T3 req 6:
    // carrying the bookend), and renders the bookend screen. It
    // runs on the keep-alive thread when SHUTDOWN_REQUESTED is
    // set (T9 req 3) — never inside the signal handler itself.
    {
        let ws_for_shutdown = ws.clone();
        let db_path_for_shutdown = db::get_db_path();
        let project_root_for_shutdown = project_root.clone();
        let taxonomy_for_shutdown = taxonomy.clone();
        let cleanup: Box<dyn Fn() + Send> = Box::new(move || {
            let sid = ws_for_shutdown
                .session_mgr
                .lock_poison_safe()
                .session_id
                .clone();
            if let Some(ref dp) = db_path_for_shutdown {
                if let Ok(conn) = db::open_connection(dp) {
                    let expired = db::expire_unresolved_cards(&conn, &sid).unwrap_or(0);
                    // req 3/8 / C2: the pull queue and (non-
                    // offer-concept) snoozes die at session end.
                    db::warn_on_err(
                        db::purge_suppressions_for_session(&conn, &sid),
                        "purge_suppressions_for_session",
                    );
                    let b = assemble_session_bookend(
                        &conn,
                        &sid,
                        &project_root_for_shutdown,
                        &taxonomy_for_shutdown,
                        &ws_for_shutdown.queue_state,
                        &ws_for_shutdown.goal_cluster_dirs,
                        &ws_for_shutdown.throttled_categories,
                    );
                    let _ = db::log_event(
                        &conn,
                        &db::EventRecord {
                            id: None,
                            session_id: sid.clone(),
                            kind: "session_end".to_string(),
                            payload_json: bookend_event_payload(&b, expired).to_string(),
                            ts: None,
                        },
                    );
                    // T9 req 2: snapshot progress at the session
                    // bookend, not just after migrations.
                    if let Err(e) = db::save_backup_from_db(&conn) {
                        eprintln!(
                            "Warning: failed to save progress backup at session end: {}",
                            e
                        );
                    }
                    println!("{}", bookend::render_bookend(&b));
                }
            }
        });
        let _ = SHUTDOWN_CLEANUP.set(std::sync::Mutex::new(Some(cleanup)));
        #[cfg(unix)]
        setup_sigint_handler();
    }

    // req 3/8/10/11: non-blocking (relative to the watcher) stdin
    // reader — g/u/n resolve the single pending card; `m` browses
    // the pull queue; a number selects a queued item into the
    // slot; `g` with no pending card opens $EDITOR on the goal
    // file (req 2); y/[anything else] resolves a pending
    // struggle offer (req 11).
    {
        let ws_for_stdin = ws.clone();
        let project_root_for_stdin = project_root.clone();
        let taxonomy_for_stdin = taxonomy.clone();
        let canon_for_stdin = canon.clone();
        let grammar_for_stdin = grammar.clone();
        let prompts_for_stdin = prompts.clone();
        let models_for_stdin = models.clone();
        let directness_for_stdin = directness;
        let surface_for_stdin = surface.clone();
        let consent_setting_for_stdin = cfg.consent.solicited_spend.clone();
        std::thread::spawn(move || {
            keys::run_stdin_loop(
                &ws_for_stdin,
                &project_root_for_stdin,
                &taxonomy_for_stdin,
                &canon_for_stdin,
                &grammar_for_stdin,
                &prompts_for_stdin,
                &models_for_stdin,
                directness_for_stdin,
                &surface_for_stdin,
                &consent_setting_for_stdin,
            );
        });
    }

    // T3 reqs 9/11-13: the struggle-offer poll — evaluates
    // idle-gating and convergence on a timer (idle can only be
    // known to have elapsed by *not* seeing a file event, so
    // this can't be driven from the file-event callback alone),
    // fires at most one offer at a time, and expires it
    // silently if the user goes back to typing (I10).
    {
        let ws_for_poll = ws.clone();
        std::thread::spawn(move || {
            offers::run_poll_loop(&ws_for_poll);
        });
    }

    let project_root_for_sweep = project_root.clone();
    let watched_extensions = surface.file_extensions.clone();
    let unthrottle_for_sweep = cfg.dial.unthrottle.clone();

    // Debounce inversion: `wake_tx`/`wake_rx` is the wake+debounce channel
    // between the (thin) notify/polling producer below and the dedicated
    // quiescence worker — a unit `send` wakes the worker and resets its
    // debounce clock; the worker treats a `recv_timeout` timeout (no new
    // wake within `QUIESCENCE_PAUSE`) as "quiescent, sweep now".
    let (wake_tx, wake_rx) = std::sync::mpsc::channel::<()>();

    // The quiescence worker: owns the debounce wait AND one long-lived DB
    // connection for the life of the thread (opened once inside
    // `run_quiescence_worker`, never per event). The notify/polling
    // callback passed to `start_watching` below is a thin producer only —
    // it never calls into the sweep directly and never blocks the watcher
    // thread.
    {
        let ws_for_worker = ws.clone();
        let project_root_for_worker = project_root.clone();
        std::thread::spawn(move || {
            sweep::run_quiescence_worker(
                ws_for_worker,
                wake_rx,
                project_root_for_worker,
                pack_dir,
                taxonomy,
                canon,
                grammar,
                prompts,
                surface,
                detent,
                models,
                directness,
                mode,
                unthrottle_for_sweep,
            );
        });
    }

    let _watcher = match crate::watcher::start_watching(
        project_root.clone(),
        &watched_extensions,
        move |path| {
            // Thin producer (T-debounce-inversion): record the touch,
            // stamp `last_event_at`, wake the worker, return immediately —
            // no sleep, no DB I/O, no LLM dispatch on the watcher thread.
            let rel_path = path
                .strip_prefix(&project_root_for_sweep)
                .unwrap_or(path.as_path())
                .to_path_buf();
            ws.pending_files.lock_poison_safe().insert(rel_path);
            *ws.last_event_at.lock_poison_safe() = std::time::SystemTime::now();
            let _ = wake_tx.send(());
        },
    ) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("Failed to start watcher: {}", e);
            std::process::exit(1);
        }
    };

    // T9 req 3: keep-alive doubles as the shutdown poller — the
    // SIGINT handler only sets SHUTDOWN_REQUESTED (async-signal-
    // safe); this ordinary thread notices within ~200ms and runs
    // the session-end cleanup here, where locking and I/O are
    // legal, then exits.
    loop {
        std::thread::sleep(std::time::Duration::from_millis(200));
        if SHUTDOWN_REQUESTED.load(std::sync::atomic::Ordering::SeqCst) {
            if let Some(mutex) = SHUTDOWN_CLEANUP.get() {
                let guard = mutex.lock_poison_safe();
                if let Some(ref cleanup) = *guard {
                    cleanup();
                }
            }
            std::process::exit(0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn sample_finding(
        concept_id: &str,
        file: &str,
        line: usize,
    ) -> crate::aggregate::AggregatedFinding {
        let mut c = sample_card();
        c.concept_name = concept_id.to_string();
        c.file = file.to_string();
        c.line = line;
        crate::aggregate::AggregatedFinding {
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
                site_file: None,
                site_line: None,
            },
        )
        .unwrap();
        queue::QueueEntry {
            finding,
            seq: card_id as u64,
            throttled: false,
            card_id,
            session_id: session_id.to_string(),
            pinned_head: false,
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
}
