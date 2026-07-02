pub mod backup;
pub mod compiler;
pub mod config;
pub mod context;
pub mod credentials;
pub mod db;
pub mod pedagogy;
pub mod provider;
pub mod redactor;
pub mod sanitizer;
pub mod watcher;
pub mod watcher_coordinator;

#[path = "cli/bypass.rs"]
pub mod cli_bypass;
#[path = "cli/eval.rs"]
pub mod cli_eval;
#[path = "cli/register.rs"]
pub mod cli_register;
#[path = "cli/setup.rs"]
pub mod cli_setup;
#[path = "cli/share.rs"]
pub mod cli_share;
#[path = "cli/goal.rs"]
pub mod cli_goal;
pub mod licensing;
pub mod offline_docs;
#[path = "team/exporter.rs"]
pub mod team_exporter;
#[path = "team/verifier.rs"]
pub mod team_verifier;
#[path = "team/dashboard.rs"]
pub mod team_dashboard;
#[path = "lsp/proxy.rs"]
pub mod lsp_proxy;
#[path = "lsp/server.rs"]
pub mod lsp_server;
#[path = "lsp/diagnostics.rs"]
pub mod lsp_diagnostics;
#[path = "lsp/code_actions.rs"]
pub mod lsp_code_actions;
#[path = "lsp/bridge.rs"]
pub mod lsp_bridge;

fn parse_duration(s: &str) -> Result<u32, String> {
    if s.ends_with('s') {
        s[..s.len()-1].parse::<u32>().map_err(|e| e.to_string())
    } else if s.ends_with('m') {
        s[..s.len()-1].parse::<u32>().map(|m| m * 60).map_err(|e| e.to_string())
    } else if s.ends_with('h') {
        s[..s.len()-1].parse::<u32>().map(|h| h * 3600).map_err(|e| e.to_string())
    } else {
        s.parse::<u32>().map_err(|_| "Invalid duration format. Use e.g. 30m, 1h, or raw seconds".to_string())
    }
}

fn print_usage() {
    println!("Murshid — Local-First Socratic AI Coding Mentor");
    println!("\nUsage:");
    println!("  murshid <command> [args]");
    println!("\nCommands:");
    println!("  setup [path]                           Onboard a new project (auto-adds .murshid/ to .gitignore and parses .env)");
    println!("  register [-g <gemini_key>] [-c <claude_key>] [--silent]  Register API keys to platform secure keyring");
    println!("  watch [path]                           Watch a directory for code updates to trigger Socratic mentor feedback");
    println!("  bypass -d <duration> -r <reason> [-f]   Temporarily bypass Socratic mentoring mode (weekly limit of 3)");
    println!("  share <output-file>                    Export struggle logs as a Markdown summary and copy to clipboard");
    println!("  goal <command> [args]                  Manage project active goals (set, get, complete, list)");
    println!("  team-dashboard [args]                  Aggregate and compile local progress analytics to HTML dashboard");
    println!("  lsp-server                             Run embedded LSP server");
    println!("  lsp-proxy                              Run LSP socket/stdio proxy");
}

fn main() {
    let _cfg = config::load_config();
    if let Some(db_path) = db::get_db_path() {
        if let Err(e) = db::initialize_db(&db_path) {
            eprintln!("[WARNING] Failed to initialize database at {}: {}", db_path.display(), e);
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
                        println!("Setup completed successfully for {}", project_root.display());
                        std::process::exit(0);
                    }
                    Err(e) => {
                        eprintln!("Setup failed: {}", e);
                        std::process::exit(1);
                    }
                }
            }
            "register" => {
                let mut gemini_key = None;
                let mut claude_key = None;
                let mut silent = false;
                
                let mut i = 2;
                while i < args.len() {
                    match args[i].as_str() {
                        "--gemini" | "-g" => {
                            if i + 1 < args.len() {
                                gemini_key = Some(args[i+1].as_str());
                                i += 2;
                            } else {
                                eprintln!("Error: --gemini requires an argument");
                                std::process::exit(1);
                            }
                        }
                        "--claude" | "-c" => {
                            if i + 1 < args.len() {
                                claude_key = Some(args[i+1].as_str());
                                i += 2;
                            } else {
                                eprintln!("Error: --claude requires an argument");
                                std::process::exit(1);
                            }
                        }
                        "--silent" | "-s" => {
                            silent = true;
                            i += 1;
                        }
                        _ => {
                            eprintln!("Unknown argument: {}", args[i]);
                            std::process::exit(1);
                        }
                    }
                }
                
                match cli_register::run_registration(gemini_key, claude_key, silent) {
                    Ok(_) => {
                        std::process::exit(0);
                    }
                    Err(e) => {
                        eprintln!("Registration failed: {}", e);
                        std::process::exit(1);
                    }
                }
            }
            "bypass" => {
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

                let mut duration_str = None;
                let mut reason_str = None;
                let mut force = false;

                let mut i = 2;
                while i < args.len() {
                    match args[i].as_str() {
                        "--duration" | "-d" => {
                            if i + 1 < args.len() {
                                duration_str = Some(&args[i+1]);
                                i += 2;
                            } else {
                                eprintln!("Error: --duration requires an argument");
                                std::process::exit(1);
                            }
                        }
                        "--reason" | "-r" => {
                            if i + 1 < args.len() {
                                reason_str = Some(&args[i+1]);
                                i += 2;
                            } else {
                                eprintln!("Error: --reason requires an argument");
                                std::process::exit(1);
                            }
                        }
                        "--force" | "-f" => {
                            force = true;
                            i += 1;
                        }
                        _ => {
                            eprintln!("Unknown argument: {}", args[i]);
                            std::process::exit(1);
                        }
                    }
                }

                let duration_str = match duration_str {
                    Some(d) => d,
                    None => {
                        eprintln!("Error: --duration is required");
                        std::process::exit(1);
                    }
                };

                if reason_str.is_none() {
                    eprintln!("Error: --reason is required to justify bypass");
                    std::process::exit(1);
                }

                let duration_secs = match parse_duration(duration_str) {
                    Ok(s) => s,
                    Err(e) => {
                        eprintln!("Invalid duration: {}", e);
                        std::process::exit(1);
                    }
                };

                // CLI_BYPASS = 2
                match cli_bypass::run_bypass(&conn, duration_secs, 2, force, None, None) {
                    Ok(_) => {
                        std::process::exit(0);
                    }
                    Err(e) => {
                        eprintln!("Bypass failed: {}", e);
                        std::process::exit(1);
                    }
                }
            }
            "share" => {
                if args.len() < 3 {
                    eprintln!("Usage: murshid share <output-file>");
                    std::process::exit(1);
                }
                let output_file = std::path::PathBuf::from(&args[2]);
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
                match cli_share::run_share(&conn, &output_file) {
                    Ok(_) => std::process::exit(0),
                    Err(e) => {
                        eprintln!("Share failed: {}", e);
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
            "team-dashboard" => {
                let code = team_dashboard::run_team_dashboard_cli(&args[2..]);
                std::process::exit(code);
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
                
                println!("Starting Murshid Socratic watcher on {}...", project_root.display());
                
                let _watcher = match watcher::start_watching(project_root.clone(), move |path| {
                    println!("\\nFile saved: {}", path.display());
                    let db_path = db::get_db_path();
                    let conn_opt = db_path.as_ref().and_then(|dp| db::open_connection(dp).ok());
                    let project_root_str = project_root.to_string_lossy().to_string();
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

                    let interceptor = compiler::CompilerInterceptor::new();
                    if let Ok(output) = interceptor.run_check(&project_root, &path) {
                        let workspace_hash = pedagogy::sha256(project_root_str.as_bytes());

                        if !output.success {
                            println!("Socratic Mentor: Compiler check detected diagnostics!");
                            
                            // Log compile failure
                            if let Some(ref conn) = conn_opt {
                                let first_diag = output.diagnostics.first();
                                let primary_err_code = first_diag.and_then(|d| d.code.clone()).unwrap_or_else(|| "unknown".to_string());
                                let primary_err_msg = first_diag.map(|d| d.message.clone()).unwrap_or_default();
                                
                                let mut primary_line_num = 1;
                                let mut primary_file_name = file_path_str.clone();
                                if let Some(diag) = first_diag {
                                    let active_span = diag.spans.iter()
                                        .find(|s| {
                                            let p = std::path::Path::new(&s.file_name);
                                            p == path || p.ends_with(&path)
                                        })
                                        .or_else(|| {
                                            diag.spans.iter().find(|s| {
                                                let p = std::path::Path::new(&s.file_name);
                                                p.starts_with(&project_root)
                                            })
                                        })
                                        .or(diag.spans.first());

                                    if let Some(span) = active_span {
                                        primary_line_num = span.line_start;
                                        primary_file_name = span.file_name.clone();
                                    }
                                }

                                let check_event = db::HistoryEvent {
                                    id: None,
                                    event_type: "compiler_check".to_string(),
                                    project_root: project_root_str.clone(),
                                    file_path: primary_file_name.clone(),
                                    success: Some(false),
                                    error_code: Some(primary_err_code.clone()),
                                    error_message: Some(primary_err_msg.clone()),
                                    line_number: Some(primary_line_num as i64),
                                    created_at: None,
                                };
                                let _ = db::log_history_event(conn, &check_event);

                                // Hook up call to handle_compile_check_event
                                let _ = pedagogy::handle_compile_check_event(
                                    conn,
                                    &workspace_hash,
                                    std::path::Path::new(&primary_file_name),
                                    &primary_err_code,
                                    false,
                                );
                            }

                            for diag in output.diagnostics {
                                let code_str = diag.code.clone().unwrap_or_else(|| "unknown".to_string());
                                let mut line_num = 1;
                                let mut file_name = path.to_string_lossy().to_string();
                                let active_span = diag.spans.iter()
                                    .find(|s| {
                                        let p = std::path::Path::new(&s.file_name);
                                        p == path || p.ends_with(&path)
                                    })
                                    .or_else(|| {
                                        diag.spans.iter().find(|s| {
                                            let p = std::path::Path::new(&s.file_name);
                                            p.starts_with(&project_root)
                                        })
                                    })
                                    .or(diag.spans.first());

                                if let Some(span) = active_span {
                                    line_num = span.line_start;
                                    file_name = span.file_name.clone();
                                }

                                println!("[ERROR {}] in {} at line {}", code_str, file_name, line_num);
                                println!("Message: {}", diag.message);

                                // Check pedagogy state
                                if let Some(ref conn) = conn_opt {
                                    let state = pedagogy::load_dialogue_state(conn, &workspace_hash, &file_name)
                                        .ok()
                                        .flatten()
                                        .unwrap_or_else(|| pedagogy::SocraticDialogueState {
                                            workspace_hash: workspace_hash.clone(),
                                            file_path_hash: file_name.clone(),
                                            scaffold_level: 1,
                                            consecutive_failures: 0,
                                            repetition_count: 0,
                                            dialogue_context_hash: None,
                                        });
                                    println!("Scaffold level: {}", state.scaffold_level);
                                }

                                // Load provider config and dispatch query
                                let api_keys = credentials::get_api_keys();
                                let provider_type = if std::env::var("ANTHROPIC_API_KEY").is_ok()
                                    || api_keys.as_ref().and_then(|k| k.claude_api_key.as_ref()).is_some()
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
                                
                                if let Some(key) = api_key {
                                    println!("Contacting model provider for Socratic guidance...");
                                    let exclude_patterns = config::load_config().watcher.exclude;
                                    if let Ok(context_payload) = context::generate_context_payload(
                                        &project_root,
                                        &std::path::Path::new(&file_name),
                                        line_num,
                                        &diag.message,
                                        &exclude_patterns,
                                    ) {
                                        let active_goal_payload = if let Some(ref conn) = conn_opt {
                                            match cli_goal::get_active_goal(conn) {
                                                Ok(Some(goal)) => {
                                                    let desc = goal.description.unwrap_or_default();
                                                    format!(
                                                        "<active_goal>\n  <title>{}</title>\n  <description>{}</description>\n</active_goal>\n",
                                                        context::sanitize_xml(&goal.title),
                                                        context::sanitize_xml(&desc)
                                                    )
                                                }
                                                _ => String::new(),
                                            }
                                        } else {
                                            String::new()
                                        };
                                        let full_payload = format!("{}{}", active_goal_payload, context_payload);
                                        match provider::dispatch_debounced(provider_type, &full_payload, Some(&key)) {
                                            Ok(response) => {
                                                println!("\\n--- Socratic Guidance ---");
                                                println!("{}", response);
                                                println!("-------------------------\\n");
                                            }
                                            Err(e) => {
                                                eprintln!("[ERROR] Failed to fetch Socratic guidance: {}", e);
                                            }
                                        }
                                    }
                                } else {
                                    println!("(Note: Set GEMINI_API_KEY environment variable to see Socratic AI mentor guidance here)");
                                }
                            }
                        } else {
                            println!("Socratic Mentor: Compilation check passed cleanly!");
                            // Log compile success
                            if let Some(ref conn) = conn_opt {
                                let check_event = db::HistoryEvent {
                                    id: None,
                                    event_type: "compiler_check".to_string(),
                                    project_root: project_root_str.clone(),
                                    file_path: file_path_str.clone(),
                                    success: Some(true),
                                    error_code: None,
                                    error_message: None,
                                    line_number: None,
                                    created_at: None,
                                };
                                let _ = db::log_history_event(conn, &check_event);

                                // Hook up call to handle_compile_check_event with success
                                let _ = pedagogy::handle_compile_check_event(
                                    conn,
                                    &workspace_hash,
                                    &path,
                                    "E0382",
                                    true,
                                );
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
            "lsp-server" => {
                let socket_path = lsp_proxy::get_socket_path();
                let listener = match lsp_server::bind_uds_socket(&socket_path) {
                    Ok(l) => l,
                    Err(e) => {
                        eprintln!("Failed to bind socket: {}", e);
                        std::process::exit(1);
                    }
                };
                println!("Murshid LSP Server listening on {}", socket_path.display());
                for stream_res in listener.incoming() {
                    if let Ok(stream) = stream_res {
                        std::thread::spawn(move || {
                            let _ = lsp_server::handle_connection(stream);
                        });
                    }
                }
                std::process::exit(0);
            }
            "lsp-proxy" => {
                if let Err(e) = lsp_proxy::run_lsp_proxy() {
                    eprintln!("Error in LSP proxy: {}", e);
                    std::process::exit(1);
                }
                std::process::exit(0);
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
