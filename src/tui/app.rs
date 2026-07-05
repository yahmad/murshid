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
    Home,
    Mastery,
    /// Holds the drilled-into concept's taxonomy slug (design doc §3.6's
    /// concept-detail "level"/drill-down, reached via `⏎` on a mastery row).
    ConceptDetail(String),
    Events,
}

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
    /// Step 2: incremented once per event-loop poll iteration (~200ms) —
    /// the header pulse's and "thinking" face's animation frame index
    /// (design doc §3.4: "index the frame by a tick counter"). Wraps via
    /// the modulo in `theme::working_pulse_frame`, so overflow is harmless.
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
            tick: 0,
            ack_until_tick: None,
            acked_card: None,
            goal_edit: None,
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
