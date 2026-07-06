//! T13 req 3: the first real integration-test target — one end-to-end pass
//! through the watch pipeline's core spine: a save lands on disk, the
//! session diff picks it up, the quiescence gate clears it, `judge_hunks`
//! (dispatched against a fixture response, never a live call) produces a
//! card, and that card is persisted to a real (file-backed) database. Every
//! piece here is exercised individually elsewhere as a unit test; this test
//! is the one place they're wired together end to end.
//!
//! This lives in `tests/` (rather than `#[cfg(test)] mod tests` inside
//! `src/`) because it exercises the crate through its public surface, the
//! same way an external caller would — `src/lib.rs` exists specifically so
//! this is possible for a binary-only crate (see T13 req 3's transport-seam
//! note in `src/provider.rs` for the sibling half of this requirement).

use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, SystemTime};

fn run_git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git must be on PATH for this test");
    assert!(
        status.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&status.stderr)
    );
}

fn init_git_repo(dir: &Path) {
    run_git(dir, &["init", "-q"]);
    run_git(
        dir,
        &[
            "-c",
            "user.email=test@test.com",
            "-c",
            "user.name=Test",
            "commit",
            "--allow-empty",
            "-q",
            "-m",
            "init",
        ],
    );
}

fn commit_all(dir: &Path, msg: &str) {
    run_git(dir, &["add", "-A"]);
    run_git(
        dir,
        &[
            "-c",
            "user.email=test@test.com",
            "-c",
            "user.name=Test",
            "commit",
            "-q",
            "-m",
            msg,
        ],
    );
}

fn fixture(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading fixture {}: {}", name, e))
}

#[test]
fn test_save_diff_quiescence_judge_hunks_fixture_dispatch_card_persisted() {
    // --- setup: a real git repo on disk, a clean baseline file ---
    let project_root = std::env::temp_dir().join("murshid_test_watch_pipeline_e2e");
    let _ = fs::remove_dir_all(&project_root);
    fs::create_dir_all(project_root.join("src")).unwrap();
    init_git_repo(&project_root);

    // The flagged call must sit INSIDE an enclosing item (`fn main`) — a
    // top-level statement has no enclosing item per the grammar (see
    // site::test_no_enclosing_item_at_top_level), so `site::compute_site`
    // would have nothing to resolve later in this test.
    let rel_file = "src/main.rs";
    let baseline = "fn print_name(name: String) { println!(\"{}\", name); }\n\nfn main() {\n    let person = Person { name: String::from(\"Ada\") };\n}\n";
    fs::write(project_root.join(rel_file), baseline).unwrap();
    commit_all(&project_root, "add print_name");

    // C2 session-start snapshot — the tree is clean at this point, so the
    // diff baseline resolves to HEAD (murshid::session::baseline_content).
    let snapshot = murshid::session::snapshot_session_start(&project_root).unwrap();
    let last_event_at = SystemTime::now();

    // --- save: the founder introduces an avoidable clone inside `fn main` ---
    let after_save = "fn print_name(name: String) { println!(\"{}\", name); }\n\nfn main() {\n    let person = Person { name: String::from(\"Ada\") };\n    print_name(person.name.clone());\n}\n";
    fs::write(project_root.join(rel_file), after_save).unwrap();

    // --- diff: the session-diff hunks against the resolved baseline ---
    let hunks =
        murshid::session::compute_session_diff(&project_root, Path::new(rel_file), &snapshot)
            .unwrap();
    assert!(!hunks.is_empty(), "the save must produce at least one hunk");

    // --- quiescence: a save, a pause, and a clean parse ---
    let grammar = murshid::pack::GrammarSpec::default();
    let now = last_event_at + Duration::from_secs(3); // past QUIESCENCE_PAUSE (2s)
    assert!(
        murshid::quiescence::should_judge(last_event_at, now, after_save, &grammar),
        "a clean parse after the pause must clear the quiescence gate"
    );

    // --- judge_hunks: fixture dispatch, never a live provider call ---
    let taxonomy = murshid::pack::load_taxonomy(&murshid::pack::default_pack_dir()).unwrap();
    let canon = murshid::pack::load_canon(&murshid::pack::default_pack_dir()).unwrap();
    let prompts = murshid::pack::load_prompt_fragments(&murshid::pack::default_pack_dir()).unwrap();

    let stage1_fixture = fixture("stage1_response.json");
    let stage2_fixture = fixture("stage2_response_valid.json");

    let outcome = murshid::pipeline::judge_hunks(
        rel_file,
        &hunks,
        after_save,
        murshid::pack::PackData {
            taxonomy: &taxonomy,
            canon: &canon,
            grammar: &grammar,
            prompts: &prompts,
        },
        |_advice_fp| false, // nothing judged yet this session
        |_prompt| Ok(stage1_fixture.clone()),
        |_prompt| Ok(stage2_fixture.clone()),
        |_path: &str| None, // T16a: no cross-file context request exercised here
    )
    .unwrap();

    assert!(
        outcome.drop_reason.is_none(),
        "the fixture response must validate cleanly"
    );
    let card = outcome
        .card
        .clone()
        .expect("expected a card from the fixture dispatch");
    let stage2 = outcome
        .stage2
        .expect("expected the stage2 leg alongside the card");
    assert_eq!(card.concept_name, "Borrow vs. clone");
    assert!(after_save.contains(&card.grounding_quote));

    // --- card persisted: a real (file-backed) database, not :memory: ---
    let db_path = std::env::temp_dir().join("murshid_test_watch_pipeline_e2e.db");
    let _ = fs::remove_file(&db_path);
    let conn = murshid::db::initialize_db(&db_path).unwrap();

    let site = murshid::site::compute_site(rel_file, after_save, card.line, &grammar)
        .expect("the flagged line must resolve to a site");
    let advice_fp = murshid::site::advice_fingerprint(&stage2.concept, &site);

    let session_id = "sess-e2e";
    let card_id = murshid::db::insert_card(
        &conn,
        &murshid::db::CardRecord {
            id: None,
            session_id: session_id.to_string(),
            concept_id: stage2.concept.clone(),
            category: stage2.category.clone(),
            rung_shown: "R2".to_string(),
            advice_fp: advice_fp.clone(),
            finding_fp: None,
            status: "shown".to_string(),
            created_ts: None,
            resolved_ts: None,
            worked_diff: Some(card.worked_diff.clone()),
            regresses_card_id: None,
            site_file: Some(card.file.clone()),
            site_line: Some(card.line as i64),
            card_body_json: murshid::db::card_body_json(&card, &stage2.category),
        },
    )
    .unwrap();

    // Persistence actually landed — read it back via a fresh query (not
    // just trusting the returned id), the same way db.rs's own tests do.
    let (persisted_concept, persisted_status, persisted_fp): (String, String, String) = conn
        .query_row(
            "SELECT concept_id, status, advice_fp FROM cards WHERE id = ?1",
            rusqlite::params![card_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(persisted_concept, "borrow-vs-clone");
    assert_eq!(persisted_status, "shown");
    assert_eq!(persisted_fp, advice_fp);

    let (site_file, site_line) = murshid::db::card_site(&conn, card_id).unwrap().unwrap();
    assert_eq!(site_file, rel_file);
    assert_eq!(site_line, card.line as i64);

    let _ = fs::remove_dir_all(&project_root);
    drop(conn);
    let _ = fs::remove_file(&db_path);
}
