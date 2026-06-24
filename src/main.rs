pub mod config;
pub mod db;
pub mod backup;
pub mod credentials;
pub mod watcher;
pub mod compiler;
pub mod watcher_coordinator;
pub mod context;
pub mod sanitizer;
pub mod pedagogy;
pub mod provider;
pub mod redactor;

#[path = "cli/setup.rs"]
pub mod cli_setup;
#[path = "cli/register.rs"]
pub mod cli_register;

fn main() {
    let _cfg = config::load_config();
    println!("Hello, world!");
}
