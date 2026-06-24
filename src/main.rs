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
