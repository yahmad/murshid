//! Orchestration glue: session-diff hunks -> two-stage judge -> one R2 card
//! (T1 vertical slice). Dispatch is injected so this is testable against
//! recorded fixture responses, never a live call, per the T1 acceptance
//! note on LLM calls in tests.

use crate::diff::{DiffOp, Hunk};
use crate::pack::{CanonEntry, TaxonomyConcept};

/// Builds the stage-1 (screen) prompt: the pack's own framing (payload 4,
/// delivered opaque per C9) + session-diff hunks + one line of enclosing
/// context per hunk + the taxonomy slug list (T1 req 6), sanitized per
/// req 5.
pub fn build_stage1_prompt(
    framing: &str,
    rel_file: &str,
    hunks: &[Hunk],
    taxonomy: &[TaxonomyConcept],
) -> String {
    let mut s = String::new();
    if !framing.is_empty() {
        s.push_str(framing);
        s.push_str("\n\n");
    }
    s.push_str(&format!("File: {}\n\n", rel_file));

    for hunk in hunks {
        s.push_str("--- hunk ---\n");
        if let Some(ctx) = hunk.ops.iter().find_map(|op| match op {
            DiffOp::Context { text, .. } => Some(text.clone()),
            _ => None,
        }) {
            s.push_str(&format!("context: {}\n", ctx));
        }
        for op in &hunk.ops {
            match op {
                DiffOp::Added { new_line, text } => {
                    s.push_str(&format!("+{} {}\n", new_line, text))
                }
                DiffOp::Removed { old_line, text } => {
                    s.push_str(&format!("-{} {}\n", old_line, text))
                }
                DiffOp::Context { .. } => {}
            }
        }
    }

    s.push_str("\nTaxonomy slugs: ");
    s.push_str(
        &taxonomy
            .iter()
            .map(|c| c.slug.as_str())
            .collect::<Vec<_>>()
            .join(", "),
    );
    s.push('\n');

    crate::sanitizer::sanitize_diagnostics(&s)
}

/// T16a: the judge-context budget (~24k tokens, config-grade per C1 — the
/// local window is 150k now, but this stays bounded on purpose). Token cost
/// uses the same ~4-chars/token estimate `consent::estimate_tokens` already
/// uses elsewhere in the crate.
pub const JUDGE_CONTEXT_BUDGET_TOKENS: usize = 24_000;

/// T16a: fills the judge-context budget — the enclosing item's own cost is
/// reserved first (it's shown unconditionally, even with zero requests),
/// then `blocks` (already resolved, in the model's own request order) are
/// added while they fit. The FIRST block that would overflow the budget,
/// and every block requested after it, is dropped — "stop at budget" per
/// spec, not a best-effort repack of smaller later blocks into the gap.
/// Returns `(included, dropped_headers)`.
pub fn fill_context_budget(
    enclosing_item_text: &str,
    blocks: &[crate::site::ContextBlock],
    budget_tokens: usize,
) -> (Vec<crate::site::ContextBlock>, Vec<String>) {
    let mut used = crate::consent::estimate_tokens(enclosing_item_text);
    let mut included = Vec::new();
    let mut dropped = Vec::new();
    let mut over_budget = false;
    for block in blocks {
        if over_budget {
            dropped.push(block.header.clone());
            continue;
        }
        let cost = crate::consent::estimate_tokens(&block.text);
        if used + cost > budget_tokens {
            dropped.push(block.header.clone());
            over_budget = true;
            continue;
        }
        used += cost;
        included.push(block.clone());
    }
    (included, dropped)
}

/// T16a: assembles the full judge context — the enclosing item ALWAYS
/// first, then the (already budget-filled) resolved blocks, each rendered
/// as `"<header>:\n<text>"`. With no blocks (T16a's backward-compat path:
/// stage-1 requested no extra context), this is byte-identical to just the
/// enclosing item text — today's exact behavior.
pub fn assemble_judge_context(
    enclosing_item_text: &str,
    blocks: &[crate::site::ContextBlock],
) -> String {
    let mut s = enclosing_item_text.to_string();
    for block in blocks {
        s.push_str(&format!("\n\n{}:\n{}", block.header, block.text));
    }
    s
}

/// T16a: builds the injected file-reader for `site::resolve_context_requests`'s
/// cross-file `range`/`file` asks, scoped to `project_root`. An absolute path
/// or any `..` component is rejected outright as a cheap fast-path, but the
/// real bound is enforced by CANONICALIZING the joined path and confirming it
/// still lives under the (canonicalized) root — so an in-repo SYMLINK pointing
/// outside the project (which has no `..` and isn't absolute) cannot be
/// followed to exfiltrate an out-of-root file to the model provider (T16a gate
/// finding, 2026-07-06). `Path::join` is purely lexical and `read_to_string`
/// follows symlinks, so lexical checks alone are not a sandbox. A path that
/// doesn't resolve to a real, readable, in-root file yields `None` (the
/// resolver treats that as unresolvable, never fabricated). Both sides are
/// canonicalized so a symlinked root (e.g. macOS `/tmp` → `/private/tmp`)
/// still matches. Shared by every call site so the rule lives in one place.
pub fn project_scoped_file_reader(
    project_root: std::path::PathBuf,
) -> impl Fn(&str) -> Option<String> {
    move |rel: &str| {
        let rel_path = std::path::Path::new(rel);
        // Cheap lexical fast-reject (an absolute path or `..` never needs the
        // filesystem to be ruled out).
        if rel_path.is_absolute()
            || rel_path
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return None;
        }
        // Authoritative bound: resolve symlinks/`.`/`..` against the real
        // filesystem and require containment under the canonical root.
        let root = project_root.canonicalize().ok()?;
        let canon = project_root.join(rel_path).canonicalize().ok()?;
        if !canon.starts_with(&root) {
            return None;
        }
        std::fs::read_to_string(canon).ok()
    }
}

/// T16a: diagnostics (T14) for the model-directed context-request path —
/// what stage-1 asked for, what actually made it into the stage-2 prompt,
/// and what was dropped (unresolvable, or trimmed by the budget) — always
/// `None` when stage-1 requested nothing (the backward-compat path never
/// constructs one).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextTrace {
    pub requested: Vec<crate::judge::ContextRequest>,
    pub fetched: Vec<String>,
    /// `(label, reason)` — a request's debug label (or its resolved header,
    /// for an over-budget drop) paired with why it didn't make it in.
    pub dropped: Vec<(String, String)>,
}

/// Builds the stage-2 (judge) prompt: the pack's own framing (payload 4) +
/// one candidate + the full judge context (the enclosing item, plus any
/// model-directed resolved context blocks — T16a, already budget-filled via
/// [`fill_context_budget`]/[`assemble_judge_context`]) + the matching canon
/// entries (T1 req 7), sanitized per req 5.
pub fn build_stage2_prompt(
    framing: &str,
    candidate_slugs: &[String],
    full_context: &str,
    canon: &[CanonEntry],
) -> String {
    let mut s = String::new();
    if !framing.is_empty() {
        s.push_str(framing);
        s.push_str("\n\n");
    }
    s.push_str("Enclosing item:\n");
    s.push_str(full_context);
    s.push_str("\n\nCanon entries:\n");
    for slug in candidate_slugs {
        if let Some(entry) = crate::pack::find_canon_for_concept(canon, slug) {
            s.push_str(&format!(
                "- {} :: {}\n  why: {}\n  use instead: {}\n",
                entry.concept, entry.what_it_does, entry.why_is_this_bad, entry.use_instead
            ));
        }
    }
    crate::sanitizer::sanitize_diagnostics(&s)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JudgeOutcome {
    pub card: Option<crate::card::Card>,
    pub stage2: Option<crate::judge::Stage2Card>,
    pub drop_reason: Option<crate::judge::JudgeDropReason>,
    /// C6/D10: whether a `likely_bug` card cleared strict mode (second
    /// independent sample agreed + concrete failure scenario), i.e. may
    /// bypass the push budget. Always `false` for non-bug cards.
    pub strict_mode_passed: bool,
    /// req 8/req-4-fix: true when every one of stage-1's candidate slugs at
    /// this site had already been judged/shown this session, so stage 2 was
    /// never dispatched.
    pub deduped: bool,
    /// T5 req 3 / C6: stage-1's OTHER output leg — positive-application
    /// detections, resolved to a concrete (new-file) line via the hunks.
    /// Populated even when `card`/`stage2` are `None` (the dual output is
    /// independent of whether a teaching-moment candidate also fired).
    pub application_detections: Vec<(crate::judge::Stage1Detection, usize)>,
    /// T16a: `Some` only when the chosen candidate asked for extra judge
    /// context — `None` is the exact backward-compat case (nothing
    /// requested, nothing to trace).
    pub context_trace: Option<ContextTrace>,
}

impl JudgeOutcome {
    fn empty() -> Self {
        Self {
            card: None,
            stage2: None,
            drop_reason: None,
            strict_mode_passed: false,
            deduped: false,
            application_detections: Vec::new(),
            context_trace: None,
        }
    }
}

/// Runs stage 1 -> (dedup check) -> stage 2 -> contract validation -> (strict
/// mode re-sample) over one changed file's session-diff hunks, producing at
/// most one card. `dispatch_stage1`/`dispatch_stage2` are injected so this
/// never makes a live call in tests; `already_judged` reports whether an
/// advice-fingerprint (`concept`, site) has already been judged/shown this
/// session (T1 req 8) — it's checked *before* stage 2 is dispatched so
/// repeat encounters at a known site don't re-burn the strong model.
/// `resolve_file` (T16a) is the injected cross-file reader for the chosen
/// candidate's `context_request` leg — see
/// [`crate::site::resolve_context_requests`]; a candidate with no
/// `context_request` never calls it.
// Still >7 args after adding the T16a file-reader: every param besides the
// PackData bundle is either per-call data (rel_file/hunks/current_content)
// or an injected closure (already_judged/dispatch_stage1/dispatch_stage2/
// resolve_file) mirroring `judge_and_collect_finding`'s established
// arity — inherent, not accidental, complexity.
#[allow(clippy::too_many_arguments)]
pub fn judge_hunks(
    rel_file: &str,
    hunks: &[Hunk],
    current_content: &str,
    pack: crate::pack::PackData,
    already_judged: impl Fn(&str) -> bool,
    dispatch_stage1: impl Fn(&str) -> Result<String, String>,
    dispatch_stage2: impl Fn(&str) -> Result<String, String>,
    resolve_file: impl Fn(&str) -> Option<String>,
) -> Result<JudgeOutcome, String> {
    if hunks.is_empty() {
        return Ok(JudgeOutcome::empty());
    }
    let crate::pack::PackData {
        taxonomy,
        canon,
        grammar,
        prompts,
    } = pack;

    let stage1_prompt = build_stage1_prompt(&prompts.stage1, rel_file, hunks, taxonomy);
    let stage1_raw = dispatch_stage1(&stage1_prompt)?;
    let (candidates, detections_raw) = crate::judge::parse_stage1_full(&stage1_raw)?;

    let changed_lines = crate::diff::changed_line_numbers(hunks);
    let fallback_line = changed_lines.first().copied();

    // T5 req 3 / C6: resolve the dual-output detections leg to a concrete
    // line regardless of whether a teaching-moment candidate also fires
    // below — this is independent evidence, not a side effect of the card
    // pipeline.
    let application_detections: Vec<(crate::judge::Stage1Detection, usize)> = match fallback_line {
        Some(fallback) => detections_raw
            .into_iter()
            .map(|d| {
                let line = crate::diff::resolve_site_hint_line(hunks, &d.site_hint, fallback);
                (d, line)
            })
            .collect(),
        None => Vec::new(),
    };

    let Some(candidate) = candidates.into_iter().next() else {
        let mut outcome = JudgeOutcome::empty();
        outcome.application_detections = application_detections;
        return Ok(outcome);
    };

    let Some(line) = fallback_line else {
        let mut outcome = JudgeOutcome::empty();
        outcome.application_detections = application_detections;
        return Ok(outcome);
    };

    // req 8 / req-4-fix: dedup before dispatching stage 2. If every
    // candidate slug is already judged at this site, skip re-screening.
    if !candidate.slugs.is_empty() {
        if let Some(site) = crate::site::compute_site(rel_file, current_content, line, grammar) {
            let all_known = candidate
                .slugs
                .iter()
                .all(|slug| already_judged(&crate::site::advice_fingerprint(slug, &site)));
            if all_known {
                let mut outcome = JudgeOutcome::empty();
                outcome.deduped = true;
                outcome.application_detections = application_detections;
                return Ok(outcome);
            }
        }
    }

    let enclosing_text = crate::site::enclosing_item_text(current_content, line, grammar)
        .unwrap_or_else(|| current_content.to_string());

    // T16a: the model-directed context-request leg. Empty/absent (today's
    // exact behavior) short-circuits to zero resolved blocks without ever
    // invoking `resolve_file` — `full_context` then equals `enclosing_text`
    // exactly, so the stage-2 prompt and the grounding check are
    // byte-identical to pre-T16a.
    let resolved = crate::site::resolve_context_requests(
        &candidate.context_request,
        rel_file,
        current_content,
        grammar,
        &resolve_file,
    );
    let (included_blocks, dropped_over_budget) =
        fill_context_budget(&enclosing_text, &resolved.blocks, JUDGE_CONTEXT_BUDGET_TOKENS);
    let full_context = assemble_judge_context(&enclosing_text, &included_blocks);

    let context_trace = if candidate.context_request.is_empty() {
        None
    } else {
        let mut dropped = resolved.unresolved.clone();
        dropped.extend(
            dropped_over_budget
                .iter()
                .map(|header| (header.clone(), "over_budget".to_string())),
        );
        Some(ContextTrace {
            requested: candidate.context_request.clone(),
            fetched: included_blocks.iter().map(|b| b.header.clone()).collect(),
            dropped,
        })
    };

    let stage2_prompt =
        build_stage2_prompt(&prompts.stage2, &candidate.slugs, &full_context, canon);
    let stage2_raw = dispatch_stage2(&stage2_prompt)?;

    let raw = match crate::judge::parse_stage2_output(&stage2_raw) {
        Ok(r) => r,
        Err(reason) => {
            let mut outcome = JudgeOutcome::empty();
            outcome.drop_reason = Some(reason);
            outcome.application_detections = application_detections;
            outcome.context_trace = context_trace;
            return Ok(outcome);
        }
    };

    // T16a (amends D9): grounding is verified against the FULL provided
    // context (enclosing item + resolved blocks), not the whole anchor
    // file — a stronger guarantee (the judge CAN ground in the caller it
    // asked for) that stays honest (it still can't ground in text it
    // wasn't given).
    let stage2_card = match crate::judge::validate_stage2_output(&raw, taxonomy, &full_context) {
        Ok(c) => c,
        Err(reason) => {
            let mut outcome = JudgeOutcome::empty();
            outcome.drop_reason = Some(reason);
            outcome.application_detections = application_detections;
            outcome.context_trace = context_trace;
            return Ok(outcome);
        }
    };

    // C6 strict mode (D10): only for likely_bug cards, dispatch a second
    // independent stage-2 sample; disagreement or a failed/unparseable
    // re-sample just means "not budget-exempt", never drops the card.
    let strict_mode_passed = if stage2_card.likely_bug {
        dispatch_stage2(&stage2_prompt)
            .ok()
            .and_then(|second_raw| crate::judge::parse_stage2_output(&second_raw).ok())
            .and_then(|second_parsed| {
                crate::judge::validate_stage2_output(&second_parsed, taxonomy, &full_context).ok()
            })
            .map(|second_card| crate::judge::strict_mode_passed(&stage2_card, &second_card))
            .unwrap_or(false)
    } else {
        false
    };

    let canon_entry = crate::pack::find_canon_for_concept(canon, &stage2_card.concept);
    let doc_ref = canon_entry
        .and_then(|e| e.refs.first().cloned())
        .unwrap_or_default();
    let concept_name = taxonomy
        .iter()
        .find(|c| c.slug == stage2_card.concept)
        .map(|c| c.name.clone())
        .unwrap_or_else(|| stage2_card.concept.clone());

    let card = crate::card::Card {
        concept_name,
        file: rel_file.to_string(),
        line,
        grounding_quote: stage2_card.grounding_quote.clone(),
        why: stage2_card.why.clone(),
        rule: stage2_card.rule.clone(),
        doc_ref,
        worked_diff: stage2_card.worked_diff.clone(),
        additional_anchors: Vec::new(),
        overflow_site_count: 0,
    };

    Ok(JudgeOutcome {
        card: Some(card),
        stage2: Some(stage2_card),
        drop_reason: None,
        strict_mode_passed,
        deduped: false,
        application_detections,
        context_trace,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn taxonomy() -> Vec<TaxonomyConcept> {
        // T9 req 9 addendum: default_pack_dir() reads MURSHID_PACKS_DIR, which
        // pack.rs's own tests mutate process-wide; take the crate-wide env
        // lock so this read can never straddle that window.
        let _lock = crate::credentials::env_test_lock();
        crate::pack::load_taxonomy(&crate::pack::default_pack_dir()).unwrap()
    }

    fn canon() -> Vec<CanonEntry> {
        let _lock = crate::credentials::env_test_lock();
        crate::pack::load_canon(&crate::pack::default_pack_dir()).unwrap()
    }

    fn grammar() -> crate::pack::GrammarSpec {
        crate::pack::GrammarSpec::default()
    }

    fn prompts() -> crate::pack::PromptFragments {
        let _lock = crate::credentials::env_test_lock();
        crate::pack::load_prompt_fragments(&crate::pack::default_pack_dir()).unwrap()
    }

    fn fixture(name: &str) -> String {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name);
        std::fs::read_to_string(&path).unwrap()
    }

    /// T16a: the injected cross-file reader for every pre-T16a test below —
    /// none of them exercise a `range`/`file` context request, so this is
    /// never actually called; it exists purely to satisfy `judge_hunks`'s
    /// new injected-closure parameter.
    fn no_file_reader(_path: &str) -> Option<String> {
        None
    }

    #[test]
    fn test_build_stage1_prompt_contains_hunks_and_taxonomy() {
        let old = "fn print_name(name: String) { println!(\"{}\", name); }\n";
        let new = "fn print_name(name: String) { println!(\"{}\", name); }\nprint_name(person.name.clone());\n";
        let hunks = crate::diff::diff_lines(old, new);
        let prompt = build_stage1_prompt(&prompts().stage1, "src/main.rs", &hunks, &taxonomy());
        assert!(prompt.contains("File: src/main.rs"));
        assert!(prompt.contains("person.name.clone()"));
        assert!(prompt.contains("borrow-vs-clone"));
    }

    /// The automated stand-in for T1's manual dogfood check: an introduced
    /// `.clone()`-where-borrow-works diff, run through the full stage1 ->
    /// stage2 -> validation pipeline against recorded fixture responses,
    /// produces one card naming `borrow-vs-clone` with a verbatim grounding
    /// quote.
    #[test]
    fn test_judge_hunks_full_flow_produces_borrow_vs_clone_card() {
        let old = "fn print_name(name: String) { println!(\"{}\", name); }\n";
        let new = "fn print_name(name: String) { println!(\"{}\", name); }\nprint_name(person.name.clone());\n";
        let hunks = crate::diff::diff_lines(old, new);

        let stage1_fixture = fixture("stage1_response.json");
        let stage2_fixture = fixture("stage2_response_valid.json");

        let outcome = judge_hunks(
            "src/main.rs",
            &hunks,
            new,
            crate::pack::PackData {
                taxonomy: &taxonomy(),
                canon: &canon(),
                grammar: &grammar(),
                prompts: &prompts(),
            },
            |_fp| false,
            |_prompt| Ok(stage1_fixture.clone()),
            |_prompt| Ok(stage2_fixture.clone()),
            no_file_reader,
        )
        .unwrap();

        assert!(outcome.drop_reason.is_none());
        assert!(!outcome.deduped);
        let card = outcome.card.expect("expected a card");
        assert_eq!(card.concept_name, "Borrow vs. clone");
        assert!(new.contains(&card.grounding_quote));
        assert_eq!(card.file, "src/main.rs");
        // req 9: worked_diff must be carried through, not discarded.
        assert!(!card.worked_diff.is_empty());
        // Not a likely_bug candidate in this fixture, so strict mode never engages.
        assert!(!outcome.strict_mode_passed);
    }

    #[test]
    fn test_judge_hunks_reports_drop_reason_on_invalid_stage2() {
        let old = "fn print_name(name: String) { println!(\"{}\", name); }\n";
        let new = "fn print_name(name: String) { println!(\"{}\", name); }\nprint_name(person.name.clone());\n";
        let hunks = crate::diff::diff_lines(old, new);

        let stage1_fixture = fixture("stage1_response.json");
        let bad_stage2 = r#"{"concept": "not-a-real-concept", "grounding_quote": "person.name.clone()", "why": "x", "rule": "y", "worked_diff": "z", "category": "idiom", "likely_bug": false}"#.to_string();

        let outcome = judge_hunks(
            "src/main.rs",
            &hunks,
            new,
            crate::pack::PackData {
                taxonomy: &taxonomy(),
                canon: &canon(),
                grammar: &grammar(),
                prompts: &prompts(),
            },
            |_fp| false,
            |_prompt| Ok(stage1_fixture.clone()),
            |_prompt| Ok(bad_stage2.clone()),
            no_file_reader,
        )
        .unwrap();

        assert!(outcome.card.is_none());
        assert!(matches!(
            outcome.drop_reason,
            Some(crate::judge::JudgeDropReason::NonTaxonomyConcept(_))
        ));
    }

    #[test]
    fn test_judge_hunks_none_when_no_hunks() {
        let outcome = judge_hunks(
            "src/main.rs",
            &[],
            "fn a() {}\n",
            crate::pack::PackData {
                taxonomy: &taxonomy(),
                canon: &canon(),
                grammar: &grammar(),
                prompts: &prompts(),
            },
            |_fp| false,
            |_| Ok("[]".to_string()),
            |_| Ok("{}".to_string()),
            no_file_reader,
        )
        .unwrap();
        assert!(outcome.card.is_none());
        assert!(outcome.drop_reason.is_none());
    }

    #[test]
    fn test_judge_hunks_none_when_stage1_has_no_candidates() {
        let old = "fn a() {}\n";
        let new = "fn a() { let x = 1; }\n";
        let hunks = crate::diff::diff_lines(old, new);

        let outcome = judge_hunks(
            "src/main.rs",
            &hunks,
            new,
            crate::pack::PackData {
                taxonomy: &taxonomy(),
                canon: &canon(),
                grammar: &grammar(),
                prompts: &prompts(),
            },
            |_fp| false,
            |_| Ok("[]".to_string()),
            |_| Ok("{}".to_string()),
            no_file_reader,
        )
        .unwrap();
        assert!(outcome.card.is_none());
    }

    // --- req 8 / req-4-fix: dedup before dispatching stage 2 ---

    #[test]
    fn test_judge_hunks_skips_stage2_when_all_candidate_slugs_already_judged() {
        let old = "fn print_name(name: String) { println!(\"{}\", name); }\n\nfn caller() {\n}\n";
        let new = "fn print_name(name: String) { println!(\"{}\", name); }\n\nfn caller() {\n    print_name(person.name.clone());\n}\n";
        let hunks = crate::diff::diff_lines(old, new);

        let stage1_fixture = fixture("stage1_response.json"); // candidate slugs = ["borrow-vs-clone"]
        let stage2_calls = std::cell::Cell::new(0);

        let outcome = judge_hunks(
            "src/main.rs",
            &hunks,
            new,
            crate::pack::PackData {
                taxonomy: &taxonomy(),
                canon: &canon(),
                grammar: &grammar(),
                prompts: &prompts(),
            },
            |_fp| true, // every advice-fp is already judged
            |_prompt| Ok(stage1_fixture.clone()),
            |_prompt| {
                stage2_calls.set(stage2_calls.get() + 1);
                Ok("{}".to_string())
            },
            no_file_reader,
        )
        .unwrap();

        assert!(outcome.deduped);
        assert!(outcome.card.is_none());
        assert_eq!(
            stage2_calls.get(),
            0,
            "stage 2 must never be dispatched for a fully-known site"
        );
    }

    #[test]
    fn test_judge_hunks_still_dispatches_stage2_when_site_unknown() {
        let old = "fn print_name(name: String) { println!(\"{}\", name); }\n";
        let new = "fn print_name(name: String) { println!(\"{}\", name); }\nprint_name(person.name.clone());\n";
        let hunks = crate::diff::diff_lines(old, new);

        let stage1_fixture = fixture("stage1_response.json");
        let stage2_fixture = fixture("stage2_response_valid.json");

        let outcome = judge_hunks(
            "src/main.rs",
            &hunks,
            new,
            crate::pack::PackData {
                taxonomy: &taxonomy(),
                canon: &canon(),
                grammar: &grammar(),
                prompts: &prompts(),
            },
            |_fp| false, // nothing known yet
            |_prompt| Ok(stage1_fixture.clone()),
            |_prompt| Ok(stage2_fixture.clone()),
            no_file_reader,
        )
        .unwrap();

        assert!(!outcome.deduped);
        assert!(outcome.card.is_some());
    }

    // --- C6/D10 strict mode: second independent stage-2 sample ---

    #[test]
    fn test_judge_hunks_strict_mode_passes_when_second_sample_agrees() {
        let old = "fn maybe_get() -> Option<i32> { None }\n";
        let new = "fn maybe_get() -> Option<i32> { None }\nlet v = maybe_get().unwrap();\n";
        let hunks = crate::diff::diff_lines(old, new);

        let stage1_fixture =
            r#"[{"site_hint": "let v", "slugs": ["option-combinators"]}]"#.to_string();
        let stage2_fixture = r#"{
            "concept": "option-combinators",
            "grounding_quote": "maybe_get().unwrap()",
            "why": "unwrap panics on None.",
            "rule": "Use map/unwrap_or instead of unwrap.",
            "worked_diff": "- .unwrap()\n+ .unwrap_or(0)",
            "category": "bug",
            "likely_bug": true,
            "failure_scenario": "maybe_get() returns None here, so unwrap() panics at runtime."
        }"#
        .to_string();

        let call_count = std::cell::Cell::new(0);
        let outcome = judge_hunks(
            "src/main.rs",
            &hunks,
            new,
            crate::pack::PackData {
                taxonomy: &taxonomy(),
                canon: &canon(),
                grammar: &grammar(),
                prompts: &prompts(),
            },
            |_fp| false,
            |_prompt| Ok(stage1_fixture.clone()),
            |_prompt| {
                call_count.set(call_count.get() + 1);
                Ok(stage2_fixture.clone())
            },
            no_file_reader,
        )
        .unwrap();

        assert_eq!(
            call_count.get(),
            2,
            "likely_bug must trigger a second independent sample"
        );
        assert!(outcome.strict_mode_passed);
        assert!(outcome.card.is_some());
    }

    #[test]
    fn test_judge_hunks_strict_mode_fails_when_second_sample_disagrees() {
        let old = "fn maybe_get() -> Option<i32> { None }\n";
        let new = "fn maybe_get() -> Option<i32> { None }\nlet v = maybe_get().unwrap();\n";
        let hunks = crate::diff::diff_lines(old, new);

        let stage1_fixture =
            r#"[{"site_hint": "let v", "slugs": ["option-combinators"]}]"#.to_string();
        let bug_fixture = r#"{
            "concept": "option-combinators",
            "grounding_quote": "maybe_get().unwrap()",
            "why": "unwrap panics on None.",
            "rule": "Use map/unwrap_or instead of unwrap.",
            "worked_diff": "- .unwrap()\n+ .unwrap_or(0)",
            "category": "bug",
            "likely_bug": true,
            "failure_scenario": "maybe_get() returns None here, so unwrap() panics at runtime."
        }"#
        .to_string();
        let not_bug_fixture = r#"{
            "concept": "option-combinators",
            "grounding_quote": "maybe_get().unwrap()",
            "why": "unwrap panics on None.",
            "rule": "Use map/unwrap_or instead of unwrap.",
            "worked_diff": "- .unwrap()\n+ .unwrap_or(0)",
            "category": "idiom",
            "likely_bug": false
        }"#
        .to_string();

        let call_count = std::cell::Cell::new(0);
        let outcome = judge_hunks(
            "src/main.rs",
            &hunks,
            new,
            crate::pack::PackData {
                taxonomy: &taxonomy(),
                canon: &canon(),
                grammar: &grammar(),
                prompts: &prompts(),
            },
            |_fp| false,
            |_prompt| Ok(stage1_fixture.clone()),
            |_prompt| {
                let n = call_count.get();
                call_count.set(n + 1);
                if n == 0 {
                    Ok(bug_fixture.clone())
                } else {
                    Ok(not_bug_fixture.clone())
                }
            },
            no_file_reader,
        )
        .unwrap();

        assert_eq!(call_count.get(), 2);
        assert!(
            !outcome.strict_mode_passed,
            "disagreement must not be budget-exempt"
        );
        // The card still ships — disagreement only removes the bypass, per C6.
        assert!(outcome.card.is_some());
    }

    #[test]
    fn test_judge_hunks_no_strict_mode_dispatch_for_non_bug_card() {
        let old = "fn print_name(name: String) { println!(\"{}\", name); }\n";
        let new = "fn print_name(name: String) { println!(\"{}\", name); }\nprint_name(person.name.clone());\n";
        let hunks = crate::diff::diff_lines(old, new);

        let stage1_fixture = fixture("stage1_response.json");
        let stage2_fixture = fixture("stage2_response_valid.json"); // likely_bug: false

        let call_count = std::cell::Cell::new(0);
        let outcome = judge_hunks(
            "src/main.rs",
            &hunks,
            new,
            crate::pack::PackData {
                taxonomy: &taxonomy(),
                canon: &canon(),
                grammar: &grammar(),
                prompts: &prompts(),
            },
            |_fp| false,
            |_prompt| Ok(stage1_fixture.clone()),
            |_prompt| {
                call_count.set(call_count.get() + 1);
                Ok(stage2_fixture.clone())
            },
            no_file_reader,
        )
        .unwrap();

        assert_eq!(
            call_count.get(),
            1,
            "non-bug cards never trigger a second stage-2 sample"
        );
        assert!(!outcome.strict_mode_passed);
    }

    // --- T16a: judge-context budget assembly ---

    fn block(header: &str, text: &str) -> crate::site::ContextBlock {
        crate::site::ContextBlock {
            header: header.to_string(),
            text: text.to_string(),
        }
    }

    #[test]
    fn test_fill_context_budget_includes_everything_within_budget() {
        let (included, dropped) = fill_context_budget("enclosing text", &[block("a:1", "short")], 1_000);
        assert_eq!(included, vec![block("a:1", "short")]);
        assert!(dropped.is_empty());
    }

    /// "stop at budget": the first block that overflows, AND every block
    /// requested after it, is dropped — not a best-effort repack of a
    /// smaller later block into the leftover room.
    #[test]
    fn test_fill_context_budget_stops_at_first_over_budget_block() {
        // Budget = 10 tokens (~40 chars). The enclosing text alone costs
        // some of that; the first block is deliberately oversized, the
        // second would easily fit alone but must still be dropped.
        let enclosing = "x".repeat(20); // ~5 tokens
        let big = block("big:1", &"y".repeat(200)); // ~50 tokens: won't fit
        let small = block("small:1", "z"); // ~1 token: would fit if tried alone
        let (included, dropped) = fill_context_budget(&enclosing, &[big, small], 10);
        assert!(included.is_empty());
        assert_eq!(dropped, vec!["big:1".to_string(), "small:1".to_string()]);
    }

    #[test]
    fn test_fill_context_budget_reserves_enclosing_item_cost_first() {
        // The enclosing text alone already consumes the whole budget, so
        // even a tiny resolved block must be dropped.
        let enclosing = "x".repeat(400); // ~100 tokens
        let (included, dropped) = fill_context_budget(&enclosing, &[block("a:1", "z")], 100);
        assert!(included.is_empty());
        assert_eq!(dropped, vec!["a:1".to_string()]);
    }

    #[test]
    fn test_assemble_judge_context_with_no_blocks_is_byte_identical_to_enclosing_text() {
        let out = assemble_judge_context("fn foo() {}", &[]);
        assert_eq!(out, "fn foo() {}");
    }

    #[test]
    fn test_assemble_judge_context_appends_headers_and_text_in_order() {
        let out = assemble_judge_context(
            "fn foo() {}",
            &[block("a.rs:1", "struct A;"), block("b.rs:3", "struct B;")],
        );
        assert_eq!(
            out,
            "fn foo() {}\n\na.rs:1:\nstruct A;\n\nb.rs:3:\nstruct B;"
        );
    }

    // --- T16a: project_scoped_file_reader ---

    #[test]
    fn test_project_scoped_file_reader_reads_a_real_file_under_root() {
        let dir = std::env::temp_dir().join(format!(
            "murshid_t16a_scoped_reader_{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("other.rs"), "struct Other;\n").unwrap();
        let reader = project_scoped_file_reader(dir.clone());
        assert_eq!(reader("other.rs"), Some("struct Other;\n".to_string()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_project_scoped_file_reader_rejects_absolute_path() {
        let dir = std::env::temp_dir().join(format!(
            "murshid_t16a_scoped_reader_abs_{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let reader = project_scoped_file_reader(dir.clone());
        assert_eq!(reader("/etc/passwd"), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_project_scoped_file_reader_rejects_parent_dir_escape() {
        let dir = std::env::temp_dir().join(format!(
            "murshid_t16a_scoped_reader_escape_{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let reader = project_scoped_file_reader(dir.clone());
        assert_eq!(reader("../../etc/passwd"), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn test_project_scoped_file_reader_rejects_symlink_escaping_root() {
        // T16a gate blocker: an in-repo symlink pointing OUTSIDE the root has
        // no `..` and isn't absolute, so the lexical checks pass — the
        // canonicalize + containment check must still refuse it, else
        // out-of-root file contents would be shipped to the model provider.
        let base = std::env::temp_dir().join(format!(
            "murshid_t16a_scoped_reader_symlink_{}",
            std::process::id()
        ));
        let root = base.join("root");
        let outside = base.join("outside");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let secret = outside.join("secret.txt");
        std::fs::write(&secret, "TOP SECRET\n").unwrap();
        // A symlink INSIDE the root that points to the outside secret. The
        // reader is called with the relative in-root name — no `..`, not
        // absolute — so only canonicalization can catch the escape.
        std::os::unix::fs::symlink(&secret, root.join("link.txt")).unwrap();
        let reader = project_scoped_file_reader(root.clone());
        assert_eq!(reader("link.txt"), None, "must not follow a symlink out of root");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn test_project_scoped_file_reader_missing_file_is_none() {
        let dir = std::env::temp_dir().join(format!(
            "murshid_t16a_scoped_reader_missing_{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let reader = project_scoped_file_reader(dir.clone());
        assert_eq!(reader("nope.rs"), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // --- T16a: backward compatibility (hard invariant) ---

    /// A stage-1 response with NO `context_request` must produce a stage-2
    /// prompt byte-identical to what pre-T16a `judge_hunks` would have
    /// dispatched (plain enclosing item + canon, nothing appended), and
    /// `context_trace` must stay `None` — proving the "absent = today's
    /// exact behavior" invariant, not just asserting it.
    #[test]
    fn test_judge_hunks_backward_compatible_when_stage1_response_has_no_context_request() {
        let old = "fn print_name(name: String) { println!(\"{}\", name); }\n";
        let new = "fn print_name(name: String) { println!(\"{}\", name); }\nprint_name(person.name.clone());\n";
        let hunks = crate::diff::diff_lines(old, new);

        let stage1_fixture = fixture("stage1_response.json"); // predates T16a: no context_request
        let stage2_fixture = fixture("stage2_response_valid.json");
        let captured: std::rc::Rc<std::cell::RefCell<Option<String>>> =
            std::rc::Rc::new(std::cell::RefCell::new(None));
        let captured_inner = captured.clone();

        let outcome = judge_hunks(
            "src/main.rs",
            &hunks,
            new,
            crate::pack::PackData {
                taxonomy: &taxonomy(),
                canon: &canon(),
                grammar: &grammar(),
                prompts: &prompts(),
            },
            |_fp| false,
            |_prompt| Ok(stage1_fixture.clone()),
            |prompt: &str| {
                *captured_inner.borrow_mut() = Some(prompt.to_string());
                Ok(stage2_fixture.clone())
            },
            no_file_reader,
        )
        .unwrap();

        assert!(outcome.card.is_some());
        assert!(
            outcome.context_trace.is_none(),
            "no context_request => no trace at all, the backward-compat guarantee"
        );

        // Independently rebuild exactly what the pre-T16a pipeline would
        // have dispatched: the same `build_stage2_prompt` over the plain
        // enclosing-item fallback text, with zero extra blocks.
        let line = crate::diff::changed_line_numbers(&hunks)[0];
        let enclosing_text =
            crate::site::enclosing_item_text(new, line, &grammar()).unwrap_or_else(|| new.to_string());
        let expected_prompt = build_stage2_prompt(
            &prompts().stage2,
            &["borrow-vs-clone".to_string()],
            &enclosing_text,
            &canon(),
        );

        assert_eq!(captured.borrow().as_ref().unwrap(), &expected_prompt);
    }

    // --- T16a: grounding against the FULL provided (multi-block) context ---

    /// A grounding quote that lives ONLY inside a resolved `caller` block
    /// (not in the enclosing item itself) still grounds — the stronger
    /// guarantee: the judge can ground in the extra context it asked for.
    #[test]
    fn test_judge_hunks_grounds_quote_found_only_in_a_resolved_caller_block() {
        let old = "fn helper(x: i32) -> i32 {\n    x\n}\n\nfn caller() {\n    let y = helper(5);\n}\n";
        let new = "fn helper(x: i32) -> i32 {\n    x.clone()\n}\n\nfn caller() {\n    let y = helper(5);\n}\n";
        let hunks = crate::diff::diff_lines(old, new);

        let stage1_fixture = r#"[{"site_hint": "fn helper", "slugs": ["borrow-vs-clone"], "context_request": [{"kind": "caller", "name": "caller"}]}]"#.to_string();
        // This quote appears ONLY inside `fn caller`'s body, never inside
        // `fn helper`'s (the site's own enclosing item).
        let stage2_fixture = r#"{
            "concept": "borrow-vs-clone",
            "grounding_quote": "let y = helper(5);",
            "why": "why",
            "rule": "rule",
            "worked_diff": "diff",
            "category": "idiom",
            "likely_bug": false
        }"#
        .to_string();

        let outcome = judge_hunks(
            "src/main.rs",
            &hunks,
            new,
            crate::pack::PackData {
                taxonomy: &taxonomy(),
                canon: &canon(),
                grammar: &grammar(),
                prompts: &prompts(),
            },
            |_fp| false,
            |_prompt| Ok(stage1_fixture.clone()),
            |_prompt| Ok(stage2_fixture.clone()),
            no_file_reader,
        )
        .unwrap();

        assert!(
            outcome.drop_reason.is_none(),
            "expected a grounded card, got: {:?}",
            outcome.drop_reason
        );
        let card = outcome.card.expect("expected a card grounded in the resolved caller block");
        assert_eq!(card.grounding_quote, "let y = helper(5);");
        // Confirm the quote genuinely isn't in the site's own enclosing
        // item — the card only grounds because of the resolved block.
        let helper_text = crate::site::enclosing_item_text(new, 2, &grammar()).unwrap();
        assert!(!helper_text.contains(&card.grounding_quote));

        let trace = outcome.context_trace.expect("a context_request was made");
        assert_eq!(trace.fetched, vec!["src/main.rs:5".to_string()]);
        assert!(trace.dropped.is_empty());
    }

    /// The other side of the same guarantee: a quote that's in NEITHER the
    /// enclosing item NOR any resolved block still fails — the strengthened
    /// grounding check stays honest, it doesn't get looser.
    #[test]
    fn test_judge_hunks_drops_quote_grounded_in_no_provided_block() {
        let old = "fn helper(x: i32) -> i32 {\n    x\n}\n\nfn caller() {\n    let y = helper(5);\n}\n";
        let new = "fn helper(x: i32) -> i32 {\n    x.clone()\n}\n\nfn caller() {\n    let y = helper(5);\n}\n";
        let hunks = crate::diff::diff_lines(old, new);

        let stage1_fixture = r#"[{"site_hint": "fn helper", "slugs": ["borrow-vs-clone"], "context_request": [{"kind": "caller", "name": "caller"}]}]"#.to_string();
        let stage2_fixture = r#"{
            "concept": "borrow-vs-clone",
            "grounding_quote": "this text appears nowhere in the provided context",
            "why": "why",
            "rule": "rule",
            "worked_diff": "diff",
            "category": "idiom",
            "likely_bug": false
        }"#
        .to_string();

        let outcome = judge_hunks(
            "src/main.rs",
            &hunks,
            new,
            crate::pack::PackData {
                taxonomy: &taxonomy(),
                canon: &canon(),
                grammar: &grammar(),
                prompts: &prompts(),
            },
            |_fp| false,
            |_prompt| Ok(stage1_fixture.clone()),
            |_prompt| Ok(stage2_fixture.clone()),
            no_file_reader,
        )
        .unwrap();

        assert!(outcome.card.is_none());
        assert_eq!(
            outcome.drop_reason,
            Some(crate::judge::JudgeDropReason::UnverifiableQuote)
        );
    }

    /// An unresolvable context request (no such symbol) is skipped, never
    /// fabricated, and shows up in the trace's `dropped` list.
    #[test]
    fn test_judge_hunks_context_trace_records_unresolvable_request() {
        let old = "fn helper(x: i32) -> i32 {\n    x\n}\n";
        let new = "fn helper(x: i32) -> i32 {\n    x.clone()\n}\n";
        let hunks = crate::diff::diff_lines(old, new);

        let stage1_fixture = r#"[{"site_hint": "fn helper", "slugs": ["borrow-vs-clone"], "context_request": [{"kind": "caller", "name": "does_not_exist"}]}]"#.to_string();
        let stage2_fixture = fixture("stage2_response_valid.json");

        let outcome = judge_hunks(
            "src/main.rs",
            &hunks,
            new,
            crate::pack::PackData {
                taxonomy: &taxonomy(),
                canon: &canon(),
                grammar: &grammar(),
                prompts: &prompts(),
            },
            |_fp| false,
            |_prompt| Ok(stage1_fixture.clone()),
            |_prompt| Ok(stage2_fixture.clone()),
            no_file_reader,
        )
        .unwrap();

        let trace = outcome
            .context_trace
            .expect("a context_request was made, so a trace must exist even when unresolved");
        assert!(trace.fetched.is_empty());
        assert_eq!(trace.dropped.len(), 1);
        assert!(trace.dropped[0].1.contains("does_not_exist"));
    }
}
