//! T15 UX redesign: the TUI's own UI state — which surface/overlay is
//! focused (a summon+pop stack, design doc §5.2, NOT numbered tabs), list
//! selections, the animated header-pulse tick, the response-acknowledgment
//! beat, the help overlay, and the quit flag. Deliberately tiny: everything
//! else the views draw (the active card, the queue, mastery, events, ...) is
//! read fresh from `WatchSession`/`profile.db` on every tick rather than
//! cached here.

use crate::watch::PendingCard;

/// The redesign's navigation model (design doc §5.2): home is the app, not a
/// tab; mastery/events/concept-detail are overlays summoned with a key and
/// popped with `esc`. [`App::focus_stack`] is never empty — its first entry
/// is always `Focus::Home`, so `esc` at the bottom of the stack is simply a
/// no-op rather than needing a special case at every call site.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Focus {
    /// T17 R4 ("history-stream is home"): Home is no longer just the live
    /// card — it's the scrollable stream of this profile's persisted cards
    /// (`db::recent_cards`), with the live/newest entry pinned + accented at
    /// the top. The old `History`/`HistoryDetail` overlay (summoned with
    /// `h`, founder decision 2026-07-06) is RETIRED — a separate "scroll
    /// back through past cards" surface is redundant now that Home already
    /// IS that surface; browsing/expanding older rows lives directly on
    /// Home (`App::stream_selected`/`stream_expanded_card`) instead of a
    /// pushed `Focus`. See `view::draw_stream`.
    Home,
    Mastery,
    /// Holds the drilled-into concept's taxonomy slug (design doc §3.6's
    /// concept-detail "level"/drill-down, reached via `⏎` on a mastery row).
    ConceptDetail(String),
    Events,
}

/// The settings popup's rows, in the fixed order it lists/cycles them —
/// `App::settings_selected` indexes into this ring. Redesign R3: settings is
/// a TRANSIENT popup (`App::settings_open`), not a `Focus` — it layers over
/// whatever's on the focus pane (mirrors the goal editor) rather than
/// replacing it, so it can open even while a card is up.
pub const SETTINGS_ROW_COUNT: usize = 2;

/// T15 req (V3): the events view's kind filter — cycled with `f`. `All`
/// shows everything; the named filters isolate exactly the "silence is
/// ambiguous" distinction the spec calls out (`judge_declined` vs
/// `judge_drop` vs `card_shown`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventsFilter {
    All,
    JudgeDeclined,
    JudgeDrop,
    CardShown,
}

impl EventsFilter {
    const CYCLE: [EventsFilter; 4] = [
        EventsFilter::All,
        EventsFilter::JudgeDeclined,
        EventsFilter::JudgeDrop,
        EventsFilter::CardShown,
    ];

    pub fn next(&self) -> EventsFilter {
        let idx = Self::CYCLE.iter().position(|f| f == self).unwrap_or(0);
        Self::CYCLE[(idx + 1) % Self::CYCLE.len()]
    }

    pub fn label(&self) -> &'static str {
        match self {
            EventsFilter::All => "all",
            EventsFilter::JudgeDeclined => "judge_declined",
            EventsFilter::JudgeDrop => "judge_drop",
            EventsFilter::CardShown => "card_shown",
        }
    }

    /// Whether an event's `kind` passes this filter.
    pub fn matches(&self, kind: &str) -> bool {
        match self {
            EventsFilter::All => true,
            EventsFilter::JudgeDeclined => kind == "judge_declined",
            EventsFilter::JudgeDrop => kind == "judge_drop",
            EventsFilter::CardShown => kind == "card_shown",
        }
    }
}

/// Redesign R2: the rail's minimum terminal width. The rail (`Length(30)`,
/// see `view::RAIL_WIDTH`) sits beside the focus pane, and the focus pane
/// must keep enough room for the card's own centered reading column
/// (`view::READING_COLUMN_WIDTH`, 64) — so the gate is exactly
/// `READING_COLUMN_WIDTH + RAIL_WIDTH`. Below this, `Tab` is a no-op and the
/// rail never renders even if `rail_open` was left `true` from a wider
/// session (a live resize down is re-checked at every draw, not just at the
/// moment `Tab` was pressed).
pub const RAIL_MIN_WIDTH: u16 = 94;

/// Pure: whether the rail is allowed to open (or stay open) at this
/// terminal width.
pub fn rail_can_open(width: u16) -> bool {
    width >= RAIL_MIN_WIDTH
}

/// Redesign R2: the header's persistent, always-visible "what's murshid's
/// stance right now" token — replaces the old header pulse's Home-only
/// reach (it vanished the instant any overlay/detail view opened, the
/// "disappearing mentor-state" gap). Redesign R5 wires in the `Ask` variant
/// reserved back then — `Overlay`'s `&'static str` payload stayed generic
/// enough that this needed no shape change, only this new variant + call
/// site.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModeToken {
    /// Home, nothing on screen — murshid is only watching the files.
    Watching,
    /// Home, and a card/struggle-offer is up (or the response-ack beat is
    /// still showing) — murshid has something live for the user right now.
    Hint,
    /// A full-screen detail reader is open (`ConceptDetail`) — the user is
    /// reading, not being actively prompted.
    Reading,
    /// Redesign R5: ask mode is open on a card — a conversational follow-up
    /// is in progress (typing, or waiting on the model). Takes priority over
    /// `Hint` (the card is still up underneath, but the stance right now is
    /// "asking", not just "here's a hint").
    Ask,
    /// One of the summoned overlays (`mastery`/`events`/`history`) — the
    /// overlay's own name IS the stance while it's open. Settings (R3) is a
    /// TRANSIENT popup layered over the token below, not one of these.
    Overlay(&'static str),
}

impl ModeToken {
    pub fn label(&self) -> &'static str {
        match self {
            ModeToken::Watching => "watching",
            ModeToken::Hint => "hint",
            ModeToken::Reading => "reading",
            ModeToken::Ask => "ask",
            ModeToken::Overlay(name) => name,
        }
    }
}

/// Pure: resolves the header's mode token from `focus` (+ whether the Home
/// surface currently has a card/offer/ack up, + whether ask mode is open).
/// `asking` wins over everything else on `Focus::Home` (checked first) —
/// ask mode only ever opens on top of a live card, so `home_has_card` is
/// already true whenever `asking` is, but the stance to show is "asking",
/// not just "hint".
pub fn mode_token(focus: &Focus, home_has_card: bool, asking: bool) -> ModeToken {
    if asking {
        return ModeToken::Ask;
    }
    match focus {
        Focus::Home => {
            if home_has_card {
                ModeToken::Hint
            } else {
                ModeToken::Watching
            }
        }
        Focus::Mastery => ModeToken::Overlay("mastery"),
        Focus::Events => ModeToken::Overlay("events"),
        Focus::ConceptDetail(_) => ModeToken::Reading,
    }
}

/// Design doc §5.4: how many ticks the response-acknowledgment beat (the
/// card border's green flash on `a`/`g`) stays visible before the surface
/// moves on to whatever's next (another queued card, or the caught-up empty
/// state). At the ~200ms poll interval the event loop already runs, 3 ticks
/// is roughly half a second — long enough to read as "that landed", short
/// enough to never feel like a delay.
pub const ACK_BEAT_TICKS: u64 = 3;

/// The TUI's own state — separate from `WatchSession` (the engine's shared,
/// multi-threaded state) on purpose: nothing here is touched by the worker
/// threads, so it needs no locking.
pub struct App {
    focus_stack: Vec<Focus>,
    pub show_help: bool,
    pub should_quit: bool,
    pub mastery_selected: usize,
    pub events_selected: usize,
    pub events_filter: EventsFilter,
    /// The settings overlay's currently-selected row (`0`=min_gap,
    /// `1`=directness) — indexes [`SETTINGS_ROW_COUNT`].
    pub settings_selected: usize,
    /// T17 R4 ("history-stream is home"): the stream's OLDER-entries
    /// selection — the live/newest entry (a real pending card, or the
    /// just-acked snapshot) is never part of this index; `0` is the
    /// most-recent OLDER row. Clamped to the freshly-fetched row count at
    /// READ time (see [`App::stream_selected_clamped`]), same posture as
    /// `mastery_selected`/`events_selected`/the old `history_selected`.
    pub stream_selected: usize,
    /// `Some(card_id)` while that OLDER stream row's full persisted body is
    /// expanded inline (`⏎` toggles) — `None` = every row collapsed. Reset
    /// whenever the selection itself moves (see `stream_select_up`/`_down`),
    /// so a stale expansion never lingers on the wrong row once scrolled
    /// away from.
    stream_expanded_card: Option<i64>,
    /// Step 2: incremented once per event-loop poll iteration (~200ms) —
    /// the header pulse's and "thinking" face's animation frame index
    /// (design doc §3.4: "index the frame by a tick counter"). Wraps via
    /// the modulo in `theme::working_pulse_frame`, so overflow is harmless.
    /// Redesign R2: whether the `Tab`-toggled rail (mastery-at-a-glance +
    /// recent activity + "N waiting"), a SPLIT of the Home surface rather
    /// than an overlay, is currently showing. Defaults `false` — ambient
    /// by design, the resting experience is unchanged until the user
    /// presses `Tab` (and only when [`rail_can_open`] allows it).
    pub rail_open: bool,
    /// The rail's currently-selected row, clamped at READ time against the
    /// freshly-built row count (see [`App::rail_selected_clamped`]) — same
    /// posture as `stream_selected`/`mastery_selected`.
    pub rail_selected: usize,
    pub tick: u64,
    /// Step 5: `Some(tick)` while the response-acknowledgment beat is still
    /// showing — cleared once `tick` advances past this value. Paired with
    /// `acked_card` below, since `ws.pending_card` is already cleared by the
    /// time a response resolves (see `App::start_ack`'s doc).
    ack_until_tick: Option<u64>,
    /// Step 5: a snapshot of the card that was just responded to (`a`/`g`),
    /// taken by the caller BEFORE `keys::handle_card_key` frees
    /// `ws.pending_card` — so the surface has something to flash green
    /// for the beat's duration even though the live slot is already empty.
    acked_card: Option<PendingCard>,
    /// `Some(buffer)` while the inline goal editor is open (founder request:
    /// the goal should be visible AND adjustable in-TUI, not only via
    /// `murshid goal <text>`). While open, the event loop routes ALL character
    /// keys into this buffer (so `q`/`m`/etc. type instead of firing commands);
    /// Enter persists via `goal::write_goal_file`, Esc discards. `None` = closed.
    goal_edit: Option<String>,
    /// Redesign R3 (fixes G3): whether the settings popup is layered over
    /// the focus pane right now. Deliberately NOT a `Focus` variant — unlike
    /// the summon+pop stack, opening/closing this must never disturb
    /// whatever's underneath (`pending_card`/rail state), so
    /// it's a plain flag beside the stack rather than part of it. `s` opens
    /// it (unconditionally — that's the whole point of the transient) and
    /// `s`/`esc` while open closes it, returning to exactly what was there.
    settings_open: bool,
    /// Redesign R3: `Some(buffer)` while the `:` command palette is open —
    /// mirrors `goal_edit`'s text-capture shape exactly (Enter executes via
    /// `mod.rs::parse_command` + dispatch, Esc cancels, Backspace edits).
    /// `None` = closed.
    command_input: Option<String>,
    /// Redesign R5 (ask mode): `Some(buffer)` while a conversational
    /// follow-up is open on the current `pending_card` — text-capture shape
    /// like `goal_edit`/`command_input`, EXCEPT `Enter` does not close it
    /// (mirrors a chat input: sending clears the buffer but keeps the
    /// thread view open for a further question). The card's `card_id` is
    /// deliberately NOT duplicated here — the live `ws.pending_card` is the
    /// single source of truth for which card the thread is on; asking never
    /// touches/drops it. `None` = closed (back on the plain card view).
    ask_input: Option<String>,
}

impl App {
    pub fn new() -> Self {
        App {
            focus_stack: vec![Focus::Home],
            show_help: false,
            should_quit: false,
            mastery_selected: 0,
            events_selected: 0,
            events_filter: EventsFilter::All,
            settings_selected: 0,
            stream_selected: 0,
            stream_expanded_card: None,
            rail_open: false,
            rail_selected: 0,
            tick: 0,
            ack_until_tick: None,
            acked_card: None,
            goal_edit: None,
            settings_open: false,
            command_input: None,
            ask_input: None,
        }
    }

    /// The currently-focused view — always `Some` entry, never empty.
    pub fn focus(&self) -> &Focus {
        self.focus_stack.last().expect("focus_stack is never empty")
    }

    /// Summons `f` on top of the stack (design doc §5.2's verb-based nav:
    /// `m`/`e`/`⏎` push, they never replace history).
    pub fn push_focus(&mut self, f: Focus) {
        self.focus_stack.push(f);
    }

    /// `esc` — pops one level back toward `Home`. A stack of just `[Home]`
    /// is a no-op: home has nowhere further back to go.
    pub fn pop_focus(&mut self) {
        if self.focus_stack.len() > 1 {
            self.focus_stack.pop();
        }
    }

    /// The global `m`/`esc`-from-anywhere-in-mastery "jump home" shortcut
    /// (design doc §3.6's concept-detail keybar: "m/esc home", distinct from
    /// plain `esc` there, which only steps back to the meter one level up).
    pub fn go_home(&mut self) {
        self.focus_stack.truncate(1);
    }

    /// `Tab` on the Home surface: flips `rail_open`, gated on
    /// [`rail_can_open`] — below `RAIL_MIN_WIDTH` this is a no-op (returns
    /// `false`) rather than opening a rail that has nowhere to fit; the
    /// caller surfaces a brief notice in that case. Opening resets
    /// `rail_selected` to `0` so a stale selection from a previous session
    /// never carries over.
    pub fn toggle_rail(&mut self, term_width: u16) -> bool {
        if !rail_can_open(term_width) {
            return false;
        }
        self.rail_open = !self.rail_open;
        if self.rail_open {
            self.rail_selected = 0;
        }
        true
    }

    /// The rail's selection, clamped to `len` (the freshly-built row count)
    /// — same "clamp at read time, never store a clamped value" posture as
    /// [`App::stream_selected_clamped`].
    pub fn rail_selected_clamped(&self, len: usize) -> usize {
        if len == 0 {
            0
        } else {
            self.rail_selected.min(len - 1)
        }
    }

    /// T17 R4: the stream's OLDER-rows selection, clamped to `len` (the
    /// freshly-fetched row count) — same "clamp at read time, never store a
    /// clamped value" posture `draw_mastery`/`draw_events` already use
    /// inline; pulled out here so both the stream's `⏎` handler
    /// (`tui/mod.rs`) and its render (`tui/view.rs`) agree on exactly the
    /// same selected row.
    pub fn stream_selected_clamped(&self, len: usize) -> usize {
        if len == 0 {
            0
        } else {
            self.stream_selected.min(len - 1)
        }
    }

    /// `\u{2191}` on the stream (rail closed): selects the NEWER neighbor
    /// (index `0` is already the newest OLDER row, so this saturates rather
    /// than going negative). Collapses whatever was expanded — a stale
    /// expansion must never linger once the selection has moved off it.
    pub fn stream_select_up(&mut self) {
        self.stream_selected = self.stream_selected.saturating_sub(1);
        self.stream_expanded_card = None;
    }

    /// `\u{2193}` on the stream (rail closed): selects the OLDER neighbor,
    /// clamped to `len` (the freshly-fetched OLDER-row count) so it can
    /// never walk past the end of a shrinking/empty list. Collapses
    /// whatever was expanded, mirroring `stream_select_up`.
    pub fn stream_select_down(&mut self, len: usize) {
        if len == 0 {
            self.stream_selected = 0;
        } else {
            self.stream_selected = (self.stream_selected + 1).min(len - 1);
        }
        self.stream_expanded_card = None;
    }

    /// `\u{23ce}` on the stream's selected OLDER row: toggles its inline
    /// expand (the row's already-persisted body, shown in a small wrapped
    /// panel beneath the list — see `view::draw_stream`) — pressing it
    /// again on the SAME row collapses it back; pressing it on a DIFFERENT
    /// row switches straight to that one.
    pub fn toggle_stream_expand(&mut self, card_id: i64) {
        self.stream_expanded_card = if self.stream_expanded_card == Some(card_id) {
            None
        } else {
            Some(card_id)
        };
    }

    /// The id of the OLDER row currently expanded inline, if any.
    pub fn stream_expanded_card(&self) -> Option<i64> {
        self.stream_expanded_card
    }

    /// Design doc §5.4: starts a response-acknowledgment beat lasting
    /// [`ACK_BEAT_TICKS`] past the current tick, holding onto `card` (a
    /// snapshot taken before the caller frees `ws.pending_card`) so the
    /// surface has something to render green for the beat's duration.
    pub fn start_ack(&mut self, card: PendingCard) {
        self.ack_until_tick = Some(self.tick + ACK_BEAT_TICKS);
        self.acked_card = Some(card);
    }

    /// Whether the ack beat is still showing at the current tick.
    pub fn ack_active(&self) -> bool {
        self.ack_until_tick.map(|until| self.tick <= until).unwrap_or(false)
    }

    /// The acknowledged card to render green for the beat's duration —
    /// `Some` only while [`App::ack_active`] is true.
    pub fn acked_card(&self) -> Option<&PendingCard> {
        if self.ack_active() {
            self.acked_card.as_ref()
        } else {
            None
        }
    }

    /// Opens the inline goal editor, seeded with the current goal text (empty
    /// string when no goal is set — `goal_text_now` returns `""` then).
    pub fn start_goal_edit(&mut self, current: String) {
        self.goal_edit = Some(current);
    }

    /// The live edit buffer while the goal editor is open (`None` = closed).
    pub fn goal_edit_buf(&self) -> Option<&str> {
        self.goal_edit.as_deref()
    }

    pub fn is_editing_goal(&self) -> bool {
        self.goal_edit.is_some()
    }

    pub fn goal_edit_push(&mut self, c: char) {
        if let Some(b) = self.goal_edit.as_mut() {
            b.push(c);
        }
    }

    pub fn goal_edit_backspace(&mut self) {
        if let Some(b) = self.goal_edit.as_mut() {
            b.pop();
        }
    }

    /// Closes the editor and returns the final buffer — Enter (save) persists
    /// it; Esc (cancel) drops the returned value.
    pub fn goal_edit_take(&mut self) -> Option<String> {
        self.goal_edit.take()
    }

    /// Opens the settings popup (redesign R3) — a transient layered over
    /// whatever's on the focus pane. Deliberately does NOT touch
    /// `settings_selected` (a fresh open resumes on whichever row was last
    /// selected, same posture as the old `Focus::Settings` push) or anything
    /// else on `App`/`WatchSession` — the whole point of G3 is that opening
    /// this must leave the card/rail underneath completely undisturbed.
    pub fn open_settings(&mut self) {
        self.settings_open = true;
    }

    /// `s`/`esc` while the settings popup is open — closes it, returning to
    /// exactly what was underneath (nothing else changes).
    pub fn close_settings(&mut self) {
        self.settings_open = false;
    }

    pub fn is_settings_open(&self) -> bool {
        self.settings_open
    }

    /// Opens the `:` command palette, seeded empty.
    pub fn start_command(&mut self) {
        self.command_input = Some(String::new());
    }

    /// The live edit buffer while the command palette is open (`None` =
    /// closed) — mirrors [`App::goal_edit_buf`].
    pub fn command_buf(&self) -> Option<&str> {
        self.command_input.as_deref()
    }

    pub fn is_editing_command(&self) -> bool {
        self.command_input.is_some()
    }

    pub fn command_push(&mut self, c: char) {
        if let Some(b) = self.command_input.as_mut() {
            b.push(c);
        }
    }

    pub fn command_backspace(&mut self) {
        if let Some(b) = self.command_input.as_mut() {
            b.pop();
        }
    }

    /// Closes the palette and returns the final buffer — Enter (execute)
    /// dispatches it; Esc (cancel) drops the returned value.
    pub fn command_take(&mut self) -> Option<String> {
        self.command_input.take()
    }

    /// Redesign R5: opens ask mode on the current card, seeded empty. The
    /// caller (`tui::mod::handle_key`) is responsible for only calling this
    /// when a card is actually pending — `App` itself has no opinion on
    /// that (it doesn't hold `WatchSession` state).
    pub fn start_ask(&mut self) {
        self.ask_input = Some(String::new());
    }

    /// The live edit buffer while ask mode is open (`None` = closed) —
    /// mirrors [`App::goal_edit_buf`]/[`App::command_buf`].
    pub fn ask_buf(&self) -> Option<&str> {
        self.ask_input.as_deref()
    }

    pub fn is_asking(&self) -> bool {
        self.ask_input.is_some()
    }

    pub fn ask_push(&mut self, c: char) {
        if let Some(b) = self.ask_input.as_mut() {
            b.push(c);
        }
    }

    pub fn ask_backspace(&mut self) {
        if let Some(b) = self.ask_input.as_mut() {
            b.pop();
        }
    }

    /// `Enter` in ask mode: unlike `goal_edit_take`/`command_take`, sending
    /// a question does NOT close ask mode (a chat input keeps accepting
    /// follow-ups) — it only clears the buffer and returns the trimmed
    /// question, or `None` (buffer left untouched) when there was nothing
    /// but whitespace to send.
    pub fn ask_send(&mut self) -> Option<String> {
        let buf = self.ask_input.as_mut()?;
        let trimmed = buf.trim().to_string();
        if trimmed.is_empty() {
            return None;
        }
        buf.clear();
        Some(trimmed)
    }

    /// `Esc` in ask mode: closes it outright, discarding whatever was typed
    /// — the card itself (`ws.pending_card`) is untouched either way.
    pub fn exit_ask(&mut self) {
        self.ask_input = None;
    }
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ladder;

    #[test]
    fn test_events_filter_cycles_and_wraps() {
        let mut f = EventsFilter::All;
        for _ in 0..4 {
            f = f.next();
        }
        assert_eq!(f, EventsFilter::All);
    }

    #[test]
    fn test_events_filter_matches() {
        assert!(EventsFilter::All.matches("anything"));
        assert!(EventsFilter::JudgeDeclined.matches("judge_declined"));
        assert!(!EventsFilter::JudgeDeclined.matches("judge_drop"));
        assert!(EventsFilter::CardShown.matches("card_shown"));
    }

    #[test]
    fn test_app_new_defaults_to_home() {
        let app = App::new();
        assert_eq!(app.focus(), &Focus::Home);
        assert!(!app.show_help);
        assert!(!app.should_quit);
        assert_eq!(app.tick, 0);
        assert!(!app.ack_active());
    }

    #[test]
    fn test_push_and_pop_focus_is_a_stack() {
        let mut app = App::new();
        app.push_focus(Focus::Mastery);
        assert_eq!(app.focus(), &Focus::Mastery);
        app.push_focus(Focus::ConceptDetail("borrow-vs-clone".to_string()));
        assert_eq!(
            app.focus(),
            &Focus::ConceptDetail("borrow-vs-clone".to_string())
        );
        app.pop_focus();
        assert_eq!(app.focus(), &Focus::Mastery);
        app.pop_focus();
        assert_eq!(app.focus(), &Focus::Home);
    }

    #[test]
    fn test_pop_focus_at_home_is_a_no_op() {
        let mut app = App::new();
        app.pop_focus();
        assert_eq!(app.focus(), &Focus::Home);
    }

    // --- Redesign R3: settings is a transient popup, not a Focus ---

    #[test]
    fn test_settings_popup_opens_and_closes_without_touching_focus_stack() {
        let mut app = App::new();
        assert_eq!(app.settings_selected, 0);
        assert!(!app.is_settings_open());
        app.open_settings();
        assert!(app.is_settings_open());
        // Opening/closing the popup must never touch the focus stack — it
        // layers OVER whatever's focused, it never replaces it.
        assert_eq!(app.focus(), &Focus::Home);
        app.close_settings();
        assert!(!app.is_settings_open());
        assert_eq!(app.focus(), &Focus::Home);
    }

    #[test]
    fn test_settings_popup_opens_over_any_focus_leaving_it_untouched() {
        let mut app = App::new();
        app.push_focus(Focus::Mastery);
        app.open_settings();
        assert!(app.is_settings_open());
        assert_eq!(
            app.focus(),
            &Focus::Mastery,
            "the popup must layer over the focus pane, not replace it"
        );
    }

    #[test]
    fn test_command_palette_open_type_backspace_take() {
        let mut app = App::new();
        assert!(!app.is_editing_command());
        app.start_command();
        assert!(app.is_editing_command());
        app.command_push('h');
        app.command_push('x');
        app.command_backspace();
        app.command_push('i');
        app.command_push('s');
        app.command_push('t');
        assert_eq!(app.command_buf(), Some("hist"));
        assert_eq!(app.command_take().as_deref(), Some("hist"));
        assert!(!app.is_editing_command(), "take() closes the palette");
        assert_eq!(app.command_buf(), None);
    }

    #[test]
    fn test_go_home_jumps_past_multiple_levels() {
        let mut app = App::new();
        app.push_focus(Focus::Mastery);
        app.push_focus(Focus::ConceptDetail("c1".to_string()));
        app.go_home();
        assert_eq!(app.focus(), &Focus::Home);
    }

    fn sample_card() -> PendingCard {
        PendingCard {
            card_id: 1,
            session_id: "sess1".to_string(),
            concept_id: "borrow-vs-clone".to_string(),
            concept_name: "Borrow vs. clone".to_string(),
            advice_fp: "fp-1".to_string(),
            category: "idiom".to_string(),
            rung: ladder::Rung::R2,
            card: crate::card::Card {
                concept_name: "Borrow vs. clone".to_string(),
                file: "src/main.rs".to_string(),
                line: 42,
                grounding_quote: "person.name.clone()".to_string(),
                why: "why".to_string(),
                rule: "rule".to_string(),
                doc_ref: "ref".to_string(),
                worked_diff: "diff".to_string(),
                additional_anchors: Vec::new(),
                overflow_site_count: 0,
            },
            site_enclosing_item: None,
            site_anchor_hash: None,
            from_struggle_offer: false,
        }
    }

    #[test]
    fn test_goal_edit_open_type_backspace_take() {
        let mut app = App::new();
        assert!(!app.is_editing_goal());
        app.start_goal_edit("ship".to_string());
        assert!(app.is_editing_goal());
        app.goal_edit_push(' ');
        app.goal_edit_push('x');
        app.goal_edit_backspace();
        app.goal_edit_push('i');
        app.goal_edit_push('t');
        assert_eq!(app.goal_edit_buf(), Some("ship it"));
        assert_eq!(app.goal_edit_take().as_deref(), Some("ship it"));
        assert!(!app.is_editing_goal(), "take() closes the editor");
        assert_eq!(app.goal_edit_buf(), None);
    }

    // --- T17 R4: stream selection / inline expand (retires the old
    // `Focus::History`/`Focus::HistoryDetail` overlay tests above) ---

    #[test]
    fn test_stream_selected_clamped() {
        let app = App::new();
        assert_eq!(app.stream_selected_clamped(0), 0);

        let mut app = App::new();
        app.stream_selected = 7;
        assert_eq!(app.stream_selected_clamped(0), 0, "no rows clamps to 0");
        assert_eq!(app.stream_selected_clamped(3), 2, "clamps to the last row");
        assert_eq!(app.stream_selected_clamped(10), 7, "within range is untouched");
    }

    #[test]
    fn test_stream_select_up_saturates_at_zero() {
        let mut app = App::new();
        app.stream_select_up();
        assert_eq!(app.stream_selected, 0, "selection never goes negative");

        app.stream_select_down(5);
        app.stream_select_down(5);
        app.stream_select_up();
        assert_eq!(app.stream_selected, 1);
    }

    #[test]
    fn test_stream_select_down_clamps_to_last_and_handles_empty() {
        let mut app = App::new();
        app.stream_select_down(0);
        assert_eq!(app.stream_selected, 0, "an empty list clamps to 0");

        let mut app = App::new();
        for _ in 0..10 {
            app.stream_select_down(3);
        }
        assert_eq!(app.stream_selected, 2, "never walks past the last row");
    }

    #[test]
    fn test_stream_selecting_collapses_a_pending_expansion() {
        let mut app = App::new();
        app.toggle_stream_expand(42);
        assert_eq!(app.stream_expanded_card(), Some(42));

        app.stream_select_down(5);
        assert_eq!(
            app.stream_expanded_card(),
            None,
            "moving the selection must collapse a stale expansion"
        );

        app.toggle_stream_expand(7);
        app.stream_select_up();
        assert_eq!(app.stream_expanded_card(), None);
    }

    #[test]
    fn test_toggle_stream_expand_toggles_same_row_and_switches_to_a_different_one() {
        let mut app = App::new();
        assert_eq!(app.stream_expanded_card(), None);

        app.toggle_stream_expand(1);
        assert_eq!(app.stream_expanded_card(), Some(1));

        // Pressing it again on the SAME row collapses it back.
        app.toggle_stream_expand(1);
        assert_eq!(app.stream_expanded_card(), None);

        app.toggle_stream_expand(1);
        // Pressing it on a DIFFERENT row switches straight to that one.
        app.toggle_stream_expand(2);
        assert_eq!(app.stream_expanded_card(), Some(2));
    }

    // --- Redesign R2: RAIL_MIN_WIDTH gate / Tab toggle / selection clamp ---

    #[test]
    fn test_rail_can_open_gates_on_min_width() {
        assert!(!rail_can_open(RAIL_MIN_WIDTH - 1));
        assert!(rail_can_open(RAIL_MIN_WIDTH));
        assert!(rail_can_open(RAIL_MIN_WIDTH + 40));
    }

    #[test]
    fn test_toggle_rail_flips_open_above_min_width() {
        let mut app = App::new();
        assert!(!app.rail_open);
        assert!(app.toggle_rail(RAIL_MIN_WIDTH));
        assert!(app.rail_open);
        assert!(app.toggle_rail(RAIL_MIN_WIDTH));
        assert!(!app.rail_open, "a second Tab closes it again");
    }

    #[test]
    fn test_toggle_rail_is_a_no_op_below_min_width() {
        let mut app = App::new();
        assert!(!app.toggle_rail(RAIL_MIN_WIDTH - 1));
        assert!(!app.rail_open, "narrow terminal: rail must stay closed");
    }

    #[test]
    fn test_toggle_rail_opening_resets_selection() {
        let mut app = App::new();
        app.rail_selected = 5;
        app.toggle_rail(RAIL_MIN_WIDTH);
        assert_eq!(app.rail_selected, 0);
    }

    #[test]
    fn test_rail_selected_clamped() {
        let app = App::new();
        assert_eq!(app.rail_selected_clamped(0), 0);

        let mut app = App::new();
        app.rail_selected = 9;
        assert_eq!(app.rail_selected_clamped(0), 0, "empty rail clamps to 0");
        assert_eq!(app.rail_selected_clamped(3), 2, "clamps to the last row");
        assert_eq!(app.rail_selected_clamped(20), 9, "within range is untouched");
    }

    // --- Redesign R2: the persistent header mode token ---

    #[test]
    fn test_mode_token_home_watching_vs_hint() {
        assert_eq!(mode_token(&Focus::Home, false, false).label(), "watching");
        assert_eq!(mode_token(&Focus::Home, true, false).label(), "hint");
    }

    #[test]
    fn test_mode_token_reading_for_detail_views() {
        assert_eq!(
            mode_token(&Focus::ConceptDetail("c1".to_string()), false, false).label(),
            "reading"
        );
    }

    #[test]
    fn test_mode_token_names_the_overlay() {
        assert_eq!(mode_token(&Focus::Mastery, false, false).label(), "mastery");
        assert_eq!(mode_token(&Focus::Events, false, false).label(), "events");
    }

    // --- Redesign R5: ask mode ---

    #[test]
    fn test_mode_token_ask_wins_over_hint() {
        assert_eq!(mode_token(&Focus::Home, true, true).label(), "ask");
        // Even the (shouldn't-happen) case of asking with no card up still
        // reports "ask" — `asking` is checked first, unconditionally.
        assert_eq!(mode_token(&Focus::Home, false, true).label(), "ask");
    }

    #[test]
    fn test_ask_mode_open_type_backspace_send_keeps_it_open() {
        let mut app = App::new();
        assert!(!app.is_asking());
        app.start_ask();
        assert!(app.is_asking());
        assert_eq!(app.ask_buf(), Some(""));

        app.ask_push('w');
        app.ask_push('h');
        app.ask_push('y');
        app.ask_push('x');
        app.ask_backspace();
        assert_eq!(app.ask_buf(), Some("why"));

        let sent = app.ask_send();
        assert_eq!(sent.as_deref(), Some("why"));
        assert!(app.is_asking(), "sending a question must not close ask mode");
        assert_eq!(app.ask_buf(), Some(""), "the buffer clears after sending");
    }

    #[test]
    fn test_ask_send_blank_buffer_is_a_no_op() {
        let mut app = App::new();
        app.start_ask();
        assert_eq!(app.ask_send(), None, "nothing but whitespace never sends");
        app.ask_push(' ');
        assert_eq!(app.ask_send(), None);
        assert_eq!(app.ask_buf(), Some(" "), "a no-op send leaves the buffer untouched");
    }

    #[test]
    fn test_exit_ask_closes_it() {
        let mut app = App::new();
        app.start_ask();
        app.ask_push('x');
        app.exit_ask();
        assert!(!app.is_asking());
        assert_eq!(app.ask_buf(), None);
    }

    #[test]
    fn test_ack_beat_active_for_its_duration_then_expires() {
        let mut app = App::new();
        app.start_ack(sample_card());
        assert!(app.ack_active());
        assert!(app.acked_card().is_some());

        app.tick = ACK_BEAT_TICKS;
        assert!(app.ack_active(), "still active at the exact boundary tick");

        app.tick = ACK_BEAT_TICKS + 1;
        assert!(!app.ack_active());
        assert!(app.acked_card().is_none());
    }
}
