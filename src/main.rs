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

fn main() {
    let _cfg = config::load_config();
    println!("Hello, world!");
}
