use std::collections::HashSet;
use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Debug, Clone, PartialEq)]
pub struct CruftItem {
    pub file_path: String,
    pub line_number: u32,
    pub content: String,
    pub cruft_type: String,
}

pub fn scan_diff_for_cruft(diff: &str) -> Vec<CruftItem> {
    let mut items = Vec::new();
    let mut current_file = String::new();
    let mut current_line = 0;

    for line in diff.lines() {
        if line.starts_with("+++ b/") {
            current_file = line[6..].to_string();
            continue;
        }
        if line.starts_with("@@ ") {
            if let Some(pos) = line.find('+') {
                let rest = &line[pos + 1..];
                let comma_pos = rest.find(',').unwrap_or(rest.len());
                let space_pos = rest.find(' ').unwrap_or(rest.len());
                let end_pos = comma_pos.min(space_pos);
                if let Ok(line_num) = rest[..end_pos].parse::<u32>() {
                    current_line = line_num;
                }
            }
            continue;
        }

        if line.starts_with('+') && !line.starts_with("+++") {
            let content = &line[1..];
            let mut detected = false;
            let mut cruft_type = String::new();

            if content.contains("eprintln!") {
                detected = true;
                cruft_type = "eprintln!".to_string();
            } else if content.contains("println!") {
                detected = true;
                cruft_type = "println!".to_string();
            } else if content.contains("dbg!") {
                detected = true;
                cruft_type = "dbg!".to_string();
            } else if content.contains("std::fmt::Debug") {
                detected = true;
                cruft_type = "std::fmt::Debug".to_string();
            } else if content.contains("#[allow(dead_code)]") {
                detected = true;
                cruft_type = "#[allow(dead_code)]".to_string();
            } else if content.contains("#[allow(unused_imports)]") {
                detected = true;
                cruft_type = "#[allow(unused_imports)]".to_string();
            } else if content.contains("use ") && (content.contains("::") || content.trim().ends_with(';')) && (content.contains("unused") || content.contains("dummy")) {
                detected = true;
                cruft_type = "unused import".to_string();
            }

            if detected {
                items.push(CruftItem {
                    file_path: current_file.clone(),
                    line_number: current_line,
                    content: content.trim().to_string(),
                    cruft_type,
                });
            }
            current_line += 1;
        } else if !line.starts_with('-') {
            current_line += 1;
        }
    }

    items
}

pub fn get_diff_stats(diff: &str) -> (usize, usize) {
    let mut files = HashSet::new();
    let mut lines_added = 0;
    for line in diff.lines() {
        if line.starts_with("+++ b/") {
            files.insert(line[6..].to_string());
        } else if line.starts_with('+') && !line.starts_with("+++") {
            lines_added += 1;
        }
    }
    (files.len(), lines_added)
}

fn log_bypass(project_root: &Path, files_modified_count: usize) -> Result<(), String> {
    let db_path_opt = crate::db::get_db_path();
    let db_path = match db_path_opt {
        Some(p) => p,
        None => return Err("Could not locate db path".to_string()),
    };

    let conn = crate::db::open_connection(&db_path).map_err(|e| e.to_string())?;

    let project_path = project_root
        .to_string_lossy()
        .to_string();

    let salt = "murshid-salt";
    let salted = format!("{}-{}", project_path, salt);
    let workspace_hash = crate::pedagogy::sha256(salted.as_bytes());

    // Reason code 100 for pre-commit cleaner bypass
    conn.execute(
        "INSERT INTO Socratic_bypass_log (duration_seconds, bypass_reason_code, workspace_hash, files_modified_count, activated_at)
         VALUES (?1, ?2, ?3, ?4, CURRENT_TIMESTAMP)",
        (0, 100, Some(workspace_hash), Some(files_modified_count as i32)),
    )
    .map_err(|e| e.to_string())?;

    Ok(())
}

pub fn run_clean_hook_with_diff(
    diff: &str,
    force: bool,
    is_interactive: bool,
    project_root: &Path,
    input_reader: &mut dyn Read,
) -> i32 {
    let config = crate::config::load_config();
    let (files, lines) = get_diff_stats(diff);

    // Check if bypass environment variable is set
    let env_bypass = std::env::var("MURSHID_BYPASS_COMMIT").is_ok();
    if env_bypass {
        if config.git.pre_commit.audit_bypass_logging {
            let _ = log_bypass(project_root, files);
        }
        return 0;
    }

    let cruft_items = scan_diff_for_cruft(diff);
    if cruft_items.is_empty() {
        return 0;
    }

    // Check execution thresholds (15 files / 1000 lines)
    if (files > 15 || lines > 1000) && !force {
        eprintln!(
            "[WARNING] Staged changes exceed threshold ({} files, {} lines). Skipping interactive clean.",
            files, lines
        );
        for item in &cruft_items {
            eprintln!("[CRUFT] {} in {} at line {}", item.cruft_type, item.file_path, item.line_number);
        }
        return 0;
    }

    if is_interactive {
        let mut reader = BufReader::new(input_reader);

        #[derive(Debug, Clone, Copy, PartialEq)]
        enum Action {
            Keep,
            Strip,
            Comment,
        }

        let mut actions = Vec::new();
        let mut quit = false;

        for item in &cruft_items {
            if quit {
                actions.push((item, Action::Keep));
                continue;
            }

            println!(
                "Staged line in {}:{} contains learning cruft: '{}'",
                item.file_path, item.line_number, item.content
            );
            println!("Would you like to:");
            println!("  [s] Strip: Automatically remove the line and re-stage.");
            println!("  [k] Keep: Commit the line as-is.");
            println!("  [c] Comment: Add #[cfg(test)] wrapper.");
            println!("  [q] Quit/Abort: Skip cleaning remaining lines and proceed to commit.");
            print!("Enter choice [s, k, c, q]: ");
            let _ = io::stdout().flush();

            let mut choice = String::new();
            if reader.read_line(&mut choice).is_ok() {
                let choice = choice.trim().to_lowercase();
                match choice.as_str() {
                    "s" => actions.push((item, Action::Strip)),
                    "c" => actions.push((item, Action::Comment)),
                    "q" => {
                        quit = true;
                        actions.push((item, Action::Keep));
                    }
                    _ => actions.push((item, Action::Keep)),
                }
            } else {
                actions.push((item, Action::Keep));
            }
        }

        // Apply actions grouped by file_path
        let mut file_actions: std::collections::HashMap<String, Vec<(&CruftItem, Action)>> = std::collections::HashMap::new();
        for (item, action) in actions {
            file_actions.entry(item.file_path.clone()).or_insert_with(Vec::new).push((item, action));
        }

        for (file_path, mut items_actions) in file_actions {
            // Sort descending by line_number to avoid line offset shifts during mutation
            items_actions.sort_by_key(|(item, _)| std::cmp::Reverse(item.line_number));

            let path = project_root.join(&file_path);
            if let Ok(content) = fs::read_to_string(&path) {
                let mut file_lines: Vec<String> = content.lines().map(|s| s.to_string()).collect();
                let mut modified = false;

                for (item, action) in items_actions {
                    let idx = (item.line_number as usize).saturating_sub(1);
                    if idx < file_lines.len() {
                        if file_lines[idx].contains(&item.content) {
                            match action {
                                Action::Strip => {
                                    file_lines.remove(idx);
                                    modified = true;
                                }
                                Action::Comment => {
                                    let line = &file_lines[idx];
                                    let leading_whitespace_len = line.len() - line.trim_start().len();
                                    let (whitespace, code) = line.split_at(leading_whitespace_len);
                                    file_lines[idx] = format!("{}#[cfg(test)] {}", whitespace, code);
                                    modified = true;
                                }
                                Action::Keep => {}
                            }
                        }
                    }
                }

                if modified {
                    let new_content = file_lines.join("\n") + "\n";
                    if fs::write(&path, new_content).is_ok() {
                        // Re-stage the modified file
                        let _ = std::process::Command::new("git")
                            .args(&["add", &file_path])
                            .current_dir(project_root)
                            .status();
                    }
                }
            }
        }

        0
    } else {
        // Headless / Non-TTY: Try GUI prompt via UDS socket
        let timeout_ms = config.git.pre_commit.gui_dialog_timeout_ms;
        let socket_path = crate::lsp_proxy::get_socket_path();

        let socket_connect = std::os::unix::net::UnixStream::connect(&socket_path);
        match socket_connect {
            Ok(mut stream) => {
                let _ = stream.set_read_timeout(Some(Duration::from_millis(timeout_ms)));
                let prompt_req = serde_json::json!({
                    "method": "gui/prompt",
                    "params": {
                        "items_count": cruft_items.len(),
                    }
                });
                
                if stream.write_all(format!("{}\n", prompt_req).as_bytes()).is_ok() && stream.flush().is_ok() {
                    let mut socket_reader = BufReader::new(&stream);
                    let mut resp = String::new();
                    if socket_reader.read_line(&mut resp).is_ok() {
                        if let Ok(val) = serde_json::from_str::<serde_json::Value>(&resp) {
                            if val.get("result").and_then(|v| v.as_str()) == Some("abort") {
                                eprintln!("Commit aborted by Socratic GUI prompt.");
                                return 1;
                            } else {
                                return 0;
                            }
                        }
                    }
                }
                // Fall back if write/read fails
                fallback_action(&config.git.pre_commit.action, &cruft_items)
            }
            Err(_) => {
                // Daemon offline, fallback to config action
                fallback_action(&config.git.pre_commit.action, &cruft_items)
            }
        }
    }
}

pub fn run_clean_hook(args: &[String]) -> i32 {
    let mut force = false;
    let mut interactive = atty_or_terminal_check();
    
    for arg in args {
        if arg == "--force" || arg == "-f" {
            force = true;
        }
        if arg == "--non-interactive" {
            interactive = false;
        }
        if arg == "--interactive" {
            interactive = true;
        }
    }

    let project_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

    let diff = match std::process::Command::new("git")
        .args(&["diff-index", "-p", "--cached", "HEAD"])
        .current_dir(&project_root)
        .output()
    {
        Ok(output) => {
            if !output.status.success() {
                // Try fallback to standard diff --cached if HEAD is missing (empty repo first commit)
                match std::process::Command::new("git")
                    .args(&["diff", "--cached"])
                    .current_dir(&project_root)
                    .output()
                {
                    Ok(out) => {
                        if out.status.success() {
                            String::from_utf8_lossy(&out.stdout).into_owned()
                        } else {
                            String::new()
                        }
                    }
                    Err(_) => String::new(),
                }
            } else {
                String::from_utf8_lossy(&output.stdout).into_owned()
            }
        }
        Err(e) => {
            eprintln!("[WARNING] Could not execute git: {}", e);
            String::new()
        }
    };

    let mut stdin = std::io::stdin();
    run_clean_hook_with_diff(&diff, force, interactive, &project_root, &mut stdin)
}

fn atty_or_terminal_check() -> bool {
    use std::io::IsTerminal;
    std::io::stdin().is_terminal()
}

fn fallback_action(action: &str, items: &[CruftItem]) -> i32 {
    if action == "fail" {
        eprintln!("[ERROR] Commit blocked by Socratic Commit Cleaner.");
        for item in items {
            eprintln!(
                "[CRUFT ERROR] Found {} in {} at line {}",
                item.cruft_type, item.file_path, item.line_number
            );
        }
        eprintln!("To bypass, commit with --no-verify or set MURSHID_BYPASS_COMMIT=1");
        1
    } else {
        // default to "warn"
        eprintln!("[WARNING] Detected learning cruft in staged changes:");
        for item in items {
            eprintln!(
                "[CRUFT WARNING] Found {} in {} at line {}",
                item.cruft_type, item.file_path, item.line_number
            );
        }
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_scan_diff_for_cruft() {
        let diff = r#"
diff --git a/src/main.rs b/src/main.rs
--- a/src/main.rs
+++ b/src/main.rs
@@ -1,5 +1,8 @@
 fn main() {
+    println!("hello");
+    let x = 10;
+    dbg!(x);
+    use std::collections::HashMap; // dummy
 }
 "#;
         let items = scan_diff_for_cruft(diff);
         assert_eq!(items.len(), 3);
         assert_eq!(items[0].cruft_type, "println!");
         assert_eq!(items[1].cruft_type, "dbg!");
         assert_eq!(items[2].cruft_type, "unused import");
     }

    #[test]
    fn test_scan_all_patterns() {
        let diff = r#"
diff --git a/src/lib.rs b/src/lib.rs
--- a/src/lib.rs
+++ b/src/lib.rs
+    eprintln!("debug error");
+    let a: std::fmt::Debug;
+    #[allow(dead_code)]
+    #[allow(unused_imports)]
+    use std::io::Read; // unused
"#;
        let items = scan_diff_for_cruft(diff);
        assert_eq!(items.len(), 5);
        assert_eq!(items[0].cruft_type, "eprintln!");
        assert_eq!(items[1].cruft_type, "std::fmt::Debug");
        assert_eq!(items[2].cruft_type, "#[allow(dead_code)]");
        assert_eq!(items[3].cruft_type, "#[allow(unused_imports)]");
        assert_eq!(items[4].cruft_type, "unused import");
    }

    #[test]
    fn test_diff_stats_thresholds() {
        let diff = r#"
diff --git a/file1.rs b/file1.rs
+++ b/file1.rs
+line1
+line2
diff --git a/file2.rs b/file2.rs
+++ b/file2.rs
+line1
"#;
        let (files, lines) = get_diff_stats(diff);
        assert_eq!(files, 2);
        assert_eq!(lines, 3);
    }

    #[test]
    fn test_non_interactive_fallback_warn() {
        let diff = "+++ b/src/main.rs\n+println!(\"hello\");";
        let temp_dir = std::env::temp_dir().join("murshid_clean_test1");
        let _ = fs::remove_dir_all(&temp_dir);
        fs::create_dir_all(&temp_dir).unwrap();

        let exit_code = run_clean_hook_with_diff(
            diff,
            false,
            false,
            &temp_dir,
            &mut std::io::empty(),
        );
        assert_eq!(exit_code, 0);

        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_interactive_quit_option() {
        let diff = "+++ b/src/main.rs\n+println!(\"hello\");";
        let temp_dir = std::env::temp_dir().join("murshid_clean_test2");
        let _ = fs::remove_dir_all(&temp_dir);
        fs::create_dir_all(&temp_dir).unwrap();

        let mut input = std::io::Cursor::new(b"q\n");
        let exit_code = run_clean_hook_with_diff(
            diff,
            false,
            true,
            &temp_dir,
            &mut input,
        );
        assert_eq!(exit_code, 0);

        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_interactive_strip_and_comment() {
        let temp_dir = std::env::temp_dir().join("murshid_clean_test3");
        let _ = fs::remove_dir_all(&temp_dir);
        fs::create_dir_all(&temp_dir.join("src")).unwrap();

        // 1. Create file with staged changes
        let file_path = temp_dir.join("src/main.rs");
        fs::write(
            &file_path,
            "fn main() {\n    println!(\"hello\");\n    let x = 10;\n    dbg!(x);\n}\n",
        )
        .unwrap();

        // 2. Mock diff showing println! at line 2 and dbg! at line 4
        let diff = r#"
diff --git a/src/main.rs b/src/main.rs
--- a/src/main.rs
+++ b/src/main.rs
@@ -1,5 +1,5 @@
 fn main() {
+    println!("hello");
     let x = 10;
+    dbg!(x);
 }
"#;

        // Strip the first, comment the second
        let mut input = std::io::Cursor::new(b"s\nc\n");
        let exit_code = run_clean_hook_with_diff(
            diff,
            false,
            true,
            &temp_dir,
            &mut input,
        );

        assert_eq!(exit_code, 0);

        // 3. Read back file and assert contents
        let final_content = fs::read_to_string(&file_path).unwrap();
        // println! at line 2 is stripped, let x is at line 2, and dbg! is commented out
        let expected = "fn main() {\n    let x = 10;\n    #[cfg(test)] dbg!(x);\n}\n";
        assert_eq!(final_content, expected);

        let _ = fs::remove_dir_all(&temp_dir);
    }
}
