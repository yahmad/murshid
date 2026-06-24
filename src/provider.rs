use std::process::Child;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct DialogueTurn {
    pub role: String, // "user" or "assistant"
    pub code_context: Option<String>,
    pub error_code: String,
    pub message: String,
}

struct ActiveConnection {
    child: Option<Child>,
}

static ACTIVE_CONN: OnceLock<Mutex<ActiveConnection>> = OnceLock::new();
static CURRENT_REQUEST_ID: AtomicU64 = AtomicU64::new(0);

fn get_active_conn() -> &'static Mutex<ActiveConnection> {
    ACTIVE_CONN.get_or_init(|| Mutex::new(ActiveConnection { child: None }))
}

pub fn abort_active_connection() {
    let mut conn = get_active_conn().lock().unwrap();
    if let Some(mut child) = conn.child.take() {
        let _ = child.kill();
    }
}

pub fn build_prompt(
    system_prompt: &str,
    history: &[DialogueTurn],
    history_limit: usize,
    format: &str, // "chatml" | "llama3" | "default"
) -> String {
    let mut processed_turns = Vec::new();
    let total_turns = history.len();
    for (idx, turn) in history.iter().enumerate() {
        let mut new_turn = turn.clone();
        if total_turns > history_limit && idx < total_turns - history_limit {
            new_turn.code_context = None;
        }
        processed_turns.push(new_turn);
    }

    match format {
        "chatml" => format_chatml(system_prompt, &processed_turns),
        "llama3" => format_llama3(system_prompt, &processed_turns),
        _ => format_default(system_prompt, &processed_turns),
    }
}

fn format_turn_content(turn: &DialogueTurn) -> String {
    if let Some(ref code) = turn.code_context {
        format!(
            "Error Code: {}\nCode Context:\n{}\nMessage: {}",
            turn.error_code, code, turn.message
        )
    } else {
        format!("Error Code: {}\nMessage: {}", turn.error_code, turn.message)
    }
}

fn format_chatml(system_prompt: &str, turns: &[DialogueTurn]) -> String {
    let mut prompt = format!("<|im_start|>system\n{}<|im_end|>\n", system_prompt);
    for turn in turns {
        let role = if turn.role == "user" {
            "user"
        } else {
            "assistant"
        };
        prompt.push_str(&format!(
            "<|im_start|>{}\n{}<|im_end|>\n",
            role,
            format_turn_content(turn)
        ));
    }
    prompt.push_str("<|im_start|>assistant\n");
    prompt
}

fn format_llama3(system_prompt: &str, turns: &[DialogueTurn]) -> String {
    let mut prompt = format!(
        "<|begin_of_text|><|start_header_id|>system<|end_header_id|>\n\n{}<|eot_id|>",
        system_prompt
    );
    for turn in turns {
        let role = if turn.role == "user" {
            "user"
        } else {
            "assistant"
        };
        prompt.push_str(&format!(
            "<|start_header_id|>{}\n\n{}<|eot_id|>",
            role,
            format_turn_content(turn)
        ));
    }
    prompt.push_str("<|start_header_id|>assistant<|end_header_id|>\n\n");
    prompt
}

fn format_default(system_prompt: &str, turns: &[DialogueTurn]) -> String {
    let mut prompt = format!("System: {}\n\n", system_prompt);
    for turn in turns {
        let role = if turn.role == "user" {
            "User"
        } else {
            "Assistant"
        };
        prompt.push_str(&format!("{}: {}\n\n", role, format_turn_content(turn)));
    }
    prompt.push_str("Assistant: ");
    prompt
}

pub fn dispatch_debounced(
    provider_type: &str,
    prompt: &str,
    api_key: Option<&str>,
) -> Result<String, String> {
    let req_id = CURRENT_REQUEST_ID.fetch_add(1, Ordering::SeqCst) + 1;

    abort_active_connection();

    let sleep_step = Duration::from_millis(100);
    for _ in 0..15 {
        std::thread::sleep(sleep_step);
        if CURRENT_REQUEST_ID.load(Ordering::SeqCst) != req_id {
            return Err("Aborted by new request".to_string());
        }
    }

    run_query_with_child_tracking(req_id, provider_type, prompt, api_key)
}

fn run_query_with_child_tracking(
    req_id: u64,
    provider_type: &str,
    prompt: &str,
    api_key: Option<&str>,
) -> Result<String, String> {
    let (url, headers, body) = match provider_type {
        "gemini" => {
            let key = api_key.unwrap_or("");
            let url = format!(
                "https://generativelanguage.googleapis.com/v1beta/models/gemini-1.5-flash:generateContent?key={}",
                key
            );
            let headers = vec![("Content-Type", "application/json")];
            let body_json = serde_json::json!({
                "contents": [{
                    "parts": [{
                        "text": prompt
                    }]
                }]
            });
            (url, headers, body_json.to_string())
        }
        "claude" => {
            let key = api_key.unwrap_or("");
            let url = "https://api.anthropic.com/v1/messages".to_string();
            let headers = vec![
                ("x-api-key", key),
                ("anthropic-version", "2023-06-01"),
                ("Content-Type", "application/json"),
            ];
            let body_json = serde_json::json!({
                "model": "claude-3-5-sonnet-20241022",
                "max_tokens": 1024,
                "messages": [{
                    "role": "user",
                    "content": prompt
                }]
            });
            (url, headers, body_json.to_string())
        }
        "ollama" => {
            let url = "http://localhost:11434/api/generate".to_string();
            let headers = vec![("Content-Type", "application/json")];
            let body_json = serde_json::json!({
                "model": "llama3",
                "prompt": prompt,
                "stream": false
            });
            (url, headers, body_json.to_string())
        }
        _ => return Err(format!("Unknown provider type: {}", provider_type)),
    };

    let mut cmd = std::process::Command::new("curl");
    cmd.args(["-s", "-X", "POST", &url]);
    for (k, v) in headers {
        cmd.args(["-H", &format!("{}: {}", k, v)]);
    }
    cmd.args(["-d", &body]);

    let mut child = cmd
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to spawn curl: {}", e))?;

    {
        let mut conn = get_active_conn().lock().unwrap();
        if CURRENT_REQUEST_ID.load(Ordering::SeqCst) != req_id {
            let _ = child.kill();
            return Err("Aborted".to_string());
        }
        conn.child = Some(child);
    }

    let child_opt = {
        let mut conn = get_active_conn().lock().unwrap();
        conn.child.take()
    };

    if let Some(child) = child_opt {
        let output = child
            .wait_with_output()
            .map_err(|e| format!("curl execution failed: {}", e))?;
        if output.status.success() {
            let res_str = String::from_utf8(output.stdout)
                .map_err(|e| format!("Invalid UTF-8 response: {}", e))?;
            parse_provider_response(provider_type, &res_str)
        } else {
            let err_str = String::from_utf8_lossy(&output.stderr).to_string();
            Err(format!("curl error: {}", err_str))
        }
    } else {
        Err("Aborted".to_string())
    }
}

pub fn parse_provider_response(provider_type: &str, response: &str) -> Result<String, String> {
    let val: serde_json::Value = serde_json::from_str(response)
        .map_err(|e| format!("Failed to parse JSON response: {}", e))?;

    match provider_type {
        "gemini" => val
            .get("candidates")
            .and_then(|c| c.as_array())
            .and_then(|arr| arr.first())
            .and_then(|first| first.get("content"))
            .and_then(|c| c.get("parts"))
            .and_then(|parts| parts.as_array())
            .and_then(|parts_arr| parts_arr.first())
            .and_then(|part| part.get("text"))
            .and_then(|t| t.as_str())
            .map(|t| t.to_string())
            .ok_or_else(|| "Failed to extract text from Gemini response".to_string()),
        "claude" => val
            .get("content")
            .and_then(|c| c.as_array())
            .and_then(|arr| arr.first())
            .and_then(|first| first.get("text"))
            .and_then(|t| t.as_str())
            .map(|t| t.to_string())
            .ok_or_else(|| "Failed to extract text from Claude response".to_string()),
        "ollama" => val
            .get("response")
            .and_then(|t| t.as_str())
            .map(|t| t.to_string())
            .ok_or_else(|| "Failed to extract text from Ollama response".to_string()),
        _ => Err("Unknown provider".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sliding_window_pruning() {
        let history = vec![
            DialogueTurn {
                role: "user".to_string(),
                code_context: Some("code 1".to_string()),
                error_code: "E0382".to_string(),
                message: "msg 1".to_string(),
            },
            DialogueTurn {
                role: "assistant".to_string(),
                code_context: None,
                error_code: "E0382".to_string(),
                message: "msg 2".to_string(),
            },
            DialogueTurn {
                role: "user".to_string(),
                code_context: Some("code 3".to_string()),
                error_code: "E0382".to_string(),
                message: "msg 3".to_string(),
            },
            DialogueTurn {
                role: "assistant".to_string(),
                code_context: None,
                error_code: "E0382".to_string(),
                message: "msg 4".to_string(),
            },
            DialogueTurn {
                role: "user".to_string(),
                code_context: Some("code 5".to_string()),
                error_code: "E0382".to_string(),
                message: "msg 5".to_string(),
            },
        ];

        // Restrict to history limit 3.
        // History has 5 turns. Index 0 and 1 are older than sliding window.
        // Index 2, 3, 4 are within the sliding window of 3.
        // Therefore, turn 0's code_context must be pruned (None).
        // Turn 2's code_context must remain (Some("code 3")).
        // Turn 4's code_context must remain (Some("code 5")).
        let prompt = build_prompt("System rules", &history, 3, "default");

        assert!(!prompt.contains("code 1"));
        assert!(prompt.contains("code 3"));
        assert!(prompt.contains("code 5"));
    }

    #[test]
    fn test_formatting_templates() {
        let history = vec![DialogueTurn {
            role: "user".to_string(),
            code_context: Some("code".to_string()),
            error_code: "E0382".to_string(),
            message: "msg".to_string(),
        }];

        let chatml = build_prompt("System rules", &history, 3, "chatml");
        assert!(chatml.starts_with("<|im_start|>system\nSystem rules<|im_end|>\n"));
        assert!(chatml.contains(
            "<|im_start|>user\nError Code: E0382\nCode Context:\ncode\nMessage: msg<|im_end|>\n"
        ));
        assert!(chatml.ends_with("<|im_start|>assistant\n"));

        let llama3 = build_prompt("System rules", &history, 3, "llama3");
        assert!(llama3.starts_with(
            "<|begin_of_text|><|start_header_id|>system<|end_header_id|>\n\nSystem rules<|eot_id|>"
        ));
        assert!(llama3.contains("<|start_header_id|>user\n\nError Code: E0382\nCode Context:\ncode\nMessage: msg<|eot_id|>"));
        assert!(llama3.ends_with("<|start_header_id|>assistant<|end_header_id|>\n\n"));
    }

    #[test]
    fn test_parse_gemini_response() {
        let response = r#"{
            "candidates": [{
                "content": {
                    "parts": [{
                        "text": "Gemini Socratic hint answer"
                    }]
                }
            }]
        }"#;
        let parsed = parse_provider_response("gemini", response).unwrap();
        assert_eq!(parsed, "Gemini Socratic hint answer");
    }

    #[test]
    fn test_parse_claude_response() {
        let response = r#"{
            "content": [{
                "type": "text",
                "text": "Claude Socratic hint answer"
            }]
        }"#;
        let parsed = parse_provider_response("claude", response).unwrap();
        assert_eq!(parsed, "Claude Socratic hint answer");
    }

    #[test]
    fn test_parse_ollama_response() {
        let response = r#"{
            "response": "Ollama Socratic hint answer"
        }"#;
        let parsed = parse_provider_response("ollama", response).unwrap();
        assert_eq!(parsed, "Ollama Socratic hint answer");
    }

    #[test]
    fn test_debounce_cancellation() {
        // Spawn two debounced dispatches in quick succession
        let handle1 = std::thread::spawn(|| dispatch_debounced("ollama", "prompt 1", None));

        // Wait 100ms
        std::thread::sleep(Duration::from_millis(100));

        let handle2 = std::thread::spawn(|| dispatch_debounced("ollama", "prompt 2", None));

        let res1 = handle1.join().unwrap();
        let res2 = handle2.join().unwrap();

        // The first request should have been cancelled/aborted because the second one took over
        assert!(res1.is_err());
        assert_eq!(res1.err().unwrap(), "Aborted by new request");

        // The second one will try to execute (and fail since Ollama is probably not running, but it won't be "Aborted by new request")
        if let Err(ref e) = res2 {
            assert_ne!(e, "Aborted by new request");
        }
    }
}
