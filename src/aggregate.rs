//! T2 req 6 — same-sweep aggregation: the same concept found at multiple
//! sites in one judging sweep collapses into ONE card listing up to 3
//! anchors ("this pattern appears in N places"); remaining sites are
//! recorded in the card's payload, not rendered inline.

use crate::card::Card;

/// One (concept, site) hit produced by the judge pipeline during a single
/// sweep pass, before aggregation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SweepFinding {
    pub concept_id: String,
    pub category: String,
    pub advice_fp: String,
    pub file: String,
    pub line: usize,
    pub card: Card,
    pub likely_bug: bool,
    pub strict_mode_passed: bool,
}

/// The result of folding one or more same-concept `SweepFinding`s from one
/// sweep into a single pushable/queueable unit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AggregatedFinding {
    pub concept_id: String,
    pub category: String,
    /// The primary (first-seen) site's advice-fp — the key used for
    /// budget/ledger/dedup/cooldown decisions.
    pub advice_fp: String,
    /// The primary card, with `additional_anchors`/`overflow_site_count`
    /// filled in when this concept had more than one site this sweep.
    pub card: Card,
    pub likely_bug: bool,
    pub strict_mode_passed: bool,
    /// Total distinct sites this concept was found at in this sweep.
    pub site_count: usize,
    /// Sites beyond the 3 rendered anchors — recorded in the `card_shown`
    /// event payload, per req 6, never rendered inline.
    pub remaining_sites: Vec<(String, usize)>,
}

/// req 6: a card renders at most this many anchors (the primary site plus
/// this many more).
pub const MAX_ANCHORS: usize = 3;

/// Groups `findings` by `concept_id`, preserving first-seen order, and folds
/// each group into one `AggregatedFinding`.
pub fn aggregate_by_concept(findings: Vec<SweepFinding>) -> Vec<AggregatedFinding> {
    let mut order: Vec<String> = Vec::new();
    let mut groups: std::collections::HashMap<String, Vec<SweepFinding>> =
        std::collections::HashMap::new();
    for f in findings {
        if !groups.contains_key(&f.concept_id) {
            order.push(f.concept_id.clone());
        }
        groups.entry(f.concept_id.clone()).or_default().push(f);
    }

    order
        .into_iter()
        .filter_map(|cid| groups.remove(&cid))
        .map(|mut group| {
            let primary = group.remove(0);
            let mut card = primary.card.clone();

            let mut additional_anchors = Vec::new();
            let mut remaining_sites = Vec::new();
            for f in group.into_iter() {
                if additional_anchors.len() < MAX_ANCHORS - 1 {
                    additional_anchors.push((f.file, f.line));
                } else {
                    remaining_sites.push((f.file, f.line));
                }
            }

            let site_count = 1 + additional_anchors.len() + remaining_sites.len();
            card.additional_anchors = additional_anchors;
            card.overflow_site_count = remaining_sites.len();

            AggregatedFinding {
                concept_id: primary.concept_id,
                category: primary.category,
                advice_fp: primary.advice_fp,
                card,
                likely_bug: primary.likely_bug,
                strict_mode_passed: primary.strict_mode_passed,
                site_count,
                remaining_sites,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finding(concept: &str, file: &str, line: usize) -> SweepFinding {
        SweepFinding {
            concept_id: concept.to_string(),
            category: "idiom".to_string(),
            advice_fp: format!("{}-{}-{}", concept, file, line),
            file: file.to_string(),
            line,
            card: Card {
                concept_name: concept.to_string(),
                file: file.to_string(),
                line,
                grounding_quote: "q".to_string(),
                why: "why".to_string(),
                rule: "rule".to_string(),
                doc_ref: "ref".to_string(),
                worked_diff: "diff".to_string(),
                additional_anchors: Vec::new(),
                overflow_site_count: 0,
            },
            likely_bug: false,
            strict_mode_passed: false,
        }
    }

    #[test]
    fn test_single_site_no_aggregation() {
        let out = aggregate_by_concept(vec![finding("borrow-vs-clone", "a.rs", 1)]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].site_count, 1);
        assert!(out[0].card.additional_anchors.is_empty());
        assert_eq!(out[0].card.overflow_site_count, 0);
    }

    #[test]
    fn test_three_sites_same_concept_fold_into_one_card_with_three_anchors() {
        let findings = vec![
            finding("borrow-vs-clone", "a.rs", 1),
            finding("borrow-vs-clone", "b.rs", 2),
            finding("borrow-vs-clone", "c.rs", 3),
        ];
        let out = aggregate_by_concept(findings);
        assert_eq!(out.len(), 1, "one aggregated card for the whole group");
        let agg = &out[0];
        assert_eq!(agg.site_count, 3);
        assert_eq!(agg.card.file, "a.rs"); // primary = first-seen
        assert_eq!(
            agg.card.additional_anchors,
            vec![("b.rs".to_string(), 2), ("c.rs".to_string(), 3),]
        );
        assert_eq!(agg.card.overflow_site_count, 0);
        assert!(agg.remaining_sites.is_empty());
        assert_eq!(agg.advice_fp, "borrow-vs-clone-a.rs-1");
    }

    #[test]
    fn test_more_than_three_sites_overflow_recorded_not_rendered() {
        let findings = vec![
            finding("borrow-vs-clone", "a.rs", 1),
            finding("borrow-vs-clone", "b.rs", 2),
            finding("borrow-vs-clone", "c.rs", 3),
            finding("borrow-vs-clone", "d.rs", 4),
            finding("borrow-vs-clone", "e.rs", 5),
        ];
        let out = aggregate_by_concept(findings);
        assert_eq!(out.len(), 1);
        let agg = &out[0];
        assert_eq!(agg.site_count, 5);
        assert_eq!(
            agg.card.additional_anchors.len(),
            2,
            "primary + 2 = 3 anchors max"
        );
        assert_eq!(agg.card.overflow_site_count, 2);
        assert_eq!(
            agg.remaining_sites,
            vec![("d.rs".to_string(), 4), ("e.rs".to_string(), 5)]
        );
    }

    #[test]
    fn test_different_concepts_stay_separate_cards_in_first_seen_order() {
        let findings = vec![
            finding("borrow-vs-clone", "a.rs", 1),
            finding("string-vs-str", "b.rs", 2),
            finding("borrow-vs-clone", "c.rs", 3),
        ];
        let out = aggregate_by_concept(findings);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].concept_id, "borrow-vs-clone");
        assert_eq!(out[0].site_count, 2);
        assert_eq!(out[1].concept_id, "string-vs-str");
        assert_eq!(out[1].site_count, 1);
    }
}
