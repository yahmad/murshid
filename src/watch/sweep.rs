//! The file-event sweep: the watcher-callback body that re-diffs every file
//! touched since it was last judged, runs the two-stage judge, aggregates
//! findings by concept, and either auto-pushes (budget-gated) or queues each
//! card — plus the drift, struggle, comment-ask, and applied-detection
//! signals that ride the same quiescence-gated pass.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::{
    aggregate, bkt, budget, card, comment, db, diff, goal, judge, ladder, memory, noise, pack,
    pipeline, provider, queue, quiescence, review, session, site,
};

use super::{
    DriftTracking, LastReview, PendingCard, ReviewResult, ReviewState, StruggleTracking,
    WatchSession,
};
use crate::sync_ext::LockExt;

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

/// T5 req 3(b)/10 fix: whether a stage-2-validated finding should PUSH/
/// QUEUE a card. Deliberately separate from evidence recording (`fail`
/// evidence is recorded unconditionally, per I23 — mastery state and
/// noise-control state are independent axes) — this function is only ever
/// consulted for the card-presentation decision, never for whether the
/// misuse itself gets recorded in the memory model.
fn should_push_misuse_finding(suppressed: bool, already_known: bool, silenced: bool) -> bool {
    !suppressed && !already_known && !silenced
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
    let mut guard = slot.lock_poison_safe();
    let matches = guard.as_ref().map(|p| p.card_id) == Some(expected_card_id);
    if matches {
        *guard = None;
    }
    matches
}

/// C2 session split on idle gap > 4h: expires the OLD session's unresolved
/// cards (req 10 / C3 "no interaction by session end ⇒ expired") before
/// rotating the snapshot to the new one, renders the T3 req 6 bookend, and
/// (on split) re-resolves the goal / recomputes the struggle baseline and
/// throttle state fresh for the new session (C12/C5). Returns the
/// (possibly just-rotated) current session id — needed by every event the
/// rest of the sweep logs, split or not.
fn handle_session_split(
    ws: &Arc<WatchSession>,
    now: std::time::SystemTime,
    project_root: &Path,
    taxonomy: &[pack::TaxonomyConcept],
    conn_opt: &Option<rusqlite::Connection>,
    unthrottle: &[String],
) -> String {
    let old_session_id = ws.session_mgr.lock_poison_safe().session_id.clone();
    let split = ws.session_mgr.lock_poison_safe().on_file_event(now);
    let session_id_now = ws.session_mgr.lock_poison_safe().session_id.clone();
    if split {
        if let Some(conn) = conn_opt {
            let expired = db::expire_unresolved_cards(conn, &old_session_id).unwrap_or(0);
            // req 3/8 / C2: the pull queue and (non-offer-
            // concept) snoozes die at session end.
            db::warn_on_err(
                db::purge_suppressions_for_session(conn, &old_session_id),
                "purge_suppressions_for_session",
            );
            // T3 req 6: the bookend renders at every session
            // end, not just process exit.
            let b = super::assemble_session_bookend(
                conn,
                &old_session_id,
                project_root,
                taxonomy,
                &ws.queue_state,
                &ws.goal_cluster_dirs,
                &ws.throttled_categories,
            );
            let _ = db::log_event(
                conn,
                &db::EventRecord {
                    id: None,
                    session_id: old_session_id.clone(),
                    kind: "session_end".to_string(),
                    payload_json: super::bookend_event_payload(&b, expired).to_string(),
                    ts: None,
                },
            );
            // T9 req 2: snapshot progress at the session
            // bookend, not just after migrations.
            if let Err(e) = db::save_backup_from_db(conn) {
                ws.notice(format!(
                    "Warning: failed to save progress backup at session end: {}",
                    e
                ));
            }
            ws.notice(crate::bookend::render_bookend(&b));
        }
        ws.queue_state.lock_poison_safe().clear();
        ws.dispatched_hunk_signatures.lock_poison_safe().clear();
        *ws.pending_card.lock_poison_safe() = None;
        *ws.pending_offer.lock_poison_safe() = None;
        *ws.drift_tracking.lock_poison_safe() = DriftTracking::default();
        *ws.struggle_tracking.lock_poison_safe() = StruggleTracking::default();
        *ws.snapshot.lock_poison_safe() =
            session::snapshot_session_start(project_root).unwrap_or_default();

        // req 1/3: re-resolve the goal at this natural
        // boundary (never overwrites a hand edit).
        {
            let changed_files: Vec<std::path::PathBuf> = ws
                .snapshot
                .lock_poison_safe()
                .files
                .keys()
                .cloned()
                .collect();
            super::resolve_and_announce_goal(
                project_root,
                &changed_files,
                ws,
                false,
                |t| {
                    if let Some(conn) = conn_opt {
                        let _ = db::log_event(
                            conn,
                            &db::EventRecord {
                                id: None,
                                session_id: session_id_now.clone(),
                                kind: "goal_inferred".to_string(),
                                payload_json: serde_json::json!({ "text": t }).to_string(),
                                ts: None,
                            },
                        );
                    }
                },
            );
        }

        if let Some(conn) = conn_opt {
            // T3 req 8: recompute the baseline fresh at
            // every session start (C12).
            let points = db::all_check_result_points(conn).unwrap_or_default();
            let durations = crate::struggle::time_to_green_durations_ms(&points);
            ws.struggle_tracking.lock_poison_safe().baseline_ms =
                crate::struggle::percentile_75_ms(&durations);

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
            *ws.throttled_categories.lock_poison_safe() =
                super::compute_throttle_state(conn, ws, &session_id_now, unthrottle);
        }
    }
    session_id_now
}

/// Diagnostics-adapter check (D3 supporting signal / catch-up sweep
/// trigger); the adapter is resolved from the active pack (I27) — this
/// engine code never names a specific tool. Normalized records stay
/// visible as plain lines in both modes (C6 degraded-mode requirement).
/// Also feeds the T3 reqs 7-11 `check_result` event and struggle-streak
/// observations (same-error streak / D15 baseline input).
#[allow(clippy::too_many_arguments)]
// Arity here is inherent per-sweep coordinator state (paths/conn/session/
// directness/detent + the pack payloads it forwards), not a deferred bundle —
// the allow stays.
fn run_diagnostics_check(
    ws: &Arc<WatchSession>,
    project_root: &Path,
    pack_dir: &Path,
    path: &Path,
    project_root_str: &str,
    file_path_str: &str,
    conn_opt: &Option<rusqlite::Connection>,
    session_id_now: &str,
    now: std::time::SystemTime,
    rel_path: &Path,
) {
    if let Ok(adapter) = pack::diagnostics_adapter(pack_dir) {
        if let Ok(output) = adapter.run_check(project_root, path) {
            if let Some(conn) = conn_opt {
                let check_event = db::HistoryEvent {
                    id: None,
                    event_type: "compiler_check".to_string(),
                    project_root: project_root_str.to_string(),
                    file_path: file_path_str.to_string(),
                    success: Some(output.success),
                    error_code: None,
                    error_message: None,
                    line_number: None,
                    created_at: None,
                };
                let _ = db::log_history_event(conn, &check_event);
            }
            for rec in &output.records {
                ws.notice(format!(
                    "[ERROR {}] in {} at line {}\nMessage: {}",
                    rec.rule_id, file_path_str, rec.range.line_start, rec.message
                ));
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
            if let Some(conn) = conn_opt {
                let _ = db::log_event(
                    conn,
                    &db::EventRecord {
                        id: None,
                        session_id: session_id_now.to_string(),
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
                let mut st = ws.struggle_tracking.lock_poison_safe();
                st.error_streak
                    .observe(output.success, primary_code.as_deref());
                st.red_streak.observe(output.success, now_ms);
                st.last_check_success = Some(output.success);
                if !output.success {
                    st.struggle_site = Some(rel_path.to_path_buf());
                }
            }
        }
    }
}

/// T3 req 10 (signal 3) / T4 reqs 9-11 (D17): scans the hunks for a fresh
/// help-flavored comment (recorded as this pass's struggle candidate) and
/// for murshid-addressed comments — a DIRECT ask that skips the offer AND
/// screen stages entirely (pull-priced, EFP-exempt), answered as a normal
/// card via stage-2 only, at this quiescence moment. Both scans run
/// regardless of the stage-1 unchanged-dedup check the caller applies
/// afterward (that dedup is stage-1-specific). Most-specific-first (repo
/// convention): a `// murshid: ...?` line is the T4 direct ask, not a
/// fuzzy signal-3 struggle candidate — excluded from the help-comment scan
/// so it doesn't ALSO fire an offer for the same comment.
#[allow(clippy::too_many_arguments)]
// Arity here is inherent per-sweep coordinator state (paths/conn/session/
// directness/detent + the pack payloads it forwards), not a deferred bundle —
// the allow stays.
fn run_comment_asks(
    ws: &Arc<WatchSession>,
    rel: &Path,
    rel_str: &str,
    sweep_content: &str,
    hunks: &[diff::Hunk],
    grammar: &pack::GrammarSpec,
    surface: &pack::SurfaceConfig,
    taxonomy: &[pack::TaxonomyConcept],
    canon: &[pack::CanonEntry],
    judge_slot: &crate::ResolvedSlot,
    session_id_now: &str,
    conn_opt: &Option<rusqlite::Connection>,
    directness: ladder::Directness,
) {
    let fresh_help_comments = crate::struggle::find_fresh_help_comments(
        hunks,
        &surface.comment_token,
        &surface.help_patterns,
        &surface.on_hold_patterns,
    );
    let first_non_addressed_help_comment = fresh_help_comments
        .into_iter()
        .find(|body| comment::strip_address_token(body, &surface.address_token).is_none());
    if let Some(snippet) = first_non_addressed_help_comment {
        ws.struggle_tracking.lock_poison_safe().help_candidate = Some((rel.to_path_buf(), snippet));
    }

    // T4 reqs 9-11 / D17: murshid-addressed comments are
    // a DIRECT ask — skip the offer AND screen stages
    // entirely (pull-priced, EFP-exempt), answered as a
    // normal card via stage-2 only, at this quiescence
    // moment. Scanned regardless of the stage-1
    // unchanged-dedup check below (that dedup is
    // stage-1-specific).
    for (comment_line, question) in
        comment::find_fresh_murshid_comments(hunks, &surface.comment_token, &surface.address_token)
    {
        let Some(site) = site::compute_site(rel_str, sweep_content, comment_line, grammar) else {
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
                db::card_exists_with_advice_fp(c, session_id_now, &comment_fp).unwrap_or(false)
            })
            .unwrap_or(false);
        if already_answered || already_known_this_session {
            continue;
        }
        let Some(conn) = conn_opt else { continue };

        let enclosing_text = site::enclosing_item_text(sweep_content, comment_line, grammar)
            .unwrap_or_else(|| sweep_content.to_string());
        let prompt = comment::build_comment_ask_prompt(&question, &enclosing_text, taxonomy);
        // T11 req 2/4: a murshid-addressed comment is a
        // direct user ask — Interactive lane, never
        // aborted by a concurrent Sweep dispatch.
        let Ok(raw_text) = judge_slot.dispatch(provider::Lane::Interactive, &prompt) else {
            continue;
        };
        // C6 (amended): D17 comment-asks are consent-
        // EXEMPT — the addressed comment IS the consent
        // gesture — but the answer still carries an
        // informational token note, no y/N gate.
        let token_note = crate::consent::token_note(
            &judge_slot.model,
            crate::consent::estimate_tokens(&prompt) + crate::consent::estimate_tokens(&raw_text),
        );
        let Ok(parsed) = judge::parse_stage2_output(&raw_text) else {
            continue;
        };
        let Ok(stage2_card) = judge::validate_stage2_output(&parsed, taxonomy, sweep_content)
        else {
            continue;
        };

        let canon_entry = pack::find_canon_for_concept(canon, &stage2_card.concept);
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
            file: rel_str.to_string(),
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
        let entry_rung = super::resolve_entry_rung(
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
                session_id: session_id_now.to_string(),
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
                site_file: Some(rel_str.to_string()),
                site_line: Some(comment_line as i64),
            },
        ) else {
            continue;
        };

        // req 11 / C7 slot contention: the new ask card
        // is safely persisted now — a direct-ask answer
        // owns the slot on arrival; a displaced pushed
        // card returns to the queue head.
        if let Some(displaced) = ws.pending_card.lock_poison_safe().take() {
            db::warn_on_err(db::requeue_card(conn, displaced.card_id), "requeue_card");
            let seq = {
                let mut s = ws.queue_seq.lock_poison_safe();
                let v = *s;
                *s += 1;
                v
            };
            ws.queue_state.lock_poison_safe().push(queue::QueueEntry {
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
                session_id: session_id_now.to_string(),
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
        db::warn_on_err(
            db::clear_suppressions_for_concept(conn, session_id_now, &stage2_card.concept),
            "clear_suppressions_for_concept",
        );

        // T15: no longer prints the full card render — `ws.pending_card` is
        // set just below, and the TUI redraws it fresh on the next tick.
        ws.notice(format!("direct ask answered \u{2014} {}", comment::DELETE_COMMENT_NOTE));
        ws.notice(token_note);

        *ws.pending_card.lock_poison_safe() = Some(PendingCard {
            card_id,
            session_id: session_id_now.to_string(),
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
}

/// T4 req 1 (gating fix): mechanical applied-detection — if the on-screen
/// card's own file was just swept this pass, relocate its site by
/// ENCLOSING-ITEM IDENTITY (stored item name + anchor hash), never by the
/// possibly-stale `card.line` — an edit ABOVE the site shifts its line
/// but not the item's identity or the anchor's own text, so this survives
/// that (unlike the old line-pinned recompute, which falsely read
/// "applied" in exactly that case).
fn run_applied_detection(
    ws: &Arc<WatchSession>,
    swept_this_pass: &[PathBuf],
    project_root: &Path,
    grammar: &pack::GrammarSpec,
    conn_opt: &Option<rusqlite::Connection>,
    session_id_now: &str,
    taxonomy: &[pack::TaxonomyConcept],
) {
    let maybe_pc = ws.pending_card.lock_poison_safe().clone();
    if let Some(pc) = maybe_pc {
        if let (Some(site_enclosing_item), Some(site_anchor_hash)) = (
            pc.site_enclosing_item.as_ref(),
            pc.site_anchor_hash.as_ref(),
        ) {
            let was_swept = swept_this_pass
                .iter()
                .any(|r| r.to_string_lossy() == pc.card.file);
            if was_swept {
                let abs = project_root.join(&pc.card.file);
                if let Ok(current_content) = std::fs::read_to_string(&abs) {
                    let outcome = site::recheck_site_in_enclosing_item(
                        &current_content,
                        site_enclosing_item,
                        site_anchor_hash,
                        grammar,
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
                                take_pending_card_if_matches(&ws.pending_card, pc.card_id);
                            if consumed {
                                if let Some(conn) = conn_opt {
                                    db::warn_on_err(
                                        db::with_tx(conn, |tx| {
                                            db::update_card_status_stmt(
                                                tx,
                                                pc.card_id,
                                                db::CardStatus::Applied,
                                            )?;
                                            db::log_event_stmt(
                                                tx,
                                                &db::EventRecord {
                                                    id: None,
                                                    session_id: session_id_now.to_string(),
                                                    kind: "card_response".to_string(),
                                                    payload_json: serde_json::json!({
                                                        "verb": "applied",
                                                        "concept": pc.concept_id,
                                                        "detected_by": "site_recheck",
                                                    })
                                                    .to_string(),
                                                    ts: None,
                                                },
                                            )?;
                                            Ok(())
                                        }),
                                        "update_card_status+log_event(applied site_recheck)",
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
                                        .map(|c| c.category.as_str().to_string())
                                        .unwrap_or_else(|| pc.category.clone());
                                    if let Ok(enc) = memory::record_encounter(
                                        conn,
                                        session_id_now,
                                        &pc.concept_id,
                                        &real_category,
                                        bkt::Grade::Hard,
                                        memory::EvidenceSource::Applied,
                                    ) {
                                        if enc.crossed_into_mastery {
                                            ws.notice(format!(
                                                "[murshid] backing off on {} \u{2014} applied {} times straight",
                                                pc.concept_name, enc.row.pass_streak
                                            ));
                                        }
                                    }
                                }
                                ws.notice(format!(
                                    "  applied \u{2014} nice, {} flips to applied",
                                    pc.concept_name
                                ));
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
                                take_pending_card_if_matches(&ws.pending_card, pc.card_id);
                            if consumed {
                                if let Some(conn) = conn_opt {
                                    db::warn_on_err(
                                        db::with_tx(conn, |tx| {
                                            db::update_card_status_stmt(
                                                tx,
                                                pc.card_id,
                                                db::CardStatus::Expired,
                                            )?;
                                            db::log_event_stmt(
                                                tx,
                                                &db::EventRecord {
                                                    id: None,
                                                    session_id: session_id_now.to_string(),
                                                    kind: "card_response".to_string(),
                                                    payload_json: serde_json::json!({
                                                        "verb": "expired",
                                                        "concept": pc.concept_id,
                                                        "detected_by": "site_recheck_item_gone",
                                                    })
                                                    .to_string(),
                                                    ts: None,
                                                },
                                            )?;
                                            Ok(())
                                        }),
                                        "update_card_status+log_event(expired item_gone)",
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

/// req 6: same-concept sites found in this sweep fold into one card each
/// (up to 3 anchors); each aggregated finding then either PUSHES (shown,
/// budget-gated — T1 req 9's "at most one on screen"), QUEUES (req 3/10:
/// persisted as a `queued` card row plus the in-memory pull queue), or
/// COLLAPSES into the concept's already-shipped card this session (req 7 /
/// C8 concept cooldown). Finishes with the one-line queue-presence
/// indicator (req 3).
///
/// T15 mentor-state indicator: returns whether this pass actually shipped
/// (shown OR queued) any card — pure telemetry the caller feeds into
/// `derive_review_result`; never itself gates any of the decisions above.
#[allow(clippy::too_many_arguments)]
// Arity here is inherent per-sweep coordinator state (paths/conn/session/
// directness/detent + the pack payloads it forwards), not a deferred bundle —
// the allow stays.
fn aggregate_and_dispatch(
    ws: &Arc<WatchSession>,
    findings: Vec<aggregate::SweepFinding>,
    conn_opt: &Option<rusqlite::Connection>,
    session_id_now: &str,
    directness: ladder::Directness,
    detent: &noise::Detent,
    now: std::time::SystemTime,
    project_root: &Path,
    grammar: &pack::GrammarSpec,
) -> bool {
    let aggregated = aggregate::aggregate_by_concept(findings);
    let mut shown_this_pass = false;
    let mut shipped_any = false;

    // req 3/10: persists a not-shown-this-pass finding as a
    // `queued` card row and adds it to the in-memory pull
    // queue (req 11: `card_queued` transition event). Returns whether the
    // insert actually landed (T15 telemetry: an insert failure ships
    // nothing, same as it always silently did before this return value
    // existed).
    let enqueue_finding = |conn: &rusqlite::Connection,
                           agg: &aggregate::AggregatedFinding,
                           regresses_card_id: Option<i64>,
                           throttled_flag: bool|
     -> bool {
        let queued_rung =
            super::resolve_entry_rung(conn, &agg.concept_id, &agg.category, directness);
        let Ok(card_id) = db::insert_card(
            conn,
            &db::CardRecord {
                id: None,
                session_id: session_id_now.to_string(),
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
            return false;
        };
        let _ = db::log_event(
            conn,
            &db::EventRecord {
                id: None,
                session_id: session_id_now.to_string(),
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
            let mut s = ws.queue_seq.lock_poison_safe();
            let v = *s;
            *s += 1;
            v
        };
        ws.queue_state.lock_poison_safe().push(queue::QueueEntry {
            finding: agg.clone(),
            seq,
            throttled: throttled_flag,
            card_id,
            session_id: session_id_now.to_string(),
            pinned_head: false,
        });
        true
    };

    for mut agg in aggregated {
        let Some(conn) = conn_opt else { continue };

        // req 5/9: cross-session ledger dedup; a regression
        // (misuse of previously applied/resolved advice) is
        // the one exception, and re-opens a new card row
        // referencing the old one.
        let mut regresses_card_id: Option<i64> = None;
        if let Ok(Some((old_id, status))) = db::find_ledger_card(conn, &agg.advice_fp) {
            if db::is_regression_eligible(&status) {
                regresses_card_id = Some(old_id);
            } else {
                continue; // permanently suppressed (req 5)
            }
        }

        // req 7 / C8 concept cooldown: a concept that already
        // shipped a card this session collapses further sites
        // into that card's aggregation instead of queuing.
        if db::concept_shown_this_session(conn, session_id_now, &agg.concept_id).unwrap_or(false) {
            let _ = db::log_event(
                conn,
                &db::EventRecord {
                    id: None,
                    session_id: session_id_now.to_string(),
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

        let throttled = ws
            .throttled_categories
            .lock_poison_safe()
            .contains(&agg.category);
        let floor_excluded = noise::floor_excludes(detent, &pack::Category::parse(&agg.category));

        // Review fix: single-slot guard — never show a
        // second card while an earlier one (this pass OR an
        // earlier pass) is still awaiting a response. Checked
        // fresh every iteration (not a one-time snapshot) so
        // a concurrent pull via `m` is also respected.
        let pending_card_present = ws.pending_card.lock_poison_safe().is_some();
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
            if enqueue_finding(conn, &agg, regresses_card_id, throttled) {
                shipped_any = true;
            }
            continue;
        }

        let candidate = budget::PushCandidate {
            likely_bug: agg.likely_bug,
            strict_mode_passed: agg.strict_mode_passed,
        };
        let decision = {
            let mut b = ws.bucket.lock_poison_safe();
            budget::decide_push(&mut b, &candidate, now)
        };
        match decision {
            budget::PushDecision::Shown => {
                // req 7 fix: this concept is shipping now —
                // collapse any sibling queued entries for it
                // into this card's aggregation.
                let extra_anchors = super::collapse_queued_siblings(
                    conn,
                    &ws.queue_state,
                    session_id_now,
                    &agg.concept_id,
                );
                if !extra_anchors.is_empty() {
                    let collapsed_count = extra_anchors.len();
                    let overflow = super::fold_anchors_into_card(&mut agg.card, extra_anchors);
                    agg.site_count += collapsed_count;
                    agg.remaining_sites.extend(overflow);
                }

                let shown_rung =
                    super::resolve_entry_rung(conn, &agg.concept_id, &agg.category, directness);
                // T15: no println! here — `ws.pending_card` is set below and
                // the TUI redraws it fresh from that live state.
                if let Ok(card_id) = db::insert_card(
                    conn,
                    &db::CardRecord {
                        id: None,
                        session_id: session_id_now.to_string(),
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
                            session_id: session_id_now.to_string(),
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
                    // (T12 gate fix: the shared helper, not the
                    // former byte-identical inline copy.)
                    let (site_enclosing_item, site_anchor_hash) = super::derive_site_identity(
                        project_root,
                        &agg.card.file,
                        agg.card.line,
                        grammar,
                    );
                    *ws.pending_card.lock_poison_safe() = Some(PendingCard {
                        card_id,
                        session_id: session_id_now.to_string(),
                        concept_id: agg.concept_id.clone(),
                        concept_name: agg.card.concept_name.clone(),
                        advice_fp: agg.advice_fp.clone(),
                        category: agg.category.clone(),
                        rung: shown_rung,
                        site_enclosing_item,
                        site_anchor_hash,
                        card: agg.card.clone(),
                    });
                    shipped_any = true;
                }
                shown_this_pass = true;
            }
            budget::PushDecision::Queued => {
                if enqueue_finding(conn, &agg, regresses_card_id, false) {
                    shipped_any = true;
                }
            }
        }
    }

    // req 3: the one-line presence indicator, printed once
    // per sweep when anything is sitting in the queue.
    let queue_len = ws.queue_state.lock_poison_safe().len();
    if let Some(line) = queue::presence_indicator(queue_len) {
        ws.notice(line);
    }

    shipped_any
}

/// T15 mentor-state indicator: the three outcomes `judge_and_collect_finding`
/// can report for one file's dispatch attempt this pass — feeds
/// `derive_review_result`'s Suggested/NothingToFlag/CouldNotReview mapping.
/// Purely additive telemetry: no variant here changes what the caller does
/// with a `Found` finding, and `Clean`/`DispatchFailed` are exactly the two
/// ways the pre-T15 `Option<SweepFinding>` return used to collapse into
/// `None` (a judged-but-not-card-worthy outcome vs. a dispatch error).
// `Clean`/`DispatchFailed` are unit variants alongside `Found`'s
// `SweepFinding` payload — boxing it would ripple into every `findings.push`/
// `aggregate_by_concept` call site for a per-pass enum that's never
// allocated in a hot loop; the allow matches this repo's existing posture
// on `#[allow(clippy::too_many_arguments)]` for inherent-shape lints.
#[allow(clippy::large_enum_variant)]
enum JudgeAttempt {
    /// A stage-2-validated, card-worthy finding to aggregate/dispatch.
    Found(aggregate::SweepFinding),
    /// Judged cleanly; nothing card-worthy (declined/dropped/silenced/
    /// suppressed/already-known this pass).
    Clean,
    /// The stage-1/stage-2 dispatch itself errored (network/pipeline
    /// failure) — never mistaken for the "clean, nothing to flag" case.
    DispatchFailed,
}

#[cfg(test)]
impl JudgeAttempt {
    /// Test-only convenience: the pre-T15 call sites compared the plain
    /// `Option<SweepFinding>` this function used to return.
    fn into_finding(self) -> Option<aggregate::SweepFinding> {
        match self {
            JudgeAttempt::Found(f) => Some(f),
            JudgeAttempt::Clean | JudgeAttempt::DispatchFailed => None,
        }
    }
}

/// Review fix / C6 "unchanged... never re-judged" dedup gate, then the
/// two-stage judge dispatch for one already-diffed file: application
/// detections are independent evidence (T5 req 3/C6, processed regardless
/// of whether a teaching-moment candidate also fired), `judge_drop`
/// events are logged, and a stage-2-validated finding records its `fail`
/// misuse evidence unconditionally (T5 req 3(b)/10 — I23 separates
/// mastery evidence from noise-control state) before the separate
/// suppressed/already-known/silenced gate decides whether it's returned
/// as a card-worthy finding at all.
// Still >7 args after the PackData bundle: the remaining ones are per-sweep
// coordinator state (session/conn/directness) and the three injected dispatch
// closures, not pack data — inherent arity, so the allow stays.
#[allow(clippy::too_many_arguments)]
fn judge_and_collect_finding(
    ws: &Arc<WatchSession>,
    rel: &Path,
    rel_str: &str,
    hunks: &[diff::Hunk],
    sweep_content: &str,
    pack: pack::PackData,
    already_judged: impl Fn(&str) -> bool,
    dispatch_stage1: impl Fn(&str) -> Result<String, String>,
    dispatch_stage2: impl Fn(&str) -> Result<String, String>,
    conn_opt: &Option<rusqlite::Connection>,
    session_id_now: &str,
    directness: ladder::Directness,
) -> JudgeAttempt {
    // Destructure the bundle so the body reads as the four values it stands in
    // for (PackData is Copy, so `pack` is still passable to judge_hunks below).
    let pack::PackData {
        taxonomy,
        grammar,
        canon: _,
        prompts: _,
    } = pack;

    // Review fix / C6 "unchanged... never re-judged":
    // this exact hunk set was already dispatched to
    // stage-1 this session with nothing new to learn —
    // skip re-burning the screen model on it.
    let hunk_sig = diff::hunks_signature(hunks);
    let unchanged_since_last_dispatch = ws
        .dispatched_hunk_signatures
        .lock_poison_safe()
        .get(rel)
        .is_some_and(|prev| prev == &hunk_sig);
    if unchanged_since_last_dispatch {
        return JudgeAttempt::Clean;
    }
    ws.dispatched_hunk_signatures
        .lock_poison_safe()
        .insert(rel.to_path_buf(), hunk_sig);

    let outcome = pipeline::judge_hunks(
        rel_str,
        hunks,
        sweep_content,
        pack,
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
            if let Some(conn) = conn_opt {
                for (detection, det_line) in &o.application_detections {
                    if !pack::is_valid_slug(taxonomy, &detection.concept) {
                        continue; // C2: what can't be named isn't taught
                    }
                    let Some(det_category) = taxonomy
                        .iter()
                        .find(|c| c.slug == detection.concept)
                        .map(|c| c.category.as_str().to_string())
                    else {
                        continue;
                    };
                    let Some(det_site) =
                        site::compute_site(rel_str, sweep_content, *det_line, grammar)
                    else {
                        continue;
                    };
                    let det_advice_fp = site::advice_fingerprint(&detection.concept, &det_site);
                    // req 3's dual guard: below-mastery
                    // AND no open card at this exact
                    // site (avoids double-counting with
                    // req 4's `hard` grade).
                    let accepted = memory::detection_accepted(
                        conn,
                        session_id_now,
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
                        session_id_now,
                        &detection.concept,
                        &det_category,
                        bkt::Grade::Pass,
                        memory::EvidenceSource::Detection,
                    ) {
                        if enc.crossed_into_mastery {
                            let name = taxonomy
                                .iter()
                                .find(|c| c.slug == detection.concept)
                                .map(|c| c.name.clone())
                                .unwrap_or_else(|| detection.concept.clone());
                            ws.notice(format!(
                                "[murshid] backing off on {} \u{2014} applied {} times straight",
                                name, enc.row.pass_streak
                            ));
                        }
                    }
                }
            }

            if let Some(reason) = &o.drop_reason {
                if let Some(conn) = conn_opt {
                    let payload = judge::judge_drop_payload(reason, rel_str);
                    // T14 req 2: a declined ({}) response is the model's
                    // correct "not a teaching moment" call, not a failure —
                    // logged under a distinct event kind so the outcome-rate
                    // read (T14 req 3) can separate it from a genuine
                    // contract failure/parse error.
                    let kind = if reason.is_declined() {
                        "judge_declined"
                    } else {
                        "judge_drop"
                    };
                    let _ = db::log_event(
                        conn,
                        &db::EventRecord {
                            id: None,
                            session_id: session_id_now.to_string(),
                            kind: kind.to_string(),
                            payload_json: payload.to_string(),
                            ts: None,
                        },
                    );
                }
            }

            if let (Some(card), Some(stage2)) = (o.card, o.stage2) {
                if let Some(site) = site::compute_site(rel_str, sweep_content, card.line, grammar) {
                    let advice_fp = site::advice_fingerprint(&stage2.concept, &site);

                    // req 8: snoozed (instance or concept
                    // scope) this session -> skip entirely.
                    let suppressed = conn_opt
                        .as_ref()
                        .map(|c| {
                            db::is_suppressed(c, session_id_now, &stage2.concept, &advice_fp)
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
                            db::card_exists_with_advice_fp(c, session_id_now, &advice_fp)
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
                    if let Some(conn) = conn_opt {
                        if db::concept_has_any_prior_card(conn, &stage2.concept).unwrap_or(false) {
                            if let Ok(enc) = memory::record_encounter(
                                conn,
                                session_id_now,
                                &stage2.concept,
                                &stage2.category,
                                bkt::Grade::Fail,
                                memory::EvidenceSource::Misuse,
                            ) {
                                if enc.leveled_down {
                                    ws.notice(format!(
                                        "  {} needs another look \u{2014} cards are back",
                                        card.concept_name
                                    ));
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
                            memory::entry_rung_for(c, &stage2.concept, &stage2.category, directness)
                                .unwrap_or(Some(ladder::Rung::R2))
                                .is_none()
                        })
                        .unwrap_or(false);

                    if should_push_misuse_finding(suppressed, already_known, silenced) {
                        return JudgeAttempt::Found(aggregate::SweepFinding {
                            concept_id: stage2.concept.clone(),
                            category: stage2.category.clone(),
                            advice_fp,
                            file: rel_str.to_string(),
                            line: card.line,
                            card,
                            likely_bug: stage2.likely_bug,
                            strict_mode_passed: o.strict_mode_passed,
                        });
                    }
                }
            }
            JudgeAttempt::Clean
        }
        Err(e) => {
            ws.notice(format!("[WARNING] Judge pipeline error: {}", e));
            JudgeAttempt::DispatchFailed
        }
    }
}

/// T15 mentor-state indicator: the pure Suggested/NothingToFlag/
/// CouldNotReview mapping — "keep the mapping simple and correct" (dogfood
/// spec). `degraded` (no usable judge/screen model this session) and
/// `dispatch_failed` (a live `judge_and_collect_finding` call errored this
/// pass) both collapse to `CouldNotReview` — neither is the model
/// legitimately declining; `shipped_any` (a card was shown or queued this
/// pass, from `aggregate_and_dispatch`'s own return) is the only path to
/// `Suggested`. Never itself consulted by judging/gating — purely the
/// telemetry label for the pass that already happened.
fn derive_review_result(degraded: bool, dispatch_failed: bool, shipped_any: bool) -> ReviewResult {
    if degraded || dispatch_failed {
        ReviewResult::CouldNotReview
    } else if shipped_any {
        ReviewResult::Suggested
    } else {
        ReviewResult::NothingToFlag
    }
}

/// T15 mentor-state indicator: closes out this pass's review telemetry.
/// `file` is `Some` only when this pass actually attempted to review
/// something (a file passed the parse gate) — `None` means nothing was
/// attempted this pass, so `last_review` is left untouched (no false
/// "reviewed nothing" claim on a pass that never looked at anything).
/// `review_state` always resets to `Watching` either way — it only ever
/// needs to go back to idle once the pass that set it to `Reviewing` ends.
fn finish_review_pass(ws: &WatchSession, file: Option<String>, result: ReviewResult) {
    if let Some(file) = file {
        *ws.last_review.lock_poison_safe() = Some(LastReview {
            file,
            result,
            at: std::time::SystemTime::now(),
        });
    }
    *ws.review_state.lock_poison_safe() = ReviewState::Watching;
}

/// The dedicated quiescence worker thread (T-debounce-inversion): owns the
/// debounce wait AND one long-lived DB connection for the whole thread
/// lifetime, opened once here and reused every pass (never
/// `open_connection` per event, unlike the old per-call `on_file_event`).
///
/// The notify/polling callback in `mod.rs`'s `run()` is now a thin
/// producer — it records a touched path, stamps `last_event_at`, and
/// `send`s a unit wake on `rx`'s sender, then returns immediately. This
/// loop is the sole consumer of those wakes: `recv_timeout` doubles as
/// both "wake me" and the debounce clock itself. `Ok(())` means a new
/// event landed — the debounce resets (drained naturally: any further
/// already-queued wakes are consumed by the next `recv_timeout` call
/// returning immediately, rather than an explicit drain loop). A
/// `RecvTimeoutError::Timeout` — no new wake within `quiescence::
/// QUIESCENCE_PAUSE` — means quiescent: sweep now, but only if something
/// was actually touched since the last sweep (`dirty`); an idle worker
/// with nothing pending must NOT call `handle_session_split` on a bare
/// timer tick, since `SessionManager::on_file_event` unconditionally
/// stamps its own `last_event_at` — a spurious periodic call would starve
/// the idle-gap session-split of ever seeing a real gap. Shutdown is
/// `RecvTimeoutError::Disconnected` (every `Sender` clone dropped, i.e.
/// the watcher's producer closure has gone away) — the loop simply exits,
/// ending the thread.
#[allow(clippy::too_many_arguments)]
pub fn run_quiescence_worker(
    ws: Arc<WatchSession>,
    rx: std::sync::mpsc::Receiver<()>,
    project_root: PathBuf,
    pack_dir: PathBuf,
    taxonomy: Vec<pack::TaxonomyConcept>,
    canon: Vec<pack::CanonEntry>,
    grammar: pack::GrammarSpec,
    prompts: pack::PromptFragments,
    surface: pack::SurfaceConfig,
    detent: noise::Detent,
    models: crate::Models,
    directness: ladder::Directness,
    mode: judge::JudgeMode,
    unthrottle: Vec<String>,
    // T14 req 1: `Some` only when `[trace] enabled` is true.
    trace_dir: Option<PathBuf>,
) {
    let db_path = db::get_db_path();
    let conn_opt = db_path.as_ref().and_then(|dp| db::open_connection(dp).ok());
    let mut dirty = false;

    loop {
        match rx.recv_timeout(quiescence::QUIESCENCE_PAUSE) {
            Ok(()) => {
                dirty = true;
                continue;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if !dirty {
                    continue; // nothing touched since the last sweep — stay idle
                }
                dirty = false;
                sweep_pending(
                    &ws,
                    &conn_opt,
                    &project_root,
                    &pack_dir,
                    &taxonomy,
                    &canon,
                    &grammar,
                    &prompts,
                    &surface,
                    &detent,
                    &models,
                    directness,
                    &mode,
                    &unthrottle,
                    trace_dir.as_deref(),
                );
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
}

/// T1's file-event sweep, restructured for the debounce inversion: the
/// worker calls this ONCE per quiescent pass (T11: Sweep-lane dispatch)
/// rather than once per raw file event. Re-diffs every file touched since
/// it was last swept/judged, aggregates findings by concept, and either
/// shows (auto-push, budget-gated) or queues each one — plus the T3/T4/T5
/// signals (drift, struggle streaks, direct murshid-comment asks,
/// mechanical applied-detection) that ride along the same pass. `now` is
/// `last_event_at`'s value at the moment quiescence was declared — the
/// timestamp of the last real touch, matching what the old per-event
/// `on_file_event` used as `now` throughout.
#[allow(clippy::too_many_arguments)]
fn sweep_pending(
    ws: &Arc<WatchSession>,
    conn_opt: &Option<rusqlite::Connection>,
    project_root: &Path,
    pack_dir: &Path,
    taxonomy: &[pack::TaxonomyConcept],
    canon: &[pack::CanonEntry],
    grammar: &pack::GrammarSpec,
    prompts: &pack::PromptFragments,
    surface: &pack::SurfaceConfig,
    detent: &noise::Detent,
    models: &crate::Models,
    directness: ladder::Directness,
    mode: &judge::JudgeMode,
    unthrottle: &[String],
    // T14 req 1: `Some` (and enabled) only when `[trace] enabled` is true —
    // `None` makes every dispatch trace a no-op (`trace::record_dispatch`).
    trace_dir: Option<&Path>,
) {
    let now = *ws.last_event_at.lock_poison_safe();
    let project_root_str = project_root.to_string_lossy().to_string();

    let session_id_now =
        handle_session_split(ws, now, project_root, taxonomy, conn_opt, unthrottle);

    // Snapshot every file touched since the last sweep BEFORE any per-file
    // removal below — the "File saved" announcement, `file_edit` history
    // log, and drift touches cover the FULL pending set; the capped
    // judge/diagnostics dispatch further down may defer part of it to the
    // next pass (same retain semantics as before).
    let all_pending: Vec<PathBuf> = ws
        .pending_files
        .lock_poison_safe()
        .iter()
        .cloned()
        .collect();

    for rel in &all_pending {
        let abs = project_root.join(rel);
        ws.notice(format!("File saved: {}", abs.display()));
        if let Some(conn) = conn_opt {
            let edit_event = db::HistoryEvent {
                id: None,
                event_type: "file_edit".to_string(),
                project_root: project_root_str.clone(),
                file_path: abs.to_string_lossy().to_string(),
                success: None,
                error_code: None,
                error_message: None,
                line_number: None,
                created_at: None,
            };
            let _ = db::log_history_event(conn, &edit_event);
        }
    }

    // T3 req 4: drift — track every touch this pass, prune to the
    // trailing 30-min window, and fire the one-per-session notice when
    // ≥70% of recent touches fall outside the goal's file cluster.
    {
        let mut dt = ws.drift_tracking.lock_poison_safe();
        for rel in &all_pending {
            dt.touches.push((rel.to_string_lossy().to_string(), now));
        }
        dt.touches
            .retain(|(_, t)| now.duration_since(*t).unwrap_or_default() <= goal::DRIFT_WINDOW);
        let recent: Vec<String> = dt.touches.iter().map(|(f, _)| f.clone()).collect();
        let cluster = ws.goal_cluster_dirs.lock_poison_safe().clone();
        let ratio = goal::drift_ratio(&cluster, &recent);
        if goal::should_fire_drift(dt.fired, ratio) {
            dt.fired = true;
            ws.notice(format!("[murshid] {}", goal::DRIFT_NOTICE));
        }
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
    // user explicitly following up). Session-level, independent of any
    // one pending file's content — runs once per pass.
    {
        let current_head = session::current_head_commit(project_root);
        let previous_head = ws.last_head_commit.lock_poison_safe().clone();
        if session::head_commit_changed(previous_head.as_deref(), current_head.as_deref()) {
            ws.notice(format!("[murshid] {}", review::REVIEW_OFFER_LINE));
        }
        *ws.last_head_commit.lock_poison_safe() = current_head;
    }

    let degraded = matches!(mode, judge::JudgeMode::Degraded { .. });

    // req 8/req 4/req 6: dedup checked against the DB, computed
    // fresh per advice-fp — never touches `bucket`/`session_mgr`/
    // etc. from inside the dispatch closures below (mutex-
    // poisoning safety: catch_unwind only ever wraps the pure
    // provider call).
    let already_judged = |fp: &str| -> bool {
        conn_opt
            .as_ref()
            .map(|c| db::card_exists_with_advice_fp(c, &session_id_now, fp).unwrap_or(false))
            .unwrap_or(false)
    };

    // req 4/req 6 catch-up sweep: re-diff every file touched since
    // it was last swept/judged, not just the file that triggered this
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
    // fix), so it's simply picked up on the next pass. Diagnostics rides
    // the same cap (a per-file `cargo check`-style run is not free
    // either) — generalized from the old single-triggering-path call to
    // "for each unique pending path" so red/green transitions and the
    // struggle streak keep observing every touched file, not just one.
    let (files_to_sweep, _retained_for_next_pass) =
        cap_dispatch_batch(all_pending, MAX_STAGE1_DISPATCHES_PER_PASS);
    let mut findings: Vec<aggregate::SweepFinding> = Vec::new();
    // T4 req 1: files actually swept (and NOT degraded-mode-only) this
    // pass — the input to the applied-detection site re-check below.
    let mut swept_this_pass: Vec<PathBuf> = Vec::new();
    // T15 mentor-state indicator: `Some(file)` once a file has actually
    // passed the parse gate this pass (the representative file the header
    // shows while `Reviewing`, and `last_review` names when the pass ends).
    // Stays `None` on a pass that never got past the parse gate for
    // anything — `finish_review_pass` then leaves `last_review` untouched.
    let mut review_file: Option<String> = None;
    // T15 mentor-state indicator: set when a live `judge_and_collect_finding`
    // dispatch errors this pass (never on a mere "nothing to flag" outcome).
    let mut any_dispatch_failed = false;

    for rel in files_to_sweep {
        let abs = project_root.join(&rel);
        let file_path_str = abs.to_string_lossy().to_string();
        let sweep_content = match std::fs::read_to_string(&abs) {
            Ok(c) => c,
            Err(_) => {
                // Unreadable/deleted: nothing to sweep, ever.
                ws.pending_files.lock_poison_safe().remove(&rel);
                continue;
            }
        };
        let rel_display = rel.to_string_lossy().to_string();
        if !site::parses_without_errors(&sweep_content, grammar) {
            // T15 fix: record the parse-gate hold so the TUI can show
            // "waiting — doesn't parse yet" instead of an ambiguous silence.
            let mut waiting = ws.parse_waiting.lock_poison_safe();
            if !waiting.contains(&rel_display) {
                waiting.push(rel_display);
            }
            continue; // still broken: stays pending for the next pass
        }
        // Parses now — clear any stale parse-gate flag for this file.
        ws.parse_waiting
            .lock_poison_safe()
            .retain(|p| p != &rel_display);

        // T15 mentor-state indicator: this file just passed the parse gate
        // and is entering the check/judge dispatch below — the header's
        // live "reviewing" face. A multi-file pass shows/remembers the
        // first file that reached this point as the pass's representative
        // (spec: "a single representative file is acceptable").
        if review_file.is_none() {
            review_file = Some(rel_display.clone());
        }
        *ws.review_state.lock_poison_safe() = ReviewState::Reviewing {
            file: rel_display.clone(),
        };

        run_diagnostics_check(
            ws,
            project_root,
            pack_dir,
            &abs,
            &project_root_str,
            &file_path_str,
            conn_opt,
            &session_id_now,
            now,
            &rel,
        );

        // Actually sweeping this file now — only past the parse gate
        // does it leave the pending set.
        ws.pending_files.lock_poison_safe().remove(&rel);

        if degraded {
            // Observe-only: diagnostics above are already recorded; no
            // LLM call for this file.
            continue;
        }
        swept_this_pass.push(rel.clone());

        let snap = ws.snapshot.lock_poison_safe().clone();
        let hunks = match session::compute_session_diff(project_root, &rel, &snap) {
            Ok(h) => h,
            Err(_) => continue,
        };
        if hunks.is_empty() {
            continue;
        }
        let rel_str = rel.to_string_lossy().to_string();

        // T11 req 2/4: watcher-driven stage-1/stage-2 card judging is the
        // Sweep lane — a new Sweep dispatch supersedes only the in-flight
        // Sweep dispatch, never an Interactive one. T14 req 1: each
        // dispatch's raw request/response (or error) is traced when
        // `trace_dir` is `Some` — `rel_str` (this file) stands in for
        // `site_hint`: the model-produced per-candidate `site_hint` doesn't
        // exist yet at stage-1 dispatch time, and threading the eventual
        // stage-1 candidate's `site_hint` into the stage-2 closure would
        // require changing `pipeline::judge_hunks`'s injected-closure
        // signature for a cosmetic label — the file is the site under
        // judgment either way.
        let dispatch_stage1 = |prompt: &str| -> Result<String, String> {
            let result = models.screen.dispatch(provider::Lane::Sweep, prompt);
            crate::trace::record_dispatch(
                trace_dir,
                &session_id_now,
                "screen",
                &models.screen.provider,
                &models.screen.model,
                &rel_str,
                prompt,
                &result,
            );
            result
        };
        let dispatch_stage2 = |prompt: &str| -> Result<String, String> {
            let result = models.judge.dispatch(provider::Lane::Sweep, prompt);
            crate::trace::record_dispatch(
                trace_dir,
                &session_id_now,
                "judge",
                &models.judge.provider,
                &models.judge.model,
                &rel_str,
                prompt,
                &result,
            );
            result
        };

        run_comment_asks(
            ws,
            &rel,
            &rel_str,
            &sweep_content,
            &hunks,
            grammar,
            surface,
            taxonomy,
            canon,
            &models.judge,
            &session_id_now,
            conn_opt,
            directness,
        );

        match judge_and_collect_finding(
            ws,
            &rel,
            &rel_str,
            &hunks,
            &sweep_content,
            pack::PackData {
                taxonomy,
                canon,
                grammar,
                prompts,
            },
            already_judged,
            dispatch_stage1,
            dispatch_stage2,
            conn_opt,
            &session_id_now,
            directness,
        ) {
            JudgeAttempt::Found(finding) => findings.push(finding),
            JudgeAttempt::Clean => {}
            JudgeAttempt::DispatchFailed => any_dispatch_failed = true,
        }
    }

    if degraded {
        // Observe-only: no card-worthy dispatch/aggregation in degraded
        // mode (matches the old on_file_event's early return here).
        finish_review_pass(
            ws,
            review_file,
            derive_review_result(true, any_dispatch_failed, false),
        );
        return;
    }

    run_applied_detection(
        ws,
        &swept_this_pass,
        project_root,
        grammar,
        conn_opt,
        &session_id_now,
        taxonomy,
    );

    let shipped_any = aggregate_and_dispatch(
        ws,
        findings,
        conn_opt,
        &session_id_now,
        directness,
        detent,
        now,
        project_root,
        grammar,
    );

    finish_review_pass(
        ws,
        review_file,
        derive_review_result(false, any_dispatch_failed, shipped_any),
    );
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

    // --- T5 review fix 2: misuse-fail decoupling (req 3(b)/10) ---

    #[test]
    fn test_should_push_misuse_finding_requires_all_three_clear() {
        assert!(should_push_misuse_finding(false, false, false));
        assert!(
            !should_push_misuse_finding(true, false, false),
            "suppressed blocks the push"
        );
        assert!(
            !should_push_misuse_finding(false, true, false),
            "already-known blocks the push"
        );
        assert!(
            !should_push_misuse_finding(false, false, true),
            "silenced blocks the push"
        );
    }

    // --- T15 mentor-state indicator: the pure ReviewResult derivation ---

    #[test]
    fn test_derive_review_result_degraded_is_always_could_not_review() {
        assert_eq!(
            derive_review_result(true, false, false),
            ReviewResult::CouldNotReview
        );
        // Even a (hypothetically) shipped card can't override "degraded" —
        // in practice `shipped_any` is never true when degraded (no dispatch
        // ever runs), but the mapping itself stays a simple priority order.
        assert_eq!(
            derive_review_result(true, false, true),
            ReviewResult::CouldNotReview
        );
    }

    #[test]
    fn test_derive_review_result_dispatch_failure_is_could_not_review() {
        assert_eq!(
            derive_review_result(false, true, false),
            ReviewResult::CouldNotReview
        );
    }

    #[test]
    fn test_derive_review_result_shipped_is_suggested() {
        assert_eq!(
            derive_review_result(false, false, true),
            ReviewResult::Suggested
        );
    }

    #[test]
    fn test_derive_review_result_clean_pass_is_nothing_to_flag() {
        assert_eq!(
            derive_review_result(false, false, false),
            ReviewResult::NothingToFlag
        );
    }

    // --- T15 mentor-state indicator: finish_review_pass telemetry ---

    #[test]
    fn test_finish_review_pass_records_last_review_and_resets_to_watching() {
        let project_root = tmp_project("finish_review_pass_records");
        let now0 = std::time::SystemTime::now();
        let detent = noise::detent_for("standard");
        let ws = WatchSession::new(&project_root, now0, &detent);
        *ws.review_state.lock_poison_safe() = ReviewState::Reviewing {
            file: "src/lib.rs".to_string(),
        };

        finish_review_pass(
            &ws,
            Some("src/lib.rs".to_string()),
            ReviewResult::NothingToFlag,
        );

        let last = ws
            .last_review
            .lock_poison_safe()
            .clone()
            .expect("a review was attempted this pass");
        assert_eq!(last.file, "src/lib.rs");
        assert_eq!(last.result, ReviewResult::NothingToFlag);
        assert_eq!(*ws.review_state.lock_poison_safe(), ReviewState::Watching);

        let _ = std::fs::remove_dir_all(&project_root);
    }

    #[test]
    fn test_finish_review_pass_leaves_last_review_untouched_when_nothing_attempted() {
        let project_root = tmp_project("finish_review_pass_untouched");
        let now0 = std::time::SystemTime::now();
        let detent = noise::detent_for("standard");
        let ws = WatchSession::new(&project_root, now0, &detent);

        // A pass that never got past the parse gate for anything.
        finish_review_pass(&ws, None, ReviewResult::NothingToFlag);

        assert!(
            ws.last_review.lock_poison_safe().is_none(),
            "a pass that reviewed nothing must never fabricate a last_review"
        );
        assert_eq!(*ws.review_state.lock_poison_safe(), ReviewState::Watching);

        let _ = std::fs::remove_dir_all(&project_root);
    }

    // --- T15 mentor-state indicator: a full `sweep_pending` pass wires the
    // telemetry through end to end ---

    /// Drives the REAL `sweep_pending` (not just its helpers) in degraded
    /// mode over one real, parseable, pending file — asserting the pass
    /// records `last_review = CouldNotReview` and resets `review_state`
    /// back to `Watching`. Degraded mode is chosen deliberately: it's the
    /// one path through `sweep_pending` that never reaches a live model
    /// dispatch (`models`' dummy `ResolvedSlot`s are never called), and an
    /// intentionally bogus `pack_dir` basename (matches no registered
    /// language id) makes `run_diagnostics_check`'s adapter lookup fail
    /// fast — so this test never spawns a real `cargo check` subprocess.
    #[test]
    fn test_sweep_pending_degraded_pass_over_real_file_records_could_not_review() {
        let project_root = tmp_project("sweep_pending_degraded");
        std::fs::write(project_root.join("lib.rs"), "fn main() {}\n").unwrap();

        let now0 = std::time::SystemTime::now();
        let detent = noise::detent_for("standard");
        let ws = Arc::new(WatchSession::new(&project_root, now0, &detent));
        ws.pending_files
            .lock_poison_safe()
            .insert(std::path::PathBuf::from("lib.rs"));
        let conn_opt = Some(db::initialize_db(":memory:").unwrap());

        let grammar = pack::GrammarSpec::default();
        let taxonomy = load_taxonomy_fixture();
        let canon = load_canon_fixture();
        let prompts = load_prompts_fixture();
        let surface = pack::SurfaceConfig::default();
        // Bogus on purpose (see doc comment above) — never resolves to the
        // real "rust" adapter, so `run_diagnostics_check` no-ops.
        let pack_dir = std::path::PathBuf::from("not-a-registered-pack-id");
        let models = crate::Models {
            screen: crate::ResolvedSlot {
                provider: "ollama".to_string(),
                model: "unused".to_string(),
                key: None,
                base_url: None,
                key_unreadable: false,
            },
            judge: crate::ResolvedSlot {
                provider: "ollama".to_string(),
                model: "unused".to_string(),
                key: None,
                base_url: None,
                key_unreadable: false,
            },
        };
        let mode = judge::JudgeMode::Degraded {
            reason: "no model configured".to_string(),
        };

        sweep_pending(
            &ws,
            &conn_opt,
            &project_root,
            &pack_dir,
            &taxonomy,
            &canon,
            &grammar,
            &prompts,
            &surface,
            &detent,
            &models,
            ladder::Directness::Balanced,
            &mode,
            &[],
            None,
        );

        let last = ws
            .last_review
            .lock_poison_safe()
            .clone()
            .expect("a real, parseable pending file must be reviewed this pass");
        assert_eq!(last.file, "lib.rs");
        assert_eq!(last.result, ReviewResult::CouldNotReview);
        assert_eq!(
            *ws.review_state.lock_poison_safe(),
            ReviewState::Watching,
            "the pass must reset back to Watching when it finishes"
        );

        let _ = std::fs::remove_dir_all(&project_root);
    }

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

    /// Acceptance: "snoozed concept misused ⇒ p drops, no card." Mirrors
    /// the exact sequence the sweep runs on a stage-2-validated finding:
    /// fail evidence is recorded UNCONDITIONALLY (I23 — mastery state is
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
        memory::record_encounter(
            &conn,
            "sess1",
            concept,
            "idiom",
            bkt::Grade::Pass,
            memory::EvidenceSource::Detection,
        )
        .unwrap();
        let before = db::get_concept_memory(&conn, concept).unwrap().unwrap();

        // The concept is snoozed (concept-scope) this session — the D11
        // noise-control mechanism, unrelated to mastery.
        db::insert_suppression(
            &conn,
            "sess1",
            concept,
            concept,
            crate::suppression::SnoozeScope::Concept,
        )
        .unwrap();
        let suppressed = db::is_suppressed(&conn, "sess1", concept, "fp-new").unwrap();
        assert!(suppressed);

        // The sweep's exact sequence: fail evidence recorded regardless.
        assert!(db::concept_has_any_prior_card(&conn, concept).unwrap());
        let outcome = memory::record_encounter(
            &conn,
            "sess1",
            concept,
            "idiom",
            bkt::Grade::Fail,
            memory::EvidenceSource::Misuse,
        )
        .unwrap();
        assert!(
            outcome.row.p_mastery < before.p_mastery,
            "p must drop from the fail"
        );

        let already_known = false; // a fresh finding this pass
        let silenced = false; // nowhere near mastery here
        assert!(
            !should_push_misuse_finding(suppressed, already_known, silenced),
            "a snoozed concept must never get a new card even though evidence was recorded"
        );
    }

    // --- T5 review fix 3: hard-evidence double-count race guard ---

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

    // --- ROADMAP item 13: pipeline branch integration flow tests ---
    //
    // These drive the extracted sweep functions END TO END against an
    // in-memory DB with INJECTED fixture dispatch (never a live provider
    // call), and assert on the real `cards`/`events` rows and queue state
    // the flow produces — not just return values. Each covers one pipeline
    // BRANCH as a flow, the level the existing leaf-helper tests above don't.
    //
    // NOTE (comment-ask branch, deliberately skipped): `run_comment_asks`
    // dispatches through a `&crate::ResolvedSlot` (which spawns curl), not an
    // injectable closure — there is no seam to feed it a fixture without a
    // real provider/curl call. Driving it would mean either a live call or a
    // localhost curl stub reaching into provider.rs's private lane statics,
    // both outside "injected fixture dispatch". Covering it would require a
    // production seam change, which is out of scope for this test-only pass.

    /// A throwaway on-disk project root — the sweep's file-reading paths
    /// (`derive_site_identity`, applied-detection recheck) want a real dir;
    /// the git calls inside `WatchSession::new` degrade gracefully on a
    /// non-repo temp dir.
    fn tmp_project(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("murshid_sweep_flow_{}", tag));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    // Pack fixtures load from the bundled pack; `env_test_lock` guards the
    // `default_pack_dir()` read against pack.rs's env-mutating tests (same
    // defensive take the pipeline unit tests use).
    fn load_taxonomy_fixture() -> Vec<pack::TaxonomyConcept> {
        let _lock = crate::credentials::env_test_lock();
        pack::load_taxonomy(&pack::default_pack_dir()).unwrap()
    }
    fn load_canon_fixture() -> Vec<pack::CanonEntry> {
        let _lock = crate::credentials::env_test_lock();
        pack::load_canon(&pack::default_pack_dir()).unwrap()
    }
    fn load_prompts_fixture() -> pack::PromptFragments {
        let _lock = crate::credentials::env_test_lock();
        pack::load_prompt_fragments(&pack::default_pack_dir()).unwrap()
    }

    fn json_fixture(name: &str) -> String {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name);
        std::fs::read_to_string(&path).unwrap()
    }

    fn flow_finding(
        concept: &str,
        category: &str,
        file: &str,
        line: usize,
    ) -> aggregate::SweepFinding {
        let mut c = sample_card();
        c.concept_name = concept.to_string();
        c.file = file.to_string();
        c.line = line;
        aggregate::SweepFinding {
            concept_id: concept.to_string(),
            category: category.to_string(),
            advice_fp: format!("fp-{}-{}-{}", concept, file, line),
            file: file.to_string(),
            line,
            card: c,
            likely_bug: false,
            strict_mode_passed: false,
        }
    }

    fn count_cards(conn: &rusqlite::Connection, concept: &str, status: &str) -> i64 {
        conn.query_row(
            "SELECT COUNT(*) FROM cards WHERE concept_id = ?1 AND status = ?2",
            rusqlite::params![concept, status],
            |r| r.get(0),
        )
        .unwrap()
    }

    fn event_kinds(conn: &rusqlite::Connection, session_id: &str) -> Vec<String> {
        db::get_events_for_session(conn, session_id)
            .unwrap()
            .into_iter()
            .map(|e| e.kind)
            .collect()
    }

    /// push-vs-queue branch, as a flow: the first finding takes the single
    /// on-screen slot (auto-push, budget-gated) while a second, distinct-
    /// concept finding in the SAME pass is forced to the queue by the
    /// single-slot guard. The first finding is PRODUCED by driving
    /// `judge_and_collect_finding` against injected stage-1/stage-2 fixtures
    /// (never a live call); the dispatch decision then rides
    /// `aggregate_and_dispatch`. Asserts on the real `cards` rows, the queue,
    /// the pending slot, and the transition events.
    #[test]
    fn test_sweep_flow_pushes_first_finding_and_queues_second() {
        let project_root = tmp_project("push_queue");
        let now0 = std::time::SystemTime::now();
        let detent = noise::detent_for("standard"); // floor includes best-practice + idiom
        let ws = Arc::new(WatchSession::new(&project_root, now0, &detent));
        let conn_opt = Some(db::initialize_db(":memory:").unwrap());
        let session_id = "sess-pushq";

        let grammar = pack::GrammarSpec::default();
        let taxonomy = load_taxonomy_fixture();
        let canon = load_canon_fixture();
        let prompts = load_prompts_fixture();

        // An introduced `.clone()`-where-borrow-works inside `fn caller`.
        let old = "fn print_name(name: String) { println!(\"{}\", name); }\n\nfn caller() {\n}\n";
        let new = "fn print_name(name: String) { println!(\"{}\", name); }\n\nfn caller() {\n    print_name(person.name.clone());\n}\n";
        let hunks = diff::diff_lines(old, new);
        let stage1 = json_fixture("stage1_response.json");
        let stage2 = json_fixture("stage2_response_valid.json");

        let f1 = judge_and_collect_finding(
            &ws,
            std::path::Path::new("src/lib.rs"),
            "src/lib.rs",
            &hunks,
            new,
            pack::PackData {
                taxonomy: &taxonomy,
                canon: &canon,
                grammar: &grammar,
                prompts: &prompts,
            },
            |_fp| false,
            |_p| Ok(stage1.clone()),
            |_p| Ok(stage2.clone()),
            &conn_opt,
            session_id,
            ladder::Directness::Balanced,
        )
        .into_finding()
        .expect("fixture dispatch must yield a card-worthy finding");
        assert_eq!(f1.concept_id, "borrow-vs-clone");

        // A second, distinct concept found the same pass.
        let f2 = flow_finding("string-vs-str", "idiom", "src/other.rs", 1);

        aggregate_and_dispatch(
            &ws,
            vec![f1, f2],
            &conn_opt,
            session_id,
            ladder::Directness::Balanced,
            &detent,
            now0,
            &project_root,
            &grammar,
        );

        let conn = conn_opt.as_ref().unwrap();
        assert_eq!(
            count_cards(conn, "borrow-vs-clone", "shown"),
            1,
            "the first finding is auto-pushed (a shown card row)"
        );
        assert_eq!(
            count_cards(conn, "string-vs-str", "queued"),
            1,
            "the second finding queues behind the single-slot guard"
        );
        assert_eq!(count_cards(conn, "string-vs-str", "shown"), 0);

        // The pending slot holds the pushed concept; the queue holds the other.
        let pending_concept = ws
            .pending_card
            .lock()
            .unwrap()
            .as_ref()
            .map(|p| p.concept_id.clone());
        assert_eq!(pending_concept.as_deref(), Some("borrow-vs-clone"));
        {
            let queue = ws.queue_state.lock().unwrap();
            assert_eq!(queue.len(), 1);
            assert_eq!(queue[0].finding.concept_id, "string-vs-str");
        }

        let kinds = event_kinds(conn, session_id);
        assert!(kinds.iter().any(|k| k == "card_shown"));
        assert!(kinds.iter().any(|k| k == "card_queued"));

        let _ = std::fs::remove_dir_all(&project_root);
    }

    // --- T14 req 2: judge_declined vs judge_drop event-kind split ---

    /// An empty `{}` stage-2 response (the pack prompt's instructed decline)
    /// must log `judge_declined`, never `judge_drop` — the correct "not a
    /// teaching moment" outcome is not a contract failure.
    #[test]
    fn test_judge_and_collect_finding_logs_judge_declined_for_empty_stage2_response() {
        let project_root = tmp_project("declined");
        let now0 = std::time::SystemTime::now();
        let detent = noise::detent_for("standard");
        let ws = Arc::new(WatchSession::new(&project_root, now0, &detent));
        let conn_opt = Some(db::initialize_db(":memory:").unwrap());
        let session_id = "sess-declined";

        let grammar = pack::GrammarSpec::default();
        let taxonomy = load_taxonomy_fixture();
        let canon = load_canon_fixture();
        let prompts = load_prompts_fixture();

        let old = "fn print_name(name: String) { println!(\"{}\", name); }\n";
        let new = "fn print_name(name: String) { println!(\"{}\", name); }\nprint_name(person.name.clone());\n";
        let hunks = diff::diff_lines(old, new);
        let stage1 = json_fixture("stage1_response.json");

        let finding = judge_and_collect_finding(
            &ws,
            std::path::Path::new("src/lib.rs"),
            "src/lib.rs",
            &hunks,
            new,
            pack::PackData {
                taxonomy: &taxonomy,
                canon: &canon,
                grammar: &grammar,
                prompts: &prompts,
            },
            |_fp| false,
            |_p| Ok(stage1.clone()),
            |_p| Ok("{}".to_string()),
            &conn_opt,
            session_id,
            ladder::Directness::Balanced,
        )
        .into_finding();
        assert!(finding.is_none());

        let conn = conn_opt.as_ref().unwrap();
        let kinds = event_kinds(conn, session_id);
        assert!(
            kinds.iter().any(|k| k == "judge_declined"),
            "expected judge_declined, got: {:?}",
            kinds
        );
        assert!(!kinds.iter().any(|k| k == "judge_drop"));

        let _ = std::fs::remove_dir_all(&project_root);
    }

    /// A partial stage-2 response (some legs present, `concept` missing) is
    /// a genuine contract failure and must keep logging `judge_drop`, never
    /// `judge_declined`.
    #[test]
    fn test_judge_and_collect_finding_logs_judge_drop_for_partial_missing_leg() {
        let project_root = tmp_project("contract_failure");
        let now0 = std::time::SystemTime::now();
        let detent = noise::detent_for("standard");
        let ws = Arc::new(WatchSession::new(&project_root, now0, &detent));
        let conn_opt = Some(db::initialize_db(":memory:").unwrap());
        let session_id = "sess-contract-failure";

        let grammar = pack::GrammarSpec::default();
        let taxonomy = load_taxonomy_fixture();
        let canon = load_canon_fixture();
        let prompts = load_prompts_fixture();

        let old = "fn print_name(name: String) { println!(\"{}\", name); }\n";
        let new = "fn print_name(name: String) { println!(\"{}\", name); }\nprint_name(person.name.clone());\n";
        let hunks = diff::diff_lines(old, new);
        let stage1 = json_fixture("stage1_response.json");
        let partial_stage2 = r#"{"grounding_quote": "person.name.clone()", "why": "x", "rule": "y", "worked_diff": "z", "category": "idiom", "likely_bug": false}"#.to_string();

        let finding = judge_and_collect_finding(
            &ws,
            std::path::Path::new("src/lib.rs"),
            "src/lib.rs",
            &hunks,
            new,
            pack::PackData {
                taxonomy: &taxonomy,
                canon: &canon,
                grammar: &grammar,
                prompts: &prompts,
            },
            |_fp| false,
            |_p| Ok(stage1.clone()),
            |_p| Ok(partial_stage2.clone()),
            &conn_opt,
            session_id,
            ladder::Directness::Balanced,
        )
        .into_finding();
        assert!(finding.is_none());

        let conn = conn_opt.as_ref().unwrap();
        let kinds = event_kinds(conn, session_id);
        assert!(
            kinds.iter().any(|k| k == "judge_drop"),
            "expected judge_drop, got: {:?}",
            kinds
        );
        assert!(!kinds.iter().any(|k| k == "judge_declined"));

        let _ = std::fs::remove_dir_all(&project_root);
    }

    /// throttle branch: a currently-throttled category (D12 auto-throttle)
    /// never takes the on-screen slot — the gate diverts the finding to the
    /// queue BEFORE the push budget is even consulted, and the `card_queued`
    /// event records `throttled=true`.
    #[test]
    fn test_sweep_flow_throttled_category_queues_instead_of_pushing() {
        let project_root = tmp_project("throttle");
        let now0 = std::time::SystemTime::now();
        let detent = noise::detent_for("standard"); // idiom is in-floor, so this isolates throttle
        let ws = Arc::new(WatchSession::new(&project_root, now0, &detent));
        ws.throttled_categories
            .lock()
            .unwrap()
            .insert("idiom".to_string());
        let conn_opt = Some(db::initialize_db(":memory:").unwrap());
        let session_id = "sess-throttle";
        let grammar = pack::GrammarSpec::default();

        aggregate_and_dispatch(
            &ws,
            vec![flow_finding("borrow-vs-clone", "idiom", "a.rs", 1)],
            &conn_opt,
            session_id,
            ladder::Directness::Balanced,
            &detent,
            now0,
            &project_root,
            &grammar,
        );

        let conn = conn_opt.as_ref().unwrap();
        assert_eq!(count_cards(conn, "borrow-vs-clone", "queued"), 1);
        assert_eq!(
            count_cards(conn, "borrow-vs-clone", "shown"),
            0,
            "a throttled category never takes the slot"
        );
        assert!(ws.pending_card.lock().unwrap().is_none());
        assert_eq!(
            ws.bucket.lock().unwrap().tokens_available(),
            1.0,
            "throttle gates before the budget is consulted — the token is untouched"
        );

        let events = db::get_events_for_session(conn, session_id).unwrap();
        let queued = events
            .iter()
            .find(|e| e.kind == "card_queued")
            .expect("a card_queued transition event");
        assert!(
            queued.payload_json.contains("\"throttled\":true"),
            "the queued event records the throttle: {}",
            queued.payload_json
        );

        let _ = std::fs::remove_dir_all(&project_root);
    }

    /// floor branch: a category outside the detent's severity floor is never
    /// budget-eligible — it queues rather than pushes (and never touches the
    /// budget), so nothing is dropped. `architecture` sits outside the `quiet`
    /// floor (bug + idiom only).
    #[test]
    fn test_sweep_flow_floor_excluded_category_queues_not_shown() {
        let project_root = tmp_project("floor");
        let now0 = std::time::SystemTime::now();
        let detent = noise::detent_for("quiet"); // floor = bug, idiom
        let ws = Arc::new(WatchSession::new(&project_root, now0, &detent));
        let conn_opt = Some(db::initialize_db(":memory:").unwrap());
        let session_id = "sess-floor";
        let grammar = pack::GrammarSpec::default();

        aggregate_and_dispatch(
            &ws,
            vec![flow_finding("layering", "architecture", "a.rs", 1)],
            &conn_opt,
            session_id,
            ladder::Directness::Balanced,
            &detent,
            now0,
            &project_root,
            &grammar,
        );

        let conn = conn_opt.as_ref().unwrap();
        assert_eq!(count_cards(conn, "layering", "queued"), 1);
        assert_eq!(count_cards(conn, "layering", "shown"), 0);
        assert!(ws.pending_card.lock().unwrap().is_none());
        assert_eq!(
            ws.bucket.lock().unwrap().tokens_available(),
            1.0,
            "a floor-excluded candidate never consumes the budget"
        );

        let _ = std::fs::remove_dir_all(&project_root);
    }

    /// concept-collapse-on-ship branch: when a card for a concept ships this
    /// pass, every sibling entry for the SAME concept still sitting in the
    /// queue collapses INTO the shipped card's aggregation — the sibling row
    /// flips to `collapsed`, its anchor folds into the shown card, and one
    /// `card_aggregated` event is logged.
    #[test]
    fn test_sweep_flow_ship_collapses_queued_sibling() {
        let project_root = tmp_project("collapse");
        let now0 = std::time::SystemTime::now();
        let detent = noise::detent_for("standard");
        let ws = Arc::new(WatchSession::new(&project_root, now0, &detent));
        let conn_opt = Some(db::initialize_db(":memory:").unwrap());
        let session_id = "sess-collapse";
        let grammar = pack::GrammarSpec::default();
        let concept = "borrow-vs-clone";
        let conn = conn_opt.as_ref().unwrap();

        // Pre-seed a QUEUED sibling of the same concept at a different site
        // (a `queued` status is not a "seen" status, so it does not trip the
        // concept-cooldown gate — the fresh finding is still free to ship).
        let sib = flow_finding(concept, "best-practice", "sibling.rs", 9);
        let sib_card_id = db::insert_card(
            conn,
            &db::CardRecord {
                id: None,
                session_id: session_id.to_string(),
                concept_id: sib.concept_id.clone(),
                category: sib.category.clone(),
                rung_shown: "R2".to_string(),
                advice_fp: sib.advice_fp.clone(),
                finding_fp: None,
                status: "queued".to_string(),
                created_ts: None,
                resolved_ts: None,
                worked_diff: None,
                regresses_card_id: None,
                site_file: Some("sibling.rs".to_string()),
                site_line: Some(9),
            },
        )
        .unwrap();
        ws.queue_state.lock().unwrap().push(queue::QueueEntry {
            finding: aggregate::AggregatedFinding {
                concept_id: sib.concept_id.clone(),
                category: sib.category.clone(),
                advice_fp: sib.advice_fp.clone(),
                card: sib.card.clone(),
                likely_bug: false,
                strict_mode_passed: false,
                site_count: 1,
                remaining_sites: Vec::new(),
            },
            seq: 0,
            throttled: false,
            card_id: sib_card_id,
            session_id: session_id.to_string(),
            pinned_head: false,
        });

        // Ship a fresh finding of the same concept at a new site.
        aggregate_and_dispatch(
            &ws,
            vec![flow_finding(concept, "best-practice", "primary.rs", 3)],
            &conn_opt,
            session_id,
            ladder::Directness::Balanced,
            &detent,
            now0,
            &project_root,
            &grammar,
        );

        // The sibling row is now collapsed; a new shown card shipped.
        let sib_status: String = conn
            .query_row(
                "SELECT status FROM cards WHERE id = ?1",
                rusqlite::params![sib_card_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(sib_status, "collapsed");
        assert_eq!(count_cards(conn, concept, "shown"), 1);
        assert!(
            ws.queue_state.lock().unwrap().is_empty(),
            "the sibling folded into the shipped card, draining the queue"
        );

        // The shipped card carries the sibling's site as a folded anchor.
        let folded = ws
            .pending_card
            .lock()
            .unwrap()
            .as_ref()
            .map(|p| p.card.additional_anchors.clone())
            .unwrap_or_default();
        assert!(
            folded.contains(&("sibling.rs".to_string(), 9)),
            "the sibling anchor is folded into the shown card: {:?}",
            folded
        );

        let kinds = event_kinds(conn, session_id);
        assert!(kinds.iter().any(|k| k == "card_aggregated"));
        assert!(kinds.iter().any(|k| k == "card_shown"));

        let _ = std::fs::remove_dir_all(&project_root);
    }

    /// applied-detection branch, as a flow: the on-screen card's own file is
    /// swept this pass and the flagged anchor is gone from its enclosing item
    /// (the fix landed). The mechanical re-check flips the card to `applied`,
    /// clears the pending slot, logs a `card_response`(verb=applied) event,
    /// and records `hard`/`applied` mastery evidence — all read back from the
    /// real DB.
    #[test]
    fn test_sweep_flow_applied_detection_flips_card_and_records_hard_evidence() {
        let project_root = tmp_project("applied");
        std::fs::create_dir_all(project_root.join("src")).unwrap();
        let rel = "src/lib.rs";
        // Before: the flagged `.clone()`; after (on disk now): the fix.
        let before = "fn print_name(name: String) { println!(\"{}\", name); }\n\nfn main() {\n    let person = Person { name: String::from(\"Ada\") };\n    print_name(person.name.clone());\n}\n";
        let after = "fn print_name(name: String) { println!(\"{}\", name); }\n\nfn main() {\n    let person = Person { name: String::from(\"Ada\") };\n    print_name(&person.name);\n}\n";
        std::fs::write(project_root.join(rel), after).unwrap();

        let grammar = pack::GrammarSpec::default();
        // Site identity is captured from the PRE-fix content (the anchor the
        // card was pinned to), exactly as the show path stored it.
        let site = site::compute_site(rel, before, 5, &grammar)
            .expect("the clone line must resolve to a site");
        let advice_fp = site::advice_fingerprint("borrow-vs-clone", &site);

        let now0 = std::time::SystemTime::now();
        let detent = noise::detent_for("standard");
        let ws = Arc::new(WatchSession::new(&project_root, now0, &detent));
        let conn_opt = Some(db::initialize_db(":memory:").unwrap());
        let session_id = "sess-applied";
        let concept = "borrow-vs-clone";
        let conn = conn_opt.as_ref().unwrap();

        let card_id = db::insert_card(
            conn,
            &db::CardRecord {
                id: None,
                session_id: session_id.to_string(),
                concept_id: concept.to_string(),
                category: "best-practice".to_string(),
                rung_shown: "R2".to_string(),
                advice_fp: advice_fp.clone(),
                finding_fp: None,
                status: "shown".to_string(),
                created_ts: None,
                resolved_ts: None,
                worked_diff: None,
                regresses_card_id: None,
                site_file: Some(rel.to_string()),
                site_line: Some(5),
            },
        )
        .unwrap();

        // The on-screen card, keyed to the pre-fix site identity.
        let mut card = sample_card();
        card.concept_name = "Borrow vs. clone".to_string();
        card.file = rel.to_string();
        card.line = 5;
        *ws.pending_card.lock().unwrap() = Some(PendingCard {
            card_id,
            session_id: session_id.to_string(),
            concept_id: concept.to_string(),
            concept_name: card.concept_name.clone(),
            advice_fp,
            category: "best-practice".to_string(),
            rung: ladder::Rung::R2,
            card,
            site_enclosing_item: Some(site.enclosing_item.clone()),
            site_anchor_hash: Some(site.anchor_hash.clone()),
        });

        run_applied_detection(
            &ws,
            &[std::path::PathBuf::from(rel)],
            &project_root,
            &grammar,
            &conn_opt,
            session_id,
            &[],
        );

        // The card flipped to applied and the slot is now free.
        let status: String = conn
            .query_row(
                "SELECT status FROM cards WHERE id = ?1",
                rusqlite::params![card_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(status, "applied");
        assert!(
            ws.pending_card.lock().unwrap().is_none(),
            "a consumed applied-detection frees the on-screen slot"
        );

        // The response and the encounter both landed.
        let events = db::get_events_for_session(conn, session_id).unwrap();
        let response = events
            .iter()
            .find(|e| e.kind == "card_response")
            .expect("a card_response event");
        assert!(response.payload_json.contains("\"verb\":\"applied\""));
        assert!(response.payload_json.contains("site_recheck"));

        // `hard`/`applied` mastery evidence was recorded.
        let mem = db::get_concept_memory(conn, concept)
            .unwrap()
            .expect("an encounter must have upserted a memory row");
        assert_eq!(mem.last_outcome, Some(bkt::Grade::Hard));
        assert!(
            events
                .iter()
                .any(|e| e.kind == "encounter" && e.payload_json.contains("\"source\":\"applied\"")),
            "the applied encounter is logged with source=applied"
        );

        let _ = std::fs::remove_dir_all(&project_root);
    }
}
