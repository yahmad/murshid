pub mod aggregate;
pub mod backup;
pub mod bkt;
pub mod bookend;
pub mod budget;
pub mod card;
pub mod comment;
pub mod config;
pub mod consent;
pub mod credentials;
pub mod db;
pub mod diff;
pub mod goal;
pub mod judge;
pub mod ladder;
pub mod memory;
pub mod noise;
pub mod offer;
pub mod pack;
pub mod pipeline;
pub mod progress;
pub mod provider;
pub mod queue;
pub mod quiescence;
pub mod response;
pub mod retrieval;
pub mod review;
pub mod sanitizer;
pub mod session;
pub mod sha256;
pub mod site;
pub mod staleness;
pub mod struggle;
pub mod suppression;
pub mod thread;
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
        "  goal [text]                            Print the current goal, or set it (bare = print, D13(c))"
    );
    println!(
        "  review [path]                          Solicited review (D18): batched screen->judge digest of the session diff"
    );
    println!(
        "  progress                               The open per-concept skill meter (I24): mastery bar, help level, staleness"
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
/// fields the tiered-snooze `n` handler needs (advice_fp/concept_name). T4
/// extends it further with the ladder rung, category (for slot-contention
/// re-queue), full card content (for rung re-render + thread anchor), and
/// the STORED (enclosing_item, anchor_hash) site identity — req 1's
/// mechanical applied-detection re-check relocates by this identity rather
/// than trusting `card.line` (gating review fix: a line-pinned recompute
/// falsely reads "applied" whenever an edit ABOVE the site shifts it).
#[derive(Clone)]
struct PendingCard {
    card_id: i64,
    session_id: String,
    concept_id: String,
    concept_name: String,
    advice_fp: String,
    category: String,
    rung: ladder::Rung,
    card: card::Card,
    site_enclosing_item: Option<String>,
    site_anchor_hash: Option<String>,
}

/// T3 reqs 11-13: the single struggle offer awaiting a y/[anything-else]
/// response — mirrors `PendingCard`'s "at most one" shape, but per req 11
/// this never occupies the card slot; it's tracked separately.
#[derive(Clone)]
struct PendingOffer {
    key: (&'static str, String),
    site_file: std::path::PathBuf,
    fired_at: std::time::SystemTime,
    /// The `cards(category='struggle-offer')` row backing this offer for
    /// EFP/throttle accounting (req 12).
    card_id: i64,
}

/// T3 reqs 7-10: per-session struggle-signal state, gathered by the watcher
/// closure and read by the offer-poll thread.
#[derive(Default)]
struct StruggleTracking {
    error_streak: struggle::ErrorStreak,
    red_streak: struggle::RedStreak,
    /// req 8: the user's own 75th-pct time-to-green (C12 cold start when
    /// there's no history yet); recomputed per session start.
    baseline_ms: u128,
    /// req 11: an offer never fires while the last check was green.
    last_check_success: Option<bool>,
    /// The file behind the active red streak — accepting an inferred-pair
    /// offer runs the judge here (req 11).
    struggle_site: Option<std::path::PathBuf>,
    /// A still-live signal-3 candidate: (file, fresh help-flavored comment).
    help_candidate: Option<(std::path::PathBuf, String)>,
    /// req 13: (signal, key) pairs already offered this session — never
    /// re-fire regardless of outcome.
    already_offered: std::collections::HashSet<(&'static str, String)>,
}

/// T3 req 4: goal-drift tracking, session-scoped.
#[derive(Default)]
struct DriftTracking {
    fired: bool,
    /// (rel-path string, touch time) — pruned to the last
    /// [`goal::DRIFT_WINDOW`] on every check.
    touches: Vec<(String, std::time::SystemTime)>,
}

/// req 4: reads the current goal text fresh from disk (it can change
/// mid-session via `g`/hand-edit); empty when no goal is set yet.
fn goal_text_now(project_root: &std::path::Path) -> String {
    goal::read_goal_file(project_root)
        .map(|g| g.text)
        .unwrap_or_default()
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
        let mut q = queue_state.lock().unwrap_or_else(|e| e.into_inner());
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

/// T5 req 3(b)/10 fix: whether a stage-2-validated finding should PUSH/
/// QUEUE a card. Deliberately separate from evidence recording (`fail`
/// evidence is recorded unconditionally, per I23 — mastery state and
/// noise-control state are independent axes) — this function is only ever
/// consulted for the card-presentation decision, never for whether the
/// misuse itself gets recorded in the memory model.
fn should_push_misuse_finding(suppressed: bool, already_known: bool, silenced: bool) -> bool {
    !suppressed && !already_known && !silenced
}

/// T3 req 6: gathers the session-end bookend from the DB, the still-live
/// pull queue, and the current goal — shared by the SIGINT-cleanup and
/// C2-session-split "session end" moments.
#[allow(clippy::too_many_arguments)]
fn assemble_session_bookend(
    conn: &rusqlite::Connection,
    session_id: &str,
    project_root: &std::path::Path,
    taxonomy: &[pack::TaxonomyConcept],
    queue_state: &std::sync::Mutex<Vec<queue::QueueEntry>>,
    goal_cluster_dirs: &std::sync::Mutex<std::collections::HashSet<String>>,
    throttled_categories: &std::sync::Mutex<std::collections::HashSet<String>>,
) -> bookend::Bookend {
    let goal_text = goal_text_now(project_root);
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
    let mut throttled: Vec<String> = throttled_categories.lock().unwrap_or_else(|e| e.into_inner()).iter().cloned().collect();
    throttled.sort();
    let queue_last_call: Vec<String> = {
        let mut q = queue_state.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let cluster = goal_cluster_dirs.lock().unwrap_or_else(|e| e.into_inner()).clone();
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
fn bookend_event_payload(b: &bookend::Bookend, expired_cards: usize) -> serde_json::Value {
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

/// T5 review fix 3: atomically consumes `slot` only if it currently holds a
/// card whose id is `expected_card_id` — the compare-and-clear primitive
/// the mechanical applied-detection path uses to avoid a race with a
/// manual `a` keystroke resolving the SAME card concurrently (the site
/// recheck does file I/O — a real, if narrow, window). Whichever side wins
/// this call is the only one that goes on to record evidence; the loser
/// gets `false` and does nothing further — never a double count, never a
/// clobber of whatever the winner already did.
fn take_pending_card_if_matches(
    slot: &std::sync::Mutex<Option<PendingCard>>,
    expected_card_id: i64,
) -> bool {
    let mut guard = slot.lock().unwrap_or_else(|e| e.into_inner());
    let matches = guard.as_ref().map(|p| p.card_id) == Some(expected_card_id);
    if matches {
        *guard = None;
    }
    matches
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
fn resolve_entry_rung(
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

/// T3 req 11: "`y` runs the judge on the struggle site and shows the card
/// through the normal slot." A reduced, single-file replay of the watcher's
/// sweep-and-show path, invoked only on an accepted struggle offer. Ledger/
/// cooldown/suppression gates are deliberately not re-applied here — the
/// user just explicitly asked for this exact site, which is the same
/// "asking trumps prior state" logic D17 uses for direct asks.
#[allow(clippy::too_many_arguments)]
fn run_struggle_judge_and_show(
    conn: &rusqlite::Connection,
    session_id: &str,
    project_root: &std::path::Path,
    site_file: &std::path::Path,
    snapshot: &session::SessionSnapshot,
    taxonomy: &[pack::TaxonomyConcept],
    canon: &[pack::CanonEntry],
    grammar: &pack::GrammarSpec,
    prompts: &pack::PromptFragments,
    screen_provider: &str,
    screen_model: &str,
    screen_key: Option<&str>,
    screen_base_url: Option<&str>,
    judge_provider: &str,
    judge_model: &str,
    judge_key: Option<&str>,
    judge_base_url: Option<&str>,
    bucket: &std::sync::Mutex<budget::TokenBucket>,
    directness: ladder::Directness,
    comment_token: &str,
) -> Option<PendingCard> {
    let hunks = session::compute_session_diff(project_root, site_file, snapshot).ok()?;
    if hunks.is_empty() {
        return None;
    }
    let rel_str = site_file.to_string_lossy().to_string();
    let abs = project_root.join(site_file);
    let content = std::fs::read_to_string(&abs).ok()?;

    let already_judged =
        |fp: &str| -> bool { db::card_exists_with_advice_fp(conn, session_id, fp).unwrap_or(false) };
    let dispatch_stage1 = |prompt: &str| -> Result<String, String> {
        judge::safe_dispatch(|| {
            provider::dispatch_debounced_with_model(
                screen_provider,
                Some(screen_model),
                prompt,
                screen_key,
                screen_base_url,
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
                judge_provider,
                Some(judge_model),
                prompt,
                judge_key,
                judge_base_url,
            )
        })
        .map_err(|m| match m {
            judge::JudgeMode::Degraded { reason } => reason,
            judge::JudgeMode::Active => "degraded".to_string(),
        })
    };

    let outcome = pipeline::judge_hunks(
        &rel_str,
        &hunks,
        &content,
        taxonomy,
        canon,
        grammar,
        prompts,
        already_judged,
        dispatch_stage1,
        dispatch_stage2,
    )
    .ok()?;

    let card = outcome.card?;
    let stage2 = outcome.stage2?;
    let site = site::compute_site(&rel_str, &content, card.line, grammar)?;
    let advice_fp = site::advice_fingerprint(&stage2.concept, &site);
    // T5 req 4: memory-driven entry rung, resolved for THIS concept now
    // that stage-2 has named it.
    let entry_rung = resolve_entry_rung(conn, &stage2.concept, &stage2.category, directness);

    // C7/D16 mitigation: an accepted offer always shows now, preempting the
    // queue; consumes a token if available, else borrows exactly one.
    {
        let mut b = bucket.lock().unwrap_or_else(|e| e.into_inner());
        budget::consume_or_borrow(&mut b, std::time::SystemTime::now());
    }

    let card_id = db::insert_card(
        conn,
        &db::CardRecord {
            id: None,
            session_id: session_id.to_string(),
            concept_id: stage2.concept.clone(),
            category: stage2.category.clone(),
            rung_shown: entry_rung.as_str().to_string(),
            advice_fp: advice_fp.clone(),
            finding_fp: None,
            status: "shown".to_string(),
            created_ts: None,
            resolved_ts: None,
            worked_diff: Some(card.worked_diff.clone()),
            regresses_card_id: None,
            site_file: Some(rel_str.clone()),
            site_line: Some(card.line as i64),
        },
    )
    .ok()?;
    let _ = db::log_event(
        conn,
        &db::EventRecord {
            id: None,
            session_id: session_id.to_string(),
            kind: "card_shown".to_string(),
            payload_json: serde_json::json!({
                "concept": stage2.concept,
                "from_struggle_offer": true,
            })
            .to_string(),
            ts: None,
        },
    );
    println!(
        "{}",
        card::render_card_at_rung(&card, entry_rung, 0, comment_token)
    );

    Some(PendingCard {
        card_id,
        session_id: session_id.to_string(),
        concept_id: stage2.concept.clone(),
        concept_name: card.concept_name.clone(),
        advice_fp,
        category: stage2.category.clone(),
        rung: entry_rung,
        card,
        site_enclosing_item: Some(site.enclosing_item),
        site_anchor_hash: Some(site.anchor_hash),
    })
}

/// T4 reqs 5-8 / D20: runs one thread turn on `pc` — persists the user
/// question and the judge's answer (mutation order: dispatch first, DB
/// writes only after a real answer comes back), and returns the rendered,
/// rung-respecting answer text. Pull-priced: no budget interaction.
#[allow(clippy::too_many_arguments)]
fn run_thread_turn(
    conn: &rusqlite::Connection,
    session_id: &str,
    pc: &PendingCard,
    question: &str,
    canon: &[pack::CanonEntry],
    judge_provider: &str,
    judge_model: &str,
    judge_key: Option<&str>,
    judge_base_url: Option<&str>,
) -> Option<String> {
    let turn_no = db::thread_user_turn_count(conn, pc.card_id).unwrap_or(0) as i64 + 1;

    let history: Vec<thread::ThreadTurn> = db::get_thread_messages(conn, pc.card_id)
        .unwrap_or_default()
        .into_iter()
        .map(|m| thread::ThreadTurn {
            role: m.role,
            content: m.content,
        })
        .collect();

    let canon_entry = pack::find_canon_for_concept(canon, &pc.concept_id);
    let prompt = thread::build_thread_prompt(
        &pc.card.file,
        pc.card.line,
        &pc.card.grounding_quote,
        &pc.card.concept_name,
        canon_entry,
        &history,
        question,
    );

    let raw = judge::safe_dispatch(|| {
        provider::dispatch_debounced_with_model(
            judge_provider,
            Some(judge_model),
            &prompt,
            judge_key,
            judge_base_url,
        )
    })
    .ok()?;
    let answer = thread::parse_thread_answer(&raw).ok()?;

    // Mutation order: the dispatch above already succeeded — only now do
    // the turns land in `threads`.
    let _ = db::insert_thread_message(
        conn,
        &db::ThreadMessage {
            id: None,
            card_id: pc.card_id,
            turn_no,
            role: "user".to_string(),
            content: question.to_string(),
            ts: None,
        },
    );
    let _ = db::log_event(
        conn,
        &db::EventRecord {
            id: None,
            session_id: session_id.to_string(),
            kind: "thread_msg".to_string(),
            payload_json: serde_json::json!({
                "card_id": pc.card_id,
                "role": "user",
                "turn_no": turn_no,
            })
            .to_string(),
            ts: None,
        },
    );

    let rendered = thread::render_thread_answer(pc.rung, &answer);

    let _ = db::insert_thread_message(
        conn,
        &db::ThreadMessage {
            id: None,
            card_id: pc.card_id,
            turn_no,
            role: "assistant".to_string(),
            content: answer.answer.clone(),
            ts: None,
        },
    );
    let _ = db::log_event(
        conn,
        &db::EventRecord {
            id: None,
            session_id: session_id.to_string(),
            kind: "thread_msg".to_string(),
            payload_json: serde_json::json!({
                "card_id": pc.card_id,
                "role": "assistant",
                "turn_no": turn_no,
            })
            .to_string(),
            ts: None,
        },
    );

    Some(rendered)
}

/// T4 reqs 12-13 / D18: `murshid review`'s batched screen->judge pass over
/// every changed file in `snapshot`'s diff, ranked into a digest. Shared by
/// the CLI `review` subcommand and the in-pane `r` key — the only
/// difference between them is which snapshot/goal/canon/taxonomy/keys the
/// caller passes in.
#[allow(clippy::too_many_arguments)]
fn run_review(
    project_root: &std::path::Path,
    snapshot: &session::SessionSnapshot,
    taxonomy: &[pack::TaxonomyConcept],
    canon: &[pack::CanonEntry],
    grammar: &pack::GrammarSpec,
    prompts: &pack::PromptFragments,
    goal_text: &str,
    goal_cluster_dirs: &std::collections::HashSet<String>,
    screen_provider: &str,
    screen_model: &str,
    screen_key: Option<&str>,
    screen_base_url: Option<&str>,
    judge_provider: &str,
    judge_model: &str,
    judge_key: Option<&str>,
    judge_base_url: Option<&str>,
    conn: Option<&rusqlite::Connection>,
) -> review::ReviewDigest {
    // T5 support for T4 req 13 / D18: the real memory-derived below-mastery
    // list, wired in place of T4's static placeholder — falls back to it
    // only when no DB connection is available at all (never blocks the
    // review surface).
    let below_mastery_owned: Vec<String> = conn
        .map(|c| memory::below_mastery_concepts(c, taxonomy))
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| {
            review::BELOW_MASTERY_PLACEHOLDER
                .iter()
                .map(|s| s.to_string())
                .collect()
        });
    let below_mastery: Vec<&str> = below_mastery_owned.iter().map(String::as_str).collect();
    let dispatch_stage1 = |prompt: &str| -> Result<String, String> {
        judge::safe_dispatch(|| {
            provider::dispatch_debounced_with_model(
                screen_provider,
                Some(screen_model),
                prompt,
                screen_key,
                screen_base_url,
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
                judge_provider,
                Some(judge_model),
                prompt,
                judge_key,
                judge_base_url,
            )
        })
        .map_err(|m| match m {
            judge::JudgeMode::Degraded { reason } => reason,
            judge::JudgeMode::Active => "degraded".to_string(),
        })
    };

    let changed_files = session::tracked_and_modified_files(project_root).unwrap_or_default();
    let mut findings = Vec::new();
    for rel in changed_files {
        let hunks = match session::compute_session_diff(project_root, &rel, snapshot) {
            Ok(h) => h,
            Err(_) => continue,
        };
        if hunks.is_empty() {
            continue;
        }
        let abs = project_root.join(&rel);
        let Ok(content) = std::fs::read_to_string(&abs) else {
            continue;
        };
        if !site::parses_without_errors(&content, grammar) {
            continue;
        }
        let rel_str = rel.to_string_lossy().to_string();
        findings.extend(review::judge_review_hunks(
            &rel_str,
            &hunks,
            &content,
            taxonomy,
            canon,
            grammar,
            &prompts.stage1,
            goal_text,
            &below_mastery,
            dispatch_stage1,
            dispatch_stage2,
        ));
    }

    review::rank_and_digest(findings, goal_cluster_dirs, goal_text)
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
#[allow(clippy::too_many_arguments)]
fn run_retrieval_questions(
    conn: &rusqlite::Connection,
    session_id: &str,
    taxonomy: &[pack::TaxonomyConcept],
    canon: &[pack::CanonEntry],
    judge_provider: &str,
    judge_model: &str,
    judge_key: Option<&str>,
    judge_base_url: Option<&str>,
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
                .map(|c| (row, c.category.clone()))
        })
        .collect();
    let now_epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let candidates = retrieval::select_stale_concepts(&rows_with_category, now_epoch, cap_remaining);

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
        let Ok(raw) = judge::safe_dispatch(|| {
            provider::dispatch_debounced_with_model(
                judge_provider,
                Some(judge_model),
                &prompt,
                judge_key,
                judge_base_url,
            )
        }) else {
            continue;
        };
        let Ok(graded) = retrieval::parse_grading_response(&raw) else {
            continue;
        };
        let Some(grade) = bkt::Grade::parse(&graded.grade_str) else {
            continue;
        };
        if let Ok(enc) = memory::record_encounter(
            conn,
            session_id,
            &candidate.concept_id,
            &candidate.category,
            grade,
            "retrieval",
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
                // D13(c): file-backed, not DB-backed — run from the project
                // root (cwd), same convention as the other path-less
                // surfaces in R3.
                let project_root =
                    std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
                match cli_goal::run_goal_cli(&project_root, &args[2..]) {
                    Ok(_) => {
                        std::process::exit(0);
                    }
                    Err(e) => {
                        eprintln!("Goal command failed: {}", e);
                        std::process::exit(1);
                    }
                }
            }
            "progress" => {
                // T5 req 9 / I24: the open per-concept skill meter — plain
                // text, no live session needed.
                let pack_dir = pack::default_pack_dir();
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
                let cfg = config::load_config();
                let mut throttled_categories = std::collections::HashSet::new();
                for category in ["bug", "idiom", "best-practice", "architecture"] {
                    let statuses = db::recent_card_statuses_for_category(
                        &conn,
                        category,
                        throttle::THROTTLE_WINDOW,
                    )
                    .unwrap_or_default();
                    let unthrottled_by_config =
                        cfg.dial.unthrottle.iter().any(|c| c == category);
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
                let rows =
                    progress::build_rows(&memory_rows, &taxonomy, &throttled_categories, now_epoch);
                print!("{}", progress::render_progress(&rows));
                std::process::exit(0);
            }
            "review" => {
                // T4 req 12 / D18: `murshid review` — a standalone
                // invocation has no live watcher session, so the "session
                // diff" is the full working tree vs HEAD (C2's fallback
                // baseline path, same one `session::baseline_content` uses
                // for a file that predates a live snapshot).
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

                let pack_dir = pack::default_pack_dir();
                let taxonomy = pack::load_or_notice(pack::load_taxonomy(&pack_dir), "taxonomy", &pack_dir);
                let canon = pack::load_or_notice(pack::load_canon(&pack_dir), "canon", &pack_dir);
                let grammar = pack::load_or_notice(pack::load_grammar(&pack_dir), "grammar", &pack_dir);
                let prompts =
                    pack::load_or_notice(pack::load_prompt_fragments(&pack_dir), "prompts", &pack_dir);
                let cfg = config::load_config();
                let keys = credentials::get_api_keys();
                let screen_provider = cfg.models.screen.provider.clone();
                let screen_model = cfg.models.screen.model.clone();
                let screen_key = resolve_slot_key(&screen_provider, &keys);
                let screen_base_url = cfg.models.screen.base_url.clone();
                let judge_provider = cfg.models.judge.provider.clone();
                let judge_model = cfg.models.judge.model.clone();
                let judge_key = resolve_slot_key(&judge_provider, &keys);
                let judge_base_url = cfg.models.judge.base_url.clone();

                let mode = judge::determine_judge_mode(
                    &screen_provider,
                    screen_key.as_deref(),
                    &judge_provider,
                    judge_key.as_deref(),
                );
                if let judge::JudgeMode::Degraded { ref reason } = mode {
                    println!("{}", judge::degraded_status_line(reason));
                    std::process::exit(1);
                }

                // C6 BYOK consent: every review invocation prompts under `ask`.
                if consent::should_prompt_for_review(&cfg.consent.solicited_spend) {
                    let estimate = 500; // a full-diff pass is the expensive call
                    println!(
                        "{}",
                        consent::consent_prompt_line("murshid review", estimate)
                    );
                    use std::io::BufRead;
                    let mut answer = String::new();
                    let _ = std::io::stdin().lock().read_line(&mut answer);
                    if offer::classify_offer_key(answer.trim()) != offer::OfferKeyAction::Accept {
                        println!("okay, skipped");
                        std::process::exit(0);
                    }
                }

                let goal_text = goal_text_now(&project_root);
                let changed_files: Vec<std::path::PathBuf> =
                    session::tracked_and_modified_files(&project_root).unwrap_or_default();
                let goal_cluster_dirs = goal::cluster_dirs_from_files(&changed_files);

                let review_conn = db::get_db_path().and_then(|dp| db::open_connection(&dp).ok());
                let digest = run_review(
                    &project_root,
                    &session::SessionSnapshot::default(),
                    &taxonomy,
                    &canon,
                    &grammar,
                    &prompts,
                    &goal_text,
                    &goal_cluster_dirs,
                    &screen_provider,
                    &screen_model,
                    screen_key.as_deref(),
                    screen_base_url.as_deref(),
                    &judge_provider,
                    &judge_model,
                    judge_key.as_deref(),
                    judge_base_url.as_deref(),
                    review_conn.as_ref(),
                );

                if let Some(conn) = review_conn.as_ref() {
                        let sid = session::generate_session_id();
                        let _ = db::log_event(
                            conn,
                            &db::EventRecord {
                                id: None,
                                session_id: sid.clone(),
                                kind: "review_requested".to_string(),
                                payload_json: serde_json::json!({
                                    "top": digest.top.len(),
                                    "more_queued": digest.more_queued,
                                })
                                .to_string(),
                                ts: None,
                            },
                        );
                        for card in &digest.top {
                            let _ = db::insert_card(
                                conn,
                                &db::CardRecord {
                                    id: None,
                                    session_id: sid.clone(),
                                    concept_id: card.concept_name.clone(),
                                    category: db::REVIEW_CATEGORY.to_string(),
                                    // T5 review fix 4 / D18: static R2, not
                                    // memory-driven — `murshid review` is a
                                    // solicited, one-shot digest surface,
                                    // outside the per-card C4 ladder flow.
                                    rung_shown: ladder::Rung::R2.as_str().to_string(),
                                    advice_fp: format!("review:{}:{}:{}", sid, card.file, card.line),
                                    finding_fp: None,
                                    status: "shown".to_string(),
                                    created_ts: None,
                                    resolved_ts: None,
                                    worked_diff: Some(card.worked_diff.clone()),
                                    regresses_card_id: None,
                                    site_file: Some(card.file.clone()),
                                    site_line: Some(card.line as i64),
                                },
                            );
                        }
                }

                print!("{}", review::render_review_digest(&digest));
                std::process::exit(0);
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
                let pack_dir = pack::default_pack_dir();
                let taxonomy = pack::load_or_notice(pack::load_taxonomy(&pack_dir), "taxonomy", &pack_dir);
                let canon = pack::load_or_notice(pack::load_canon(&pack_dir), "canon", &pack_dir);
                // T6: grammar reference (payload 6) + judge-prompt fragments
                // (payload 4) — engine loses all language-shaped literals.
                // T6 review defect 3: any payload fallback prints a
                // degraded-mode notice instead of silently assuming Rust.
                let grammar = pack::load_or_notice(pack::load_grammar(&pack_dir), "grammar", &pack_dir);
                let prompts =
                    pack::load_or_notice(pack::load_prompt_fragments(&pack_dir), "prompts", &pack_dir);

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

                // T5 req 4 / C4: entry rung is now memory-driven per
                // concept (BKT band -> Wood shift -> knob offset); only the
                // knob setting itself is a fixed, session-wide value.
                let directness = ladder::directness_from_config(&cfg.dial.directness);
                println!("[murshid] directness: {}", cfg.dial.directness);

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

                // T3 req 10: pack-seeded help-comment pattern lists. Falls
                // back to the bundled pack's own defaults (T6: the fallback
                // literal lives in `pack::SurfaceConfig::default`, the pack
                // loader — never inline in this engine module) and prints a
                // degraded-mode notice when it does (review defect 3).
                let surface = pack::load_or_notice(pack::load_surface(&pack_dir), "surface", &pack_dir);

                // T3 req 5: the goal's file cluster (directories of the
                // session-start snapshot's tracked-and-modified files),
                // recomputed on every session split.
                let goal_cluster_dirs: std::sync::Arc<std::sync::Mutex<std::collections::HashSet<String>>> =
                    std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashSet::new()));
                // T3 req 4: drift tracking, session-scoped.
                let drift_tracking: std::sync::Arc<std::sync::Mutex<DriftTracking>> =
                    std::sync::Arc::new(std::sync::Mutex::new(DriftTracking::default()));
                // T3 reqs 7-10: struggle-signal state, session-scoped.
                let struggle_tracking: std::sync::Arc<std::sync::Mutex<StruggleTracking>> =
                    std::sync::Arc::new(std::sync::Mutex::new(StruggleTracking::default()));
                // T3 reqs 11-13: the single struggle offer awaiting a
                // response (never occupies the card slot).
                let pending_offer: std::sync::Arc<std::sync::Mutex<Option<PendingOffer>>> =
                    std::sync::Arc::new(std::sync::Mutex::new(None));
                // T4 req 8 / C6 BYOK consent: whether the session's first
                // thread turn has already been confirmed under `ask`
                // (`always` never consults this).
                let thread_consent_confirmed: std::sync::Arc<std::sync::Mutex<bool>> =
                    std::sync::Arc::new(std::sync::Mutex::new(false));
                // T4 req 12 / D18: the last-seen HEAD commit hash, for
                // offering `murshid review` at commit detection.
                let last_head_commit: std::sync::Arc<std::sync::Mutex<Option<String>>> =
                    std::sync::Arc::new(std::sync::Mutex::new(session::current_head_commit(&project_root)));

                // T3 req 1: infer the goal (branch -> commits -> file
                // cluster), never overwriting an explicit hand-edit (req 3),
                // and render the banner. NEVER prompts for input (req 1).
                {
                    let branch = goal::current_branch(&project_root);
                    let commit_subjects = goal::recent_commit_subjects(&project_root, 3);
                    let changed_files: Vec<std::path::PathBuf> =
                        snapshot.lock().unwrap_or_else(|e| e.into_inner()).files.keys().cloned().collect();
                    let (goal_text, was_inferred) = goal::resolve_session_goal(
                        &project_root,
                        branch.as_deref(),
                        &commit_subjects,
                        &changed_files,
                    );
                    *goal_cluster_dirs.lock().unwrap_or_else(|e| e.into_inner()) =
                        goal::cluster_dirs_from_files(&changed_files);
                    match &goal_text {
                        Some(t) => println!("[murshid] {}", goal::goal_banner(t)),
                        None => println!("[murshid] goal: (none yet — g to set)"),
                    }
                    if was_inferred {
                        if let (Some(dp), Some(t)) = (db::get_db_path(), goal_text.as_deref()) {
                            if let Ok(conn) = db::open_connection(&dp) {
                                let sid = session_mgr.lock().unwrap_or_else(|e| e.into_inner()).session_id.clone();
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
                    }
                }

                {
                    let sid = session_mgr.lock().unwrap_or_else(|e| e.into_inner()).session_id.clone();
                    if let Some(dp) = db::get_db_path() {
                        if let Ok(conn) = db::open_connection(&dp) {
                            // T3 req 8: recompute the user's own baseline
                            // fresh at every session start (C12).
                            let points = db::all_check_result_points(&conn).unwrap_or_default();
                            let durations = struggle::time_to_green_durations_ms(&points);
                            struggle_tracking.lock().unwrap_or_else(|e| e.into_inner()).baseline_ms =
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
                            *throttled_categories.lock().unwrap_or_else(|e| e.into_inner()) =
                                compute_throttle_state(&conn, &sid, &cfg.dial.unthrottle);
                        }
                    }
                }

                let keys = credentials::get_api_keys();
                let screen_provider = cfg.models.screen.provider.clone();
                let screen_model = cfg.models.screen.model.clone();
                let screen_key = resolve_slot_key(&screen_provider, &keys);
                let screen_base_url = cfg.models.screen.base_url.clone();
                let judge_provider = cfg.models.judge.provider.clone();
                let judge_model = cfg.models.judge.model.clone();
                let judge_key = resolve_slot_key(&judge_provider, &keys);
                let judge_base_url = cfg.models.judge.base_url.clone();

                // C6 degraded mode: no key for a non-Ollama slot -> observe-only.
                let mode = judge::determine_judge_mode(
                    &screen_provider,
                    screen_key.as_deref(),
                    &judge_provider,
                    judge_key.as_deref(),
                );
                if let judge::JudgeMode::Degraded { ref reason } = mode {
                    println!("{}", judge::degraded_status_line(reason));
                } else if let Some(dp) = db::get_db_path() {
                    // T5 reqs 7-8 / D22: session-start recall questions —
                    // never during the work session, never in degraded mode
                    // (grading needs a live judge call).
                    if let Ok(conn) = db::open_connection(&dp) {
                        let sid = session_mgr.lock().unwrap_or_else(|e| e.into_inner()).session_id.clone();
                        run_retrieval_questions(
                            &conn,
                            &sid,
                            &taxonomy,
                            &canon,
                            &judge_provider,
                            &judge_model,
                            judge_key.as_deref(),
                            judge_base_url.as_deref(),
                        );
                    }
                }

                // req 10: best-effort session-end cleanup on Ctrl+C — marks any
                // still-`shown` card `expired`, logs `session_end` (T3 req 6:
                // carrying the bookend), and renders the bookend screen —
                // mirroring credentials.rs's SIGHUP-reload pattern
                // (spawn-a-thread handler).
                {
                    let session_mgr_for_shutdown = session_mgr.clone();
                    let db_path_for_shutdown = db::get_db_path();
                    let project_root_for_shutdown = project_root.clone();
                    let taxonomy_for_shutdown = taxonomy.clone();
                    let queue_for_shutdown = queue_state.clone();
                    let goal_cluster_for_shutdown = goal_cluster_dirs.clone();
                    let throttled_for_shutdown = throttled_categories.clone();
                    let cleanup: Box<dyn Fn() + Send> = Box::new(move || {
                        let sid = session_mgr_for_shutdown.lock().unwrap_or_else(|e| e.into_inner()).session_id.clone();
                        if let Some(ref dp) = db_path_for_shutdown {
                            if let Ok(conn) = db::open_connection(dp) {
                                let expired = db::expire_unresolved_cards(&conn, &sid).unwrap_or(0);
                                // req 3/8 / C2: the pull queue and (non-
                                // offer-concept) snoozes die at session end.
                                let _ = db::purge_suppressions_for_session(&conn, &sid);
                                let b = assemble_session_bookend(
                                    &conn,
                                    &sid,
                                    &project_root_for_shutdown,
                                    &taxonomy_for_shutdown,
                                    &queue_for_shutdown,
                                    &goal_cluster_for_shutdown,
                                    &throttled_for_shutdown,
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
                                    eprintln!("Warning: failed to save progress backup at session end: {}", e);
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
                    let pending_card_for_stdin = pending_card.clone();
                    let queue_for_stdin = queue_state.clone();
                    let pending_offer_for_stdin = pending_offer.clone();
                    let session_mgr_for_stdin = session_mgr.clone();
                    let goal_cluster_for_stdin = goal_cluster_dirs.clone();
                    let project_root_for_stdin = project_root.clone();
                    let snapshot_for_stdin = snapshot.clone();
                    let taxonomy_for_stdin = taxonomy.clone();
                    let canon_for_stdin = canon.clone();
                    let grammar_for_stdin = grammar.clone();
                    let prompts_for_stdin = prompts.clone();
                    let bucket_for_stdin = bucket.clone();
                    let screen_provider_for_stdin = screen_provider.clone();
                    let screen_model_for_stdin = screen_model.clone();
                    let screen_key_for_stdin = screen_key.clone();
                    let screen_base_url_for_stdin = screen_base_url.clone();
                    let judge_provider_for_stdin = judge_provider.clone();
                    let judge_model_for_stdin = judge_model.clone();
                    let judge_key_for_stdin = judge_key.clone();
                    let judge_base_url_for_stdin = judge_base_url.clone();
                    let directness_for_stdin = directness;
                    let surface_for_stdin = surface.clone();
                    let consent_setting_for_stdin = cfg.consent.solicited_spend.clone();
                    let thread_consent_confirmed_for_stdin = thread_consent_confirmed.clone();
                    std::thread::spawn(move || {
                        use std::io::BufRead;
                        let stdin = std::io::stdin();
                        let mut lines_iter = stdin.lock().lines().map_while(Result::ok);
                        while let Some(line) = lines_iter.next() {
                            let trimmed = line.trim().to_string();
                            let trimmed = trimmed.as_str();

                            // req 11-13: a pending struggle offer takes
                            // priority over y/n only — review fix: every
                            // other key (per `offer::classify_offer_key`)
                            // falls through to its normal binding below and
                            // leaves the offer live (I10's silent-expiry
                            // path, or a later y/n, still resolves it).
                            let maybe_offer = pending_offer_for_stdin.lock().unwrap_or_else(|e| e.into_inner()).clone();
                            if let Some(po) = maybe_offer {
                                let action = offer::classify_offer_key(trimmed);
                                if action != offer::OfferKeyAction::Ignore {
                                    *pending_offer_for_stdin.lock().unwrap_or_else(|e| e.into_inner()) = None;
                                    let sid = session_mgr_for_stdin.lock().unwrap_or_else(|e| e.into_inner()).session_id.clone();
                                    let Some(dp) = db::get_db_path() else { continue };
                                    let Ok(conn) = db::open_connection(&dp) else { continue };

                                    if action == offer::OfferKeyAction::Accept {
                                        let _ = db::update_card_status(&conn, po.card_id, "applied");
                                        let _ = db::log_event(
                                            &conn,
                                            &db::EventRecord {
                                                id: None,
                                                session_id: sid.clone(),
                                                kind: "prompt_response".to_string(),
                                                payload_json: serde_json::json!({
                                                    "verb": "accepted",
                                                    "signal": po.key.0,
                                                    "concept": po.key.1,
                                                })
                                                .to_string(),
                                                ts: None,
                                            },
                                        );
                                        if pending_card_for_stdin.lock().unwrap_or_else(|e| e.into_inner()).is_none() {
                                            let snap = snapshot_for_stdin.lock().unwrap_or_else(|e| e.into_inner()).clone();
                                            match run_struggle_judge_and_show(
                                                &conn,
                                                &sid,
                                                &project_root_for_stdin,
                                                &po.site_file,
                                                &snap,
                                                &taxonomy_for_stdin,
                                                &canon_for_stdin,
                                                &grammar_for_stdin,
                                                &prompts_for_stdin,
                                                &screen_provider_for_stdin,
                                                &screen_model_for_stdin,
                                                screen_key_for_stdin.as_deref(),
                                                screen_base_url_for_stdin.as_deref(),
                                                &judge_provider_for_stdin,
                                                &judge_model_for_stdin,
                                                judge_key_for_stdin.as_deref(),
                                                judge_base_url_for_stdin.as_deref(),
                                                &bucket_for_stdin,
                                                directness_for_stdin,
                                                &surface_for_stdin.comment_token,
                                            ) {
                                                Some(pc) => {
                                                    *pending_card_for_stdin.lock().unwrap_or_else(|e| e.into_inner()) = Some(pc);
                                                }
                                                None => println!(
                                                    "  nothing new to show at that site right now"
                                                ),
                                            }
                                        }
                                    } else {
                                        // Explicit "n" only (review fix —
                                        // Ignore never reaches here).
                                        let _ = db::update_card_status(&conn, po.card_id, "not_now");
                                        let _ = db::log_event(
                                            &conn,
                                            &db::EventRecord {
                                                id: None,
                                                session_id: sid.clone(),
                                                kind: "prompt_response".to_string(),
                                                payload_json: serde_json::json!({
                                                    "verb": "declined",
                                                    "signal": po.key.0,
                                                    "concept": po.key.1,
                                                })
                                                .to_string(),
                                                ts: None,
                                            },
                                        );
                                        // req 13: two declines across
                                        // sessions for this concept -> 7-day
                                        // suppression.
                                        let declines = db::count_declined_offers_for_concept(
                                            &conn, &po.key.1,
                                        )
                                        .unwrap_or(0);
                                        if offer::should_suppress_after_declines(declines) {
                                            let expiry = offer::suppression_expiry_epoch_secs(
                                                std::time::SystemTime::now(),
                                            );
                                            let _ = db::insert_offer_suppression(
                                                &conn, &sid, &po.key.1, expiry,
                                            );
                                        }
                                    }
                                    continue;
                                }
                                // Ignore: fall through — the offer stays
                                // pending untouched.
                            }

                            // req 2: `g` with no pending card opens $EDITOR
                            // on the goal file (T1/T2's `g` = got_it on a
                            // pending card takes precedence when one exists).
                            // Review fix: never pre-create the file with a
                            // placeholder before handing off to $EDITOR — a
                            // quit-without-saving would otherwise leave a
                            // zero-byte stub sitting there. Only the parent
                            // directory needs to exist for a save to land;
                            // $EDITOR itself handles a missing path (and
                            // goal.rs's resolve_session_goal additionally
                            // never treats an empty file as explicit, so an
                            // editor that *does* leave a stub is harmless).
                            if trimmed.eq_ignore_ascii_case("g")
                                && pending_card_for_stdin.lock().unwrap_or_else(|e| e.into_inner()).is_none()
                            {
                                let path = goal::goal_file_path(&project_root_for_stdin);
                                if let Some(parent) = path.parent() {
                                    let _ = std::fs::create_dir_all(parent);
                                }
                                let editor =
                                    std::env::var("EDITOR").unwrap_or_else(|_| "vi".to_string());
                                let _ = std::process::Command::new(editor).arg(&path).status();
                                continue;
                            }

                            if trimmed.eq_ignore_ascii_case("m") {
                                let mut q = queue_for_stdin.lock().unwrap_or_else(|e| e.into_inner());
                                let cluster = goal_cluster_for_stdin.lock().unwrap_or_else(|e| e.into_inner()).clone();
                                let goal_text = goal_text_now(&project_root_for_stdin);
                                queue::sort_queue(&mut q, &cluster, &goal_text);
                                if q.is_empty() {
                                    println!("  (queue is empty)");
                                } else {
                                    print!("{}", queue::render_queue_list(&q));
                                }
                                continue;
                            }

                            // T4 req 12 / D18: `r` runs `murshid review` in-pane
                            // over the live session diff. BYOK consent (C6)
                            // prompts EVERY invocation under `ask`, with a rough
                            // token estimate; the caller confirms on the very
                            // next line (the same read-ahead shape as `k` below).
                            if trimmed.eq_ignore_ascii_case("r") {
                                if consent::should_prompt_for_review(&consent_setting_for_stdin) {
                                    let estimate = consent::estimate_tokens(&goal_text_now(&project_root_for_stdin))
                                        .max(500); // a full-diff pass is the expensive call
                                    println!(
                                        "  {}",
                                        consent::consent_prompt_line("murshid review", estimate)
                                    );
                                    let Some(confirm_line) = lines_iter.next() else { continue };
                                    if offer::classify_offer_key(confirm_line.trim())
                                        != offer::OfferKeyAction::Accept
                                    {
                                        println!("  okay, skipped");
                                        continue;
                                    }
                                }

                                let goal_text = goal_text_now(&project_root_for_stdin);
                                let cluster = goal_cluster_for_stdin.lock().unwrap_or_else(|e| e.into_inner()).clone();
                                let snap = snapshot_for_stdin.lock().unwrap_or_else(|e| e.into_inner()).clone();
                                let review_conn =
                                    db::get_db_path().and_then(|dp| db::open_connection(&dp).ok());
                                let digest = run_review(
                                    &project_root_for_stdin,
                                    &snap,
                                    &taxonomy_for_stdin,
                                    &canon_for_stdin,
                                    &grammar_for_stdin,
                                    &prompts_for_stdin,
                                    &goal_text,
                                    &cluster,
                                    &screen_provider_for_stdin,
                                    &screen_model_for_stdin,
                                    screen_key_for_stdin.as_deref(),
                                    screen_base_url_for_stdin.as_deref(),
                                    &judge_provider_for_stdin,
                                    &judge_model_for_stdin,
                                    judge_key_for_stdin.as_deref(),
                                    judge_base_url_for_stdin.as_deref(),
                                    review_conn.as_ref(),
                                );

                                if let (Some(conn), Some(sid_for_review)) = (
                                    review_conn.as_ref(),
                                    Some(session_mgr_for_stdin.lock().unwrap_or_else(|e| e.into_inner()).session_id.clone()),
                                ) {
                                    {
                                        let _ = db::log_event(
                                            conn,
                                            &db::EventRecord {
                                                id: None,
                                                session_id: sid_for_review.clone(),
                                                kind: "review_requested".to_string(),
                                                payload_json: serde_json::json!({
                                                    "top": digest.top.len(),
                                                    "more_queued": digest.more_queued,
                                                })
                                                .to_string(),
                                                ts: None,
                                            },
                                        );
                                        // req 13: review cards are logged
                                        // EFP-exempt (db::REVIEW_CATEGORY).
                                        for card in &digest.top {
                                            let _ = db::insert_card(
                                                conn,
                                                &db::CardRecord {
                                                    id: None,
                                                    session_id: sid_for_review.clone(),
                                                    concept_id: card.concept_name.clone(),
                                                    category: db::REVIEW_CATEGORY.to_string(),
                                                    // T5 review fix 4 / D18: static R2,
                                                    // not memory-driven — see the CLI
                                                    // `review` subcommand's identical
                                                    // insert above for the rationale.
                                                    rung_shown: ladder::Rung::R2.as_str().to_string(),
                                                    advice_fp: format!(
                                                        "review:{}:{}:{}",
                                                        sid_for_review, card.file, card.line
                                                    ),
                                                    finding_fp: None,
                                                    status: "shown".to_string(),
                                                    created_ts: None,
                                                    resolved_ts: None,
                                                    worked_diff: Some(card.worked_diff.clone()),
                                                    regresses_card_id: None,
                                                    site_file: Some(card.file.clone()),
                                                    site_line: Some(card.line as i64),
                                                },
                                            );
                                        }
                                    }
                                }

                                print!("{}", review::render_review_digest(&digest));
                                continue;
                            }

                            if let Ok(choice) = trimmed.parse::<usize>() {
                                if choice == 0 {
                                    continue;
                                }
                                if pending_card_for_stdin.lock().unwrap_or_else(|e| e.into_inner()).is_some() {
                                    println!(
                                        "  finish the current card first (g/u/n), then pick again"
                                    );
                                    continue;
                                }
                                let mut q = queue_for_stdin.lock().unwrap_or_else(|e| e.into_inner());
                                let cluster = goal_cluster_for_stdin.lock().unwrap_or_else(|e| e.into_inner()).clone();
                                let goal_text = goal_text_now(&project_root_for_stdin);
                                queue::sort_queue(&mut q, &cluster, &goal_text);
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
                                let pulled_rung = resolve_entry_rung(
                                    &conn,
                                    &entry.finding.concept_id,
                                    &entry.finding.category,
                                    directness_for_stdin,
                                );
                                let _ = db::update_card_rung(&conn, entry.card_id, pulled_rung.as_str());
                                println!(
                                    "{}",
                                    card::render_card_at_rung(
                                        &shown_card,
                                        pulled_rung,
                                        0,
                                        &surface_for_stdin.comment_token
                                    )
                                );
                                // req 1: re-derive the STORED site identity
                                // (enclosing item + anchor hash) for the
                                // applied-detection re-check — a queued card
                                // never had a live `Site` object retained.
                                let (site_enclosing_item, site_anchor_hash) = std::fs::read_to_string(
                                    project_root_for_stdin.join(&shown_card.file),
                                )
                                .ok()
                                .and_then(|c| {
                                    site::compute_site(&shown_card.file, &c, shown_card.line, &grammar_for_stdin)
                                })
                                .map(|s| (Some(s.enclosing_item), Some(s.anchor_hash)))
                                .unwrap_or((None, None));
                                *pending_card_for_stdin.lock().unwrap_or_else(|e| e.into_inner()) = Some(PendingCard {
                                    card_id: entry.card_id,
                                    session_id: entry.session_id,
                                    concept_id: entry.finding.concept_id,
                                    concept_name: shown_card.concept_name.clone(),
                                    advice_fp: entry.finding.advice_fp,
                                    category: entry.finding.category.clone(),
                                    rung: pulled_rung,
                                    site_enclosing_item,
                                    site_anchor_hash,
                                    card: shown_card,
                                });
                                continue;
                            }

                            // T4 reqs 2-5 / C4/C5: extends the classifier-
                            // then-fall-through pattern — `e`/`t`/`k` are
                            // card-scoped actions that do NOT consume the
                            // slot (the card stays focused); the plain C3
                            // lifecycle verbs (a/g/u/n) still do.
                            match response::classify_card_key(trimmed) {
                                response::CardKeyAction::Ignore => {}

                                response::CardKeyAction::Escalate
                                | response::CardKeyAction::TellMe => {
                                    let maybe_pc = pending_card_for_stdin.lock().unwrap_or_else(|e| e.into_inner()).clone();
                                    let Some(pc) = maybe_pc else { continue };
                                    let Some(dp) = db::get_db_path() else { continue };
                                    let Ok(conn) = db::open_connection(&dp) else { continue };

                                    let escalate = response::classify_card_key(trimmed)
                                        == response::CardKeyAction::Escalate;
                                    let new_rung = if escalate {
                                        ladder::escalate_one(pc.rung)
                                    } else {
                                        ladder::tell_me(pc.rung)
                                    };

                                    // req 3/4: every step (and every reveal,
                                    // even a repeat `t` at R3) is logged —
                                    // click-through gaming must be visible.
                                    let _ = db::update_card_rung(&conn, pc.card_id, new_rung.as_str());
                                    let _ = db::log_event(
                                        &conn,
                                        &db::EventRecord {
                                            id: None,
                                            session_id: pc.session_id.clone(),
                                            kind: "card_response".to_string(),
                                            payload_json: serde_json::json!({
                                                "verb": "escalated",
                                                "concept": pc.concept_id,
                                                "from": pc.rung.as_str(),
                                                "to": new_rung.as_str(),
                                            })
                                            .to_string(),
                                            ts: None,
                                        },
                                    );
                                    println!(
                                        "{}",
                                        card::render_card_at_rung(
                                            &pc.card,
                                            new_rung,
                                            0,
                                            &surface_for_stdin.comment_token
                                        )
                                    );
                                    let mut updated = pc;
                                    updated.rung = new_rung;
                                    *pending_card_for_stdin.lock().unwrap_or_else(|e| e.into_inner()) = Some(updated);
                                }

                                response::CardKeyAction::Ask => {
                                    let maybe_pc = pending_card_for_stdin.lock().unwrap_or_else(|e| e.into_inner()).clone();
                                    let Some(pc) = maybe_pc else { continue };
                                    let sid = session_mgr_for_stdin.lock().unwrap_or_else(|e| e.into_inner()).session_id.clone();
                                    let Some(dp) = db::get_db_path() else { continue };
                                    let Ok(conn) = db::open_connection(&dp) else { continue };

                                    // req 5 / C12: the 5-user-turn cap.
                                    let turns_so_far =
                                        db::thread_user_turn_count(&conn, pc.card_id).unwrap_or(0);
                                    if thread::thread_cap_reached(turns_so_far) {
                                        println!("  {}", thread::THREAD_CAP_NOTICE);
                                        continue;
                                    }

                                    println!("  ask \u{2014} type your question:");
                                    let Some(question_line) = lines_iter.next() else { continue };
                                    let question = question_line.trim().to_string();
                                    if question.is_empty() {
                                        continue;
                                    }

                                    // req 8 / C6 BYOK consent: first thread
                                    // turn per session confirms under `ask`.
                                    let already_confirmed =
                                        *thread_consent_confirmed_for_stdin.lock().unwrap_or_else(|e| e.into_inner());
                                    if consent::should_prompt_for_thread(
                                        &consent_setting_for_stdin,
                                        already_confirmed,
                                    ) {
                                        let estimate = consent::estimate_tokens(&question);
                                        println!(
                                            "  {}",
                                            consent::consent_prompt_line("this thread turn", estimate)
                                        );
                                        let Some(confirm_line) = lines_iter.next() else { continue };
                                        if offer::classify_offer_key(confirm_line.trim())
                                            != offer::OfferKeyAction::Accept
                                        {
                                            println!("  okay, skipped");
                                            continue;
                                        }
                                        *thread_consent_confirmed_for_stdin.lock().unwrap_or_else(|e| e.into_inner()) = true;
                                    }

                                    match run_thread_turn(
                                        &conn,
                                        &sid,
                                        &pc,
                                        &question,
                                        &canon_for_stdin,
                                        &judge_provider_for_stdin,
                                        &judge_model_for_stdin,
                                        judge_key_for_stdin.as_deref(),
                                        judge_base_url_for_stdin.as_deref(),
                                    ) {
                                        Some(rendered) => {
                                            println!("{}", rendered);
                                            let new_count =
                                                db::thread_user_turn_count(&conn, pc.card_id)
                                                    .unwrap_or(0);
                                            if thread::thread_cap_reached(new_count) {
                                                println!("  {}", thread::THREAD_CAP_NOTICE);
                                            }
                                        }
                                        None => println!(
                                            "  (no answer \u{2014} degraded mode or provider error)"
                                        ),
                                    }
                                }

                                response::CardKeyAction::Response(verb) => {
                                    let maybe_pc = pending_card_for_stdin.lock().unwrap_or_else(|e| e.into_inner()).take();
                                    let Some(pc) = maybe_pc else { continue };
                                    let Some(dp) = db::get_db_path() else {
                                        continue;
                                    };
                                    let Ok(conn) = db::open_connection(&dp) else {
                                        continue;
                                    };
                                    let _ = db::update_card_status(&conn, pc.card_id, verb);

                                    // T5 req 3(c)/10: `applied` (manual `a`)
                                    // is the ONLY response verb that is
                                    // evidence — got_it/not_now/not_useful
                                    // are dismissals (I23), never mastery
                                    // signal. Guarded by the single testable
                                    // source of truth in memory.rs so a
                                    // future new verb can't silently become
                                    // evidence by accident.
                                    if let Some(grade) =
                                        memory::should_record_evidence_for_response(verb)
                                    {
                                        // A D17 comment-ask card's `category`
                                        // field holds the pseudo-category
                                        // `comment-ask` (bookkeeping only) —
                                        // resolve the concept's REAL
                                        // taxonomy category for BKT priors.
                                        let real_category = taxonomy_for_stdin
                                            .iter()
                                            .find(|c| c.slug == pc.concept_id)
                                            .map(|c| c.category.clone())
                                            .unwrap_or_else(|| pc.category.clone());
                                        if let Ok(enc) = memory::record_encounter(
                                            &conn,
                                            &pc.session_id,
                                            &pc.concept_id,
                                            &real_category,
                                            grade,
                                            "applied",
                                        ) {
                                            if enc.crossed_into_mastery {
                                                println!(
                                                    "[murshid] backing off on {} \u{2014} applied {} times straight",
                                                    pc.concept_name, enc.row.pass_streak
                                                );
                                            }
                                        }
                                    }

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
                            }
                        }
                    });
                }

                // T3 reqs 9/11-13: the struggle-offer poll — evaluates
                // idle-gating and convergence on a timer (idle can only be
                // known to have elapsed by *not* seeing a file event, so
                // this can't be driven from the file-event callback alone),
                // fires at most one offer at a time, and expires it
                // silently if the user goes back to typing (I10).
                {
                    let pending_offer_for_poll = pending_offer.clone();
                    let pending_card_for_poll = pending_card.clone();
                    let struggle_tracking_for_poll = struggle_tracking.clone();
                    let last_event_at_for_poll = last_event_at.clone();
                    let session_mgr_for_poll = session_mgr.clone();
                    let throttled_categories_for_poll = throttled_categories.clone();
                    std::thread::spawn(move || {
                        loop {
                            std::thread::sleep(std::time::Duration::from_secs(3));

                            let Some(dp) = db::get_db_path() else { continue };
                            let Ok(conn) = db::open_connection(&dp) else { continue };
                            let sid = session_mgr_for_poll.lock().unwrap_or_else(|e| e.into_inner()).session_id.clone();
                            let now = std::time::SystemTime::now();
                            let last_evt = *last_event_at_for_poll.lock().unwrap_or_else(|e| e.into_inner());

                            // I10: continuing to type expires a live offer
                            // silently — no decline persistence penalty.
                            {
                                let live = pending_offer_for_poll.lock().unwrap_or_else(|e| e.into_inner()).clone();
                                if let Some(po) = live {
                                    if offer::expired_by_continued_typing(po.fired_at, last_evt) {
                                        let _ = db::update_card_status(&conn, po.card_id, "expired");
                                        let _ = db::log_event(
                                            &conn,
                                            &db::EventRecord {
                                                id: None,
                                                session_id: sid.clone(),
                                                kind: "prompt_response".to_string(),
                                                payload_json: serde_json::json!({
                                                    "verb": "expired",
                                                    "signal": po.key.0,
                                                    "concept": po.key.1,
                                                })
                                                .to_string(),
                                                ts: None,
                                            },
                                        );
                                        *pending_offer_for_poll.lock().unwrap_or_else(|e| e.into_inner()) = None;
                                    }
                                    continue; // at most one live offer at a time
                                }
                            }

                            if pending_card_for_poll.lock().unwrap_or_else(|e| e.into_inner()).is_some() {
                                continue; // never stack an offer atop a shown card
                            }

                            // req 12: offers share D12's auto-throttle too.
                            if throttled_categories_for_poll
                                .lock()
                                .unwrap()
                                .contains(offer::OFFER_CATEGORY)
                            {
                                continue;
                            }

                            let idle = offer::is_idle(last_evt, now);
                            if !idle {
                                continue;
                            }

                            let now_ms = now
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_millis();

                            // req 9/I14: converged inferred pair takes
                            // priority; signal 3 (self-declared) fires
                            // alone. Selected FIRST, gated SECOND (req 11,
                            // clarified 2026-07-03): the never-while-green
                            // gate applies only to the inferred pair —
                            // `may_offer` is per-evidence-type, so a fresh
                            // help comment can still fire on a green build.
                            let candidate = {
                                let st = struggle_tracking_for_poll.lock().unwrap_or_else(|e| e.into_inner());
                                let same_error = st.error_streak.fired();
                                let time_in_red = st.red_streak.fired(now_ms, st.baseline_ms);
                                if struggle::inferred_pair_converged(same_error, time_in_red) {
                                    st.error_streak.code().zip(st.struggle_site.clone()).map(
                                        |(code, site)| {
                                            (
                                                offer::Evidence::ErrorStreak {
                                                    code: code.to_string(),
                                                    minutes: st.red_streak.minutes_in_red(now_ms),
                                                },
                                                site,
                                            )
                                        },
                                    )
                                } else {
                                    st.help_candidate
                                        .clone()
                                        .map(|(site, snippet)| (offer::Evidence::HelpComment { snippet }, site))
                                }
                            };
                            let Some((evidence, site_file)) = candidate else { continue };

                            let last_success = struggle_tracking_for_poll.lock().unwrap_or_else(|e| e.into_inner()).last_check_success;
                            if !offer::may_offer(&evidence, last_success, idle) {
                                continue;
                            }

                            let key = offer::offer_key(&evidence);

                            if struggle_tracking_for_poll
                                .lock()
                                .unwrap()
                                .already_offered
                                .contains(&key)
                            {
                                continue;
                            }
                            let now_secs = now
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_secs() as i64;
                            if db::is_offer_suppressed(&conn, &key.1, now_secs).unwrap_or(false) {
                                continue;
                            }

                            // Review fix: only mark this (signal, key) as
                            // offered once the `cards` row actually lands —
                            // a DB error here must not silently burn the
                            // session's one shot at this signal with
                            // nothing ever shown.
                            let Ok(card_id) = db::insert_card(
                                &conn,
                                &db::CardRecord {
                                    id: None,
                                    session_id: sid.clone(),
                                    concept_id: key.1.clone(),
                                    category: offer::OFFER_CATEGORY.to_string(),
                                    rung_shown: "offer".to_string(),
                                    advice_fp: format!("struggle-offer:{}:{}", key.0, key.1),
                                    finding_fp: None,
                                    status: "shown".to_string(),
                                    created_ts: None,
                                    resolved_ts: None,
                                    worked_diff: None,
                                    regresses_card_id: None,
                                    site_file: None,
                                    site_line: None,
                                },
                            ) else {
                                continue;
                            };
                            struggle_tracking_for_poll
                                .lock()
                                .unwrap()
                                .already_offered
                                .insert(key.clone());
                            let _ = db::log_event(
                                &conn,
                                &db::EventRecord {
                                    id: None,
                                    session_id: sid.clone(),
                                    kind: "prompt_offered".to_string(),
                                    payload_json: serde_json::json!({
                                        "signal": key.0,
                                        "concept": key.1,
                                    })
                                    .to_string(),
                                    ts: None,
                                },
                            );
                            println!("{}", offer::offer_line(&evidence));
                            *pending_offer_for_poll.lock().unwrap_or_else(|e| e.into_inner()) = Some(PendingOffer {
                                key,
                                site_file,
                                fired_at: now,
                                card_id,
                            });
                        }
                    });
                }

                let project_root_cb = project_root.clone();
                let watched_extensions = surface.file_extensions.clone();

                let _watcher = match watcher::start_watching(project_root.clone(), &watched_extensions, move |path| {
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
                    *last_event_at.lock().unwrap_or_else(|e| e.into_inner()) = now;

                    // C2 session split on idle gap > 4h: expire the OLD session's
                    // unresolved cards (req 10 / C3 "no interaction by session end
                    // ⇒ expired") before rotating the snapshot to the new one.
                    let old_session_id = session_mgr.lock().unwrap_or_else(|e| e.into_inner()).session_id.clone();
                    let split = session_mgr.lock().unwrap_or_else(|e| e.into_inner()).on_file_event(now);
                    let session_id_now = session_mgr.lock().unwrap_or_else(|e| e.into_inner()).session_id.clone();
                    if split {
                        if let Some(ref conn) = conn_opt {
                            let expired =
                                db::expire_unresolved_cards(conn, &old_session_id).unwrap_or(0);
                            // req 3/8 / C2: the pull queue and (non-offer-
                            // concept) snoozes die at session end.
                            let _ = db::purge_suppressions_for_session(conn, &old_session_id);
                            // T3 req 6: the bookend renders at every session
                            // end, not just process exit.
                            let b = assemble_session_bookend(
                                conn,
                                &old_session_id,
                                &project_root_cb,
                                &taxonomy,
                                &queue_state,
                                &goal_cluster_dirs,
                                &throttled_categories,
                            );
                            let _ = db::log_event(
                                conn,
                                &db::EventRecord {
                                    id: None,
                                    session_id: old_session_id.clone(),
                                    kind: "session_end".to_string(),
                                    payload_json: bookend_event_payload(&b, expired).to_string(),
                                    ts: None,
                                },
                            );
                            // T9 req 2: snapshot progress at the session
                            // bookend, not just after migrations.
                            if let Err(e) = db::save_backup_from_db(conn) {
                                eprintln!("Warning: failed to save progress backup at session end: {}", e);
                            }
                            println!("{}", bookend::render_bookend(&b));
                        }
                        queue_state.lock().unwrap_or_else(|e| e.into_inner()).clear();
                        dispatched_hunk_signatures.lock().unwrap_or_else(|e| e.into_inner()).clear();
                        *pending_card.lock().unwrap_or_else(|e| e.into_inner()) = None;
                        *pending_offer.lock().unwrap_or_else(|e| e.into_inner()) = None;
                        *drift_tracking.lock().unwrap_or_else(|e| e.into_inner()) = DriftTracking::default();
                        *struggle_tracking.lock().unwrap_or_else(|e| e.into_inner()) = StruggleTracking::default();
                        *snapshot.lock().unwrap_or_else(|e| e.into_inner()) =
                            session::snapshot_session_start(&project_root_cb).unwrap_or_default();

                        // req 1/3: re-resolve the goal at this natural
                        // boundary (never overwrites a hand edit).
                        {
                            let branch = goal::current_branch(&project_root_cb);
                            let commit_subjects = goal::recent_commit_subjects(&project_root_cb, 3);
                            let changed_files: Vec<std::path::PathBuf> =
                                snapshot.lock().unwrap_or_else(|e| e.into_inner()).files.keys().cloned().collect();
                            let (goal_text, was_inferred) = goal::resolve_session_goal(
                                &project_root_cb,
                                branch.as_deref(),
                                &commit_subjects,
                                &changed_files,
                            );
                            *goal_cluster_dirs.lock().unwrap_or_else(|e| e.into_inner()) =
                                goal::cluster_dirs_from_files(&changed_files);
                            if let Some(t) = &goal_text {
                                println!("[murshid] {}", goal::goal_banner(t));
                            }
                            if was_inferred {
                                if let (Some(conn), Some(t)) = (&conn_opt, goal_text.as_deref()) {
                                    let _ = db::log_event(
                                        conn,
                                        &db::EventRecord {
                                            id: None,
                                            session_id: session_id_now.clone(),
                                            kind: "goal_inferred".to_string(),
                                            payload_json: serde_json::json!({ "text": t })
                                                .to_string(),
                                            ts: None,
                                        },
                                    );
                                }
                            }
                        }

                        if let Some(ref conn) = conn_opt {
                            // T3 req 8: recompute the baseline fresh at
                            // every session start (C12).
                            let points = db::all_check_result_points(conn).unwrap_or_default();
                            let durations = struggle::time_to_green_durations_ms(&points);
                            struggle_tracking.lock().unwrap_or_else(|e| e.into_inner()).baseline_ms =
                                struggle::percentile_75_ms(&durations);

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
                            *throttled_categories.lock().unwrap_or_else(|e| e.into_inner()) =
                                compute_throttle_state(conn, &session_id_now, &cfg.dial.unthrottle);
                        }
                    }

                    let rel_path = path
                        .strip_prefix(&project_root_cb)
                        .unwrap_or(path.as_path())
                        .to_path_buf();
                    pending_files.lock().unwrap_or_else(|e| e.into_inner()).insert(rel_path.clone());

                    // T3 req 4: drift — track this touch, prune to the
                    // trailing 30-min window, and fire the one-per-session
                    // notice when ≥70% of recent touches fall outside the
                    // goal's file cluster.
                    {
                        let rel_str = rel_path.to_string_lossy().to_string();
                        let mut dt = drift_tracking.lock().unwrap_or_else(|e| e.into_inner());
                        dt.touches.push((rel_str, now));
                        dt.touches
                            .retain(|(_, t)| now.duration_since(*t).unwrap_or_default() <= goal::DRIFT_WINDOW);
                        let recent: Vec<String> = dt.touches.iter().map(|(f, _)| f.clone()).collect();
                        let cluster = goal_cluster_dirs.lock().unwrap_or_else(|e| e.into_inner()).clone();
                        let ratio = goal::drift_ratio(&cluster, &recent);
                        if goal::should_fire_drift(dt.fired, ratio) {
                            dt.fired = true;
                            println!("[murshid] {}", goal::DRIFT_NOTICE);
                        }
                    }

                    // D8/C12 quiescence gate: wait out the pause, then bail if a
                    // newer file event superseded this one (its own timer will
                    // handle the latest state).
                    std::thread::sleep(
                        quiescence::QUIESCENCE_PAUSE + std::time::Duration::from_millis(100),
                    );
                    if *last_event_at.lock().unwrap_or_else(|e| e.into_inner()) != now {
                        return;
                    }

                    let content = match std::fs::read_to_string(&path) {
                        Ok(c) => c,
                        Err(_) => return,
                    };
                    if !site::parses_without_errors(&content, &grammar) {
                        return; // parse errors: wait (D8)
                    }

                    // T4 req 12 / D18: offer `murshid review` at commit
                    // detection — never auto-runs, just the one-line offer.
                    // Known behavior (accepted for v1, review fix 4): this
                    // fires on ANY HEAD change, including a branch switch
                    // (checkout/rebase), not just a genuine new commit —
                    // head_commit_changed can't distinguish the two from a
                    // bare hash comparison. Tolerated because the offer
                    // itself is harmless noise on a branch switch (one
                    // extra line, never auto-runs, no spend without the
                    // user explicitly following up).
                    {
                        let current_head = session::current_head_commit(&project_root_cb);
                        let previous_head = last_head_commit.lock().unwrap_or_else(|e| e.into_inner()).clone();
                        if session::head_commit_changed(previous_head.as_deref(), current_head.as_deref()) {
                            println!("[murshid] {}", review::REVIEW_OFFER_LINE);
                        }
                        *last_head_commit.lock().unwrap_or_else(|e| e.into_inner()) = current_head;
                    }

                    // Diagnostics-adapter check (D3 supporting signal / catch-up
                    // sweep trigger); the adapter is resolved from the active
                    // pack (I27) — this engine code never names a specific
                    // tool. Normalized records stay visible as plain lines in
                    // both modes (C6 degraded-mode requirement).
                    if let Ok(adapter) = pack::diagnostics_adapter(&pack_dir) {
                        if let Ok(output) = adapter.run_check(&project_root_cb, &path) {
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
                            for rec in &output.records {
                                println!(
                                    "[ERROR {}] in {} at line {}",
                                    rec.rule_id, file_path_str, rec.range.line_start
                                );
                                println!("Message: {}", rec.message);
                            }

                            // T3 reqs 7-11: `check_result` (C5) feeds both the
                            // same-error streak (signal 1) and the D15 baseline
                            // input (signal 2's percentile is computed from this
                            // history at session start); the primary code is the
                            // top-priority record (the adapter already
                            // prioritizes the active file first).
                            let primary_code = output.records.first().map(|r| r.rule_id.clone());
                            let now_ms = now
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_millis();
                            if let Some(ref conn) = conn_opt {
                                let _ = db::log_event(
                                    conn,
                                    &db::EventRecord {
                                        id: None,
                                        session_id: session_id_now.clone(),
                                        kind: "check_result".to_string(),
                                        payload_json: serde_json::json!({
                                            "success": output.success,
                                            "primary_code": primary_code,
                                            "ts_ms": now_ms,
                                        })
                                        .to_string(),
                                        ts: None,
                                    },
                                );
                            }
                            {
                                let mut st = struggle_tracking.lock().unwrap_or_else(|e| e.into_inner());
                                st.error_streak.observe(output.success, primary_code.as_deref());
                                st.red_streak.observe(output.success, now_ms);
                                st.last_check_success = Some(output.success);
                                if !output.success {
                                    st.struggle_site = Some(rel_path.clone());
                                }
                            }
                        }
                    }

                    if let judge::JudgeMode::Degraded { .. } = &mode {
                        // Observe-only: events above are already recorded; no LLM call.
                        pending_files.lock().unwrap_or_else(|e| e.into_inner()).clear();
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
                                screen_base_url.as_deref(),
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
                                judge_base_url.as_deref(),
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
                        pending_files.lock().unwrap_or_else(|e| e.into_inner()).iter().cloned().collect();
                    let (files_to_sweep, _retained_for_next_pass) =
                        cap_dispatch_batch(all_pending, MAX_STAGE1_DISPATCHES_PER_PASS);
                    let mut findings: Vec<aggregate::SweepFinding> = Vec::new();
                    // T4 req 1: files actually swept this pass — the input
                    // to the applied-detection site re-check below.
                    let mut swept_this_pass: Vec<std::path::PathBuf> = Vec::new();

                    for rel in files_to_sweep {
                        let abs = project_root_cb.join(&rel);
                        let sweep_content = match std::fs::read_to_string(&abs) {
                            Ok(c) => c,
                            Err(_) => {
                                // Unreadable/deleted: nothing to sweep, ever.
                                pending_files.lock().unwrap_or_else(|e| e.into_inner()).remove(&rel);
                                continue;
                            }
                        };
                        if !site::parses_without_errors(&sweep_content, &grammar) {
                            continue; // still broken: stays pending for the next pass
                        }
                        // Actually sweeping this file now — only here does it
                        // leave the pending set.
                        pending_files.lock().unwrap_or_else(|e| e.into_inner()).remove(&rel);
                        swept_this_pass.push(rel.clone());

                        let snap = snapshot.lock().unwrap_or_else(|e| e.into_inner()).clone();
                        let hunks =
                            match session::compute_session_diff(&project_root_cb, &rel, &snap) {
                                Ok(h) => h,
                                Err(_) => continue,
                            };
                        if hunks.is_empty() {
                            continue;
                        }
                        let rel_str = rel.to_string_lossy().to_string();

                        // T3 req 10 (signal 3): a fresh help-flavored
                        // comment is self-declared and local — scanned
                        // regardless of whether stage-1 dispatch below gets
                        // skipped as unchanged. Most-specific-first (repo
                        // convention): a `// murshid: ...?` line is a T4
                        // direct ask (below), not a fuzzy signal-3 struggle
                        // candidate — excluded here so it doesn't ALSO fire
                        // an offer for the same comment.
                        let fresh_help_comments = struggle::find_fresh_help_comments(
                            &hunks,
                            &surface.comment_token,
                            &surface.help_patterns,
                            &surface.on_hold_patterns,
                        );
                        let first_non_addressed_help_comment = fresh_help_comments
                            .into_iter()
                            .find(|body| comment::strip_address_token(body, &surface.address_token).is_none());
                        if let Some(snippet) = first_non_addressed_help_comment {
                            struggle_tracking.lock().unwrap_or_else(|e| e.into_inner()).help_candidate =
                                Some((rel.clone(), snippet));
                        }

                        // T4 reqs 9-11 / D17: murshid-addressed comments are
                        // a DIRECT ask — skip the offer AND screen stages
                        // entirely (pull-priced, EFP-exempt), answered as a
                        // normal card via stage-2 only, at this quiescence
                        // moment. Scanned regardless of the stage-1
                        // unchanged-dedup check below (that dedup is
                        // stage-1-specific).
                        for (comment_line, question) in comment::find_fresh_murshid_comments(
                            &hunks,
                            &surface.comment_token,
                            &surface.address_token,
                        ) {
                            let Some(site) = site::compute_site(&rel_str, &sweep_content, comment_line, &grammar)
                            else {
                                continue;
                            };
                            let comment_fp = comment::comment_advice_fingerprint(&question, &site);

                            // req 10 hygiene: answered comments never
                            // re-trigger; and never re-dispatch the exact
                            // same still-uncommitted comment twice in one
                            // session while it awaits an answer.
                            let already_answered = conn_opt
                                .as_ref()
                                .and_then(|c| db::find_ledger_card(c, &comment_fp).ok())
                                .flatten()
                                .is_some();
                            let already_known_this_session = conn_opt
                                .as_ref()
                                .map(|c| {
                                    db::card_exists_with_advice_fp(c, &session_id_now, &comment_fp)
                                        .unwrap_or(false)
                                })
                                .unwrap_or(false);
                            if already_answered || already_known_this_session {
                                continue;
                            }
                            let Some(ref conn) = conn_opt else { continue };

                            let enclosing_text =
                                site::enclosing_item_text(&sweep_content, comment_line, &grammar)
                                    .unwrap_or_else(|| sweep_content.clone());
                            let prompt =
                                comment::build_comment_ask_prompt(&question, &enclosing_text, &taxonomy);
                            let Ok(raw_text) = judge::safe_dispatch(|| {
                                provider::dispatch_debounced_with_model(
                                    &judge_provider,
                                    Some(&judge_model),
                                    &prompt,
                                    judge_key.as_deref(),
                                    judge_base_url.as_deref(),
                                )
                            }) else {
                                continue;
                            };
                            // C6 (amended): D17 comment-asks are consent-
                            // EXEMPT — the addressed comment IS the consent
                            // gesture — but the answer still carries an
                            // informational token note, no y/N gate.
                            let token_note = consent::token_note(
                                &judge_model,
                                consent::estimate_tokens(&prompt) + consent::estimate_tokens(&raw_text),
                            );
                            let Ok(parsed) = judge::parse_stage2_output(&raw_text) else {
                                continue;
                            };
                            let Ok(stage2_card) =
                                judge::validate_stage2_output(&parsed, &taxonomy, &sweep_content)
                            else {
                                continue;
                            };

                            let canon_entry =
                                pack::find_canon_for_concept(&canon, &stage2_card.concept);
                            let doc_ref = canon_entry
                                .and_then(|e| e.refs.first().cloned())
                                .unwrap_or_default();
                            let concept_name = taxonomy
                                .iter()
                                .find(|c| c.slug == stage2_card.concept)
                                .map(|c| c.name.clone())
                                .unwrap_or_else(|| stage2_card.concept.clone());
                            let ask_card = card::Card {
                                concept_name,
                                file: rel_str.clone(),
                                line: comment_line,
                                grounding_quote: stage2_card.grounding_quote.clone(),
                                why: stage2_card.why.clone(),
                                rule: stage2_card.rule.clone(),
                                doc_ref,
                                worked_diff: stage2_card.worked_diff.clone(),
                                additional_anchors: Vec::new(),
                                overflow_site_count: 0,
                            };
                            // T5 req 4: a direct ask is an explicit
                            // engagement — always resolves to SOME rung
                            // (never silenced).
                            let entry_rung = resolve_entry_rung(
                                conn,
                                &stage2_card.concept,
                                &stage2_card.category,
                                directness,
                            );

                            // Mutation-order safety: the new ask card's DB
                            // write must succeed BEFORE anything currently
                            // occupying the slot is evicted — an insert
                            // failure here must leave the existing pending
                            // card (if any) untouched, not lose it.
                            let Ok(card_id) = db::insert_card(
                                conn,
                                &db::CardRecord {
                                    id: None,
                                    session_id: session_id_now.clone(),
                                    concept_id: stage2_card.concept.clone(),
                                    category: db::COMMENT_ASK_CATEGORY.to_string(),
                                    rung_shown: entry_rung.as_str().to_string(),
                                    advice_fp: comment_fp.clone(),
                                    finding_fp: None,
                                    status: "shown".to_string(),
                                    created_ts: None,
                                    resolved_ts: None,
                                    worked_diff: Some(ask_card.worked_diff.clone()),
                                    regresses_card_id: None,
                                    site_file: Some(rel_str.clone()),
                                    site_line: Some(comment_line as i64),
                                },
                            ) else {
                                continue;
                            };

                            // req 11 / C7 slot contention: the new ask card
                            // is safely persisted now — a direct-ask answer
                            // owns the slot on arrival; a displaced pushed
                            // card returns to the queue head.
                            if let Some(displaced) = pending_card.lock().unwrap_or_else(|e| e.into_inner()).take() {
                                let _ = db::requeue_card(conn, displaced.card_id);
                                let seq = {
                                    let mut s = queue_seq.lock().unwrap_or_else(|e| e.into_inner());
                                    let v = *s;
                                    *s += 1;
                                    v
                                };
                                queue_state.lock().unwrap_or_else(|e| e.into_inner()).push(queue::QueueEntry {
                                    finding: aggregate::AggregatedFinding {
                                        concept_id: displaced.concept_id.clone(),
                                        category: displaced.category.clone(),
                                        advice_fp: displaced.advice_fp.clone(),
                                        card: displaced.card.clone(),
                                        likely_bug: false,
                                        strict_mode_passed: false,
                                        site_count: 1,
                                        remaining_sites: Vec::new(),
                                    },
                                    seq,
                                    throttled: false,
                                    card_id: displaced.card_id,
                                    session_id: displaced.session_id.clone(),
                                    pinned_head: true,
                                });
                            }

                            let _ = db::log_event(
                                conn,
                                &db::EventRecord {
                                    id: None,
                                    session_id: session_id_now.clone(),
                                    kind: "comment_ask".to_string(),
                                    payload_json: serde_json::json!({
                                        "question": question,
                                        "concept": stage2_card.concept,
                                    })
                                    .to_string(),
                                    ts: None,
                                },
                            );

                            // req 10: asking trumps prior suppression state
                            // (snooze tiers AND offer-declines) for this
                            // concept.
                            let _ = db::clear_suppressions_for_concept(
                                conn,
                                &session_id_now,
                                &stage2_card.concept,
                            );

                            println!(
                                "{}",
                                card::render_card_at_rung(
                                    &ask_card,
                                    entry_rung,
                                    0,
                                    &surface.comment_token
                                )
                            );
                            println!("  {}", comment::DELETE_COMMENT_NOTE);
                            println!("  {}", token_note);

                            *pending_card.lock().unwrap_or_else(|e| e.into_inner()) = Some(PendingCard {
                                card_id,
                                session_id: session_id_now.clone(),
                                concept_id: stage2_card.concept.clone(),
                                concept_name: ask_card.concept_name.clone(),
                                advice_fp: comment_fp,
                                category: db::COMMENT_ASK_CATEGORY.to_string(),
                                rung: entry_rung,
                                site_enclosing_item: Some(site.enclosing_item.clone()),
                                site_anchor_hash: Some(site.anchor_hash.clone()),
                                card: ask_card,
                            });
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

                        let outcome = pipeline::judge_hunks(
                            &rel_str,
                            &hunks,
                            &sweep_content,
                            &taxonomy,
                            &canon,
                            &grammar,
                            &prompts,
                            already_judged,
                            dispatch_stage1,
                            dispatch_stage2,
                        );

                        match outcome {
                            Ok(o) => {
                                // T5 req 3 / C6: stage-1's dual output —
                                // positive-application detections are
                                // independent evidence, processed
                                // regardless of whether a teaching-moment
                                // candidate also fired this pass.
                                if let Some(ref conn) = conn_opt {
                                    for (detection, det_line) in &o.application_detections {
                                        if !pack::is_valid_slug(&taxonomy, &detection.concept) {
                                            continue; // C2: what can't be named isn't taught
                                        }
                                        let Some(det_category) = taxonomy
                                            .iter()
                                            .find(|c| c.slug == detection.concept)
                                            .map(|c| c.category.clone())
                                        else {
                                            continue;
                                        };
                                        let Some(det_site) =
                                            site::compute_site(&rel_str, &sweep_content, *det_line, &grammar)
                                        else {
                                            continue;
                                        };
                                        let det_advice_fp =
                                            site::advice_fingerprint(&detection.concept, &det_site);
                                        // req 3's dual guard: below-mastery
                                        // AND no open card at this exact
                                        // site (avoids double-counting with
                                        // req 4's `hard` grade).
                                        let accepted = memory::detection_accepted(
                                            conn,
                                            &session_id_now,
                                            &detection.concept,
                                            &det_category,
                                            &det_advice_fp,
                                        )
                                        .unwrap_or(false);
                                        if !accepted {
                                            continue;
                                        }
                                        if let Ok(enc) = memory::record_encounter(
                                            conn,
                                            &session_id_now,
                                            &detection.concept,
                                            &det_category,
                                            bkt::Grade::Pass,
                                            "detection",
                                        ) {
                                            if enc.crossed_into_mastery {
                                                let name = taxonomy
                                                    .iter()
                                                    .find(|c| c.slug == detection.concept)
                                                    .map(|c| c.name.clone())
                                                    .unwrap_or_else(|| detection.concept.clone());
                                                println!(
                                                    "[murshid] backing off on {} \u{2014} applied {} times straight",
                                                    name, enc.row.pass_streak
                                                );
                                            }
                                        }
                                    }
                                }

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
                                        site::compute_site(&rel_str, &sweep_content, card.line, &grammar)
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

                                        // T5 req 3(b)/10: a stage-2-
                                        // validated finding on a PREVIOUSLY
                                        // TAUGHT concept is misuse evidence
                                        // (`fail`) — recorded REGARDLESS of
                                        // suppression/already-known. I23
                                        // separates mastery evidence from
                                        // noise-control state: whether the
                                        // user snoozed this concept, or a
                                        // card already exists for this exact
                                        // site, has no bearing on whether
                                        // the misuse actually happened in
                                        // their code. Only the CARD PUSH
                                        // below is gated by those two.
                                        if let Some(ref conn) = conn_opt {
                                            if db::concept_has_any_prior_card(conn, &stage2.concept)
                                                .unwrap_or(false)
                                            {
                                                if let Ok(enc) = memory::record_encounter(
                                                    conn,
                                                    &session_id_now,
                                                    &stage2.concept,
                                                    &stage2.category,
                                                    bkt::Grade::Fail,
                                                    "misuse",
                                                ) {
                                                    if enc.leveled_down {
                                                        println!(
                                                            "  {} needs another look \u{2014} cards are back",
                                                            card.concept_name
                                                        );
                                                    }
                                                }
                                            }
                                        }

                                        // T5 req 4: a mastered (silenced)
                                        // concept gets no new card even
                                        // though a finding was judged
                                        // (I18/C4: "concept mastered; no
                                        // card") — read fresh, AFTER the
                                        // fail evidence above may just have
                                        // dropped p below the gate.
                                        let silenced = conn_opt
                                            .as_ref()
                                            .map(|c| {
                                                memory::entry_rung_for(
                                                    c,
                                                    &stage2.concept,
                                                    &stage2.category,
                                                    directness,
                                                )
                                                .unwrap_or(Some(ladder::Rung::R2))
                                                .is_none()
                                            })
                                            .unwrap_or(false);

                                        if should_push_misuse_finding(suppressed, already_known, silenced) {
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

                    // T4 req 1 (gating fix): mechanical applied-detection —
                    // if the on-screen card's own file was just swept this
                    // pass, relocate its site by ENCLOSING-ITEM IDENTITY
                    // (stored item name + anchor hash), never by the
                    // possibly-stale `card.line` — an edit ABOVE the site
                    // shifts its line but not the item's identity or the
                    // anchor's own text, so this survives that (unlike the
                    // old line-pinned recompute, which falsely read
                    // "applied" in exactly that case).
                    {
                        let maybe_pc = pending_card.lock().unwrap_or_else(|e| e.into_inner()).clone();
                        if let Some(pc) = maybe_pc {
                            if let (Some(site_enclosing_item), Some(site_anchor_hash)) =
                                (pc.site_enclosing_item.as_ref(), pc.site_anchor_hash.as_ref())
                            {
                                let was_swept = swept_this_pass
                                    .iter()
                                    .any(|r| r.to_string_lossy() == pc.card.file);
                                if was_swept {
                                    let abs = project_root_cb.join(&pc.card.file);
                                    if let Ok(current_content) = std::fs::read_to_string(&abs) {
                                        let outcome = site::recheck_site_in_enclosing_item(
                                            &current_content,
                                            site_enclosing_item,
                                            site_anchor_hash,
                                            &grammar,
                                        );
                                        match outcome {
                                            site::SiteRecheckOutcome::Applied => {
                                                // T5 review fix 3: atomically
                                                // consume the pending-card
                                                // slot, gated on card_id —
                                                // the recheck above (file
                                                // read + parse) is a window
                                                // where a manual `a`
                                                // keystroke on the stdin
                                                // thread could resolve the
                                                // SAME card first. Whichever
                                                // path wins this compare-and-
                                                // clear is the only one that
                                                // records evidence; losing
                                                // here is a silent no-op,
                                                // never a double count.
                                                let consumed =
                                                    take_pending_card_if_matches(&pending_card, pc.card_id);
                                                if consumed {
                                                    if let Some(ref conn) = conn_opt {
                                                        let _ = db::update_card_status(
                                                            conn, pc.card_id, "applied",
                                                        );
                                                        let _ = db::log_event(
                                                            conn,
                                                            &db::EventRecord {
                                                                id: None,
                                                                session_id: session_id_now.clone(),
                                                                kind: "card_response".to_string(),
                                                                payload_json: serde_json::json!({
                                                                    "verb": "applied",
                                                                    "concept": pc.concept_id,
                                                                    "detected_by": "site_recheck",
                                                                })
                                                                .to_string(),
                                                                ts: None,
                                                            },
                                                        );
                                                        // T5 req 3(c):
                                                        // mechanical applied-
                                                        // detection is also
                                                        // `hard` evidence —
                                                        // help was shown,
                                                        // then the flagged
                                                        // pattern was fixed.
                                                        let real_category = taxonomy
                                                            .iter()
                                                            .find(|c| c.slug == pc.concept_id)
                                                            .map(|c| c.category.clone())
                                                            .unwrap_or_else(|| pc.category.clone());
                                                        if let Ok(enc) = memory::record_encounter(
                                                            conn,
                                                            &session_id_now,
                                                            &pc.concept_id,
                                                            &real_category,
                                                            bkt::Grade::Hard,
                                                            "applied",
                                                        ) {
                                                            if enc.crossed_into_mastery {
                                                                println!(
                                                                    "[murshid] backing off on {} \u{2014} applied {} times straight",
                                                                    pc.concept_name, enc.row.pass_streak
                                                                );
                                                            }
                                                        }
                                                    }
                                                    println!(
                                                        "  applied \u{2014} nice, {} flips to applied",
                                                        pc.concept_name
                                                    );
                                                }
                                            }
                                            site::SiteRecheckOutcome::ItemGone => {
                                                // C2: an item rename retires
                                                // the site — NOT evidence of
                                                // a fix; expire, don't
                                                // falsely credit "applied".
                                                // Same compare-and-clear
                                                // race guard as `Applied`
                                                // above (fix 3) — a manual
                                                // response in the same
                                                // window must win outright,
                                                // never get overwritten here.
                                                let consumed =
                                                    take_pending_card_if_matches(&pending_card, pc.card_id);
                                                if consumed {
                                                    if let Some(ref conn) = conn_opt {
                                                        let _ = db::update_card_status(
                                                            conn, pc.card_id, "expired",
                                                        );
                                                        let _ = db::log_event(
                                                            conn,
                                                            &db::EventRecord {
                                                                id: None,
                                                                session_id: session_id_now.clone(),
                                                                kind: "card_response".to_string(),
                                                                payload_json: serde_json::json!({
                                                                    "verb": "expired",
                                                                    "concept": pc.concept_id,
                                                                    "detected_by": "site_recheck_item_gone",
                                                                })
                                                                .to_string(),
                                                                ts: None,
                                                            },
                                                        );
                                                    }
                                                }
                                            }
                                            site::SiteRecheckOutcome::StillPresent => {}
                                        }
                                    }
                                }
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
                            let queued_rung =
                                resolve_entry_rung(conn, &agg.concept_id, &agg.category, directness);
                            let Ok(card_id) = db::insert_card(
                                conn,
                                &db::CardRecord {
                                    id: None,
                                    session_id: session_id_now.clone(),
                                    concept_id: agg.concept_id.clone(),
                                    category: agg.category.clone(),
                                    rung_shown: queued_rung.as_str().to_string(),
                                    advice_fp: agg.advice_fp.clone(),
                                    finding_fp: None,
                                    status: "queued".to_string(),
                                    created_ts: None,
                                    resolved_ts: None,
                                    worked_diff: Some(agg.card.worked_diff.clone()),
                                    regresses_card_id,
                                    site_file: Some(agg.card.file.clone()),
                                    site_line: Some(agg.card.line as i64),
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
                                let mut s = queue_seq.lock().unwrap_or_else(|e| e.into_inner());
                                let v = *s;
                                *s += 1;
                                v
                            };
                            queue_state.lock().unwrap_or_else(|e| e.into_inner()).push(queue::QueueEntry {
                                finding: agg.clone(),
                                seq,
                                throttled: throttled_flag,
                                card_id,
                                session_id: session_id_now.clone(),
                                pinned_head: false,
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
                            throttled_categories.lock().unwrap_or_else(|e| e.into_inner()).contains(&agg.category);
                        let floor_excluded = noise::floor_excludes(&detent, &agg.category);

                        // Review fix: single-slot guard — never show a
                        // second card while an earlier one (this pass OR an
                        // earlier pass) is still awaiting a response. Checked
                        // fresh every iteration (not a one-time snapshot) so
                        // a concurrent pull via `m` is also respected.
                        let pending_card_present = pending_card.lock().unwrap_or_else(|e| e.into_inner()).is_some();
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
                            let mut b = bucket.lock().unwrap_or_else(|e| e.into_inner());
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

                                let shown_rung = resolve_entry_rung(
                                    conn,
                                    &agg.concept_id,
                                    &agg.category,
                                    directness,
                                );
                                println!(
                                    "{}",
                                    card::render_card_at_rung(&agg.card, shown_rung, 0, &surface.comment_token)
                                );
                                if let Ok(card_id) = db::insert_card(
                                    conn,
                                    &db::CardRecord {
                                        id: None,
                                        session_id: session_id_now.clone(),
                                        concept_id: agg.concept_id.clone(),
                                        category: agg.category.clone(),
                                        rung_shown: shown_rung.as_str().to_string(),
                                        advice_fp: agg.advice_fp.clone(),
                                        finding_fp: None,
                                        status: "shown".to_string(),
                                        created_ts: None,
                                        resolved_ts: None,
                                        worked_diff: Some(agg.card.worked_diff.clone()),
                                        regresses_card_id,
                                        site_file: Some(agg.card.file.clone()),
                                        site_line: Some(agg.card.line as i64),
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
                                    // req 1: derive the STORED site identity
                                    // (enclosing item + anchor hash) fresh —
                                    // the aggregated finding only carries the
                                    // opaque advice_fp, not the Site struct.
                                    let (site_enclosing_item, site_anchor_hash) =
                                        std::fs::read_to_string(
                                            project_root_cb.join(&agg.card.file),
                                        )
                                        .ok()
                                        .and_then(|c| {
                                            site::compute_site(&agg.card.file, &c, agg.card.line, &grammar)
                                        })
                                        .map(|s| (Some(s.enclosing_item), Some(s.anchor_hash)))
                                        .unwrap_or((None, None));
                                    *pending_card.lock().unwrap_or_else(|e| e.into_inner()) = Some(PendingCard {
                                        card_id,
                                        session_id: session_id_now.clone(),
                                        concept_id: agg.concept_id.clone(),
                                        concept_name: agg.card.concept_name.clone(),
                                        advice_fp: agg.advice_fp.clone(),
                                        category: agg.category.clone(),
                                        rung: shown_rung,
                                        site_enclosing_item,
                                        site_anchor_hash,
                                        card: agg.card.clone(),
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
                    let queue_len = queue_state.lock().unwrap_or_else(|e| e.into_inner()).len();
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
                site_file: None,
                site_line: None,
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

    // --- T5 review fix 2: misuse-fail decoupling (req 3(b)/10) ---

    #[test]
    fn test_should_push_misuse_finding_requires_all_three_clear() {
        assert!(should_push_misuse_finding(false, false, false));
        assert!(!should_push_misuse_finding(true, false, false), "suppressed blocks the push");
        assert!(!should_push_misuse_finding(false, true, false), "already-known blocks the push");
        assert!(!should_push_misuse_finding(false, false, true), "silenced blocks the push");
    }

    /// Acceptance: "snoozed concept misused ⇒ p drops, no card." Mirrors
    /// the exact sequence main.rs runs on a stage-2-validated finding: fail
    /// evidence is recorded UNCONDITIONALLY (I23 — mastery state is
    /// independent of noise-control state), then the push decision is
    /// gated separately by suppression.
    #[test]
    fn test_snoozed_concept_misused_records_evidence_but_never_pushes_a_card() {
        let conn = db::initialize_db(":memory:").unwrap();
        let concept = "borrow-vs-clone";

        // "Previously taught" (req 3(b)'s precondition) + a real prior
        // encounter so there's a p to observe dropping.
        db::insert_card(
            &conn,
            &db::CardRecord {
                id: None,
                session_id: "sess1".to_string(),
                concept_id: concept.to_string(),
                category: "idiom".to_string(),
                rung_shown: "R2".to_string(),
                advice_fp: "fp-old".to_string(),
                finding_fp: None,
                status: "applied".to_string(),
                created_ts: None,
                resolved_ts: None,
                worked_diff: None,
                regresses_card_id: None,
                site_file: None,
                site_line: None,
            },
        )
        .unwrap();
        memory::record_encounter(&conn, "sess1", concept, "idiom", bkt::Grade::Pass, "detection")
            .unwrap();
        let before = db::get_concept_memory(&conn, concept).unwrap().unwrap();

        // The concept is snoozed (concept-scope) this session — the D11
        // noise-control mechanism, unrelated to mastery.
        db::insert_suppression(&conn, "sess1", concept, concept, "concept").unwrap();
        let suppressed = db::is_suppressed(&conn, "sess1", concept, "fp-new").unwrap();
        assert!(suppressed);

        // main.rs's exact sequence: fail evidence recorded regardless.
        assert!(db::concept_has_any_prior_card(&conn, concept).unwrap());
        let outcome =
            memory::record_encounter(&conn, "sess1", concept, "idiom", bkt::Grade::Fail, "misuse")
                .unwrap();
        assert!(outcome.row.p_mastery < before.p_mastery, "p must drop from the fail");

        let already_known = false; // a fresh finding this pass
        let silenced = false; // nowhere near mastery here
        assert!(
            !should_push_misuse_finding(suppressed, already_known, silenced),
            "a snoozed concept must never get a new card even though evidence was recorded"
        );
    }

    // --- T5 review fix 3: hard-evidence double-count race guard ---

    fn pending_card_with_id(card_id: i64) -> PendingCard {
        PendingCard {
            card_id,
            session_id: "sess1".to_string(),
            concept_id: "borrow-vs-clone".to_string(),
            concept_name: "Borrow vs. clone".to_string(),
            advice_fp: "fp-1".to_string(),
            category: "idiom".to_string(),
            rung: ladder::Rung::R2,
            card: sample_card(),
            site_enclosing_item: Some("fn foo".to_string()),
            site_anchor_hash: Some("hash".to_string()),
        }
    }

    #[test]
    fn test_take_pending_card_if_matches_consumes_on_match() {
        let slot = std::sync::Mutex::new(Some(pending_card_with_id(5)));
        assert!(take_pending_card_if_matches(&slot, 5));
        assert!(
            slot.lock().unwrap().is_none(),
            "a matching take must clear the slot"
        );
    }

    /// Acceptance: the mechanical applied-detection path and a manual `a`
    /// keystroke racing it in the same file-I/O window can never BOTH
    /// record evidence for the same card — whichever compare-and-clear
    /// runs first wins, the second sees a mismatch (or an empty slot) and
    /// does nothing.
    #[test]
    fn test_take_pending_card_if_matches_loses_when_already_consumed() {
        let slot = std::sync::Mutex::new(Some(pending_card_with_id(5)));
        // Simulates the manual `a` key winning the race first.
        assert!(take_pending_card_if_matches(&slot, 5));
        // The mechanical path's own attempt on the SAME card_id, arriving
        // second, must lose — not record evidence a second time.
        assert!(!take_pending_card_if_matches(&slot, 5));
    }

    #[test]
    fn test_take_pending_card_if_matches_never_clobbers_a_different_card() {
        // A different card now occupies the slot (e.g. the next sweep
        // already pushed a new one) — a stale mechanical-detection attempt
        // for the OLD card_id must not steal or clear the new one.
        let slot = std::sync::Mutex::new(Some(pending_card_with_id(7)));
        assert!(!take_pending_card_if_matches(&slot, 5));
        assert_eq!(
            slot.lock().unwrap().as_ref().map(|p| p.card_id),
            Some(7),
            "a mismatched take must never clear an unrelated pending card"
        );
    }

    #[test]
    fn test_take_pending_card_if_matches_on_empty_slot_is_false() {
        let slot: std::sync::Mutex<Option<PendingCard>> = std::sync::Mutex::new(None);
        assert!(!take_pending_card_if_matches(&slot, 5));
    }
}
