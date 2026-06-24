pub fn redact_markdown_blocks(completion: &str, user_identifiers: &[&str]) -> String {
    let mut result = String::new();
    let parts: Vec<&str> = completion.split("```").collect();

    for (idx, part) in parts.iter().enumerate() {
        if idx % 2 == 1 {
            let mut matches_identifier = false;
            for &id in user_identifiers {
                if !id.is_empty() && part.contains(id) {
                    matches_identifier = true;
                    break;
                }
            }

            if matches_identifier {
                result.push_str("[CODE_BLOCK_REDACTED: Socratic assistance policy forbids sharing direct code solutions. Think about the concepts instead!]");
            } else {
                result.push_str("```");
                result.push_str(part);
                result.push_str("```");
            }
        } else {
            result.push_str(part);
        }
    }
    result
}

pub fn redact_clippy_fixes(diagnostic_message: &str) -> String {
    let mut lines = Vec::new();
    for line in diagnostic_message.lines() {
        if line.trim().starts_with("help:") || line.trim().starts_with("suggestion:") {
            if let Some(backtick_pos) = line.find('`') {
                let hint = &line[..backtick_pos];
                lines.push(format!(
                    "{} [Code replacement withheld. Focus on the concept.]",
                    hint
                ));
            } else {
                lines.push(line.to_string());
            }
        } else {
            lines.push(line.to_string());
        }
    }
    lines.join("\n")
}

pub fn check_local_model_leaks(completion: &str, active_variables: &[&str]) -> String {
    let mut result = String::new();
    let parts: Vec<&str> = completion.split("```").collect();

    for (idx, part) in parts.iter().enumerate() {
        if idx % 2 == 1 {
            let mut match_count = 0;
            for &var in active_variables {
                if !var.is_empty() && part.contains(var) {
                    match_count += 1;
                }
            }

            if match_count >= 3 {
                result.push_str("[CODE_BLOCK_REDACTED: Socratic assistance policy forbids sharing direct code solutions. Think about the concepts instead!]");
            } else {
                result.push_str("```");
                result.push_str(part);
                result.push_str("```");
            }
        } else {
            result.push_str(part);
        }
    }
    result
}

pub fn extract_variables(code_context: &str) -> Vec<String> {
    let mut vars = std::collections::HashSet::new();
    let mut current = String::new();
    for c in code_context.chars() {
        if c.is_alphanumeric() || c == '_' {
            current.push(c);
        } else if !current.is_empty() {
            if current
                .chars()
                .next()
                .is_some_and(|first| first.is_lowercase())
            {
                vars.insert(current.clone());
            }
            current.clear();
        }
    }
    if !current.is_empty()
        && current
            .chars()
            .next()
            .is_some_and(|first| first.is_lowercase())
    {
        vars.insert(current);
    }
    vars.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_redact_markdown_blocks() {
        let completion = "You should fix the struct:\n```rust\nstruct MyConfig {\n    port: u16,\n}\n```\nLet me know if this helps.";
        let redacted = redact_markdown_blocks(completion, &["MyConfig"]);
        assert!(redacted.contains("[CODE_BLOCK_REDACTED: Socratic assistance policy forbids sharing direct code solutions. Think about the concepts instead!]"));
        assert!(!redacted.contains("struct MyConfig"));
    }

    #[test]
    fn test_redact_clippy_fixes() {
        let diag = "error[E0308]: mismatched types\n  help: use Option::is_some instead: `val.is_some()`\nerror: aborting due to previous error";
        let redacted = redact_clippy_fixes(diag);
        assert!(redacted.contains(
            "help: use Option::is_some instead:  [Code replacement withheld. Focus on the concept.]"
        ));
        assert!(!redacted.contains("`val.is_some()`"));
    }

    #[test]
    fn test_check_local_model_leaks() {
        let completion = "Try this code:\n```rust\nlet x = cfg.port + offset;\n```";

        // Match >=3 active variables (cfg, port, offset) -> Redacts it
        let redacted = check_local_model_leaks(completion, &["cfg", "port", "offset"]);
        assert!(redacted.contains("[CODE_BLOCK_REDACTED: Socratic assistance policy forbids sharing direct code solutions. Think about the concepts instead!]"));

        // Match < 3 active variables (cfg, port) -> Keeps it
        let kept = check_local_model_leaks(completion, &["cfg", "port"]);
        assert!(kept.contains("let x = cfg.port + offset;"));
    }

    #[test]
    fn test_extract_variables() {
        let code = "let my_var = cfg.port; let another = 10;";
        let vars = extract_variables(code);
        assert!(vars.iter().any(|v| v == "my_var"));
        assert!(vars.iter().any(|v| v == "cfg"));
        assert!(vars.iter().any(|v| v == "port"));
        assert!(vars.iter().any(|v| v == "another"));
    }
}
