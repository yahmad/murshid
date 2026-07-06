//! TOML configuration: the layered load (system/user/project), precedence-
//! aware merge, and lock policy, plus the C12 default knobs (frequency,
//! directness, model slots, consent). Values are read here and interpreted
//! at their use sites.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq)]
pub struct ProviderConfig {
    pub api_key_source: String,
    pub suppress_api_key_warning: bool,
    pub lock_policy: bool,
    pub local_output_leak_check: bool,
}

impl Default for ProviderConfig {
    fn default() -> Self {
        Self {
            api_key_source: "keychain".to_string(),
            suppress_api_key_warning: false,
            lock_policy: false,
            local_output_leak_check: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProactivenessConfig {
    pub debounce_ms: u64,
    pub consecutive_failure_threshold: u32,
}

impl Default for ProactivenessConfig {
    fn default() -> Self {
        Self {
            debounce_ms: 1500,
            consecutive_failure_threshold: 3,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct PedagogyConfig {
    pub style: String,
    pub withhold_code: bool,
    pub explanation_depth: String,
    pub auto_scale: bool,
    pub lock_difficulty: bool,
    pub escape_hatch_threshold: u32,
    pub max_weekly_bypass_sessions: u32,
    pub history_limit: u32,
}

impl Default for PedagogyConfig {
    fn default() -> Self {
        Self {
            style: "socratic".to_string(),
            withhold_code: true,
            explanation_depth: "normal".to_string(),
            auto_scale: true,
            lock_difficulty: false,
            escape_hatch_threshold: 5,
            max_weekly_bypass_sessions: 3,
            history_limit: 3,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct GitPreCommitConfig {
    pub action: String,
    pub gui_dialog: bool,
    pub gui_dialog_timeout_ms: u64,
    pub audit_bypass_logging: bool,
}

impl Default for GitPreCommitConfig {
    fn default() -> Self {
        Self {
            action: "warn".to_string(),
            gui_dialog: true,
            gui_dialog_timeout_ms: 2000,
            audit_bypass_logging: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct GitConfig {
    pub pre_commit: GitPreCommitConfig,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EvaluationConfig {
    pub golden_dataset_path: String,
}

impl Default for EvaluationConfig {
    fn default() -> Self {
        Self {
            golden_dataset_path: "~/.config/murshid/eval_dataset.jsonl".to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct WatcherConfig {
    pub exclude: Vec<String>,
    pub include_external_links: Vec<String>,
    pub target_cache_limit: String,
    pub max_watch_threads: u32,
    pub max_watch_fds: u32,
}

impl Default for WatcherConfig {
    fn default() -> Self {
        Self {
            exclude: vec![
                "**/target/**".to_string(),
                "**/.git/**".to_string(),
                "**/.murshid_experiments/**".to_string(),
            ],
            include_external_links: Vec::new(),
            target_cache_limit: "5GB".to_string(),
            max_watch_threads: 4,
            max_watch_fds: 1000,
        }
    }
}

/// C6 model seam: one slot's `(provider, model)` pair. Keys are resolved via
/// the existing keyring/env flow (credentials.rs), keyed off `provider`; a
/// `provider` of `"ollama"`, `"lmstudio"`, or `"openai"` needs no key at all.
///
/// T8 req 1: `base_url` is an optional override for the shared OpenAI-
/// compatible local-provider path (`provider.rs`'s `"ollama" | "lmstudio" |
/// "openai"` arm). Absent for standard installs — `"ollama"` and
/// `"lmstudio"` fall back to their well-known local ports; `"openai"` (a
/// generic OpenAI-compatible endpoint) requires it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelSlotConfig {
    pub provider: String,
    pub model: String,
    pub base_url: Option<String>,
}

/// C6 `[models]`: two independent slots — `screen` (cheap/fast) and `judge`
/// (strong).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelsConfig {
    pub screen: ModelSlotConfig,
    pub judge: ModelSlotConfig,
}

impl Default for ModelsConfig {
    fn default() -> Self {
        Self {
            screen: ModelSlotConfig {
                provider: "gemini".to_string(),
                model: "gemini-2.5-flash".to_string(),
                base_url: None,
            },
            judge: ModelSlotConfig {
                provider: "claude".to_string(),
                model: "claude-3-5-sonnet-20241022".to_string(),
                base_url: None,
            },
        }
    }
}

/// T2 req 1 / D10 / C12: the frequency knob. Detents set a (budget, floor)
/// pair — see `noise::detent_for`. Default `quiet` (I7 ship-chill overrides
/// T1's hardcoded `standard`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DialConfig {
    pub frequency: String,
    /// T2 req 10 undo path: categories forced back off auto-throttle via
    /// config (the spec's "config key OR the `m` list marks it" — this repo
    /// implements the config-key half of that "or").
    pub unthrottle: Vec<String>,
    /// T4 req 2 / C4: `guide-me|balanced|tell-me` — shifts the R2 + knob-
    /// offset entry rung ±1 (T5 will replace this with memory-driven entry).
    pub directness: String,
}

impl Default for DialConfig {
    fn default() -> Self {
        Self {
            frequency: "quiet".to_string(),
            unthrottle: Vec::new(),
            directness: "balanced".to_string(),
        }
    }
}

/// T4 req 8/12 / C6 BYOK consent: `ask` (default) prompts once per session
/// for the first thread turn, and once per `murshid review` invocation, with
/// a rough token estimate; `always` skips the prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsentConfig {
    pub solicited_spend: String,
}

impl Default for ConsentConfig {
    fn default() -> Self {
        Self {
            solicited_spend: "ask".to_string(),
        }
    }
}

/// T10 req 2: the explicit `[pack] language` override — the highest-
/// precedence leg of `pack::resolve_pack_id`'s three-way resolution.
/// `None` (the default) means "no override configured", so resolution
/// falls through to marker detection.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PackConfig {
    pub language: Option<String>,
}

/// T14 req 1: the raw-model-I/O trace-capture knob (`trace::record_dispatch`).
/// Default ON: the dogfood phase leans toward diagnosability — trace files
/// are local-first (never leave the machine) and purely additive
/// observability (no behavior change) — but disable-able for anyone who
/// doesn't want raw prompts/responses (which may echo source snippets)
/// written to disk at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceConfig {
    pub enabled: bool,
}

impl Default for TraceConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct AppConfig {
    pub provider: ProviderConfig,
    pub proactiveness: ProactivenessConfig,
    pub pedagogy: PedagogyConfig,
    pub git: GitConfig,
    pub evaluation: EvaluationConfig,
    pub watcher: WatcherConfig,
    pub models: ModelsConfig,
    pub dial: DialConfig,
    pub consent: ConsentConfig,
    pub pack: PackConfig,
    pub trace: TraceConfig,
}

pub fn get_home_dir() -> Option<PathBuf> {
    std::env::var("HOME")
        .ok()
        .map(PathBuf::from)
        .or_else(|| std::env::var("USERPROFILE").ok().map(PathBuf::from))
}

pub fn get_system_config_path() -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        PathBuf::from("/Library/Application Support/murshid/config.toml")
    }
    #[cfg(target_os = "windows")]
    {
        let progdata =
            std::env::var("ProgramData").unwrap_or_else(|_| "C:\\ProgramData".to_string());
        PathBuf::from(format!("{}\\murshid\\config.toml", progdata))
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        PathBuf::from("/etc/murshid/config.toml")
    }
}

pub fn get_user_config_path() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        get_home_dir().map(|h| h.join("Library/Application Support/murshid/config.toml"))
    }
    #[cfg(target_os = "windows")]
    {
        std::env::var("APPDATA")
            .ok()
            .map(|a| PathBuf::from(a).join("murshid\\config.toml"))
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        get_home_dir().map(|h| h.join(".config/murshid/config.toml"))
    }
}

pub fn resolve_user_config_path() -> Option<PathBuf> {
    let primary = get_user_config_path();
    if let Some(ref p) = primary {
        if p.exists() {
            return Some(p.clone());
        }
    }
    if let Some(home) = get_home_dir() {
        let fallback = home.join(".config/murshid/config.toml");
        if fallback.exists() {
            return Some(fallback);
        }
    }
    primary
}

pub fn get_project_config_path() -> Option<PathBuf> {
    let mut dir = std::env::current_dir().ok()?;
    loop {
        let p1 = dir.join(".murshid/config.toml");
        if p1.exists() {
            return Some(p1);
        }
        let p2 = dir.join(".murshid.toml");
        if p2.exists() {
            return Some(p2);
        }
        if !dir.pop() {
            break;
        }
    }
    None
}

fn parse_bool(s: &str) -> Option<bool> {
    match s.to_lowercase().as_str() {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

fn parse_u64(s: &str) -> Option<u64> {
    s.parse::<u64>().ok()
}

fn parse_u32(s: &str) -> Option<u32> {
    s.parse::<u32>().ok()
}

fn clean_string_val(s: &str) -> String {
    let s = s.trim();
    if s.starts_with('"') && s.ends_with('"') && s.len() >= 2 {
        s[1..s.len() - 1].to_string()
    } else {
        s.to_string()
    }
}

/// Accepted values for the enum-typed config knobs. A value outside its set is
/// a typo, not a new mode, so [`merge_enum_field`] rejects it with a warning
/// instead of storing it verbatim (the pre-item-5 behaviour, which let a typo
/// take silent effect).
const API_KEY_SOURCE_VALUES: &[&str] = &["keychain", "environment", "env"];
const DIRECTNESS_VALUES: &[&str] = &["guide-me", "balanced", "tell-me"];
const SOLICITED_SPEND_VALUES: &[&str] = &["ask", "always"];
const PEDAGOGY_STYLE_VALUES: &[&str] = &["socratic"];

/// Merges an enum-valued config field: `raw` wins only if it is one of
/// `allowed`; otherwise the current (valid) value is KEPT and a warn-on-unknown
/// notice is printed to stderr — mirroring [`crate::pack::payload_fallback_notice`].
/// This makes a typo a no-op rather than a silent behaviour change. It matters
/// most for `api_key_source`: a misspelling there must never downgrade secret
/// storage from the OS keychain to plaintext environment variables — keeping the
/// current value (whose base default is `keychain`) fails secure.
fn merge_enum_field(field: &str, current: &mut String, raw: String, allowed: &[&str]) {
    if allowed.contains(&raw.as_str()) {
        *current = raw;
    } else {
        eprintln!(
            "[WARNING] unknown {} value {:?}, keeping {:?}",
            field, raw, current
        );
    }
}

fn parse_string_array(val: &str) -> Vec<String> {
    let val = val.trim();
    if !val.starts_with('[') || !val.ends_with(']') {
        return Vec::new();
    }
    let inner = &val[1..val.len() - 1];
    let mut result = Vec::new();
    let mut current = String::new();
    let mut in_string = false;
    for c in inner.chars() {
        if c == '"' {
            in_string = !in_string;
            continue;
        }
        if c == ',' && !in_string {
            let trimmed = current.trim().to_string();
            if !trimmed.is_empty() {
                result.push(trimmed);
            }
            current.clear();
        } else {
            current.push(c);
        }
    }
    let trimmed = current.trim().to_string();
    if !trimmed.is_empty() {
        result.push(trimmed);
    }
    result
}

pub fn parse_toml(content: &str) -> HashMap<String, HashMap<String, String>> {
    let mut sections: HashMap<String, HashMap<String, String>> = HashMap::new();
    let mut current_section = String::new();

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        if line.starts_with('[') && line.ends_with(']') {
            current_section = line[1..line.len() - 1].trim().to_string();
            continue;
        }

        if let Some(pos) = line.find('=') {
            let key = line[..pos].trim().to_string();
            let raw_val = line[pos + 1..].trim();

            let mut in_string = false;
            let mut val_str = String::new();
            for c in raw_val.chars() {
                if c == '"' {
                    in_string = !in_string;
                }
                if c == '#' && !in_string {
                    break;
                }
                val_str.push(c);
            }
            let value = val_str.trim().to_string();
            sections
                .entry(current_section.clone())
                .or_default()
                .insert(key, value);
        }
    }
    sections
}

impl AppConfig {
    pub fn merge_toml(
        &mut self,
        sections: &HashMap<String, HashMap<String, String>>,
        is_system: bool,
        locked_sections: &mut HashSet<String>,
    ) {
        for (section_name, values) in sections {
            let section_key = section_name.to_lowercase();

            if !is_system && locked_sections.contains(&section_key) {
                continue;
            }

            if is_system {
                if let Some(lp_val) = values.get("lock_policy") {
                    if parse_bool(lp_val) == Some(true) {
                        locked_sections.insert(section_key.clone());
                    }
                }
            }

            match section_key.as_str() {
                "provider" => {
                    if let Some(v) = values.get("api_key_source") {
                        merge_enum_field(
                            "[provider] api_key_source",
                            &mut self.provider.api_key_source,
                            clean_string_val(v),
                            API_KEY_SOURCE_VALUES,
                        );
                    }
                    if let Some(v) = values.get("suppress_api_key_warning") {
                        if let Some(b) = parse_bool(v) {
                            self.provider.suppress_api_key_warning = b;
                        }
                    }
                    if let Some(v) = values.get("lock_policy") {
                        if let Some(b) = parse_bool(v) {
                            self.provider.lock_policy = b;
                        }
                    }
                    if let Some(v) = values.get("local_output_leak_check") {
                        if let Some(b) = parse_bool(v) {
                            self.provider.local_output_leak_check = b;
                        }
                    }
                }
                "proactiveness" => {
                    if let Some(v) = values.get("debounce_ms") {
                        if let Some(u) = parse_u64(v) {
                            self.proactiveness.debounce_ms = u;
                        }
                    }
                    if let Some(v) = values.get("consecutive_failure_threshold") {
                        if let Some(u) = parse_u32(v) {
                            self.proactiveness.consecutive_failure_threshold = u;
                        }
                    }
                }
                "pedagogy" => {
                    if let Some(v) = values.get("style") {
                        merge_enum_field(
                            "[pedagogy] style",
                            &mut self.pedagogy.style,
                            clean_string_val(v),
                            PEDAGOGY_STYLE_VALUES,
                        );
                    }
                    if let Some(v) = values.get("withhold_code") {
                        if let Some(b) = parse_bool(v) {
                            self.pedagogy.withhold_code = b;
                        }
                    }
                    if let Some(v) = values.get("explanation_depth") {
                        self.pedagogy.explanation_depth = clean_string_val(v);
                    }
                    if let Some(v) = values.get("auto_scale") {
                        if let Some(b) = parse_bool(v) {
                            self.pedagogy.auto_scale = b;
                        }
                    }
                    if let Some(v) = values.get("lock_difficulty") {
                        if let Some(b) = parse_bool(v) {
                            self.pedagogy.lock_difficulty = b;
                        }
                    }
                    if let Some(v) = values.get("escape_hatch_threshold") {
                        if let Some(u) = parse_u32(v) {
                            self.pedagogy.escape_hatch_threshold = u;
                        }
                    }
                    if let Some(v) = values.get("max_weekly_bypass_sessions") {
                        if let Some(u) = parse_u32(v) {
                            self.pedagogy.max_weekly_bypass_sessions = u;
                        }
                    }
                    if let Some(v) = values.get("history_limit") {
                        if let Some(u) = parse_u32(v) {
                            self.pedagogy.history_limit = u;
                        }
                    }
                }
                "git.pre_commit" => {
                    if !is_system
                        && (locked_sections.contains("git")
                            || locked_sections.contains("git.pre_commit"))
                    {
                        continue;
                    }
                    if let Some(v) = values.get("action") {
                        self.git.pre_commit.action = clean_string_val(v);
                    }
                    if let Some(v) = values.get("gui_dialog") {
                        if let Some(b) = parse_bool(v) {
                            self.git.pre_commit.gui_dialog = b;
                        }
                    }
                    if let Some(v) = values.get("gui_dialog_timeout_ms") {
                        if let Some(u) = parse_u64(v) {
                            self.git.pre_commit.gui_dialog_timeout_ms = u;
                        }
                    }
                    if let Some(v) = values.get("audit_bypass_logging") {
                        if let Some(b) = parse_bool(v) {
                            self.git.pre_commit.audit_bypass_logging = b;
                        }
                    }
                }
                "evaluation" => {
                    if let Some(v) = values.get("golden_dataset_path") {
                        self.evaluation.golden_dataset_path = clean_string_val(v);
                    }
                }
                "watcher" => {
                    if let Some(v) = values.get("exclude") {
                        self.watcher.exclude = parse_string_array(v);
                    }
                    if let Some(v) = values.get("include_external_links") {
                        self.watcher.include_external_links = parse_string_array(v);
                    }
                    if let Some(v) = values.get("target_cache_limit") {
                        self.watcher.target_cache_limit = clean_string_val(v);
                    }
                    if let Some(v) = values.get("max_watch_threads") {
                        if let Some(u) = parse_u32(v) {
                            self.watcher.max_watch_threads = u;
                        }
                    }
                    if let Some(v) = values.get("max_watch_fds") {
                        if let Some(u) = parse_u32(v) {
                            self.watcher.max_watch_fds = u;
                        }
                    }
                }
                "models.screen" => {
                    if let Some(v) = values.get("provider") {
                        self.models.screen.provider = clean_string_val(v);
                    }
                    if let Some(v) = values.get("model") {
                        self.models.screen.model = clean_string_val(v);
                    }
                    // T8 req 1: optional base_url override (alias defaults
                    // live in provider.rs; explicit config always wins).
                    if let Some(v) = values.get("base_url") {
                        self.models.screen.base_url = Some(clean_string_val(v));
                    }
                }
                "models.judge" => {
                    if let Some(v) = values.get("provider") {
                        self.models.judge.provider = clean_string_val(v);
                    }
                    if let Some(v) = values.get("model") {
                        self.models.judge.model = clean_string_val(v);
                    }
                    if let Some(v) = values.get("base_url") {
                        self.models.judge.base_url = Some(clean_string_val(v));
                    }
                }
                "dial" => {
                    if let Some(v) = values.get("frequency") {
                        self.dial.frequency = clean_string_val(v);
                    }
                    if let Some(v) = values.get("unthrottle") {
                        self.dial.unthrottle = parse_string_array(v);
                    }
                    if let Some(v) = values.get("directness") {
                        merge_enum_field(
                            "[dial] directness",
                            &mut self.dial.directness,
                            clean_string_val(v),
                            DIRECTNESS_VALUES,
                        );
                    }
                }
                "consent" => {
                    if let Some(v) = values.get("solicited_spend") {
                        merge_enum_field(
                            "[consent] solicited_spend",
                            &mut self.consent.solicited_spend,
                            clean_string_val(v),
                            SOLICITED_SPEND_VALUES,
                        );
                    }
                }
                // T10 req 2: `[pack] language` — the config-key override
                // leg of pack resolution. `lock_policy` gating for this
                // section is already handled generically above (the
                // section-name-keyed `locked_sections` check applies to
                // every section, not just `provider`).
                "pack" => {
                    if let Some(v) = values.get("language") {
                        self.pack.language = Some(clean_string_val(v));
                    }
                }
                // T14 req 1: `[trace] enabled` — the trace-capture on/off
                // knob. `lock_policy` gating is handled generically above.
                "trace" => {
                    if let Some(v) = values.get("enabled") {
                        if let Some(b) = parse_bool(v) {
                            self.trace.enabled = b;
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

#[cfg(unix)]
fn check_metadata_security(metadata: &std::fs::Metadata) -> Result<(), String> {
    if std::env::var("MURSHID_TEST_BYPASS_SECURITY").is_ok() {
        return Ok(());
    }

    use std::os::unix::fs::MetadataExt;
    let uid = metadata.uid();
    let mode = metadata.mode() & 0o777;

    let check_uid = std::env::var("MURSHID_TEST_BYPASS_OWNER").is_err();
    if check_uid && uid != 0 {
        return Err(format!("File owner UID is {}, not root (0)", uid));
    }

    if mode != 0o644 && mode != 0o600 {
        return Err(format!(
            "File permissions are {:o}, must be 0644 or 0600",
            mode
        ));
    }

    Ok(())
}

#[cfg(not(unix))]
fn check_metadata_security(_metadata: &std::fs::Metadata) -> Result<(), String> {
    Ok(())
}

/// Verifies the root-owned / 0644-or-0600 gate on a system config file. Opens
/// the file and inspects the metadata of the *open descriptor* (fstat), so it
/// cannot be fooled by a path that changes between check and use.
pub fn check_system_config_security(path: &Path) -> Result<(), String> {
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let metadata = file.metadata().map_err(|e| e.to_string())?;
    check_metadata_security(&metadata)
}

/// Reads the system config only if it passes the ownership/permission gate,
/// checking and reading through **one** open descriptor. This closes the TOCTOU
/// window a `stat(path)`-then-`read(path)` pair leaves open: an attacker who can
/// write the system-config directory could otherwise pass the gate on a benign
/// file and swap in a malicious one before the read. `Ok(None)` means the file
/// is absent; `Err` means it exists but failed the gate (or could not be read).
fn read_secure_system_config(path: &Path) -> Result<Option<String>, String> {
    use std::io::Read;
    let mut file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };
    let metadata = file.metadata().map_err(|e| e.to_string())?;
    check_metadata_security(&metadata)?;
    let mut content = String::new();
    file.read_to_string(&mut content)
        .map_err(|e| e.to_string())?;
    Ok(Some(content))
}

/// Loads the layered config (system → user → project). NOT memoized: a
/// process may legitimately re-read after a config file changes on disk
/// (`watcher_coordinator::acquire_resources` re-reads the watcher limits, and
/// its test rewrites the user config mid-run and relies on the fresh read), so
/// a global `OnceLock` cache would be incorrect. Redundant re-reads within a
/// single logical operation are instead avoided by loading once and threading
/// the `&AppConfig` (e.g. `load_keys_from_source` now loads once, not twice).
pub fn load_config() -> AppConfig {
    let mut config = AppConfig::default();
    let mut locked_sections = HashSet::new();

    let system_path = get_system_config_path();
    match read_secure_system_config(&system_path) {
        Ok(Some(content)) => {
            let parsed = parse_toml(&content);
            config.merge_toml(&parsed, true, &mut locked_sections);
        }
        Ok(None) => {}
        Err(e) => {
            eprintln!(
                "[WARNING] System configuration security check failed for {}: {}. Ignoring system overrides.",
                system_path.display(),
                e
            );
        }
    }

    if let Some(user_path) = resolve_user_config_path() {
        if user_path.exists() {
            if let Ok(content) = std::fs::read_to_string(&user_path) {
                let parsed = parse_toml(&content);
                config.merge_toml(&parsed, false, &mut locked_sections);
            }
        }
    }

    if let Some(project_path) = get_project_config_path() {
        if project_path.exists() {
            if let Ok(content) = std::fs::read_to_string(&project_path) {
                let parsed = parse_toml(&content);
                config.merge_toml(&parsed, false, &mut locked_sections);
            }
        }
    }

    config
}

/// Redesign R0 (G5): persists the two LIVE session dials — `[dial]
/// frequency`/`directness`, the settings-overlay (`s`) values — back to the
/// on-disk config so a change made mid-session survives past quit. A
/// targeted text edit, not a full parse-and-re-emit round-trip: this repo's
/// config reader (`parse_toml`/`merge_toml` above) has no matching writer,
/// and there is no `toml` crate in the tree to serialize with (C10's
/// allowlist), so a "load struct -> re-emit every field" approach would
/// silently drop every OTHER section (`[models.screen]`, `[models.judge]`,
/// `[watcher]`, ...) and every comment the user already has on disk. Instead
/// this rewrites only the `frequency =`/`directness =` lines inside (or
/// appended to, if absent) the file's `[dial]` section — every other line,
/// including comments elsewhere in the file, is passed through byte-for-byte.
/// Known loss: an inline trailing comment on the SAME line as a rewritten
/// `frequency =`/`directness =` value is dropped along with that line (the
/// spec's accepted trade-off); everything else survives.
pub fn write_dial_config(path: &Path, directness: &str, frequency: &str) -> std::io::Result<()> {
    let existing = std::fs::read_to_string(path).unwrap_or_default();
    let updated = set_dial_lines(&existing, directness, frequency);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, updated)
}

/// Pure helper behind [`write_dial_config`]: returns `content` with the
/// `[dial]` section's `frequency`/`directness` lines rewritten in place (or
/// appended, if the key or the whole section is missing). Split out so the
/// line-surgery logic is testable without any filesystem I/O.
fn set_dial_lines(content: &str, directness: &str, frequency: &str) -> String {
    let mut lines: Vec<String> = content.lines().map(|l| l.to_string()).collect();

    let section_start = lines.iter().position(|l| l.trim() == "[dial]");

    let (section_start, section_end) = match section_start {
        Some(start) => {
            let mut end = lines.len();
            for (i, l) in lines.iter().enumerate().skip(start + 1) {
                let t = l.trim();
                if t.starts_with('[') && t.ends_with(']') {
                    end = i;
                    break;
                }
            }
            (start, end)
        }
        None => {
            // No `[dial]` section on disk yet: append a new one at the end,
            // with a blank separator line first if the file has trailing
            // content that isn't already blank.
            if !lines.is_empty() && !lines.last().unwrap().trim().is_empty() {
                lines.push(String::new());
            }
            lines.push("[dial]".to_string());
            let start = lines.len() - 1;
            (start, lines.len())
        }
    };

    let mut found_frequency = false;
    let mut found_directness = false;
    for l in lines[section_start + 1..section_end].iter_mut() {
        let trimmed = l.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some(pos) = trimmed.find('=') {
            let key = trimmed[..pos].trim();
            if key == "frequency" {
                *l = format!("frequency = \"{}\"", frequency);
                found_frequency = true;
            } else if key == "directness" {
                *l = format!("directness = \"{}\"", directness);
                found_directness = true;
            }
        }
    }

    let mut insert_pos = section_end;
    if !found_frequency {
        lines.insert(insert_pos, format!("frequency = \"{}\"", frequency));
        insert_pos += 1;
    }
    if !found_directness {
        lines.insert(insert_pos, format!("directness = \"{}\"", directness));
    }

    let mut out = lines.join("\n");
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write;

    #[test]
    fn test_parse_toml_basic() {
        let content = r#"
            [provider]
            api_key_source = "keychain"
            suppress_api_key_warning = false
            lock_policy = true # inline comment

            [proactiveness]
            debounce_ms = 1200
            
            [watcher]
            exclude = ["**/target/**", "**/.git/**"]
        "#;
        let parsed = parse_toml(content);

        assert_eq!(
            parsed
                .get("provider")
                .unwrap()
                .get("api_key_source")
                .unwrap(),
            "\"keychain\""
        );
        assert_eq!(
            parsed
                .get("provider")
                .unwrap()
                .get("suppress_api_key_warning")
                .unwrap(),
            "false"
        );
        assert_eq!(
            parsed.get("provider").unwrap().get("lock_policy").unwrap(),
            "true"
        );
        assert_eq!(
            parsed
                .get("proactiveness")
                .unwrap()
                .get("debounce_ms")
                .unwrap(),
            "1200"
        );
        assert_eq!(
            parsed.get("watcher").unwrap().get("exclude").unwrap(),
            "[\"**/target/**\", \"**/.git/**\"]"
        );
    }

    #[test]
    fn test_clean_string_val() {
        assert_eq!(clean_string_val("\"hello\""), "hello");
        assert_eq!(clean_string_val("hello"), "hello");
    }

    #[test]
    fn test_parse_string_array() {
        let arr = parse_string_array("[\"a\", \"b\"]");
        assert_eq!(arr, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn test_merge_toml_precedence() {
        // Later layers override earlier ones. Uses valid `api_key_source`
        // values (item 5 rejects unknown ones), alternating so each merge
        // visibly changes the field.
        let mut config = AppConfig::default();
        let mut locked = HashSet::new();

        let system_toml = parse_toml(
            r#"
            [provider]
            api_key_source = "environment"
        "#,
        );
        config.merge_toml(&system_toml, false, &mut locked);
        assert_eq!(config.provider.api_key_source, "environment");

        let user_toml = parse_toml(
            r#"
            [provider]
            api_key_source = "keychain"
        "#,
        );
        config.merge_toml(&user_toml, false, &mut locked);
        assert_eq!(config.provider.api_key_source, "keychain");

        let project_toml = parse_toml(
            r#"
            [provider]
            api_key_source = "environment"
        "#,
        );
        config.merge_toml(&project_toml, false, &mut locked);
        assert_eq!(config.provider.api_key_source, "environment");
    }

    #[test]
    fn test_merge_toml_unknown_enum_value_keeps_default_and_does_not_downgrade() {
        // Item 5 security invariant: a typo in api_key_source must NOT
        // silently downgrade secret storage to plaintext env — the secure
        // `keychain` default is kept.
        let mut config = AppConfig::default();
        let mut locked = HashSet::new();
        let toml = parse_toml(
            r#"
            [provider]
            api_key_source = "keycahin"
        "#,
        );
        config.merge_toml(&toml, false, &mut locked);
        assert_eq!(config.provider.api_key_source, "keychain");
    }

    #[test]
    fn test_models_config_defaults_are_cheap_screen_strong_judge() {
        let config = AppConfig::default();
        assert_eq!(config.models.screen.provider, "gemini");
        assert_eq!(config.models.judge.provider, "claude");
    }

    // --- T8 req 1: base_url is absent for standard installs ---

    #[test]
    fn test_models_config_base_url_defaults_to_none() {
        let config = AppConfig::default();
        assert_eq!(config.models.screen.base_url, None);
        assert_eq!(config.models.judge.base_url, None);
    }

    #[test]
    fn test_merge_toml_models_base_url() {
        let mut config = AppConfig::default();
        let mut locked = HashSet::new();

        let toml = parse_toml(
            r#"
            [models.screen]
            provider = "lmstudio"
            model = "some-local-model"
            base_url = "http://192.168.1.50:1234/v1"

            [models.judge]
            provider = "openai"
            model = "gpt-4o"
            base_url = "http://localhost:8000/v1"
        "#,
        );
        config.merge_toml(&toml, false, &mut locked);

        assert_eq!(
            config.models.screen.base_url,
            Some("http://192.168.1.50:1234/v1".to_string())
        );
        assert_eq!(
            config.models.judge.base_url,
            Some("http://localhost:8000/v1".to_string())
        );
    }

    #[test]
    fn test_merge_toml_models_without_base_url_key_leaves_it_none() {
        let mut config = AppConfig::default();
        let mut locked = HashSet::new();

        let toml = parse_toml(
            r#"
            [models.screen]
            provider = "ollama"
            model = "llama3"
        "#,
        );
        config.merge_toml(&toml, false, &mut locked);

        assert_eq!(config.models.screen.base_url, None);
    }

    #[test]
    fn test_merge_toml_two_slot_models_config() {
        let mut config = AppConfig::default();
        let mut locked = HashSet::new();

        let toml = parse_toml(
            r#"
            [models.screen]
            provider = "ollama"
            model = "llama3"

            [models.judge]
            provider = "claude"
            model = "claude-3-5-sonnet-20241022"
        "#,
        );
        config.merge_toml(&toml, false, &mut locked);

        assert_eq!(config.models.screen.provider, "ollama");
        assert_eq!(config.models.screen.model, "llama3");
        assert_eq!(config.models.judge.provider, "claude");
        assert_eq!(config.models.judge.model, "claude-3-5-sonnet-20241022");
    }

    #[test]
    fn test_merge_toml_locked() {
        let mut config = AppConfig::default();
        let mut locked = HashSet::new();

        // 1. Merge system config with lock_policy = true
        let system_toml = parse_toml(
            r#"
            [provider]
            api_key_source = "environment"
            lock_policy = true
        "#,
        );
        config.merge_toml(&system_toml, true, &mut locked);
        assert_eq!(config.provider.api_key_source, "environment");
        assert!(locked.contains("provider"));

        // 2. Try to override via user config (should be ignored)
        let user_toml = parse_toml(
            r#"
            [provider]
            api_key_source = "keychain"
        "#,
        );
        config.merge_toml(&user_toml, false, &mut locked);
        assert_eq!(config.provider.api_key_source, "environment"); // still the locked system value
    }

    #[test]
    fn test_dial_defaults_to_quiet() {
        let config = AppConfig::default();
        assert_eq!(config.dial.frequency, "quiet");
        assert!(config.dial.unthrottle.is_empty());
    }

    #[test]
    fn test_merge_toml_dial_section() {
        let mut config = AppConfig::default();
        let mut locked = HashSet::new();

        let toml = parse_toml(
            r#"
            [dial]
            frequency = "chatty"
            unthrottle = ["idiom", "architecture"]
        "#,
        );
        config.merge_toml(&toml, false, &mut locked);

        assert_eq!(config.dial.frequency, "chatty");
        assert_eq!(
            config.dial.unthrottle,
            vec!["idiom".to_string(), "architecture".to_string()]
        );
    }

    // --- T4 req 2: directness knob ---

    #[test]
    fn test_directness_defaults_to_balanced() {
        let config = AppConfig::default();
        assert_eq!(config.dial.directness, "balanced");
    }

    #[test]
    fn test_merge_toml_dial_directness() {
        let mut config = AppConfig::default();
        let mut locked = HashSet::new();
        let toml = parse_toml(
            r#"
            [dial]
            directness = "tell-me"
        "#,
        );
        config.merge_toml(&toml, false, &mut locked);
        assert_eq!(config.dial.directness, "tell-me");
    }

    // --- T4 req 8/12: BYOK consent (C6) ---

    #[test]
    fn test_consent_defaults_to_ask() {
        let config = AppConfig::default();
        assert_eq!(config.consent.solicited_spend, "ask");
    }

    #[test]
    fn test_merge_toml_consent_section() {
        let mut config = AppConfig::default();
        let mut locked = HashSet::new();
        let toml = parse_toml(
            r#"
            [consent]
            solicited_spend = "always"
        "#,
        );
        config.merge_toml(&toml, false, &mut locked);
        assert_eq!(config.consent.solicited_spend, "always");
    }

    // --- T10 req 2: `[pack] language` config key ---

    #[test]
    fn test_pack_config_defaults_to_no_override() {
        let config = AppConfig::default();
        assert_eq!(config.pack.language, None);
    }

    #[test]
    fn test_merge_toml_pack_section() {
        let mut config = AppConfig::default();
        let mut locked = HashSet::new();
        let toml = parse_toml(
            r#"
            [pack]
            language = "go"
        "#,
        );
        config.merge_toml(&toml, false, &mut locked);
        assert_eq!(config.pack.language, Some("go".to_string()));
    }

    #[test]
    fn test_merge_toml_pack_precedence_project_over_user() {
        let mut config = AppConfig::default();
        let mut locked = HashSet::new();

        let user_toml = parse_toml("[pack]\nlanguage = \"go\"\n");
        config.merge_toml(&user_toml, false, &mut locked);
        assert_eq!(config.pack.language, Some("go".to_string()));

        let project_toml = parse_toml("[pack]\nlanguage = \"rust\"\n");
        config.merge_toml(&project_toml, false, &mut locked);
        assert_eq!(config.pack.language, Some("rust".to_string()));
    }

    /// A system config that locks the `pack` section (via the generic
    /// `lock_policy` mechanism, same as every other section) must resist a
    /// project-level override of `[pack] language`.
    #[test]
    fn test_merge_toml_pack_section_respects_lock_policy() {
        let mut config = AppConfig::default();
        let mut locked = HashSet::new();

        let system_toml = parse_toml(
            r#"
            [pack]
            language = "go"
            lock_policy = true
        "#,
        );
        config.merge_toml(&system_toml, true, &mut locked);
        assert_eq!(config.pack.language, Some("go".to_string()));
        assert!(locked.contains("pack"));

        let project_toml = parse_toml("[pack]\nlanguage = \"rust\"\n");
        config.merge_toml(&project_toml, false, &mut locked);
        assert_eq!(config.pack.language, Some("go".to_string()));
    }

    // --- T14 req 1: `[trace] enabled` knob ---

    #[test]
    fn test_trace_defaults_to_enabled() {
        let config = AppConfig::default();
        assert!(config.trace.enabled);
    }

    #[test]
    fn test_merge_toml_trace_section_can_disable() {
        let mut config = AppConfig::default();
        let mut locked = HashSet::new();
        let toml = parse_toml(
            r#"
            [trace]
            enabled = false
        "#,
        );
        config.merge_toml(&toml, false, &mut locked);
        assert!(!config.trace.enabled);
    }

    #[cfg(unix)]
    #[test]
    fn test_system_config_security() {
        // T9 req 9: env mutation below is process-global — hold the crate
        // env lock like every other env-mutating test.
        let _env_lock = crate::credentials::env_test_lock();
        let temp_dir = std::env::temp_dir();
        let test_file = temp_dir.join("test_murshid_sys_config.toml");
        {
            let mut f = File::create(&test_file).unwrap();
            f.write_all(b"lock_policy = true").unwrap();
        }

        // Set test environment variable to bypass root owner check for this test
        unsafe {
            std::env::set_var("MURSHID_TEST_BYPASS_OWNER", "1");
        }

        // Set permission to 0644
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&test_file).unwrap().permissions();
        perms.set_mode(0o644);
        std::fs::set_permissions(&test_file, perms).unwrap();

        // Security check should succeed with 0644
        assert!(check_system_config_security(&test_file).is_ok());

        // Set permission to 0600
        let mut perms = std::fs::metadata(&test_file).unwrap().permissions();
        perms.set_mode(0o600);
        std::fs::set_permissions(&test_file, perms).unwrap();

        // Security check should succeed with 0600
        assert!(check_system_config_security(&test_file).is_ok());

        // Set permission to 0755
        let mut perms = std::fs::metadata(&test_file).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&test_file, perms).unwrap();

        // Security check should fail with 0755
        assert!(check_system_config_security(&test_file).is_err());

        // Clean up
        std::fs::remove_file(&test_file).unwrap();
        unsafe {
            std::env::remove_var("MURSHID_TEST_BYPASS_OWNER");
        }
    }

    // --- Redesign R0 (G5): `[dial]` write-back round-trip ---
    // No env/HOME mutation anywhere below: every path is an injected temp
    // file, never the resolved real user config path.

    #[test]
    fn test_set_dial_lines_rewrites_in_place_and_preserves_other_sections() {
        let content = r#"[models.screen]
provider = "gemini"
model = "gemini-2.5-flash"

[dial]
frequency = "quiet"
unthrottle = ["idiom"]
directness = "balanced"

[models.judge]
provider = "claude"
model = "claude-3-5-sonnet-20241022"
"#;
        let updated = set_dial_lines(content, "tell-me", "chatty");

        assert!(updated.contains("[models.screen]"));
        assert!(updated.contains("provider = \"gemini\""));
        assert!(updated.contains("model = \"gemini-2.5-flash\""));
        assert!(updated.contains("[models.judge]"));
        assert!(updated.contains("provider = \"claude\""));
        assert!(updated.contains("model = \"claude-3-5-sonnet-20241022\""));
        assert!(updated.contains("unthrottle = [\"idiom\"]"));

        assert!(updated.contains("frequency = \"chatty\""));
        assert!(updated.contains("directness = \"tell-me\""));
        // The stale values must not survive alongside the new ones.
        assert!(!updated.contains("frequency = \"quiet\""));
        assert!(!updated.contains("directness = \"balanced\""));
    }

    #[test]
    fn test_set_dial_lines_appends_dial_section_when_absent() {
        let content = "[models.screen]\nprovider = \"gemini\"\nmodel = \"gemini-2.5-flash\"\n";
        let updated = set_dial_lines(content, "guide-me", "standard");

        assert!(updated.contains("[models.screen]"));
        assert!(updated.contains("provider = \"gemini\""));
        assert!(updated.contains("[dial]"));
        assert!(updated.contains("frequency = \"standard\""));
        assert!(updated.contains("directness = \"guide-me\""));
    }

    #[test]
    fn test_set_dial_lines_on_empty_content_produces_dial_only() {
        let updated = set_dial_lines("", "balanced", "quiet");
        assert!(updated.contains("[dial]"));
        assert!(updated.contains("frequency = \"quiet\""));
        assert!(updated.contains("directness = \"balanced\""));
    }

    #[test]
    fn test_write_dial_config_round_trip_via_temp_file_preserves_models_sections() {
        let temp_dir = std::env::temp_dir();
        let test_file = temp_dir.join(format!(
            "murshid_test_dial_roundtrip_{}.toml",
            std::process::id()
        ));

        let initial = r#"# a user comment above provider
[provider]
api_key_source = "keychain"

[models.screen]
provider = "gemini"
model = "gemini-2.5-flash"

[models.judge]
provider = "claude"
model = "claude-3-5-sonnet-20241022"
base_url = "http://localhost:8000/v1"

[dial]
frequency = "quiet"
directness = "balanced"
"#;
        std::fs::write(&test_file, initial).unwrap();

        // Load, exactly as `load_config`'s user-config layer would.
        let mut config = AppConfig::default();
        let mut locked = HashSet::new();
        let parsed = parse_toml(&std::fs::read_to_string(&test_file).unwrap());
        config.merge_toml(&parsed, false, &mut locked);
        assert_eq!(config.dial.frequency, "quiet");
        assert_eq!(config.dial.directness, "balanced");

        // Change directness + frequency (as the settings overlay would) and
        // write back.
        write_dial_config(&test_file, "tell-me", "chatty").unwrap();

        // Reload from the SAME file and assert the new dial values landed
        // AND the [models.*] sections are byte-value-intact.
        let mut reloaded = AppConfig::default();
        let mut locked2 = HashSet::new();
        let new_content = std::fs::read_to_string(&test_file).unwrap();
        let reparsed = parse_toml(&new_content);
        reloaded.merge_toml(&reparsed, false, &mut locked2);

        assert_eq!(reloaded.dial.frequency, "chatty");
        assert_eq!(reloaded.dial.directness, "tell-me");

        assert_eq!(reloaded.provider.api_key_source, "keychain");
        assert_eq!(reloaded.models.screen.provider, "gemini");
        assert_eq!(reloaded.models.screen.model, "gemini-2.5-flash");
        assert_eq!(reloaded.models.judge.provider, "claude");
        assert_eq!(
            reloaded.models.judge.model,
            "claude-3-5-sonnet-20241022"
        );
        assert_eq!(
            reloaded.models.judge.base_url,
            Some("http://localhost:8000/v1".to_string())
        );

        // The comment and every other section's lines survive byte-for-byte
        // (this repo's writer is a targeted [dial]-only edit, not a
        // full re-serialize, so there is nothing else to lose).
        assert!(new_content.contains("# a user comment above provider"));
        assert!(new_content.contains("[provider]"));
        assert!(new_content.contains("api_key_source = \"keychain\""));

        std::fs::remove_file(&test_file).unwrap();
    }

    #[test]
    fn test_write_dial_config_creates_file_when_absent() {
        let temp_dir = std::env::temp_dir();
        let test_dir = temp_dir.join(format!("murshid_test_dial_newdir_{}", std::process::id()));
        let test_file = test_dir.join("config.toml");
        let _ = std::fs::remove_dir_all(&test_dir);

        assert!(!test_file.exists());
        write_dial_config(&test_file, "balanced", "quiet").unwrap();
        assert!(test_file.exists());

        let content = std::fs::read_to_string(&test_file).unwrap();
        assert!(content.contains("frequency = \"quiet\""));
        assert!(content.contains("directness = \"balanced\""));

        std::fs::remove_dir_all(&test_dir).unwrap();
    }

    #[test]
    fn test_write_dial_config_write_error_does_not_panic() {
        // A path whose PARENT is itself a plain file (not a directory) can
        // never be created/opened for write — this must return an `Err`,
        // never panic, so the settings overlay can log-and-continue instead
        // of crashing the TUI.
        let temp_dir = std::env::temp_dir();
        let parent_as_file = temp_dir.join(format!(
            "murshid_test_dial_parent_is_file_{}",
            std::process::id()
        ));
        std::fs::write(&parent_as_file, "not a directory").unwrap();
        let bogus_path = parent_as_file.join("config.toml");

        let result = write_dial_config(&bogus_path, "balanced", "quiet");
        assert!(result.is_err());

        std::fs::remove_file(&parent_as_file).unwrap();
    }
}
