//! The single BYOK model dispatcher (C6). Renders each request as a
//! `curl --config` document fed over stdin (so URL/headers/key never appear
//! in argv), spawns curl as a subprocess, and enforces the two dispatch
//! lanes: `Sweep` (watcher auto-judging, self-superseding) and
//! `Interactive` (user-initiated, serialized, never aborted by a Sweep).

use crate::sync_ext::LockExt;
use std::process::Child;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

/// T11 req 2: the two dispatch lanes. `Sweep` is the watcher-driven
/// stage-1/stage-2 card judging path; a new Sweep dispatch supersedes
/// (aborts) only the in-flight Sweep dispatch. `Interactive` is every
/// user-initiated call (review digest, thread turns, struggle judge,
/// comment-asks, recall grading) — never aborted by Sweep, and serialized
/// among themselves (a second Interactive call waits for the prior one
/// rather than cancelling it, so a user action never self-cancels).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lane {
    Sweep,
    Interactive,
}

/// The LLM providers a model slot can dispatch to (C6). Parsed once from the
/// config string via [`Provider::parse`]; every request builder / response
/// parser below matches on this exhaustively, so adding a provider is a
/// compile error listing the arms to fill rather than a runtime
/// "Unknown provider type" string scattered across the module.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Provider {
    Gemini,
    Claude,
    /// The three that share the OpenAI-compatible `{base_url}/chat/completions`
    /// request+response shape (T8): Ollama, LM Studio, and a generic
    /// OpenAI-compatible endpoint.
    OpenAiCompat(OpenAiKind),
}

/// The OpenAI-compatible providers, which differ only in their default
/// base-url alias and default model (T8 req 2/3).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum OpenAiKind {
    Ollama,
    LmStudio,
    OpenAi,
}

impl Provider {
    /// Parses the config `provider` string; `None` for an unrecognized value
    /// (the single string→type boundary — the caller maps `None` to the
    /// former "Unknown provider type" error / degraded mode).
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "gemini" => Some(Provider::Gemini),
            "claude" => Some(Provider::Claude),
            "ollama" => Some(Provider::OpenAiCompat(OpenAiKind::Ollama)),
            "lmstudio" => Some(Provider::OpenAiCompat(OpenAiKind::LmStudio)),
            "openai" => Some(Provider::OpenAiCompat(OpenAiKind::OpenAi)),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Provider::Gemini => "gemini",
            Provider::Claude => "claude",
            Provider::OpenAiCompat(OpenAiKind::Ollama) => "ollama",
            Provider::OpenAiCompat(OpenAiKind::LmStudio) => "lmstudio",
            Provider::OpenAiCompat(OpenAiKind::OpenAi) => "openai",
        }
    }

    /// The default model id when a slot leaves `model` unset — the single
    /// source for these literals (they used to be duplicated between the
    /// request builder and `config.rs`'s slot defaults).
    pub fn default_model(&self) -> &'static str {
        match self {
            Provider::Gemini => "gemini-2.5-flash",
            Provider::Claude => "claude-3-5-sonnet-20241022",
            Provider::OpenAiCompat(OpenAiKind::Ollama) => "llama3",
            Provider::OpenAiCompat(OpenAiKind::LmStudio | OpenAiKind::OpenAi) => "",
        }
    }

    /// Whether a slot for this provider needs no API key to be dispatch-ready
    /// (T8 req 5: the OpenAI-compatible arm is local-first; a generic `openai`
    /// endpoint MAY carry a bearer key but does not require one).
    pub fn is_keyless(&self) -> bool {
        matches!(self, Provider::OpenAiCompat(_))
    }
}

/// Per-lane dispatch state: each lane owns its own request-id counter and
/// active-child slot, so an abort in one lane can never touch the other
/// (T11 req 2).
struct LaneState {
    req_id: AtomicU64,
    child: Mutex<Option<Child>>,
    /// T11 req 2: held across the whole dispatch (spawn through
    /// wait_with_output) so a second Interactive call blocks until the
    /// prior one finishes instead of aborting it. Unused by Sweep, which
    /// supersedes instead of waiting.
    serialize: Mutex<()>,
}

impl LaneState {
    const fn new() -> Self {
        LaneState {
            req_id: AtomicU64::new(0),
            child: Mutex::new(None),
            serialize: Mutex::new(()),
        }
    }
}

static SWEEP_LANE: LaneState = LaneState::new();
static INTERACTIVE_LANE: LaneState = LaneState::new();

fn lane_state(lane: Lane) -> &'static LaneState {
    match lane {
        Lane::Sweep => &SWEEP_LANE,
        Lane::Interactive => &INTERACTIVE_LANE,
    }
}

/// Kills the in-flight child (if any) for `lane` only — never touches the
/// other lane's active connection (T11 req 2).
fn abort_lane(lane: Lane) {
    let mut child_slot = lane_state(lane).child.lock_poison_safe();
    if let Some(mut child) = child_slot.take() {
        let _ = child.kill();
    }
}

pub fn dispatch_debounced(
    lane: Lane,
    provider_type: &str,
    prompt: &str,
    api_key: Option<&str>,
) -> Result<String, String> {
    dispatch_debounced_with_model(lane, provider_type, None, prompt, api_key, None)
}

/// C6 two-slot model seam: same lane/abort semantics as
/// [`dispatch_debounced`], but threads a specific `model` (from
/// `[models.screen]`/`[models.judge]`) instead of the provider's hardcoded
/// default, so config can select e.g. a specific Ollama model.
///
/// T8 req 1-3: also threads an optional `base_url` override for the shared
/// OpenAI-compatible local-provider arm (`"ollama" | "lmstudio" | "openai"`)
/// — `None` falls back to each provider's alias default.
///
/// T11 req 1/4: no fixed pre-dispatch sleep (the event pipeline's quiescence
/// gate owns burst coalescing), and every call site names its `lane`
/// explicitly — there is no default lane.
pub fn dispatch_debounced_with_model(
    lane: Lane,
    provider_type: &str,
    model: Option<&str>,
    prompt: &str,
    api_key: Option<&str>,
    base_url: Option<&str>,
) -> Result<String, String> {
    // The single string→`Provider` boundary: everything downstream matches on
    // the enum exhaustively.
    let provider = Provider::parse(provider_type)
        .ok_or_else(|| format!("Unknown provider type: {}", provider_type))?;
    let state = lane_state(lane);

    match lane {
        // T11 req 2: a new Sweep dispatch supersedes (aborts) only the
        // in-flight Sweep dispatch, then proceeds immediately — no waiting,
        // no fixed delay.
        Lane::Sweep => {
            let req_id = state.req_id.fetch_add(1, Ordering::SeqCst) + 1;
            abort_lane(lane);
            run_query_with_child_tracking(lane, req_id, provider, model, prompt, api_key, base_url)
        }
        // T11 req 2: Interactive dispatches serialize — hold the lane's
        // serialize mutex across the whole call so a second Interactive
        // call waits for the prior one instead of aborting it.
        Lane::Interactive => {
            // T13 addendum 9: poison recovery, not `.unwrap()` — a poisoned
            // serialize mutex (a prior Interactive dispatch's thread
            // panicked while holding it) would otherwise wedge the whole
            // lane forever; the actual provider-call panic path is already
            // caught upstream (`judge::safe_dispatch`), so recovering the
            // guard here is defense in depth, not a new failure surface.
            let _guard = state.serialize.lock_poison_safe();
            let req_id = state.req_id.fetch_add(1, Ordering::SeqCst) + 1;
            run_query_with_child_tracking(lane, req_id, provider, model, prompt, api_key, base_url)
        }
    }
}

/// (url, headers, body) for a built provider request.
type ProviderRequest = (String, Vec<(&'static str, String)>, String);

/// T8 req 2: alias resolution for the shared OpenAI-compatible local-
/// provider arm. An explicitly configured `base_url` always overrides the
/// alias default (nonstandard port, LAN box); `"openai"` has no alias
/// default and is a config error without one. Trailing slashes on an
/// explicit override are stripped so the `/chat/completions` join is clean.
fn resolve_base_url(kind: OpenAiKind, base_url: Option<&str>) -> Result<String, String> {
    if let Some(explicit) = base_url {
        return Ok(explicit.trim_end_matches('/').to_string());
    }
    match kind {
        OpenAiKind::Ollama => Ok("http://localhost:11434/v1".to_string()),
        OpenAiKind::LmStudio => Ok("http://localhost:1234/v1".to_string()),
        OpenAiKind::OpenAi => {
            Err("provider \"openai\" requires a configured base_url (no alias default)".to_string())
        }
    }
}

/// Validates a slot's `provider` + `base_url` at config-load time so a
/// misconfiguration fails fast instead of silently at first dispatch (ROADMAP
/// item 7). The only failure today is a generic `openai` endpoint with no
/// configured `base_url` (it has no alias default); every other provider is
/// always resolvable. Returns the same message the dispatch path would.
pub fn validate_slot_base_url(provider: &str, base_url: Option<&str>) -> Result<(), String> {
    match Provider::parse(provider) {
        Some(Provider::OpenAiCompat(kind)) => resolve_base_url(kind, base_url).map(|_| ()),
        // Gemini/Claude build their own URLs; an unrecognized provider degrades
        // elsewhere (never reaches base_url resolution).
        _ => Ok(()),
    }
}

/// Pure request builder (no I/O, no shared/global state) — split out so
/// model-threading (C6 two-slot seam) is testable without racing the
/// debounce globals `run_query_with_child_tracking`'s callers share.
fn build_provider_request(
    provider: Provider,
    model: Option<&str>,
    prompt: &str,
    api_key: Option<&str>,
    base_url: Option<&str>,
) -> Result<ProviderRequest, String> {
    match provider {
        Provider::Gemini => {
            let key = api_key.unwrap_or("");
            let model_id = model.unwrap_or_else(|| provider.default_model());
            // T9 req 6: the key rides the x-goog-api-key header, never the
            // URL query string — URLs land in `ps` output and proxy logs.
            let url = format!(
                "https://generativelanguage.googleapis.com/v1beta/models/{}:generateContent",
                model_id
            );
            let headers = vec![
                ("Content-Type", "application/json".to_string()),
                ("x-goog-api-key", key.to_string()),
            ];
            let body_json = serde_json::json!({
                "contents": [{
                    "parts": [{
                        "text": prompt
                    }]
                }]
            });
            Ok((url, headers, body_json.to_string()))
        }
        Provider::Claude => {
            let key = api_key.unwrap_or("").to_string();
            let model_id = model.unwrap_or_else(|| provider.default_model());
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
        Provider::OpenAiCompat(kind) => {
            let base = resolve_base_url(kind, base_url)?;
            let model_id = model.unwrap_or_else(|| provider.default_model());
            let url = format!("{}/chat/completions", base);
            let mut headers = vec![("Content-Type", "application/json".to_string())];
            // req 3: no auth header when no key resolves; a key is only
            // ever expected for a generic "openai"-compatible endpoint.
            if kind == OpenAiKind::OpenAi {
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
    }
}

/// T9 req 6: renders a request as a curl `--config` document (fed via
/// stdin) so URL, headers, and body never appear in curl's argv. Pure and
/// separately tested — the argv-hygiene guarantee lives here.
fn curl_config_for(url: &str, headers: &[(&str, String)], body: &str) -> String {
    // curl config-file quoting: inside double quotes, backslash escapes
    // apply — escape `\` and `"`; JSON bodies from serde are single-line,
    // but escape control chars defensively anyway.
    fn quote(val: &str) -> String {
        let mut out = String::with_capacity(val.len() + 2);
        out.push('"');
        for c in val.chars() {
            match c {
                '\\' => out.push_str("\\\\"),
                '"' => out.push_str("\\\""),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                _ => out.push(c),
            }
        }
        out.push('"');
        out
    }

    let mut config = String::new();
    config.push_str("silent\n");
    config.push_str("request = \"POST\"\n");
    // T9 req 9(b): bound every transport call.
    config.push_str("max-time = 60\n");
    config.push_str(&format!("url = {}\n", quote(url)));
    for (k, v) in headers {
        config.push_str(&format!("header = {}\n", quote(&format!("{}: {}", k, v))));
    }
    config.push_str(&format!("data = {}\n", quote(body)));
    config
}

/// T13 req 3: the transport seam — spawns the child process that will
/// receive the curl `--config` document on stdin. Extracted behind an
/// injectable function (defaulting to `spawn_curl`, the real production
/// path) so a test can substitute a different child process without
/// spawning a real `curl` or touching the network — zero behavior change
/// for production callers, which always go through `spawn_curl`.
fn spawn_curl() -> std::io::Result<Child> {
    std::process::Command::new("curl")
        .args(["--config", "-"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
}

fn run_query_with_child_tracking(
    lane: Lane,
    req_id: u64,
    provider: Provider,
    model: Option<&str>,
    prompt: &str,
    api_key: Option<&str>,
    base_url: Option<&str>,
) -> Result<String, String> {
    run_query_with_transport(
        lane, req_id, provider, model, prompt, api_key, base_url, spawn_curl,
    )
}

/// T13 req 3: same as [`run_query_with_child_tracking`], but with the
/// child-process spawn step (the "curl execution") injected via `transport`
/// — the seam a test uses to substitute a non-curl child without any
/// network I/O. Everything downstream of the spawn (stdin write, lane
/// registration/abort, wait, req-id recheck, response parsing) is
/// unchanged.
#[allow(clippy::too_many_arguments)]
fn run_query_with_transport(
    lane: Lane,
    req_id: u64,
    provider: Provider,
    model: Option<&str>,
    prompt: &str,
    api_key: Option<&str>,
    base_url: Option<&str>,
    transport: impl FnOnce() -> std::io::Result<Child>,
) -> Result<String, String> {
    let (url, headers, body) = build_provider_request(provider, model, prompt, api_key, base_url)?;

    // T9 req 6: the URL, headers (key material), and body travel to curl as
    // a --config document on stdin — argv stays constant (`curl --config -`)
    // so no secret is ever visible in `ps` output. req 9(b): max-time bounds
    // every transport call so an unresponsive endpoint (e.g. a local model
    // mid-generation) cannot wedge a dispatch thread indefinitely.
    let config = curl_config_for(&url, &headers, &body);

    let mut child = transport().map_err(|e| format!("Failed to spawn curl: {}", e))?;

    if let Some(mut stdin) = child.stdin.take() {
        use std::io::Write;
        if let Err(e) = stdin.write_all(config.as_bytes()) {
            let _ = child.kill();
            return Err(format!("Failed to write curl config: {}", e));
        }
        // Dropping stdin closes it; curl reads the config to EOF.
    }

    let state = lane_state(lane);

    {
        let mut child_slot = state.child.lock_poison_safe();
        if state.req_id.load(Ordering::SeqCst) != req_id {
            let _ = child.kill();
            return Err("Aborted".to_string());
        }
        *child_slot = Some(child);
    }

    let child_opt = {
        let mut child_slot = state.child.lock_poison_safe();
        child_slot.take()
    };

    let result = if let Some(child) = child_opt {
        let output = child
            .wait_with_output()
            .map_err(|e| format!("curl execution failed: {}", e))?;
        if output.status.success() {
            let res_str = String::from_utf8(output.stdout)
                .map_err(|e| format!("Invalid UTF-8 response: {}", e))?;
            parse_provider_response(provider, &res_str)
        } else {
            let err_str = String::from_utf8_lossy(&output.stderr).to_string();
            Err(format!("curl error: {}", err_str))
        }
    } else {
        Err("Aborted".to_string())
    };

    // T11 req 3: re-check the lane's request id after wait_with_output
    // returns — a superseded result (this lane moved on to a newer dispatch
    // while we were waiting on the child) is dropped, never delivered as
    // fresh, regardless of whether the kill above actually landed in time.
    if state.req_id.load(Ordering::SeqCst) != req_id {
        return Err("Aborted".to_string());
    }

    result
}

pub fn parse_provider_response(provider: Provider, response: &str) -> Result<String, String> {
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

    match provider {
        Provider::Gemini => val
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
        Provider::Claude => val
            .get("content")
            .and_then(|c| c.as_array())
            .and_then(|arr| arr.first())
            .and_then(|first| first.get("text"))
            .and_then(|t| t.as_str())
            .map(|t| t.to_string())
            .ok_or_else(|| "Failed to extract text from Claude response".to_string()),
        // T8 req 4: the shared OpenAI-compatible response shape —
        // choices[0].message.content — for Ollama / LM Studio / OpenAI.
        Provider::OpenAiCompat(_) => val
            .get("choices")
            .and_then(|c| c.as_array())
            .and_then(|arr| arr.first())
            .and_then(|first| first.get("message"))
            .and_then(|m| m.get("content"))
            .and_then(|t| t.as_str())
            .map(|t| t.to_string())
            .ok_or_else(|| "Failed to extract text from OpenAI-compatible response".to_string()),
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
        let parsed = parse_provider_response(Provider::Gemini, response).unwrap();
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
        let parsed = parse_provider_response(Provider::Claude, response).unwrap();
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
        let parsed =
            parse_provider_response(Provider::OpenAiCompat(OpenAiKind::Ollama), response).unwrap();
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
        let parsed =
            parse_provider_response(Provider::OpenAiCompat(OpenAiKind::LmStudio), response)
                .unwrap();
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
        let parsed =
            parse_provider_response(Provider::OpenAiCompat(OpenAiKind::OpenAi), response).unwrap();
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
        let parsed = parse_provider_response(Provider::Gemini, response_gemini);
        assert!(parsed.is_err());
        assert_eq!(parsed.err().unwrap(), "API Error: API key not valid");

        let response_ollama = r#"{
            "error": "Failed to generate"
        }"#;
        let parsed2 =
            parse_provider_response(Provider::OpenAiCompat(OpenAiKind::Ollama), response_ollama);
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
        let (url, _headers, _body) = build_provider_request(
            Provider::Gemini,
            Some("gemini-1.5-pro"),
            "hi",
            Some("key"),
            None,
        )
        .unwrap();
        assert!(url.contains("gemini-1.5-pro"));
        assert!(!url.contains("gemini-2.5-flash"));
    }

    #[test]
    fn test_build_provider_request_gemini_defaults_when_no_model_given() {
        let (url, _headers, _body) =
            build_provider_request(Provider::Gemini, None, "hi", Some("key"), None).unwrap();
        assert!(url.contains("gemini-2.5-flash"));
    }

    // --- T9 req 6: key hygiene (no key material outside the config doc) ---

    #[test]
    fn test_gemini_key_in_header_never_in_url() {
        let (url, headers, _body) =
            build_provider_request(Provider::Gemini, None, "hi", Some("sk-gemini-secret"), None)
                .unwrap();
        assert!(!url.contains("sk-gemini-secret"));
        assert!(!url.contains("key="));
        assert!(
            headers
                .iter()
                .any(|(k, v)| *k == "x-goog-api-key" && v == "sk-gemini-secret")
        );
    }

    #[test]
    fn test_curl_config_carries_url_headers_body_with_bounded_time() {
        let headers = vec![("x-api-key", "sk-claude-secret".to_string())];
        let config = curl_config_for(
            "https://api.example.com/v1/messages",
            &headers,
            r#"{"model":"m","prompt":"say \"hi\""}"#,
        );
        assert!(config.contains("url = \"https://api.example.com/v1/messages\"\n"));
        assert!(config.contains("header = \"x-api-key: sk-claude-secret\"\n"));
        assert!(config.contains("request = \"POST\"\n"));
        // req 9(b): every transport call is time-bounded.
        assert!(config.contains("max-time = "));
        // Quotes inside the JSON body survive curl's config quoting.
        assert!(config.contains(r#"data = "{\"model\":\"m\",\"prompt\":\"say \\\"hi\\\"\"}""#));
    }

    #[test]
    fn test_curl_argv_is_constant_and_key_free() {
        // The spawn site passes ONLY ["--config", "-"] as arguments; this
        // pins the invariant at the closest testable seam: the config doc
        // holds the secrets, and no other argument is ever interpolated.
        // T13 req 3: the spawn itself now lives behind the transport seam
        // (`spawn_curl`), the production default `run_query_with_child_tracking`
        // wires in — so that's where the invariant is pinned now.
        let src = include_str!("provider.rs");
        let spawn_section = src
            .split("fn spawn_curl()")
            .nth(1)
            .expect("spawn fn present");
        // Stop scanning at the next `fn` so a change elsewhere in the file
        // can't accidentally satisfy this assertion.
        let spawn_section = spawn_section.split("\nfn ").next().unwrap_or(spawn_section);
        let args_lines: Vec<&str> = spawn_section
            .lines()
            .filter(|l| l.trim_start().starts_with(".args("))
            .collect();
        assert_eq!(
            args_lines.len(),
            1,
            "exactly one argument-list call expected, got: {:?}",
            args_lines
        );
        assert!(args_lines[0].contains(r#"["--config", "-"]"#));
    }

    // --- T13 req 3: the transport seam is genuinely swappable ---

    #[test]
    fn test_transport_seam_allows_substituting_a_non_curl_child_process() {
        // Injects a transport that spawns `sh` (draining the --config doc
        // from stdin, then emitting a canned Gemini response carrying a
        // sentinel string) instead of curl — proving the "curl execution"
        // step is truly swappable, not hardcoded. No network, no curl binary
        // needed; production dispatch (`run_query_with_child_tracking`)
        // still always uses `spawn_curl` unchanged. The sentinel makes the
        // assertion discriminating: a regression that ignored the injected
        // transport and spawned real curl could never return this exact
        // text (only a network/auth/timeout error).
        let _guard = TEST_MUTEX.lock_poison_safe();
        let state = lane_state(Lane::Interactive);
        let req_id = state.req_id.fetch_add(1, Ordering::SeqCst) + 1;

        let transport = || {
            std::process::Command::new("sh")
                .args([
                    "-c",
                    r#"cat >/dev/null; printf '%s' '{"candidates":[{"content":{"parts":[{"text":"transport-sentinel"}]}}]}'"#,
                ])
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
        };

        let result = run_query_with_transport(
            Lane::Interactive,
            req_id,
            Provider::Gemini,
            None,
            "hi",
            Some("key"),
            None,
            transport,
        );

        assert_eq!(
            result,
            Ok("transport-sentinel".to_string()),
            "the sentinel response must round-trip through the injected transport"
        );
    }

    #[test]
    fn test_build_provider_request_claude_uses_model_override() {
        let (_url, _headers, body) = build_provider_request(
            Provider::Claude,
            Some("claude-3-opus"),
            "hi",
            Some("key"),
            None,
        )
        .unwrap();
        assert!(body.contains("claude-3-opus"));
        assert!(!body.contains("claude-3-5-sonnet-20241022"));
    }

    // --- T8: shared OpenAI-compatible local-provider arm ---

    #[test]
    fn test_build_provider_request_ollama_is_selectable_with_model_and_no_key() {
        let (url, _headers, body) = build_provider_request(
            Provider::OpenAiCompat(OpenAiKind::Ollama),
            Some("codellama"),
            "hi",
            None,
            None,
        )
        .unwrap();
        assert!(url.starts_with("http://localhost:11434/v1"));
        assert!(body.contains("codellama"));
    }

    #[test]
    fn test_build_provider_request_ollama_alias_default_url_and_endpoint() {
        let (url, _headers, body) = build_provider_request(
            Provider::OpenAiCompat(OpenAiKind::Ollama),
            Some("llama3"),
            "hi",
            None,
            None,
        )
        .unwrap();
        assert_eq!(url, "http://localhost:11434/v1/chat/completions");
        assert!(body.contains("\"stream\":false"));
        assert!(body.contains("\"messages\""));
    }

    #[test]
    fn test_build_provider_request_ollama_default_model_is_llama3() {
        let (_url, _headers, body) = build_provider_request(
            Provider::OpenAiCompat(OpenAiKind::Ollama),
            None,
            "hi",
            None,
            None,
        )
        .unwrap();
        assert!(body.contains("\"model\":\"llama3\""));
    }

    #[test]
    fn test_build_provider_request_lmstudio_alias_default_url() {
        let (url, _headers, _body) = build_provider_request(
            Provider::OpenAiCompat(OpenAiKind::LmStudio),
            Some("some-local-model"),
            "hi",
            None,
            None,
        )
        .unwrap();
        assert_eq!(url, "http://localhost:1234/v1/chat/completions");
    }

    #[test]
    fn test_build_provider_request_openai_without_base_url_is_config_error() {
        let result = build_provider_request(
            Provider::OpenAiCompat(OpenAiKind::OpenAi),
            Some("gpt-4o"),
            "hi",
            None,
            None,
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_slot_base_url_flags_openai_without_base_url() {
        // ROADMAP item 7: a keyed `openai` slot with no base_url is a config
        // error surfaced at load, not silently at first dispatch.
        assert!(validate_slot_base_url("openai", None).is_err());
        assert!(validate_slot_base_url("openai", Some("http://localhost:8000/v1")).is_ok());
        // Alias-backed and self-URL providers are always resolvable.
        assert!(validate_slot_base_url("ollama", None).is_ok());
        assert!(validate_slot_base_url("lmstudio", None).is_ok());
        assert!(validate_slot_base_url("gemini", None).is_ok());
        assert!(validate_slot_base_url("claude", None).is_ok());
    }

    #[test]
    fn test_build_provider_request_openai_with_base_url_succeeds() {
        let (url, _headers, _body) = build_provider_request(
            Provider::OpenAiCompat(OpenAiKind::OpenAi),
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
            Provider::OpenAiCompat(OpenAiKind::Ollama),
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
            Provider::OpenAiCompat(OpenAiKind::LmStudio),
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
        let (_url, headers, _body) = build_provider_request(
            Provider::OpenAiCompat(OpenAiKind::Ollama),
            Some("llama3"),
            "hi",
            Some("sk-somehow"),
            None,
        )
        .unwrap();
        assert!(!headers.iter().any(|(k, _)| *k == "Authorization"));
    }

    #[test]
    fn test_build_provider_request_openai_sends_bearer_auth_when_key_present() {
        let (_url, headers, _body) = build_provider_request(
            Provider::OpenAiCompat(OpenAiKind::OpenAi),
            Some("gpt-4o"),
            "hi",
            Some("sk-test"),
            Some("http://localhost:8000/v1"),
        )
        .unwrap();
        assert!(
            headers
                .iter()
                .any(|(k, v)| *k == "Authorization" && v == "Bearer sk-test")
        );
    }

    #[test]
    fn test_build_provider_request_openai_no_auth_header_without_key() {
        let (_url, headers, _body) = build_provider_request(
            Provider::OpenAiCompat(OpenAiKind::OpenAi),
            Some("gpt-4o"),
            "hi",
            None,
            Some("http://localhost:8000/v1"),
        )
        .unwrap();
        assert!(!headers.iter().any(|(k, _)| *k == "Authorization"));
    }

    // --- T11: dispatch lanes (no fixed debounce, scoped aborts per lane) ---
    //
    // These tests dispatch real curl calls against a local
    // `std::net::TcpListener` stub (never an external service), so they can
    // exercise the actual lane statics (`SWEEP_LANE`/`INTERACTIVE_LANE`).
    // Because those statics are process-global, this suite serializes on
    // `TEST_MUTEX` so lane tests never interleave with each other under
    // `cargo test`'s parallel execution — the same pattern used elsewhere in
    // this codebase for global-state tests (e.g. `watcher_coordinator`,
    // `credentials`). Each test still opens its own ephemeral port, so the
    // *servers* stay parallel-safe; only the shared lane state is
    // serialized.
    static TEST_MUTEX: Mutex<()> = Mutex::new(());

    /// Spins up a one-shot-per-connection HTTP stub on `127.0.0.1:0`
    /// serving a canned OpenAI-compatible JSON body, optionally after
    /// `delay` (simulating a slow/fast provider). Returns the `base_url`
    /// (`http://127.0.0.1:{port}/v1`) to pass to `dispatch_debounced_with_model`.
    fn spawn_stub_server(delay: std::time::Duration, body: &'static str) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                std::thread::spawn(move || {
                    use std::io::{Read, Write};
                    // Drain (some of) the request so curl doesn't see a
                    // reset on a still-open write side; the request itself
                    // is irrelevant to this stub.
                    let mut buf = [0u8; 4096];
                    let _ = stream.set_read_timeout(Some(std::time::Duration::from_millis(500)));
                    let _ = stream.read(&mut buf);
                    if !delay.is_zero() {
                        std::thread::sleep(delay);
                    }
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = stream.write_all(response.as_bytes());
                    let _ = stream.flush();
                });
            }
        });
        format!("http://127.0.0.1:{}/v1", port)
    }

    fn openai_body(text: &str) -> String {
        serde_json::json!({
            "choices": [{ "message": { "content": text } }]
        })
        .to_string()
    }

    // (d) req 1: a lone dispatch on an idle lane completes without any
    // artificial delay — well under 1s excluding transport (the old fixed
    // debounce was a 15x100ms = 1.5s busy-sleep before every dispatch).
    #[test]
    fn test_no_fixed_delay_on_idle_lane() {
        let _guard = TEST_MUTEX.lock_poison_safe();
        let body = openai_body("fast reply");
        let leaked_body: &'static str = Box::leak(body.into_boxed_str());
        let base_url = spawn_stub_server(std::time::Duration::from_millis(0), leaked_body);

        let start = std::time::Instant::now();
        let result = dispatch_debounced_with_model(
            Lane::Sweep,
            "openai",
            Some("test-model"),
            "hi",
            None,
            Some(&base_url),
        );
        let elapsed = start.elapsed();

        assert_eq!(result, Ok("fast reply".to_string()));
        assert!(
            elapsed < std::time::Duration::from_secs(1),
            "expected sub-1s dispatch with no fixed debounce, took {:?}",
            elapsed
        );
    }

    // (b) req 2: a newer Sweep dispatch aborts (supersedes) an older
    // in-flight Sweep dispatch.
    #[test]
    fn test_newer_sweep_aborts_older_sweep() {
        let _guard = TEST_MUTEX.lock_poison_safe();
        let old_body = openai_body("stale sweep result");
        let old_leaked: &'static str = Box::leak(old_body.into_boxed_str());
        let old_url = spawn_stub_server(std::time::Duration::from_millis(300), old_leaked);

        let new_body = openai_body("fresh sweep result");
        let new_leaked: &'static str = Box::leak(new_body.into_boxed_str());
        let new_url = spawn_stub_server(std::time::Duration::from_millis(0), new_leaked);

        let older = std::thread::spawn(move || {
            dispatch_debounced_with_model(
                Lane::Sweep,
                "openai",
                Some("test-model"),
                "old prompt",
                None,
                Some(&old_url),
            )
        });
        // Give the older dispatch time to spawn its curl child before the
        // newer one supersedes it.
        std::thread::sleep(std::time::Duration::from_millis(50));

        let newer = dispatch_debounced_with_model(
            Lane::Sweep,
            "openai",
            Some("test-model"),
            "new prompt",
            None,
            Some(&new_url),
        );
        let older_result = older.join().unwrap();

        assert_eq!(older_result, Err("Aborted".to_string()));
        assert_eq!(newer, Ok("fresh sweep result".to_string()));
    }

    // (c) req 3: a superseded Sweep result is dropped even if its curl
    // child runs to completion (with a real, successful response) after a
    // newer Sweep dispatch has already taken over the lane — the post-wait
    // request-id recheck drops it regardless of whether the kill landed in
    // time.
    #[test]
    fn test_superseded_sweep_result_dropped_after_completion() {
        let _guard = TEST_MUTEX.lock_poison_safe();
        let old_body = openai_body("should never be delivered");
        let old_leaked: &'static str = Box::leak(old_body.into_boxed_str());
        // Short delay: the older dispatch's curl child WILL complete
        // successfully — this proves the drop isn't merely a side effect of
        // the kill always winning the race.
        let old_url = spawn_stub_server(std::time::Duration::from_millis(150), old_leaked);

        let new_body = openai_body("newest sweep result");
        let new_leaked: &'static str = Box::leak(new_body.into_boxed_str());
        let new_url = spawn_stub_server(std::time::Duration::from_millis(0), new_leaked);

        let older = std::thread::spawn(move || {
            dispatch_debounced_with_model(
                Lane::Sweep,
                "openai",
                Some("test-model"),
                "old prompt",
                None,
                Some(&old_url),
            )
        });
        std::thread::sleep(std::time::Duration::from_millis(20));

        let newer = dispatch_debounced_with_model(
            Lane::Sweep,
            "openai",
            Some("test-model"),
            "new prompt",
            None,
            Some(&new_url),
        );
        let older_result = older.join().unwrap();

        // Never delivered as fresh content, even though its own transport
        // call may well have completed successfully by now.
        assert_eq!(older_result, Err("Aborted".to_string()));
        assert_eq!(newer, Ok("newest sweep result".to_string()));
    }

    // (a) req 2: an Interactive dispatch survives a concurrent Sweep
    // supersede — Sweep dispatches never touch the Interactive lane.
    #[test]
    fn test_interactive_survives_concurrent_sweep_supersede() {
        let _guard = TEST_MUTEX.lock_poison_safe();
        let interactive_body = openai_body("interactive answer");
        let interactive_leaked: &'static str = Box::leak(interactive_body.into_boxed_str());
        let interactive_url =
            spawn_stub_server(std::time::Duration::from_millis(200), interactive_leaked);

        let sweep_old_body = openai_body("sweep stale");
        let sweep_old_leaked: &'static str = Box::leak(sweep_old_body.into_boxed_str());
        let sweep_old_url =
            spawn_stub_server(std::time::Duration::from_millis(150), sweep_old_leaked);

        let sweep_new_body = openai_body("sweep fresh");
        let sweep_new_leaked: &'static str = Box::leak(sweep_new_body.into_boxed_str());
        let sweep_new_url =
            spawn_stub_server(std::time::Duration::from_millis(0), sweep_new_leaked);

        let interactive_handle = std::thread::spawn(move || {
            dispatch_debounced_with_model(
                Lane::Interactive,
                "openai",
                Some("test-model"),
                "interactive prompt",
                None,
                Some(&interactive_url),
            )
        });

        let sweep_old_handle = std::thread::spawn(move || {
            dispatch_debounced_with_model(
                Lane::Sweep,
                "openai",
                Some("test-model"),
                "sweep old prompt",
                None,
                Some(&sweep_old_url),
            )
        });
        std::thread::sleep(std::time::Duration::from_millis(30));

        let sweep_new_result = dispatch_debounced_with_model(
            Lane::Sweep,
            "openai",
            Some("test-model"),
            "sweep new prompt",
            None,
            Some(&sweep_new_url),
        );
        let sweep_old_result = sweep_old_handle.join().unwrap();
        let interactive_result = interactive_handle.join().unwrap();

        // The concurrent Sweep supersede happened (and behaved per req 2)...
        assert_eq!(sweep_old_result, Err("Aborted".to_string()));
        assert_eq!(sweep_new_result, Ok("sweep fresh".to_string()));
        // ...but never touched the Interactive lane's own in-flight call.
        assert_eq!(interactive_result, Ok("interactive answer".to_string()));
    }

    // req 2: a second Interactive call waits for the prior one rather than
    // aborting it — a user action must never self-cancel.
    #[test]
    fn test_second_interactive_waits_for_first_never_aborts_it() {
        let _guard = TEST_MUTEX.lock_poison_safe();
        let first_body = openai_body("first interactive answer");
        let first_leaked: &'static str = Box::leak(first_body.into_boxed_str());
        let first_url = spawn_stub_server(std::time::Duration::from_millis(150), first_leaked);

        let second_body = openai_body("second interactive answer");
        let second_leaked: &'static str = Box::leak(second_body.into_boxed_str());
        let second_url = spawn_stub_server(std::time::Duration::from_millis(0), second_leaked);

        let first_handle = std::thread::spawn(move || {
            dispatch_debounced_with_model(
                Lane::Interactive,
                "openai",
                Some("test-model"),
                "first prompt",
                None,
                Some(&first_url),
            )
        });
        std::thread::sleep(std::time::Duration::from_millis(20));

        let second_handle = std::thread::spawn(move || {
            dispatch_debounced_with_model(
                Lane::Interactive,
                "openai",
                Some("test-model"),
                "second prompt",
                None,
                Some(&second_url),
            )
        });

        let first_result = first_handle.join().unwrap();
        let second_result = second_handle.join().unwrap();

        // Neither call is ever aborted — the second waited its turn.
        assert_eq!(first_result, Ok("first interactive answer".to_string()));
        assert_eq!(second_result, Ok("second interactive answer".to_string()));
    }
}
