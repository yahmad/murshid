//! Card/offer interaction logic for the watch loop: card lifecycle responses
//! (applied/got-it/not-now/not-useful), rung escalation, and struggle-offer
//! accept/decline. Each keystroke maps to a C3 response and its persisted
//! event.
//!
//! T15: this module's DB-mutating bodies are now plain callable functions
//! (`handle_card_key`, `handle_offer_key`, and the `apply_*` helpers they
//! delegate to) rather than being inlined in a blocking stdin-reading loop —
//! the retired `run_stdin_loop` used to own them directly. The TUI
//! (`crate::tui`) is the only caller now; it maps a crossterm key event to
//! `response::classify_card_key` / `offer::classify_offer_key` exactly as
//! the old loop did, then calls the same functions here, so a/g/u/n/e/t and
//! offer y/n have IDENTICAL DB effects to the pre-T15 loop.
//!
//! Redesign R5: `k` (ask) is real now — entering ask mode is a pure `App`
//! state change owned entirely by `tui::mod::handle_key` (no DB effect, so
//! `handle_card_key`'s own `Ask` arm below is a no-op left for match
//! exhaustiveness/safety only); the actual conversational dispatch +
//! persistence is [`apply_ask_send`], called from the same background-
//! thread/busy-guard pattern `apply_offer_accept` already established.
//!
//! Rendering is deliberately NOT done here: the TUI always redraws its
//! views fresh from `WatchSession`/`profile.db` state on every tick, so
//! these functions only mutate state and return short notice strings for
//! the activity log — never a full card render.

use std::path::Path;

use crate::{
    budget, db, ladder, memory, offer, pack, pipeline, provider, response, session, site,
    suppression, thread,
};

use super::{PendingCard, PendingOffer, WatchSession};
use crate::sync_ext::LockExt;

/// T3 req 11: "`y` runs the judge on the struggle site and shows the card
/// through the normal slot." A reduced, single-file replay of the watcher's
/// sweep-and-show path, invoked only on an accepted struggle offer. Ledger/
/// cooldown/suppression gates are deliberately not re-applied here — the
/// user just explicitly asked for this exact site, which is the same
/// "asking trumps prior state" logic D17 uses for direct asks.
///
/// T15: no longer renders/prints the card itself — the caller sets
/// `ws.pending_card` from the returned value, and the TUI redraws it fresh
/// from that live state on its next tick.
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
    models: &crate::Models,
    ws: &WatchSession,
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
        models.screen.dispatch(provider::Lane::Interactive, prompt)
    };
    let dispatch_stage2 = |prompt: &str| -> Result<String, String> {
        models.judge.dispatch(provider::Lane::Interactive, prompt)
    };
    // T16a: same bounding rule as the sweep's own dispatch, scoped to this
    // project root — a struggle-accept judge can resolve a `range`/`file`
    // context request too.
    let resolve_file = pipeline::project_scoped_file_reader(project_root.to_path_buf());

    let outcome = pipeline::judge_hunks(
        &rel_str,
        &hunks,
        &content,
        pack::PackData {
            taxonomy,
            canon,
            grammar,
            prompts,
        },
        already_judged,
        dispatch_stage1,
        dispatch_stage2,
        resolve_file,
    )
    .ok()?;

    let card = outcome.card?;
    let stage2 = outcome.stage2?;
    let site = site::compute_site(&rel_str, &content, card.line, grammar)?;
    let advice_fp = site::advice_fingerprint(&stage2.concept, &site);
    // T5 req 4: memory-driven entry rung, resolved for THIS concept now
    // that stage-2 has named it. T15 settings overlay: reads
    // `ws.directness` fresh (via `resolve_entry_rung`), not a value frozen
    // earlier — a settings-overlay change is honored on this very call.
    let entry_rung = super::resolve_entry_rung(conn, ws, &stage2.concept, &stage2.category);

    // C7/D16 mitigation: an accepted offer always shows now, preempting the
    // queue; consumes a token if available, else borrows exactly one.
    {
        let mut b = ws.bucket.lock_poison_safe();
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
            card_body_json: db::card_body_json(&card, &stage2.category),
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
        from_struggle_offer: true,
    })
}

/// T2 review fix (req 7): whether a queued entry may still be shown once
/// pulled via `m` — re-checked right before showing (not just at enqueue
/// time), because a sibling entry for the same concept can ship (auto-push)
/// or get snoozed to concept-scope in the time between queuing and pulling.
/// Without this re-check a user could pull two cards for one concept.
///
/// T15: interactive queue-pull-into-slot is deferred from the TUI v1 (the
/// spec's V1 dashboard only calls for a read-only "queue depth + top
/// concepts" summary; a live pull UI would also collide with `1`-`4` being
/// reserved for view-switching) — this helper is currently exercised only by
/// its own unit tests below, kept ready for whenever queue-pull is wired.
pub fn pull_is_blocked(
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

/// T15: the `Response(verb)` arm of the old `run_stdin_loop`, extracted
/// verbatim (same DB effect, same transaction shape) so `a`/`g`/`u`/`n`
/// record identically whether the caller is the retired stdin loop or the
/// TUI. `pc` must already be taken out of `ws.pending_card` by the caller
/// (mirrors the old loop's `.take()` — the slot is freed the instant a
/// response is accepted, before any DB work). Returns notice lines for the
/// activity log (e.g. a widening or backing-off notice) — empty when there's
/// nothing to say.
pub fn apply_card_response(
    conn: &rusqlite::Connection,
    pc: &PendingCard,
    verb: response::ResponseVerb,
    taxonomy: &[pack::TaxonomyConcept],
) -> Vec<String> {
    let mut notices = Vec::new();

    // req 8 / D11(c): tiered snooze on `not_now` — the scope decision is a
    // plain read, made once up front so the (possibly-retried) transaction
    // closure below is pure w.r.t. it (no re-reading a changing count on
    // retry).
    let snooze_scope = if verb == response::ResponseVerb::NotNow {
        let prior =
            db::count_instance_snoozes_for_concept(conn, &pc.session_id, &pc.concept_id)
                .unwrap_or(0);
        Some(suppression::tiered_snooze_scope(prior))
    } else {
        None
    };
    let widened = snooze_scope == Some(suppression::SnoozeScope::Concept);

    // update_card_status(verb) -> insert_suppression (if not_now) ->
    // enforce_suppression_cap (if not_now) -> log_event("card_response"),
    // all in one transaction: these are the state cards/events derive
    // noise/throttle/BKT state from, so a partial failure between them must
    // never be observable.
    db::warn_on_err(
        db::with_tx(conn, |tx| {
            db::update_card_status_stmt(tx, pc.card_id, verb.into())?;
            match snooze_scope {
                Some(suppression::SnoozeScope::Instance) => {
                    db::insert_suppression_stmt(
                        tx,
                        &pc.session_id,
                        &pc.concept_id,
                        &pc.advice_fp,
                        suppression::SnoozeScope::Instance,
                    )?;
                    db::enforce_suppression_cap_stmt(tx, &pc.session_id)?;
                }
                Some(suppression::SnoozeScope::Concept) => {
                    db::insert_suppression_stmt(
                        tx,
                        &pc.session_id,
                        &pc.concept_id,
                        &pc.concept_id,
                        suppression::SnoozeScope::Concept,
                    )?;
                    db::enforce_suppression_cap_stmt(tx, &pc.session_id)?;
                }
                None => {}
            }
            db::log_event_stmt(
                tx,
                &db::EventRecord {
                    id: None,
                    session_id: pc.session_id.clone(),
                    kind: "card_response".to_string(),
                    payload_json: serde_json::json!({
                        "verb": verb.as_str(),
                        "concept": pc.concept_id,
                        "widened": widened,
                    })
                    .to_string(),
                    ts: None,
                },
            )?;
            Ok(())
        }),
        "update_card_status+insert_suppression+enforce_suppression_cap+log_event(card_response)",
    );

    if widened {
        notices.push(suppression::widening_notice(&pc.concept_name));
    }

    // T5 req 3(c)/10: `applied` (manual `a`) is the ONLY response verb that
    // is evidence — got_it/not_now/not_useful are dismissals (I23), never
    // mastery signal. Guarded by the single testable source of truth in
    // memory.rs so a future new verb can't silently become evidence by
    // accident. Deliberately not part of the transaction above: it writes to
    // `concept_memory`, a separate table outside this pair's cards<->events
    // atomicity contract (see report).
    if let Some(grade) = memory::should_record_evidence_for_response(verb.into()) {
        // A D17 comment-ask card's `category` field holds the pseudo-
        // category `comment-ask` (bookkeeping only) — resolve the concept's
        // REAL taxonomy category for BKT priors.
        let real_category = taxonomy
            .iter()
            .find(|c| c.slug == pc.concept_id)
            .map(|c| c.category.as_str().to_string())
            .unwrap_or_else(|| pc.category.clone());
        if let Ok(enc) = memory::record_encounter(
            conn,
            &pc.session_id,
            &pc.concept_id,
            &real_category,
            grade,
            memory::EvidenceSource::Applied,
        ) {
            if enc.crossed_into_mastery {
                notices.push(format!(
                    "[murshid] backing off on {} \u{2014} applied {} times straight",
                    pc.concept_name, enc.row.pass_streak
                ));
            }
        }
    }

    notices
}

/// T15: the `Escalate`/`TellMe` arm of the old `run_stdin_loop`, extracted
/// verbatim — persists the new rung and logs the `card_response`(escalated)
/// event. Returns the new rung; the caller updates `ws.pending_card.rung`
/// (the TUI re-renders the card at its new rung on the next tick — no print
/// here).
pub fn apply_escalate_or_tell_me(
    conn: &rusqlite::Connection,
    pc: &PendingCard,
    escalate: bool,
) -> ladder::Rung {
    let new_rung = if escalate {
        ladder::escalate_one(pc.rung)
    } else {
        ladder::tell_me(pc.rung)
    };

    // req 3/4: every step (and every reveal, even a repeat `t` at R3) is
    // logged — click-through gaming must be visible.
    db::warn_on_err(
        db::update_card_rung(conn, pc.card_id, new_rung.as_str()),
        "update_card_rung",
    );
    let _ = db::log_event(
        conn,
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
    new_rung
}

/// T15: the single entry point the TUI calls for a focused-card key press —
/// classifies via [`response::classify_card_key`] (IDENTICAL to the old
/// stdin loop) and dispatches to the extracted functions above, mutating
/// `ws.pending_card` exactly as the old loop's inline arms did. Redesign R5:
/// `k` (ask) is intercepted by `tui::mod::handle_key` BEFORE it ever reaches
/// here (entering ask mode is a pure `App`-level state change, not a DB
/// effect), so this function's own `Ask` arm is unreachable from the TUI in
/// practice — it stays a harmless no-op (leaves the card pending, no
/// notice) for match-exhaustiveness and any other caller.
pub fn handle_card_key(
    conn: &rusqlite::Connection,
    ws: &WatchSession,
    action: response::CardKeyAction,
    taxonomy: &[pack::TaxonomyConcept],
) -> Vec<String> {
    match action {
        response::CardKeyAction::Ignore => Vec::new(),

        response::CardKeyAction::Escalate | response::CardKeyAction::TellMe => {
            let Some(pc) = ws.pending_card.lock_poison_safe().clone() else {
                return Vec::new();
            };
            let escalate = action == response::CardKeyAction::Escalate;
            let new_rung = apply_escalate_or_tell_me(conn, &pc, escalate);
            let mut updated = pc;
            updated.rung = new_rung;
            *ws.pending_card.lock_poison_safe() = Some(updated);
            Vec::new()
        }

        response::CardKeyAction::Ask => Vec::new(),

        response::CardKeyAction::Response(verb) => {
            let Some(pc) = ws.pending_card.lock_poison_safe().take() else {
                return Vec::new();
            };
            apply_card_response(conn, &pc, verb, taxonomy)
        }
    }
}

/// T15: the offer-accept arm of the old `run_stdin_loop`, extracted
/// verbatim — pairs the `applied`/`prompt_response(accepted)` status+event
/// write, then (only if the slot is still free — the same guard the old
/// loop used) replays the judge-and-show path and sets `ws.pending_card`.
/// Returns notice lines for the activity log (no render — the TUI draws the
/// freshly-set pending card itself).
#[allow(clippy::too_many_arguments)]
pub fn apply_offer_accept(
    conn: &rusqlite::Connection,
    ws: &WatchSession,
    sid: &str,
    po: &PendingOffer,
    project_root: &Path,
    taxonomy: &[pack::TaxonomyConcept],
    canon: &[pack::CanonEntry],
    grammar: &pack::GrammarSpec,
    prompts: &pack::PromptFragments,
    models: &crate::Models,
) -> Vec<String> {
    // Card status + its prompt_response event derive noise/BKT state, so a
    // partial write between them must never be observable — pair them in
    // one transaction (best-effort outer semantics preserved via
    // warn_on_err).
    db::warn_on_err(
        db::with_tx(conn, |tx| {
            db::update_card_status_stmt(tx, po.card_id, db::CardStatus::Applied)?;
            db::log_event_stmt(
                tx,
                &db::EventRecord {
                    id: None,
                    session_id: sid.to_string(),
                    kind: "prompt_response".to_string(),
                    payload_json: serde_json::json!({
                        "verb": "accepted",
                        "signal": po.key.0,
                        "concept": po.key.1,
                    })
                    .to_string(),
                    ts: None,
                },
            )?;
            Ok(())
        }),
        "update_card_status+log_event(prompt_response accepted)",
    );

    let mut notices = Vec::new();
    if ws.pending_card.lock_poison_safe().is_none() {
        let snap = ws.snapshot.lock_poison_safe().clone();
        let rel_file = po
            .site_file
            .strip_prefix(project_root)
            .unwrap_or(&po.site_file)
            .to_string_lossy()
            .to_string();
        let now = std::time::SystemTime::now();
        match run_struggle_judge_and_show(
            conn,
            sid,
            project_root,
            &po.site_file,
            &snap,
            taxonomy,
            canon,
            grammar,
            prompts,
            models,
            ws,
        ) {
            Some(pc) => {
                *ws.pending_card.lock_poison_safe() = Some(pc);
                *ws.last_review.lock_poison_safe() = Some(super::LastReview {
                    file: rel_file,
                    result: super::ReviewResult::Suggested,
                    at: now,
                });
            }
            None => {
                // Founder dogfood 2026-07-05: an accepted offer that finds
                // nothing must NOT vanish silently. The "nothing new" notice
                // below goes to the activity log, which the redesigned home
                // surface no longer renders — so also record the mentor-state
                // outcome the idle surface DOES show ("looked at X — nothing
                // worth flagging"), giving the accepted offer a visible result.
                *ws.last_review.lock_poison_safe() = Some(super::LastReview {
                    file: rel_file,
                    result: super::ReviewResult::NothingToFlag,
                    at: now,
                });
                notices.push("nothing new to show at that site right now".to_string());
            }
        }
    }
    notices
}

/// T15: the offer-decline arm of the old `run_stdin_loop`, extracted
/// verbatim — pairs the `not_now`/`prompt_response(declined)` status+event
/// write, then applies req 13's two-declines-across-sessions suppression.
pub fn apply_offer_decline(conn: &rusqlite::Connection, sid: &str, po: &PendingOffer) {
    // Pair status + event in one transaction (see the Accept arm).
    db::warn_on_err(
        db::with_tx(conn, |tx| {
            db::update_card_status_stmt(tx, po.card_id, db::CardStatus::NotNow)?;
            db::log_event_stmt(
                tx,
                &db::EventRecord {
                    id: None,
                    session_id: sid.to_string(),
                    kind: "prompt_response".to_string(),
                    payload_json: serde_json::json!({
                        "verb": "declined",
                        "signal": po.key.0,
                        "concept": po.key.1,
                    })
                    .to_string(),
                    ts: None,
                },
            )?;
            Ok(())
        }),
        "update_card_status+log_event(prompt_response declined)",
    );
    // req 13: two declines across sessions for this concept -> 7-day
    // suppression.
    let declines = db::count_declined_offers_for_concept(conn, &po.key.1).unwrap_or(0);
    if offer::should_suppress_after_declines(declines) {
        let expiry = offer::suppression_expiry_epoch_secs(std::time::SystemTime::now());
        db::warn_on_err(
            db::insert_offer_suppression(conn, sid, &po.key.1, expiry),
            "insert_offer_suppression",
        );
    }
}

/// Redesign R5: why an ask-mode send didn't land — distinguishes the
/// unreachable-model case (never worth blaming the user's wording) from an
/// empty/unhelpful reply and a post-dispatch persistence failure, so the
/// caller's notice can stay calm and accurate (mirrors the comment-ask
/// failure-notice split in `sweep.rs`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AskSendError {
    /// The dispatch itself failed (rate-limited/offline/misconfigured) —
    /// the string is the raw reason, kept for logs/debugging only.
    Dispatch(String),
    /// The model replied, but with nothing (blank after trimming).
    Empty,
    /// The model answered, but the DB write failed.
    Persist(String),
}

/// Redesign R5: the plain-language, calm notice for an `AskSendError` — same
/// tone as `sweep::comment_ask_failure_notice` (never alarming, always a
/// clear next step or an honest "try again").
pub fn ask_failure_notice(err: &AskSendError) -> String {
    match err {
        AskSendError::Dispatch(_) => {
            "couldn't reach the model for that question (rate-limited or offline?) \u{2014} try again shortly"
                .to_string()
        }
        AskSendError::Empty => {
            "murshid didn't have anything to add \u{2014} try rephrasing the question".to_string()
        }
        AskSendError::Persist(_) => {
            "got an answer but couldn't save it \u{2014} try again".to_string()
        }
    }
}

/// Redesign R5 (ask mode): the BLOCKING model call `tui::mod::handle_key`
/// runs on a background thread (never the event loop) when `k`'s `\u{23ce}`
/// sends a question. Builds the conversational prompt from the card's own
/// concept/why/rule/grounding + its enclosing item + the prior thread turns
/// (read fresh via `db::get_thread_messages`, NOT a stale snapshot — a
/// second question in the same session must see the first exchange),
/// dispatches on the Interactive lane (a user-initiated call, same lane
/// `run_struggle_judge_and_show` uses, never aborted by a concurrent Sweep
/// dispatch), and on success persists BOTH turns via `db::append_ask_turn`.
/// The reply is free-form PROSE, persisted verbatim — no JSON/grounding
/// contract, no `parse_stage2_output`/`validate_stage2_output`.
pub fn apply_ask_send(
    conn: &rusqlite::Connection,
    pc: &PendingCard,
    question: &str,
    models: &crate::Models,
) -> Result<(), AskSendError> {
    let prior = db::get_thread_messages(conn, pc.card_id).unwrap_or_default();
    let history: Vec<thread::ThreadTurn> = prior
        .iter()
        .map(|m| thread::ThreadTurn {
            role: m.role.clone(),
            content: m.content.clone(),
        })
        .collect();
    let prompt = thread::build_ask_prompt(
        &pc.concept_name,
        &pc.card.why,
        &pc.card.rule,
        &pc.card.grounding_quote,
        pc.site_enclosing_item.as_deref(),
        &history,
        question,
    );

    let raw = models
        .judge
        .dispatch(provider::Lane::Interactive, &prompt)
        .map_err(AskSendError::Dispatch)?;
    let answer = raw.trim();
    if answer.is_empty() {
        return Err(AskSendError::Empty);
    }

    db::append_ask_turn(conn, &pc.session_id, pc.card_id, question, answer)
        .map_err(|e| AskSendError::Persist(e.to_string()))
}

/// T15: the single entry point the TUI calls for a key press while a
/// struggle offer is pending — classifies via [`offer::classify_offer_key`]
/// (IDENTICAL to the old stdin loop's offer-priority check) and dispatches
/// to the extracted functions above. The caller is expected to have already
/// cleared `ws.pending_offer` before calling this (mirrors the old loop:
/// the offer is consumed the instant y/n resolves it, before any DB work).
#[allow(clippy::too_many_arguments)]
pub fn handle_offer_key(
    conn: &rusqlite::Connection,
    ws: &WatchSession,
    sid: &str,
    po: &PendingOffer,
    action: offer::OfferKeyAction,
    project_root: &Path,
    taxonomy: &[pack::TaxonomyConcept],
    canon: &[pack::CanonEntry],
    grammar: &pack::GrammarSpec,
    prompts: &pack::PromptFragments,
    models: &crate::Models,
) -> Vec<String> {
    match action {
        offer::OfferKeyAction::Ignore => Vec::new(),
        offer::OfferKeyAction::Accept => apply_offer_accept(
            conn,
            ws,
            sid,
            po,
            project_root,
            taxonomy,
            canon,
            grammar,
            prompts,
            models,
        ),
        offer::OfferKeyAction::Decline => {
            apply_offer_decline(conn, sid, po);
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::card;
    use std::sync::Arc;

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
                card_body_json: None,
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
            suppression::SnoozeScope::Concept,
        )
        .unwrap();
        assert!(pull_is_blocked(&conn, "sess1", "borrow-vs-clone", "fp-x"));
    }

    #[test]
    fn test_pull_is_not_blocked_when_clear() {
        let conn = db::initialize_db(":memory:").unwrap();
        assert!(!pull_is_blocked(&conn, "sess1", "borrow-vs-clone", "fp-x"));
    }

    // --- T15 acceptance: `handle_card_key`/`handle_offer_key` — the exact
    // functions the TUI calls — must have IDENTICAL DB effects to the
    // retired stdin loop's inline arms. These drive the real functions
    // against a real (in-memory) DB and a real `WatchSession`, asserting on
    // the persisted `cards`/`events`/`concept_memory` rows, not just return
    // values — the same level the sweep flow tests already exercise. ---

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

    fn taxonomy_fixture() -> Vec<pack::TaxonomyConcept> {
        vec![pack::TaxonomyConcept {
            slug: "borrow-vs-clone".to_string(),
            name: "Borrow vs. clone".to_string(),
            category: pack::Category::Idiom,
        }]
    }

    fn session_with_pending_card(
        conn: &rusqlite::Connection,
        session_id: &str,
    ) -> (Arc<WatchSession>, i64) {
        let card_id = db::insert_card(
            conn,
            &db::CardRecord {
                id: None,
                session_id: session_id.to_string(),
                concept_id: "borrow-vs-clone".to_string(),
                category: "idiom".to_string(),
                rung_shown: "R2".to_string(),
                advice_fp: "fp-1".to_string(),
                finding_fp: None,
                status: "shown".to_string(),
                created_ts: None,
                resolved_ts: None,
                worked_diff: None,
                regresses_card_id: None,
                site_file: None,
                site_line: None,
                card_body_json: None,
            },
        )
        .unwrap();

        let now0 = std::time::SystemTime::now();
        let detent = crate::noise::detent_for("standard");
        let project_root = std::env::temp_dir();
        let ws = Arc::new(WatchSession::new(&project_root, now0, &detent));
        *ws.pending_card.lock_poison_safe() = Some(PendingCard {
            card_id,
            session_id: session_id.to_string(),
            concept_id: "borrow-vs-clone".to_string(),
            concept_name: "Borrow vs. clone".to_string(),
            advice_fp: "fp-1".to_string(),
            category: "idiom".to_string(),
            rung: ladder::Rung::R2,
            card: sample_card(),
            site_enclosing_item: None,
            site_anchor_hash: None,
            from_struggle_offer: false,
        });
        (ws, card_id)
    }

    fn card_status(conn: &rusqlite::Connection, card_id: i64) -> String {
        conn.query_row(
            "SELECT status FROM cards WHERE id = ?1",
            rusqlite::params![card_id],
            |r| r.get(0),
        )
        .unwrap()
    }

    #[test]
    fn test_handle_card_key_applied_records_status_frees_slot_and_records_evidence() {
        let conn = db::initialize_db(":memory:").unwrap();
        let (ws, card_id) = session_with_pending_card(&conn, "sess1");
        let taxonomy = taxonomy_fixture();

        let notices = handle_card_key(
            &conn,
            &ws,
            response::classify_card_key("a"),
            &taxonomy,
        );
        assert!(notices.is_empty());
        assert_eq!(card_status(&conn, card_id), "applied");
        assert!(
            ws.pending_card.lock_poison_safe().is_none(),
            "a resolved response frees the slot"
        );

        let events = db::get_events_for_session(&conn, "sess1").unwrap();
        let response_event = events
            .iter()
            .find(|e| e.kind == "card_response")
            .expect("a card_response event");
        assert!(response_event.payload_json.contains("\"verb\":\"applied\""));

        // T5 req 3(c): `applied` is evidence — a concept_memory row exists.
        assert!(db::get_concept_memory(&conn, "borrow-vs-clone").unwrap().is_some());
    }

    #[test]
    fn test_handle_card_key_got_it_records_status_and_is_not_evidence() {
        let conn = db::initialize_db(":memory:").unwrap();
        let (ws, card_id) = session_with_pending_card(&conn, "sess1");
        let taxonomy = taxonomy_fixture();

        handle_card_key(&conn, &ws, response::classify_card_key("g"), &taxonomy);
        assert_eq!(card_status(&conn, card_id), "got_it");
        assert!(ws.pending_card.lock_poison_safe().is_none());
        // got_it is a dismissal (I23), never mastery evidence.
        assert!(db::get_concept_memory(&conn, "borrow-vs-clone").unwrap().is_none());
    }

    #[test]
    fn test_handle_card_key_not_useful_records_status() {
        let conn = db::initialize_db(":memory:").unwrap();
        let (ws, card_id) = session_with_pending_card(&conn, "sess1");
        let taxonomy = taxonomy_fixture();

        handle_card_key(&conn, &ws, response::classify_card_key("u"), &taxonomy);
        assert_eq!(card_status(&conn, card_id), "not_useful");
        assert!(ws.pending_card.lock_poison_safe().is_none());
    }

    #[test]
    fn test_handle_card_key_not_now_records_status_and_snoozes_instance_scoped() {
        let conn = db::initialize_db(":memory:").unwrap();
        let (ws, card_id) = session_with_pending_card(&conn, "sess1");
        let taxonomy = taxonomy_fixture();

        handle_card_key(&conn, &ws, response::classify_card_key("n"), &taxonomy);
        assert_eq!(card_status(&conn, card_id), "not_now");
        assert!(ws.pending_card.lock_poison_safe().is_none());
        // First not_now this session -> instance-scoped snooze (D11(c)).
        assert!(db::is_suppressed(&conn, "sess1", "borrow-vs-clone", "fp-1").unwrap());
    }

    #[test]
    fn test_handle_card_key_escalate_bumps_rung_and_keeps_card_pending() {
        let conn = db::initialize_db(":memory:").unwrap();
        let (ws, card_id) = session_with_pending_card(&conn, "sess1");
        let taxonomy = taxonomy_fixture();

        let notices = handle_card_key(&conn, &ws, response::classify_card_key("e"), &taxonomy);
        assert!(notices.is_empty());
        let pc = ws.pending_card.lock_poison_safe().clone();
        assert!(pc.is_some(), "escalate does not consume the slot");
        assert_eq!(pc.unwrap().rung, ladder::escalate_one(ladder::Rung::R2));

        let stored_rung: String = conn
            .query_row(
                "SELECT rung_shown FROM cards WHERE id = ?1",
                rusqlite::params![card_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(stored_rung, ladder::escalate_one(ladder::Rung::R2).as_str());

        let events = db::get_events_for_session(&conn, "sess1").unwrap();
        assert!(events
            .iter()
            .any(|e| e.kind == "card_response" && e.payload_json.contains("\"verb\":\"escalated\"")));
    }

    /// Redesign R5: `handle_card_key`'s own `Ask` arm is a no-op (entering ask
    /// mode is now handled entirely at the TUI/`App` layer, before this
    /// function is ever reached for `k`) — no notice, card stays pending.
    #[test]
    fn test_handle_card_key_ask_is_a_no_op_and_leaves_card_pending() {
        let conn = db::initialize_db(":memory:").unwrap();
        let (ws, _card_id) = session_with_pending_card(&conn, "sess1");
        let taxonomy = taxonomy_fixture();

        let notices = handle_card_key(&conn, &ws, response::classify_card_key("k"), &taxonomy);
        assert!(notices.is_empty());
        assert!(ws.pending_card.lock_poison_safe().is_some());
    }

    // --- redesign R5: ask-mode failure notice wording ---

    #[test]
    fn test_ask_failure_notice_wording_by_reason() {
        assert!(
            ask_failure_notice(&AskSendError::Dispatch("timeout".to_string()))
                .contains("try again shortly")
        );
        assert!(ask_failure_notice(&AskSendError::Empty).contains("try rephrasing"));
        assert!(ask_failure_notice(&AskSendError::Persist("disk full".to_string()))
            .contains("try again"));
    }

    #[test]
    fn test_handle_card_key_ignore_for_unbound_keys() {
        let conn = db::initialize_db(":memory:").unwrap();
        let (ws, card_id) = session_with_pending_card(&conn, "sess1");
        let taxonomy = taxonomy_fixture();

        let notices = handle_card_key(&conn, &ws, response::classify_card_key("m"), &taxonomy);
        assert!(notices.is_empty());
        assert_eq!(card_status(&conn, card_id), "shown", "untouched");
        assert!(ws.pending_card.lock_poison_safe().is_some());
    }

    fn sample_offer(card_id: i64) -> PendingOffer {
        PendingOffer {
            key: ("error-streak", "E0308".to_string()),
            site_file: std::path::PathBuf::from("does/not/exist.rs"),
            fired_at: std::time::SystemTime::now(),
            card_id,
        }
    }

    #[test]
    fn test_handle_offer_key_decline_records_status_and_suppresses_after_two() {
        let conn = db::initialize_db(":memory:").unwrap();
        let card_id = db::insert_card(
            &conn,
            &db::CardRecord {
                id: None,
                session_id: "sess1".to_string(),
                concept_id: "E0308".to_string(),
                category: offer::OFFER_CATEGORY.to_string(),
                rung_shown: ladder::RungShown::Offer.as_str().to_string(),
                advice_fp: "struggle-offer:error-streak:E0308".to_string(),
                finding_fp: None,
                status: "shown".to_string(),
                created_ts: None,
                resolved_ts: None,
                worked_diff: None,
                regresses_card_id: None,
                site_file: None,
                site_line: None,
                card_body_json: None,
            },
        )
        .unwrap();
        let po = sample_offer(card_id);

        let taxonomy = taxonomy_fixture();
        let canon: Vec<pack::CanonEntry> = Vec::new();
        let grammar = pack::GrammarSpec::default();
        let prompts = pack::PromptFragments::default();
        let models = crate::Models {
            screen: crate::ResolvedSlot {
                provider: "ollama".to_string(),
                model: "x".to_string(),
                key: None,
                base_url: None,
                key_unreadable: false,
            },
            judge: crate::ResolvedSlot {
                provider: "ollama".to_string(),
                model: "x".to_string(),
                key: None,
                base_url: None,
                key_unreadable: false,
            },
        };
        let project_root = std::env::temp_dir();

        let notices = handle_offer_key(
            &conn,
            &Arc::new(WatchSession::new(
                &project_root,
                std::time::SystemTime::now(),
                &crate::noise::detent_for("standard"),
            )),
            "sess1",
            &po,
            offer::OfferKeyAction::Decline,
            &project_root,
            &taxonomy,
            &canon,
            &grammar,
            &prompts,
            &models,
        );
        assert!(notices.is_empty());
        assert_eq!(card_status(&conn, card_id), "not_now");
        let events = db::get_events_for_session(&conn, "sess1").unwrap();
        assert!(events.iter().any(|e| {
            e.kind == "prompt_response" && e.payload_json.contains("\"verb\":\"declined\"")
        }));

        // A second decline (a fresh session, same concept key) crosses the
        // req 13 threshold -> a 7-day offer suppression.
        let card_id2 = db::insert_card(
            &conn,
            &db::CardRecord {
                id: None,
                session_id: "sess2".to_string(),
                concept_id: "E0308".to_string(),
                category: offer::OFFER_CATEGORY.to_string(),
                rung_shown: ladder::RungShown::Offer.as_str().to_string(),
                advice_fp: "struggle-offer:error-streak:E0308".to_string(),
                finding_fp: None,
                status: "shown".to_string(),
                created_ts: None,
                resolved_ts: None,
                worked_diff: None,
                regresses_card_id: None,
                site_file: None,
                site_line: None,
                card_body_json: None,
            },
        )
        .unwrap();
        let po2 = sample_offer(card_id2);
        handle_offer_key(
            &conn,
            &Arc::new(WatchSession::new(
                &project_root,
                std::time::SystemTime::now(),
                &crate::noise::detent_for("standard"),
            )),
            "sess2",
            &po2,
            offer::OfferKeyAction::Decline,
            &project_root,
            &taxonomy,
            &canon,
            &grammar,
            &prompts,
            &models,
        );
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        assert!(
            db::is_offer_suppressed(&conn, "E0308", now_secs).unwrap(),
            "two declines across sessions must suppress the offer (req 13)"
        );
    }

    #[test]
    fn test_handle_offer_key_accept_records_applied_status_and_leaves_slot_free_when_no_diff() {
        let conn = db::initialize_db(":memory:").unwrap();
        let card_id = db::insert_card(
            &conn,
            &db::CardRecord {
                id: None,
                session_id: "sess1".to_string(),
                concept_id: "E0308".to_string(),
                category: offer::OFFER_CATEGORY.to_string(),
                rung_shown: ladder::RungShown::Offer.as_str().to_string(),
                advice_fp: "struggle-offer:error-streak:E0308".to_string(),
                finding_fp: None,
                status: "shown".to_string(),
                created_ts: None,
                resolved_ts: None,
                worked_diff: None,
                regresses_card_id: None,
                site_file: None,
                site_line: None,
                card_body_json: None,
            },
        )
        .unwrap();
        let po = sample_offer(card_id);

        let taxonomy = taxonomy_fixture();
        let canon: Vec<pack::CanonEntry> = Vec::new();
        let grammar = pack::GrammarSpec::default();
        let prompts = pack::PromptFragments::default();
        let models = crate::Models {
            screen: crate::ResolvedSlot {
                provider: "ollama".to_string(),
                model: "x".to_string(),
                key: None,
                base_url: None,
                key_unreadable: false,
            },
            judge: crate::ResolvedSlot {
                provider: "ollama".to_string(),
                model: "x".to_string(),
                key: None,
                base_url: None,
                key_unreadable: false,
            },
        };
        let project_root = std::env::temp_dir();
        let ws = Arc::new(WatchSession::new(
            &project_root,
            std::time::SystemTime::now(),
            &crate::noise::detent_for("standard"),
        ));

        // The offer's `site_file` doesn't exist on disk, so the judge-and-
        // show replay finds no diff and returns `None` WITHOUT ever
        // dispatching to a provider (no network call in this test).
        let notices = handle_offer_key(
            &conn,
            &ws,
            "sess1",
            &po,
            offer::OfferKeyAction::Accept,
            &project_root,
            &taxonomy,
            &canon,
            &grammar,
            &prompts,
            &models,
        );
        assert_eq!(notices, vec!["nothing new to show at that site right now"]);
        assert_eq!(card_status(&conn, card_id), "applied");
        assert!(ws.pending_card.lock_poison_safe().is_none());
        // Founder dogfood fix: a found-nothing accepted offer records a
        // visible mentor-state outcome (the idle surface renders it) rather
        // than only an unshown activity-log notice.
        let last = ws.last_review.lock_poison_safe().clone();
        assert_eq!(
            last.map(|r| r.result),
            Some(crate::watch::ReviewResult::NothingToFlag),
            "accepted offer that finds nothing must surface as NothingToFlag"
        );
        let events = db::get_events_for_session(&conn, "sess1").unwrap();
        assert!(events.iter().any(|e| {
            e.kind == "prompt_response" && e.payload_json.contains("\"verb\":\"accepted\"")
        }));
    }
}
