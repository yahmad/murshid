//! T5 req 9 / I24 — `murshid progress`: the open per-concept skill meter.
//! Plain text, I21 rendering rules (NO_COLOR-safe, no color-only meaning),
//! sorted by category then p_mastery, zero-row (never-encountered) concepts
//! listed dimmed.

use crate::db::ConceptMemoryRow;
use crate::pack::TaxonomyConcept;
use std::collections::HashSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConceptState {
    Learning,
    Mastered,
    Stale,
    Throttled,
}

impl ConceptState {
    pub fn as_str(&self) -> &'static str {
        match self {
            ConceptState::Learning => "learning",
            ConceptState::Mastered => "mastered",
            ConceptState::Stale => "stale",
            ConceptState::Throttled => "throttled",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProgressRow {
    pub concept_id: String,
    pub name: String,
    pub category: String,
    pub p_mastery: f64,
    pub help_level: i32,
    /// `None` = never encountered (a zero row).
    pub last_encounter_age_secs: Option<u64>,
    pub state: ConceptState,
    pub zero_row: bool,
}

/// req 9: state precedence when more than one condition applies —
/// mastered (nothing left to throttle/stale) > stale (the actionable
/// signal) > throttled (T2's noise-control flag) > learning (default).
fn resolve_state(
    category: &str,
    p_mastery: f64,
    elapsed: Option<std::time::Duration>,
    retrieval_skips: i32,
    throttled_categories: &HashSet<String>,
) -> ConceptState {
    if crate::bkt::is_mastered(p_mastery) {
        return ConceptState::Mastered;
    }
    if let Some(elapsed) = elapsed {
        if crate::staleness::is_stale(
            &crate::pack::Category::parse(category),
            p_mastery,
            elapsed,
            retrieval_skips as u32,
        ) {
            return ConceptState::Stale;
        }
    }
    if throttled_categories.contains(category) {
        return ConceptState::Throttled;
    }
    ConceptState::Learning
}

/// req 9: assembles one row per taxonomy concept — a `concept_memory` row
/// when one exists, a dimmed zero-row otherwise.
pub fn build_rows(
    memory_rows: &[ConceptMemoryRow],
    taxonomy: &[TaxonomyConcept],
    throttled_categories: &HashSet<String>,
    now_epoch_secs: u64,
) -> Vec<ProgressRow> {
    let mut rows: Vec<ProgressRow> = taxonomy
        .iter()
        .map(
            |concept| match memory_rows.iter().find(|r| r.concept_id == concept.slug) {
                Some(m) => {
                    let elapsed = m
                        .last_encounter_ts
                        .as_deref()
                        .and_then(|s| s.parse::<u64>().ok())
                        .map(|last| {
                            std::time::Duration::from_secs(now_epoch_secs.saturating_sub(last))
                        });
                    ProgressRow {
                        concept_id: concept.slug.clone(),
                        name: concept.name.clone(),
                        category: concept.category.as_str().to_string(),
                        p_mastery: m.p_mastery,
                        help_level: m.help_level,
                        last_encounter_age_secs: elapsed.map(|d| d.as_secs()),
                        state: resolve_state(
                            concept.category.as_str(),
                            m.p_mastery,
                            elapsed,
                            m.retrieval_skips,
                            throttled_categories,
                        ),
                        zero_row: false,
                    }
                }
                None => ProgressRow {
                    concept_id: concept.slug.clone(),
                    name: concept.name.clone(),
                    category: concept.category.as_str().to_string(),
                    p_mastery: 0.0,
                    help_level: 0,
                    last_encounter_age_secs: None,
                    state: ConceptState::Learning,
                    zero_row: true,
                },
            },
        )
        .collect();

    // req 9: sorted category then p.
    rows.sort_by(|a, b| {
        a.category.cmp(&b.category).then(
            a.p_mastery
                .partial_cmp(&b.p_mastery)
                .unwrap_or(std::cmp::Ordering::Equal),
        )
    });
    rows
}

/// req 9: a NO_COLOR-safe bar — meaning conveyed by fill characters and the
/// percentage text, never by color alone.
fn render_bar(p_mastery: f64, width: usize) -> String {
    let filled = ((p_mastery.clamp(0.0, 1.0) * width as f64).round() as usize).min(width);
    let mut bar = String::with_capacity(width);
    bar.push_str(&"#".repeat(filled));
    bar.push_str(&"-".repeat(width - filled));
    bar
}

fn render_age(age_secs: Option<u64>) -> String {
    match age_secs {
        None => "never".to_string(),
        // Under a minute reads "just now" — the old `(s / 60).max(1)` reported
        // a misleading "1m ago" for an encounter that happened seconds ago.
        Some(s) if s < 60 => "just now".to_string(),
        Some(s) if s < 3600 => format!("{}m ago", s / 60),
        Some(s) if s < 86400 => format!("{}h ago", s / 3600),
        Some(s) => format!("{}d ago", s / 86400),
    }
}

const BAR_WIDTH: usize = 10;

/// req 9: renders one row. Dimming (zero-row concepts) uses an ANSI dim
/// escape ONLY when color is allowed (`card::color_allowed`); the `(not yet
/// encountered)` text marker carries the meaning regardless — I21's "no
/// color-only meaning".
fn render_row(row: &ProgressRow, use_color: bool) -> String {
    let bar = render_bar(row.p_mastery, BAR_WIDTH);
    let pct = (row.p_mastery.clamp(0.0, 1.0) * 100.0).round() as u32;
    let line = if row.zero_row {
        format!(
            "  [{}] {:<28} [{}] {:>3}%  help:{}  {}  (not yet encountered)",
            row.category,
            row.name,
            bar,
            pct,
            row.help_level,
            render_age(row.last_encounter_age_secs)
        )
    } else {
        format!(
            "  [{}] {:<28} [{}] {:>3}%  help:{}  {}  {}",
            row.category,
            row.name,
            bar,
            pct,
            row.help_level,
            render_age(row.last_encounter_age_secs),
            row.state.as_str()
        )
    };
    if row.zero_row && use_color {
        format!("\x1b[2m{}\x1b[0m", line)
    } else {
        line
    }
}

/// req 9: renders the full meter.
pub fn render_progress(rows: &[ProgressRow]) -> String {
    render_progress_with_color(rows, crate::card::color_allowed())
}

pub fn render_progress_with_color(rows: &[ProgressRow], use_color: bool) -> String {
    if rows.is_empty() {
        return "(no concepts in the taxonomy)\n".to_string();
    }
    let mut out = String::new();
    for row in rows {
        out.push_str(&render_row(row, use_color));
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_render_age_buckets() {
        assert_eq!(render_age(None), "never");
        assert_eq!(render_age(Some(0)), "just now");
        assert_eq!(render_age(Some(59)), "just now");
        assert_eq!(render_age(Some(60)), "1m ago");
        assert_eq!(render_age(Some(3599)), "59m ago");
        assert_eq!(render_age(Some(3600)), "1h ago");
        assert_eq!(render_age(Some(86400)), "1d ago");
    }

    fn taxonomy() -> Vec<TaxonomyConcept> {
        vec![
            TaxonomyConcept {
                slug: "c1".to_string(),
                name: "Concept One".to_string(),
                category: crate::pack::Category::Idiom,
            },
            TaxonomyConcept {
                slug: "c2".to_string(),
                name: "Concept Two".to_string(),
                category: crate::pack::Category::Bug,
            },
            TaxonomyConcept {
                slug: "c3".to_string(),
                name: "Concept Three".to_string(),
                category: crate::pack::Category::Idiom,
            },
        ]
    }

    fn mem_row(concept: &str, p: f64, age_secs: Option<u64>, now: u64) -> ConceptMemoryRow {
        ConceptMemoryRow {
            concept_id: concept.to_string(),
            p_mastery: p,
            help_level: 1,
            last_encounter_ts: age_secs.map(|a| (now - a).to_string()),
            last_outcome: Some("pass".to_string()),
            lapse_count: 0,
            fade_announced_ts: None,
            pass_streak: 2,
            retrieval_skips: 0,
        }
    }

    // --- req 9: sorting ---

    #[test]
    fn test_build_rows_sorted_by_category_then_p() {
        let now = 1_000_000u64;
        let memory = vec![
            mem_row("c1", 0.8, Some(60), now),
            mem_row("c3", 0.3, Some(60), now),
        ];
        let rows = build_rows(&memory, &taxonomy(), &HashSet::new(), now);
        let order: Vec<&str> = rows.iter().map(|r| r.concept_id.as_str()).collect();
        // bug < idiom alphabetically; within idiom, p=0.3 (c3) before p=0.8 (c1).
        assert_eq!(order, vec!["c2", "c3", "c1"]);
    }

    // --- req 9: zero-row concepts ---

    #[test]
    fn test_zero_row_concept_never_encountered() {
        let now = 1_000_000u64;
        let rows = build_rows(&[], &taxonomy(), &HashSet::new(), now);
        assert!(rows.iter().all(|r| r.zero_row));
        assert!(rows.iter().all(|r| r.last_encounter_age_secs.is_none()));
    }

    #[test]
    fn test_render_zero_row_marks_not_yet_encountered() {
        let now = 1_000_000u64;
        let rows = build_rows(&[], &taxonomy(), &HashSet::new(), now);
        let rendered = render_progress_with_color(&rows, false);
        assert!(rendered.contains("(not yet encountered)"));
    }

    // --- req 9: state resolution ---

    #[test]
    fn test_state_mastered() {
        let now = 1_000_000u64;
        let memory = vec![mem_row("c1", 0.97, Some(60), now)];
        let rows = build_rows(&memory, &taxonomy(), &HashSet::new(), now);
        let c1 = rows.iter().find(|r| r.concept_id == "c1").unwrap();
        assert_eq!(c1.state, ConceptState::Mastered);
    }

    #[test]
    fn test_state_stale() {
        let now = 10_000_000u64;
        let window = crate::staleness::staleness_window(&crate::pack::Category::Idiom)
            .unwrap()
            .as_secs();
        let memory = vec![mem_row("c1", 0.9, Some(window + 10), now)];
        let rows = build_rows(&memory, &taxonomy(), &HashSet::new(), now);
        let c1 = rows.iter().find(|r| r.concept_id == "c1").unwrap();
        assert_eq!(c1.state, ConceptState::Stale);
    }

    #[test]
    fn test_state_throttled() {
        let now = 1_000_000u64;
        let memory = vec![mem_row("c1", 0.5, Some(60), now)];
        let mut throttled = HashSet::new();
        throttled.insert("idiom".to_string());
        let rows = build_rows(&memory, &taxonomy(), &throttled, now);
        let c1 = rows.iter().find(|r| r.concept_id == "c1").unwrap();
        assert_eq!(c1.state, ConceptState::Throttled);
    }

    #[test]
    fn test_state_learning_default() {
        let now = 1_000_000u64;
        let memory = vec![mem_row("c1", 0.5, Some(60), now)];
        let rows = build_rows(&memory, &taxonomy(), &HashSet::new(), now);
        let c1 = rows.iter().find(|r| r.concept_id == "c1").unwrap();
        assert_eq!(c1.state, ConceptState::Learning);
    }

    // --- req 9 / I21: NO_COLOR-safe rendering ---

    #[test]
    fn test_render_no_color_has_no_ansi_escapes() {
        let now = 1_000_000u64;
        let rows = build_rows(&[], &taxonomy(), &HashSet::new(), now);
        let rendered = render_progress_with_color(&rows, false);
        assert!(!rendered.contains('\x1b'));
    }

    #[test]
    fn test_render_bar_full_and_empty() {
        assert_eq!(render_bar(0.0, 10), "-".repeat(10));
        assert_eq!(render_bar(1.0, 10), "#".repeat(10));
        assert_eq!(render_bar(0.5, 10), "#####-----");
    }

    #[test]
    fn test_render_progress_snapshot_contains_percentage_and_help_level() {
        let now = 1_000_000u64;
        let memory = vec![mem_row("c1", 0.6, Some(3600), now)];
        let rows = build_rows(&memory, &taxonomy(), &HashSet::new(), now);
        let rendered = render_progress_with_color(&rows, false);
        assert!(rendered.contains(" 60%"));
        assert!(rendered.contains("help:1"));
        assert!(rendered.contains("1h ago"));
    }
}
