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

#[derive(Debug, Clone, Default, PartialEq)]
pub struct PedagogyTeamConfig {
    pub rules_path: String,
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
    pub team: PedagogyTeamConfig,
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
            team: PedagogyTeamConfig::default(),
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
                        self.provider.api_key_source = clean_string_val(v);
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
                        self.pedagogy.style = clean_string_val(v);
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
                "pedagogy.team" => {
                    if !is_system && locked_sections.contains("pedagogy") {
                        continue;
                    }
                    if let Some(v) = values.get("rules_path") {
                        self.pedagogy.team.rules_path = clean_string_val(v);
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
                        self.dial.directness = clean_string_val(v);
                    }
                }
                "consent" => {
                    if let Some(v) = values.get("solicited_spend") {
                        self.consent.solicited_spend = clean_string_val(v);
                    }
                }
                _ => {}
            }
        }
    }
}

#[cfg(unix)]
pub fn check_system_config_security(path: &Path) -> Result<(), String> {
    if std::env::var("MURSHID_TEST_BYPASS_SECURITY").is_ok() {
        return Ok(());
    }

    use std::os::unix::fs::MetadataExt;
    let metadata = std::fs::metadata(path).map_err(|e| e.to_string())?;
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
pub fn check_system_config_security(_path: &Path) -> Result<(), String> {
    Ok(())
}

pub fn load_config() -> AppConfig {
    let mut config = AppConfig::default();
    let mut locked_sections = HashSet::new();

    let system_path = get_system_config_path();
    if system_path.exists() {
        match check_system_config_security(&system_path) {
            Ok(()) => {
                if let Ok(content) = std::fs::read_to_string(&system_path) {
                    let parsed = parse_toml(&content);
                    config.merge_toml(&parsed, true, &mut locked_sections);
                }
            }
            Err(e) => {
                eprintln!(
                    "[WARNING] System configuration security check failed for {}: {}. Ignoring system overrides.",
                    system_path.display(),
                    e
                );
            }
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
        let mut config = AppConfig::default();
        let mut locked = HashSet::new();

        let system_toml = parse_toml(
            r#"
            [provider]
            api_key_source = "system_val"
        "#,
        );
        config.merge_toml(&system_toml, false, &mut locked);
        assert_eq!(config.provider.api_key_source, "system_val");

        let user_toml = parse_toml(
            r#"
            [provider]
            api_key_source = "user_val"
        "#,
        );
        config.merge_toml(&user_toml, false, &mut locked);
        assert_eq!(config.provider.api_key_source, "user_val");

        let project_toml = parse_toml(
            r#"
            [provider]
            api_key_source = "project_val"
        "#,
        );
        config.merge_toml(&project_toml, false, &mut locked);
        assert_eq!(config.provider.api_key_source, "project_val");
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
            api_key_source = "system_val"
            lock_policy = true
        "#,
        );
        config.merge_toml(&system_toml, true, &mut locked);
        assert_eq!(config.provider.api_key_source, "system_val");
        assert!(locked.contains("provider"));

        // 2. Try to override via user config (should be ignored)
        let user_toml = parse_toml(
            r#"
            [provider]
            api_key_source = "user_val"
        "#,
        );
        config.merge_toml(&user_toml, false, &mut locked);
        assert_eq!(config.provider.api_key_source, "system_val"); // still system_val
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

    #[cfg(unix)]
    #[test]
    fn test_system_config_security() {
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
}
