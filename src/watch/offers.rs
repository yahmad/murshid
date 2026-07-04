//! The struggle-offer poll loop (D15): on a timer, evaluates idle-gating and
//! signal convergence (repeated error / time-in-red / help comment) and
//! fires at most one proactive help *offer* at a time — never an unsolicited
//! hint. A live offer expires silently if the user keeps typing (I10).

use std::sync::Arc;

use crate::{db, offer, struggle};

use super::{PendingOffer, WatchSession};

/// T3 reqs 9/11-13: the struggle-offer poll — evaluates idle-gating and
/// convergence on a timer (idle can only be known to have elapsed by *not*
/// seeing a file event, so this can't be driven from the file-event
/// callback alone), fires at most one offer at a time, and expires it
/// silently if the user goes back to typing (I10). Moved verbatim off
/// `main()`'s inline poll-thread closure (T12).
pub fn run_poll_loop(ws: &Arc<WatchSession>) {
    loop {
        std::thread::sleep(std::time::Duration::from_secs(3));

        let Some(dp) = db::get_db_path() else {
            continue;
        };
        let Ok(conn) = db::open_connection(&dp) else {
            continue;
        };
        let sid = ws
            .session_mgr
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .session_id
            .clone();
        let now = std::time::SystemTime::now();
        let last_evt = *ws.last_event_at.lock().unwrap_or_else(|e| e.into_inner());

        // I10: continuing to type expires a live offer
        // silently — no decline persistence penalty.
        {
            let live = ws
                .pending_offer
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            if let Some(po) = live {
                if offer::expired_by_continued_typing(po.fired_at, last_evt) {
                    db::warn_on_err(
                        db::update_card_status(&conn, po.card_id, "expired"),
                        "update_card_status",
                    );
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
                    *ws.pending_offer.lock().unwrap_or_else(|e| e.into_inner()) = None;
                }
                continue; // at most one live offer at a time
            }
        }

        if ws
            .pending_card
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
        {
            continue; // never stack an offer atop a shown card
        }

        // req 12: offers share D12's auto-throttle too.
        if ws
            .throttled_categories
            .lock()
            .unwrap_or_else(|e| e.into_inner())
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
            let st = ws
                .struggle_tracking
                .lock()
                .unwrap_or_else(|e| e.into_inner());
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
            } else {
                st.help_candidate
                    .clone()
                    .map(|(site, snippet)| (offer::Evidence::HelpComment { snippet }, site))
            }
        };
        let Some((evidence, site_file)) = candidate else {
            continue;
        };

        let last_success = ws
            .struggle_tracking
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .last_check_success;
        if !offer::may_offer(&evidence, last_success, idle) {
            continue;
        }

        let key = offer::offer_key(&evidence);

        if ws
            .struggle_tracking
            .lock()
            .unwrap_or_else(|e| e.into_inner())
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
        ws.struggle_tracking
            .lock()
            .unwrap_or_else(|e| e.into_inner())
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
        *ws.pending_offer.lock().unwrap_or_else(|e| e.into_inner()) = Some(PendingOffer {
            key,
            site_file,
            fired_at: now,
            card_id,
        });
    }
}
