use std::process::Child;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

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

pub fn dispatch_debounced(
    provider_type: &str,
    prompt: &str,
    api_key: Option<&str>,
) -> Result<String, String> {
    dispatch_debounced_with_model(provider_type, None, prompt, api_key, None)
}

/// C6 two-slot model seam: same debounce/abort semantics as
/// [`dispatch_debounced`], but threads a specific `model` (from
/// `[models.screen]`/`[models.judge]`) instead of the provider's hardcoded
/// default, so config can select e.g. a specific Ollama model.
///
/// T8 req 1-3: also threads an optional `base_url` override for the shared
/// OpenAI-compatible local-provider arm (`"ollama" | "lmstudio" | "openai"`)
/// — `None` falls back to each provider's alias default.
pub fn dispatch_debounced_with_model(
    provider_type: &str,
    model: Option<&str>,
    prompt: &str,
    api_key: Option<&str>,
    base_url: Option<&str>,
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

    run_query_with_child_tracking(req_id, provider_type, model, prompt, api_key, base_url)
}

/// (url, headers, body) for a built provider request.
type ProviderRequest = (String, Vec<(&'static str, String)>, String);

/// T8 req 2: alias resolution for the shared OpenAI-compatible local-
/// provider arm. An explicitly configured `base_url` always overrides the
/// alias default (nonstandard port, LAN box); `"openai"` has no alias
/// default and is a config error without one. Trailing slashes on an
/// explicit override are stripped so the `/chat/completions` join is clean.
fn resolve_base_url(provider_type: &str, base_url: Option<&str>) -> Result<String, String> {
    if let Some(explicit) = base_url {
        return Ok(explicit.trim_end_matches('/').to_string());
    }
    match provider_type {
        "ollama" => Ok("http://localhost:11434/v1".to_string()),
        "lmstudio" => Ok("http://localhost:1234/v1".to_string()),
        "openai" => Err(
            "provider \"openai\" requires a configured base_url (no alias default)".to_string(),
        ),
        _ => Err(format!("Unknown provider type: {}", provider_type)),
    }
}

/// Pure request builder (no I/O, no shared/global state) — split out so
/// model-threading (C6 two-slot seam) is testable without racing the
/// debounce globals `run_query_with_child_tracking`'s callers share.
fn build_provider_request(
    provider_type: &str,
    model: Option<&str>,
    prompt: &str,
    api_key: Option<&str>,
    base_url: Option<&str>,
) -> Result<ProviderRequest, String> {
    match provider_type {
        "gemini" => {
            let key = api_key.unwrap_or("");
            let model_id = model.unwrap_or("gemini-2.5-flash");
            let url = format!(
                "https://generativelanguage.googleapis.com/v1beta/models/{}:generateContent?key={}",
                model_id, key
            );
            let headers = vec![("Content-Type", "application/json".to_string())];
            let body_json = serde_json::json!({
                "contents": [{
                    "parts": [{
                        "text": prompt
                    }]
                }]
            });
            Ok((url, headers, body_json.to_string()))
        }
        "claude" => {
            let key = api_key.unwrap_or("").to_string();
            let model_id = model.unwrap_or("claude-3-5-sonnet-20241022");
            let url = "https://api.anthropic.com/v1/messages".to_string();
            let headers = vec![
                ("x-api-key", key),
                ("anthropic-version", "2023-06-01".to_string()),
                ("Content-Type", "application/json".to_string()),
            ];
            let body_json = serde_json::json!({
                "model": model_id,
                "max_tokens": 1024,
                "messages": [{
                    "role": "user",
                    "content": prompt
                }]
            });
            Ok((url, headers, body_json.to_string()))
        }
        // T8: the shared OpenAI-compatible local-provider arm — Ollama
        // (native /v1 since 2024), LM Studio, and a generic OpenAI-
        // compatible endpoint all speak the same POST
        // {base_url}/chat/completions shape (req 3).
        "ollama" | "lmstudio" | "openai" => {
            let base = resolve_base_url(provider_type, base_url)?;
            let model_id = model.unwrap_or(if provider_type == "ollama" {
                "llama3"
            } else {
                ""
            });
            let url = format!("{}/chat/completions", base);
            let mut headers = vec![("Content-Type", "application/json".to_string())];
            // req 3: no auth header when no key resolves; a key is only
            // ever expected for a generic "openai"-compatible endpoint.
            if provider_type == "openai" {
                if let Some(key) = api_key {
                    if !key.is_empty() {
                        headers.push(("Authorization", format!("Bearer {}", key)));
                    }
                }
            }
            let body_json = serde_json::json!({
                "model": model_id,
                "messages": [{
                    "role": "user",
                    "content": prompt
                }],
                "stream": false
            });
            Ok((url, headers, body_json.to_string()))
        }
        _ => Err(format!("Unknown provider type: {}", provider_type)),
    }
}

fn run_query_with_child_tracking(
    req_id: u64,
    provider_type: &str,
    model: Option<&str>,
    prompt: &str,
    api_key: Option<&str>,
    base_url: Option<&str>,
) -> Result<String, String> {
    let (url, headers, body) =
        build_provider_request(provider_type, model, prompt, api_key, base_url)?;

    let mut cmd = std::process::Command::new("curl");
    cmd.args(["-s", "-X", "POST", &url]);
    for (k, v) in &headers {
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

    if let Some(err_val) = val.get("error") {
        if let Some(msg) = err_val.get("message").and_then(|m| m.as_str()) {
            return Err(format!("API Error: {}", msg));
        }
        if let Some(msg) = err_val.as_str() {
            return Err(format!("API Error: {}", msg));
        }
        return Err(format!("API Error: {:?}", err_val));
    }

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
        // T8 req 4: the shared OpenAI-compatible response shape —
        // choices[0].message.content — for "ollama" | "lmstudio" | "openai".
        "ollama" | "lmstudio" | "openai" => val
            .get("choices")
            .and_then(|c| c.as_array())
            .and_then(|arr| arr.first())
            .and_then(|first| first.get("message"))
            .and_then(|m| m.get("content"))
            .and_then(|t| t.as_str())
            .map(|t| t.to_string())
            .ok_or_else(|| "Failed to extract text from OpenAI-compatible response".to_string()),
        _ => Err("Unknown provider".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            "choices": [{
                "message": {
                    "content": "Ollama Socratic hint answer"
                }
            }]
        }"#;
        let parsed = parse_provider_response("ollama", response).unwrap();
        assert_eq!(parsed, "Ollama Socratic hint answer");
    }

    #[test]
    fn test_parse_lmstudio_response() {
        let response = r#"{
            "choices": [{
                "message": {
                    "content": "LM Studio Socratic hint answer"
                }
            }]
        }"#;
        let parsed = parse_provider_response("lmstudio", response).unwrap();
        assert_eq!(parsed, "LM Studio Socratic hint answer");
    }

    #[test]
    fn test_parse_openai_compat_response() {
        let response = r#"{
            "choices": [{
                "message": {
                    "content": "Generic OpenAI-compatible answer"
                }
            }]
        }"#;
        let parsed = parse_provider_response("openai", response).unwrap();
        assert_eq!(parsed, "Generic OpenAI-compatible answer");
    }

    #[test]
    fn test_parse_api_error_response() {
        let response_gemini = r#"{
            "error": {
                "code": 400,
                "message": "API key not valid",
                "status": "INVALID_ARGUMENT"
            }
        }"#;
        let parsed = parse_provider_response("gemini", response_gemini);
        assert!(parsed.is_err());
        assert_eq!(parsed.err().unwrap(), "API Error: API key not valid");

        let response_ollama = r#"{
            "error": "Failed to generate"
        }"#;
        let parsed2 = parse_provider_response("ollama", response_ollama);
        assert!(parsed2.is_err());
        assert_eq!(parsed2.err().unwrap(), "API Error: Failed to generate");
    }

    // build_provider_request is pure (no I/O, no shared debounce globals),
    // so model-threading (C6 two-slot seam) is tested directly against it —
    // calling the real dispatch_debounced*/network path here would race
    // test_debounce_cancellation's shared CURRENT_REQUEST_ID/ACTIVE_CONN
    // statics under `cargo test`'s parallel execution.

    #[test]
    fn test_build_provider_request_gemini_uses_model_override() {
        let (url, _headers, _body) =
            build_provider_request("gemini", Some("gemini-1.5-pro"), "hi", Some("key"), None)
                .unwrap();
        assert!(url.contains("gemini-1.5-pro"));
        assert!(!url.contains("gemini-2.5-flash"));
    }

    #[test]
    fn test_build_provider_request_gemini_defaults_when_no_model_given() {
        let (url, _headers, _body) =
            build_provider_request("gemini", None, "hi", Some("key"), None).unwrap();
        assert!(url.contains("gemini-2.5-flash"));
    }

    #[test]
    fn test_build_provider_request_claude_uses_model_override() {
        let (_url, _headers, body) =
            build_provider_request("claude", Some("claude-3-opus"), "hi", Some("key"), None)
                .unwrap();
        assert!(body.contains("claude-3-opus"));
        assert!(!body.contains("claude-3-5-sonnet-20241022"));
    }

    // --- T8: shared OpenAI-compatible local-provider arm ---

    #[test]
    fn test_build_provider_request_ollama_is_selectable_with_model_and_no_key() {
        let (url, _headers, body) =
            build_provider_request("ollama", Some("codellama"), "hi", None, None).unwrap();
        assert!(url.starts_with("http://localhost:11434/v1"));
        assert!(body.contains("codellama"));
    }

    #[test]
    fn test_build_provider_request_ollama_alias_default_url_and_endpoint() {
        let (url, _headers, body) =
            build_provider_request("ollama", Some("llama3"), "hi", None, None).unwrap();
        assert_eq!(url, "http://localhost:11434/v1/chat/completions");
        assert!(body.contains("\"stream\":false"));
        assert!(body.contains("\"messages\""));
    }

    #[test]
    fn test_build_provider_request_ollama_default_model_is_llama3() {
        let (_url, _headers, body) = build_provider_request("ollama", None, "hi", None, None)
            .unwrap();
        assert!(body.contains("\"model\":\"llama3\""));
    }

    #[test]
    fn test_build_provider_request_lmstudio_alias_default_url() {
        let (url, _headers, _body) =
            build_provider_request("lmstudio", Some("some-local-model"), "hi", None, None)
                .unwrap();
        assert_eq!(url, "http://localhost:1234/v1/chat/completions");
    }

    #[test]
    fn test_build_provider_request_openai_without_base_url_is_config_error() {
        let result = build_provider_request("openai", Some("gpt-4o"), "hi", None, None);
        assert!(result.is_err());
    }

    #[test]
    fn test_build_provider_request_openai_with_base_url_succeeds() {
        let (url, _headers, _body) = build_provider_request(
            "openai",
            Some("gpt-4o"),
            "hi",
            None,
            Some("http://localhost:8000/v1"),
        )
        .unwrap();
        assert_eq!(url, "http://localhost:8000/v1/chat/completions");
    }

    #[test]
    fn test_build_provider_request_explicit_base_url_overrides_ollama_alias() {
        let (url, _headers, _body) = build_provider_request(
            "ollama",
            Some("llama3"),
            "hi",
            None,
            Some("http://192.168.1.50:11434/v1"),
        )
        .unwrap();
        assert_eq!(url, "http://192.168.1.50:11434/v1/chat/completions");
    }

    #[test]
    fn test_build_provider_request_explicit_base_url_trailing_slash_joins_cleanly() {
        let (url, _headers, _body) = build_provider_request(
            "lmstudio",
            Some("some-model"),
            "hi",
            None,
            Some("http://localhost:1234/v1/"),
        )
        .unwrap();
        assert_eq!(url, "http://localhost:1234/v1/chat/completions");
    }

    #[test]
    fn test_build_provider_request_ollama_has_no_auth_header_even_with_key() {
        // req 3/5: keyless local providers never send Authorization, even
        // if a key somehow resolved for that slot.
        let (_url, headers, _body) =
            build_provider_request("ollama", Some("llama3"), "hi", Some("sk-somehow"), None)
                .unwrap();
        assert!(!headers.iter().any(|(k, _)| *k == "Authorization"));
    }

    #[test]
    fn test_build_provider_request_openai_sends_bearer_auth_when_key_present() {
        let (_url, headers, _body) = build_provider_request(
            "openai",
            Some("gpt-4o"),
            "hi",
            Some("sk-test"),
            Some("http://localhost:8000/v1"),
        )
        .unwrap();
        assert!(headers
            .iter()
            .any(|(k, v)| *k == "Authorization" && v == "Bearer sk-test"));
    }

    #[test]
    fn test_build_provider_request_openai_no_auth_header_without_key() {
        let (_url, headers, _body) = build_provider_request(
            "openai",
            Some("gpt-4o"),
            "hi",
            None,
            Some("http://localhost:8000/v1"),
        )
        .unwrap();
        assert!(!headers.iter().any(|(k, _)| *k == "Authorization"));
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
