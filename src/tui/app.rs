//! T15: the TUI's own UI state — which tab is focused, list selections,
//! the help overlay, and the quit flag. Deliberately tiny: everything else
//! the views draw (the active card, the queue, mastery, events, ...) is
//! read fresh from `WatchSession`/`profile.db` on every tick rather than
//! cached here.

/// The four required views (T15 spec): Dashboard (flagship), Mastery meter,
/// Event/history browser, Card+thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Dashboard,
    Mastery,
    Events,
    Card,
}

impl Tab {
    pub const ALL: [Tab; 4] = [Tab::Dashboard, Tab::Mastery, Tab::Events, Tab::Card];

    pub fn label(&self) -> &'static str {
        match self {
            Tab::Dashboard => "1 Dashboard",
            Tab::Mastery => "2 Mastery",
            Tab::Events => "3 Events",
            Tab::Card => "4 Card",
        }
    }

    /// Tab switch cycles forward with the `Tab` key — no memorized order
    /// needed since the keybar always shows `1-4` too.
    pub fn next(&self) -> Tab {
        match self {
            Tab::Dashboard => Tab::Mastery,
            Tab::Mastery => Tab::Events,
            Tab::Events => Tab::Card,
            Tab::Card => Tab::Dashboard,
        }
    }
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

/// The TUI's own state — separate from `WatchSession` (the engine's shared,
/// multi-threaded state) on purpose: nothing here is touched by the worker
/// threads, so it needs no locking.
pub struct App {
    pub tab: Tab,
    pub show_help: bool,
    pub should_quit: bool,
    pub mastery_selected: usize,
    pub events_selected: usize,
    pub events_filter: EventsFilter,
}

impl App {
    pub fn new() -> Self {
        App {
            tab: Tab::Dashboard,
            show_help: false,
            should_quit: false,
            mastery_selected: 0,
            events_selected: 0,
            events_filter: EventsFilter::All,
        }
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

    #[test]
    fn test_tab_cycles_through_all_four_and_back() {
        let mut t = Tab::Dashboard;
        for _ in 0..4 {
            t = t.next();
        }
        assert_eq!(t, Tab::Dashboard);
    }

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
    fn test_app_new_defaults() {
        let app = App::new();
        assert_eq!(app.tab, Tab::Dashboard);
        assert!(!app.show_help);
        assert!(!app.should_quit);
    }
}
