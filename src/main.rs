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
pub mod response;
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

/// Resolves a model slot's key using the existing keyring/env flow (C6:
/// "Keys via existing keyring/env flow"). Ollama needs no key.
fn resolve_slot_key(provider: &str, keys: &Option<credentials::CachedKeys>) -> Option<String> {
    match provider {
        "claude" => keys.as_ref().and_then(|k| k.claude_api_key.clone()),
        "gemini" => keys.as_ref().and_then(|k| k.gemini_api_key.clone()),
        _ => None, // ollama (local) and anything else: no key
    }
}

/// State pinned to the single card currently on screen (T1: at most one),
/// awaiting a `g`/`u`/`n` response (req 10).
#[derive(Clone)]
struct PendingCard {
    card_id: i64,
    session_id: String,
    concept_id: String,
}

type ShutdownCleanup = std::sync::Mutex<Option<Box<dyn Fn() + Send>>>;
static SHUTDOWN_CLEANUP: std::sync::OnceLock<ShutdownCleanup> = std::sync::OnceLock::new();

#[cfg(unix)]
fn setup_sigint_handler() {
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| unsafe {
        libc::signal(libc::SIGINT, sigint_handler as usize);
    });
}

#[cfg(unix)]
extern "C" fn sigint_handler(_sig: libc::c_int) {
    std::thread::spawn(|| {
        if let Some(mutex) = SHUTDOWN_CLEANUP.get() {
            if let Ok(guard) = mutex.lock() {
                if let Some(ref cleanup) = *guard {
                    cleanup();
                }
            }
        }
        std::process::exit(0);
    });
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
                // req 4/req 6: files touched since they were last swept/judged.
                let pending_files: std::sync::Arc<
                    std::sync::Mutex<std::collections::HashSet<std::path::PathBuf>>,
                > = std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashSet::new()));
                // req 10: the single on-screen card awaiting a response.
                let pending_card: std::sync::Arc<std::sync::Mutex<Option<PendingCard>>> =
                    std::sync::Arc::new(std::sync::Mutex::new(None));

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

                // req 5: two-slot [models] config (screen cheap/fast, judge strong).
                let cfg = config::load_config();
                let keys = credentials::get_api_keys();
                let screen_provider = cfg.models.screen.provider.clone();
                let screen_model = cfg.models.screen.model.clone();
                let screen_key = resolve_slot_key(&screen_provider, &keys);
                let judge_provider = cfg.models.judge.provider.clone();
                let judge_model = cfg.models.judge.model.clone();
                let judge_key = resolve_slot_key(&judge_provider, &keys);

                // C6 degraded mode: no key for a non-Ollama slot -> observe-only.
                let mode = judge::determine_judge_mode(
                    &screen_provider,
                    screen_key.as_deref(),
                    &judge_provider,
                    judge_key.as_deref(),
                );
                if let judge::JudgeMode::Degraded { ref reason } = mode {
                    println!("{}", judge::degraded_status_line(reason));
                }

                // req 10: best-effort session-end cleanup on Ctrl+C — marks any
                // still-`shown` card `expired` and logs `session_end`, mirroring
                // credentials.rs's SIGHUP-reload pattern (spawn-a-thread handler).
                {
                    let session_mgr_for_shutdown = session_mgr.clone();
                    let db_path_for_shutdown = db::get_db_path();
                    let cleanup: Box<dyn Fn() + Send> = Box::new(move || {
                        let sid = session_mgr_for_shutdown.lock().unwrap().session_id.clone();
                        if let Some(ref dp) = db_path_for_shutdown {
                            if let Ok(conn) = db::open_connection(dp) {
                                let expired = db::expire_unresolved_cards(&conn, &sid).unwrap_or(0);
                                let _ = db::log_event(
                                    &conn,
                                    &db::EventRecord {
                                        id: None,
                                        session_id: sid,
                                        kind: "session_end".to_string(),
                                        payload_json:
                                            serde_json::json!({ "expired_cards": expired })
                                                .to_string(),
                                        ts: None,
                                    },
                                );
                            }
                        }
                    });
                    let _ = SHUTDOWN_CLEANUP.set(std::sync::Mutex::new(Some(cleanup)));
                    #[cfg(unix)]
                    setup_sigint_handler();
                }

                // req 10: non-blocking (relative to the watcher) stdin reader —
                // g/u/n resolve the single pending card.
                {
                    let pending_card_for_stdin = pending_card.clone();
                    std::thread::spawn(move || {
                        use std::io::BufRead;
                        let stdin = std::io::stdin();
                        for line in stdin.lock().lines().map_while(Result::ok) {
                            let Some(verb) = response::response_verb_for_key(&line) else {
                                continue;
                            };
                            let maybe_pc = pending_card_for_stdin.lock().unwrap().take();
                            let Some(pc) = maybe_pc else { continue };
                            let Some(dp) = db::get_db_path() else {
                                continue;
                            };
                            let Ok(conn) = db::open_connection(&dp) else {
                                continue;
                            };
                            let _ = db::update_card_status(&conn, pc.card_id, verb);
                            let _ = db::log_event(
                                &conn,
                                &db::EventRecord {
                                    id: None,
                                    session_id: pc.session_id.clone(),
                                    kind: "card_response".to_string(),
                                    payload_json: serde_json::json!({
                                        "verb": verb,
                                        "concept": pc.concept_id,
                                    })
                                    .to_string(),
                                    ts: None,
                                },
                            );
                        }
                    });
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

                    // C2 session split on idle gap > 4h: expire the OLD session's
                    // unresolved cards (req 10 / C3 "no interaction by session end
                    // ⇒ expired") before rotating the snapshot to the new one.
                    let old_session_id = session_mgr.lock().unwrap().session_id.clone();
                    let split = session_mgr.lock().unwrap().on_file_event(now);
                    let session_id_now = session_mgr.lock().unwrap().session_id.clone();
                    if split {
                        if let Some(ref conn) = conn_opt {
                            let expired =
                                db::expire_unresolved_cards(conn, &old_session_id).unwrap_or(0);
                            let _ = db::log_event(
                                conn,
                                &db::EventRecord {
                                    id: None,
                                    session_id: old_session_id,
                                    kind: "session_end".to_string(),
                                    payload_json: serde_json::json!({ "expired_cards": expired })
                                        .to_string(),
                                    ts: None,
                                },
                            );
                        }
                        *pending_card.lock().unwrap() = None;
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

                    let rel_path = path
                        .strip_prefix(&project_root_cb)
                        .unwrap_or(path.as_path())
                        .to_path_buf();
                    pending_files.lock().unwrap().insert(rel_path.clone());

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

                    if let judge::JudgeMode::Degraded { .. } = &mode {
                        // Observe-only: events above are already recorded; no LLM call.
                        pending_files.lock().unwrap().clear();
                        return;
                    }

                    // req 8/req 4/req 6: dedup checked against the DB, computed
                    // fresh per advice-fp — never touches `bucket`/`session_mgr`/
                    // etc. from inside the dispatch closures below (mutex-
                    // poisoning safety: catch_unwind only ever wraps the pure
                    // provider call).
                    let already_judged = |fp: &str| -> bool {
                        conn_opt
                            .as_ref()
                            .map(|c| {
                                db::card_exists_with_advice_fp(c, &session_id_now, fp)
                                    .unwrap_or(false)
                            })
                            .unwrap_or(false)
                    };
                    let dispatch_stage1 = |prompt: &str| -> Result<String, String> {
                        judge::safe_dispatch(|| {
                            provider::dispatch_debounced_with_model(
                                &screen_provider,
                                Some(&screen_model),
                                prompt,
                                screen_key.as_deref(),
                            )
                        })
                        .map_err(|m| match m {
                            judge::JudgeMode::Degraded { reason } => reason,
                            judge::JudgeMode::Active => "degraded".to_string(),
                        })
                    };
                    let dispatch_stage2 = |prompt: &str| -> Result<String, String> {
                        judge::safe_dispatch(|| {
                            provider::dispatch_debounced_with_model(
                                &judge_provider,
                                Some(&judge_model),
                                prompt,
                                judge_key.as_deref(),
                            )
                        })
                        .map_err(|m| match m {
                            judge::JudgeMode::Degraded { reason } => reason,
                            judge::JudgeMode::Active => "degraded".to_string(),
                        })
                    };

                    // req 4/req 6 catch-up sweep: re-diff every file touched since
                    // it was last swept, not just the file that triggered this
                    // save — "at most one card on screen" (T1 req 9) still holds
                    // across the whole sweep.
                    let files_to_sweep: Vec<std::path::PathBuf> =
                        pending_files.lock().unwrap().iter().cloned().collect();
                    let mut shown_this_pass = false;

                    for rel in files_to_sweep {
                        pending_files.lock().unwrap().remove(&rel);
                        if shown_this_pass {
                            continue;
                        }

                        let abs = project_root_cb.join(&rel);
                        let sweep_content = match std::fs::read_to_string(&abs) {
                            Ok(c) => c,
                            Err(_) => continue,
                        };
                        if !site::parses_without_errors(&sweep_content) {
                            pending_files.lock().unwrap().insert(rel);
                            continue; // still broken: leave dirty for the next pass
                        }

                        let snap = snapshot.lock().unwrap().clone();
                        let hunks =
                            match session::compute_session_diff(&project_root_cb, &rel, &snap) {
                                Ok(h) => h,
                                Err(_) => continue,
                            };
                        if hunks.is_empty() {
                            continue;
                        }

                        let rel_str = rel.to_string_lossy().to_string();
                        let outcome = pipeline::judge_hunks(
                            &rel_str,
                            &hunks,
                            &sweep_content,
                            &taxonomy,
                            &canon,
                            already_judged,
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
                                        site::compute_site(&rel_str, &sweep_content, card.line)
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
                                                strict_mode_passed: o.strict_mode_passed,
                                            };
                                            let decision = {
                                                let mut b = bucket.lock().unwrap();
                                                budget::decide_push(&mut b, &candidate, now)
                                            };
                                            match decision {
                                                budget::PushDecision::Shown => {
                                                    println!("{}", card::render_card(&card, 0));
                                                    if let Some(ref conn) = conn_opt {
                                                        if let Ok(card_id) = db::insert_card(
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
                                                                worked_diff: Some(
                                                                    stage2.worked_diff.clone(),
                                                                ),
                                                            },
                                                        ) {
                                                            let _ = db::log_event(
                                                                conn,
                                                                &db::EventRecord {
                                                                    id: None,
                                                                    session_id: session_id_now.clone(),
                                                                    kind: "card_shown".to_string(),
                                                                    payload_json: serde_json::json!({
                                                                        "concept": stage2.concept,
                                                                    })
                                                                    .to_string(),
                                                                    ts: None,
                                                                },
                                                            );
                                                            *pending_card.lock().unwrap() =
                                                                Some(PendingCard {
                                                                    card_id,
                                                                    session_id: session_id_now
                                                                        .clone(),
                                                                    concept_id: stage2
                                                                        .concept
                                                                        .clone(),
                                                                });
                                                        }
                                                    }
                                                    shown_this_pass = true;
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
