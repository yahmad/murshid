//! `murshid review` — the solicited, whole-diff review digest (D18). Runs
//! the two-stage judge over every changed file, ranks findings, and prints
//! a capped digest. EFP-exempt: it is user-initiated, outside the noise
//! budget.

use crate::{config, consent, credentials, db, goal, judge, pack, review, session};

/// T4 req 12 / D18: the three-way pre-flight gate for `murshid review` —
/// a degraded judge mode wins outright (nothing useful to review, so it's
/// checked first, ahead of any consent prompt); otherwise an `ask`-consent
/// prompt (C6) gates on the parsed y/N answer. Pure and IO-free (the
/// caller does the stdin read and passes the raw answer in) so this branch
/// can be unit-tested directly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReviewGate {
    Degraded(String),
    ConsentDeclined,
    Proceed,
}

pub fn decide_review_gate(
    mode: &judge::JudgeMode,
    should_prompt: bool,
    answer: &str,
) -> ReviewGate {
    if let judge::JudgeMode::Degraded { reason } = mode {
        return ReviewGate::Degraded(reason.clone());
    }
    if should_prompt && consent::classify_yes_no_key(answer.trim()) != consent::YesNoAction::Yes {
        return ReviewGate::ConsentDeclined;
    }
    ReviewGate::Proceed
}

/// T4 req 12 / D18: a standalone invocation has no live watcher session, so
/// the "session diff" is the full working tree vs HEAD (C2's fallback
/// baseline path, same one `session::baseline_content` uses for a file that
/// predates a live snapshot).
///
/// Returns instead of calling `std::process::exit` (`cli::dispatch` is the
/// single exit point): `Ok(())` for exit 0, `Err(code)` otherwise.
pub fn run(args: &[String]) -> Result<(), i32> {
    let project_root = crate::cli::project_root_from_args(args);
    let project_root = match project_root.canonicalize() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Error: Invalid project path: {}", e);
            return Err(1);
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
    let models = crate::Models::resolve(&cfg.models, &keys);
    for warning in models.config_warnings() {
        eprintln!("[WARNING] model slot misconfigured: {warning}");
    }

    let mode = judge::determine_judge_mode(
        &models.screen.provider,
        judge::KeyStatus::resolve(models.screen.key.as_deref(), models.screen.key_unreadable),
        &models.judge.provider,
        judge::KeyStatus::resolve(models.judge.key.as_deref(), models.judge.key_unreadable),
    );

    // C6 BYOK consent: every review invocation prompts under `ask`. Only
    // read stdin when there's an actual prompt to answer (degraded mode
    // exits before ever reaching this, same as before the refactor).
    let should_prompt = consent::should_prompt_for_review(&cfg.consent.solicited_spend);
    let degraded = matches!(mode, judge::JudgeMode::Degraded { .. });
    let mut answer = String::new();
    if !degraded && should_prompt {
        let estimate = 500; // a full-diff pass is the expensive call
        println!(
            "{}",
            consent::consent_prompt_line("murshid review", estimate)
        );
        use std::io::BufRead;
        let _ = std::io::stdin().lock().read_line(&mut answer);
    }

    match decide_review_gate(&mode, should_prompt, &answer) {
        ReviewGate::Degraded(reason) => {
            println!("{}", judge::degraded_status_line(&reason));
            return Err(1);
        }
        ReviewGate::ConsentDeclined => {
            println!("okay, skipped");
            return Ok(());
        }
        ReviewGate::Proceed => {}
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
        &models,
        review_conn.as_ref(),
    );

    if let Some(conn) = review_conn.as_ref() {
        let sid = session::generate_session_id();
        crate::persist_review_digest(conn, &sid, &digest);
    }

    print!("{}", review::render_review_digest(&digest));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn active() -> judge::JudgeMode {
        judge::JudgeMode::Active
    }

    fn degraded(reason: &str) -> judge::JudgeMode {
        judge::JudgeMode::Degraded {
            reason: reason.to_string(),
        }
    }

    #[test]
    fn degraded_wins_regardless_of_prompt_or_answer() {
        assert_eq!(
            decide_review_gate(&degraded("no key"), true, "y"),
            ReviewGate::Degraded("no key".to_string())
        );
        assert_eq!(
            decide_review_gate(&degraded("no key"), false, ""),
            ReviewGate::Degraded("no key".to_string())
        );
    }

    #[test]
    fn consent_declined_when_prompted_and_answer_is_not_accept() {
        assert_eq!(
            decide_review_gate(&active(), true, "n"),
            ReviewGate::ConsentDeclined
        );
        assert_eq!(
            decide_review_gate(&active(), true, ""),
            ReviewGate::ConsentDeclined
        );
        assert_eq!(
            decide_review_gate(&active(), true, "garbage"),
            ReviewGate::ConsentDeclined
        );
    }

    #[test]
    fn proceeds_when_accepted_or_no_prompt_needed() {
        assert_eq!(
            decide_review_gate(&active(), true, "y"),
            ReviewGate::Proceed
        );
        assert_eq!(
            decide_review_gate(&active(), false, ""),
            ReviewGate::Proceed
        );
    }
}
