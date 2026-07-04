//! `murshid goal [text]` (R3) — D13(c) file-backed goal storage. T3 reshapes
//! this from the removed DB-backed `goals` table (see FOUNDATIONS-INHERITED.md)
//! to the user-editable `.murshid/goal` file `goal.rs` now owns.

use std::path::Path;

/// Bare `murshid goal` prints the current goal (and any notes); `murshid
/// goal <text...>` sets it explicitly. This never touches the auto-write
/// marker (`goal.rs::write_last_inferred_at`) — only inference does — so an
/// explicit set protects itself the same way a hand-edit does: its fresh
/// mtime simply postdates whatever auto-write marker (if any) already
/// exists, which is what `explicit_wins` compares against (req 3).
pub fn run_goal_cli(project_root: &Path, args: &[String]) -> Result<(), String> {
    if args.is_empty() {
        match crate::goal::read_goal_file(project_root) {
            Some(g) => {
                println!("{}", g.text);
                for note in &g.notes {
                    println!("{}", note);
                }
            }
            None => println!("No goal set yet. Use 'murshid goal <text>' to set one."),
        }
        return Ok(());
    }

    let text = args.join(" ");
    crate::goal::write_goal_file(project_root, &text)?;
    println!("Goal set: {}", text);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_root(name: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(name);
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn test_run_goal_cli_set_then_get() {
        let root = temp_root("murshid_test_cli_goal_set_get");

        run_goal_cli(
            &root,
            &["fix".to_string(), "auth".to_string(), "timeout".to_string()],
        )
        .unwrap();
        assert_eq!(
            crate::goal::read_goal_file(&root).unwrap().text,
            "fix auth timeout"
        );

        // Bare `goal` (no args) must not error even without capturing stdout.
        run_goal_cli(&root, &[]).unwrap();

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn test_run_goal_cli_get_with_no_goal_set_does_not_error() {
        let root = temp_root("murshid_test_cli_goal_get_empty");
        assert!(run_goal_cli(&root, &[]).is_ok());
        let _ = fs::remove_dir_all(&root);
    }
}
