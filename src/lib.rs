//! T13 req 3: the library crate. Extracted from `main.rs` so integration
//! tests under `tests/` can exercise the real pipeline (session diff ->
//! quiescence -> judge_hunks -> card persistence) through the crate's public
//! surface, instead of only via unit tests colocated inside `src/`.
//! `main.rs` stays a thin binary entry point on top of this.

pub mod aggregate;
pub mod backup;
pub mod bkt;
pub mod bookend;
pub mod budget;
pub mod card;
pub mod comment;
pub mod config;
pub mod consent;
pub mod credentials;
pub mod db;
pub mod diff;
pub mod goal;
pub mod judge;
pub mod ladder;
pub mod memory;
pub mod noise;
pub mod offer;
pub mod pack;
pub mod pipeline;
pub mod progress;
pub mod provider;
pub mod queue;
pub mod quiescence;
pub mod response;
pub mod retrieval;
pub mod review;
pub mod sanitizer;
pub mod session;
pub mod sha256;
pub mod site;
pub mod staleness;
pub mod struggle;
pub mod suppression;
pub mod sync_ext;
pub mod thread;
pub mod throttle;
pub mod watcher;
pub mod watcher_coordinator;

pub mod cli;
pub mod watch;

/// Resolves a model slot's key using the existing keyring/env flow (C6:
/// "Keys via existing keyring/env flow"). Ollama needs no key.
pub fn resolve_slot_key(provider: &str, keys: &Option<credentials::CachedKeys>) -> Option<String> {
    // The cache is keyed by Provider, so every keyed provider — including the
    // generic `openai` OpenAI-compatible endpoint (whose bearer auth was
    // previously dead because this returned None for it) — resolves uniformly.
    // Keyless locals (ollama/lmstudio) and unrecognized providers are simply
    // absent from the map.
    let provider = crate::provider::Provider::parse(provider)?;
    keys.as_ref()
        .and_then(|k| k.get(provider))
        .map(str::to_string)
}

/// A C6 model slot resolved to everything a dispatch needs: provider, model
/// id, the (optional) key from the keyring/env flow, and the (optional)
/// base-url override. Built once via [`ResolvedSlot::resolve`] instead of
/// threading the four fields by hand through every watch/review signature (the
/// bundle that collapses the `{screen,judge}_{provider,model,key,base_url}`
/// param storms and the `#[allow(clippy::too_many_arguments)]` that came with
/// them).
#[derive(Debug, Clone)]
pub struct ResolvedSlot {
    pub provider: String,
    pub model: String,
    pub key: Option<String>,
    pub base_url: Option<String>,
}

/// The two C6 slots — `screen` (fast/cheap) and `judge` (strong) — resolved
/// together.
#[derive(Debug, Clone)]
pub struct Models {
    pub screen: ResolvedSlot,
    pub judge: ResolvedSlot,
}

impl ResolvedSlot {
    /// Resolves a config slot into a dispatch-ready slot, attaching its key via
    /// the keyring/env flow ([`resolve_slot_key`]).
    pub fn resolve(slot: &config::ModelSlotConfig, keys: &Option<credentials::CachedKeys>) -> Self {
        ResolvedSlot {
            provider: slot.provider.clone(),
            model: slot.model.clone(),
            key: resolve_slot_key(&slot.provider, keys),
            base_url: slot.base_url.clone(),
        }
    }

    /// A config error that would otherwise only surface at first dispatch —
    /// today, a generic `openai` slot with no `base_url` (no alias default).
    /// `None` when the slot is dispatch-ready (ROADMAP item 7: fail fast at
    /// load).
    pub fn base_url_config_error(&self) -> Option<String> {
        provider::validate_slot_base_url(&self.provider, self.base_url.as_deref()).err()
    }

    /// Dispatches `prompt` for this slot on `lane`, folding the `safe_dispatch`
    /// panic guard and the `JudgeMode -> String` degraded-mode mapping that was
    /// copy-pasted at every call site.
    pub fn dispatch(&self, lane: provider::Lane, prompt: &str) -> Result<String, String> {
        judge::safe_dispatch(|| {
            provider::dispatch_debounced_with_model(
                lane,
                &self.provider,
                Some(&self.model),
                prompt,
                self.key.as_deref(),
                self.base_url.as_deref(),
            )
        })
        .map_err(|m| match m {
            judge::JudgeMode::Degraded { reason } => reason,
            judge::JudgeMode::Active => "degraded".to_string(),
        })
    }
}

impl Models {
    /// Resolves both slots from `[models]` config plus the cached keys.
    pub fn resolve(models: &config::ModelsConfig, keys: &Option<credentials::CachedKeys>) -> Self {
        Models {
            screen: ResolvedSlot::resolve(&models.screen, keys),
            judge: ResolvedSlot::resolve(&models.judge, keys),
        }
    }

    /// Per-slot config warnings to surface at load (ROADMAP item 7) — a
    /// misconfigured `base_url` here would otherwise stay silent until the
    /// first dispatch. Each entry is prefixed with the slot it came from.
    pub fn config_warnings(&self) -> Vec<String> {
        [("screen", &self.screen), ("judge", &self.judge)]
            .into_iter()
            .filter_map(|(label, slot)| {
                slot.base_url_config_error()
                    .map(|e| format!("[models.{label}] {e}"))
            })
            .collect()
    }
}

/// req 4: reads the current goal text fresh from disk (it can change
/// mid-session via `g`/hand-edit); empty when no goal is set yet.
pub fn goal_text_now(project_root: &std::path::Path) -> String {
    goal::read_goal_file(project_root)
        .map(|g| g.text)
        .unwrap_or_default()
}

/// T9 req 7: shared persistence for a solicited review digest (D18) — the
/// CLI `review` arm and the in-pane `r` key previously ran verbatim copies
/// of this `review_requested` event + EFP-exempt card-insert block.
pub fn persist_review_digest(
    conn: &rusqlite::Connection,
    session_id: &str,
    digest: &review::ReviewDigest,
) {
    db::warn_on_err(
        db::with_tx(conn, |tx| {
            db::log_event_stmt(
                tx,
                &db::EventRecord {
                    id: None,
                    session_id: session_id.to_string(),
                    kind: "review_requested".to_string(),
                    payload_json: serde_json::json!({
                        "top": digest.top.len(),
                        "more_queued": digest.more_queued,
                    })
                    .to_string(),
                    ts: None,
                },
            )?;
            // req 13: review cards are logged EFP-exempt (db::REVIEW_CATEGORY).
            for card in &digest.top {
                db::insert_card_stmt(
                    tx,
                    &db::CardRecord {
                        id: None,
                        session_id: session_id.to_string(),
                        concept_id: card.concept_name.clone(),
                        category: db::REVIEW_CATEGORY.to_string(),
                        // T5 review fix 4 / D18: static R2, not memory-driven —
                        // `murshid review` is a solicited, one-shot digest surface,
                        // outside the per-card C4 ladder flow.
                        rung_shown: ladder::Rung::R2.as_str().to_string(),
                        advice_fp: format!("review:{}:{}:{}", session_id, card.file, card.line),
                        finding_fp: None,
                        status: "shown".to_string(),
                        created_ts: None,
                        resolved_ts: None,
                        worked_diff: Some(card.worked_diff.clone()),
                        regresses_card_id: None,
                        site_file: Some(card.file.clone()),
                        site_line: Some(card.line as i64),
                    },
                )?;
            }
            Ok(())
        }),
        "log_event+insert_card(persist_review_digest)",
    );
}

/// T4 reqs 12-13 / D18: `murshid review`'s batched screen->judge pass over
/// every changed file in `snapshot`'s diff, ranked into a digest. Shared by
/// the CLI `review` subcommand and the in-pane `r` key — the only
/// difference between them is which snapshot/goal/canon/taxonomy/keys the
/// caller passes in.
#[allow(clippy::too_many_arguments)]
pub fn run_review(
    project_root: &std::path::Path,
    snapshot: &session::SessionSnapshot,
    taxonomy: &[pack::TaxonomyConcept],
    canon: &[pack::CanonEntry],
    grammar: &pack::GrammarSpec,
    prompts: &pack::PromptFragments,
    goal_text: &str,
    goal_cluster_dirs: &std::collections::HashSet<String>,
    models: &Models,
    conn: Option<&rusqlite::Connection>,
) -> review::ReviewDigest {
    // T5 support for T4 req 13 / D18: the real memory-derived below-mastery
    // list, wired in place of T4's static placeholder — falls back to it
    // only when no DB connection is available at all (never blocks the
    // review surface).
    let below_mastery_owned: Vec<String> = conn
        .map(|c| memory::below_mastery_concepts(c, taxonomy))
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| {
            review::BELOW_MASTERY_PLACEHOLDER
                .iter()
                .map(|s| s.to_string())
                .collect()
        });
    let below_mastery: Vec<&str> = below_mastery_owned.iter().map(String::as_str).collect();
    // T11 req 2/4: the review digest (CLI `review` and the in-pane `r` key)
    // is user-initiated — Interactive lane, never aborted by a concurrent
    // Sweep dispatch.
    let dispatch_stage1 =
        |prompt: &str| models.screen.dispatch(provider::Lane::Interactive, prompt);
    let dispatch_stage2 = |prompt: &str| models.judge.dispatch(provider::Lane::Interactive, prompt);

    let changed_files = session::tracked_and_modified_files(project_root).unwrap_or_default();
    let mut findings = Vec::new();
    for rel in changed_files {
        let hunks = match session::compute_session_diff(project_root, &rel, snapshot) {
            Ok(h) => h,
            Err(_) => continue,
        };
        if hunks.is_empty() {
            continue;
        }
        let abs = project_root.join(&rel);
        let Ok(content) = std::fs::read_to_string(&abs) else {
            continue;
        };
        if !site::parses_without_errors(&content, grammar) {
            continue;
        }
        let rel_str = rel.to_string_lossy().to_string();
        findings.extend(review::judge_review_hunks(
            &rel_str,
            &hunks,
            &content,
            taxonomy,
            canon,
            grammar,
            &prompts.stage1,
            goal_text,
            &below_mastery,
            dispatch_stage1,
            dispatch_stage2,
        ));
    }

    review::rank_and_digest(findings, goal_cluster_dirs, goal_text)
}
