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
use crate::watch::{self, keys, WatchSession};
use crate::{budget, pack, response};

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
///
/// T17 R3: `canon`/`grammar`/`prompts` are gone from this signature — they
/// were only ever forwarded to `handle_key`'s now-deleted offer-accept arm
/// (`keys::handle_offer_key`); the TUI's own draw path never needed them
/// (`view::DrawContext` never carried them either).
#[allow(clippy::too_many_arguments)]
pub fn run(
    ws: Arc<WatchSession>,
    project_root: PathBuf,
    taxonomy: Vec<pack::TaxonomyConcept>,
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
                    // Redesign R2: the rail's `Tab` width-gate needs the
                    // CURRENT terminal width; `terminal.size()` mirrors
                    // exactly what `draw`'s `f.area()` just used this tick
                    // (autoresize already ran inside `terminal.draw` above).
                    let term_width = terminal.size().map(|s| s.width).unwrap_or(0);
                    handle_key(
                        &mut app,
                        &ws,
                        conn.as_ref(),
                        &project_root,
                        &taxonomy,
                        &models,
                        term_width,
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

/// T17 R0: the settings overlay's `min_gap` row cycle — `off`, then 5/8/15/30
/// minutes. `off` sits first so the kill switch is always one `\u{2190}` away
/// from the quietest "on" setting. Degrades to the 8-minute default (mirrors
/// `noise`'s old fail-toward-quiet posture) if `current` is ever a value this
/// module itself didn't produce.
const MIN_GAP_CYCLE: [Option<Duration>; 5] = [
    None,
    Some(Duration::from_secs(5 * 60)),
    Some(Duration::from_secs(8 * 60)),
    Some(Duration::from_secs(15 * 60)),
    Some(Duration::from_secs(30 * 60)),
];

fn min_gap_index(current: Option<Duration>) -> usize {
    MIN_GAP_CYCLE
        .iter()
        .position(|g| *g == current)
        .unwrap_or(2) // default to 8m if unrecognized
}

/// T17 R0 settings overlay: `\u{2192}` — off -> 5m -> 8m -> 15m -> 30m -> off.
fn next_min_gap(current: Option<Duration>) -> Option<Duration> {
    let idx = min_gap_index(current);
    MIN_GAP_CYCLE[(idx + 1) % MIN_GAP_CYCLE.len()]
}

/// T17 R0 settings overlay: `\u{2190}` — the same ring, reversed.
fn prev_min_gap(current: Option<Duration>) -> Option<Duration> {
    let idx = min_gap_index(current);
    MIN_GAP_CYCLE[(idx + MIN_GAP_CYCLE.len() - 1) % MIN_GAP_CYCLE.len()]
}

/// The settings overlay's `\u{2190}`/`\u{2192}` (and Enter, forward): `row` 0
/// is `min_gap`, `row` 1 is directness (mirrors `App::settings_selected`'s
/// indexing and `view::settings_rows`' order). Both writes update the LIVE,
/// session-scoped dial AND (redesign R0 / G5) persist to `config.toml`
/// immediately afterward via [`persist_dial_settings`], so a value changed
/// here survives past the current session. `min_gap` additionally updates
/// the LIVE `TokenBucket` in the same call, so the "next hint" ETA/mute
/// state reflects the change immediately; the label alone (`ws.min_gap`)
/// would otherwise silently drift from the bucket's actual behavior.
/// Cycling INTO `off` swaps in a true capacity-0 `TokenBucket::off` (the
/// kill switch — never ready, regardless of elapsed time); cycling OUT of
/// `off` rebuilds a fresh capacity-1 bucket at the new period. Between two
/// "on" values, `set_refill_period` is used instead, which preserves
/// whatever tokens have already accrued (T15's live-tune behavior, unchanged
/// by T17).
fn apply_settings_cycle(ws: &WatchSession, row: usize, forward: bool) {
    match row {
        0 => {
            let current = *ws.min_gap.lock_poison_safe();
            let next = if forward {
                next_min_gap(current)
            } else {
                prev_min_gap(current)
            };
            *ws.min_gap.lock_poison_safe() = next;
            let now = std::time::SystemTime::now();
            let mut bucket = ws.bucket.lock_poison_safe();
            match next {
                Some(gap) => {
                    if bucket.capacity() == 0 {
                        *bucket = budget::TokenBucket::for_min_gap(gap, now);
                    } else {
                        bucket.set_refill_period(gap);
                    }
                }
                None => *bucket = budget::TokenBucket::off(now),
            }
        }
        1 => {
            let current = *ws.directness.lock_poison_safe();
            let next = if forward { current.next() } else { current.prev() };
            *ws.directness.lock_poison_safe() = next;
        }
        _ => return,
    }
    persist_dial_settings(ws);
}

/// Redesign R0 (G5) / T17 R0: writes the CURRENT live dial values (post-cycle)
/// into the resolved user `config.toml`, via `config::write_dial_config`'s
/// targeted `[dial]`-only edit (every other section/comment survives — see
/// that function's doc). Never panics on failure (missing HOME, permission
/// error, ...) — a write failure is surfaced through `ws.notice` (the same
/// activity-log channel every other background/worker notice uses) so the
/// TUI keeps running with the change live for the rest of the session, just
/// not saved.
fn persist_dial_settings(ws: &WatchSession) {
    let Some(path) = crate::config::resolve_user_config_path() else {
        ws.notice("couldn't save settings: no user config path available".to_string());
        return;
    };
    let directness = ws.directness.lock_poison_safe().as_str().to_string();
    let min_gap = *ws.min_gap.lock_poison_safe();
    if let Err(e) = crate::config::write_dial_config(&path, &directness, min_gap) {
        ws.notice(format!("couldn't save settings to config.toml: {}", e));
    }
}

/// Maps one crossterm key event to an action. Overlay summon/pop and list
/// navigation (Mastery/Events/ConceptDetail) are handled entirely in `App`'s
/// `Focus` stack (design doc §5.2); a focused card on the `Home` surface is
/// handled by reusing the SAME `keys::handle_card_key` function the retired
/// stdin loop's inline arms called — this is the T15 requirement that
/// a/g/y/u/n/e/t behave IDENTICALLY to the pre-T15 loop, unchanged by the UX
/// redesign (T17 R3: the offer's own `y`/`n` consent branch is gone — `y` is
/// simply `response::ResponseVerb::Useful` now, routed through the same
/// `handle_card_key` path as every other card key).
/// Clears `WatchSession::busy` on drop — so a background dispatch releases the
/// single-flight/"working" flag even if it panics (a stuck `busy` would
/// otherwise wedge the UI in "working…" and block all further accepts).
struct BusyGuard(Arc<WatchSession>);
impl Drop for BusyGuard {
    fn drop(&mut self) {
        *self.0.busy.lock_poison_safe() = None;
    }
}

// =====================================================================
// Redesign R3: the `:` command palette — "go anywhere by name" so bare
// letter-keys stop proliferating as surfaces are added. `parse_command` is
// pure (no `App`/`WatchSession` access, just the typed text + the taxonomy
// needed to resolve `concept <query>`) so it's fully unit-testable on its
// own; `dispatch_command` below is the only place that turns a resolved
// `Command` into an actual action, and it does so by calling the SAME
// functions the corresponding key already calls — never a reimplementation
// of a view.
// =====================================================================

/// One command the palette can resolve to. `Unknown` covers both an
/// ambiguous prefix (matches more than one command name) and a name that
/// matches none; `Noop` is the empty input (nothing typed, `Enter` pressed
/// anyway) — both are handled gracefully by the caller, never a panic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Command {
    Settings,
    Goal,
    Mastery,
    Events,
    ConceptDetail(String),
    Home,
    Help,
    Quit,
    Unknown,
    Noop,
}

/// The full command-name set, listed most-specific (longest) name first per
/// this repo's scanning-order convention — though the match below is
/// order-independent by construction: a name is only matched when the typed
/// prefix is unambiguous (matches exactly one entry), so no entry can ever
/// be shadowed by a shorter one checked earlier.
const COMMAND_NAMES: [&str; 9] = [
    "watching", "settings", "concept", "mastery", "events", "quit", "help", "goal", "home",
];

/// Pure: resolves the palette's typed text (everything after the `:`) to a
/// `Command` — prefix match, case-insensitive (`:mas` \u{2192} `mastery`),
/// `concept <query>` resolved against `taxonomy` by slug or human-name
/// substring. Never panics: empty input is `Noop`, anything that doesn't
/// resolve to exactly one command name is `Unknown`. T17 R4: `history` is
/// retired (Home already IS the history stream — see `Focus`'s doc).
pub(crate) fn parse_command(input: &str, taxonomy: &[pack::TaxonomyConcept]) -> Command {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Command::Noop;
    }
    let mut parts = trimmed.splitn(2, char::is_whitespace);
    let cmd = parts.next().unwrap_or("").to_lowercase();
    let arg = parts.next().unwrap_or("").trim();
    if cmd.is_empty() {
        return Command::Noop;
    }

    let matches: Vec<&str> =
        COMMAND_NAMES.iter().copied().filter(|name| name.starts_with(cmd.as_str())).collect();
    let resolved = match matches.as_slice() {
        [only] => *only,
        _ => return Command::Unknown,
    };

    match resolved {
        "concept" => match resolve_concept(arg, taxonomy) {
            Some(slug) => Command::ConceptDetail(slug),
            None => Command::Unknown,
        },
        "settings" => Command::Settings,
        "goal" => Command::Goal,
        "mastery" => Command::Mastery,
        "events" => Command::Events,
        "home" | "watching" => Command::Home,
        "help" => Command::Help,
        "quit" => Command::Quit,
        _ => Command::Unknown,
    }
}

/// Pure: resolves a `concept` command's argument against the taxonomy — an
/// exact (case-insensitive) slug match first, then a case-insensitive
/// substring match against the human-readable name. Empty query never
/// matches (there's nothing to search for).
fn resolve_concept(query: &str, taxonomy: &[pack::TaxonomyConcept]) -> Option<String> {
    if query.is_empty() {
        return None;
    }
    let q = query.to_lowercase();
    if let Some(c) = taxonomy.iter().find(|c| c.slug.to_lowercase() == q) {
        return Some(c.slug.clone());
    }
    taxonomy.iter().find(|c| c.name.to_lowercase().contains(&q)).map(|c| c.slug.clone())
}

/// Pure: a short label describing what a resolved `Command` will do —
/// rendered next to the palette's typed text as a live hint. Empty for
/// `Unknown`/`Noop` (nothing to hint at).
pub(crate) fn command_hint(cmd: &Command, taxonomy: &[pack::TaxonomyConcept]) -> String {
    match cmd {
        Command::Settings => "settings".to_string(),
        Command::Goal => "goal".to_string(),
        Command::Mastery => "mastery".to_string(),
        Command::Events => "events".to_string(),
        Command::ConceptDetail(slug) => {
            let name = taxonomy.iter().find(|c| &c.slug == slug).map(|c| c.name.as_str());
            match name {
                Some(n) => format!("concept: {}", n),
                None => format!("concept: {}", slug),
            }
        }
        Command::Home => "home".to_string(),
        Command::Help => "help".to_string(),
        Command::Quit => "quit".to_string(),
        Command::Unknown | Command::Noop => String::new(),
    }
}

/// Whether the Home surface is idle — no card, and the response-ack beat
/// isn't still showing. Shared by `handle_key`'s own `m` gate and
/// `dispatch_command`'s `mastery` command, so the palette's `mastery`
/// command behaves IDENTICALLY to pressing `m` directly (same gate, not a
/// reimplementation). T17 R3: the offer's own slot is gone — a hint now
/// occupies `pending_card` directly, so that single check is sufficient.
fn home_surface_is_idle(app: &App, ws: &WatchSession) -> bool {
    !app.ack_active() && ws.pending_card.lock_poison_safe().is_none()
}

/// Turns a resolved `Command` into an action — reusing the SAME functions
/// the corresponding key already calls (`push_focus`, the goal/settings
/// openers, `go_home`, quit) rather than reimplementing any view. `Unknown`
/// surfaces a brief `ws.notice` and otherwise does nothing; `Noop` (empty
/// input) is silently ignored.
fn dispatch_command(cmd: Command, app: &mut App, ws: &WatchSession, project_root: &Path) {
    match cmd {
        Command::Settings => app.open_settings(),
        Command::Goal => app.start_goal_edit(crate::goal_text_now(project_root)),
        Command::Mastery => {
            // Mirrors `m`'s own gate exactly: mastery is unreachable while a
            // card/offer is up, whether summoned by key or by name.
            if home_surface_is_idle(app, ws) {
                app.push_focus(Focus::Mastery);
            }
        }
        Command::Events => app.push_focus(Focus::Events),
        Command::ConceptDetail(slug) => app.push_focus(Focus::ConceptDetail(slug)),
        Command::Home => app.go_home(),
        Command::Help => app.show_help = true,
        Command::Quit => app.should_quit = true,
        Command::Unknown => ws.notice("no such command"),
        Command::Noop => {}
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_key(
    app: &mut App,
    ws: &Arc<WatchSession>,
    conn: Option<&rusqlite::Connection>,
    project_root: &Path,
    taxonomy: &[pack::TaxonomyConcept],
    models: &crate::Models,
    term_width: u16,
    key: KeyEvent,
) {
    // Redesign R2: Ctrl-C must quit even with help open — the help text
    // itself advertises "q or Ctrl-C quit", so the guard below must never
    // absorb it. Checked BEFORE the help guard (was after it, which let
    // help's "any other key is absorbed" swallow Ctrl-C too).
    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        app.should_quit = true;
        return;
    }

    // The help overlay dismisses on `?`/`esc` only (redesign R1: it used to
    // swallow ANY key, so e.g. pressing `m` while help was open closed help
    // instead of opening mastery — every other surface only reacts to its
    // own bound keys, and help should be no different). Any other key while
    // help is open is simply absorbed (still never reaches the bindings
    // below), so help stays up until deliberately dismissed.
    if app.show_help {
        if matches!(key.code, KeyCode::Char('?') | KeyCode::Esc) {
            app.show_help = false;
        }
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

    // Redesign R3: the `:` command palette — while open, ALL keys are text
    // (same capture posture as the goal editor above, checked BEFORE the
    // global `q`/`?` below so typing either into a command name/argument
    // never quits or opens help by accident). `Enter` resolves the typed
    // text via `parse_command` and dispatches it; `Esc` cancels.
    if app.is_editing_command() {
        match key.code {
            KeyCode::Enter => {
                if let Some(text) = app.command_take() {
                    let cmd = parse_command(&text, taxonomy);
                    dispatch_command(cmd, app, ws, project_root);
                }
            }
            KeyCode::Esc => {
                app.command_take();
            }
            KeyCode::Backspace => app.command_backspace(),
            KeyCode::Char(c) => app.command_push(c),
            _ => {}
        }
        return;
    }

    // Redesign R5: ask mode — while open, ALL keys are text (same capture
    // posture as the goal editor/command palette above, checked BEFORE the
    // global `q`/`?` below so typing either into a question never quits or
    // opens help by accident). `Enter` sends the question (a BLOCKING model
    // call, so it runs on a background thread exactly like offer-accept
    // below — never the event loop); `Esc` always exits back to the card.
    // T17 R5 ("ask on any card"): the target may now be the live pending
    // card OR a selected historical row (`app.ask_target()`) — resolved
    // into one `keys::AskSubject` below, so everything past that point is
    // unchanged from the R5-predecessor path.
    if app.is_asking() {
        match key.code {
            KeyCode::Esc => app.exit_ask(),
            KeyCode::Backspace => app.ask_backspace(),
            KeyCode::Char(c) => app.ask_push(c),
            KeyCode::Enter => {
                // Gate R5 fixes: (defect 2) validate BEFORE draining the input
                // buffer, so a blocked send (no card / busy / thread cap) keeps
                // the typed question instead of silently losing it; and
                // (defect 1 / C12) enforce the per-card thread cap before
                // spawning a dispatch, so the prompt can't grow unbounded.
                let question = app.ask_buf().map(str::trim).unwrap_or("").to_string();
                if question.is_empty() {
                    return;
                }
                let Some(target_id) = app.ask_target() else {
                    ws.notice("no card to ask about anymore");
                    return;
                };
                if ws.busy.lock_poison_safe().is_some() {
                    ws.notice("still working on the previous request \u{2014} one moment");
                    return;
                }
                let Some(conn) = conn else {
                    ws.notice("couldn't open the database for that request");
                    return;
                };
                let turns = crate::db::thread_user_turn_count(conn, target_id).unwrap_or(0);
                if crate::thread::thread_cap_reached(turns) {
                    ws.notice(crate::thread::THREAD_CAP_NOTICE);
                    return;
                }
                // T17 R5: resolve WHO to ask — the live in-memory pending
                // card when it's still the SAME one being asked about (full
                // fidelity, incl. `site_enclosing_item` — unchanged from
                // before this rung), else the SELECTED historical card's
                // persisted body (`db::card_detail`). Neither
                // `apply_ask_send` nor `thread::build_ask_prompt` changed
                // shape for this — only this resolution step, feeding the
                // same card-id-scoped machinery T4 already established.
                let live_pc = ws.pending_card.lock_poison_safe().clone();
                let subject = match live_pc.filter(|pc| pc.card_id == target_id) {
                    Some(pc) => keys::AskSubject::from_pending(&pc),
                    None => {
                        let Some(detail) =
                            crate::db::card_detail(conn, target_id).ok().flatten()
                        else {
                            ws.notice("no card to ask about anymore");
                            return;
                        };
                        let session_id = ws.session_mgr.lock_poison_safe().session_id.clone();
                        match keys::AskSubject::from_card_detail(&detail, &session_id) {
                            Some(s) => s,
                            None => {
                                ws.notice(
                                    "no card text recorded for that card \u{2014} nothing to ask about",
                                );
                                return;
                            }
                        }
                    }
                };
                // Cleared to send — NOW drain the input buffer.
                app.ask_send();
                *ws.busy.lock_poison_safe() = Some("asking the model \u{2026}".to_string());
                let ws2 = Arc::clone(ws);
                let models2 = models.clone();
                std::thread::spawn(move || {
                    // Releases `busy` on return OR panic — same single-flight
                    // guard the offer-accept dispatch uses.
                    let _busy = BusyGuard(Arc::clone(&ws2));
                    match crate::db::get_db_path()
                        .and_then(|p| crate::db::open_connection(&p).ok())
                    {
                        Some(conn2) => {
                            if let Err(e) =
                                keys::apply_ask_send(&conn2, &subject, &question, &models2)
                            {
                                ws2.notice(keys::ask_failure_notice(&e));
                            }
                        }
                        None => {
                            ws2.notice("couldn't open the database for that request")
                        }
                    }
                });
            }
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

    // Settings popup (redesign R3): a TRANSIENT layered over the focus pane
    // (mirrors the goal editor), not part of the `Focus` stack — so it's
    // checked here, in the same slot the retired `Focus::Settings` arm used
    // to occupy, rather than inside the `match app.focus()` below. `q`/`?`
    // above still fire even while it's open (unchanged from before: the old
    // `Focus::Settings` arm sat after that same global check too).
    if app.is_settings_open() {
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
            KeyCode::Char('s') | KeyCode::Esc => {
                app.close_settings();
            }
            _ => {}
        }
        return;
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
                // Redesign R1 (Esc-as-spring): `esc` always pops exactly one
                // level toward home — here that's a no-op difference from
                // `go_home` (Mastery only ever sits one level up), but using
                // `pop_focus` uniformly is what makes the model consistent
                // with `ConceptDetail` below, whose `esc` must NOT jump all
                // the way home. `m` keeps its own dedicated "jump home from
                // anywhere" meaning.
                KeyCode::Char('m') => app.go_home(),
                KeyCode::Esc => app.pop_focus(),
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
            // T17 R6 ("events split-pane"): esc/pgup/pgdn are live ONLY
            // while the detail pane is open — mirrors the stream's
            // `Focus::Home` arm exactly (see `App::events_expanded_id`'s
            // doc).
            match key.code {
                KeyCode::Esc if app.events_expanded_id().is_some() => {
                    app.close_events_detail();
                    return;
                }
                KeyCode::PageUp if app.events_expanded_id().is_some() => {
                    app.events_scroll_up();
                    return;
                }
                KeyCode::PageDown if app.events_expanded_id().is_some() => {
                    app.events_scroll_down();
                    return;
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    if let Some(conn) = conn {
                        let sid = ws.session_mgr.lock_poison_safe().session_id.clone();
                        let rows = view::events_rows(conn, &sid, app.events_filter);
                        app.events_select_down(rows.len());
                        let selected = app.events_selected_clamped(rows.len());
                        app.follow_events_detail(rows.get(selected).and_then(|e| e.id));
                    }
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    if let Some(conn) = conn {
                        let sid = ws.session_mgr.lock_poison_safe().session_id.clone();
                        let rows = view::events_rows(conn, &sid, app.events_filter);
                        app.events_select_up();
                        let selected = app.events_selected_clamped(rows.len());
                        app.follow_events_detail(rows.get(selected).and_then(|e| e.id));
                    }
                }
                KeyCode::Enter => {
                    if let Some(conn) = conn {
                        let sid = ws.session_mgr.lock_poison_safe().session_id.clone();
                        let rows = view::events_rows(conn, &sid, app.events_filter);
                        let selected = app.events_selected_clamped(rows.len());
                        if let Some(id) = rows.get(selected).and_then(|e| e.id) {
                            app.toggle_events_expand(id);
                        }
                    }
                }
                KeyCode::Char('f') => {
                    app.events_filter = app.events_filter.next();
                    app.events_selected = 0;
                    // A filter change reshuffles the row set — a pane open
                    // on a row that may no longer be in the filtered list
                    // must close rather than risk showing a stale/wrong
                    // event.
                    app.close_events_detail();
                }
                // See the Mastery arm's comment above: `esc` pops one level
                // (a no-op difference from `go_home` here, since Events only
                // ever sits one level up), `E` keeps the dedicated jump-home.
                KeyCode::Char('E') => app.go_home(),
                KeyCode::Esc => app.pop_focus(),
                _ => {}
            }
            return;
        }
        Focus::Home => {}
    }

    // Redesign R2: `Tab` toggles the rail split — live on Home REGARDLESS
    // of card/offer presence (unlike `m` below, gated on
    // `home_surface_is_idle`): the whole point of the rail is that it sits
    // BESIDE a live card, so the card's own keys must keep working. Gated
    // on terminal width via `App::toggle_rail`; below `app::RAIL_MIN_WIDTH`
    // it's a no-op plus a brief notice rather than a silent swallow.
    if key.code == KeyCode::Tab {
        if !app.toggle_rail(term_width) {
            ws.notice("terminal too narrow for the rail panel".to_string());
        }
        return;
    }

    // Redesign R2: rail navigation — live ONLY while the rail is open, and
    // deliberately NOT a focus-mode: none of arrows/Enter/`j`/`k` collide
    // with a card action (`a`/`g`/`u`/`n`/`e`/`t`), an offer key (`y`/`n`),
    // or a global (`G`/`E`/`m`/`s`/`?`/`q`) — all of those stay live
    // exactly as before, falling through below unchanged.
    if app.rail_open {
        match key.code {
            KeyCode::Down | KeyCode::Char('j') => {
                let len = view::rail_row_kinds_from_ws(ws, conn, taxonomy).len();
                app.rail_selected = (app.rail_selected + 1).min(len.saturating_sub(1));
                return;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                app.rail_selected = app.rail_selected.saturating_sub(1);
                return;
            }
            KeyCode::Enter => {
                let kinds = view::rail_row_kinds_from_ws(ws, conn, taxonomy);
                let selected = app.rail_selected_clamped(kinds.len());
                if let Some(target) = kinds.get(selected).and_then(|k| k.drill_target()) {
                    app.push_focus(target);
                }
                return;
            }
            _ => {}
        }
    } else {
        // T17 R4/R5: stream browsing + the split detail pane — live only
        // while the rail is CLOSED (the rail already owns arrows/Enter when
        // open, see above), and deliberately plain arrows only (not `j`/`k`
        // — lowercase `k` is already bound to "ask a question", see the
        // `CardKeyAction::Ask` arm below). Sanctioned drive-by (R4 gate
        // learning-note): a single self-consistent `match key.code` — every
        // handled key returns for itself, `_` falls through to the rest of
        // this function — replaces the old `matches!(Down|Up|Enter)` outer
        // guard coupled to a SEPARATE inner `match … => unreachable!()`; a
        // key added to one arm can no longer panic by being missed in the
        // other, because there's only one arm list now.
        match key.code {
            // T17 R5: esc/pgup/pgdn are live ONLY while the detail pane is
            // open — a plain, unqualified `esc`/`pgup`/`pgdn` on an idle
            // stream falls through (no meaning yet) rather than being
            // silently swallowed.
            KeyCode::Esc if app.stream_expanded_card().is_some() => {
                app.close_stream_detail();
                return;
            }
            KeyCode::PageUp if app.stream_expanded_card().is_some() => {
                app.detail_scroll_up();
                return;
            }
            KeyCode::PageDown if app.stream_expanded_card().is_some() => {
                app.detail_scroll_down();
                return;
            }
            KeyCode::Down => {
                if let Some(conn) = conn {
                    let rows = view::history_rows(conn);
                    let live_id = view::active_stream_card_id(app, ws);
                    let older = view::stream_older_rows(&rows, live_id);
                    app.stream_select_down(older.len());
                    // T17 R5: FOLLOW the open detail pane onto the new
                    // selection (a no-op while it's closed) — the decided
                    // answer to "does moving the selection close the pane".
                    let selected = app.stream_selected_clamped(older.len());
                    app.follow_stream_detail(older.get(selected).map(|r| r.id));
                }
                return;
            }
            KeyCode::Up => {
                if let Some(conn) = conn {
                    let rows = view::history_rows(conn);
                    let live_id = view::active_stream_card_id(app, ws);
                    let older = view::stream_older_rows(&rows, live_id);
                    app.stream_select_up();
                    let selected = app.stream_selected_clamped(older.len());
                    app.follow_stream_detail(older.get(selected).map(|r| r.id));
                }
                return;
            }
            KeyCode::Enter => {
                if let Some(conn) = conn {
                    let rows = view::history_rows(conn);
                    let live_id = view::active_stream_card_id(app, ws);
                    let older = view::stream_older_rows(&rows, live_id);
                    let selected = app.stream_selected_clamped(older.len());
                    if let Some(row) = older.get(selected) {
                        app.toggle_stream_expand(row.id);
                    }
                }
                return;
            }
            _ => {}
        }
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

    // T17 R4: the old HISTORY overlay's `h` binding is RETIRED — Home
    // already IS the history stream (see `Focus`'s doc), so lowercase `h`
    // is free again (`response::classify_card_key('h')` was already
    // `Ignore` — it was never a card action either).

    // Settings popup (founder ask, MUR-7; redesign R3, fixes G3): `s` — the
    // live frequency/directness view+editor. As of R3 it's a TRANSIENT popup
    // (see `App::open_settings`/`draw_settings_overlay`), so — UNLIKE `m`
    // below — it's deliberately unconditional: opening it over a live card
    // is the entire point (adjust the dial WITHOUT leaving the card), and it
    // never disturbs `pending_card`/rail state underneath.
    if key.code == KeyCode::Char('s') {
        app.open_settings();
        return;
    }

    // Redesign R3: `:` opens the command palette — "go anywhere by name",
    // same unconditional posture as `s`/`G`/`E` above (it's a transient
    // overlay too, not a card action).
    if key.code == KeyCode::Char(':') {
        app.start_command();
        return;
    }

    // `m` only summons the mastery overlay from the empty/working/waiting
    // faces — when a card or offer is on screen, `m` is unbound (matching
    // the pre-redesign card-key classifier, which already treats `m` as
    // `Ignore`), exactly mirroring the idle keybar (design doc §5.3).
    if home_surface_is_idle(app, ws) && key.code == KeyCode::Char('m') {
        app.push_focus(Focus::Mastery);
        return;
    }

    let Some(conn) = conn else { return };
    let KeyCode::Char(c) = key.code else { return };
    let key_str = c.to_string();

    // T17 R3: the struggle offer's `[y/N]` consent dialogue is gone — a
    // struggle/perception hint now occupies `ws.pending_card` directly
    // (`watch::offers::run_poll_loop` fires it inline at gate-pass), so
    // every key on Home falls straight through to the normal card binding
    // below; `y` is simply `response::ResponseVerb::Useful` now.
    let action = response::classify_card_key(&key_str);

    // Redesign R5: `k` (ask) is intercepted HERE, before it ever reaches
    // `keys::handle_card_key` — entering ask mode is a pure `App`-level
    // state change (no DB effect), so it doesn't belong in that DB-effect
    // function. T17 R5 ("ask on any card"): a SELECTED row's open detail
    // pane takes priority over the live pending card — that's the whole
    // point of "inline ask on the selected card" (the live card's OWN `k`
    // stays exactly as it was whenever no detail pane is open). Either way,
    // opens only when there's actually something to ask about (asking about
    // nothing is a no-op, not a notice) — the card/pane itself is left
    // completely untouched, still there underneath.
    if action == response::CardKeyAction::Ask {
        if let Some(detail_id) = app.stream_expanded_card() {
            app.start_ask(detail_id);
        } else if let Some(pc) = ws.pending_card.lock_poison_safe().clone() {
            app.start_ask(pc.card_id);
        }
        return;
    }

    // Step 5: the response-acknowledgment beat. `handle_card_key` already
    // frees `ws.pending_card` the instant `a`/`g`/`y` resolves (mirrors the
    // pre-redesign stdin loop's `.take()`), so the surface would otherwise
    // jump straight past the card with no confirmation it landed — snapshot
    // it first, purely for the TUI's own flash, before calling the SAME
    // unmodified `handle_card_key`. T17 R3: `y` (useful) gets the same ack
    // flash as `a`/`g` — a positive resolution, not a silent dismissal.
    let ack_snapshot = match action {
        response::CardKeyAction::Response(response::ResponseVerb::Applied)
        | response::CardKeyAction::Response(response::ResponseVerb::GotIt)
        | response::CardKeyAction::Response(response::ResponseVerb::Useful) => {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_taxonomy() -> Vec<pack::TaxonomyConcept> {
        vec![
            pack::TaxonomyConcept {
                slug: "borrow-vs-clone".to_string(),
                name: "Borrow vs. clone".to_string(),
                category: pack::Category::Idiom,
            },
            pack::TaxonomyConcept {
                slug: "error-handling".to_string(),
                name: "Error handling".to_string(),
                category: pack::Category::BestPractice,
            },
        ]
    }

    // --- redesign R3: `parse_command` (pure) ---

    #[test]
    fn test_parse_command_empty_is_noop() {
        assert_eq!(parse_command("", &sample_taxonomy()), Command::Noop);
        assert_eq!(parse_command("   ", &sample_taxonomy()), Command::Noop);
    }

    // T17 R4: the `history` command name is RETIRED alongside the `h`
    // key/overlay it used to summon — Home already IS the history stream
    // now, so there's nowhere left to go by that name.
    #[test]
    fn test_parse_command_history_is_retired() {
        assert_eq!(parse_command("hist", &sample_taxonomy()), Command::Unknown);
        assert_eq!(parse_command("history", &sample_taxonomy()), Command::Unknown);
    }

    #[test]
    fn test_parse_command_settings_short_and_long_form() {
        assert_eq!(parse_command("s", &sample_taxonomy()), Command::Settings);
        assert_eq!(parse_command("settings", &sample_taxonomy()), Command::Settings);
    }

    #[test]
    fn test_parse_command_is_case_insensitive() {
        assert_eq!(parse_command("MASTERY", &sample_taxonomy()), Command::Mastery);
        assert_eq!(parse_command("Settings", &sample_taxonomy()), Command::Settings);
        assert_eq!(parse_command("QUIT", &sample_taxonomy()), Command::Quit);
    }

    #[test]
    fn test_parse_command_concept_resolves_by_slug_or_name_substring() {
        assert_eq!(
            parse_command("concept borrow-vs-clone", &sample_taxonomy()),
            Command::ConceptDetail("borrow-vs-clone".to_string())
        );
        assert_eq!(
            parse_command("concept borrow", &sample_taxonomy()),
            Command::ConceptDetail("borrow-vs-clone".to_string()),
            "a human-name substring must resolve too"
        );
        assert_eq!(
            parse_command("concept CLONE", &sample_taxonomy()),
            Command::ConceptDetail("borrow-vs-clone".to_string()),
            "resolution is case-insensitive"
        );
    }

    #[test]
    fn test_parse_command_concept_with_no_match_or_no_query_is_unknown() {
        assert_eq!(parse_command("concept nonexistent", &sample_taxonomy()), Command::Unknown);
        assert_eq!(parse_command("concept", &sample_taxonomy()), Command::Unknown);
        assert_eq!(parse_command("concept   ", &sample_taxonomy()), Command::Unknown);
    }

    #[test]
    fn test_parse_command_home_and_watching_are_aliases() {
        assert_eq!(parse_command("home", &sample_taxonomy()), Command::Home);
        assert_eq!(parse_command("watching", &sample_taxonomy()), Command::Home);
    }

    #[test]
    fn test_parse_command_all_the_remaining_names() {
        let taxonomy = sample_taxonomy();
        assert_eq!(parse_command("goal", &taxonomy), Command::Goal);
        assert_eq!(parse_command("mastery", &taxonomy), Command::Mastery);
        assert_eq!(parse_command("events", &taxonomy), Command::Events);
        assert_eq!(parse_command("help", &taxonomy), Command::Help);
        assert_eq!(parse_command("quit", &taxonomy), Command::Quit);
    }

    #[test]
    fn test_parse_command_unknown_name_never_panics() {
        assert_eq!(parse_command("bogus", &sample_taxonomy()), Command::Unknown);
        assert_eq!(parse_command("xyz123", &sample_taxonomy()), Command::Unknown);
    }

    #[test]
    fn test_parse_command_ambiguous_prefix_is_unknown() {
        // "h" alone is a prefix of both "home" AND "help" — refusing to
        // guess is safer than silently picking one.
        assert_eq!(parse_command("h", &sample_taxonomy()), Command::Unknown);
    }

}
