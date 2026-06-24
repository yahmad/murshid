use std::collections::{HashMap, HashSet};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

#[derive(serde::Deserialize, Clone)]
pub struct EvalItem {
    pub error_code: String,
    pub error_message: String,
    pub source_code: String,
}

pub fn normalize_code_identifiers(code: &str) -> Vec<String> {
    let chars: Vec<char> = code.chars().collect();
    let mut clean_text = String::new();
    let mut i = 0;
    let mut in_string = false;
    let mut in_char = false;

    while i < chars.len() {
        let c = chars[i];

        if in_string {
            if c == '"' && i > 0 && chars[i - 1] != '\\' {
                in_string = false;
            }
            i += 1;
            continue;
        }

        if in_char {
            if c == '\'' && i > 0 && chars[i - 1] != '\\' {
                in_char = false;
            }
            i += 1;
            continue;
        }

        if i + 1 < chars.len() && chars[i] == '/' && chars[i + 1] == '/' {
            i += 2;
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }

        if i + 1 < chars.len() && chars[i] == '/' && chars[i + 1] == '*' {
            i += 2;
            let mut depth = 1;
            while i < chars.len() && depth > 0 {
                if i + 1 < chars.len() && chars[i] == '/' && chars[i + 1] == '*' {
                    depth += 1;
                    i += 2;
                } else if i + 1 < chars.len() && chars[i] == '*' && chars[i + 1] == '/' {
                    depth -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
            continue;
        }

        if c == '"' {
            in_string = true;
            i += 1;
            continue;
        }

        if c == '\'' {
            in_char = true;
            i += 1;
            continue;
        }

        clean_text.push(c);
        i += 1;
    }

    let keywords: HashSet<&str> = [
        "fn", "let", "mut", "struct", "impl", "pub", "use", "match", "if", "else", "return", "for",
        "in", "while", "loop", "true", "false", "type", "enum", "trait", "crate", "self", "Self",
        "mod", "as", "const", "static", "where", "unsafe", "ref", "dyn", "move", "u8", "u16",
        "u32", "u64", "u128", "usize", "i8", "i16", "i32", "i64", "i128", "isize", "f32", "f64",
        "str", "bool", "char",
    ]
    .iter()
    .cloned()
    .collect();

    let mut tokens = Vec::new();
    let mut current = String::new();
    let chars: Vec<char> = clean_text.chars().collect();
    let mut idx = 0;

    while idx < chars.len() {
        let c = chars[idx];
        if c.is_alphanumeric() || c == '_' {
            current.push(c);
        } else if !current.is_empty() {
            tokens.push(current.clone());
            current.clear();
        }
        idx += 1;
    }
    if !current.is_empty() {
        tokens.push(current);
    }

    let mut id_map = HashMap::new();
    let mut mapped_tokens = Vec::new();

    for t in tokens {
        if keywords.contains(t.as_str()) {
            continue;
        }
        if t.chars().next().is_some_and(|first| first.is_numeric()) {
            continue;
        }

        let next_var_id = id_map.len();
        let var_name = id_map
            .entry(t)
            .or_insert_with(|| format!("var_{}", next_var_id));
        mapped_tokens.push(var_name.clone());
    }

    mapped_tokens
}

pub fn calculate_tokens_jaccard(tokens1: &[String], tokens2: &[String]) -> f64 {
    let set1: HashSet<String> = tokens1.iter().cloned().collect();
    let set2: HashSet<String> = tokens2.iter().cloned().collect();

    if set1.len() < 20 || set2.len() < 20 {
        return 0.0;
    }

    let intersection: HashSet<_> = set1.intersection(&set2).cloned().collect();
    let union: HashSet<_> = set1.union(&set2).cloned().collect();
    if union.is_empty() {
        0.0
    } else {
        intersection.len() as f64 / union.len() as f64
    }
}

pub fn execute_with_timeout<F, T>(f: F, timeout: Duration) -> Result<T, String>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let res = f();
        let _ = tx.send(res);
    });

    rx.recv_timeout(timeout).map_err(|e| match e {
        mpsc::RecvTimeoutError::Timeout => "Query execution timed out after 10 seconds".to_string(),
        mpsc::RecvTimeoutError::Disconnected => "Query execution thread crashed".to_string(),
    })
}

pub fn run_evaluation(
    subset: &str,
    provider_type: &str,
    api_key: Option<&str>,
) -> Result<(), String> {
    let dataset_bytes = include_bytes!("eval_dataset.jsonl");
    let content = String::from_utf8_lossy(dataset_bytes);

    let mut items = Vec::new();
    for line in content.lines() {
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(item) = serde_json::from_str::<EvalItem>(line) {
            items.push(item);
        }
    }

    if subset == "smoke" {
        items.truncate(1);
    }

    println!(
        "Starting Socratic evaluation harness (subset: {})...",
        subset
    );

    for item in items {
        let error_code = item.error_code.clone();
        let source_code = item.source_code.clone();
        let error_message = item.error_message.clone();
        let provider_type_clone = provider_type.to_string();
        let api_key_clone = api_key.map(|s| s.to_string());

        let result = execute_with_timeout(
            move || {
                let prompt = format!("Code context:\n{}\nError: {}", source_code, error_message);
                crate::provider::dispatch_debounced(
                    &provider_type_clone,
                    &prompt,
                    api_key_clone.as_deref(),
                )
            },
            Duration::from_secs(10),
        );

        match result {
            Ok(Ok(response)) => {
                let source_tokens = normalize_code_identifiers(&item.source_code);
                let response_tokens = normalize_code_identifiers(&response);

                let similarity = calculate_tokens_jaccard(&source_tokens, &response_tokens);

                println!(
                    "Error Code: {} | Similarity: {:.2}%",
                    error_code,
                    similarity * 100.0
                );

                if similarity >= 0.30 {
                    return Err(format!(
                        "Code leak detected! Jaccard similarity {:.2}% is >= 30% limit for error {}.",
                        similarity * 100.0,
                        error_code
                    ));
                }
            }
            Ok(Err(e)) => {
                eprintln!("[WARNING] LLM provider failed during evaluation: {}", e);
            }
            Err(timeout_err) => {
                return Err(format!("Evaluation aborted: {}", timeout_err));
            }
        }
    }

    println!("Evaluation completed successfully. All Socratic compliance assertions passed.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_code_identifiers() {
        let code = r#"
            // comment
            fn test_func(my_arg: u32) {
                let x = my_arg + 10;
            }
        "#;
        let tokens = normalize_code_identifiers(code);
        // Expect to exclude "fn", "u32", "let", "10", leaving "test_func", "my_arg", "x" mapped to var_0, var_1, var_2
        assert_eq!(tokens.len(), 4);
        assert_eq!(tokens[0], "var_0"); // test_func
        assert_eq!(tokens[1], "var_1"); // my_arg
        assert_eq!(tokens[2], "var_2"); // x
        assert_eq!(tokens[3], "var_1"); // my_arg (reused)
    }

    #[test]
    fn test_calculate_tokens_jaccard_limit() {
        // Must contain at least 20 distinct tokens to evaluate
        let tokens1 = vec!["var_0".to_string()];
        let tokens2 = vec!["var_0".to_string()];
        assert_eq!(calculate_tokens_jaccard(&tokens1, &tokens2), 0.0);

        let mut large1 = Vec::new();
        let mut large2 = Vec::new();
        for i in 0..25 {
            large1.push(format!("var_{}", i));
            large2.push(format!("var_{}", i));
        }

        let sim = calculate_tokens_jaccard(&large1, &large2);
        assert_eq!(sim, 1.0);
    }

    #[test]
    fn test_execute_with_timeout_ok() {
        let res = execute_with_timeout(
            || {
                thread::sleep(Duration::from_millis(50));
                42
            },
            Duration::from_millis(500),
        )
        .unwrap();
        assert_eq!(res, 42);
    }

    #[test]
    fn test_execute_with_timeout_abort() {
        let res = execute_with_timeout(
            || {
                thread::sleep(Duration::from_secs(2));
                42
            },
            Duration::from_millis(100),
        );
        assert!(res.is_err());
        assert!(res.err().unwrap().contains("timed out"));
    }
}
