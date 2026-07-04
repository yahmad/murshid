//! The `murshid` subcommand implementations (`setup`/`goal`/`review`/
//! `progress`). `main.rs` parses argv and hands off to [`dispatch`] here;
//! the `watch` arm lives in the sibling `watch` module.
//!
//! Single-exit-point convention: every subcommand's `run` returns
//! `Result<(), i32>` (`Ok(())` = exit 0, `Err(code)` = exit `code`) instead
//! of calling `std::process::exit` inline. `dispatch` translates that into
//! a plain exit code; `main` is the only place that actually calls
//! `std::process::exit`.

use std::path::PathBuf;

pub mod goal;
pub mod progress;
pub mod review;
pub mod setup;

/// Resolves the project-root argument shared by path-taking subcommands
/// (`setup [path]`, `review [path]`): explicit `args[2]` if given, else cwd.
/// (`goal`/`progress` don't take this arg — D13(c): they're always
/// cwd-rooted, same convention as the rest of R3.)
pub fn project_root_from_args(args: &[String]) -> PathBuf {
    if args.len() > 2 {
        PathBuf::from(&args[2])
    } else {
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
    }
}

/// The usage text shown by `--help`/`-h` (stdout, exit 0) and on a usage
/// error — unknown command or no command at all (stderr, exit 2). Ends
/// with a single trailing newline; print with `print!`/`eprint!`, not
/// `println!`/`eprintln!`, to avoid doubling it.
pub fn usage_text() -> &'static str {
    concat!(
        "Murshid — Local-First Socratic AI Coding Mentor\n",
        "\nUsage:\n",
        "  murshid <command> [args]\n",
        "\nCommands:\n",
        "  setup [path]                           Onboard a new project (auto-adds .murshid/ to .gitignore and parses .env)\n",
        "  watch [path]                           Watch a directory for code updates to trigger Socratic mentor feedback\n",
        "  goal [text]                            Print the current goal, or set it (bare = print, D13(c))\n",
        "  review [path]                          Solicited review (D18): batched screen->judge digest of the session diff\n",
        "  progress                               The open per-concept skill meter (I24): mastery bar, help level, staleness\n",
    )
}

fn to_exit_code(r: Result<(), i32>) -> i32 {
    match r {
        Ok(()) => 0,
        Err(code) => code,
    }
}

/// The single dispatch point for argv -> subcommand: parses `args[1]` and
/// returns the process exit code `main` should exit with. `--help`/`-h`
/// and `--version`/`-V` print to stdout and exit 0; an unknown command or
/// no command at all prints usage to stderr and exits 2 (the conventional
/// usage-error code, distinct from a runtime failure's exit 1).
pub fn dispatch(args: &[String]) -> i32 {
    let Some(command) = args.get(1).map(String::as_str) else {
        eprint!("{}", usage_text());
        return 2;
    };

    match command {
        "--help" | "-h" => {
            print!("{}", usage_text());
            0
        }
        "--version" | "-V" => {
            println!("murshid {}", env!("CARGO_PKG_VERSION"));
            0
        }
        "setup" => to_exit_code(setup::run(&project_root_from_args(args))),
        "goal" => {
            // D13(c): file-backed, not DB-backed — run from the project
            // root (cwd), same convention as the other path-less surfaces
            // in R3.
            let project_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            to_exit_code(goal::run(&project_root, &args[2..]))
        }
        "progress" => to_exit_code(progress::run()),
        "review" => to_exit_code(review::run(args)),
        "watch" => {
            crate::watch::run(args);
            0
        }
        _ => {
            eprint!("{}", usage_text());
            2
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_root_from_args_uses_explicit_path() {
        let args = vec![
            "murshid".to_string(),
            "review".to_string(),
            "/some/path".to_string(),
        ];
        assert_eq!(project_root_from_args(&args), PathBuf::from("/some/path"));
    }

    #[test]
    fn project_root_from_args_falls_back_to_cwd() {
        let args = vec!["murshid".to_string(), "review".to_string()];
        let expected = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        assert_eq!(project_root_from_args(&args), expected);
    }

    #[test]
    fn usage_text_has_single_trailing_newline() {
        let text = usage_text();
        assert!(text.ends_with('\n'));
        assert!(!text.ends_with("\n\n"));
    }
}
