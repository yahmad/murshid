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
#[path = "cli/clean.rs"]
pub mod cli_clean;
#[path = "cli/experiment.rs"]
pub mod cli_experiment;

fn main() {
    let _cfg = config::load_config();
    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 {
        match args[1].as_str() {
            "team-dashboard" => {
                let code = team_dashboard::run_team_dashboard_cli(&args[2..]);
                std::process::exit(code);
            }
            "clean" => {
                let code = cli_clean::run_clean_hook(&args[2..]);
                std::process::exit(code);
            }
            "experiment" => {
                let code = cli_experiment::run_experiment_cli(&args[2..]);
                std::process::exit(code);
            }
            "watch" => {
                let project_root = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
                println!("Starting Murshid Socratic watcher on {}...", project_root.display());
                
                let _watcher = match watcher::start_watching(project_root.clone(), move |path| {
                    println!("\nFile saved: {}", path.display());
                    let interceptor = compiler::CompilerInterceptor::new();
                    if let Ok(output) = interceptor.run_check(&project_root, &path) {
                        if !output.success {
                            println!("Socratic Mentor: Compiler check detected diagnostics!");
                            for diag in output.diagnostics {
                                let code_str = diag.code.clone().unwrap_or_else(|| "unknown".to_string());
                                let mut line_num = 1;
                                let mut file_name = path.to_string_lossy().to_string();
                                if let Some(span) = diag.spans.first() {
                                    line_num = span.line_start;
                                    file_name = span.file_name.clone();
                                }

                                println!("[ERROR {}] in {} at line {}", code_str, file_name, line_num);
                                println!("Message: {}", diag.message);

                                // Check pedagogy state
                                let db_path = match db::get_db_path() {
                                    Some(p) => p,
                                    None => continue,
                                };
                                if let Ok(conn) = db::open_connection(&db_path) {
                                    let state = pedagogy::load_dialogue_state(&conn, "workspace-hash", &file_name)
                                        .ok()
                                        .flatten()
                                        .unwrap_or_else(|| pedagogy::SocraticDialogueState {
                                            workspace_hash: "workspace-hash".to_string(),
                                            file_path_hash: file_name.clone(),
                                            scaffold_level: 1,
                                            consecutive_failures: 0,
                                            repetition_count: 0,
                                            dialogue_context_hash: None,
                                        });
                                    println!("Scaffold level: {}", state.scaffold_level);
                                }

                                // Load provider config and dispatch query
                                let provider_config = config::load_config().provider;
                                let api_key = match provider_config.api_key_source.as_str() {
                                    "keychain" => credentials::get_credential("murshid", "gemini_api_key").ok(),
                                    _ => std::env::var("GEMINI_API_KEY").ok(),
                                };
                                
                                if let Some(key) = api_key {
                                    println!("Contacting model provider for Socratic guidance...");
                                    let exclude_patterns = config::load_config().watcher.exclude;
                                    if let Ok(context_payload) = context::generate_context_payload(
                                        &project_root,
                                        &path,
                                        line_num,
                                        &diag.message,
                                        &exclude_patterns,
                                    ) {
                                        let provider_type = if std::env::var("ANTHROPIC_API_KEY").is_ok() {
                                            "claude"
                                        } else {
                                            "gemini"
                                        };
                                        if let Ok(response) = provider::dispatch_debounced(provider_type, &context_payload, Some(&key)) {
                                            println!("\n--- Socratic Guidance ---");
                                            println!("{}", response);
                                            println!("-------------------------\n");
                                        }
                                    }
                                } else {
                                    println!("(Note: Set GEMINI_API_KEY environment variable to see Socratic AI mentor guidance here)");
                                }
                            }
                        } else {
                            println!("Socratic Mentor: Compilation check passed cleanly!");
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
                println!("Hello, world!");
            }
        }
    } else {
        println!("Hello, world!");
    }
}
