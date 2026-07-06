//! T15 UX redesign ("Focus", `specs/explorations/tui-ux-redesign.md`) —
//! view drawing: the 4-region layout (header / surface / ambient band /
//! keybar), the home surface's five faces (hero card, response-ack beat,
//! struggle-offer callout, working, waiting-on-parse, caught-up empty), the
//! mastery meter + concept-detail drill-down overlays, the events overlay,
//! and the `?` help overlay.
//!
//! Deliberately "read fresh, render plain": every view re-reads
//! `WatchSession`/`profile.db` state on every tick rather than caching
//! anything, and reuses the pure engine functions (`progress::build_rows`,
//! `queue::sort_queue`/`presence_indicator`, `ladder::render_worked_example`)
//! wherever they already exist. No ratatui type is ever passed back into the
//! engine.

use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Clear, List, ListItem, ListState, Padding, Paragraph, Sparkline, Wrap,
};
use ratatui::Frame;

use crate::sync_ext::LockExt;
use crate::watch::{LastReview, PendingCard, PendingOffer, ReviewResult, ReviewState, WatchSession};
use crate::{bkt, db, judge, ladder, offer, pack, progress, queue};

use super::app::{App, Focus};
use super::theme;

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

/// Design doc §4/§7 Step 1: the 4-region layout — header (1) / surface
/// (min) / ambient band (1) / keybar (1) — replacing the old 3-row
/// header+tabs/body/keybar split.
pub fn draw(f: &mut Frame, app: &App, ctx: &DrawContext) {
    let size = f.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(3),
            Constraint::Length(1),
            // Keybar: 2 rows + wrap, so a full card's chip set
            // (a/g/u/n/e/t/k/G) stays fully visible instead of clipping off
            // the right edge on a normal-width terminal.
            Constraint::Length(2),
        ])
        .split(size);

    draw_header(f, chunks[0], app, ctx);

    match app.focus() {
        Focus::Home => draw_home_surface(f, chunks[1], app, ctx),
        Focus::Mastery => draw_mastery(f, chunks[1], app, ctx),
        Focus::ConceptDetail(concept_id) => draw_concept_detail(f, chunks[1], concept_id, ctx),
        Focus::Events => draw_events(f, chunks[1], app, ctx),
        Focus::Settings => draw_settings(f, chunks[1], app, ctx),
        Focus::History => draw_history(f, chunks[1], app, ctx),
        Focus::HistoryDetail(card_id) => {
            draw_history_detail(f, chunks[1], app, ctx, *card_id)
        }
    }

    draw_ambient_band(f, chunks[2], ctx);
    draw_keybar(f, chunks[3], app, ctx);

    if app.show_help {
        draw_help_overlay(f, size);
    }
    if let Some(buf) = app.goal_edit_buf() {
        draw_goal_edit_overlay(f, size, buf);
    }
}

/// The inline goal editor (founder request): a centered box with the live
/// buffer + a block cursor, over whatever surface was showing.
fn draw_goal_edit_overlay(f: &mut Frame, area: Rect, buf: &str) {
    let popup = centered_rect(70, 30, area);
    f.render_widget(Clear, popup);
    let block = Block::default().borders(Borders::ALL).title("Set goal");
    let body = vec![
        Line::raw(""),
        Line::from(vec![
            Span::raw(buf.to_string()),
            Span::styled("\u{2588}", Style::default().add_modifier(Modifier::REVERSED)),
        ]),
        Line::raw(""),
        Line::styled(
            "\u{23ce} save    esc cancel",
            theme::ambient_style(),
        ),
    ];
    f.render_widget(
        Paragraph::new(body).wrap(Wrap { trim: false }).block(block),
        popup,
    );
}

// =====================================================================
// Header (Step 2): a REVERSED 1-line title bar — left is the wordmark (or,
// off home, the k9s-style breadcrumb design doc §6 calls for); right is the
// pulse (home) or the overlay's own context.
// =====================================================================

fn draw_header(f: &mut Frame, area: Rect, app: &App, ctx: &DrawContext) {
    let use_color = theme::color_allowed();
    let left = header_left_spans(app, ctx);
    let right = header_right_spans(app, ctx, use_color);
    let line = justify_line(left, right, area.width);
    f.render_widget(
        Paragraph::new(line).style(Style::default().add_modifier(Modifier::REVERSED)),
        area,
    );
}

fn header_left_spans(app: &App, ctx: &DrawContext) -> Vec<Span<'static>> {
    match app.focus() {
        Focus::Home => vec![Span::raw("murshid")],
        Focus::Mastery => vec![Span::raw("murshid \u{b7} mastery")],
        Focus::ConceptDetail(id) => {
            let name = ctx
                .taxonomy
                .iter()
                .find(|c| &c.slug == id)
                .map(|c| c.name.clone())
                .unwrap_or_else(|| id.clone());
            vec![Span::raw(format!(
                "murshid \u{b7} mastery \u{203a} {}",
                name
            ))]
        }
        Focus::Events => vec![Span::raw("murshid \u{b7} events")],
        Focus::Settings => vec![Span::raw("murshid \u{b7} settings")],
        Focus::History => vec![Span::raw("murshid \u{b7} history")],
        Focus::HistoryDetail(id) => {
            vec![Span::raw(format!("murshid \u{b7} history \u{203a} #{}", id))]
        }
    }
}

fn header_right_spans(app: &App, ctx: &DrawContext, use_color: bool) -> Vec<Span<'static>> {
    match app.focus() {
        Focus::Home => {
            let pulse = home_pulse_span(app, ctx, use_color);
            let context = home_pulse_context(ctx);
            vec![pulse, Span::raw(format!(" \u{b7} {}", context))]
        }
        Focus::Mastery => {
            let rows = mastery_rows_from_ctx(ctx);
            let active = rows
                .iter()
                .filter(|r| !r.zero_row && r.state == progress::ConceptState::Learning)
                .count();
            vec![Span::raw(format!(
                "{} concepts \u{b7} {} active",
                rows.len(),
                active
            ))]
        }
        Focus::ConceptDetail(id) => {
            let category = ctx
                .taxonomy
                .iter()
                .find(|c| &c.slug == id)
                .map(|c| c.category.as_str().to_string())
                .unwrap_or_else(|| "?".to_string());
            vec![Span::raw(category)]
        }
        Focus::Events => vec![Span::raw(format!(
            "session \u{b7} filter: {}",
            app.events_filter.label()
        ))],
        Focus::Settings => vec![Span::raw("session only \u{b7} not saved to config.toml")],
        Focus::History => {
            let rows = history_rows_from_ctx(ctx);
            vec![Span::raw(format!("{} cards", rows.len()))]
        }
        Focus::HistoryDetail(_) => Vec::new(),
    }
}

/// T15 mentor-state indicator: the header pulse's four faces, pure and
/// independent of ratatui — `home_pulse_span` below is the only thing that
/// turns this into a styled `Span`. `busy` (the offer-accept struggle
/// judge) always wins; `Reviewing` is the NORMAL sweep's live face and
/// takes the next precedence, ahead of the parse-gate hold.
#[derive(Debug, Clone, PartialEq, Eq)]
enum PulseState {
    Thinking,
    Reviewing(String),
    WaitingParse,
    Watching,
}

/// Pure precedence selection (design doc's degradation-safe glyph+word
/// posture, extended for the mentor-state indicator): `busy` >
/// `reviewing` > `parse_waiting` > `watching`.
fn select_pulse_state(busy: bool, reviewing_file: Option<String>, parse_waiting: bool) -> PulseState {
    if busy {
        PulseState::Thinking
    } else if let Some(file) = reviewing_file {
        PulseState::Reviewing(file)
    } else if parse_waiting {
        PulseState::WaitingParse
    } else {
        PulseState::Watching
    }
}

fn home_pulse_span(app: &App, ctx: &DrawContext, use_color: bool) -> Span<'static> {
    let busy = ctx.ws.busy.lock_poison_safe().is_some();
    let reviewing_file = match ctx.ws.review_state.lock_poison_safe().clone() {
        ReviewState::Reviewing { file } => Some(file),
        ReviewState::Watching => None,
    };
    let parse_waiting = !ctx.ws.parse_waiting.lock_poison_safe().is_empty();

    match select_pulse_state(busy, reviewing_file, parse_waiting) {
        PulseState::Thinking => {
            let glyph = theme::working_pulse_frame(app.tick);
            let color = if use_color { Color::Yellow } else { Color::Reset };
            Span::styled(format!("{} thinking", glyph), Style::default().fg(color))
        }
        PulseState::Reviewing(file) => {
            let glyph = theme::working_pulse_frame(app.tick);
            let color = if use_color {
                theme::REVIEWING_PULSE.color
            } else {
                Color::Reset
            };
            Span::styled(
                format!("{} reviewing {}", glyph, file),
                Style::default().fg(color),
            )
        }
        PulseState::WaitingParse => theme::WAITING_PULSE.span(use_color),
        PulseState::Watching => theme::LIVE_PULSE.span(use_color),
    }
}

/// The header's one-word "what's murshid looking at" hint. There is no
/// dedicated "file last judged" field on `WatchSession` (adding one would be
/// a THIRD engine change beyond the two flagged needs in the design doc's
/// build plan) — this approximates it from the pending-files set the sweep
/// already maintains, falling back to a plain state word.
fn home_pulse_context(ctx: &DrawContext) -> String {
    if ctx.ws.busy.lock_poison_safe().is_some() {
        pending_files_hint(ctx).unwrap_or_else(|| "working".to_string())
    } else if !ctx.ws.parse_waiting.lock_poison_safe().is_empty() {
        "parse".to_string()
    } else {
        pending_files_hint(ctx).unwrap_or_else(|| "idle".to_string())
    }
}

fn pending_files_hint(ctx: &DrawContext) -> Option<String> {
    let files = ctx.ws.pending_files.lock_poison_safe();
    let mut v: Vec<String> = files.iter().map(|p| p.display().to_string()).collect();
    v.sort();
    v.into_iter().next()
}

/// Pure: `left`/`right` on one `width`-wide reversed row, one space of
/// margin on each side, the remainder split as padding between them.
fn justify_line(left: Vec<Span<'static>>, right: Vec<Span<'static>>, width: u16) -> Line<'static> {
    let left_len: usize = left.iter().map(|s| s.content.chars().count()).sum();
    let right_len: usize = right.iter().map(|s| s.content.chars().count()).sum();
    let used = left_len + right_len + 2;
    let pad = (width as usize).saturating_sub(used).max(1);
    let mut spans = vec![Span::raw(" ")];
    spans.extend(left);
    spans.push(Span::raw(" ".repeat(pad)));
    spans.extend(right);
    spans.push(Span::raw(" "));
    Line::from(spans)
}

// =====================================================================
// Ambient band (Step 2): goal · next nudge · judge — one dim line.
// =====================================================================

fn draw_ambient_band(f: &mut Frame, area: Rect, ctx: &DrawContext) {
    let use_color = theme::color_allowed();
    let goal_text = crate::goal_text_now(ctx.project_root);
    let goal_part = if goal_text.trim().is_empty() {
        "(none set)".to_string()
    } else {
        // Bound the goal so the ambient line (goal · next nudge · judge)
        // stays on one row instead of pushing the rest off the right edge.
        // The ambient band is a single non-wrapping status row (goal · next
        // nudge · judge), so the goal still needs a cap — but a less aggressive
        // one than before (founder: text was truncating too eagerly). The full
        // goal is always shown untruncated on the empty surface's goal line.
        const GOAL_MAX: usize = 52;
        clip(&goal_text, GOAL_MAX)
    };

    let nudge = {
        let b = ctx.ws.bucket.lock_poison_safe();
        let now = std::time::SystemTime::now();
        next_nudge_span(b.is_ready_at(now), b.time_until_ready_at(now), use_color)
    };

    let mut spans = vec![
        Span::styled(format!("goal: {}", goal_part), theme::ambient_style()),
        Span::styled("  \u{b7}  ", theme::ambient_style()),
        nudge,
        Span::styled("  \u{b7}  ", theme::ambient_style()),
    ];
    match ctx.mode {
        judge::JudgeMode::Degraded { .. } => spans.push(theme::DEGRADED_JUDGE.span(use_color)),
        judge::JudgeMode::Active => spans.push(Span::styled("judge live", theme::ambient_style())),
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// T15 "next nudge" indicator — replaces the old budget gauge (founder found
/// "budget" confusing, and the 7-cell bar misleading at burst 1, where it
/// read as all-full or all-empty). Plain language: "next nudge: ready" when a
/// proactive card may fire now, else "next nudge: ~Nm" while the push bucket
/// refills. Pure over (ready, eta).
fn next_nudge_span(ready: bool, eta: Option<std::time::Duration>, use_color: bool) -> Span<'static> {
    if ready {
        let color = if use_color { Color::Green } else { Color::Reset };
        Span::styled("next nudge: ready", Style::default().fg(color))
    } else {
        Span::styled(
            format!("next nudge: {}", format_nudge_eta(eta)),
            theme::ambient_style(),
        )
    }
}

/// Compact wait-until-next-nudge: "<1m" under a minute, else "~Nm" (rounded up).
fn format_nudge_eta(eta: Option<std::time::Duration>) -> String {
    match eta {
        None => "ready".to_string(),
        Some(d) => {
            let secs = d.as_secs();
            if secs < 60 {
                "<1m".to_string()
            } else {
                format!("~{}m", secs.div_ceil(60))
            }
        }
    }
}

// =====================================================================
// Home surface (Steps 1/3/4/5): the five faces, selected by state — never
// stacked lines in a dashboard.
// =====================================================================

fn draw_home_surface(f: &mut Frame, area: Rect, app: &App, ctx: &DrawContext) {
    let use_color = theme::color_allowed();

    // Step 5: the response-acknowledgment beat pre-empts everything else
    // for its short, fixed duration.
    if let Some(card) = app.acked_card() {
        draw_hero_card(f, area, card, ctx, use_color, true);
        return;
    }

    // Step 4: an offer never occupies the card slot, but it does pre-empt
    // the card's visual spotlight while it's live (design doc §5.4).
    let maybe_offer: Option<PendingOffer> = ctx.ws.pending_offer.lock_poison_safe().clone();
    if let Some(po) = maybe_offer {
        draw_offer_callout(f, area, &po, use_color);
        return;
    }

    let maybe_card: Option<PendingCard> = ctx.ws.pending_card.lock_poison_safe().clone();
    if let Some(pc) = maybe_card {
        draw_hero_card(f, area, &pc, ctx, use_color, false);
        return;
    }

    // Step 3: busy/parse-wait are only their own face when there's no
    // live card to show instead — the header pulse already carries
    // "still working" independent of which face the surface shows.
    let busy_label = ctx.ws.busy.lock_poison_safe().clone();
    if let Some(label) = busy_label {
        draw_centered_message(f, area, working_state_lines(app.tick, &label, use_color));
        return;
    }

    let waiting = ctx.ws.parse_waiting.lock_poison_safe().clone();
    if !waiting.is_empty() {
        draw_centered_message(f, area, waiting_state_lines(&waiting, use_color));
        return;
    }

    draw_centered_message(f, area, empty_state_lines(ctx, use_color));
}

/// Step 1: the card as a centered, bordered, category-colored `Block` (the
/// hero) — title bar `{glyph+word} · {concept}` on the left, `rung n/3` on
/// the right, with the queue presence line just beneath it.
fn draw_hero_card(
    f: &mut Frame,
    area: Rect,
    pc: &PendingCard,
    ctx: &DrawContext,
    use_color: bool,
    acked: bool,
) {
    let category = pack::Category::parse(&pc.category);
    let role = theme::category_style(&category);
    let border_color = if acked {
        if use_color { Color::Green } else { Color::Reset }
    } else if use_color {
        role.color
    } else {
        Color::Reset
    };

    let title_left = Line::from(vec![
        role.span(use_color),
        Span::raw(" \u{b7} "),
        Span::raw(pc.concept_name.clone()),
    ]);
    let title_right = Line::raw(format!("rung {}/3", rung_number(pc.rung)));

    let body = render_card_block_with_color(pc, ctx.surface, use_color);
    let queue_line = home_queue_presence_line(ctx)
        .map(|s| Line::styled(s, theme::ambient_style()).alignment(Alignment::Center));

    draw_surface_block(f, area, title_left, Some(title_right), border_color, body, queue_line);
}

fn rung_number(rung: ladder::Rung) -> u8 {
    match rung {
        ladder::Rung::R0 => 0,
        ladder::Rung::R1 => 1,
        ladder::Rung::R2 => 2,
        ladder::Rung::R3 => 3,
    }
}

fn home_queue_presence_line(ctx: &DrawContext) -> Option<String> {
    let mut q = ctx.ws.queue_state.lock_poison_safe().clone();
    let cluster = ctx.ws.goal_cluster_dirs.lock_poison_safe().clone();
    let goal_text = crate::goal_text_now(ctx.project_root);
    queue::sort_queue(&mut q, &cluster, &goal_text);
    queue::presence_indicator(q.len())
}

/// Step 1 (`view::render_card_block`, design doc §7): the card's interior
/// content — gutter anchor, why, labeled Rule + `→` doc line, multi-site
/// line — as styled `Line`s. Rung-aware (design doc §3.8): R0/R1 fold to a
/// recall question / bare nudge; R3 unfolds the worked example in place of
/// the Rule section (I19). Does NOT include the title bar — that's a
/// `Block` title (see `draw_hero_card`), not body content.
pub fn render_card_block(pc: &PendingCard, surface: &pack::SurfaceConfig) -> Vec<Line<'static>> {
    render_card_block_with_color(pc, surface, theme::color_allowed())
}

pub fn render_card_block_with_color(
    pc: &PendingCard,
    surface: &pack::SurfaceConfig,
    use_color: bool,
) -> Vec<Line<'static>> {
    let card = &pc.card;
    let mut lines = vec![Line::raw(format!("{}:{}", card.file, card.line))];

    match pc.rung {
        ladder::Rung::R0 => {
            lines.push(gutter_line(&card.grounding_quote));
            lines.push(Line::raw(""));
            lines.push(Line::raw(
                "how would you write this differently? (e for a hint, t for the fix)",
            ));
        }
        ladder::Rung::R1 => {
            lines.push(Line::raw(""));
            lines.push(Line::raw("(nudge \u{2014} e for more, t for the fix)"));
        }
        ladder::Rung::R2 | ladder::Rung::R3 => {
            lines.push(gutter_line(&card.grounding_quote));
            lines.push(Line::raw(""));
            lines.push(Line::raw(card.why.clone()));
            lines.push(Line::raw(""));
            if pc.rung == ladder::Rung::R3 {
                lines.extend(worked_example_lines(card, &surface.comment_token, use_color));
            } else {
                lines.push(Line::from(vec![
                    Span::raw("Rule  "),
                    Span::raw(card.rule.clone()),
                ]));
                lines.push(Line::from(vec![
                    Span::raw("      \u{2192} "),
                    Span::raw(card.doc_ref.clone()),
                ]));
            }
            let total_sites = 1 + card.additional_anchors.len() + card.overflow_site_count;
            if total_sites > 1 {
                lines.push(Line::raw(""));
                let anchors: Vec<String> = card
                    .additional_anchors
                    .iter()
                    .map(|(f, l)| format!("{}:{}", f, l))
                    .collect();
                lines.push(Line::styled(
                    format!(
                        "\u{2191} this pattern also appears at {}",
                        anchors.join(", ")
                    ),
                    theme::ambient_style(),
                ));
            }
        }
    }
    lines
}

fn gutter_line(quote: &str) -> Line<'static> {
    Line::from(vec![Span::raw("\u{2502} "), Span::raw(quote.to_string())])
}

fn worked_example_lines(
    card: &crate::card::Card,
    comment_token: &str,
    use_color: bool,
) -> Vec<Line<'static>> {
    let text = ladder::render_worked_example(&card.worked_diff, &card.why, comment_token);
    let mut lines = vec![Line::raw("worked example")];
    for l in text.lines() {
        let color = if l.starts_with('+') {
            Color::Green
        } else if l.starts_with('-') {
            Color::Red
        } else {
            Color::Reset
        };
        let color = if use_color { color } else { Color::Reset };
        lines.push(Line::from(Span::styled(l.to_string(), Style::default().fg(color))));
    }
    lines
}

/// Step 4: the struggle offer as a centered `⚑` callout — never the card's
/// slot, always a distinct "attention" accent (design doc §3.3/§4.1).
fn draw_offer_callout(f: &mut Frame, area: Rect, po: &PendingOffer, use_color: bool) {
    let role = theme::ATTENTION;
    let title_left = Line::from(role.span(use_color));
    let border_color = if use_color { role.color } else { Color::Reset };
    let body = vec![Line::raw(""), Line::raw(offer_evidence_line(po)), Line::raw("")];
    draw_surface_block(f, area, title_left, None, border_color, body, None);
}

/// I11 "show the evidence": reconstructs an `offer::Evidence` from the
/// `PendingOffer`'s own key + elapsed time since it fired, then reuses the
/// already-tested `offer::offer_line` — no new offer-evidence storage
/// needed, everything here is already on `PendingOffer`.
fn offer_evidence_line(po: &PendingOffer) -> String {
    let minutes = std::time::SystemTime::now()
        .duration_since(po.fired_at)
        .map(|d| d.as_secs() / 60)
        .unwrap_or(0);
    let evidence = match po.key.0 {
        "error-streak" => offer::Evidence::ErrorStreak {
            code: po.key.1.clone(),
            minutes,
        },
        _ => offer::Evidence::HelpComment {
            snippet: po.key.1.clone(),
        },
    };
    offer::offer_line(&evidence)
}

/// Step 3: the "asking the model…" working face (design doc §3.4) — the
/// animated pulse glyph plus a reassurance that the UI stays live.
fn working_state_lines(tick: u64, label: &str, use_color: bool) -> Vec<Line<'static>> {
    let glyph = theme::working_pulse_frame(tick);
    let color = if use_color { Color::Yellow } else { Color::Reset };
    vec![
        Line::from(Span::styled(
            format!("{}  {}", glyph, label),
            Style::default().fg(color),
        )),
        Line::raw(""),
        Line::styled("(this runs in the background \u{2014}", theme::ambient_style()),
        Line::styled("the UI stays live, press q to quit)", theme::ambient_style()),
    ]
}

/// Step 3: the parse-gate hold (design doc §3.5) — a first-class, clearly
/// benign face, not a buried line. Reuses [`parse_wait_line`]'s exact
/// summary sentence (kept, with its pinned tests, as the content model the
/// build plan calls for) and adds the per-file bulleted list on top.
fn waiting_state_lines(waiting: &[String], use_color: bool) -> Vec<Line<'static>> {
    let color = if use_color {
        theme::WAITING_PULSE.color
    } else {
        Color::Reset
    };
    let mut lines = vec![
        Line::from(Span::styled(
            format!("{}  waiting on a clean parse", theme::WAITING_PULSE.glyph),
            Style::default().fg(color),
        )),
        Line::raw(""),
    ];
    if let Some(summary) = parse_wait_line(waiting) {
        lines.push(Line::styled(summary, theme::ambient_style()));
        lines.push(Line::raw(""));
    }
    for f in waiting {
        lines.push(Line::styled(format!("\u{b7} {}", f), theme::ambient_style()));
    }
    lines
}

/// T15 dogfood fix (2026-07-05, "silence is ambiguous"): the C12 parse
/// gate's status line. `None` when nothing is held; otherwise a one-liner
/// naming the file(s) that don't parse yet. Kept verbatim (same content,
/// same tests) as the pre-redesign dashboard's line — the redesign only
/// promotes it from a buried line to a first-class surface face (Step 3).
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

/// Step 1 (§3.2): the caught-up empty state — calm, labeled silence, never
/// a deadpan "(no card on screen)".
///
/// T15 mentor-state indicator (founder ask): a `last_review` whose outcome
/// is NOT `Suggested` (a `Suggested` review means a card is on screen or
/// queued — this empty surface wouldn't even be showing) gets one calm,
/// ambient one-liner appended — "reviewed and found nothing" is now an
/// explicit, legible statement instead of silence the user could mistake
/// for murshid being idle/broken.
fn empty_state_lines(ctx: &DrawContext, use_color: bool) -> Vec<Line<'static>> {
    let check_color = if use_color { Color::Green } else { Color::Reset };
    let mut lines = vec![
        Line::from(Span::styled("\u{2713}", Style::default().fg(check_color))),
        Line::raw(""),
        Line::raw("You're all caught up."),
        Line::raw(""),
        Line::styled(
            "murshid is watching. Keep coding \u{2014} I'll speak",
            theme::ambient_style(),
        ),
        Line::styled(
            "up when there's something worth a look.",
            theme::ambient_style(),
        ),
        Line::raw(""),
        Line::styled(caught_up_status_line(ctx), theme::ambient_style()),
    ];
    if let Some(outcome_line) = last_review_outcome_line(ctx) {
        lines.push(Line::styled(outcome_line, theme::ambient_style()));
    }
    lines.push(Line::raw(""));
    lines.push(goal_display_line(ctx));
    // Founder 2026-07-05: a compact recent-activity tail so the idle home
    // surface shows *something is happening* — the activity log the redesign
    // otherwise renders nowhere on home. Last few notices, dim + width-capped;
    // the full scrollable log is still the `E` events view.
    let recent: Vec<String> = ctx.ws.activity_log.lock_poison_safe().clone();
    if !recent.is_empty() {
        lines.push(Line::raw(""));
        lines.push(Line::styled("recent", theme::ambient_style()));
        for msg in recent.iter().rev().take(3).rev() {
            // No aggressive clip: the surface now wraps (draw_centered_message),
            // so a full notice reads on 1-2 lines instead of being cut at 54.
            // A generous cap only bounds a pathological entry.
            lines.push(Line::styled(clip(msg, 160), theme::ambient_style()));
        }
    }
    lines
}

/// Width-cap a status string with an ellipsis, so a long notice/goal can't
/// spill a bottom line off the right edge.
fn clip(s: &str, max: usize) -> String {
    if s.chars().count() > max {
        let kept: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{}\u{2026}", kept)
    } else {
        s.to_string()
    }
}

/// Reads `ctx.ws.last_review` and (when its result isn't `Suggested`) the
/// degraded-mode reason off `ctx.mode`, then renders [`review_outcome_line`].
/// Kept separate from the pure formatter so the formatter itself stays
/// unit-testable without a `DrawContext`.
fn last_review_outcome_line(ctx: &DrawContext) -> Option<String> {
    let last = ctx.ws.last_review.lock_poison_safe().clone()?;
    if last.result == ReviewResult::Suggested {
        return None;
    }
    let degraded_reason = match ctx.mode {
        judge::JudgeMode::Degraded { reason } => Some(reason.as_str()),
        judge::JudgeMode::Active => None,
    };
    let now_epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    Some(review_outcome_line(&last, degraded_reason, now_epoch))
}

/// T15 mentor-state indicator: pure — given a `LastReview` (whose result is
/// `NothingToFlag`/`CouldNotReview`; `Suggested` is never rendered here —
/// see `last_review_outcome_line`), a reason to name for `CouldNotReview`
/// (the degraded-mode reason when there is one), and "now", the calm
/// ambient one-liner the idle surface shows instead of silence.
fn review_outcome_line(last: &LastReview, degraded_reason: Option<&str>, now_epoch: i64) -> String {
    let then_epoch = last
        .at
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(now_epoch);
    let age = relative_age(now_epoch, then_epoch);
    match last.result {
        ReviewResult::NothingToFlag => {
            format!(
                "looked at {} {} \u{2014} nothing worth flagging",
                last.file, age
            )
        }
        ReviewResult::CouldNotReview => {
            let reason = degraded_reason.unwrap_or("couldn't reach the model");
            format!("couldn't review {} {} \u{2014} {}", last.file, age, reason)
        }
        // Never actually reached (callers guard on this), but a total match
        // keeps the function honest about what it can be asked to render.
        ReviewResult::Suggested => String::new(),
    }
}

/// The goal shown prominently on the calm empty surface (founder request:
/// "easy to see"). The value renders in normal weight (not dim) so it stands
/// out, with a dim `(g to change)` affordance; no goal reads as an invitation.
fn goal_display_line(ctx: &DrawContext) -> Line<'static> {
    let goal = crate::goal_text_now(ctx.project_root);
    if goal.trim().is_empty() {
        Line::styled(
            "no goal set \u{2014} press g to set one",
            theme::ambient_style(),
        )
    } else {
        Line::from(vec![
            Span::styled("goal: ", theme::ambient_style()),
            Span::raw(goal),
            Span::styled("   (G to change)", theme::ambient_style()),
        ])
    }
}

fn caught_up_status_line(ctx: &DrawContext) -> String {
    let Some(conn) = ctx.conn else {
        return "murshid is ready \u{2014} no database connection yet.".to_string();
    };
    let sid = ctx.ws.session_mgr.lock_poison_safe().session_id.clone();
    let shown = db::bookend_shown_count(conn, &sid).unwrap_or(0);
    let events = db::get_events_for_session(conn, &sid).unwrap_or_default();
    let last_shown_ts = events
        .iter()
        .rev()
        .find(|e| e.kind == "card_shown")
        .and_then(|e| e.ts.clone());
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let age = last_shown_ts
        .as_deref()
        .and_then(parse_sqlite_ts_epoch_secs)
        .map(|then| relative_age(now, then));
    match age {
        Some(age_str) => format!("last thought: {} \u{b7} {} shown this session", age_str, shown),
        None => format!("{} shown this session", shown),
    }
}

// --- Generic surface-block/centered-message helpers, shared by the hero
// card and the offer callout / working / waiting / empty faces. ---

/// The reading column's max width (design doc §4.2: "~62 cols wide,
/// capped").
const READING_COLUMN_WIDTH: u16 = 64;

fn centered_columns(max_width: u16, area: Rect) -> Rect {
    let width = area.width.min(max_width);
    let margin = area.width.saturating_sub(width) / 2;
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(margin),
            Constraint::Length(width),
            Constraint::Min(0),
        ])
        .split(area)[1]
}

/// The hero card / offer callout's shared frame: a centered, padded,
/// colored-border `Block` holding `body`, with an optional dim `below` line
/// (the queue presence indicator) underneath.
fn draw_surface_block(
    f: &mut Frame,
    area: Rect,
    title_left: Line<'static>,
    title_right: Option<Line<'static>>,
    border_color: Color,
    body: Vec<Line<'static>>,
    below: Option<Line<'static>>,
) {
    // Founder 2026-07-06: the card body WRAPS (Paragraph::wrap below), so a
    // one-row-per-line height (body.len()) clips the bottom of the card — the
    // wrapped `why`/`rule`/worked-example rows fall outside the block and the
    // "good part" is lost. Size to the ESTIMATED wrapped height instead. Inner
    // text width = column minus 2 borders + 2*2 horizontal padding. The +1
    // slack per wrapping line covers word-wrap breaking on word boundaries
    // (which can use one more row than a naive char/width division).
    let inner_width = READING_COLUMN_WIDTH.saturating_sub(6).max(1) as usize;
    let wrapped_rows: u16 = body
        .iter()
        .map(|line| {
            let len: usize = line.spans.iter().map(|s| s.content.chars().count()).sum();
            if len <= inner_width {
                1
            } else {
                (len.div_ceil(inner_width) + 1) as u16
            }
        })
        .sum();
    let content_height = (wrapped_rows + 2).max(3);
    let card_height = content_height.min(area.height.saturating_sub(1).max(3));
    let remaining = area.height.saturating_sub(card_height + 1);
    let top_margin = remaining / 3;
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(top_margin),
            Constraint::Length(card_height),
            Constraint::Length(1),
            Constraint::Min(0),
        ])
        .split(area);

    let card_area = centered_columns(READING_COLUMN_WIDTH, rows[1]);
    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color))
        .padding(Padding::horizontal(2))
        .title_top(title_left);
    if let Some(right) = title_right {
        block = block.title_top(right.right_aligned());
    }
    f.render_widget(
        Paragraph::new(body).wrap(Wrap { trim: false }).block(block),
        card_area,
    );

    if let Some(line) = below {
        let below_area = centered_columns(READING_COLUMN_WIDTH, rows[2]);
        f.render_widget(Paragraph::new(line), below_area);
    }
}

/// The borderless working/waiting/empty faces' shared frame: `lines`
/// vertically AND horizontally centered in `area`.
fn draw_centered_message(f: &mut Frame, area: Rect, lines: Vec<Line<'static>>) {
    // Founder 2026-07-06: WRAP so long lines (notices, prose) never hard-
    // truncate at the column edge, and give the content the full lower area
    // (`Min(0)`) so wrapped rows are never bottom-clipped. Roughly upper-
    // centered via a proportional top margin — an exact line-count height was
    // wrong the moment any line wrapped to more than one row.
    let top_margin = (area.height / 5).min(area.height.saturating_sub(1));
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(top_margin), Constraint::Min(0)])
        .split(area);
    let cols = centered_columns(70, rows[1]);
    f.render_widget(
        Paragraph::new(lines)
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: false }),
        cols,
    );
}

// --- Pure date-age helpers (no chrono, C10 stdlib-only) ---

/// Days since the Unix epoch for a UTC civil `(year, month, day)` — Howard
/// Hinnant's `days_from_civil` algorithm.
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let mp = (m as i64 + 9) % 12; // [0,11]: Mar=0 .. Feb=11
    let doy = (153 * mp + 2) / 5 + d as i64 - 1; // [0,365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146097 + doe - 719468
}

/// Parses a SQLite `CURRENT_TIMESTAMP` string (`"YYYY-MM-DD HH:MM:SS"`,
/// always UTC) into Unix epoch seconds. `None` on any malformed input — a
/// rendering-only helper never trusts stored TEXT blindly.
fn parse_sqlite_ts_epoch_secs(ts: &str) -> Option<i64> {
    let (date, time) = ts.split_once(' ')?;
    let mut d = date.split('-');
    let y: i64 = d.next()?.parse().ok()?;
    let m: u32 = d.next()?.parse().ok()?;
    let day: u32 = d.next()?.parse().ok()?;
    let mut t = time.split(':');
    let h: i64 = t.next()?.parse().ok()?;
    let mi: i64 = t.next()?.parse().ok()?;
    let s: i64 = t.next()?.parse().ok()?;
    Some(days_from_civil(y, m, day) * 86400 + h * 3600 + mi * 60 + s)
}

fn relative_age(now_epoch: i64, then_epoch: i64) -> String {
    let secs = (now_epoch - then_epoch).max(0) as u64;
    match secs {
        s if s < 60 => "just now".to_string(),
        s if s < 3600 => format!("{}m ago", s / 60),
        s if s < 86400 => format!("{}h ago", s / 3600),
        s => format!("{}d ago", s / 86400),
    }
}

fn relative_age_from_secs(age_secs: Option<u64>) -> String {
    match age_secs {
        None => "not yet seen".to_string(),
        Some(s) if s < 60 => "just now".to_string(),
        Some(s) if s < 3600 => format!("{}m ago", s / 60),
        Some(s) if s < 86400 => format!("{}h ago", s / 3600),
        Some(s) => format!("{}d ago", s / 86400),
    }
}

// =====================================================================
// Mastery overlay (Step 6): `progress::build_rows` as real span-built
// `█/░` bars, category group headers, colored by `ConceptState`.
// =====================================================================

/// Shared by the mastery view, the header's "N concepts · M active", and
/// `⏎`'s concept-detail lookup (`mod.rs`'s key handler).
pub(crate) fn mastery_rows(
    ws: &WatchSession,
    conn: &rusqlite::Connection,
    taxonomy: &[pack::TaxonomyConcept],
) -> Vec<progress::ProgressRow> {
    let memory_rows = db::list_concept_memory(conn).unwrap_or_default();
    let throttled = ws.throttled_categories.lock_poison_safe().clone();
    let now_epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    progress::build_rows(&memory_rows, taxonomy, &throttled, now_epoch)
}

fn mastery_rows_from_ctx(ctx: &DrawContext) -> Vec<progress::ProgressRow> {
    ctx.conn
        .map(|conn| mastery_rows(ctx.ws, conn, ctx.taxonomy))
        .unwrap_or_default()
}

const MASTERY_BAR_WIDTH: usize = 20;

/// Pure: the meter's `█`/`░` bar, colored by `row.state` (design doc §3.6/
/// §4.3) — a zero-row (never-encountered) concept renders as a fully-dim
/// bar, matching `progress::render_row`'s "(not yet encountered)".
fn mastery_bar(row: &progress::ProgressRow, use_color: bool) -> Span<'static> {
    if row.zero_row {
        return Span::styled(
            "\u{2591}".repeat(MASTERY_BAR_WIDTH),
            theme::ambient_style(),
        );
    }
    let filled = ((row.p_mastery.clamp(0.0, 1.0) * MASTERY_BAR_WIDTH as f64).round() as usize)
        .min(MASTERY_BAR_WIDTH);
    let role = theme::state_style(&row.state);
    let color = if use_color { role.color } else { Color::Reset };
    let bar = format!(
        "{}{}",
        "\u{2588}".repeat(filled),
        "\u{2591}".repeat(MASTERY_BAR_WIDTH - filled)
    );
    Span::styled(bar, Style::default().fg(color))
}

fn draw_mastery(f: &mut Frame, area: Rect, app: &App, ctx: &DrawContext) {
    let use_color = theme::color_allowed();
    let rows = mastery_rows_from_ctx(ctx);
    if rows.is_empty() {
        f.render_widget(
            Paragraph::new("(no concepts in the taxonomy)").style(theme::ambient_style()),
            area,
        );
        return;
    }
    let selected = app.mastery_selected.min(rows.len() - 1);

    let mut items: Vec<ListItem> = Vec::new();
    let mut list_index_of_selected = 0usize;
    let mut current_category: Option<String> = None;
    for (i, row) in rows.iter().enumerate() {
        if current_category.as_deref() != Some(row.category.as_str()) {
            items.push(ListItem::new(Line::styled(
                row.category.to_uppercase(),
                theme::ambient_style(),
            )));
            current_category = Some(row.category.clone());
        }
        if i == selected {
            list_index_of_selected = items.len();
        }
        items.push(ListItem::new(mastery_row_line(row, i == selected, use_color)));
    }

    let mut state = ListState::default();
    state.select(Some(list_index_of_selected));
    let list = List::new(items).highlight_style(theme::focus_style());
    f.render_stateful_widget(list, area, &mut state);
}

fn mastery_row_line(row: &progress::ProgressRow, selected: bool, use_color: bool) -> Line<'static> {
    let marker = if selected { "\u{203a} " } else { "  " };
    let cat_role = theme::category_style(&pack::Category::parse(&row.category));
    let bar = mastery_bar(row, use_color);
    let pct = (row.p_mastery.clamp(0.0, 1.0) * 100.0).round() as u32;
    let help = if row.zero_row {
        "\u{2013}".to_string()
    } else {
        row.help_level.to_string()
    };
    let mut spans = vec![
        Span::raw(marker),
        cat_role.span(use_color),
        Span::raw(" "),
        Span::raw(format!("{:<28}", row.name)),
        Span::raw(" "),
        bar,
    ];
    if row.zero_row {
        spans.push(Span::raw(format!("   {:>3}  help {}  ", "\u{2014}", help)));
        spans.push(Span::styled("not yet seen", theme::ambient_style()));
    } else {
        spans.push(Span::raw(format!(
            "  {:>3}%  help {}  {}  ",
            pct,
            help,
            relative_age_from_secs(row.last_encounter_age_secs)
        )));
        spans.push(theme::state_style(&row.state).span(use_color));
    }
    Line::from(spans)
}

// =====================================================================
// Concept detail (Step 7): the mastery meter's drill-down — a `Sparkline`
// trend + recent encounters, fed by `db::get_concept_events` (flagged need
// #2).
// =====================================================================

fn draw_concept_detail(f: &mut Frame, area: Rect, concept_id: &str, ctx: &DrawContext) {
    let use_color = theme::color_allowed();
    let Some(conn) = ctx.conn else {
        f.render_widget(
            Paragraph::new("(no database connection)").style(theme::ambient_style()),
            area,
        );
        return;
    };
    let rows = mastery_rows(ctx.ws, conn, ctx.taxonomy);
    let Some(row) = rows.iter().find(|r| r.concept_id == concept_id) else {
        f.render_widget(
            Paragraph::new("(concept not found)").style(theme::ambient_style()),
            area,
        );
        return;
    };
    let events = db::get_concept_events(conn, concept_id).unwrap_or_default();
    let heights = grade_history_heights(&events);
    let recent = recent_lines_for_concept(&events);
    let recent_height = recent.len().max(1) as u16;

    let cols = centered_columns(76, area);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // mastery line
            Constraint::Length(1), // blank
            Constraint::Length(1), // trend label
            Constraint::Length(3), // sparkline
            Constraint::Length(1), // blank
            Constraint::Length(1), // "recent" header
            Constraint::Length(recent_height),
            Constraint::Min(0),
        ])
        .split(cols);

    let pct = (row.p_mastery.clamp(0.0, 1.0) * 100.0).round() as u32;
    let mut mastery_spans = vec![Span::raw("mastery   "), mastery_bar(row, use_color)];
    mastery_spans.push(Span::raw(format!(
        "  {:>3}%   help level {}   last seen {}",
        pct,
        row.help_level,
        relative_age_from_secs(row.last_encounter_age_secs)
    )));
    f.render_widget(Paragraph::new(Line::from(mastery_spans)), chunks[0]);

    let trend_word = trend_description(&heights);
    let trend_label = if heights.is_empty() {
        "trend     (no graded encounters yet)".to_string()
    } else {
        format!("trend     ({} encounters \u{2014} {})", heights.len(), trend_word)
    };
    f.render_widget(
        Paragraph::new(Line::styled(trend_label, theme::ambient_style())),
        chunks[2],
    );
    let sparkline_color = if use_color { Color::Cyan } else { Color::Reset };
    let sparkline = Sparkline::default()
        .data(&heights)
        .style(Style::default().fg(sparkline_color));
    f.render_widget(sparkline, chunks[3]);

    f.render_widget(
        Paragraph::new(Line::styled("recent", theme::ambient_style())),
        chunks[5],
    );
    if recent.is_empty() {
        f.render_widget(
            Paragraph::new(Line::styled("(no history yet)", theme::ambient_style())),
            chunks[6],
        );
    } else {
        f.render_widget(Paragraph::new(recent), chunks[6]);
    }
}

/// Pure: `encounter` events' grades as `Sparkline` bar heights (fail=1,
/// hard=2, pass=3) — non-`encounter` events and unparseable grades are
/// skipped, never a panic.
fn grade_history_heights(events: &[db::EventRecord]) -> Vec<u64> {
    events
        .iter()
        .filter(|e| e.kind == "encounter")
        .filter_map(|e| event_payload_field(&e.payload_json, "grade"))
        .filter_map(|g| bkt::Grade::parse(&g))
        .map(|g| match g {
            bkt::Grade::Fail => 1,
            bkt::Grade::Hard => 2,
            bkt::Grade::Pass => 3,
        })
        .collect()
}

/// Pure: a one-word trend summary from the grade-height sequence — "am I
/// actually learning?" (I25) at a glance, backing the `Sparkline`'s glyphs.
fn trend_description(heights: &[u64]) -> &'static str {
    match (heights.first(), heights.last()) {
        (Some(first), Some(last)) if last > first => "climbing",
        (Some(first), Some(last)) if last < first => "dipping",
        (Some(_), Some(_)) => "steady",
        _ => "no data yet",
    }
}

/// The concept-detail "recent" list — the most recent [`RECENT_EVENTS_MAX`]
/// events for this concept, newest first, each a `{age} {word} {detail}`
/// line built from the same event-role/payload-summary helpers the events
/// overlay (Step 8) uses.
const RECENT_EVENTS_MAX: usize = 5;

fn recent_lines_for_concept(events: &[db::EventRecord]) -> Vec<Line<'static>> {
    let use_color = theme::color_allowed();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    events
        .iter()
        .rev()
        .take(RECENT_EVENTS_MAX)
        .map(|e| {
            let age = e
                .ts
                .as_deref()
                .and_then(parse_sqlite_ts_epoch_secs)
                .map(|then| relative_age(now, then))
                .unwrap_or_else(|| "\u{2014}".to_string());
            let role = event_role(&e.kind, &e.payload_json);
            let word = if role.word.is_empty() {
                e.kind.clone()
            } else {
                role.word.to_string()
            };
            let color = if use_color { role.color } else { Color::Reset };
            let summary = summarize_event_payload(&e.kind, &e.payload_json);
            Line::from(vec![
                Span::raw(format!("{:<10}", age)),
                Span::styled(format!("{:<10}", word), Style::default().fg(color)),
                Span::raw(summary),
            ])
        })
        .collect()
}

// =====================================================================
// Events overlay (Step 8): `judge_declined` vs `judge_drop` vs `card_shown`
// rendered distinctly (the "silence is ambiguous" fix made visible), a
// human payload summary instead of raw JSON.
// =====================================================================

fn event_payload_field(payload_json: &str, field: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(payload_json).ok()?;
    v.get(field)?.as_str().map(|s| s.to_string())
}

/// Pure: maps an event `kind` (+ its `verb`, when the kind carries one) to
/// a `(glyph, color, word)` role (design doc §3.7/§4.1). Unknown kinds
/// degrade to a plain ambient marker — `word` empty signals "use the raw
/// `kind` string instead" to the caller.
fn event_role(kind: &str, payload_json: &str) -> theme::Role {
    let verb = event_payload_field(payload_json, "verb");
    let ambient = |word: &'static str| theme::Role {
        glyph: "\u{b7}",
        ascii: ".",
        color: Color::DarkGray,
        word,
    };
    match kind {
        "card_shown" => theme::Role {
            glyph: "\u{2726}",
            ascii: "*",
            color: Color::Cyan,
            word: "shown",
        },
        "card_response" => match verb.as_deref() {
            Some("applied") => theme::SUCCESS,
            Some("got_it") => theme::Role {
                glyph: "\u{2713}",
                ascii: "v",
                color: Color::Green,
                word: "got it",
            },
            Some("escalated") => ambient("escalated"),
            Some("not_now") => ambient("snoozed"),
            Some("not_useful") => theme::DECLINED,
            _ => ambient("responded"),
        },
        "prompt_response" => match verb.as_deref() {
            Some("accepted") => theme::SUCCESS,
            Some("declined") => theme::DECLINED,
            _ => ambient("prompt"),
        },
        "judge_declined" => theme::DECLINED,
        "judge_drop" => theme::DROPPED,
        "prompt_offered" => ambient("offered"),
        "throttle_change" => ambient("throttled"),
        "goal_inferred" => theme::Role {
            glyph: "\u{25c6}",
            ascii: "+",
            color: Color::DarkGray,
            word: "goal",
        },
        "encounter" => ambient("encounter"),
        "fade" => theme::SUCCESS,
        _ => theme::Role {
            glyph: "\u{b7}",
            ascii: ".",
            color: Color::DarkGray,
            word: "",
        },
    }
}

/// Pure: a human-meaningful payload summary (concept + reason), replacing
/// the raw `payload_json` the pre-redesign events view truncated to 60
/// chars.
fn summarize_event_payload(kind: &str, payload_json: &str) -> String {
    let concept = event_payload_field(payload_json, "concept");
    match kind {
        "card_shown" | "card_response" | "prompt_offered" => concept.unwrap_or_default(),
        "prompt_response" => {
            let signal = event_payload_field(payload_json, "signal");
            match (concept, signal) {
                (Some(c), Some(s)) => format!("{} ({})", c, s),
                (Some(c), None) => c,
                _ => String::new(),
            }
        }
        "throttle_change" => {
            let category = event_payload_field(payload_json, "category").unwrap_or_default();
            let action = event_payload_field(payload_json, "action").unwrap_or_default();
            format!("{} \u{2014} {}", category, action)
        }
        "goal_inferred" => event_payload_field(payload_json, "text").unwrap_or_default(),
        "encounter" => {
            let grade = event_payload_field(payload_json, "grade").unwrap_or_default();
            format!("{} \u{2014} {}", concept.unwrap_or_default(), grade)
        }
        "fade" => format!("{} \u{2014} backing off", concept.unwrap_or_default()),
        "judge_drop" => "contract failure".to_string(),
        "judge_declined" => "not a teaching moment".to_string(),
        _ => concept.unwrap_or_default(),
    }
}

fn draw_events(f: &mut Frame, area: Rect, app: &App, ctx: &DrawContext) {
    let use_color = theme::color_allowed();
    let Some(conn) = ctx.conn else {
        f.render_widget(
            Paragraph::new("(no database connection)").style(theme::ambient_style()),
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

    if filtered.is_empty() {
        f.render_widget(
            Paragraph::new("(no events yet)").style(theme::ambient_style()),
            area,
        );
        return;
    }

    let selected = app.events_selected.min(filtered.len() - 1);
    let items: Vec<ListItem> = filtered
        .iter()
        .map(|e| {
            let role = event_role(&e.kind, &e.payload_json);
            let word: &str = if role.word.is_empty() { &e.kind } else { role.word };
            let color = if use_color { role.color } else { Color::Reset };
            let ts = e.ts.clone().unwrap_or_default();
            let summary = summarize_event_payload(&e.kind, &e.payload_json);
            let spans = vec![
                Span::styled(format!("{:<20}", ts), theme::ambient_style()),
                Span::styled(
                    format!("{} {:<11}", role.glyph, word),
                    Style::default().fg(color),
                ),
                Span::raw(summary),
            ];
            ListItem::new(Line::from(spans))
        })
        .collect();

    let mut state = ListState::default();
    state.select(Some(selected));
    let list = List::new(items).highlight_style(theme::focus_style());
    f.render_stateful_widget(list, area, &mut state);
}

// =====================================================================
// HISTORY overlay (founder decision, 2026-07-06): scroll back through past
// cards and re-read each one (full card body + worked diff + thread
// transcript) — a full-screen overlay (summoned `h`, popped `h`/`esc`), NOT
// a split pane. Cross-session, bounded to the most recent
// [`HISTORY_LIST_LIMIT`] cards. Distinct from the `E` events log (raw
// session event history, kept unchanged): this is a readable card+thread
// reader, not a debug feed.
// =====================================================================

/// Bounds the HISTORY list to the most recent N cards (founder decision:
/// "recent cards, cross-session, bounded ~50 newest"). Shared by
/// `draw_history` and `tui/mod.rs`'s `⏎`-opens-detail handler so the
/// rendered list and the selectable list are always the exact same rows.
pub(crate) const HISTORY_LIST_LIMIT: i64 = 50;

/// The HISTORY list's rows for this draw pass — `pub(crate)` (mirrors
/// `mastery_rows`) so `tui/mod.rs`'s key handler fetches the identical set
/// `draw_history` renders.
pub(crate) fn history_rows(conn: &rusqlite::Connection) -> Vec<db::CardListRow> {
    db::recent_cards(conn, HISTORY_LIST_LIMIT).unwrap_or_default()
}

fn history_rows_from_ctx(ctx: &DrawContext) -> Vec<db::CardListRow> {
    ctx.conn.map(history_rows).unwrap_or_default()
}

/// Pure: one HISTORY list row — `{age} {status} {concept} · {category}
/// [{n}↩ if thread_turns>0]`. Meaning survives `use_color=false` (status/
/// concept/category words and the `↩` thread marker are all plain text, only
/// the category's color is stripped).
pub(crate) fn history_row_line(
    row: &db::CardListRow,
    now_epoch: i64,
    use_color: bool,
) -> Line<'static> {
    let age = row
        .created_ts
        .as_deref()
        .and_then(parse_sqlite_ts_epoch_secs)
        .map(|then| relative_age(now_epoch, then))
        .unwrap_or_else(|| "\u{2014}".to_string());
    let cat_role = theme::category_style(&pack::Category::parse(&row.category));
    let mut spans = vec![
        Span::raw(format!("{:<10}", age)),
        Span::raw(format!("{:<10}", row.status)),
        Span::raw(format!("{:<28}", row.concept_id)),
        Span::raw(" \u{b7} "),
        cat_role.span(use_color),
    ];
    if row.thread_turns > 0 {
        spans.push(Span::styled(
            format!("  {}\u{21a9}", row.thread_turns),
            theme::ambient_style(),
        ));
    }
    Line::from(spans)
}

fn draw_history(f: &mut Frame, area: Rect, app: &App, ctx: &DrawContext) {
    let use_color = theme::color_allowed();
    let Some(conn) = ctx.conn else {
        f.render_widget(
            Paragraph::new("(no database connection)").style(theme::ambient_style()),
            area,
        );
        return;
    };
    let rows = history_rows(conn);
    if rows.is_empty() {
        f.render_widget(
            Paragraph::new("(no card history yet)").style(theme::ambient_style()),
            area,
        );
        return;
    }
    let now_epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let selected = app.history_selected_clamped(rows.len());
    let items: Vec<ListItem> = rows
        .iter()
        .map(|r| ListItem::new(history_row_line(r, now_epoch, use_color)))
        .collect();

    let mut state = ListState::default();
    state.select(Some(selected));
    let list = List::new(items).highlight_style(theme::focus_style());
    f.render_stateful_widget(list, area, &mut state);
}

/// Pure: the HISTORY detail reader's full body — the persisted card prose
/// (or a "(card text not recorded)" note for a `None` body, e.g. a
/// pre-migration row or a struggle-offer card), the worked diff (`+`/`-`
/// colored, same convention as the live card's worked-example rendering),
/// and the interleaved thread transcript (or "(no thread messages)").
pub(crate) fn history_detail_lines(
    detail: &db::CardDetail,
    msgs: &[db::ThreadMessage],
    now_epoch: i64,
    use_color: bool,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();

    let cat_role = theme::category_style(&pack::Category::parse(&detail.category));
    let header_name = detail
        .body
        .as_ref()
        .map(|b| b.concept_name.clone())
        .unwrap_or_else(|| detail.concept_id.clone());
    lines.push(Line::from(vec![
        cat_role.span(use_color),
        Span::raw(" \u{b7} "),
        Span::raw(header_name),
    ]));

    let age = detail
        .created_ts
        .as_deref()
        .and_then(parse_sqlite_ts_epoch_secs)
        .map(|then| relative_age(now_epoch, then))
        .unwrap_or_else(|| "\u{2014}".to_string());
    lines.push(Line::styled(
        format!(
            "{}  \u{b7}  rung {}  \u{b7}  {}",
            detail.status, detail.rung_shown, age
        ),
        theme::ambient_style(),
    ));
    lines.push(Line::raw(""));

    match &detail.body {
        Some(body) => {
            lines.push(gutter_line(&body.grounding_quote));
            lines.push(Line::raw(""));
            lines.push(Line::raw(body.why.clone()));
            lines.push(Line::raw(""));
            lines.push(Line::from(vec![
                Span::raw("Rule  "),
                Span::raw(body.rule.clone()),
            ]));
            lines.push(Line::from(vec![
                Span::raw("      \u{2192} "),
                Span::raw(body.doc_ref.clone()),
            ]));
        }
        None => {
            lines.push(Line::styled(
                "(card text not recorded)",
                theme::ambient_style(),
            ));
        }
    }

    if let Some(diff) = &detail.worked_diff {
        lines.push(Line::raw(""));
        lines.push(Line::raw("worked example"));
        for l in diff.lines() {
            let color = if l.starts_with('+') {
                Color::Green
            } else if l.starts_with('-') {
                Color::Red
            } else {
                Color::Reset
            };
            let color = if use_color { color } else { Color::Reset };
            lines.push(Line::from(Span::styled(
                l.to_string(),
                Style::default().fg(color),
            )));
        }
    }

    lines.push(Line::raw(""));
    if msgs.is_empty() {
        lines.push(Line::styled("(no thread messages)", theme::ambient_style()));
    } else {
        lines.push(Line::styled("thread", theme::ambient_style()));
        for m in msgs {
            let label = if m.role == "user" { "you" } else { "murshid" };
            let color = if use_color {
                if m.role == "user" { Color::Cyan } else { Color::Reset }
            } else {
                Color::Reset
            };
            lines.push(Line::from(vec![
                Span::styled(format!("{:<8}", label), Style::default().fg(color)),
                Span::raw(m.content.clone()),
            ]));
        }
    }

    lines
}

fn draw_history_detail(f: &mut Frame, area: Rect, app: &App, ctx: &DrawContext, card_id: i64) {
    let use_color = theme::color_allowed();
    let Some(conn) = ctx.conn else {
        f.render_widget(
            Paragraph::new("(no database connection)").style(theme::ambient_style()),
            area,
        );
        return;
    };
    let detail = match db::card_detail(conn, card_id) {
        Ok(Some(d)) => d,
        _ => {
            f.render_widget(
                Paragraph::new("(card not found)").style(theme::ambient_style()),
                area,
            );
            return;
        }
    };
    let msgs = db::get_thread_messages(conn, card_id).unwrap_or_default();
    let now_epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let lines = history_detail_lines(&detail, &msgs, now_epoch, use_color);
    let cols = centered_columns(78, area);
    f.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((app.history_scroll(), 0)),
        cols,
    );
}

// =====================================================================
// Settings overlay (founder ask, MUR-7 2026-07-05): the startup dials —
// `frequency`/`directness` — made VISIBLE and, unlike the rest of the
// dashboard, ADJUSTABLE while the session runs. Summoned with `s`, popped
// with `s`/`esc`, styled like the mastery list (`\u{203a}` + REVERSED
// selection). Session-scoped only: neither row is ever written back to
// `config.toml` — see `apply_settings_cycle` in `tui/mod.rs`.
// =====================================================================

/// One settings row: `(label, current value, one-line plain-language
/// meaning)`. Reads `WatchSession::frequency`/`directness` fresh on every
/// draw, same "read fresh, render plain" posture as every other view here.
fn settings_rows(ctx: &DrawContext) -> Vec<(&'static str, String, &'static str)> {
    let frequency = ctx.ws.frequency.lock_poison_safe().clone();
    let directness = *ctx.ws.directness.lock_poison_safe();
    vec![
        (
            "frequency",
            frequency,
            "how often murshid speaks (quiet \u{b7} standard \u{b7} chatty)",
        ),
        (
            "directness",
            directness.as_str().to_string(),
            "how much it tells vs asks (guide-me \u{b7} balanced \u{b7} tell-me)",
        ),
    ]
}

fn draw_settings(f: &mut Frame, area: Rect, app: &App, ctx: &DrawContext) {
    let rows = settings_rows(ctx);
    let selected = app.settings_selected.min(rows.len().saturating_sub(1));
    let items: Vec<ListItem> = rows
        .iter()
        .enumerate()
        .map(|(i, (label, value, meaning))| {
            let marker = if i == selected { "\u{203a} " } else { "  " };
            let line = Line::from(vec![
                Span::raw(marker),
                Span::raw(format!("{:<12}", label)),
                Span::raw(format!("{:<10}", value)),
                Span::styled(*meaning, theme::ambient_style()),
            ]);
            ListItem::new(line)
        })
        .collect();

    let mut state = ListState::default();
    state.select(Some(selected));
    let list = List::new(items).highlight_style(theme::focus_style());
    f.render_stateful_widget(list, area, &mut state);
}

// =====================================================================
// Keybar (Step 9 restyle) and help overlay.
// =====================================================================

fn push_chip(spans: &mut Vec<Span<'static>>, key: &str, label: &str) {
    if !spans.is_empty() {
        spans.push(Span::raw("   "));
    }
    spans.extend(theme::chip(key, label));
}

/// The founder's core ask: an always-visible keybar showing exactly the
/// keys valid on the focused object right now (design doc §5.3) — chips
/// driven by focus + card/offer presence, never a flat key dump.
/// Card keybar chips for the CURRENT rung. Founder 2026-07-06: `e` (more) and
/// `t` (fix) both clamp to R3, so at R3 — where every comment-ask answer and
/// any fully-escalated card already sits — pressing them did nothing and gave
/// no feedback, yet the keybar still advertised them. Only offer them when they
/// can actually advance the card (below R3). `k` (ask) is a read-only no-op in
/// TUI v1 (no thread view yet), so it isn't advertised until that lands. The
/// resolve keys (a/g/u/n) and the always-on `G` goal are shown at every rung.
pub(crate) fn card_key_chips(rung: ladder::Rung) -> Vec<(&'static str, &'static str)> {
    let mut chips = vec![
        ("a", "applied"),
        ("g", "got it"),
        ("u", "not useful"),
        ("n", "not now"),
    ];
    if rung != ladder::Rung::R3 {
        chips.push(("e", "more"));
        chips.push(("t", "fix"));
    }
    chips.push(("G", "goal"));
    chips
}

fn draw_keybar(f: &mut Frame, area: Rect, app: &App, ctx: &DrawContext) {
    let mut spans: Vec<Span<'static>> = Vec::new();
    // The inline goal editor owns the keybar while open, regardless of focus.
    if app.is_editing_goal() {
        push_chip(&mut spans, "\u{23ce}", "save goal");
        push_chip(&mut spans, "esc", "cancel");
        f.render_widget(Paragraph::new(Line::from(spans)), area);
        return;
    }
    match app.focus() {
        Focus::Home => {
            if app.ack_active() {
                push_chip(&mut spans, "q", "quit");
            } else if ctx.ws.pending_offer.lock_poison_safe().is_some() {
                push_chip(&mut spans, "y", "yes, look");
                push_chip(&mut spans, "n", "not now");
                spans.push(Span::raw("   "));
                spans.push(Span::styled(
                    "(or keep typing \u{2014} this fades)",
                    theme::ambient_style(),
                ));
            } else if let Some(pc) = ctx.ws.pending_card.lock_poison_safe().clone() {
                for (k, label) in card_key_chips(pc.rung) {
                    push_chip(&mut spans, k, label);
                }
            } else {
                push_chip(&mut spans, "m", "mastery");
                push_chip(&mut spans, "s", "settings");
                push_chip(&mut spans, "h", "history");
                push_chip(&mut spans, "G", "set goal");
                push_chip(&mut spans, "?", "help");
                push_chip(&mut spans, "q", "quit");
            }
        }
        Focus::Mastery => {
            push_chip(&mut spans, "\u{2191}/\u{2193}", "move");
            push_chip(&mut spans, "\u{23ce}", "concept detail");
            push_chip(&mut spans, "m/esc", "home");
            push_chip(&mut spans, "?", "help");
            push_chip(&mut spans, "q", "quit");
        }
        Focus::ConceptDetail(_) => {
            push_chip(&mut spans, "esc", "back to meter");
            push_chip(&mut spans, "m", "home");
            push_chip(&mut spans, "q", "quit");
        }
        Focus::Events => {
            let filter_label = format!("cycle filter ({})", app.events_filter.label());
            push_chip(&mut spans, "\u{2191}/\u{2193}", "move");
            push_chip(&mut spans, "f", &filter_label);
            push_chip(&mut spans, "E/esc", "home");
            push_chip(&mut spans, "q", "quit");
        }
        Focus::Settings => {
            push_chip(&mut spans, "\u{2191}/\u{2193}", "select");
            push_chip(&mut spans, "\u{2190}/\u{2192}", "change");
            push_chip(&mut spans, "s/esc", "home");
            push_chip(&mut spans, "?", "help");
            push_chip(&mut spans, "q", "quit");
        }
        Focus::History => {
            push_chip(&mut spans, "\u{2191}/\u{2193}", "move");
            push_chip(&mut spans, "\u{23ce}", "open");
            push_chip(&mut spans, "h/esc", "home");
            push_chip(&mut spans, "q", "quit");
        }
        Focus::HistoryDetail(_) => {
            push_chip(&mut spans, "\u{2191}/\u{2193}", "scroll");
            push_chip(&mut spans, "esc", "back");
            push_chip(&mut spans, "q", "quit");
        }
    }
    // Wrap onto the keybar's 2 rows rather than clipping chips off the right
    // edge — every valid key stays visible on a normal-width terminal.
    f.render_widget(
        Paragraph::new(Line::from(spans)).wrap(Wrap { trim: true }),
        area,
    );
}

fn draw_help_overlay(f: &mut Frame, area: Rect) {
    let popup = centered_rect(72, 80, area);
    f.render_widget(Clear, popup);
    let text = "Murshid \u{2014} help\n\
\n\
Home is the app; mastery/settings/history are summoned, not tabs:\n\
  m       mastery  \u{2014} the per-concept mastery meter\n\
  \u{23ce}       (in mastery) concept detail \u{2014} trend + recent history\n\
  s       settings \u{2014} view/adjust frequency + directness live (session-only, not saved)\n\
  h       history  \u{2014} scroll back through past cards (full card + worked diff + thread)\n\
  esc     pop one level back toward home\n\
\n\
Card actions (home, when a card is on screen):\n\
  a  applied     g  got it        u  not useful     n  not now (snooze)\n\
  e  escalate    t  tell me (jump to the worked example)\n\
  k  ask \u{2014} read-only in this spike\n\
\n\
Struggle offer (when one is pending):\n\
  y  yes, look    n  not now (or keep typing \u{2014} it fades)\n\
\n\
Global (work anywhere on home):\n\
  G  set / change the goal (dedicated key \u{2014} works with or without a card)\n\
  E  event log \u{2014} raw session event history (debug / history view, not primary)\n\
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

    fn sample_card() -> crate::card::Card {
        crate::card::Card {
            concept_name: "Borrow vs. clone".to_string(),
            file: "src/main.rs".to_string(),
            line: 42,
            grounding_quote: "person.name.clone()".to_string(),
            why: "The call only reads the name.".to_string(),
            rule: "Take &str when the function only needs to read the value".to_string(),
            doc_ref: "https://example.com/pack-docs/redundant-clone".to_string(),
            worked_diff: "- fn f(name: String)\n+ fn f(name: &str)".to_string(),
            additional_anchors: Vec::new(),
            overflow_site_count: 0,
        }
    }

    fn sample_pending_card(rung: ladder::Rung) -> PendingCard {
        PendingCard {
            card_id: 1,
            session_id: "sess1".to_string(),
            concept_id: "borrow-vs-clone".to_string(),
            concept_name: "Borrow vs. clone".to_string(),
            advice_fp: "fp-1".to_string(),
            category: "idiom".to_string(),
            rung,
            card: sample_card(),
            site_enclosing_item: None,
            site_anchor_hash: None,
        }
    }

    fn lines_to_strings(lines: &[Line]) -> Vec<String> {
        lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>())
            .collect()
    }

    // --- Step 1: render_card_block shape ---

    #[test]
    fn test_render_card_block_r2_has_anchor_why_rule_doc_ref() {
        let pc = sample_pending_card(ladder::Rung::R2);
        let lines = render_card_block_with_color(&pc, &pack::SurfaceConfig::default(), false);
        let joined = lines_to_strings(&lines).join("\n");
        assert!(joined.contains("src/main.rs:42"));
        assert!(joined.contains("person.name.clone()"));
        assert!(joined.contains("The call only reads the name."));
        assert!(joined.contains("Rule"));
        assert!(joined.contains("redundant-clone"));
    }

    #[test]
    fn test_render_card_block_r0_is_a_bare_recall_question() {
        let pc = sample_pending_card(ladder::Rung::R0);
        let lines = render_card_block_with_color(&pc, &pack::SurfaceConfig::default(), false);
        let joined = lines_to_strings(&lines).join("\n");
        assert!(joined.contains("how would you write this differently?"));
        assert!(!joined.contains("Rule"));
        assert!(!joined.contains(&pc.card.why));
    }

    #[test]
    fn test_render_card_block_r1_is_a_minimal_nudge() {
        let pc = sample_pending_card(ladder::Rung::R1);
        let lines = render_card_block_with_color(&pc, &pack::SurfaceConfig::default(), false);
        let joined = lines_to_strings(&lines).join("\n");
        assert!(joined.contains("nudge"));
        assert!(!joined.contains(&pc.card.why));
        assert!(!joined.contains(&pc.card.rule));
    }

    #[test]
    fn test_render_card_block_r3_unfolds_worked_example_in_place_of_rule() {
        let pc = sample_pending_card(ladder::Rung::R3);
        let lines = render_card_block_with_color(&pc, &pack::SurfaceConfig::default(), false);
        let joined = lines_to_strings(&lines).join("\n");
        assert!(joined.contains("worked example"));
        assert!(joined.contains("fn f(name: String)"));
        assert!(joined.contains("fn f(name: &str)"));
        assert!(!joined.contains("Rule  Take &str"));
    }

    #[test]
    fn test_render_card_block_multi_site_line_present_only_when_aggregated() {
        let mut pc = sample_pending_card(ladder::Rung::R2);
        let single = render_card_block_with_color(&pc, &pack::SurfaceConfig::default(), false);
        assert!(!lines_to_strings(&single).join("\n").contains("also appears"));

        pc.card.additional_anchors = vec![("b.rs".to_string(), 7)];
        let multi = render_card_block_with_color(&pc, &pack::SurfaceConfig::default(), false);
        assert!(lines_to_strings(&multi).join("\n").contains("also appears at b.rs:7"));
    }

    // --- parse_wait_line (kept verbatim, pre-redesign pinned behavior) ---

    #[test]
    fn test_parse_wait_line_none_when_nothing_held() {
        assert!(parse_wait_line(&[]).is_none());
    }

    #[test]
    fn test_parse_wait_line_names_files_and_prompts_the_fix() {
        let line = parse_wait_line(&["src/main.rs".to_string(), "src/lib.rs".to_string()])
            .expect("non-empty waiting set must produce a status line");
        assert!(line.contains("2 file(s)"));
        assert!(line.contains("src/main.rs") && line.contains("src/lib.rs"));
        assert!(line.contains("fix syntax to resume"));
    }

    // --- header justify_line ---

    #[test]
    fn test_justify_line_pads_between_left_and_right() {
        let left = vec![Span::raw("murshid")];
        let right = vec![Span::raw("watching")];
        let line = justify_line(left, right, 40);
        let joined: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(joined.starts_with(" murshid"));
        assert!(joined.trim_end().ends_with("watching"));
        assert_eq!(joined.chars().count(), 40);
    }

    #[test]
    fn test_justify_line_never_panics_when_content_exceeds_width() {
        let left = vec![Span::raw("a very very very long wordmark indeed")];
        let right = vec![Span::raw("a very very very long context indeed")];
        // Must not panic even though left+right > width.
        let _ = justify_line(left, right, 10);
    }

    // --- next-nudge indicator (replaces the old budget gauge) ---

    #[test]
    fn test_card_key_chips_are_rung_aware() {
        let keys = |r| -> Vec<&'static str> {
            card_key_chips(r).into_iter().map(|(k, _)| k).collect()
        };
        // At R3 (fullest) more/fix would be no-ops → not offered; k never in v1.
        let r3 = keys(ladder::Rung::R3);
        assert!(!r3.contains(&"e") && !r3.contains(&"t"), "R3 must not offer more/fix");
        assert!(!r3.contains(&"k"), "ask is not advertised in v1");
        for k in ["a", "g", "u", "n", "G"] {
            assert!(r3.contains(&k), "resolve/goal keys always present: {k}");
        }
        // Below R3 they CAN advance the card → offered.
        for r in [ladder::Rung::R0, ladder::Rung::R1, ladder::Rung::R2] {
            let ks = keys(r);
            assert!(ks.contains(&"e") && ks.contains(&"t"), "{r:?} must offer more/fix");
        }
    }

    #[test]
    fn test_clip_caps_with_ellipsis_and_leaves_short_alone() {
        assert_eq!(clip("short", 54), "short");
        let long = "x".repeat(80);
        let c = clip(&long, 10);
        assert_eq!(c.chars().count(), 10); // 9 kept + ellipsis
        assert!(c.ends_with('\u{2026}'));
    }

    #[test]
    fn test_next_nudge_span_ready_reads_ready() {
        let span = next_nudge_span(true, None, false);
        assert_eq!(span.content, "next nudge: ready");
    }

    #[test]
    fn test_next_nudge_span_cooling_shows_eta() {
        let span = next_nudge_span(false, Some(std::time::Duration::from_secs(6 * 60 + 30)), false);
        assert_eq!(span.content, "next nudge: ~7m"); // rounds up
    }

    #[test]
    fn test_format_nudge_eta_rounds_and_floors() {
        assert_eq!(format_nudge_eta(None), "ready");
        assert_eq!(format_nudge_eta(Some(std::time::Duration::from_secs(30))), "<1m");
        assert_eq!(format_nudge_eta(Some(std::time::Duration::from_secs(60))), "~1m");
        assert_eq!(format_nudge_eta(Some(std::time::Duration::from_secs(7 * 60))), "~7m");
        assert_eq!(format_nudge_eta(Some(std::time::Duration::from_secs(6 * 60 + 1))), "~7m");
    }

    // --- mastery bar ---

    fn progress_row(p_mastery: f64, zero_row: bool, state: progress::ConceptState) -> progress::ProgressRow {
        progress::ProgressRow {
            concept_id: "c1".to_string(),
            name: "Concept One".to_string(),
            category: "idiom".to_string(),
            p_mastery,
            help_level: 1,
            last_encounter_age_secs: Some(60),
            state,
            zero_row,
        }
    }

    #[test]
    fn test_mastery_bar_full_and_empty() {
        let full = mastery_bar(&progress_row(1.0, false, progress::ConceptState::Mastered), false);
        assert_eq!(full.content, "\u{2588}".repeat(MASTERY_BAR_WIDTH));
        let empty = mastery_bar(&progress_row(0.0, false, progress::ConceptState::Learning), false);
        assert_eq!(empty.content, "\u{2591}".repeat(MASTERY_BAR_WIDTH));
    }

    #[test]
    fn test_mastery_bar_zero_row_is_fully_dim() {
        let bar = mastery_bar(&progress_row(0.0, true, progress::ConceptState::Learning), true);
        assert_eq!(bar.content, "\u{2591}".repeat(MASTERY_BAR_WIDTH));
    }

    // --- events payload summarizer / role mapping ---

    #[test]
    fn test_event_role_distinguishes_declined_from_dropped() {
        let declined = event_role("judge_declined", "{}");
        let dropped = event_role("judge_drop", "{}");
        assert_eq!(declined.word, "declined");
        assert_eq!(dropped.word, "dropped");
        assert_ne!(declined.color, dropped.color);
    }

    #[test]
    fn test_event_role_card_response_disambiguates_by_verb() {
        let applied = event_role("card_response", r#"{"verb":"applied"}"#);
        let not_useful = event_role("card_response", r#"{"verb":"not_useful"}"#);
        assert_eq!(applied.word, "applied");
        assert_eq!(not_useful.word, "declined");
    }

    #[test]
    fn test_summarize_event_payload_renders_meaning_not_raw_json() {
        let summary = summarize_event_payload(
            "throttle_change",
            r#"{"category":"idiom","action":"throttled"}"#,
        );
        assert_eq!(summary, "idiom \u{2014} throttled");
        assert!(!summary.contains('{'));
    }

    #[test]
    fn test_summarize_event_payload_malformed_json_never_panics() {
        let summary = summarize_event_payload("card_shown", "not json");
        assert_eq!(summary, "");
    }

    // --- grade history / trend ---

    fn encounter_event(concept: &str, grade: &str) -> db::EventRecord {
        db::EventRecord {
            id: None,
            session_id: "sess1".to_string(),
            kind: "encounter".to_string(),
            payload_json: serde_json::json!({"concept": concept, "grade": grade}).to_string(),
            ts: None,
        }
    }

    #[test]
    fn test_grade_history_heights_maps_grades_and_skips_other_kinds() {
        let events = vec![
            encounter_event("c1", "fail"),
            encounter_event("c1", "hard"),
            encounter_event("c1", "pass"),
            db::EventRecord {
                id: None,
                session_id: "sess1".to_string(),
                kind: "card_shown".to_string(),
                payload_json: serde_json::json!({"concept": "c1"}).to_string(),
                ts: None,
            },
        ];
        assert_eq!(grade_history_heights(&events), vec![1, 2, 3]);
    }

    #[test]
    fn test_trend_description_climbing_dipping_steady_and_empty() {
        assert_eq!(trend_description(&[1, 2, 3]), "climbing");
        assert_eq!(trend_description(&[3, 2, 1]), "dipping");
        assert_eq!(trend_description(&[2, 2]), "steady");
        assert_eq!(trend_description(&[]), "no data yet");
    }

    // --- date parsing ---

    #[test]
    fn test_parse_sqlite_ts_epoch_secs_known_epoch() {
        assert_eq!(
            parse_sqlite_ts_epoch_secs("1970-01-01 00:00:00"),
            Some(0)
        );
        assert_eq!(
            parse_sqlite_ts_epoch_secs("1970-01-02 00:00:00"),
            Some(86400)
        );
    }

    #[test]
    fn test_parse_sqlite_ts_epoch_secs_malformed_is_none() {
        assert!(parse_sqlite_ts_epoch_secs("not a timestamp").is_none());
        assert!(parse_sqlite_ts_epoch_secs("").is_none());
    }

    #[test]
    fn test_relative_age_buckets() {
        assert_eq!(relative_age(100, 100), "just now");
        assert_eq!(relative_age(160, 100), "1m ago");
        assert_eq!(relative_age(3700, 100), "1h ago");
        assert_eq!(relative_age(90_100, 100), "1d ago");
    }

    // --- T15 mentor-state indicator: pulse-state precedence ---

    #[test]
    fn test_select_pulse_state_busy_wins_over_everything() {
        let state = select_pulse_state(true, Some("src/foo.rs".to_string()), true);
        assert_eq!(state, PulseState::Thinking);
    }

    #[test]
    fn test_select_pulse_state_reviewing_wins_over_parse_waiting() {
        let state = select_pulse_state(false, Some("src/foo.rs".to_string()), true);
        assert_eq!(state, PulseState::Reviewing("src/foo.rs".to_string()));
    }

    #[test]
    fn test_select_pulse_state_parse_waiting_when_not_busy_or_reviewing() {
        let state = select_pulse_state(false, None, true);
        assert_eq!(state, PulseState::WaitingParse);
    }

    #[test]
    fn test_select_pulse_state_watching_when_nothing_else_is_true() {
        let state = select_pulse_state(false, None, false);
        assert_eq!(state, PulseState::Watching);
    }

    // --- T15 mentor-state indicator: the idle-surface outcome-line formatter ---

    fn sample_last_review(result: ReviewResult) -> LastReview {
        LastReview {
            file: "src/foo.rs".to_string(),
            result,
            at: std::time::UNIX_EPOCH + std::time::Duration::from_secs(1000),
        }
    }

    #[test]
    fn test_review_outcome_line_nothing_to_flag_names_file_and_age() {
        let last = sample_last_review(ReviewResult::NothingToFlag);
        let line = review_outcome_line(&last, None, 1060); // 60s later -> "1m ago"
        assert_eq!(line, "looked at src/foo.rs 1m ago \u{2014} nothing worth flagging");
    }

    #[test]
    fn test_review_outcome_line_could_not_review_names_the_degraded_reason() {
        let last = sample_last_review(ReviewResult::CouldNotReview);
        let line = review_outcome_line(&last, Some("no model configured"), 1000);
        assert_eq!(
            line,
            "couldn't review src/foo.rs just now \u{2014} no model configured"
        );
    }

    #[test]
    fn test_review_outcome_line_could_not_review_falls_back_without_a_reason() {
        let last = sample_last_review(ReviewResult::CouldNotReview);
        let line = review_outcome_line(&last, None, 1000);
        assert!(line.contains("couldn't reach the model"));
    }

    // --- T15 HISTORY view: history_row_line / history_detail_lines ---

    fn sample_history_row(thread_turns: i64) -> db::CardListRow {
        db::CardListRow {
            id: 1,
            concept_id: "borrow-vs-clone".to_string(),
            category: "idiom".to_string(),
            status: "got_it".to_string(),
            rung_shown: "R2".to_string(),
            created_ts: Some("2026-07-06 12:00:00".to_string()),
            resolved_ts: None,
            thread_turns,
            has_worked_diff: true,
        }
    }

    fn line_to_string(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn test_history_row_line_shows_age_status_concept_category() {
        let row = sample_history_row(0);
        // 2026-07-06 12:00:00 UTC + 3600s -> "1h ago".
        let now = parse_sqlite_ts_epoch_secs("2026-07-06 13:00:00").unwrap();
        let text = line_to_string(&history_row_line(&row, now, false));
        assert!(text.contains("1h ago"));
        assert!(text.contains("got_it"));
        assert!(text.contains("borrow-vs-clone"));
        assert!(text.contains("idiom"), "category word survives no-color: {text}");
        assert!(
            !text.contains('\u{21a9}'),
            "no thread marker when thread_turns == 0"
        );
    }

    #[test]
    fn test_history_row_line_shows_thread_marker_only_when_present() {
        let with_thread = sample_history_row(3);
        let now = parse_sqlite_ts_epoch_secs("2026-07-06 12:00:00").unwrap();
        let text = line_to_string(&history_row_line(&with_thread, now, false));
        assert!(text.contains("3\u{21a9}"));
    }

    #[test]
    fn test_history_row_line_missing_timestamp_degrades_to_dash() {
        let mut row = sample_history_row(0);
        row.created_ts = None;
        let text = line_to_string(&history_row_line(&row, 0, false));
        assert!(text.contains('\u{2014}'));
    }

    fn sample_card_detail(body: Option<db::PersistedCardBody>) -> db::CardDetail {
        db::CardDetail {
            id: 7,
            concept_id: "borrow-vs-clone".to_string(),
            category: "idiom".to_string(),
            status: "got_it".to_string(),
            rung_shown: "R2".to_string(),
            created_ts: Some("2026-07-06 12:00:00".to_string()),
            resolved_ts: None,
            worked_diff: Some("- old\n+ new".to_string()),
            body,
        }
    }

    fn sample_body() -> db::PersistedCardBody {
        db::PersistedCardBody {
            concept_name: "Borrow vs. clone".to_string(),
            grounding_quote: "person.name.clone()".to_string(),
            why: "The call only reads the name.".to_string(),
            rule: "Take &str when the function only needs to read the value".to_string(),
            doc_ref: "https://example.com/pack-docs/redundant-clone".to_string(),
            category: "idiom".to_string(),
        }
    }

    #[test]
    fn test_history_detail_lines_renders_body_and_worked_diff() {
        let detail = sample_card_detail(Some(sample_body()));
        let lines = history_detail_lines(&detail, &[], 0, false);
        let joined = lines_to_strings(&lines).join("\n");
        assert!(joined.contains("Borrow vs. clone"));
        assert!(joined.contains("person.name.clone()"));
        assert!(joined.contains("The call only reads the name."));
        assert!(joined.contains("Rule"));
        assert!(joined.contains("redundant-clone"));
        assert!(joined.contains("worked example"));
        assert!(joined.contains("- old"));
        assert!(joined.contains("+ new"));
    }

    #[test]
    fn test_history_detail_lines_null_body_shows_not_recorded_note() {
        let detail = sample_card_detail(None);
        let lines = history_detail_lines(&detail, &[], 0, false);
        let joined = lines_to_strings(&lines).join("\n");
        assert!(joined.contains("card text not recorded"));
        // Metadata + worked diff still render even with no body.
        assert!(joined.contains("got_it"));
        assert!(joined.contains("- old"));
    }

    #[test]
    fn test_history_detail_lines_empty_thread_shows_a_note() {
        let detail = sample_card_detail(Some(sample_body()));
        let lines = history_detail_lines(&detail, &[], 0, false);
        let joined = lines_to_strings(&lines).join("\n");
        assert!(joined.contains("no thread messages"));
    }

    #[test]
    fn test_history_detail_lines_interleaves_thread_turns_in_order() {
        let detail = sample_card_detail(Some(sample_body()));
        let msgs = vec![
            db::ThreadMessage {
                id: None,
                card_id: 7,
                turn_no: 1,
                role: "user".to_string(),
                content: "why does this need a clone?".to_string(),
                ts: None,
            },
            db::ThreadMessage {
                id: None,
                card_id: 7,
                turn_no: 1,
                role: "assistant".to_string(),
                content: "because the fn only reads it".to_string(),
                ts: None,
            },
        ];
        let lines = history_detail_lines(&detail, &msgs, 0, false);
        let joined = lines_to_strings(&lines).join("\n");
        assert!(!joined.contains("no thread messages"));
        let user_pos = joined.find("why does this need a clone?").unwrap();
        let assistant_pos = joined.find("because the fn only reads it").unwrap();
        assert!(
            user_pos < assistant_pos,
            "turns must render in their original order"
        );
    }
}
