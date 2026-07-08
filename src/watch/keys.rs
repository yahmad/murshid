//! Card interaction logic for the watch loop: card lifecycle responses
//! (applied/got-it/not-now/not-useful/useful), rung escalation, and (T17 R3)
//! the direct-hint judge-and-show motion the offer-poll loop now runs
//! inline at gate-pass. Each keystroke maps to a C3 response and its
//! persisted event.
//!
//! T15: this module's DB-mutating bodies are plain callable functions
//! (`handle_card_key` and the `apply_*` helpers it delegates to) rather than
//! being inlined in a blocking stdin-reading loop — the retired
//! `run_stdin_loop` used to own them directly. The TUI (`crate::tui`) is the
//! only caller now; it maps a crossterm key event to
//! `response::classify_card_key`, then calls the same functions here, so
//! a/g/u/n/y/e/t have IDENTICAL DB effects to the pre-T15 loop.
//!
//! T17 R3: the offer's `[y/N]` consent dialogue (`apply_offer_accept`/
//! `apply_offer_decline`/`handle_offer_key`) is deleted — `watch::offers`
//! now calls [`run_struggle_judge_and_show`] directly at gate-pass, through
//! the same composed learning-memory gate (`suppression::hint_is_suppressed`)
//! a sweep-pushed card goes through.
//!
//! Redesign R5: `k` (ask) is real now — entering ask mode is a pure `App`
//! state change owned entirely by `tui::mod::handle_key` (no DB effect, so
//! `handle_card_key`'s own `Ask` arm below is a no-op left for match
//! exhaustiveness/safety only); the actual conversational dispatch +
//! persistence is [`apply_ask_send`], called from the same background-
//! thread/busy-guard pattern the direct-hint dispatch already established.
//!
//! Rendering is deliberately NOT done here: the TUI always redraws its
//! views fresh from `WatchSession`/`profile.db` state on every tick, so
//! these functions only mutate state and return short notice strings for
//! the activity log — never a full card render.

use std::path::Path;

use crate::{db, ladder, memory, pack, pipeline, provider, response, session, site, suppression, thread};

use super::{PendingCard, WatchSession};
use crate::sync_ext::LockExt;

/// T16c: the perceived concept + evidence from a `Perceived` struggle
/// candidate (T16b), threaded into the struggle judge's stage-1 dispatch so
/// the response addresses what perception already identified — the arc AND
/// the concept — instead of re-deriving a candidate from scratch. `None`
/// when the firing evidence was mechanical (error-streak/help-comment),
/// which behaves exactly as before this task (arc framing only, no bias).
/// `pub(super)`: T17 R3 moved this construction to `watch::offers`, the
/// sibling module that now owns the fire path.
pub(super) struct PerceivedHint<'a> {
    pub(super) concept: &'a str,
    pub(super) evidence_line: &'a str,
}

/// T16c: appends the perceived concept + evidence to a stage-1 prompt
/// already built by `pipeline::build_stage1_prompt`, biasing the screen
/// model toward the concept perception already named. Pure (no I/O, no
/// dispatch) — the prompt text is passed straight through byte-identical
/// when `hint` is `None`, the exact "mechanical offer, arc framing only"
/// backward-compatible case.
fn augment_stage1_prompt_with_perceived_hint(prompt: &str, hint: Option<&PerceivedHint>) -> String {
    match hint {
        Some(h) => format!(
            "{prompt}\n\nPerception already flagged this struggle before you were asked to \
             look: the developer appears stuck on `{}` \u{2014} {}. Strongly prefer this \
             concept as your candidate if the accumulated diff above supports it; only pick a \
             different one if it clearly doesn't fit.\n",
            h.concept, h.evidence_line
        ),
        None => prompt.to_string(),
    }
}

/// T3 req 11, amended T17 R3: "the judge runs on the struggle site and
/// shows the card through the normal slot" — now run INLINE at gate-pass
/// (cadence/evidence gates + the composed learning-memory gate), not behind
/// an accept keypress. A reduced, single-file replay of the watcher's
/// sweep-and-show path. Ledger/cooldown/suppression gates ARE (T17 R3)
/// re-applied here, right after the judge names the real concept/site and
/// BEFORE any `cards` row is inserted — see the `hint_is_suppressed` call
/// below — so a suppressed concept's judge output is discarded, never
/// shown, and never persisted.
///
/// T15: no longer renders/prints the card itself — the caller sets
/// `ws.pending_card` from the returned value, and the TUI redraws it fresh
/// from that live state on its next tick.
///
/// T16c: judges the ACCUMULATED struggle arc rather than a single latest
/// hunk — the diff baseline prefers the "since murshid last engaged"
/// snapshot (`ws.last_engaged_snapshot`, stamped the last time a struggle
/// response shipped) over the I1/C2 session-start `snapshot`, falling back
/// to it when there's no engaged-baseline entry yet (this session's first
/// struggle response) — see `session::compute_since_engaged_diff`. When the
/// firing evidence was T16b's `Perceived` evidence, `perceived_hint` biases
/// stage 1 toward the concept perception already named
/// (`augment_stage1_prompt_with_perceived_hint`). Already flows through
/// T16a's model-directed context path — `pipeline::judge_hunks` is the same
/// function the sweep dispatches through, context-request leg and all.
///
/// `pub(super)`: T17 R3's sole caller is the sibling `watch::offers` poll
/// loop (the direct-hint fire path); `signal` is the firing evidence's
/// `offer::offer_key` tag (`"error-streak"`/`"help-comment"`/`"perceived"`),
/// carried through only for the `hint_shown` event's audit payload (I11).
#[allow(clippy::too_many_arguments)]
pub(super) fn run_struggle_judge_and_show(
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
    perceived_hint: Option<&PerceivedHint>,
    signal: &str,
) -> Option<PendingCard> {
    let engaged = ws.last_engaged_snapshot.lock_poison_safe().clone();
    let hunks =
        session::compute_since_engaged_diff(project_root, site_file, &engaged, snapshot).ok()?;
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
    // lane, never aborted by a concurrent Sweep dispatch. T16c: the raw
    // stage-1 prompt (already built by `pipeline::build_stage1_prompt`
    // inside `judge_hunks`) is augmented with the perceived concept +
    // evidence, if any, right before dispatch — no change needed to the
    // shared `judge_hunks`/`build_stage1_prompt` the sweep also uses.
    let dispatch_stage1 = |prompt: &str| -> Result<String, String> {
        let augmented = augment_stage1_prompt_with_perceived_hint(prompt, perceived_hint);
        models
            .screen
            .dispatch(provider::Lane::Interactive, &augmented)
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
    // T17 R3 (crux gap #2): the REAL advice_fp, computed the moment the
    // judge names the real concept + site — no more synthetic
    // `struggle-offer:{signal}:{concept}` fp on a shown card, so the I3
    // ledger / cross-session shown-and-ignored tally actually apply to it.
    let advice_fp = site::advice_fingerprint(&stage2.concept, &site);
    let category = pack::Category::parse(&stage2.category);

    // T17 R3: the full composed learning-memory gate, now that the real fp
    // exists — session dedup / ledger / ignored-tally need exactly this fp,
    // which didn't exist before the judge ran. Checked BEFORE any `cards`
    // row lands: a suppressed concept's judge output is discarded here,
    // never inserted, never shown.
    let now_epoch_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let category_throttled = ws.throttled_categories.lock_poison_safe().contains(&stage2.category);
    let suppressed = suppression::hint_is_suppressed(
        conn,
        session_id,
        &stage2.concept,
        &advice_fp,
        &category,
        category_throttled,
        now_epoch_secs,
    )
    .unwrap_or(false);
    if suppressed {
        return None;
    }

    // T5 req 4: memory-driven entry rung, resolved for THIS concept now
    // that stage-2 has named it. T15 settings overlay: reads
    // `ws.directness` fresh (via `resolve_entry_rung`), not a value frozen
    // earlier — a settings-overlay change is honored on this very call.
    let entry_rung = super::resolve_entry_rung(conn, ws, &stage2.concept, &stage2.category);

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
    // T17 R3 (T16 carry-over): logged where `prompt_offered` used to be —
    // the bookend's struggled-concepts recap (`db::struggled_concepts_this_session`)
    // reads this event now (and still accepts old `prompt_offered` rows from
    // pre-R3 sessions).
    let _ = db::log_event(
        conn,
        &db::EventRecord {
            id: None,
            session_id: session_id.to_string(),
            kind: "hint_shown".to_string(),
            payload_json: serde_json::json!({
                "concept": stage2.concept,
                "signal": signal,
                "fp": advice_fp,
            })
            .to_string(),
            ts: None,
        },
    );

    // T16c: this response IS an engagement — stamp the baseline now, using
    // the exact content the judge just reasoned over, so a LATER struggle
    // response at this same file starts from here rather than re-showing
    // ground this card already covered. Never touches `ws.snapshot` (the
    // I1/C2 session-start baseline the ordinary sweep's advice-window and
    // the applied-detection anchor key off).
    session::stamp_engaged_baseline(
        &mut ws.last_engaged_snapshot.lock_poison_safe(),
        site_file,
        &content,
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

    // T17 R2: a `not_useful` dismissal ALSO seeds the cross-session,
    // concept-scoped DECAYING learning-memory suppression — in addition to
    // the site-fp ledger recorded by `update_card_status` above — so
    // dismissing a hint quiets the whole CONCEPT for a spaced window, not
    // just that one code site (crux gap #3), and a repeat `not_useful`
    // lengthens the window (the count of prior rows drives the decay). The
    // prior count is read once, outside any retry; best-effort — the site
    // ledger already blocks the exact fp even if this insert fails.
    if verb == response::ResponseVerb::NotUseful {
        let prior =
            db::count_hint_concept_suppressions_for_concept(conn, &pc.concept_id).unwrap_or(0);
        let expiry = suppression::hint_concept_suppression_expiry_epoch_secs(
            std::time::SystemTime::now(),
            prior,
        );
        db::warn_on_err(
            db::insert_hint_concept_suppression(conn, &pc.session_id, &pc.concept_id, expiry),
            "insert_hint_concept_suppression (T17 R2)",
        );
    }

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::card;
    use std::sync::Arc;

    // --- T16c: perceived-hint threading into the stage-1 prompt ---

    #[test]
    fn test_augment_stage1_prompt_with_perceived_hint_is_a_no_op_when_none() {
        let prompt = "File: src/main.rs\n\n--- hunk ---\n";
        assert_eq!(
            augment_stage1_prompt_with_perceived_hint(prompt, None),
            prompt,
            "a mechanical offer (no hint) must dispatch the byte-identical prompt"
        );
    }

    #[test]
    fn test_augment_stage1_prompt_with_perceived_hint_appends_concept_and_evidence() {
        let prompt = "File: src/parse_config.rs\n\n--- hunk ---\n";
        let hint = PerceivedHint {
            concept: "ownership",
            evidence_line: "looks like you're circling ownership in parse_config",
        };
        let augmented = augment_stage1_prompt_with_perceived_hint(prompt, Some(&hint));
        assert!(augmented.starts_with(prompt), "original prompt preserved");
        assert!(augmented.contains("`ownership`"));
        assert!(augmented.contains("looks like you're circling ownership in parse_config"));
    }

    // --- T16c: "since last engaged" baseline wiring at the struggle-accept
    // integration level (not just the pure session::compute_since_engaged_diff
    // unit test) ---

    fn init_git_repo(dir: &std::path::Path) {
        let run = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(dir)
                .output()
                .unwrap()
        };
        run(&["init", "-q"]);
        run(&[
            "-c",
            "user.email=test@test.com",
            "-c",
            "user.name=Test",
            "commit",
            "--allow-empty",
            "-q",
            "-m",
            "init",
        ]);
    }

    /// T16c wiring smoke test (T17 R3: `run_struggle_judge_and_show` is now
    /// called directly, no accept keypress involved) — genuinely reads
    /// `ws.last_engaged_snapshot` (not just `ws.snapshot`) — stamping the
    /// engaged baseline at the file's CURRENT content makes the accumulated
    /// diff empty, so the call short-circuits to `None` ("nothing new") with
    /// NO live dispatch (deterministic, no network needed). The
    /// discriminating correctness proof for the baseline-PREFERENCE logic
    /// itself (engaged over session-start, with the session-start fallback)
    /// is the dedicated fixture test in `session.rs`
    /// (`test_compute_since_engaged_diff_prefers_the_engaged_baseline_over_session_start`)
    /// — asserting that distinction HERE would require a live dispatch (this
    /// call site's stage-1/stage-2 closures aren't injectable the way
    /// `judge_hunks`'s test harness is), which the T1 acceptance note
    /// forbids in tests.
    #[test]
    fn test_run_struggle_judge_and_show_reads_the_engaged_baseline_and_skips_dispatch_when_unchanged(
    ) {
        let root = std::env::temp_dir().join(format!(
            "murshid_t16c_engaged_baseline_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        init_git_repo(&root);
        let file = root.join("lib.rs");
        std::fs::write(&file, "fn a() {}\n").unwrap();
        std::process::Command::new("git")
            .args(["add", "-A"])
            .current_dir(&root)
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args([
                "-c",
                "user.email=test@test.com",
                "-c",
                "user.name=Test",
                "commit",
                "-q",
                "-m",
                "add file",
            ])
            .current_dir(&root)
            .output()
            .unwrap();

        let conn = db::initialize_db(":memory:").unwrap();
        let ws = Arc::new(WatchSession::new(
            &root,
            std::time::SystemTime::now(),
            std::time::Duration::from_secs(600),
        ));

        // Pretend murshid already engaged on this exact file at its CURRENT
        // content — no new struggle since then.
        session::stamp_engaged_baseline(
            &mut ws.last_engaged_snapshot.lock_poison_safe(),
            std::path::Path::new("lib.rs"),
            "fn a() {}\n",
        );

        let snap = ws.snapshot.lock_poison_safe().clone();
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

        // The file's content EXACTLY matches the engaged-baseline stamp —
        // even though it differs from the (older) session-start snapshot —
        // so there's nothing NEW to judge and no dispatch should occur.
        let result = run_struggle_judge_and_show(
            &conn,
            "sess1",
            &root,
            &root.join("lib.rs"),
            &snap,
            &taxonomy,
            &canon,
            &grammar,
            &prompts,
            &models,
            &ws,
            None,
            "error-streak",
        );
        assert!(
            result.is_none(),
            "must diff against the engaged baseline (identical content), not session-start \
             (which would show a stale, already-taught diff)"
        );
        assert!(ws.pending_card.lock_poison_safe().is_none());

        let _ = std::fs::remove_dir_all(&root);
    }

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

    // --- T15 acceptance: `handle_card_key` — the exact function the TUI
    // calls — must have IDENTICAL DB effects to the retired stdin loop's
    // inline arms. These drive the real function against a real
    // (in-memory) DB and a real `WatchSession`, asserting on the persisted
    // `cards`/`events`/`concept_memory` rows, not just return
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
        let project_root = std::env::temp_dir();
        let ws = Arc::new(WatchSession::new(&project_root, now0, std::time::Duration::from_secs(600)));
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

    /// T17 R2 acceptance (crux gap #3): dismissing a hint with `u` seeds a
    /// cross-session, CONCEPT-scoped suppression — so a DIFFERENT code site
    /// (a different advice_fp) of the same concept is also quieted, not just
    /// the one site the card pointed at. Proven end-to-end through the real
    /// `apply_card_response` path and the composed `hint_is_suppressed` gate.
    #[test]
    fn test_not_useful_seeds_concept_scoped_hint_suppression_cross_site() {
        let conn = db::initialize_db(":memory:").unwrap();
        let (ws, _card_id) = session_with_pending_card(&conn, "sess1");
        let taxonomy = taxonomy_fixture();

        // Before the dismissal the concept is not hint-suppressed.
        assert!(!db::is_hint_concept_suppressed(&conn, "borrow-vs-clone", 0).unwrap());

        handle_card_key(&conn, &ws, response::classify_card_key("u"), &taxonomy);

        // `u` recorded a concept-scoped, cross-session hint suppression.
        assert!(
            db::is_hint_concept_suppressed(&conn, "borrow-vs-clone", 0).unwrap(),
            "not_useful must seed a hint-concept suppression"
        );
        // A DIFFERENT site (fp) of the SAME concept is now suppressed too.
        assert!(
            suppression::hint_is_suppressed(
                &conn,
                "sess1",
                "borrow-vs-clone",
                "fp-a-totally-different-site",
                &crate::pack::Category::Idiom,
                false,
                0,
            )
            .unwrap(),
            "gap #3: dismissing one site quiets the whole concept, cross-site"
        );
    }

    /// T17 R3 acceptance: `y` (👍 useful) end-to-end through the real
    /// `handle_card_key` path — the CHECK-constraint lesson from R2 says
    /// "don't trust compilation, run the actual insert", so this asserts the
    /// real `cards.status = 'useful'` row lands, that it ledger-blocks the
    /// EXACT fp (but does NOT concept-suppress — a DIFFERENT site of the
    /// same concept stays eligible), and that it counts as positive
    /// engagement in the D12 action-rate computation.
    #[test]
    fn test_handle_card_key_useful_end_to_end() {
        let conn = db::initialize_db(":memory:").unwrap();
        let (ws, card_id) = session_with_pending_card(&conn, "sess1");
        let taxonomy = taxonomy_fixture();

        let notices = handle_card_key(&conn, &ws, response::classify_card_key("y"), &taxonomy);
        assert!(notices.is_empty());

        // The status actually landed (not just compiled) — no CHECK
        // constraint guards `cards.status`, but the real INSERT is the only
        // trustworthy proof.
        assert_eq!(card_status(&conn, card_id), "useful");
        assert!(
            ws.pending_card.lock_poison_safe().is_none(),
            "a resolved response frees the slot"
        );

        // `useful` is NOT mastery evidence (deferred signal, same rule as R2).
        assert!(db::get_concept_memory(&conn, "borrow-vs-clone").unwrap().is_none());

        // Ledger-blocks the EXACT fp ("fp-1", from `session_with_pending_card`).
        let (_, ledger_status) = db::find_ledger_card(&conn, "fp-1").unwrap().unwrap();
        assert_eq!(ledger_status, "useful");

        // But does NOT concept-suppress — a DIFFERENT site of the SAME
        // concept stays eligible ("more like this" keeps the concept live).
        assert!(
            !db::is_hint_concept_suppressed(&conn, "borrow-vs-clone", 0).unwrap(),
            "useful must never seed a hint-concept suppression (unlike not_useful)"
        );
        assert!(
            !suppression::hint_is_suppressed(
                &conn,
                "sess1",
                "borrow-vs-clone",
                "fp-a-totally-different-site",
                &crate::pack::Category::Idiom,
                false,
                0,
            )
            .unwrap(),
            "a different site of the same concept must stay eligible after `useful`"
        );

        // Counts as positive engagement in the D12 EFP/channel-health window.
        let statuses = db::recent_card_statuses_for_category(&conn, "idiom", 20).unwrap();
        let rate = crate::throttle::action_rate(&statuses).unwrap();
        assert!(
            (rate - 1.0).abs() < 1e-9,
            "a lone `useful` card must count as fully-engaged: {rate}"
        );
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

}
