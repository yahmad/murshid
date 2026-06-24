pub mod config;
pub mod db;
pub mod backup;
pub mod credentials;
pub mod watcher;
pub mod compiler;
pub mod watcher_coordinator;

fn main() {
    let _cfg = config::load_config();
    println!("Hello, world!");
}
