//! Language-pack loading and registry (T6 — D23/D24, I27-I30, C9/C10).
//!
//! This module is the engine/pack seam's ONE allowed home for
//! language-specific literals ("rust", "cargo", "clippy", grammar crate
//! names, ...): it is both the pack *loader* (reads data payloads 1-6 off
//! disk) and the pack *registry* (the small match/table that resolves a
//! pack directory to its compiled-in tree-sitter grammar and diagnostics
//! adapter — I27/C9: full dynamic plugin loading was rejected for v1, so
//! "one piece of code per pack" is realized as a compile-time-linked
//! implementation selected here by the pack's directory name). No other
//! engine module may reference a specific language: they all take pack DATA
//! (taxonomy, canon, surface, prompts, grammar) as plain parameters.
//!
//! Adding a language (T7's honesty test, I29) means: a new `packs/<lang>/`
//! data directory, a new adapter implementing [`DiagnosticsAdapter`], and
//! one new match arm each in [`resolve_ts_language`] and
//! [`diagnostics_adapter`] — nothing outside this file changes.

use std::path::{Path, PathBuf};

/// The Rust pack's diagnostics adapter (I27's "one narrow adapter"),
/// declared as a child of the seam file per T7's tightened pass criterion:
/// the entire code-registration surface — every adapter module and its
/// registry match arm — lives inside `pack.rs`, never `main.rs`. The file
/// itself stays at `src/compiler.rs` (no move needed for a `#[path]`
/// declaration); adding a language means adding one sibling declaration
/// here, never touching `main.rs`'s module list.
#[path = "compiler.rs"]
mod compiler;

/// The Go pack's diagnostics adapter (T7's honesty test, I29): a second
/// sibling declaration, exactly mirroring `compiler`'s — the file stays at
/// `src/go_adapter.rs`, mounted here as a child of the seam file only.
#[path = "go_adapter.rs"]
mod go_adapter;

// ---------------------------------------------------------------------
// Pack-path resolution (T1-review flag / T6 scope item 4)
// ---------------------------------------------------------------------

/// Resolves the directory containing all installed packs, in order:
/// 1. `MURSHID_PACKS_DIR` env var (explicit override, e.g. for tests/CI).
/// 2. Exe-relative `../share/murshid/packs` (a brew/installed binary's
///    layout: `<prefix>/bin/murshid` + `<prefix>/share/murshid/packs/`).
/// 3. The platform XDG/user data dir (`murshid/packs`).
/// 4. `CARGO_MANIFEST_DIR/packs` (dev/checkout fallback — always last).
pub fn resolve_packs_dir() -> PathBuf {
    resolve_packs_dir_from(std::env::var("MURSHID_PACKS_DIR").ok())
}

/// Pure resolution given an explicit override value (step 1 of the order
/// above). T9 req 9 addendum: tests exercise precedence through THIS
/// function with an explicit `Some`/`None` instead of mutating the
/// process-global `MURSHID_PACKS_DIR` — a mutating test once raced a
/// concurrent pack-loading reader onto its synthetic pack dir (observed:
/// the surface lockstep test read empty help_patterns). No test may set
/// that env var.
fn resolve_packs_dir_from(env_override: Option<String>) -> PathBuf {
    if let Some(dir) = env_override {
        if !dir.is_empty() {
            return PathBuf::from(dir);
        }
    }

    if let Ok(exe) = std::env::current_exe() {
        if let Some(exe_dir) = exe.parent() {
            let candidate = exe_dir.join("../share/murshid/packs");
            if candidate.is_dir() {
                return candidate;
            }
        }
    }

    if let Some(data_dir) = xdg_data_packs_dir() {
        if data_dir.is_dir() {
            return data_dir;
        }
    }

    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("packs")
}

fn xdg_data_packs_dir() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        crate::config::get_home_dir().map(|h| h.join("Library/Application Support/murshid/packs"))
    }
    #[cfg(target_os = "windows")]
    {
        std::env::var("APPDATA")
            .ok()
            .map(|a| PathBuf::from(a).join("murshid\\packs"))
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        if let Ok(xdg) = std::env::var("XDG_DATA_HOME") {
            if !xdg.is_empty() {
                return Some(PathBuf::from(xdg).join("murshid/packs"));
            }
        }
        crate::config::get_home_dir().map(|h| h.join(".local/share/murshid/packs"))
    }
}

/// The engine's last-resort fallback pack id (T10 req 2c) — the ONE
/// hardcoded `"rust"` literal allowed in pack-id resolution outside the
/// match-arm registries above (T10 acceptance grep). Every production call
/// site that used to hardcode a pack now goes through
/// [`resolve_pack_id`]/[`resolve_pack_dir`] instead, which both read this
/// constant rather than repeating the literal.
const FALLBACK_PACK_ID: &str = "rust";

/// The default active pack directory: [`FALLBACK_PACK_ID`]'s directory.
/// Used by this module's own unit tests (which always exercise the bundled
/// Rust pack) and as the fallback destination in [`resolve_pack_dir`].
pub fn default_pack_dir() -> PathBuf {
    resolve_packs_dir().join(FALLBACK_PACK_ID)
}

// ---------------------------------------------------------------------
// T10 — pack auto-detection (SPEC v0.5 D2/I29; founder ruling 2026-07-03:
// marker auto-detection, not just a config key). Makes the Go pack shipped
// in T7 reachable without hand-editing config.
// ---------------------------------------------------------------------

/// T10 req 2/6: the four marker outcomes at a project root. Kept as its own
/// enum (rather than returning the id `String` directly) so the pure
/// detection function stays fully testable independent of the "which id
/// wins" policy decision, which lives in [`resolve_pack_id`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkerDetection {
    GoOnly,
    RustOnly,
    Both,
    Neither,
}

/// T10 req 6: pure marker detection over a caller-supplied directory
/// listing (never touches the filesystem itself, so it's trivially
/// testable) — `go.mod` -> go, `Cargo.toml` -> rust, both present -> Both,
/// neither -> Neither. The policy for what to DO with each outcome (which
/// id wins, whether to print a notice) lives in [`resolve_pack_id`].
pub fn detect_pack_marker(entries: &[String]) -> MarkerDetection {
    let has_go = entries.iter().any(|e| e == "go.mod");
    let has_rust = entries.iter().any(|e| e == "Cargo.toml");
    match (has_go, has_rust) {
        (true, true) => MarkerDetection::Both,
        (true, false) => MarkerDetection::GoOnly,
        (false, true) => MarkerDetection::RustOnly,
        (false, false) => MarkerDetection::Neither,
    }
}

/// T10 req 3: the one startup line printed when both markers are present
/// and no `[pack] language` override resolves the ambiguity — names both
/// the choice made (`rust`) and the override key, per req 3's "never a
/// prompt, never an error".
pub fn ambiguous_marker_notice() -> String {
    "[murshid] both go.mod and Cargo.toml found at the project root — defaulting to the rust pack (set `[pack] language = \"go\"` to override).".to_string()
}

/// Lists the bare file/directory names directly under `dir` (T10 req 6's
/// "caller supplies the root dir listing" seam — the impure half of marker
/// detection, kept separate from the pure [`detect_pack_marker`]). Returns
/// an empty listing if `dir` can't be read (matches the fallback's
/// existing "neither marker present" behavior).
fn dir_entry_names(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .map(|read_dir| {
            read_dir
                .filter_map(|entry| entry.ok())
                .filter_map(|entry| entry.file_name().into_string().ok())
                .collect()
        })
        .unwrap_or_default()
}

/// T10 req 1/2: the single pack-id resolution entry point used by every
/// command that loads a pack (`watch`, `review`, `progress`). Precedence:
/// (a) explicit `[pack] language` config key; (b) marker detection at
/// `project_root` (`go.mod` -> `"go"`, `Cargo.toml` -> `"rust"`, both ->
/// `"rust"` + one startup notice, neither -> silent `"rust"`); (c) fallback
/// `"rust"` (unreachable in practice since (b)'s `Neither` arm already
/// returns it, but keeps the precedence chain explicit).
pub fn resolve_pack_id(project_root: &Path, config: &crate::config::AppConfig) -> String {
    // (a) explicit config override.
    if let Some(language) = config.pack.language.as_ref() {
        if !language.is_empty() {
            return language.clone();
        }
    }

    // (b) marker detection.
    let entries = dir_entry_names(project_root);
    match detect_pack_marker(&entries) {
        MarkerDetection::GoOnly => "go".to_string(),
        MarkerDetection::RustOnly => FALLBACK_PACK_ID.to_string(),
        MarkerDetection::Both => {
            println!("{}", ambiguous_marker_notice());
            FALLBACK_PACK_ID.to_string()
        }
        MarkerDetection::Neither => FALLBACK_PACK_ID.to_string(),
    }
}

/// T10 req 1/4: the resolved pack DIRECTORY for `project_root` under
/// `config` — the single call every `default_pack_dir()` production call
/// site (`watch`/`review`/`progress`) replaces. Resolves the id via
/// [`resolve_pack_id`], then falls back to the bundled Rust pack (via the
/// existing [`payload_fallback_notice`] mechanism — no crash, no new notice
/// format) if `packs/<id>/` doesn't exist, e.g. a stale/bogus configured id
/// or a detected id (`"go"`) whose pack was never installed.
pub fn resolve_pack_dir(project_root: &Path, config: &crate::config::AppConfig) -> PathBuf {
    let id = resolve_pack_id(project_root, config);
    let packs_root = resolve_packs_dir();
    let candidate = packs_root.join(&id);
    if candidate.is_dir() {
        return candidate;
    }
    println!(
        "{}",
        payload_fallback_notice("pack directory", &candidate, "no such directory")
    );
    packs_root.join(FALLBACK_PACK_ID)
}

/// A pack's language id is its directory name (`packs/rust` -> `"rust"`,
/// `packs/go` -> `"go"`) — no separate manifest field needed.
fn language_id_from_pack_dir(pack_dir: &Path) -> String {
    pack_dir
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string()
}

// ---------------------------------------------------------------------
// Payload 2 — concept taxonomy
// ---------------------------------------------------------------------

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize, PartialEq, Eq)]
pub struct TaxonomyConcept {
    pub slug: String,
    pub name: String,
    pub category: String,
}

#[derive(Debug, Clone, serde::Deserialize)]
struct TaxonomyFile {
    concepts: Vec<TaxonomyConcept>,
}

pub fn load_taxonomy(pack_dir: &Path) -> Result<Vec<TaxonomyConcept>, String> {
    let path = pack_dir.join("taxonomy.json");
    let content = std::fs::read_to_string(&path)
        .map_err(|e| format!("Failed to read {}: {}", path.display(), e))?;
    let parsed: TaxonomyFile =
        serde_json::from_str(&content).map_err(|e| format!("Failed to parse taxonomy: {}", e))?;
    Ok(parsed.concepts)
}

// ---------------------------------------------------------------------
// Payload 3 — idiom canon
// ---------------------------------------------------------------------

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize, PartialEq, Eq)]
pub struct CanonEntry {
    pub id: String,
    pub concept: String,
    pub what_it_does: String,
    pub why_is_this_bad: String,
    pub example: String,
    pub use_instead: String,
    pub refs: Vec<String>,
    pub source_rule_ids: Vec<String>,
}

#[derive(Debug, Clone, serde::Deserialize)]
struct CanonFile {
    entries: Vec<CanonEntry>,
}

pub fn load_canon(pack_dir: &Path) -> Result<Vec<CanonEntry>, String> {
    let path = pack_dir.join("canon.json");
    let content = std::fs::read_to_string(&path)
        .map_err(|e| format!("Failed to read {}: {}", path.display(), e))?;
    let parsed: CanonFile =
        serde_json::from_str(&content).map_err(|e| format!("Failed to parse canon: {}", e))?;
    Ok(parsed.entries)
}

/// C2/C6 concept binding: `concept` must be a forced-choice slug from the
/// pack taxonomy.
pub fn is_valid_slug(taxonomy: &[TaxonomyConcept], slug: &str) -> bool {
    taxonomy.iter().any(|c| c.slug == slug)
}

pub fn find_canon_for_concept<'a>(
    canon: &'a [CanonEntry],
    concept: &str,
) -> Option<&'a CanonEntry> {
    canon.iter().find(|c| c.concept == concept)
}

// ---------------------------------------------------------------------
// Payload 4 — judge-prompt fragments (delivered via the opaque slot: the
// engine never parses their internal structure, only loads and prepends
// the whole blob per stage)
// ---------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PromptFragments {
    pub stage1: String,
    pub stage2: String,
}

pub fn load_prompt_fragments(pack_dir: &Path) -> Result<PromptFragments, String> {
    let stage1_path = pack_dir.join("prompts/stage1.md");
    let stage2_path = pack_dir.join("prompts/stage2.md");
    let stage1 = std::fs::read_to_string(&stage1_path)
        .map_err(|e| format!("Failed to read {}: {}", stage1_path.display(), e))?;
    let stage2 = std::fs::read_to_string(&stage2_path)
        .map_err(|e| format!("Failed to read {}: {}", stage2_path.display(), e))?;
    Ok(PromptFragments {
        stage1: stage1.trim().to_string(),
        stage2: stage2.trim().to_string(),
    })
}

// ---------------------------------------------------------------------
// Payload 5 — surface trivia
// ---------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SurfaceConfig {
    pub comment_token: String,
    pub check_command: String,
    pub file_extensions: Vec<String>,
    /// T3 req 10: help-seeking phrases, most-specific-first, pack-seeded
    /// (D15's self-declared signal 3 pattern list).
    pub help_patterns: Vec<String>,
    /// T3 req 10: literal on-hold-TODO phrasing layered on top of the
    /// engine's structural issue-ref/version/"when X lands" detectors.
    pub on_hold_patterns: Vec<String>,
    /// T4 req 9 / D17: the mentor-address token that turns a plain comment
    /// into a direct ask, e.g. `murshid:` in `// murshid: how do I avoid
    /// this clone?`. Combined with `comment_token` per pack/per language.
    pub address_token: String,
}

/// The bundled Rust pack's own values, used as the engine's last-resort
/// fallback when the pack files are missing/corrupt (this literal lives in
/// the pack loader, not a generic engine module). MUST stay in lockstep
/// with `packs/rust/surface.toml` — `tests::test_surface_default_matches_loaded_pack`
/// fails loudly if they drift (T6 review defect: a missing/corrupt
/// surface.toml used to silently kill T3 signal-3 help detection by
/// falling back to empty pattern lists).
impl Default for SurfaceConfig {
    fn default() -> Self {
        Self {
            comment_token: "//".to_string(),
            check_command: "cargo check".to_string(),
            file_extensions: vec!["rs".to_string()],
            help_patterns: vec![
                "doesn't handle".to_string(),
                "doesn't work".to_string(),
                "why does".to_string(),
                "how does".to_string(),
                "how do".to_string(),
                "not sure".to_string(),
                "stuck".to_string(),
            ],
            on_hold_patterns: vec![
                "when it lands".to_string(),
                "when it ships".to_string(),
                "when this lands".to_string(),
                "when this ships".to_string(),
                "once fixed".to_string(),
                "once merged".to_string(),
            ],
            address_token: "murshid:".to_string(),
        }
    }
}

pub fn load_surface(pack_dir: &Path) -> Result<SurfaceConfig, String> {
    let path = pack_dir.join("surface.toml");
    let content = std::fs::read_to_string(&path)
        .map_err(|e| format!("Failed to read {}: {}", path.display(), e))?;
    let sections = crate::config::parse_toml(&content);
    let top = sections.get("").cloned().unwrap_or_default();

    let comment_token = top
        .get("comment_token")
        .map(|v| v.trim_matches('"').to_string())
        .ok_or_else(|| "surface.toml missing comment_token".to_string())?;
    let check_command = top
        .get("check_command")
        .map(|v| v.trim_matches('"').to_string())
        .ok_or_else(|| "surface.toml missing check_command".to_string())?;
    let file_extensions = top
        .get("file_extensions")
        .map(|v| {
            v.trim_matches(|c| c == '[' || c == ']')
                .split(',')
                .map(|s| s.trim().trim_matches('"').to_string())
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
        })
        .ok_or_else(|| "surface.toml missing file_extensions".to_string())?;

    let parse_pattern_array = |key: &str| -> Vec<String> {
        top.get(key)
            .map(|v| {
                v.trim_matches(|c| c == '[' || c == ']')
                    .split(',')
                    .map(|s| s.trim().trim_matches('"').to_string())
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    };

    // T4 req 9: optional — falls back to the onboarding-taught default
    // `murshid:` when a pack doesn't override it.
    let address_token = top
        .get("address_token")
        .map(|v| v.trim_matches('"').to_string())
        .unwrap_or_else(|| "murshid:".to_string());

    Ok(SurfaceConfig {
        comment_token,
        check_command,
        file_extensions,
        // T3 req 10: optional — packs that don't seed these fall back to
        // an empty list (no pack-specific literal phrases; the engine's
        // structural on-hold detectors and question-mark check still work).
        help_patterns: parse_pattern_array("help_patterns"),
        on_hold_patterns: parse_pattern_array("on_hold_patterns"),
        address_token,
    })
}

// ---------------------------------------------------------------------
// Payload 6 — grammar reference (C9: registration is pack DATA; the small
// match in resolve_ts_language does not count against I29)
// ---------------------------------------------------------------------

/// One tree-sitter node kind the engine treats as a C2 "enclosing item"
/// (fn/struct/impl/mod for Rust) — pack data, per T6 scope item 3.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize, PartialEq, Eq)]
pub struct ItemKindDef {
    /// The tree-sitter node kind string (e.g. `"function_item"`).
    pub kind: String,
    /// The human-readable prefix used to render the item's name
    /// (e.g. `"fn"` -> `"fn foo"`).
    pub label: String,
    /// Field name holding the item's plain name (fn/struct/mod-shaped
    /// items).
    #[serde(default)]
    pub name_field: Option<String>,
    /// Field name holding the implementing type (impl-shaped items).
    #[serde(default)]
    pub type_field: Option<String>,
    /// Field name holding the trait being implemented, if any
    /// (impl-shaped items; optional even when `type_field` is set).
    #[serde(default)]
    pub trait_field: Option<String>,
}

#[derive(Debug, Clone, serde::Deserialize)]
struct GrammarFile {
    #[serde(default)]
    #[allow(dead_code)]
    grammar_crate: String,
    #[serde(default)]
    #[allow(dead_code)]
    grammar_version: String,
    item_kinds: Vec<ItemKindDef>,
    container_kinds: Vec<String>,
}

/// The pack's parsed grammar reference: node-kind vocabulary (site.rs's
/// former Rust-only `ITEM_KINDS`/`CONTAINER_KINDS`, now pack data) plus the
/// resolved compiled-in tree-sitter `Language` (registered below).
#[derive(Clone)]
pub struct GrammarSpec {
    pub language_id: String,
    pub item_kinds: Vec<ItemKindDef>,
    pub container_kinds: Vec<String>,
    pub ts_language: tree_sitter::Language,
}

/// C9 payload 6: the pack's directory name maps to its compiled-in
/// tree-sitter grammar crate here. Per-language grammar crates stay
/// compile-time linked (D23's rejection of full dynamic plugins) — this
/// match is the pack registry's job, not an engine edit (I29).
fn resolve_ts_language(language_id: &str) -> Result<tree_sitter::Language, String> {
    match language_id {
        "rust" => Ok(tree_sitter_rust::LANGUAGE.into()),
        "go" => Ok(tree_sitter_go::LANGUAGE.into()),
        other => Err(format!(
            "no compiled-in tree-sitter grammar registered for pack '{}'",
            other
        )),
    }
}

pub fn load_grammar(pack_dir: &Path) -> Result<GrammarSpec, String> {
    let path = pack_dir.join("grammar.json");
    let content = std::fs::read_to_string(&path)
        .map_err(|e| format!("Failed to read {}: {}", path.display(), e))?;
    let parsed: GrammarFile =
        serde_json::from_str(&content).map_err(|e| format!("Failed to parse grammar: {}", e))?;
    let language_id = language_id_from_pack_dir(pack_dir);
    let ts_language = resolve_ts_language(&language_id)?;
    Ok(GrammarSpec {
        language_id,
        item_kinds: parsed.item_kinds,
        container_kinds: parsed.container_kinds,
        ts_language,
    })
}

/// The bundled Rust pack's own grammar reference, used as the engine's
/// last-resort fallback (mirrors `SurfaceConfig::default`) — this literal
/// duplication of `packs/rust/grammar.json` lives in the pack loader, not a
/// generic engine module.
impl Default for GrammarSpec {
    fn default() -> Self {
        Self {
            language_id: "rust".to_string(),
            item_kinds: vec![
                ItemKindDef {
                    kind: "function_item".to_string(),
                    label: "fn".to_string(),
                    name_field: Some("name".to_string()),
                    type_field: None,
                    trait_field: None,
                },
                ItemKindDef {
                    kind: "struct_item".to_string(),
                    label: "struct".to_string(),
                    name_field: Some("name".to_string()),
                    type_field: None,
                    trait_field: None,
                },
                ItemKindDef {
                    kind: "mod_item".to_string(),
                    label: "mod".to_string(),
                    name_field: Some("name".to_string()),
                    type_field: None,
                    trait_field: None,
                },
                ItemKindDef {
                    kind: "impl_item".to_string(),
                    label: "impl".to_string(),
                    name_field: None,
                    type_field: Some("type".to_string()),
                    trait_field: Some("trait".to_string()),
                },
            ],
            container_kinds: vec![
                "block".to_string(),
                "field_declaration_list".to_string(),
                "declaration_list".to_string(),
                "source_file".to_string(),
                "match_block".to_string(),
                "enum_variant_list".to_string(),
            ],
            ts_language: tree_sitter_rust::LANGUAGE.into(),
        }
    }
}

// ---------------------------------------------------------------------
// Payload 1 — diagnostics adapter (I27's "one narrow adapter"; I28's
// normalized record amended by C9)
// ---------------------------------------------------------------------

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct FileRange {
    pub line_start: usize,
    pub line_end: usize,
    pub column_start: usize,
    pub column_end: usize,
}

/// I28's normalized record, as amended by C9: `severity` renamed
/// `tool_level` (tool-native input; the engine's severity axis IS the
/// category enum), and `data` is the LSP-style opaque escape hatch relayed
/// to the judge unparsed.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct NormalizedRecord {
    /// Namespaced per I28 (e.g. `rust/E0425`, `rust/clippy::needless_collect`).
    pub rule_id: String,
    pub source_tool: String,
    pub tool_level: String,
    pub message: String,
    pub file: String,
    pub range: FileRange,
    #[serde(default)]
    pub suggested_fix: Option<String>,
    #[serde(default)]
    pub doc_ref: Option<String>,
    /// SARIF-style finding-fingerprint (I28/C2): pack-supplied identity for
    /// this finding across runs. NOT the engine's advice-fingerprint (C2's
    /// `(concept_id, site)`, computed by `site::advice_fingerprint`).
    pub fingerprint: String,
    #[serde(default)]
    pub data: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AdapterCheckOutput {
    pub success: bool,
    pub is_infra_error: bool,
    pub records: Vec<NormalizedRecord>,
}

/// I27: the one piece of code a pack contributes — tool invocation plus
/// native-output-to-[`NormalizedRecord`] mapping. Everything else about a
/// pack is declarative data.
pub trait DiagnosticsAdapter {
    fn run_check(
        &self,
        project_root: &Path,
        active_file: &Path,
    ) -> Result<AdapterCheckOutput, String>;
}

/// Resolves the adapter for `pack_dir`. This match is the pack registry's
/// job (I29): adding a language adds a match arm here plus its own adapter
/// module, never edits to `site.rs`/`quiescence.rs`/`pipeline.rs`/etc.
pub fn diagnostics_adapter(pack_dir: &Path) -> Result<Box<dyn DiagnosticsAdapter>, String> {
    let language_id = language_id_from_pack_dir(pack_dir);
    match language_id.as_str() {
        "rust" => Ok(Box::new(compiler::CompilerInterceptor::new())),
        "go" => Ok(Box::new(go_adapter::GoVetInterceptor::new())),
        other => Err(format!(
            "no diagnostics adapter registered for pack '{}'",
            other
        )),
    }
}

// ---------------------------------------------------------------------
// C6 degraded-mode notice for pack-load failures (T6 review defect 3):
// "no pack resolved" must say why, never silently assume a language.
// ---------------------------------------------------------------------

/// Builds the one-line notice printed whenever a pack payload fails to
/// load and the caller falls back to a built-in default — mirrors
/// `judge::degraded_status_line`'s "state why, never silently degrade"
/// shape, applied to pack resolution instead of the model seam.
pub fn payload_fallback_notice(payload_name: &str, pack_dir: &Path, error: &str) -> String {
    format!(
        "[murshid] degraded: pack payload '{}' failed to load from {} ({}) — using a built-in fallback.",
        payload_name,
        pack_dir.display(),
        error
    )
}

/// Unwraps a pack payload load `result`, printing [`payload_fallback_notice`]
/// and returning `T::default()` on failure instead of silently degrading.
/// Every `load_*(pack_dir).unwrap_or_default()` call site in the engine
/// should route through this instead (T6 review gating fix 1).
pub fn load_or_notice<T: Default>(
    result: Result<T, String>,
    payload_name: &str,
    pack_dir: &Path,
) -> T {
    match result {
        Ok(v) => v,
        Err(e) => {
            println!("{}", payload_fallback_notice(payload_name, pack_dir, &e));
            T::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_load_taxonomy_has_ten_seed_concepts() {
        let taxonomy = load_taxonomy(&default_pack_dir()).unwrap();
        assert_eq!(taxonomy.len(), 10);

        let expected_slugs = [
            "option-combinators",
            "question-mark-propagation",
            "iterator-chains",
            "borrow-vs-clone",
            "string-vs-str",
            "match-ergonomics",
            "derive-traits",
            "collect-annotations",
            "if-let-patterns",
            "lifetime-elision",
        ];
        for slug in expected_slugs {
            assert!(
                taxonomy.iter().any(|c| c.slug == slug),
                "missing taxonomy slug: {}",
                slug
            );
        }

        let valid_categories = ["bug", "idiom", "best-practice", "architecture"];
        for concept in &taxonomy {
            assert!(
                valid_categories.contains(&concept.category.as_str()),
                "invalid category for {}: {}",
                concept.slug,
                concept.category
            );
        }
    }

    #[test]
    fn test_load_canon_one_entry_per_concept() {
        let taxonomy = load_taxonomy(&default_pack_dir()).unwrap();
        let canon = load_canon(&default_pack_dir()).unwrap();
        assert_eq!(canon.len(), taxonomy.len());
        for concept in &taxonomy {
            assert!(
                find_canon_for_concept(&canon, &concept.slug).is_some(),
                "missing canon entry for {}",
                concept.slug
            );
        }
    }

    #[test]
    fn test_load_surface() {
        let surface = load_surface(&default_pack_dir()).unwrap();
        assert_eq!(surface.comment_token, "//");
        assert_eq!(surface.check_command, "cargo check");
        assert_eq!(surface.file_extensions, vec!["rs".to_string()]);
    }

    /// T4 req 9: the Rust pack seeds its D17 mentor-address token.
    #[test]
    fn test_load_surface_address_token() {
        let surface = load_surface(&default_pack_dir()).unwrap();
        assert_eq!(surface.address_token, "murshid:");
    }

    /// T4 acceptance: a synthetic pack with a non-`//` comment token and a
    /// custom address token still round-trips correctly (pack-agnosticism).
    #[test]
    fn test_load_surface_synthetic_pack_non_slash_slash_token() {
        let temp_dir = std::env::temp_dir().join("murshid_test_synthetic_pack_surface");
        std::fs::create_dir_all(&temp_dir).unwrap();
        std::fs::write(
            temp_dir.join("surface.toml"),
            "comment_token = \"#\"\ncheck_command = \"go vet ./...\"\nfile_extensions = [\"go\"]\naddress_token = \"mentor:\"\n",
        )
        .unwrap();
        let surface = load_surface(&temp_dir).unwrap();
        assert_eq!(surface.comment_token, "#");
        assert_eq!(surface.address_token, "mentor:");
        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    /// T3 req 10: the Rust pack seeds the help/on-hold pattern lists used by
    /// the signal-3 help-comment matcher.
    #[test]
    fn test_load_surface_help_and_on_hold_patterns() {
        let surface = load_surface(&default_pack_dir()).unwrap();
        assert!(!surface.help_patterns.is_empty());
        assert!(surface.help_patterns.contains(&"stuck".to_string()));
        assert!(!surface.on_hold_patterns.is_empty());
        assert!(
            surface
                .on_hold_patterns
                .contains(&"once merged".to_string())
        );
    }

    /// T6 review (gating defect 1): `SurfaceConfig::default()` must stay in
    /// lockstep with the loaded Rust pack's `surface.toml` — a
    /// missing/corrupt pack file used to silently fall back to EMPTY
    /// help/on-hold pattern lists, killing T3 signal-3 detection with no
    /// visible symptom. Mirrors `test_grammar_default_matches_loaded_pack`.
    #[test]
    fn test_surface_default_matches_loaded_pack() {
        let loaded = load_surface(&default_pack_dir()).unwrap();
        assert_eq!(loaded, SurfaceConfig::default());
    }

    #[test]
    fn test_is_valid_slug() {
        let taxonomy = load_taxonomy(&default_pack_dir()).unwrap();
        assert!(is_valid_slug(&taxonomy, "borrow-vs-clone"));
        assert!(!is_valid_slug(&taxonomy, "not-a-real-concept"));
    }

    // --- T6: payload 4 (prompt fragments) ---

    #[test]
    fn test_load_prompt_fragments_are_non_empty_and_distinct() {
        let prompts = load_prompt_fragments(&default_pack_dir()).unwrap();
        assert!(!prompts.stage1.is_empty());
        assert!(!prompts.stage2.is_empty());
        assert_ne!(prompts.stage1, prompts.stage2);
    }

    // --- T6: payload 6 (grammar reference) ---

    #[test]
    fn test_load_grammar_matches_default_fallback_vocabulary() {
        let grammar = load_grammar(&default_pack_dir()).unwrap();
        assert_eq!(grammar.language_id, "rust");
        let kinds: Vec<&str> = grammar.item_kinds.iter().map(|d| d.kind.as_str()).collect();
        assert!(kinds.contains(&"function_item"));
        assert!(kinds.contains(&"impl_item"));
        assert!(grammar.container_kinds.contains(&"block".to_string()));
    }

    #[test]
    fn test_grammar_default_matches_loaded_pack() {
        let loaded = load_grammar(&default_pack_dir()).unwrap();
        let default = GrammarSpec::default();
        assert_eq!(loaded.item_kinds, default.item_kinds);
        assert_eq!(loaded.container_kinds, default.container_kinds);
    }

    #[test]
    fn test_unregistered_language_id_is_an_error() {
        // T7: "go" graduated to a real registered pack, so the placeholder
        // for "a pack that doesn't exist yet" moved to another still-
        // unregistered language id — the test's assertions are unchanged.
        let temp_dir = std::env::temp_dir().join("murshid_test_pack_registry_unknown_lang");
        let _ = std::fs::remove_dir_all(&temp_dir);
        let pack_dir = temp_dir.join("python");
        std::fs::create_dir_all(&pack_dir).unwrap();
        std::fs::write(
            pack_dir.join("grammar.json"),
            r#"{"item_kinds": [], "container_kinds": []}"#,
        )
        .unwrap();
        assert!(load_grammar(&pack_dir).is_err());
        assert!(diagnostics_adapter(&pack_dir).is_err());
        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    // --- T6: diagnostics adapter registry ---

    #[test]
    fn test_diagnostics_adapter_resolves_for_rust_pack() {
        assert!(diagnostics_adapter(&default_pack_dir()).is_ok());
    }

    // --- T6 req 4: pack-path resolution for installed binaries ---

    // T9 req 9 addendum: these precedence tests go through
    // `resolve_packs_dir_from` with an explicit override instead of mutating
    // the process-global MURSHID_PACKS_DIR — a mutating test raced concurrent
    // pack-loading readers onto its synthetic dir (observed twice: the
    // surface lockstep test read empty help_patterns). No env, no locks.

    #[test]
    fn test_resolve_packs_dir_env_var_takes_precedence() {
        let temp_dir = std::env::temp_dir().join("murshid_test_packs_dir_env_override");
        let override_val = temp_dir.to_string_lossy().into_owned();
        assert_eq!(resolve_packs_dir_from(Some(override_val)), temp_dir);
        // Empty override is ignored, same as an empty env var.
        assert_ne!(resolve_packs_dir_from(Some(String::new())), temp_dir);
    }

    #[test]
    fn test_resolve_packs_dir_falls_back_to_manifest_dir_in_dev() {
        let resolved = resolve_packs_dir_from(None);
        assert!(
            resolved.ends_with("packs"),
            "dev fallback should resolve to the checkout's packs/ dir, got {}",
            resolved.display()
        );
    }

    /// Acceptance: a pack loads correctly from a simulated INSTALLED layout
    /// (a temp dir standing in for `<prefix>/share/murshid/packs`, resolved
    /// through the same override leg an installed binary's
    /// `MURSHID_PACKS_DIR` would take) — not the dev CARGO_MANIFEST_DIR
    /// fallback.
    #[test]
    fn test_pack_loads_from_simulated_installed_layout() {
        let installed_root =
            std::env::temp_dir().join("murshid_test_simulated_install/share/murshid/packs");
        let rust_pack_dir = installed_root.join("rust");
        let _ = std::fs::remove_dir_all(&installed_root);
        std::fs::create_dir_all(rust_pack_dir.join("prompts")).unwrap();

        std::fs::write(
            rust_pack_dir.join("taxonomy.json"),
            r#"{"concepts": [{"slug": "s", "name": "S", "category": "idiom"}]}"#,
        )
        .unwrap();
        std::fs::write(
            rust_pack_dir.join("canon.json"),
            r#"{"entries": [{"id": "c", "concept": "s", "what_it_does": "", "why_is_this_bad": "", "example": "", "use_instead": "", "refs": [], "source_rule_ids": []}]}"#,
        )
        .unwrap();
        std::fs::write(
            rust_pack_dir.join("surface.toml"),
            "comment_token = \"//\"\ncheck_command = \"cargo check\"\nfile_extensions = [\"rs\"]\n",
        )
        .unwrap();
        std::fs::write(
            rust_pack_dir.join("grammar.json"),
            r#"{"item_kinds": [], "container_kinds": []}"#,
        )
        .unwrap();
        std::fs::write(rust_pack_dir.join("prompts/stage1.md"), "screen framing").unwrap();
        std::fs::write(rust_pack_dir.join("prompts/stage2.md"), "judge framing").unwrap();

        let resolved_rust_dir =
            resolve_packs_dir_from(Some(installed_root.to_string_lossy().into_owned()))
                .join("rust");
        assert_eq!(resolved_rust_dir, rust_pack_dir);

        assert_eq!(load_taxonomy(&resolved_rust_dir).unwrap().len(), 1);
        assert_eq!(load_canon(&resolved_rust_dir).unwrap().len(), 1);
        assert_eq!(
            load_surface(&resolved_rust_dir).unwrap().comment_token,
            "//"
        );
        assert_eq!(
            load_grammar(&resolved_rust_dir).unwrap().language_id,
            "rust"
        );
        assert!(diagnostics_adapter(&resolved_rust_dir).is_ok());
        let prompts = load_prompt_fragments(&resolved_rust_dir).unwrap();
        assert_eq!(prompts.stage1, "screen framing");
        assert_eq!(prompts.stage2, "judge framing");

        let _ = std::fs::remove_dir_all(&installed_root);
    }

    // --- T6 review gating fix 1(ii): degraded-mode notice on payload fallback ---

    #[test]
    fn test_load_or_notice_passes_through_ok_value() {
        let result: Result<Vec<TaxonomyConcept>, String> = Ok(vec![TaxonomyConcept {
            slug: "s".to_string(),
            name: "S".to_string(),
            category: "idiom".to_string(),
        }]);
        let value = load_or_notice(result, "taxonomy", &default_pack_dir());
        assert_eq!(value.len(), 1);
    }

    #[test]
    fn test_load_or_notice_falls_back_to_default_on_error() {
        let result: Result<Vec<TaxonomyConcept>, String> = Err("missing file".to_string());
        let value = load_or_notice(result, "taxonomy", &default_pack_dir());
        assert!(value.is_empty());
    }

    /// The printed notice must name the failing payload and carry the
    /// underlying error — "no pack resolved" must say why, never silently
    /// assume a language (T6 review defect 3).
    #[test]
    fn test_payload_fallback_notice_names_payload_and_error() {
        let notice =
            payload_fallback_notice("taxonomy", &default_pack_dir(), "No such file or directory");
        assert!(notice.contains("taxonomy"));
        assert!(notice.contains("No such file or directory"));
        assert!(notice.contains("degraded"));
    }

    // --- T10: pack auto-detection ---

    fn make_temp_project_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("murshid_test_t10_{}", name));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    // req 6: detection matrix over the pure fn.

    #[test]
    fn test_detect_pack_marker_go_only() {
        let entries = vec!["go.mod".to_string(), "main.go".to_string()];
        assert_eq!(detect_pack_marker(&entries), MarkerDetection::GoOnly);
    }

    #[test]
    fn test_detect_pack_marker_rust_only() {
        let entries = vec!["Cargo.toml".to_string(), "src".to_string()];
        assert_eq!(detect_pack_marker(&entries), MarkerDetection::RustOnly);
    }

    #[test]
    fn test_detect_pack_marker_both() {
        let entries = vec!["go.mod".to_string(), "Cargo.toml".to_string()];
        assert_eq!(detect_pack_marker(&entries), MarkerDetection::Both);
    }

    #[test]
    fn test_detect_pack_marker_neither() {
        let entries = vec!["README.md".to_string(), "src".to_string()];
        assert_eq!(detect_pack_marker(&entries), MarkerDetection::Neither);
    }

    // req 3: the ambiguity notice names both the choice and the override key.

    #[test]
    fn test_ambiguous_marker_notice_names_choice_and_override_key() {
        let notice = ambiguous_marker_notice();
        assert!(notice.contains("rust"));
        assert!(notice.contains("go.mod"));
        assert!(notice.contains("Cargo.toml"));
        assert!(notice.contains("[pack] language"));
    }

    // req 1/2: resolve_pack_id precedence — config override wins over markers.

    #[test]
    fn test_resolve_pack_id_config_override_wins_over_go_marker() {
        let dir = make_temp_project_dir("config_override");
        std::fs::write(dir.join("go.mod"), "module example\n").unwrap();
        let mut config = crate::config::AppConfig::default();
        config.pack.language = Some("rust".to_string());
        assert_eq!(resolve_pack_id(&dir, &config), "rust");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_resolve_pack_id_detects_go_marker_with_no_config() {
        let dir = make_temp_project_dir("go_marker");
        std::fs::write(dir.join("go.mod"), "module example\n").unwrap();
        let config = crate::config::AppConfig::default();
        assert_eq!(resolve_pack_id(&dir, &config), "go");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_resolve_pack_id_detects_rust_marker_with_no_config() {
        let dir = make_temp_project_dir("rust_marker");
        std::fs::write(dir.join("Cargo.toml"), "[package]\n").unwrap();
        let config = crate::config::AppConfig::default();
        assert_eq!(resolve_pack_id(&dir, &config), "rust");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_resolve_pack_id_both_markers_falls_back_to_rust() {
        let dir = make_temp_project_dir("both_markers");
        std::fs::write(dir.join("go.mod"), "module example\n").unwrap();
        std::fs::write(dir.join("Cargo.toml"), "[package]\n").unwrap();
        let config = crate::config::AppConfig::default();
        assert_eq!(resolve_pack_id(&dir, &config), "rust");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_resolve_pack_id_neither_marker_falls_back_to_rust_silently() {
        let dir = make_temp_project_dir("neither_marker");
        let config = crate::config::AppConfig::default();
        assert_eq!(resolve_pack_id(&dir, &config), "rust");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A locked `[pack]` section (T10 precedence (a)) still wins over a
    /// marker at the project root — locking only stops further config
    /// merges from changing the value, it doesn't disable the override.
    #[test]
    fn test_resolve_pack_id_locked_config_still_wins_over_markers() {
        use std::collections::HashSet;
        let dir = make_temp_project_dir("locked_config");
        std::fs::write(dir.join("go.mod"), "module example\n").unwrap();

        let mut config = crate::config::AppConfig::default();
        let mut locked = HashSet::new();
        let system_toml =
            crate::config::parse_toml("[pack]\nlanguage = \"rust\"\nlock_policy = true\n");
        config.merge_toml(&system_toml, true, &mut locked);
        // A project attempt to override is ignored (locked).
        let project_toml = crate::config::parse_toml("[pack]\nlanguage = \"go\"\n");
        config.merge_toml(&project_toml, false, &mut locked);

        assert_eq!(resolve_pack_id(&dir, &config), "rust");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // req 4: missing packs/<id>/ dir falls back to rust, no crash.

    #[test]
    fn test_resolve_pack_dir_falls_back_to_rust_for_bogus_configured_id() {
        let dir = make_temp_project_dir("bogus_pack_id");
        let mut config = crate::config::AppConfig::default();
        config.pack.language = Some("not-a-real-pack".to_string());
        let resolved = resolve_pack_dir(&dir, &config);
        assert_eq!(resolved, default_pack_dir());
        assert!(resolved.is_dir());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_resolve_pack_dir_resolves_installed_go_pack() {
        let dir = make_temp_project_dir("go_pack_installed");
        let mut config = crate::config::AppConfig::default();
        config.pack.language = Some("go".to_string());
        let resolved = resolve_pack_dir(&dir, &config);
        assert!(resolved.ends_with("go"));
        assert!(resolved.is_dir());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_resolve_pack_dir_resolves_rust_by_default() {
        let dir = make_temp_project_dir("rust_pack_default");
        let config = crate::config::AppConfig::default();
        let resolved = resolve_pack_dir(&dir, &config);
        assert_eq!(resolved, default_pack_dir());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
