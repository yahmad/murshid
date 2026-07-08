//! T3 reqs 9/11-13, T17 R3 — the struggle-hint poll: on a timer, evaluates
//! idle-gating and signal convergence (repeated error / time-in-red / help
//! comment / T16b perception), and — the always-hint flip — fires the
//! direct hint INLINE at gate-pass, through the SAME composed learning-
//! memory gate (`suppression::hint_is_suppressed`) a sweep-pushed card goes
//! through. There is no more `[y/N]` consent dialogue: the judge runs the
//! moment the cadence/evidence/suppression gates all clear, and the
//! resulting card (if any) occupies the single `ws.pending_card` slot
//! directly — never a separate "offer" slot.

use std::path::Path;
use std::sync::Arc;
use std::time::SystemTime;

use crate::{db, offer, pack, struggle, suppression};

use super::{keys, WatchSession};
use crate::sync_ext::LockExt;

/// T3 reqs 9/11-13, T17 R3: the struggle-hint poll — evaluates idle-gating
/// and convergence on a timer (idle can only be known to have elapsed by
/// *not* seeing a file event, so this can't be driven from the file-event
/// callback alone), then runs the judge inline and shows the resulting card
/// directly, through the composed learning-memory gate. Moved verbatim off
/// `main()`'s inline poll-thread closure (T12); reworked from "fire an
/// offer, wait for `y`" to "fire the hint" (T17 R3).
#[allow(clippy::too_many_arguments)]
pub fn run_poll_loop(
    ws: &Arc<WatchSession>,
    project_root: &Path,
    taxonomy: &[pack::TaxonomyConcept],
    canon: &[pack::CanonEntry],
    grammar: &pack::GrammarSpec,
    prompts: &pack::PromptFragments,
    models: &crate::Models,
) {
    loop {
        std::thread::sleep(std::time::Duration::from_secs(3));

        let Some(dp) = db::get_db_path() else {
            continue;
        };
        let Ok(conn) = db::open_connection(&dp) else {
            continue;
        };
        let sid = ws.session_mgr.lock_poison_safe().session_id.clone();
        let now = SystemTime::now();
        let last_evt = *ws.last_event_at.lock_poison_safe();

        if ws.pending_card.lock_poison_safe().is_some() {
            continue; // never stack a hint atop a shown card — one slot
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
            let st = ws.struggle_tracking.lock_poison_safe();
            let same_error = st.error_streak.fired();
            let time_in_red = st.red_streak.fired(now_ms, st.baseline_ms);
            if struggle::inferred_pair_converged(same_error, time_in_red) {
                st.error_streak
                    .code()
                    .zip(st.struggle_site.clone())
                    .map(|(code, site)| {
                        (
                            offer::Evidence::ErrorStreak {
                                code: code.to_string(),
                                minutes: st.red_streak.minutes_in_red(now_ms),
                            },
                            site,
                        )
                    })
            } else if let Some((site, snippet)) = st.help_candidate.clone() {
                Some((offer::Evidence::HelpComment { snippet }, site))
            } else {
                // T16b: perception is a CANDIDATE GENERATOR ONLY — proposed
                // here, disposed of by this exact same gate (idle/
                // suppression/already_offered/one-at-a-time, unchanged
                // below). Existing mechanical proxies above (converged
                // pair, help comment) still take priority untouched;
                // perception only ever fills a gap they left, never
                // displaces them. The anti-nag confidence-floor /
                // fire-alone-vs-co-fire rule itself lives in
                // `offer::select_perceived_candidate` (pure, unit-tested).
                let mechanical_signal_active = same_error || time_in_red;
                let live_candidate = ws.perception_candidate.lock_poison_safe().clone();
                offer::select_perceived_candidate(live_candidate.as_ref(), mechanical_signal_active)
            }
        };
        let Some((evidence, site_file)) = candidate else {
            continue;
        };

        let last_success = ws.struggle_tracking.lock_poison_safe().last_check_success;
        if !offer::may_offer(&evidence, last_success, idle) {
            continue;
        }

        let key = offer::offer_key(&evidence);

        if ws
            .struggle_tracking
            .lock_poison_safe()
            .already_offered
            .contains(&key)
        {
            continue;
        }

        // T17 R3 (crux gap #2, part a): the cheap CONCEPT-level pre-check —
        // BEFORE spending judge tokens — for evidence that already names a
        // real taxonomy concept (T16b's `Perceived`). Mechanical evidence
        // (ErrorStreak/HelpComment) has no concept yet at this point (its
        // `key.1` is an E-code / raw snippet, not a taxonomy slug) — those
        // proceed straight to the judge, and the FULL post-judge gate (with
        // the real fp) is what actually protects them.
        if let offer::Evidence::Perceived { concept, .. } = &evidence {
            if let Some(taxon) = taxonomy.iter().find(|c| &c.slug == concept) {
                let now_secs = now
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs() as i64;
                let category_throttled = ws
                    .throttled_categories
                    .lock_poison_safe()
                    .contains(taxon.category.as_str());
                let pre_suppressed = suppression::hint_concept_pre_suppressed(
                    &conn,
                    concept,
                    &taxon.category,
                    category_throttled,
                    now_secs,
                )
                .unwrap_or(false);
                if pre_suppressed {
                    continue;
                }
            }
        }

        // R0/D16 (amended): the shared min_gap TokenBucket gates the fire —
        // consuming a token to attempt it, same as every other proactive
        // surface (`off` = capacity-0 = never ready = hints muted). No
        // borrow/accept-preempt path here (T17 R3 drops it — it only ever
        // made sense for an explicit accept keypress); the D16 likely-bug
        // strict-mode bypass stays exactly where it always lived, on the
        // SWEEP path (`budget::decide_push`), untouched by this module.
        let token_ok = {
            let mut b = ws.bucket.lock_poison_safe();
            b.try_consume(now)
        };
        if !token_ok {
            continue;
        }

        // Mark this (signal, key) as attempted BEFORE the judge dispatch —
        // there is no longer a separate bookkeeping `cards` row to gate on
        // (that synthetic-fp row is gone, T17 R3), so the one-shot-per-
        // session guard is applied at the point the attempt is spent.
        ws.struggle_tracking
            .lock_poison_safe()
            .already_offered
            .insert(key.clone());

        // T16c: preserve the model's own evidence sentence for a Perceived
        // candidate — threaded into the struggle judge so its response
        // addresses what perception already identified.
        let perceived_hint = match &evidence {
            offer::Evidence::Perceived {
                concept,
                evidence_line,
                ..
            } => Some(keys::PerceivedHint {
                concept,
                evidence_line,
            }),
            _ => None,
        };

        let snap = ws.snapshot.lock_poison_safe().clone();
        let rel_file = site_file
            .strip_prefix(project_root)
            .unwrap_or(&site_file)
            .to_string_lossy()
            .to_string();

        // T17 R3: the judge runs INLINE at gate-pass (not behind an accept)
        // — `run_struggle_judge_and_show` computes the real advice_fp and
        // applies the full composed learning-memory gate itself, before any
        // `cards` row lands (crux gap #2 / R2 gate wiring).
        match keys::run_struggle_judge_and_show(
            &conn,
            &sid,
            project_root,
            &site_file,
            &snap,
            taxonomy,
            canon,
            grammar,
            prompts,
            models,
            ws,
            perceived_hint.as_ref(),
            key.0,
        ) {
            Some(pc) => {
                ws.notice(offer::offer_line(&evidence));
                *ws.pending_card.lock_poison_safe() = Some(pc);
                *ws.last_review.lock_poison_safe() = Some(super::LastReview {
                    file: rel_file,
                    result: super::ReviewResult::Suggested,
                    at: now,
                });
            }
            None => {
                // Nothing to show (no new diff, OR the learning-memory gate
                // suppressed it) — never vanish silently; the idle surface
                // still gets a visible mentor-state outcome.
                *ws.last_review.lock_poison_safe() = Some(super::LastReview {
                    file: rel_file,
                    result: super::ReviewResult::NothingToFlag,
                    at: now,
                });
            }
        }
    }
}
