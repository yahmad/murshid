//! T15: view drawing for the four required tabs (Dashboard/Mastery/Events/
//! Card), the always-visible contextual keybar, and the `?` help overlay.
//!
//! Deliberately "read fresh, render plain": every view re-reads
//! `WatchSession`/`profile.db` state on every tick rather than caching
//! anything, and card/queue/mastery rendering reuses the pure engine
//! functions (`card::render_card_at_rung`, `progress::build_rows`,
//! `queue::render_queue_list`) wrapped in a scrollable `Paragraph` — the
//! architecture doc's cheapest-path recommendation. No ratatui type is ever
//! passed back into the engine.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::Frame;

use crate::sync_ext::LockExt;
use crate::watch::WatchSession;
use crate::{db, judge, pack};

use super::app::{App, Tab};

/// Everything a draw pass needs, bundled once per tick — avoids an
/// eight-plus-argument `draw` signature (this repo's convention for a
/// param bundle rather than a `#[allow(clippy::too_many_arguments)]`).
pub struct DrawContext<'a> {
    pub ws: &'a WatchSession,
    pub conn: Option<&'a rusqlite::Connection>,
    pub project_root: &'a std::path::Path,
    pub taxonomy: &'a [pack::TaxonomyConcept],
    pub mode: &'a judge::JudgeMode,
    pub surface: &'a pack::SurfaceConfig,
}

pub fn draw(f: &mut Frame, app: &App, ctx: &DrawContext) {
    let size = f.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(3),
            Constraint::Length(2),
        ])
        .split(size);

    draw_tabs(f, chunks[0], app.tab);

    match app.tab {
        Tab::Dashboard => draw_dashboard(f, chunks[1], ctx),
        Tab::Mastery => draw_mastery(f, chunks[1], app, ctx),
        Tab::Events => draw_events(f, chunks[1], app, ctx),
        Tab::Card => draw_card(f, chunks[1], ctx),
    }

    draw_keybar(f, chunks[2], app, ctx);

    if app.show_help {
        draw_help_overlay(f, size);
    }
}

fn draw_tabs(f: &mut Frame, area: Rect, active: Tab) {
    let mut spans = Vec::new();
    for (i, t) in Tab::ALL.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw("  "));
        }
        let style = if *t == active {
            Style::default().add_modifier(Modifier::REVERSED)
        } else {
            Style::default()
        };
        spans.push(Span::styled(t.label(), style));
    }
    spans.push(Span::raw("    ? help   q/Ctrl-C quit"));
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// V1 — the flagship dashboard (T15 spec): active card, queue depth + top
/// concepts, budget/throttle state, goal + drift, judge/degraded mode, and
/// a recent-activity strip.
/// T15 fix (dogfood 2026-07-05, "silence is ambiguous"): the dashboard's
/// parse-gate status line. `None` when nothing is held by the C12 parse gate;
/// otherwise a one-liner naming the file(s) that don't parse yet, so a
/// deliberate "waiting" hold is never mistaken for murshid being broken/idle.
pub fn parse_wait_line(waiting: &[String]) -> Option<String> {
    if waiting.is_empty() {
        return None;
    }
    Some(format!(
        "\u{23f8}  waiting \u{2014} {} file(s) don't parse yet (fix syntax to resume): {}",
        waiting.len(),
        waiting.join(", ")
    ))
}

fn draw_dashboard(f: &mut Frame, area: Rect, ctx: &DrawContext) {
    let mut body = String::new();

    match ctx.ws.pending_card.lock_poison_safe().clone() {
        Some(pc) => body.push_str(&crate::card::render_card_at_rung(
            &pc.card,
            pc.rung,
            0,
            &ctx.surface.comment_token,
        )),
        None => body.push_str("(no card on screen)\n"),
    }
    body.push('\n');

    // T15 fix: make the C12 parse-gate hold legible — otherwise "waiting
    // because your file doesn't parse yet" looks identical to "idle/broken"
    // (the dogfood 2026-07-05 "silence is ambiguous" finding).
    if let Some(line) = parse_wait_line(&ctx.ws.parse_waiting.lock_poison_safe()) {
        body.push_str(&line);
        body.push_str("\n\n");
    }

    if let Some(po) = ctx.ws.pending_offer.lock_poison_safe().clone() {
        body.push_str(&format!(
            "offer pending: {} ({}) \u{2014} y accept / n decline\n\n",
            po.key.0, po.key.1
        ));
    }

    {
        let mut q = ctx.ws.queue_state.lock_poison_safe().clone();
        let cluster = ctx.ws.goal_cluster_dirs.lock_poison_safe().clone();
        let goal_text = crate::goal_text_now(ctx.project_root);
        crate::queue::sort_queue(&mut q, &cluster, &goal_text);
        body.push_str(&format!("queue: {} pending\n", q.len()));
        if !q.is_empty() {
            let top: Vec<_> = q.iter().take(3).cloned().collect();
            body.push_str(&crate::queue::render_queue_list(&top));
        }
        body.push('\n');
    }

    {
        let tokens = ctx.ws.bucket.lock_poison_safe().tokens_available();
        let mut throttled: Vec<String> = ctx
            .ws
            .throttled_categories
            .lock_poison_safe()
            .iter()
            .cloned()
            .collect();
        throttled.sort();
        body.push_str(&format!("budget: {:.1} tokens available\n", tokens));
        if throttled.is_empty() {
            body.push_str("throttled categories: none\n");
        } else {
            body.push_str(&format!(
                "throttled categories: {}\n",
                throttled.join(", ")
            ));
        }
        body.push('\n');
    }

    {
        let goal_text = crate::goal_text_now(ctx.project_root);
        let drift_fired = ctx.ws.drift_tracking.lock_poison_safe().fired;
        body.push_str(&format!(
            "goal: {}\n",
            if goal_text.trim().is_empty() {
                "(none set)".to_string()
            } else {
                goal_text
            }
        ));
        if drift_fired {
            body.push_str(&format!("  {}\n", crate::goal::DRIFT_NOTICE));
        }
        body.push('\n');
    }

    match ctx.mode {
        judge::JudgeMode::Degraded { reason } => {
            body.push_str(&format!("{}\n", judge::degraded_status_line(reason)))
        }
        judge::JudgeMode::Active => body.push_str("judge: live\n"),
    }
    body.push('\n');

    body.push_str("recent activity:\n");
    let log = ctx.ws.activity_log.lock_poison_safe();
    if log.is_empty() {
        body.push_str("  (nothing yet)\n");
    } else {
        for line in log.iter().rev().take(8) {
            for l in line.lines() {
                body.push_str("  ");
                body.push_str(l);
                body.push('\n');
            }
        }
    }
    drop(log);

    let para = Paragraph::new(body)
        .wrap(Wrap { trim: false })
        .block(Block::default().borders(Borders::ALL).title("Dashboard"));
    f.render_widget(para, area);
}

/// V2 — `progress::build_rows` as a navigable list (mastery, help
/// level/rung, staleness, throttle flag).
fn draw_mastery(f: &mut Frame, area: Rect, app: &App, ctx: &DrawContext) {
    let Some(conn) = ctx.conn else {
        f.render_widget(
            Paragraph::new("(no database connection)")
                .block(Block::default().borders(Borders::ALL).title("Mastery")),
            area,
        );
        return;
    };
    let memory_rows = db::list_concept_memory(conn).unwrap_or_default();
    let throttled = ctx.ws.throttled_categories.lock_poison_safe().clone();
    let now_epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let rows = crate::progress::build_rows(&memory_rows, ctx.taxonomy, &throttled, now_epoch);

    if rows.is_empty() {
        f.render_widget(
            Paragraph::new("(no concepts in the taxonomy)")
                .block(Block::default().borders(Borders::ALL).title("Mastery")),
            area,
        );
        return;
    }

    let selected = app.mastery_selected.min(rows.len() - 1);
    let items: Vec<ListItem> = rows
        .iter()
        .map(|r| {
            let pct = (r.p_mastery.clamp(0.0, 1.0) * 100.0).round() as u32;
            ListItem::new(format!(
                "[{}] {:<28} {:>3}%  help:{}  {}",
                r.category,
                r.name,
                pct,
                r.help_level,
                r.state.as_str()
            ))
        })
        .collect();

    let mut state = ListState::default();
    state.select(Some(selected));
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Mastery meter (\u{2191}/\u{2193} or j/k)"),
        )
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    f.render_stateful_widget(list, area, &mut state);
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        s.chars().take(n).collect::<String>() + "\u{2026}"
    }
}

/// V3 — the `events` table as a scrollable, filterable log, rendering T14's
/// `judge_declined` vs `judge_drop` (+ `card_shown`) distinctly (the
/// "silence is ambiguous" fix made visible).
fn draw_events(f: &mut Frame, area: Rect, app: &App, ctx: &DrawContext) {
    let Some(conn) = ctx.conn else {
        f.render_widget(
            Paragraph::new("(no database connection)")
                .block(Block::default().borders(Borders::ALL).title("Events")),
            area,
        );
        return;
    };
    let sid = ctx.ws.session_mgr.lock_poison_safe().session_id.clone();
    let events = db::get_events_for_session(conn, &sid).unwrap_or_default();
    let filtered: Vec<&db::EventRecord> = events
        .iter()
        .filter(|e| app.events_filter.matches(&e.kind))
        .collect();

    let title = format!(
        "Events [{}] (f to cycle filter)",
        app.events_filter.label()
    );
    if filtered.is_empty() {
        f.render_widget(
            Paragraph::new("(no events yet)")
                .block(Block::default().borders(Borders::ALL).title(title)),
            area,
        );
        return;
    }

    let selected = app.events_selected.min(filtered.len() - 1);
    let items: Vec<ListItem> = filtered
        .iter()
        .map(|e| {
            // T14: `judge_declined` (correct "not a teaching moment" call)
            // vs `judge_drop` (a genuine contract failure) vs `card_shown`
            // are marked distinctly — the exact ambiguity the spec calls
            // out made legible.
            let marker = match e.kind.as_str() {
                "judge_declined" => "~",
                "judge_drop" => "!",
                "card_shown" => "*",
                _ => " ",
            };
            let ts = e.ts.clone().unwrap_or_default();
            ListItem::new(format!(
                "{} {} {}  {}",
                marker,
                ts,
                e.kind,
                truncate(&e.payload_json, 60)
            ))
        })
        .collect();

    let mut state = ListState::default();
    state.select(Some(selected));
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(title))
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    f.render_stateful_widget(list, area, &mut state);
}

/// V4 — the card's six fields (rendered) plus the anchored thread
/// transcript. Thread interaction (`k`) is read-only in v1 (T15 spec's
/// explicit allowance) — this view shows whatever transcript already
/// exists in `threads` rather than accepting new input.
fn draw_card(f: &mut Frame, area: Rect, ctx: &DrawContext) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
        .split(area);

    let maybe_pc = ctx.ws.pending_card.lock_poison_safe().clone();
    let card_text = match &maybe_pc {
        Some(pc) => {
            crate::card::render_card_at_rung(&pc.card, pc.rung, 0, &ctx.surface.comment_token)
        }
        None => "(no card focused)".to_string(),
    };
    f.render_widget(
        Paragraph::new(card_text)
            .wrap(Wrap { trim: false })
            .block(Block::default().borders(Borders::ALL).title("Card")),
        chunks[0],
    );

    let thread_text = match (&maybe_pc, ctx.conn) {
        (Some(pc), Some(conn)) => {
            let msgs = db::get_thread_messages(conn, pc.card_id).unwrap_or_default();
            if msgs.is_empty() {
                "(no thread yet \u{2014} ask (k) is read-only in this spike)".to_string()
            } else {
                msgs.iter()
                    .map(|m| format!("{}: {}", m.role, m.content))
                    .collect::<Vec<_>>()
                    .join("\n\n")
            }
        }
        _ => "(no card focused)".to_string(),
    };
    f.render_widget(
        Paragraph::new(thread_text)
            .wrap(Wrap { trim: false })
            .block(Block::default().borders(Borders::ALL).title("Thread")),
        chunks[1],
    );
}

/// The founder's core ask: an always-visible keybar showing exactly the
/// keys valid on the focused object — no memorization.
fn draw_keybar(f: &mut Frame, area: Rect, app: &App, ctx: &DrawContext) {
    let text = match app.tab {
        Tab::Mastery => {
            "\u{2191}/\u{2193} or j/k navigate   1-4 switch view   ? help   q quit".to_string()
        }
        Tab::Events => format!(
            "\u{2191}/\u{2193} navigate   f cycle filter ({})   1-4 switch view   ? help   q quit",
            app.events_filter.label()
        ),
        Tab::Dashboard | Tab::Card => {
            let has_offer = ctx.ws.pending_offer.lock_poison_safe().is_some();
            let has_card = ctx.ws.pending_card.lock_poison_safe().is_some();
            if has_offer {
                "y accept   n decline   1-4 switch view   ? help   q quit".to_string()
            } else if has_card {
                "a applied   g got it   u not useful   n not now   e escalate   t tell me   k ask(read-only)   1-4 switch view   ? help   q quit".to_string()
            } else {
                "(no active card)   1-4 switch view   ? help   q quit".to_string()
            }
        }
    };
    f.render_widget(Paragraph::new(text), area);
}

fn draw_help_overlay(f: &mut Frame, area: Rect) {
    let popup = centered_rect(72, 80, area);
    f.render_widget(Clear, popup);
    let text = "Murshid TUI \u{2014} help\n\
\n\
Views:\n\
  1  Dashboard \u{2014} active card, queue, budget/throttle, goal/drift, judge mode, recent activity\n\
  2  Mastery   \u{2014} per-concept mastery meter (\u{2191}/\u{2193} or j/k to navigate)\n\
  3  Events    \u{2014} the session's event log (\u{2191}/\u{2193} navigate, f cycles the kind filter)\n\
  4  Card      \u{2014} the focused card's six fields + the thread transcript\n\
  Tab          \u{2014} cycle views\n\
\n\
Card actions (Dashboard/Card, when a card is on screen):\n\
  a  applied      g  got it        u  not useful     n  not now (snooze)\n\
  e  escalate     t  tell me (jump to the worked example)\n\
  k  ask \u{2014} read-only in this spike; view the existing thread in the Card view\n\
\n\
Struggle offer (when one is pending):\n\
  y  accept       n  decline\n\
\n\
Global:\n\
  ?  toggle this help\n\
  q  or Ctrl-C    quit (runs the session-end bookend, same as before)\n\
\n\
press any key to close this help";
    let block = Block::default().borders(Borders::ALL).title("Help");
    f.render_widget(
        Paragraph::new(text).wrap(Wrap { trim: false }).block(block),
        popup,
    );
}

fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_truncate_short_string_unchanged() {
        assert_eq!(truncate("hello", 10), "hello");
    }

    #[test]
    fn test_parse_wait_line_none_when_nothing_held() {
        assert!(parse_wait_line(&[]).is_none());
    }

    #[test]
    fn test_parse_wait_line_names_files_and_prompts_the_fix() {
        let line = parse_wait_line(&["src/main.rs".to_string(), "src/lib.rs".to_string()])
            .expect("non-empty waiting set must produce a status line");
        assert!(line.contains("2 file(s)"), "counts the held files: {line}");
        assert!(line.contains("src/main.rs") && line.contains("src/lib.rs"));
        assert!(
            line.contains("fix syntax to resume"),
            "tells the user why it's quiet and how to resume: {line}"
        );
    }

    #[test]
    fn test_truncate_long_string_gets_ellipsis() {
        let s = "a".repeat(100);
        let t = truncate(&s, 10);
        assert_eq!(t.chars().count(), 11); // 10 chars + the ellipsis marker
        assert!(t.ends_with('\u{2026}'));
    }
}
