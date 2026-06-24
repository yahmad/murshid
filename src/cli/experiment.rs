use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

static LAST_ACTIVITY: OnceLock<Mutex<Instant>> = OnceLock::new();

pub fn record_compile_activity() {
    if let Some(m) = LAST_ACTIVITY.get() {
        if let Ok(mut guard) = m.lock() {
            *guard = Instant::now();
        }
    }
}

pub fn notify_watcher_of_experiment(path: PathBuf) {
    // 1. Register in-process watcher
    crate::watcher::register_watch_path(path.clone());

    // 2. Register with daemon via UDS if online
    let socket_path = crate::lsp_proxy::get_socket_path();
    if let Ok(mut stream) = std::os::unix::net::UnixStream::connect(&socket_path) {
        let client_pid = std::process::id();
        let handshake = serde_json::json!({
            "workspace_root": path.to_string_lossy().to_string(),
            "client_pid": client_pid,
        });
        if stream.write_all(format!("{}\n", handshake).as_bytes()).is_ok() && stream.flush().is_ok() {
            let msg = format!("watch_path:{}\n", path.to_string_lossy());
            let _ = stream.write_all(msg.as_bytes());
            let _ = stream.flush();
        }
    }
}

pub fn run_experiment_cli(args: &[String]) -> i32 {
    if args.is_empty() {
        eprintln!("Usage: murshid experiment <start|merge|prune> [args] or --repair");
        return 1;
    }

    match args[0].as_str() {
        "start" => {
            if args.len() < 2 {
                eprintln!("Error: missing experiment name. Usage: murshid experiment start <name>");
                return 1;
            }
            if let Err(e) = start_experiment(&args[1]) {
                eprintln!("Error starting experiment: {}", e);
                return 1;
            }
            0
        }
        "merge" => {
            if let Err(e) = merge_experiment() {
                eprintln!("Error merging experiment: {}", e);
                return 1;
            }
            0
        }
        "prune" => {
            let mut older_than = 0;
            let mut i = 1;
            while i < args.len() {
                if args[i] == "--older-than" && i + 1 < args.len() {
                    if let Ok(days) = args[i + 1].parse::<u64>() {
                        older_than = days;
                    }
                    i += 2;
                } else {
                    i += 1;
                }
            }
            if let Err(e) = prune_experiments(older_than) {
                eprintln!("Error pruning: {}", e);
                return 1;
            }
            0
        }
        "--repair" => {
            if let Err(e) = repair_experiments() {
                eprintln!("Error repairing: {}", e);
                return 1;
            }
            0
        }
        _ => {
            eprintln!("Unknown experiment subcommand: {}", args[0]);
            1
        }
    }
}

pub fn start_experiment(name: &str) -> Result<(), String> {
    let project_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

    // 1. Verify workspace is clean
    let status = std::process::Command::new("git")
        .args(&["diff-index", "--quiet", "HEAD", "--"])
        .current_dir(&project_root)
        .status();
    if let Ok(stat) = status {
        if !stat.success() {
            return Err("Uncommitted changes in active workspace.".to_string());
        }
    } else {
        return Err("Failed to run git diff-index".to_string());
    }

    // 2. Append to gitignore if missing
    let gitignore_path = project_root.join(".gitignore");
    let mut gitignore_content = if gitignore_path.exists() {
        fs::read_to_string(&gitignore_path).unwrap_or_default()
    } else {
        String::new()
    };

    if !gitignore_content.lines().any(|l| l.trim() == ".murshid_experiments/") {
        if !gitignore_content.is_empty() && !gitignore_content.ends_with('\n') {
            gitignore_content.push('\n');
        }
        gitignore_content.push_str("\n# Murshid experimental worktrees\n.murshid_experiments/\n");
        fs::write(&gitignore_path, gitignore_content).map_err(|e| e.to_string())?;
    }

    // 3. Git worktree add
    let worktree_dir = format!(".murshid_experiments/murshid-experiment-{}", name);
    let branch_name = format!("experiment/{}", name);

    // Delete existing branch if it exists to avoid conflicts
    let _ = std::process::Command::new("git")
        .args(&["branch", "-D", &branch_name])
        .current_dir(&project_root)
        .status();

    let output = std::process::Command::new("git")
        .args(&["worktree", "add", &worktree_dir, "-b", &branch_name])
        .current_dir(&project_root)
        .output();
    match output {
        Ok(out) => {
            if !out.status.success() {
                return Err(format!("git worktree add failed: {}", String::from_utf8_lossy(&out.stderr)));
            }
        }
        Err(e) => return Err(format!("Failed to execute git worktree: {}", e)),
    }

    // 4. Configure local isolated target directory for the worktree
    let cargo_config_dir = project_root.join(&worktree_dir).join(".cargo");
    fs::create_dir_all(&cargo_config_dir).map_err(|e| e.to_string())?;
    let cargo_config_path = cargo_config_dir.join("config.toml");
    fs::write(&cargo_config_path, "[build]\ntarget-dir = \"target\"\n").map_err(|e| e.to_string())?;

    // 5. IDE discovery integration
    let _ = update_vscode_workspace(&project_root, name);
    let _ = update_neovim_workspace(&project_root, name);

    // 6. Transition watch focus
    let exp_root = project_root.join(&worktree_dir);
    notify_watcher_of_experiment(exp_root);

    Ok(())
}

pub fn merge_experiment() -> Result<(), String> {
    let project_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

    let active = get_active_experiments(&project_root)?;
    if active.is_empty() {
        return Err("No active experiments found.".to_string());
    }

    // Merge the first active experiment discovered
    let (exp_path, branch) = &active[0];
    let name = branch.replace("experiment/", "");

    // 1. Run cargo check and cargo test in experiment directory
    let check = std::process::Command::new("cargo")
        .arg("check")
        .current_dir(exp_path)
        .status();
    if !check.is_ok() || !check.unwrap().success() {
        return Err("cargo check failed in experiment workspace.".to_string());
    }

    let test = std::process::Command::new("cargo")
        .arg("test")
        .current_dir(exp_path)
        .status();
    if !test.is_ok() || !test.unwrap().success() {
        return Err("cargo test failed in experiment workspace.".to_string());
    }

    // 2. Commit experiment workspace changes
    let _ = std::process::Command::new("git")
        .args(&["add", "."])
        .current_dir(exp_path)
        .status();
    let _ = std::process::Command::new("git")
        .args(&["commit", "-am", &format!("experiment: completed {}", name)])
        .current_dir(exp_path)
        .status();

    // 3. Checkout main/master in primary root
    let mut main_branch = "main".to_string();
    let checkout_main = std::process::Command::new("git")
        .args(&["checkout", "main"])
        .current_dir(&project_root)
        .status();
    if !checkout_main.is_ok() || !checkout_main.unwrap().success() {
        let checkout_master = std::process::Command::new("git")
            .args(&["checkout", "master"])
            .current_dir(&project_root)
            .status();
        if checkout_master.is_ok() && checkout_master.unwrap().success() {
            main_branch = "master".to_string();
        }
    }

    // 4. Merge experiment branch
    let merge = std::process::Command::new("git")
        .args(&["merge", branch])
        .current_dir(&project_root)
        .status();
    if !merge.is_ok() || !merge.unwrap().success() {
        return Err(format!("Failed to merge branch {} into {}", branch, main_branch));
    }

    // 5. Remove worktree
    let _ = std::process::Command::new("git")
        .args(&["worktree", "remove", &exp_path.to_string_lossy()])
        .current_dir(&project_root)
        .status();
    let _ = std::process::Command::new("git")
        .args(&["worktree", "remove", "--force", &exp_path.to_string_lossy()])
        .current_dir(&project_root)
        .status();

    // 6. Delete experiment branch
    let _ = std::process::Command::new("git")
        .args(&["branch", "-d", branch])
        .current_dir(&project_root)
        .status();
    let _ = std::process::Command::new("git")
        .args(&["branch", "-D", branch])
        .current_dir(&project_root)
        .status();

    // 7. Clean up IDE configuration files
    let _ = remove_vscode_workspace(&project_root, &name);
    let _ = remove_neovim_workspace(&project_root, &name);

    Ok(())
}

pub fn repair_experiments() -> Result<(), String> {
    let project_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

    // Run git worktree prune
    let _ = std::process::Command::new("git")
        .args(&["worktree", "prune"])
        .current_dir(&project_root)
        .status();

    let active = get_active_experiments(&project_root)?;

    // Rebuild VS Code worktree discovery settings
    let project_name = project_root
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "murshid".to_string());
    let workspace_path = project_root.join(format!("{}.code-workspace", project_name));
    if workspace_path.exists() {
        let _ = fs::remove_file(&workspace_path);
    }
    let coc_path = project_root.join("coc-settings.json");
    if coc_path.exists() {
        let _ = fs::remove_file(&coc_path);
    }

    for (exp_path, branch) in &active {
        let name = branch.replace("experiment/", "");
        let _ = update_vscode_workspace(&project_root, &name);
        let _ = update_neovim_workspace(&project_root, &name);

        // Make sure local target config is restored
        let cargo_config_dir = exp_path.join(".cargo");
        let _ = fs::create_dir_all(&cargo_config_dir);
        let _ = fs::write(cargo_config_dir.join("config.toml"), "[build]\ntarget-dir = \"target\"\n");
    }

    // Clean up physical folders in .murshid_experiments that are not registered
    let experiments_dir = project_root.join(".murshid_experiments");
    if experiments_dir.exists() {
        if let Ok(entries) = fs::read_dir(&experiments_dir) {
            for entry in entries.flatten() {
                if let Ok(file_type) = entry.file_type() {
                    if file_type.is_dir() {
                        let path = entry.path();
                        let is_active = active.iter().any(|(active_path, _)| {
                            if let (Ok(p1), Ok(p2)) = (path.canonicalize(), active_path.canonicalize()) {
                                p1 == p2
                            } else {
                                false
                            }
                        });
                        if !is_active {
                            let _ = fs::remove_dir_all(&path);
                        }
                    }
                }
            }
        }
    }

    Ok(())
}

pub fn prune_experiments(older_than_days: u64) -> Result<(), String> {
    let project_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

    let _ = repair_experiments();

    let active = get_active_experiments(&project_root)?;
    let now = std::time::SystemTime::now();

    for (exp_path, branch) in active {
        let name = branch.replace("experiment/", "");
        if let Ok(metadata) = fs::metadata(&exp_path) {
            if let Ok(modified) = metadata.modified() {
                if let Ok(duration) = now.duration_since(modified) {
                    let days = duration.as_secs() / (24 * 3600);
                    if days >= older_than_days {
                        let _ = std::process::Command::new("git")
                            .args(&["worktree", "remove", "--force", &exp_path.to_string_lossy()])
                            .current_dir(&project_root)
                            .status();
                        let _ = std::process::Command::new("git")
                            .args(&["branch", "-D", &branch])
                            .current_dir(&project_root)
                            .status();
                        let _ = remove_vscode_workspace(&project_root, &name);
                        let _ = remove_neovim_workspace(&project_root, &name);
                    }
                }
            }
        }
    }

    Ok(())
}

pub fn start_background_pruner(project_root: PathBuf) {
    let _ = LAST_ACTIVITY.get_or_init(|| Mutex::new(Instant::now()));

    std::thread::spawn(move || {
        let config = crate::config::load_config();
        
        let limit_str = config.watcher.target_cache_limit.to_lowercase();
        let limit_bytes: u64 = if limit_str.ends_with("gb") {
            limit_str.trim_end_matches("gb").trim().parse::<u64>().unwrap_or(5) * 1024 * 1024 * 1024
        } else if limit_str.ends_with("mb") {
            limit_str.trim_end_matches("mb").trim().parse::<u64>().unwrap_or(5000) * 1024 * 1024
        } else {
            limit_str.parse::<u64>().unwrap_or(5 * 1024 * 1024 * 1024)
        };

        loop {
            std::thread::sleep(Duration::from_secs(60));

            let last = if let Some(m) = LAST_ACTIVITY.get() {
                if let Ok(guard) = m.lock() {
                    *guard
                } else {
                    Instant::now()
                }
            } else {
                Instant::now()
            };

            if last.elapsed() >= Duration::from_secs(1800) {
                let _ = prune_orphaned_and_stale_caches(&project_root);
            }

            let experiments_dir = project_root.join(".murshid_experiments");
            if experiments_dir.exists() {
                let size = crate::watcher_coordinator::get_dir_size(&experiments_dir);
                if size > limit_bytes {
                    eprintln!(
                        "[WARNING] Socratic disk space warning: experimental build cache is {} bytes, which exceeds the limit of {} bytes.",
                        size, limit_bytes
                    );
                }
            }
        }
    });
}

fn prune_orphaned_and_stale_caches(project_root: &Path) -> Result<(), String> {
    let _ = std::process::Command::new("git")
        .args(&["worktree", "prune"])
        .current_dir(project_root)
        .status();

    let active_worktrees = get_active_experiments(project_root)?;
    let experiments_dir = project_root.join(".murshid_experiments");
    if experiments_dir.exists() {
        if let Ok(entries) = fs::read_dir(&experiments_dir) {
            for entry in entries.flatten() {
                if let Ok(file_type) = entry.file_type() {
                    if file_type.is_dir() {
                        let path = entry.path();
                        let is_active = active_worktrees.iter().any(|(wt_path, _)| {
                            if let (Ok(p1), Ok(p2)) = (path.canonicalize(), wt_path.canonicalize()) {
                                p1 == p2
                            } else {
                                false
                            }
                        });
                        if !is_active {
                            let _ = fs::remove_dir_all(&path);
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

fn get_active_experiments(project_root: &Path) -> Result<Vec<(PathBuf, String)>, String> {
    let output = std::process::Command::new("git")
        .args(&["worktree", "list"])
        .current_dir(project_root)
        .output()
        .map_err(|e| e.to_string())?;

    let mut exps = Vec::new();
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        if line.contains(".murshid_experiments/") {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 3 {
                let path = PathBuf::from(parts[0]);
                let branch_part = parts[parts.len() - 1];
                if branch_part.starts_with('[') && branch_part.ends_with(']') {
                    let branch = branch_part[1..branch_part.len() - 1].to_string();
                    if branch.starts_with("experiment/") {
                        exps.push((path, branch));
                    }
                }
            }
        }
    }
    Ok(exps)
}

fn update_vscode_workspace(project_root: &Path, name: &str) -> Result<(), String> {
    let project_name = project_root
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "murshid".to_string());
    let workspace_path = project_root.join(format!("{}.code-workspace", project_name));
    
    let mut workspace_json = if workspace_path.exists() {
        let content = fs::read_to_string(&workspace_path).unwrap_or_default();
        serde_json::from_str::<serde_json::Value>(&content).unwrap_or_else(|_| serde_json::json!({}))
    } else {
        serde_json::json!({})
    };

    let mut folders = workspace_json
        .get("folders")
        .and_then(|f| f.as_array())
        .cloned()
        .unwrap_or_else(Vec::new);

    if !folders.iter().any(|f| f.get("path").and_then(|p| p.as_str()) == Some(".")) {
        folders.push(serde_json::json!({ "name": "Primary", "path": "." }));
    }

    let exp_path = format!(".murshid_experiments/murshid-experiment-{}", name);
    if !folders.iter().any(|f| f.get("path").and_then(|p| p.as_str()) == Some(&exp_path)) {
        folders.push(serde_json::json!({
            "name": format!("Experiment ({})", name),
            "path": exp_path
        }));
    }
    workspace_json["folders"] = serde_json::Value::Array(folders);

    let mut settings = workspace_json
        .get("settings")
        .and_then(|s| s.as_object())
        .cloned()
        .unwrap_or_else(serde_json::Map::new);

    let mut linked_projects = settings
        .get("rust-analyzer.linkedProjects")
        .and_then(|lp| lp.as_array())
        .cloned()
        .unwrap_or_else(|| vec![serde_json::json!("./Cargo.toml")]);

    let exp_cargo = format!("./.murshid_experiments/murshid-experiment-{}/Cargo.toml", name);
    if !linked_projects.iter().any(|lp| lp.as_str() == Some(&exp_cargo)) {
        linked_projects.push(serde_json::json!(exp_cargo));
    }
    settings.insert("rust-analyzer.linkedProjects".to_string(), serde_json::Value::Array(linked_projects));

    let mut files_exclude = settings
        .get("files.exclude")
        .and_then(|fe| fe.as_object())
        .cloned()
        .unwrap_or_else(serde_json::Map::new);
    files_exclude.insert("**/.murshid_experiments/**/target".to_string(), serde_json::Value::Bool(true));
    settings.insert("files.exclude".to_string(), serde_json::Value::Object(files_exclude));

    workspace_json["settings"] = serde_json::Value::Object(settings);

    let pretty = serde_json::to_string_pretty(&workspace_json).map_err(|e| e.to_string())?;
    fs::write(&workspace_path, pretty).map_err(|e| e.to_string())?;
    Ok(())
}

fn update_neovim_workspace(project_root: &Path, name: &str) -> Result<(), String> {
    let coc_path = project_root.join("coc-settings.json");
    let mut coc_json = if coc_path.exists() {
        let content = fs::read_to_string(&coc_path).unwrap_or_default();
        serde_json::from_str::<serde_json::Value>(&content).unwrap_or_else(|_| serde_json::json!({}))
    } else {
        serde_json::json!({})
    };

    let mut linked_projects = coc_json
        .get("rust-analyzer.linkedProjects")
        .and_then(|lp| lp.as_array())
        .cloned()
        .unwrap_or_else(|| vec![serde_json::json!("Cargo.toml")]);

    let exp_cargo = format!(".murshid_experiments/murshid-experiment-{}/Cargo.toml", name);
    if !linked_projects.iter().any(|lp| lp.as_str() == Some(&exp_cargo)) {
        linked_projects.push(serde_json::json!(exp_cargo));
    }
    coc_json["rust-analyzer.linkedProjects"] = serde_json::Value::Array(linked_projects);

    let pretty = serde_json::to_string_pretty(&coc_json).map_err(|e| e.to_string())?;
    fs::write(&coc_path, pretty).map_err(|e| e.to_string())?;
    Ok(())
}

fn remove_vscode_workspace(project_root: &Path, name: &str) -> Result<(), String> {
    let project_name = project_root
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "murshid".to_string());
    let workspace_path = project_root.join(format!("{}.code-workspace", project_name));
    if workspace_path.exists() {
        if let Ok(content) = fs::read_to_string(&workspace_path) {
            if let Ok(mut workspace_json) = serde_json::from_str::<serde_json::Value>(&content) {
                if let Some(folders) = workspace_json.get_mut("folders").and_then(|f| f.as_array_mut()) {
                    let exp_path = format!(".murshid_experiments/murshid-experiment-{}", name);
                    folders.retain(|f| f.get("path").and_then(|p| p.as_str()) != Some(&exp_path));
                }
                if let Some(settings) = workspace_json.get_mut("settings").and_then(|s| s.as_object_mut()) {
                    if let Some(linked_projects) = settings.get_mut("rust-analyzer.linkedProjects").and_then(|lp| lp.as_array_mut()) {
                        let exp_cargo = format!("./.murshid_experiments/murshid-experiment-{}/Cargo.toml", name);
                        linked_projects.retain(|lp| lp.as_str() != Some(&exp_cargo));
                    }
                }
                let _ = serde_json::to_string_pretty(&workspace_json).map(|json| fs::write(&workspace_path, json));
            }
        }
    }
    Ok(())
}

fn remove_neovim_workspace(project_root: &Path, name: &str) -> Result<(), String> {
    let coc_path = project_root.join("coc-settings.json");
    if coc_path.exists() {
        if let Ok(content) = fs::read_to_string(&coc_path) {
            if let Ok(mut coc_json) = serde_json::from_str::<serde_json::Value>(&content) {
                if let Some(linked_projects) = coc_json.get_mut("rust-analyzer.linkedProjects").and_then(|lp| lp.as_array_mut()) {
                    let exp_cargo = format!(".murshid_experiments/murshid-experiment-{}/Cargo.toml", name);
                    linked_projects.retain(|lp| lp.as_str() != Some(&exp_cargo));
                }
                let _ = serde_json::to_string_pretty(&coc_json).map(|json| fs::write(&coc_path, json));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_update_vscode_and_neovim_configs() {
        let temp_dir = std::env::temp_dir().join("murshid_ide_test");
        let _ = fs::remove_dir_all(&temp_dir);
        fs::create_dir_all(&temp_dir).unwrap();

        // Run update configurations
        let res1 = update_vscode_workspace(&temp_dir, "test-exp");
        let res2 = update_neovim_workspace(&temp_dir, "test-exp");
        assert!(res1.is_ok());
        assert!(res2.is_ok());

        // Read VS Code workspace file
        let workspace_file = temp_dir.join("murshid_ide_test.code-workspace");
        assert!(workspace_file.exists());
        let workspace_content = fs::read_to_string(&workspace_file).unwrap();
        let val: serde_json::Value = serde_json::from_str(&workspace_content).unwrap();

        let folders = val.get("folders").unwrap().as_array().unwrap();
        assert_eq!(folders.len(), 2);
        assert_eq!(folders[0].get("path").unwrap().as_str().unwrap(), ".");
        assert!(folders[1].get("path").unwrap().as_str().unwrap().contains("test-exp"));

        // Read coc-settings
        let coc_file = temp_dir.join("coc-settings.json");
        assert!(coc_file.exists());
        let coc_content = fs::read_to_string(&coc_file).unwrap();
        let coc_val: serde_json::Value = serde_json::from_str(&coc_content).unwrap();
        let lp = coc_val.get("rust-analyzer.linkedProjects").unwrap().as_array().unwrap();
        assert_eq!(lp.len(), 2);

        // Remove/Clean up configurations
        remove_vscode_workspace(&temp_dir, "test-exp").unwrap();
        remove_neovim_workspace(&temp_dir, "test-exp").unwrap();

        let cleaned_workspace = fs::read_to_string(&workspace_file).unwrap();
        let cleaned_val: serde_json::Value = serde_json::from_str(&cleaned_workspace).unwrap();
        assert_eq!(cleaned_val.get("folders").unwrap().as_array().unwrap().len(), 1);

        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_record_compile_activity_and_dynamic_channel() {
        let (tx, rx) = std::sync::mpsc::channel();
        let _ = crate::watcher::WATCHER_ADD_PATH_TX.set(Mutex::new(tx));

        let test_path = PathBuf::from("/test/experiment/path");
        notify_watcher_of_experiment(test_path.clone());

        let received = rx.recv_timeout(Duration::from_millis(500)).unwrap();
        assert_eq!(received, test_path);

        let _ = LAST_ACTIVITY.get_or_init(|| Mutex::new(Instant::now()));
        record_compile_activity();
        let last = LAST_ACTIVITY.get().unwrap().lock().unwrap();
        assert!(last.elapsed() < Duration::from_secs(5));
    }
}
