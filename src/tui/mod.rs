//! T15 (MUR-7 spike) — the full-screen ratatui TUI that replaces `watch`'s
//! terminal presentation (Decision 2). This module (and `app`/`view` below
//! it) is the ONLY place `ratatui`/`crossterm` types may appear anywhere in
//! the crate; the engine (`watch`, `db`, the pure render functions) stays
//! stdlib + the C10 allowlist and never depends on this module.
//!
//! Terminal lifecycle: raw mode + the alternate screen are entered once at
//! startup and always restored — on a normal quit AND via a panic hook, so
//! a panic never leaves the user's terminal wedged (the feasibility doc's
//! flagged hazard). Resize (SIGWINCH) needs no special handling: ratatui's
//! `Terminal::draw` calls `autoresize()` internally on every frame, so the
//! next tick's layout already reflects the new size.
//!
//! Event loop: a crossterm poll with a timeout on the main thread — the
//! same thread `watch::run`'s old keep-alive/shutdown-poll loop used to
//! own. The background workers (offer-poll, file-event/sweep) keep running
//! on their own threads exactly as before; this loop only reads their
//! shared `WatchSession` state and redraws.
//!
//! Quit: Ctrl-C arrives as a key event in raw mode (crossterm disables
//! signal generation for it), so the TUI owns it directly as "quit" — no
//! race with the old SIGINT path. A real SIGINT (`kill -INT`, or a
//! terminal that doesn't honor ISIG) still sets `watch::shutdown_requested`
//! asynchronously; this loop polls that flag too. Either way, quitting
//! restores the terminal FIRST, then runs `watch::run_shutdown_cleanup`
//! (the same session-end bookend + cleanup the old SIGINT path ran) — a TUI
//! exit is never lossier than the old one.

pub mod app;
pub mod theme;
pub mod view;

use std::io::stdout;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::Terminal;

use crate::sync_ext::LockExt;
use crate::watch::{self, keys, PendingOffer, WatchSession};
use crate::{noise, offer, pack, response};

use app::{App, Focus};
use view::DrawContext;

type Term = Terminal<CrosstermBackend<std::io::Stdout>>;

fn install_panic_hook() {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(stdout(), LeaveAlternateScreen);
        prev(info);
    }));
}

fn enter_terminal() -> std::io::Result<Term> {
    enable_raw_mode()?;
    // If entering the alt screen fails after raw mode is on, undo raw mode
    // before propagating — otherwise the error path leaves the user's
    // terminal in raw mode (a wedge only `reset` clears). (Gate note, T15.)
    if let Err(e) = execute!(stdout(), EnterAlternateScreen) {
        let _ = disable_raw_mode();
        return Err(e);
    }
    // Same guard for the backend construction: if it fails after the alt
    // screen is up, undo BOTH before propagating (the panic hook doesn't
    // fire on a plain Err) — else `run` exits(1) with a wedged terminal.
    // (Gate finding, T15.)
    match Terminal::new(CrosstermBackend::new(stdout())) {
        Ok(t) => Ok(t),
        Err(e) => {
            let _ = execute!(stdout(), LeaveAlternateScreen);
            let _ = disable_raw_mode();
            Err(e)
        }
    }
}

fn restore_terminal() {
    let _ = disable_raw_mode();
    let _ = execute!(stdout(), LeaveAlternateScreen);
}

/// The TUI entry point — replaces the old keep-alive/shutdown-poll loop at
/// the bottom of `watch::run`. Never returns: quitting always ends in
/// `restore_terminal` + `watch::run_shutdown_cleanup` + `process::exit(0)`.
#[allow(clippy::too_many_arguments)]
pub fn run(
    ws: Arc<WatchSession>,
    project_root: PathBuf,
    taxonomy: Vec<pack::TaxonomyConcept>,
    canon: Vec<pack::CanonEntry>,
    grammar: pack::GrammarSpec,
    prompts: pack::PromptFragments,
    models: crate::Models,
    surface: pack::SurfaceConfig,
    mode: crate::judge::JudgeMode,
) -> ! {
    install_panic_hook();
    let mut terminal = match enter_terminal() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("[murshid] failed to start the TUI terminal: {}", e);
            std::process::exit(1);
        }
    };

    let db_path = crate::db::get_db_path();
    let conn = db_path.as_ref().and_then(|p| crate::db::open_connection(p).ok());

    let mut app = App::new();

    // T15 terminal-lifecycle hazard: if the underlying tty/pty disappears
    // without delivering a signal (observed in testing: a closed pty
    // reports POLLHUP, so `event::poll` returns `Ok(true)` IMMEDIATELY
    // forever, even though nothing real is ever readable), the 200ms poll
    // timeout below stops being a real wait and this loop would otherwise
    // spin at 100% CPU indefinitely. Guarded by a circuit breaker: a real
    // human cannot sustain a key press faster than a few ms apart, so N
    // consecutive sub-5ms iterations that did NOT yield a real keypress is
    // conclusively a dead terminal, not a fast typist — bail out and run
    // the normal shutdown path rather than spin forever.
    const SPIN_GUARD_THRESHOLD: u32 = 200;
    let mut consecutive_fast_empty_polls: u32 = 0;

    loop {
        if watch::shutdown_requested() {
            break;
        }

        // Step 2: one tick per poll iteration (~200ms) — the header pulse's
        // and the "thinking" face's animation frame index (design doc
        // §3.4: "index the frame by a tick counter"). `wrapping_add` so a
        // very long session never panics on overflow; the frame index only
        // ever consumes it modulo the frame count.
        app.tick = app.tick.wrapping_add(1);

        let ctx = DrawContext {
            ws: &ws,
            conn: conn.as_ref(),
            project_root: &project_root,
            taxonomy: &taxonomy,
            mode: &mode,
            surface: &surface,
        };
        let _ = terminal.draw(|f| view::draw(f, &app, &ctx));

        let poll_started = std::time::Instant::now();
        let mut handled_real_key = false;
        match event::poll(Duration::from_millis(200)) {
            Ok(true) => match event::read() {
                Ok(Event::Key(key)) if key.kind == KeyEventKind::Press => {
                    handled_real_key = true;
                    handle_key(
                        &mut app,
                        &ws,
                        conn.as_ref(),
                        &project_root,
                        &taxonomy,
                        &canon,
                        &grammar,
                        &prompts,
                        &models,
                        key,
                    );
                }
                Ok(_) => {}
                Err(_) => break,
            },
            Ok(false) => {}
            Err(_) => break,
        }

        if handled_real_key || poll_started.elapsed() >= Duration::from_millis(5) {
            consecutive_fast_empty_polls = 0;
        } else {
            consecutive_fast_empty_polls += 1;
            if consecutive_fast_empty_polls >= SPIN_GUARD_THRESHOLD {
                break;
            }
        }

        if app.should_quit {
            break;
        }
    }

    restore_terminal();
    watch::run_shutdown_cleanup();
    std::process::exit(0);
}

/// The settings overlay's `\u{2190}`/`\u{2192}` (and Enter, forward): `row` 0
/// is frequency, `row` 1 is directness (mirrors `App::settings_selected`'s
/// indexing and `view::settings_rows`' order). Both writes are session-
/// scoped only — never persisted to `config.toml` (out of scope; a naive
/// rewrite risks clobbering the user's file/comments). Frequency additionally
/// updates the LIVE `TokenBucket`'s refill rate (`set_refill_period`) in the
/// same call, so the "next nudge" ETA reflects the change immediately; the
/// label alone (`ws.frequency`) would otherwise silently drift from the
/// bucket's actual rate.
fn apply_settings_cycle(ws: &WatchSession, row: usize, forward: bool) {
    match row {
        0 => {
            let current = ws.frequency.lock_poison_safe().clone();
            let next = if forward {
                noise::next_frequency(&current)
            } else {
                noise::prev_frequency(&current)
            };
            *ws.frequency.lock_poison_safe() = next.to_string();
            let detent = noise::detent_for(next);
            ws.bucket.lock_poison_safe().set_refill_period(detent.refill_period);
        }
        1 => {
            let current = *ws.directness.lock_poison_safe();
            let next = if forward { current.next() } else { current.prev() };
            *ws.directness.lock_poison_safe() = next;
        }
        _ => {}
    }
}

/// Maps one crossterm key event to an action. Overlay summon/pop and list
/// navigation (Mastery/Events/ConceptDetail) are handled entirely in `App`'s
/// `Focus` stack (design doc §5.2); a focused card or pending offer on the
/// `Home` surface is handled by reusing the SAME `keys::handle_card_key` /
/// `keys::handle_offer_key` functions the retired stdin loop's inline arms
/// called — this is the T15 requirement that a/g/u/n/e/t and offer y/n
/// behave IDENTICALLY to the pre-T15 loop, unchanged by the UX redesign.
/// Clears `WatchSession::busy` on drop — so a background dispatch releases the
/// single-flight/"working" flag even if it panics (a stuck `busy` would
/// otherwise wedge the UI in "working…" and block all further accepts).
struct BusyGuard(Arc<WatchSession>);
impl Drop for BusyGuard {
    fn drop(&mut self) {
        *self.0.busy.lock_poison_safe() = None;
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_key(
    app: &mut App,
    ws: &Arc<WatchSession>,
    conn: Option<&rusqlite::Connection>,
    project_root: &Path,
    taxonomy: &[pack::TaxonomyConcept],
    canon: &[pack::CanonEntry],
    grammar: &pack::GrammarSpec,
    prompts: &pack::PromptFragments,
    models: &crate::Models,
    key: KeyEvent,
) {
    // The help overlay swallows the next key to dismiss itself — it never
    // reaches the tab/card/offer bindings below.
    if app.show_help {
        app.show_help = false;
        return;
    }

    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        app.should_quit = true;
        return;
    }

    // Inline goal editor (founder request): while open, ALL keys are text —
    // 'q'/'m'/'e'/etc. type into the goal instead of firing commands. Enter
    // persists via the SAME `goal::write_goal_file` the CLI uses; Esc cancels.
    // (Ctrl-C above still hard-quits.)
    if app.is_editing_goal() {
        match key.code {
            KeyCode::Enter => {
                if let Some(text) = app.goal_edit_take() {
                    let trimmed = text.trim();
                    if !trimmed.is_empty() {
                        match crate::goal::write_goal_file(project_root, trimmed) {
                            Ok(()) => ws.notice(format!("goal updated: {}", trimmed)),
                            Err(e) => ws.notice(format!("couldn't save goal: {}", e)),
                        }
                    }
                }
            }
            KeyCode::Esc => {
                app.goal_edit_take();
            }
            KeyCode::Backspace => app.goal_edit_backspace(),
            KeyCode::Char(c) => app.goal_edit_push(c),
            _ => {}
        }
        return;
    }

    match key.code {
        KeyCode::Char('q') => {
            app.should_quit = true;
            return;
        }
        KeyCode::Char('?') => {
            app.show_help = true;
            return;
        }
        _ => {}
    }

    // T15 UX redesign, Step 9: navigation is a summon+pop stack (design doc
    // §5.2), not numbered tabs — `m`/`e` summon, `esc` pops one level,
    // `⏎` on a mastery row drills into that concept's detail. Only
    // `Focus::Home` falls through to the card/offer key bindings below
    // (mirrors the pre-redesign Dashboard/Card-tab-only gate: an open
    // overlay owns its keys exclusively).
    match app.focus().clone() {
        Focus::Mastery => {
            match key.code {
                KeyCode::Down | KeyCode::Char('j') => app.mastery_selected += 1,
                KeyCode::Up | KeyCode::Char('k') => {
                    app.mastery_selected = app.mastery_selected.saturating_sub(1)
                }
                KeyCode::Enter => {
                    if let Some(conn) = conn {
                        let rows = view::mastery_rows(ws, conn, taxonomy);
                        if let Some(row) = rows.get(app.mastery_selected.min(rows.len().saturating_sub(1))) {
                            app.push_focus(Focus::ConceptDetail(row.concept_id.clone()));
                        }
                    }
                }
                KeyCode::Char('m') | KeyCode::Esc => app.go_home(),
                _ => {}
            }
            return;
        }
        Focus::ConceptDetail(_) => {
            match key.code {
                KeyCode::Char('m') => app.go_home(),
                KeyCode::Esc => app.pop_focus(),
                _ => {}
            }
            return;
        }
        Focus::Events => {
            match key.code {
                KeyCode::Down | KeyCode::Char('j') => app.events_selected += 1,
                KeyCode::Up | KeyCode::Char('k') => {
                    app.events_selected = app.events_selected.saturating_sub(1)
                }
                KeyCode::Char('f') => {
                    app.events_filter = app.events_filter.next();
                    app.events_selected = 0;
                }
                KeyCode::Char('E') | KeyCode::Esc => app.go_home(),
                _ => {}
            }
            return;
        }
        Focus::Settings => {
            match key.code {
                KeyCode::Down | KeyCode::Char('j') => {
                    app.settings_selected = (app.settings_selected + 1) % app::SETTINGS_ROW_COUNT;
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    app.settings_selected = (app.settings_selected + app::SETTINGS_ROW_COUNT - 1)
                        % app::SETTINGS_ROW_COUNT;
                }
                KeyCode::Right | KeyCode::Enter => {
                    apply_settings_cycle(ws, app.settings_selected, true);
                }
                KeyCode::Left => {
                    apply_settings_cycle(ws, app.settings_selected, false);
                }
                KeyCode::Char('s') | KeyCode::Esc => app.go_home(),
                _ => {}
            }
            return;
        }
        Focus::Home => {}
    }

    // Goal editor — a DEDICATED, always-available key on the home surface, so
    // it means the same thing whether or not a card is on screen (founder:
    // no mix-and-match). `G` (uppercase) never collides with the lowercase
    // card actions, so `g` stays purely "got it".
    if key.code == KeyCode::Char('G') {
        app.start_goal_edit(crate::goal_text_now(project_root));
        return;
    }

    // Events overlay demotion (founder request, T15): events is now a
    // debug/history view, not a primary feature — summoned by `E`
    // (uppercase), parallel to `G` above, so it works whether or not a card
    // is on screen. This frees lowercase `e` to mean escalate-only (a card
    // action, see `response::classify_card_key` below) with no idle
    // meaning at all.
    if key.code == KeyCode::Char('E') {
        app.push_focus(Focus::Events);
        return;
    }

    // `m` only summons the mastery overlay from the empty/working/waiting
    // faces — when a card or offer is on screen, `m` is unbound (matching
    // the pre-redesign card-key classifier, which already treats `m` as
    // `Ignore`), exactly mirroring the idle keybar (design doc §5.3).
    let home_surface_is_idle =
        !app.ack_active() && ws.pending_card.lock_poison_safe().is_none() && ws.pending_offer.lock_poison_safe().is_none();
    if home_surface_is_idle && key.code == KeyCode::Char('m') {
        app.push_focus(Focus::Mastery);
        return;
    }

    // Settings overlay (founder ask, MUR-7): `s` — the live frequency/
    // directness view+editor. Free (not a card action) and gated on the
    // same idle check `m` uses: while a card or offer is on screen, `s`
    // stays unbound rather than colliding with a card response.
    if home_surface_is_idle && key.code == KeyCode::Char('s') {
        app.push_focus(Focus::Settings);
        return;
    }

    let Some(conn) = conn else { return };
    let KeyCode::Char(c) = key.code else { return };
    let key_str = c.to_string();

    // req 11-13 (mirrors the old stdin loop): a pending struggle offer
    // takes priority over y/n only — every other key falls through to the
    // normal card binding below and leaves the offer live.
    let maybe_offer: Option<PendingOffer> = ws.pending_offer.lock_poison_safe().clone();
    if let Some(po) = maybe_offer {
        let action = offer::classify_offer_key(&key_str);
        if action != offer::OfferKeyAction::Ignore {
            *ws.pending_offer.lock_poison_safe() = None;
            match action {
                // Decline is a fast local DB write — run inline on the loop.
                offer::OfferKeyAction::Decline => {
                    let sid = ws.session_mgr.lock_poison_safe().session_id.clone();
                    let notices = keys::handle_offer_key(
                        conn, ws, &sid, &po, action, project_root, taxonomy, canon, grammar,
                        prompts, models,
                    );
                    for n in notices {
                        ws.notice(n);
                    }
                }
                // Accept runs the struggle judge — a BLOCKING network dispatch.
                // Run it on a background thread so the event loop keeps
                // redrawing (showing the "working…" state) and stays
                // responsive, instead of freezing the whole UI on the call
                // (dogfood 2026-07-05). Single-flight: the busy flag prevents a
                // second accept from stacking another dispatch.
                offer::OfferKeyAction::Accept => {
                    if ws.busy.lock_poison_safe().is_some() {
                        ws.notice("still working on the previous request \u{2014} one moment");
                    } else {
                        *ws.busy.lock_poison_safe() =
                            Some("asking the model \u{2026}".to_string());
                        let ws2 = Arc::clone(ws);
                        let po2 = po.clone();
                        let pr2 = project_root.to_path_buf();
                        let tax2 = taxonomy.to_vec();
                        let canon2 = canon.to_vec();
                        let gram2 = grammar.clone();
                        let prompts2 = prompts.clone();
                        let models2 = models.clone();
                        std::thread::spawn(move || {
                            // Releases `busy` on return OR panic.
                            let _busy = BusyGuard(Arc::clone(&ws2));
                            match crate::db::get_db_path()
                                .and_then(|p| crate::db::open_connection(&p).ok())
                            {
                                Some(conn2) => {
                                    let sid2 =
                                        ws2.session_mgr.lock_poison_safe().session_id.clone();
                                    let notices = keys::handle_offer_key(
                                        &conn2,
                                        &ws2,
                                        &sid2,
                                        &po2,
                                        offer::OfferKeyAction::Accept,
                                        &pr2,
                                        &tax2,
                                        &canon2,
                                        &gram2,
                                        &prompts2,
                                        &models2,
                                    );
                                    for n in notices {
                                        ws2.notice(n);
                                    }
                                }
                                None => {
                                    ws2.notice("couldn't open the database for that request")
                                }
                            }
                        });
                    }
                }
                offer::OfferKeyAction::Ignore => {}
            }
            return;
        }
    }

    let action = response::classify_card_key(&key_str);

    // Step 5: the response-acknowledgment beat. `handle_card_key` already
    // frees `ws.pending_card` the instant `a`/`g` resolves (mirrors the
    // pre-redesign stdin loop's `.take()`), so the surface would otherwise
    // jump straight past the card with no confirmation it landed — snapshot
    // it first, purely for the TUI's own flash, before calling the SAME
    // unmodified `handle_card_key`.
    let ack_snapshot = match action {
        response::CardKeyAction::Response(response::ResponseVerb::Applied)
        | response::CardKeyAction::Response(response::ResponseVerb::GotIt) => {
            ws.pending_card.lock_poison_safe().clone()
        }
        _ => None,
    };

    let notices = keys::handle_card_key(conn, ws, action, taxonomy);
    for n in notices {
        ws.notice(n);
    }

    if let Some(card) = ack_snapshot {
        app.start_ack(card);
    }
}
