//! In-pane keystroke handling for the watch loop: card lifecycle responses
//! (applied/got-it/not-now/not-useful), rung escalation, follow-up threads,
//! queue browsing, and goal edits. Each keystroke maps to a C3 response and
//! its persisted event.

use std::path::Path;
use std::sync::Arc;

use crate::{
    budget, card, consent, db, goal, judge, ladder, memory, offer, pack, pipeline, provider, queue,
    response, session, site, suppression, thread,
};

use super::{PendingCard, WatchSession};

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
    project_root: &Path,
    site_file: &Path,
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

    let already_judged = |fp: &str| -> bool {
        db::card_exists_with_advice_fp(conn, session_id, fp).unwrap_or(false)
    };
    // T11 req 2/4: struggle judge is a user-initiated call — Interactive
    // lane, never aborted by a concurrent Sweep dispatch.
    let dispatch_stage1 = |prompt: &str| -> Result<String, String> {
        judge::safe_dispatch(|| {
            provider::dispatch_debounced_with_model(
                provider::Lane::Interactive,
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
                provider::Lane::Interactive,
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
    let entry_rung = super::resolve_entry_rung(conn, &stage2.concept, &stage2.category, directness);

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

    // T11 req 2/4: a thread turn is user-initiated — Interactive lane.
    let raw = judge::safe_dispatch(|| {
        provider::dispatch_debounced_with_model(
            provider::Lane::Interactive,
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

/// req 3/8/10/11: non-blocking (relative to the watcher) stdin reader —
/// g/u/n resolve the single pending card; `m` browses the pull queue; a
/// number selects a queued item into the slot; `g` with no pending card
/// opens $EDITOR on the goal file (req 2); y/[anything else] resolves a
/// pending struggle offer (req 11). Moved verbatim off `main()`'s inline
/// stdin-reader closure (T12).
#[allow(clippy::too_many_arguments)]
pub fn run_stdin_loop(
    ws: &Arc<WatchSession>,
    project_root: &Path,
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
    directness: ladder::Directness,
    surface: &pack::SurfaceConfig,
    consent_setting: &str,
) {
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
        let maybe_offer = ws
            .pending_offer
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if let Some(po) = maybe_offer {
            let action = offer::classify_offer_key(trimmed);
            if action != offer::OfferKeyAction::Ignore {
                *ws.pending_offer.lock().unwrap_or_else(|e| e.into_inner()) = None;
                let sid = ws
                    .session_mgr
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .session_id
                    .clone();
                let Some(dp) = db::get_db_path() else {
                    continue;
                };
                let Ok(conn) = db::open_connection(&dp) else {
                    continue;
                };

                if action == offer::OfferKeyAction::Accept {
                    db::warn_on_err(
                        db::update_card_status(&conn, po.card_id, "applied"),
                        "update_card_status",
                    );
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
                    if ws
                        .pending_card
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .is_none()
                    {
                        let snap = ws
                            .snapshot
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .clone();
                        match run_struggle_judge_and_show(
                            &conn,
                            &sid,
                            project_root,
                            &po.site_file,
                            &snap,
                            taxonomy,
                            canon,
                            grammar,
                            prompts,
                            screen_provider,
                            screen_model,
                            screen_key,
                            screen_base_url,
                            judge_provider,
                            judge_model,
                            judge_key,
                            judge_base_url,
                            &ws.bucket,
                            directness,
                            &surface.comment_token,
                        ) {
                            Some(pc) => {
                                *ws.pending_card.lock().unwrap_or_else(|e| e.into_inner()) =
                                    Some(pc);
                            }
                            None => println!("  nothing new to show at that site right now"),
                        }
                    }
                } else {
                    // Explicit "n" only (review fix —
                    // Ignore never reaches here).
                    db::warn_on_err(
                        db::update_card_status(&conn, po.card_id, "not_now"),
                        "update_card_status",
                    );
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
                    let declines =
                        db::count_declined_offers_for_concept(&conn, &po.key.1).unwrap_or(0);
                    if offer::should_suppress_after_declines(declines) {
                        let expiry =
                            offer::suppression_expiry_epoch_secs(std::time::SystemTime::now());
                        db::warn_on_err(
                            db::insert_offer_suppression(&conn, &sid, &po.key.1, expiry),
                            "insert_offer_suppression",
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
            && ws
                .pending_card
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_none()
        {
            let path = goal::goal_file_path(project_root);
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".to_string());
            let _ = std::process::Command::new(editor).arg(&path).status();
            continue;
        }

        if trimmed.eq_ignore_ascii_case("m") {
            let mut q = ws.queue_state.lock().unwrap_or_else(|e| e.into_inner());
            let cluster = ws
                .goal_cluster_dirs
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            let goal_text = crate::goal_text_now(project_root);
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
            if consent::should_prompt_for_review(consent_setting) {
                let estimate =
                    consent::estimate_tokens(&crate::goal_text_now(project_root)).max(500); // a full-diff pass is the expensive call
                println!(
                    "  {}",
                    consent::consent_prompt_line("murshid review", estimate)
                );
                let Some(confirm_line) = lines_iter.next() else {
                    continue;
                };
                if offer::classify_offer_key(confirm_line.trim()) != offer::OfferKeyAction::Accept {
                    println!("  okay, skipped");
                    continue;
                }
            }

            let goal_text = crate::goal_text_now(project_root);
            let cluster = ws
                .goal_cluster_dirs
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            let snap = ws
                .snapshot
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            let review_conn = db::get_db_path().and_then(|dp| db::open_connection(&dp).ok());
            let digest = crate::run_review(
                project_root,
                &snap,
                taxonomy,
                canon,
                grammar,
                prompts,
                &goal_text,
                &cluster,
                screen_provider,
                screen_model,
                screen_key,
                screen_base_url,
                judge_provider,
                judge_model,
                judge_key,
                judge_base_url,
                review_conn.as_ref(),
            );

            if let Some(conn) = review_conn.as_ref() {
                let sid_for_review = ws
                    .session_mgr
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .session_id
                    .clone();
                crate::persist_review_digest(conn, &sid_for_review, &digest);
            }

            print!("{}", crate::review::render_review_digest(&digest));
            continue;
        }

        if let Ok(choice) = trimmed.parse::<usize>() {
            if choice == 0 {
                continue;
            }
            if ws
                .pending_card
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_some()
            {
                println!("  finish the current card first (g/u/n), then pick again");
                continue;
            }
            let mut q = ws.queue_state.lock().unwrap_or_else(|e| e.into_inner());
            let cluster = ws
                .goal_cluster_dirs
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            let goal_text = crate::goal_text_now(project_root);
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
                db::warn_on_err(
                    db::update_card_status(&conn, entry.card_id, "collapsed"),
                    "update_card_status",
                );
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
                println!("  that one's already settled \u{2014} press m to see what's left");
                continue;
            }

            // req 7 fix: this concept is shipping now —
            // collapse any remaining siblings still in
            // the queue into it.
            let mut shown_card = entry.finding.card.clone();
            let extra_anchors = super::collapse_queued_siblings(
                &conn,
                &ws.queue_state,
                &entry.session_id,
                &entry.finding.concept_id,
            );
            if !extra_anchors.is_empty() {
                let _ = super::fold_anchors_into_card(&mut shown_card, extra_anchors);
            }

            db::warn_on_err(
                db::update_card_status(&conn, entry.card_id, "shown"),
                "update_card_status",
            );
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
            let pulled_rung = super::resolve_entry_rung(
                &conn,
                &entry.finding.concept_id,
                &entry.finding.category,
                directness,
            );
            db::warn_on_err(
                db::update_card_rung(&conn, entry.card_id, pulled_rung.as_str()),
                "update_card_rung",
            );
            println!(
                "{}",
                card::render_card_at_rung(&shown_card, pulled_rung, 0, &surface.comment_token)
            );
            // req 1: re-derive the STORED site identity
            // (enclosing item + anchor hash) for the
            // applied-detection re-check — a queued card
            // never had a live `Site` object retained.
            let (site_enclosing_item, site_anchor_hash) = super::derive_site_identity(
                project_root,
                &shown_card.file,
                shown_card.line,
                grammar,
            );
            *ws.pending_card.lock().unwrap_or_else(|e| e.into_inner()) = Some(PendingCard {
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

            response::CardKeyAction::Escalate | response::CardKeyAction::TellMe => {
                let maybe_pc = ws
                    .pending_card
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone();
                let Some(pc) = maybe_pc else { continue };
                let Some(dp) = db::get_db_path() else {
                    continue;
                };
                let Ok(conn) = db::open_connection(&dp) else {
                    continue;
                };

                let escalate =
                    response::classify_card_key(trimmed) == response::CardKeyAction::Escalate;
                let new_rung = if escalate {
                    ladder::escalate_one(pc.rung)
                } else {
                    ladder::tell_me(pc.rung)
                };

                // req 3/4: every step (and every reveal,
                // even a repeat `t` at R3) is logged —
                // click-through gaming must be visible.
                db::warn_on_err(
                    db::update_card_rung(&conn, pc.card_id, new_rung.as_str()),
                    "update_card_rung",
                );
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
                    card::render_card_at_rung(&pc.card, new_rung, 0, &surface.comment_token)
                );
                let mut updated = pc;
                updated.rung = new_rung;
                *ws.pending_card.lock().unwrap_or_else(|e| e.into_inner()) = Some(updated);
            }

            response::CardKeyAction::Ask => {
                let maybe_pc = ws
                    .pending_card
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone();
                let Some(pc) = maybe_pc else { continue };
                let sid = ws
                    .session_mgr
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .session_id
                    .clone();
                let Some(dp) = db::get_db_path() else {
                    continue;
                };
                let Ok(conn) = db::open_connection(&dp) else {
                    continue;
                };

                // req 5 / C12: the 5-user-turn cap.
                let turns_so_far = db::thread_user_turn_count(&conn, pc.card_id).unwrap_or(0);
                if thread::thread_cap_reached(turns_so_far) {
                    println!("  {}", thread::THREAD_CAP_NOTICE);
                    continue;
                }

                println!("  ask \u{2014} type your question:");
                let Some(question_line) = lines_iter.next() else {
                    continue;
                };
                let question = question_line.trim().to_string();
                if question.is_empty() {
                    continue;
                }

                // req 8 / C6 BYOK consent: first thread
                // turn per session confirms under `ask`.
                let already_confirmed = *ws
                    .thread_consent_confirmed
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                if consent::should_prompt_for_thread(consent_setting, already_confirmed) {
                    let estimate = consent::estimate_tokens(&question);
                    println!(
                        "  {}",
                        consent::consent_prompt_line("this thread turn", estimate)
                    );
                    let Some(confirm_line) = lines_iter.next() else {
                        continue;
                    };
                    if offer::classify_offer_key(confirm_line.trim())
                        != offer::OfferKeyAction::Accept
                    {
                        println!("  okay, skipped");
                        continue;
                    }
                    *ws.thread_consent_confirmed
                        .lock()
                        .unwrap_or_else(|e| e.into_inner()) = true;
                }

                match run_thread_turn(
                    &conn,
                    &sid,
                    &pc,
                    &question,
                    canon,
                    judge_provider,
                    judge_model,
                    judge_key,
                    judge_base_url,
                ) {
                    Some(rendered) => {
                        println!("{}", rendered);
                        let new_count = db::thread_user_turn_count(&conn, pc.card_id).unwrap_or(0);
                        if thread::thread_cap_reached(new_count) {
                            println!("  {}", thread::THREAD_CAP_NOTICE);
                        }
                    }
                    None => println!("  (no answer \u{2014} degraded mode or provider error)"),
                }
            }

            response::CardKeyAction::Response(verb) => {
                let maybe_pc = ws
                    .pending_card
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .take();
                let Some(pc) = maybe_pc else { continue };
                let Some(dp) = db::get_db_path() else {
                    continue;
                };
                let Ok(conn) = db::open_connection(&dp) else {
                    continue;
                };
                db::warn_on_err(
                    db::update_card_status(&conn, pc.card_id, verb),
                    "update_card_status",
                );

                // T5 req 3(c)/10: `applied` (manual `a`)
                // is the ONLY response verb that is
                // evidence — got_it/not_now/not_useful
                // are dismissals (I23), never mastery
                // signal. Guarded by the single testable
                // source of truth in memory.rs so a
                // future new verb can't silently become
                // evidence by accident.
                if let Some(grade) = memory::should_record_evidence_for_response(verb) {
                    // A D17 comment-ask card's `category`
                    // field holds the pseudo-category
                    // `comment-ask` (bookkeeping only) —
                    // resolve the concept's REAL
                    // taxonomy category for BKT priors.
                    let real_category = taxonomy
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
                            db::warn_on_err(
                                db::insert_suppression(
                                    &conn,
                                    &pc.session_id,
                                    &pc.concept_id,
                                    &pc.advice_fp,
                                    "instance",
                                ),
                                "insert_suppression(instance)",
                            );
                        }
                        suppression::SnoozeScope::Concept => {
                            db::warn_on_err(
                                db::insert_suppression(
                                    &conn,
                                    &pc.session_id,
                                    &pc.concept_id,
                                    &pc.concept_id,
                                    "concept",
                                ),
                                "insert_suppression(concept)",
                            );
                            widened = true;
                            println!("  {}", suppression::widening_notice(&pc.concept_name));
                        }
                    }
                    db::warn_on_err(
                        db::enforce_suppression_cap(&conn, &pc.session_id),
                        "enforce_suppression_cap",
                    );
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
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
