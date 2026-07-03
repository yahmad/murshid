//! T13 req 3: thin binary entry point over the `murshid` library crate
//! (`src/lib.rs`) — module declarations and the cross-module helpers
//! (`resolve_slot_key`, `goal_text_now`, `persist_review_digest`,
//! `run_review`) live there now so `tests/` integration tests can reach the
//! real pipeline through the crate's public surface.

use murshid::{cli, config, db, watch};

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
        "  goal [text]                            Print the current goal, or set it (bare = print, D13(c))"
    );
    println!(
        "  review [path]                          Solicited review (D18): batched screen->judge digest of the session diff"
    );
    println!(
        "  progress                               The open per-concept skill meter (I24): mastery bar, help level, staleness"
    );
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
                match cli::setup::run_setup(&project_root) {
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
                // D13(c): file-backed, not DB-backed — run from the project
                // root (cwd), same convention as the other path-less
                // surfaces in R3.
                let project_root =
                    std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
                match cli::goal::run_goal_cli(&project_root, &args[2..]) {
                    Ok(_) => {
                        std::process::exit(0);
                    }
                    Err(e) => {
                        eprintln!("Goal command failed: {}", e);
                        std::process::exit(1);
                    }
                }
            }
            "progress" => cli::progress::run(),
            "review" => cli::review::run(&args),
            "watch" => watch::run(&args),
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
