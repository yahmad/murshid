//! `murshid review` — the solicited, whole-diff review digest (D18). Runs
//! the two-stage judge over every changed file, ranks findings, and prints
//! a capped digest. EFP-exempt: it is user-initiated, outside the noise
//! budget.

use crate::{config, consent, credentials, db, goal, judge, offer, pack, review, session};

/// T4 req 12 / D18: `murshid review` — a standalone invocation has no live
/// watcher session, so the "session diff" is the full working tree vs HEAD
/// (C2's fallback baseline path, same one `session::baseline_content` uses
/// for a file that predates a live snapshot).
pub fn run(args: &[String]) {
    let project_root = if args.len() > 2 {
        std::path::PathBuf::from(&args[2])
    } else {
        std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
    };
    let project_root = match project_root.canonicalize() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Error: Invalid project path: {}", e);
            std::process::exit(1);
        }
    };

    // T10 req 1: resolved dynamically (config override ->
    // project marker -> rust fallback), same seam every other
    // pack-loading command uses.
    let cfg = config::load_config();
    let pack_dir = pack::resolve_pack_dir(&project_root, &cfg);
    let taxonomy = pack::load_or_notice(pack::load_taxonomy(&pack_dir), "taxonomy", &pack_dir);
    let canon = pack::load_or_notice(pack::load_canon(&pack_dir), "canon", &pack_dir);
    let grammar = pack::load_or_notice(pack::load_grammar(&pack_dir), "grammar", &pack_dir);
    let prompts =
        pack::load_or_notice(pack::load_prompt_fragments(&pack_dir), "prompts", &pack_dir);
    let keys = credentials::get_api_keys();
    let screen_provider = cfg.models.screen.provider.clone();
    let screen_model = cfg.models.screen.model.clone();
    let screen_key = crate::resolve_slot_key(&screen_provider, &keys);
    let screen_base_url = cfg.models.screen.base_url.clone();
    let judge_provider = cfg.models.judge.provider.clone();
    let judge_model = cfg.models.judge.model.clone();
    let judge_key = crate::resolve_slot_key(&judge_provider, &keys);
    let judge_base_url = cfg.models.judge.base_url.clone();

    let mode = judge::determine_judge_mode(
        &screen_provider,
        screen_key.as_deref(),
        &judge_provider,
        judge_key.as_deref(),
    );
    if let judge::JudgeMode::Degraded { ref reason } = mode {
        println!("{}", judge::degraded_status_line(reason));
        std::process::exit(1);
    }

    // C6 BYOK consent: every review invocation prompts under `ask`.
    if consent::should_prompt_for_review(&cfg.consent.solicited_spend) {
        let estimate = 500; // a full-diff pass is the expensive call
        println!(
            "{}",
            consent::consent_prompt_line("murshid review", estimate)
        );
        use std::io::BufRead;
        let mut answer = String::new();
        let _ = std::io::stdin().lock().read_line(&mut answer);
        if offer::classify_offer_key(answer.trim()) != offer::OfferKeyAction::Accept {
            println!("okay, skipped");
            std::process::exit(0);
        }
    }

    let goal_text = crate::goal_text_now(&project_root);
    let changed_files: Vec<std::path::PathBuf> =
        session::tracked_and_modified_files(&project_root).unwrap_or_default();
    let goal_cluster_dirs = goal::cluster_dirs_from_files(&changed_files);

    let review_conn = db::get_db_path().and_then(|dp| db::open_connection(&dp).ok());
    let digest = crate::run_review(
        &project_root,
        &session::SessionSnapshot::default(),
        &taxonomy,
        &canon,
        &grammar,
        &prompts,
        &goal_text,
        &goal_cluster_dirs,
        &screen_provider,
        &screen_model,
        screen_key.as_deref(),
        screen_base_url.as_deref(),
        &judge_provider,
        &judge_model,
        judge_key.as_deref(),
        judge_base_url.as_deref(),
        review_conn.as_ref(),
    );

    if let Some(conn) = review_conn.as_ref() {
        let sid = session::generate_session_id();
        crate::persist_review_digest(conn, &sid, &digest);
    }

    print!("{}", review::render_review_digest(&digest));
    std::process::exit(0);
}
