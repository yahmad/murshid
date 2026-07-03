pub mod backup;
pub mod budget;
pub mod card;
pub mod compiler;
pub mod config;
pub mod context;
pub mod credentials;
pub mod db;
pub mod diff;
pub mod judge;
pub mod pack;
pub mod pipeline;
pub mod provider;
pub mod quiescence;
pub mod sanitizer;
pub mod session;
pub mod sha256;
pub mod site;
pub mod watcher;
pub mod watcher_coordinator;

#[path = "cli/goal.rs"]
pub mod cli_goal;
#[path = "cli/setup.rs"]
pub mod cli_setup;

fn print_usage() {
    println!("Murshid — Local-First Socratic AI Coding Mentor");
    println!("\nUsage:");
    println!("  murshid <command> [args]");
    println!("\nCommands:");
    println!(
        "  setup [path]                           Onboard a new project (auto-adds .murshid/ to .gitignore and parses .env)"
    );
    println!(
        "  watch [path]                           Watch a directory for code updates to trigger Socratic mentor feedback"
    );
    println!(
        "  goal <command> [args]                  Manage project active goals (set, get, complete, list)"
    );
}

/// Selects a provider + key using the existing keyring/env flow (C6: "Keys
/// via existing keyring/env flow"). T1 note: the pack's `[models] screen` /
/// `judge` two-slot config (C6) is not yet parsed by config.rs — both stages
/// reuse this single selection for now; the model-seam signature already
/// takes independent screen/judge keys so a later config extension plugs in
/// without further pipeline changes.
fn select_provider_and_key() -> (String, Option<String>) {
    let api_keys = credentials::get_api_keys();
    let provider_type = if std::env::var("GEMINI_API_KEY").is_ok()
        || std::env::var("MURSHID_GEMINI_API_KEY").is_ok()
    {
        "gemini"
    } else if std::env::var("ANTHROPIC_API_KEY").is_ok()
        || api_keys
            .as_ref()
            .and_then(|k| k.claude_api_key.as_ref())
            .is_some()
    {
        "claude"
    } else {
        "gemini"
    };
    let api_key = if provider_type == "claude" {
        api_keys.as_ref().and_then(|k| k.claude_api_key.clone())
    } else {
        api_keys.as_ref().and_then(|k| k.gemini_api_key.clone())
    };
    (provider_type.to_string(), api_key)
}

fn main() {
    let _cfg = config::load_config();
    if let Some(db_path) = db::get_db_path() {
        if let Err(e) = db::initialize_db(&db_path) {
            eprintln!(
                "[WARNING] Failed to initialize database at {}: {}",
                db_path.display(),
                e
            );
        }
    }

    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 {
        match args[1].as_str() {
            "setup" => {
                let project_root = if args.len() > 2 {
                    std::path::PathBuf::from(&args[2])
                } else {
                    std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
                };
                match cli_setup::run_setup(&project_root) {
                    Ok(_) => {
                        println!(
                            "Setup completed successfully for {}",
                            project_root.display()
                        );
                        std::process::exit(0);
                    }
                    Err(e) => {
                        eprintln!("Setup failed: {}", e);
                        std::process::exit(1);
                    }
                }
            }
            "goal" => {
                let db_path = match db::get_db_path() {
                    Some(p) => p,
                    None => {
                        eprintln!("Error: Database path not found");
                        std::process::exit(1);
                    }
                };
                let conn = match db::open_connection(&db_path) {
                    Ok(c) => c,
                    Err(e) => {
                        eprintln!("Failed to open database: {}", e);
                        std::process::exit(1);
                    }
                };
                match cli_goal::run_goal_cli(&conn, &args[2..]) {
                    Ok(_) => {
                        std::process::exit(0);
                    }
                    Err(e) => {
                        eprintln!("Goal command failed: {}", e);
                        std::process::exit(1);
                    }
                }
            }
            "watch" => {
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

                println!("Starting Murshid watcher on {}...", project_root.display());

                // --- T1 vertical slice state (C2 session, C12 budget) ---
                let taxonomy = pack::load_taxonomy(&pack::default_pack_dir()).unwrap_or_default();
                let canon = pack::load_canon(&pack::default_pack_dir()).unwrap_or_default();

                let now0 = std::time::SystemTime::now();
                let session_mgr =
                    std::sync::Arc::new(std::sync::Mutex::new(session::SessionManager::new(now0)));
                let snapshot = std::sync::Arc::new(std::sync::Mutex::new(
                    session::snapshot_session_start(&project_root).unwrap_or_default(),
                ));
                let bucket =
                    std::sync::Arc::new(std::sync::Mutex::new(budget::TokenBucket::standard(now0)));
                let last_event_at = std::sync::Arc::new(std::sync::Mutex::new(now0));

                {
                    let sid = session_mgr.lock().unwrap().session_id.clone();
                    if let Some(dp) = db::get_db_path() {
                        if let Ok(conn) = db::open_connection(&dp) {
                            let _ = db::log_event(
                                &conn,
                                &db::EventRecord {
                                    id: None,
                                    session_id: sid,
                                    kind: "session_start".to_string(),
                                    payload_json: "{}".to_string(),
                                    ts: None,
                                },
                            );
                        }
                    }
                }

                let (provider_type, api_key) = select_provider_and_key();
                // C6 degraded mode: no key for the model seam -> observe-only.
                let mode = judge::determine_judge_mode(api_key.as_deref(), api_key.as_deref());
                if let judge::JudgeMode::Degraded { ref reason } = mode {
                    println!("{}", judge::degraded_status_line(reason));
                }

                let project_root_cb = project_root.clone();

                let _watcher = match watcher::start_watching(project_root.clone(), move |path| {
                    println!("File saved: {}", path.display());
                    let db_path = db::get_db_path();
                    let conn_opt = db_path.as_ref().and_then(|dp| db::open_connection(dp).ok());
                    let project_root_str = project_root_cb.to_string_lossy().to_string();
                    let file_path_str = path.to_string_lossy().to_string();

                    if let Some(ref conn) = conn_opt {
                        let edit_event = db::HistoryEvent {
                            id: None,
                            event_type: "file_edit".to_string(),
                            project_root: project_root_str.clone(),
                            file_path: file_path_str.clone(),
                            success: None,
                            error_code: None,
                            error_message: None,
                            line_number: None,
                            created_at: None,
                        };
                        let _ = db::log_history_event(conn, &edit_event);
                    }

                    let now = std::time::SystemTime::now();
                    *last_event_at.lock().unwrap() = now;

                    // C2 session split on idle gap > 4h; re-snapshot on a new session.
                    let split = session_mgr.lock().unwrap().on_file_event(now);
                    let session_id_now = session_mgr.lock().unwrap().session_id.clone();
                    if split {
                        *snapshot.lock().unwrap() =
                            session::snapshot_session_start(&project_root_cb).unwrap_or_default();
                        if let Some(ref conn) = conn_opt {
                            let _ = db::log_event(
                                conn,
                                &db::EventRecord {
                                    id: None,
                                    session_id: session_id_now.clone(),
                                    kind: "session_start".to_string(),
                                    payload_json: "{}".to_string(),
                                    ts: None,
                                },
                            );
                        }
                    }

                    // D8/C12 quiescence gate: wait out the pause, then bail if a
                    // newer file event superseded this one (its own timer will
                    // handle the latest state).
                    std::thread::sleep(
                        quiescence::QUIESCENCE_PAUSE + std::time::Duration::from_millis(100),
                    );
                    if *last_event_at.lock().unwrap() != now {
                        return;
                    }

                    let content = match std::fs::read_to_string(&path) {
                        Ok(c) => c,
                        Err(_) => return,
                    };
                    if !site::parses_without_errors(&content) {
                        return; // parse errors: wait (D8)
                    }

                    // cargo check (D3 supporting signal / catch-up sweep trigger);
                    // diagnostics stay visible as plain lines in both modes (C6
                    // degraded-mode requirement).
                    let interceptor = compiler::CompilerInterceptor::new();
                    if let Ok(output) = interceptor.run_check(&project_root_cb, &path) {
                        if let Some(ref conn) = conn_opt {
                            let check_event = db::HistoryEvent {
                                id: None,
                                event_type: "compiler_check".to_string(),
                                project_root: project_root_str.clone(),
                                file_path: file_path_str.clone(),
                                success: Some(output.success),
                                error_code: None,
                                error_message: None,
                                line_number: None,
                                created_at: None,
                            };
                            let _ = db::log_history_event(conn, &check_event);
                        }
                        for diag in &output.diagnostics {
                            let code_str =
                                diag.code.clone().unwrap_or_else(|| "unknown".to_string());
                            let line_num = diag.spans.first().map(|s| s.line_start).unwrap_or(1);
                            println!(
                                "[ERROR {}] in {} at line {}",
                                code_str, file_path_str, line_num
                            );
                            println!("Message: {}", diag.message);
                        }
                    }

                    let rel_path = path
                        .strip_prefix(&project_root_cb)
                        .unwrap_or(path.as_path())
                        .to_path_buf();
                    let snap = snapshot.lock().unwrap().clone();
                    let hunks =
                        match session::compute_session_diff(&project_root_cb, &rel_path, &snap) {
                            Ok(h) => h,
                            Err(_) => return,
                        };
                    if hunks.is_empty() {
                        return;
                    }

                    if let judge::JudgeMode::Degraded { .. } = &mode {
                        // Observe-only: events above are already recorded; no LLM call.
                        return;
                    }

                    let rel_str = rel_path.to_string_lossy().to_string();
                    let dispatch_stage1 = |prompt: &str| -> Result<String, String> {
                        judge::safe_dispatch(|| {
                            provider::dispatch_debounced(&provider_type, prompt, api_key.as_deref())
                        })
                        .map_err(|m| match m {
                            judge::JudgeMode::Degraded { reason } => reason,
                            judge::JudgeMode::Active => "degraded".to_string(),
                        })
                    };
                    let dispatch_stage2 = |prompt: &str| -> Result<String, String> {
                        judge::safe_dispatch(|| {
                            provider::dispatch_debounced(&provider_type, prompt, api_key.as_deref())
                        })
                        .map_err(|m| match m {
                            judge::JudgeMode::Degraded { reason } => reason,
                            judge::JudgeMode::Active => "degraded".to_string(),
                        })
                    };

                    let outcome = pipeline::judge_hunks(
                        &rel_str,
                        &hunks,
                        &content,
                        &taxonomy,
                        &canon,
                        dispatch_stage1,
                        dispatch_stage2,
                    );

                    match outcome {
                        Ok(o) => {
                            if let Some(reason) = &o.drop_reason {
                                if let Some(ref conn) = conn_opt {
                                    let payload = judge::judge_drop_payload(reason, &rel_str);
                                    let _ = db::log_event(
                                        conn,
                                        &db::EventRecord {
                                            id: None,
                                            session_id: session_id_now.clone(),
                                            kind: "judge_drop".to_string(),
                                            payload_json: payload.to_string(),
                                            ts: None,
                                        },
                                    );
                                }
                            }

                            if let (Some(card), Some(stage2)) = (o.card, o.stage2) {
                                if let Some(site) =
                                    site::compute_site(&rel_str, &content, card.line)
                                {
                                    let advice_fp =
                                        site::advice_fingerprint(&stage2.concept, &site);
                                    let already_shown = conn_opt
                                        .as_ref()
                                        .map(|c| {
                                            db::card_exists_with_advice_fp(
                                                c,
                                                &session_id_now,
                                                &advice_fp,
                                            )
                                            .unwrap_or(false)
                                        })
                                        .unwrap_or(false);

                                    if !already_shown {
                                        let candidate = budget::PushCandidate {
                                            likely_bug: stage2.likely_bug,
                                            strict_mode_passed: false,
                                        };
                                        let decision = {
                                            let mut b = bucket.lock().unwrap();
                                            budget::decide_push(&mut b, &candidate, now)
                                        };
                                        match decision {
                                            budget::PushDecision::Shown => {
                                                println!("{}", card::render_card(&card, 0));
                                                if let Some(ref conn) = conn_opt {
                                                    let _ = db::insert_card(
                                                        conn,
                                                        &db::CardRecord {
                                                            id: None,
                                                            session_id: session_id_now.clone(),
                                                            concept_id: stage2.concept.clone(),
                                                            category: stage2.category.clone(),
                                                            rung_shown: "R2".to_string(),
                                                            advice_fp,
                                                            finding_fp: None,
                                                            status: "shown".to_string(),
                                                            created_ts: None,
                                                            resolved_ts: None,
                                                        },
                                                    );
                                                    let _ = db::log_event(
                                                        conn,
                                                        &db::EventRecord {
                                                            id: None,
                                                            session_id: session_id_now.clone(),
                                                            kind: "card_shown".to_string(),
                                                            payload_json: serde_json::json!({"concept": stage2.concept})
                                                                .to_string(),
                                                            ts: None,
                                                        },
                                                    );
                                                }
                                            }
                                            budget::PushDecision::Queued => {
                                                println!("  1 more queued \u{2014} T2");
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            eprintln!("[WARNING] Judge pipeline error: {}", e);
                        }
                    }
                }) {
                    Ok(w) => w,
                    Err(e) => {
                        eprintln!("Failed to start watcher: {}", e);
                        std::process::exit(1);
                    }
                };

                // Keep watcher running
                loop {
                    std::thread::sleep(std::time::Duration::from_secs(60));
                }
            }
            _ => {
                print_usage();
                std::process::exit(1);
            }
        }
    } else {
        print_usage();
        std::process::exit(1);
    }
}
