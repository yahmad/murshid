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
//! data directory, a new adapter implementing [`DiagnosticsAdapter`], and one
//! new [`PackRegistration`] entry in [`PACK_REGISTRY`] — the single table both
//! grammar and adapter resolution read, so the two can never disagree about
//! which ids are registered. Nothing outside this file changes.

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
// Shared adapter subprocess watchdog (Reliability item 1)
// ---------------------------------------------------------------------

/// Wall-clock ceiling on a single diagnostics-adapter check subprocess
/// (`cargo check`, `go vet`). Sweeps run on ONE quiescence worker thread, so a
/// wedged toolchain that never exits would otherwise block every future sweep
/// indefinitely. Generous enough for a legitimate cold `cargo check` on a large
/// crate; well short of "hung forever". Mirrors curl's `max-time` bound on the
/// provider side.
pub(crate) const ADAPTER_CHECK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// SIGTERMs a child, waits up to 500ms, then SIGKILLs if still alive. Shared by
/// both adapters (the Rust adapter also uses it to preempt a previous run) and
/// by [`wait_with_output_timeout`]'s watchdog path.
#[cfg(unix)]
pub(crate) fn terminate_process(child: &mut std::process::Child) {
    let pid = child.id();
    // Send SIGTERM (15).
    unsafe {
        let _ = libc::kill(pid as libc::pid_t, 15);
    }

    // Wait up to 500ms for a graceful exit.
    let start = std::time::Instant::now();
    while start.elapsed().as_millis() < 500 {
        match child.try_wait() {
            Ok(Some(_)) => return, // Exited.
            _ => std::thread::sleep(std::time::Duration::from_millis(50)),
        }
    }

    // Still running — SIGKILL (9) and reap.
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(not(unix))]
pub(crate) fn terminate_process(child: &mut std::process::Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// Like [`std::process::Child::wait_with_output`], but bounded by `timeout`. On
/// timeout the child is terminated via [`terminate_process`] and `Ok(None)` is
/// returned; the caller maps that to a TIMEOUT infra error rather than hanging
/// the sweep worker. stdout/stderr are drained on reader threads so a child
/// that fills a pipe buffer can never deadlock the wait; the reader threads
/// unblock on EOF once the child is killed.
pub(crate) fn wait_with_output_timeout(
    mut child: std::process::Child,
    timeout: std::time::Duration,
) -> std::io::Result<Option<std::process::Output>> {
    use std::io::Read;

    let mut stdout_pipe = child.stdout.take();
    let mut stderr_pipe = child.stderr.take();
    let stdout_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(pipe) = stdout_pipe.as_mut() {
            let _ = pipe.read_to_end(&mut buf);
        }
        buf
    });
    let stderr_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(pipe) = stderr_pipe.as_mut() {
            let _ = pipe.read_to_end(&mut buf);
        }
        buf
    });

    let start = std::time::Instant::now();
    let status = loop {
        match child.try_wait()? {
            Some(status) => break Some(status),
            None => {
                if start.elapsed() >= timeout {
                    terminate_process(&mut child);
                    break None;
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        }
    };

    let stdout = stdout_reader.join().unwrap_or_default();
    let stderr = stderr_reader.join().unwrap_or_default();

    Ok(status.map(|status| std::process::Output {
        status,
        stdout,
        stderr,
    }))
}

// ---------------------------------------------------------------------
// Pack registry (I29/C9) — the single source of truth both the grammar
// (payload 6) and adapter (payload 1) resolutions read
// ---------------------------------------------------------------------

/// One pack's compiled-in *code* registration: the two constructors only a
/// pack's own crates can supply — its tree-sitter grammar and its diagnostics
/// adapter. Everything else about a pack is declarative data read off disk
/// (taxonomy, canon, surface, prompts, grammar vocabulary); these two are the
/// "one piece of code per pack" (I27/C9). Keyed by
/// [`language_id`](Self::language_id), a pack's `packs/<id>/` directory name.
struct PackRegistration {
    /// The pack's language id — its `packs/<id>/` directory name.
    language_id: &'static str,
    /// Constructs the pack's compiled-in tree-sitter `Language`.
    grammar: fn() -> tree_sitter::Language,
    /// Constructs the pack's diagnostics adapter.
    adapter: fn() -> Box<dyn DiagnosticsAdapter>,
}

/// The pack registry (I29/C9): the ONE table mapping a pack's language id to
/// its compiled-in code. Per-language grammar crates and adapters stay
/// compile-time linked (D23's rejection of full dynamic plugins). Both
/// [`resolve_ts_language`] and [`diagnostics_adapter`] resolve through this
/// slice, so the grammar and adapter sides can no longer disagree about which
/// ids are registered — they read the same source of truth. Adding a language
/// is a single entry here (plus its `packs/<lang>/` data dir and its adapter
/// module).
static PACK_REGISTRY: &[PackRegistration] = &[
    PackRegistration {
        language_id: "rust",
        grammar: || tree_sitter_rust::LANGUAGE.into(),
        adapter: || Box::new(compiler::CompilerInterceptor::new()),
    },
    PackRegistration {
        language_id: "go",
        grammar: || tree_sitter_go::LANGUAGE.into(),
        adapter: || Box::new(go_adapter::GoVetInterceptor::new()),
    },
];

/// Looks up the [`PackRegistration`] for `language_id`, or `None` if no pack
/// with that id is compiled in. The shared lookup behind both resolutions.
fn lookup_pack(language_id: &str) -> Option<&'static PackRegistration> {
    PACK_REGISTRY
        .iter()
        .find(|reg| reg.language_id == language_id)
}

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

/// C8's taxonomy category — the closed set of pack-authored concept
/// categories (`bug`/`idiom`/`best-practice`/`architecture`) PLUS an
/// escape hatch: since `category` arrives as pack-authored data (read off
/// `taxonomy.json`, not an engine-internal enum), an unrecognized/extended
/// pack category must degrade gracefully rather than panic — `Other`
/// preserves the exact string so every existing "unknown category" fallback
/// (BKT priors, staleness window, noise floor, queue rank, ...) keeps
/// today's behavior verbatim. Same idiom as `ladder::Rung`/`bkt::Grade`,
/// except `parse` is infallible (there is no reject case, only Other).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Category {
    Bug,
    Idiom,
    BestPractice,
    Architecture,
    Other(String),
}

impl Category {
    pub fn as_str(&self) -> &str {
        match self {
            Category::Bug => "bug",
            Category::Idiom => "idiom",
            Category::BestPractice => "best-practice",
            Category::Architecture => "architecture",
            Category::Other(s) => s.as_str(),
        }
    }

    /// Infallible: an unrecognized category is pack data, never a reject —
    /// it becomes `Other(s)`, carrying the original string through so the
    /// on-disk/JSON round-trip is lossless.
    pub fn parse(s: &str) -> Category {
        match s {
            "bug" => Category::Bug,
            "idiom" => Category::Idiom,
            "best-practice" => Category::BestPractice,
            "architecture" => Category::Architecture,
            other => Category::Other(other.to_string()),
        }
    }
}

/// Serializes as the plain category string (e.g. `"bug"`, or the raw
/// `Other` string) — the taxonomy JSON shape is unchanged by this type.
impl serde::Serialize for Category {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> serde::Deserialize<'de> for Category {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Ok(Category::parse(&s))
    }
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize, PartialEq, Eq)]
pub struct TaxonomyConcept {
    pub slug: String,
    pub name: String,
    pub category: Category,
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

/// The four loaded pack payloads the judge pipeline reads together, bundled as
/// borrowed refs so the `(taxonomy, canon, grammar, prompts)` tuple stops
/// recurring through every `judge_hunks`/`judge_and_collect_finding` signature
/// (the cluster the prior pass flagged with `too_many_arguments` TODOs). `Copy`,
/// so passing it costs nothing and it reads like the four values it stands in
/// for. (The watch layer additionally holds `pack_dir`/`surface`; those aren't
/// engine-judge inputs, so they stay out of this bundle.)
#[derive(Clone, Copy)]
pub struct PackData<'a> {
    pub taxonomy: &'a [TaxonomyConcept],
    pub canon: &'a [CanonEntry],
    pub grammar: &'a GrammarSpec,
    pub prompts: &'a PromptFragments,
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

/// The exact `packs/rust/surface.toml` bytes, embedded at compile time so the
/// engine's last-resort fallback ([`SurfaceConfig::default`]) parses the same
/// payload the loader reads from disk — no hand-mirrored literal to drift out
/// of lockstep (T6 review defect: a missing/corrupt surface.toml used to
/// silently kill T3 signal-3 help detection by falling back to empty pattern
/// lists).
const RUST_SURFACE_TOML: &str = include_str!("../packs/rust/surface.toml");

/// The bundled Rust pack's own values, used as the engine's last-resort
/// fallback when the pack files are missing/corrupt (this literal lives in
/// the pack loader, not a generic engine module). Parsed from the embedded
/// [`RUST_SURFACE_TOML`], so it is identical-by-construction to the loaded
/// Rust pack.
impl Default for SurfaceConfig {
    fn default() -> Self {
        parse_surface(RUST_SURFACE_TOML)
            .expect("embedded packs/rust/surface.toml must parse as a SurfaceConfig")
    }
}

pub fn load_surface(pack_dir: &Path) -> Result<SurfaceConfig, String> {
    let path = pack_dir.join("surface.toml");
    let content = std::fs::read_to_string(&path)
        .map_err(|e| format!("Failed to read {}: {}", path.display(), e))?;
    parse_surface(&content)
}

fn parse_surface(content: &str) -> Result<SurfaceConfig, String> {
    let sections = crate::config::parse_toml(content);
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
// Payload 6 — grammar reference (C9: registration is pack DATA; the shared
// PACK_REGISTRY lookup in resolve_ts_language does not count against I29)
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

/// C9 payload 6: resolves the pack's compiled-in tree-sitter grammar through
/// the shared [`PACK_REGISTRY`]. Per-language grammar crates stay compile-time
/// linked (D23's rejection of full dynamic plugins). Reads the same table as
/// [`diagnostics_adapter`], so an id resolves a grammar iff it also resolves
/// an adapter (I29).
fn resolve_ts_language(language_id: &str) -> Result<tree_sitter::Language, String> {
    lookup_pack(language_id)
        .map(|reg| (reg.grammar)())
        .ok_or_else(|| {
            format!(
                "no compiled-in tree-sitter grammar registered for pack '{}'",
                language_id
            )
        })
}

pub fn load_grammar(pack_dir: &Path) -> Result<GrammarSpec, String> {
    let path = pack_dir.join("grammar.json");
    let content = std::fs::read_to_string(&path)
        .map_err(|e| format!("Failed to read {}: {}", path.display(), e))?;
    let language_id = language_id_from_pack_dir(pack_dir);
    parse_grammar(&content, language_id)
}

fn parse_grammar(content: &str, language_id: String) -> Result<GrammarSpec, String> {
    let parsed: GrammarFile =
        serde_json::from_str(content).map_err(|e| format!("Failed to parse grammar: {}", e))?;
    let ts_language = resolve_ts_language(&language_id)?;
    Ok(GrammarSpec {
        language_id,
        item_kinds: parsed.item_kinds,
        container_kinds: parsed.container_kinds,
        ts_language,
    })
}

/// The exact `packs/rust/grammar.json` bytes, embedded at compile time — see
/// [`RUST_SURFACE_TOML`] for the rationale.
const RUST_GRAMMAR_JSON: &str = include_str!("../packs/rust/grammar.json");

/// The bundled Rust pack's own grammar reference, used as the engine's
/// last-resort fallback (mirrors `SurfaceConfig::default`). Parsed from the
/// embedded [`RUST_GRAMMAR_JSON`], so it is identical-by-construction to the
/// loaded Rust pack.
impl Default for GrammarSpec {
    fn default() -> Self {
        parse_grammar(RUST_GRAMMAR_JSON, "rust".to_string())
            .expect("embedded packs/rust/grammar.json must parse as a GrammarSpec")
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

/// SARIF-style finding-fingerprint (I28/C2): a stable identity for a finding
/// across runs, derived from its namespaced rule id and location. This is
/// engine-level identity shared by every diagnostics adapter, so it lives on
/// the pack seam rather than being re-implemented per adapter (it was
/// byte-identical in the rust and go adapters). The field order is part of the
/// on-disk `finding_fp` contract and must not change.
pub fn record_fingerprint(rule_id: &str, file: &str, range: &FileRange) -> String {
    let raw = format!(
        "{}|{}|{}|{}|{}|{}",
        rule_id, file, range.line_start, range.column_start, range.line_end, range.column_end
    );
    crate::sha256::sha256_hex(raw.as_bytes())
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

/// Resolves the adapter for `pack_dir` through the shared [`PACK_REGISTRY`]
/// (I29) — the same table [`resolve_ts_language`] reads, so the two can't
/// disagree about which ids are registered. Adding a language adds one
/// [`PackRegistration`] entry plus its own adapter module, never edits to
/// `site.rs`/`quiescence.rs`/`pipeline.rs`/etc.
pub fn diagnostics_adapter(pack_dir: &Path) -> Result<Box<dyn DiagnosticsAdapter>, String> {
    let language_id = language_id_from_pack_dir(pack_dir);
    lookup_pack(&language_id)
        .map(|reg| (reg.adapter)())
        .ok_or_else(|| {
            format!(
                "no diagnostics adapter registered for pack '{}'",
                language_id
            )
        })
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

    // --- Reliability item 1: adapter subprocess watchdog ---

    #[cfg(unix)]
    #[test]
    fn test_wait_with_output_timeout_kills_a_wedged_child() {
        use std::process::{Command, Stdio};
        use std::time::{Duration, Instant};

        let child = Command::new("sleep")
            .arg("30")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn sleep");

        let start = Instant::now();
        let result = wait_with_output_timeout(child, Duration::from_millis(200)).unwrap();
        // Timed out → None, and it returned promptly (not after the full 30s).
        assert!(result.is_none(), "wedged child must time out to None");
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "watchdog must return near the timeout, not after the child's own duration"
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_wait_with_output_timeout_returns_output_for_fast_child() {
        use std::process::{Command, Stdio};
        use std::time::Duration;

        let child = Command::new("sh")
            .args(["-c", "printf hello"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn sh");

        let output = wait_with_output_timeout(child, Duration::from_secs(10))
            .unwrap()
            .expect("fast child must return Some(output) before the timeout");
        assert!(output.status.success());
        assert_eq!(output.stdout, b"hello");
    }

    // --- de-stringify refactor: Category as_str/parse round-trip ---

    #[test]
    fn test_category_round_trips_through_str() {
        for c in [
            Category::Bug,
            Category::Idiom,
            Category::BestPractice,
            Category::Architecture,
        ] {
            assert_eq!(Category::parse(c.as_str()), c);
        }
    }

    #[test]
    fn test_category_unknown_becomes_other_infallibly() {
        assert_eq!(
            Category::parse("some-future-pack-category"),
            Category::Other("some-future-pack-category".to_string())
        );
        assert_eq!(
            Category::parse("some-future-pack-category").as_str(),
            "some-future-pack-category"
        );
    }

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
                concept.category.as_str()
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

    /// The embedded `packs/rust/{surface.toml,grammar.json}` payloads must
    /// parse — otherwise `SurfaceConfig::default()`/`GrammarSpec::default()`
    /// panic at first use. Replaces the old hand-mirrored-literal lockstep
    /// tests: the defaults now parse the same bytes the loader reads, so
    /// equality is by construction; this only guards the payloads staying
    /// parseable.
    #[test]
    fn test_embedded_rust_pack_defaults_parse() {
        let surface = SurfaceConfig::default();
        assert_eq!(surface.comment_token, "//");
        assert!(!surface.help_patterns.is_empty());
        let grammar = GrammarSpec::default();
        assert_eq!(grammar.language_id, "rust");
        assert!(grammar.item_kinds.iter().any(|d| d.kind == "function_item"));
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

    /// Consolidation invariant (ROADMAP item 11): grammar and adapter
    /// resolution read the same `PACK_REGISTRY`, so they can never disagree
    /// about which ids are registered. Every registered id resolves BOTH a
    /// grammar and an adapter; an unregistered id resolves NEITHER.
    #[test]
    fn test_pack_registry_grammar_and_adapter_stay_in_agreement() {
        let base = std::env::temp_dir().join("murshid_test_pack_registry_agreement");
        let _ = std::fs::remove_dir_all(&base);

        // Every registered id resolves both sides.
        for reg in PACK_REGISTRY {
            assert!(
                resolve_ts_language(reg.language_id).is_ok(),
                "registered id '{}' must resolve a grammar",
                reg.language_id
            );
            // `diagnostics_adapter` keys off the pack dir's basename, so name
            // the temp dir after the id.
            let pack_dir = base.join(reg.language_id);
            std::fs::create_dir_all(&pack_dir).unwrap();
            assert!(
                diagnostics_adapter(&pack_dir).is_ok(),
                "registered id '{}' must resolve an adapter",
                reg.language_id
            );
        }

        // An unregistered id resolves neither side — the shared table can't
        // let one succeed while the other fails.
        assert!(resolve_ts_language("not-a-registered-pack").is_err());
        let unknown_dir = base.join("not-a-registered-pack");
        std::fs::create_dir_all(&unknown_dir).unwrap();
        assert!(diagnostics_adapter(&unknown_dir).is_err());

        let _ = std::fs::remove_dir_all(&base);
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
            category: Category::Idiom,
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

    // -------------------------------------------------------------------
    // T18 R0 — pack validation harness.
    //
    // Runs as ordinary `cargo test`, iterating every entry in
    // PACK_REGISTRY (this module can see it — it's a private item of an
    // ancestor module) and loading each pack's data through the SAME
    // production loaders (`load_taxonomy`/`load_canon`/`load_grammar`)
    // used at runtime, so there is no second parsing path to drift out of
    // sync with production. R0 is observation-only: no pack JSON is
    // touched, no engine behavior changes.
    // -------------------------------------------------------------------

    /// The in-checkout pack directory for `language_id` (`packs/<id>/`),
    /// independent of [`resolve_packs_dir`]'s installed/XDG search order —
    /// the validator always checks the source-of-truth JSON in this
    /// checkout, not whatever pack happens to be installed on the machine
    /// running the tests.
    fn checkout_pack_dir(language_id: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("packs")
            .join(language_id)
    }

    /// Kebab-case slug format (C2/C8: `concept_id` persists forever, so the
    /// charset is deliberately narrow): lowercase ASCII letters, digits,
    /// and single hyphens only; no leading/trailing hyphen, no `--`.
    fn is_kebab_slug(slug: &str) -> bool {
        !slug.is_empty()
            && !slug.starts_with('-')
            && !slug.ends_with('-')
            && !slug.contains("--")
            && slug
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    }

    /// Parses a `packs/<id>/taxonomy.lock` golden file: `#`-comments and
    /// blank lines ignored, one `<slug> <category>` pair per remaining
    /// line.
    fn parse_taxonomy_lock(content: &str) -> Vec<(String, String)> {
        content
            .lines()
            .map(|l| l.trim())
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .map(|l| {
                let mut parts = l.splitn(2, char::is_whitespace);
                let slug = parts.next().unwrap_or("").to_string();
                let category = parts.next().unwrap_or("").trim().to_string();
                (slug, category)
            })
            .collect()
    }

    /// Cross-pack: `PACK_REGISTRY`'s language ids are unique. Guards
    /// against a future copy-paste registration that would make
    /// `lookup_pack` return the wrong entry silently.
    #[test]
    fn test_pack_registry_language_ids_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for reg in PACK_REGISTRY {
            assert!(
                seen.insert(reg.language_id),
                "duplicate language_id '{}' in PACK_REGISTRY",
                reg.language_id
            );
        }
    }

    /// Per pack, per taxonomy concept: slug format + uniqueness, non-empty
    /// name, and category in the closed set C12 defines
    /// (bug/idiom/best-practice/architecture) — no `Other` in a shipped
    /// pack (`Other` is the engine's infallible-parse escape hatch for
    /// unrecognized data, never an authored value).
    #[test]
    fn test_every_registered_pack_taxonomy_is_well_formed() {
        for reg in PACK_REGISTRY {
            let pack_dir = checkout_pack_dir(reg.language_id);
            let taxonomy = load_taxonomy(&pack_dir)
                .unwrap_or_else(|e| panic!("pack '{}' taxonomy: {}", reg.language_id, e));
            assert!(
                !taxonomy.is_empty(),
                "pack '{}' taxonomy has zero concepts",
                reg.language_id
            );

            let mut seen_slugs = std::collections::HashSet::new();
            for concept in &taxonomy {
                assert!(
                    is_kebab_slug(&concept.slug),
                    "pack '{}' slug '{}' is not kebab-case",
                    reg.language_id,
                    concept.slug
                );
                assert!(
                    seen_slugs.insert(concept.slug.clone()),
                    "pack '{}' has a duplicate slug '{}'",
                    reg.language_id,
                    concept.slug
                );
                assert!(
                    !concept.name.trim().is_empty(),
                    "pack '{}' slug '{}' has an empty name",
                    reg.language_id,
                    concept.slug
                );
                assert!(
                    !matches!(concept.category, Category::Other(_)),
                    "pack '{}' slug '{}' has category '{}', outside the closed C12 set \
                     (bug/idiom/best-practice/architecture) — shipped packs may not use \
                     Category::Other",
                    reg.language_id,
                    concept.slug,
                    concept.category.as_str()
                );
            }
        }
    }

    /// Per pack, per canon entry: `concept` resolves to a taxonomy slug
    /// (validated via [`is_valid_slug`], the same forced-choice check the
    /// judge pipeline uses), required prose fields are non-empty (derived
    /// from what `pipeline.rs`/`review.rs`/`thread.rs`/`retrieval.rs`
    /// actually read off `CanonEntry`: `what_it_does`, `why_is_this_bad`,
    /// `use_instead`; plus `id`/`concept`/`example` as the remaining
    /// authored-prose fields on the struct — `refs`/`source_rule_ids` stay
    /// optional, both packs already ship entries with an empty `refs` or
    /// `source_rule_ids`), and `id`s are unique within the pack.
    #[test]
    fn test_every_registered_pack_canon_is_well_formed() {
        for reg in PACK_REGISTRY {
            let pack_dir = checkout_pack_dir(reg.language_id);
            let taxonomy = load_taxonomy(&pack_dir)
                .unwrap_or_else(|e| panic!("pack '{}' taxonomy: {}", reg.language_id, e));
            let canon = load_canon(&pack_dir)
                .unwrap_or_else(|e| panic!("pack '{}' canon: {}", reg.language_id, e));
            assert!(
                !canon.is_empty(),
                "pack '{}' canon has zero entries",
                reg.language_id
            );

            let mut seen_ids = std::collections::HashSet::new();
            for entry in &canon {
                assert!(
                    seen_ids.insert(entry.id.clone()),
                    "pack '{}' has a duplicate canon id '{}'",
                    reg.language_id,
                    entry.id
                );
                assert!(
                    is_valid_slug(&taxonomy, &entry.concept),
                    "pack '{}' canon entry '{}' has concept '{}', which does not resolve to \
                     any taxonomy slug",
                    reg.language_id,
                    entry.id,
                    entry.concept
                );
                for (field_name, value) in [
                    ("id", entry.id.as_str()),
                    ("concept", entry.concept.as_str()),
                    ("what_it_does", entry.what_it_does.as_str()),
                    ("why_is_this_bad", entry.why_is_this_bad.as_str()),
                    ("example", entry.example.as_str()),
                    ("use_instead", entry.use_instead.as_str()),
                ] {
                    assert!(
                        !value.trim().is_empty(),
                        "pack '{}' canon entry '{}' has an empty '{}' field",
                        reg.language_id,
                        entry.id,
                        field_name
                    );
                }
            }
        }
    }

    /// Per pack: `grammar.json` loads through the real
    /// [`load_grammar`]/[`parse_grammar`] path (which also exercises
    /// [`resolve_ts_language`] — the same `PACK_REGISTRY` lookup
    /// `diagnostics_adapter` uses), and the resulting item/container kind
    /// vocabulary is well-formed: non-empty `kind`/`label` strings, no
    /// duplicate item `kind`s, no duplicate/empty `container_kinds`
    /// entries.
    #[test]
    fn test_every_registered_pack_grammar_is_well_formed() {
        for reg in PACK_REGISTRY {
            let pack_dir = checkout_pack_dir(reg.language_id);
            let grammar = load_grammar(&pack_dir)
                .unwrap_or_else(|e| panic!("pack '{}' grammar: {}", reg.language_id, e));
            assert_eq!(grammar.language_id, reg.language_id);
            assert!(
                !grammar.item_kinds.is_empty(),
                "pack '{}' grammar has zero item_kinds",
                reg.language_id
            );
            assert!(
                !grammar.container_kinds.is_empty(),
                "pack '{}' grammar has zero container_kinds",
                reg.language_id
            );

            let mut seen_item_kinds = std::collections::HashSet::new();
            for item in &grammar.item_kinds {
                assert!(
                    !item.kind.trim().is_empty(),
                    "pack '{}' has an item_kind with an empty 'kind'",
                    reg.language_id
                );
                assert!(
                    !item.label.trim().is_empty(),
                    "pack '{}' item_kind '{}' has an empty 'label'",
                    reg.language_id,
                    item.kind
                );
                assert!(
                    seen_item_kinds.insert(item.kind.clone()),
                    "pack '{}' has a duplicate item_kind '{}'",
                    reg.language_id,
                    item.kind
                );
            }

            let mut seen_container_kinds = std::collections::HashSet::new();
            for kind in &grammar.container_kinds {
                assert!(
                    !kind.trim().is_empty(),
                    "pack '{}' has an empty container_kinds entry",
                    reg.language_id
                );
                assert!(
                    seen_container_kinds.insert(kind.clone()),
                    "pack '{}' has a duplicate container_kinds entry '{}'",
                    reg.language_id,
                    kind
                );
            }
        }
    }

    /// The append-only guard (specs/tasks/T18-pack-pipeline.md, "The crux
    /// — slugs are forever"): every slug/category pair recorded in
    /// `packs/<id>/taxonomy.lock` must still be present, byte-identical,
    /// in the live `taxonomy.json`. A slug missing entirely, or present
    /// with a different category, fails loudly — additions (a slug in
    /// taxonomy.json but not yet in the lock) are fine and expected
    /// between enrichment PRs.
    #[test]
    fn test_taxonomy_is_append_only_against_golden_lock() {
        for reg in PACK_REGISTRY {
            let pack_dir = checkout_pack_dir(reg.language_id);
            let taxonomy = load_taxonomy(&pack_dir)
                .unwrap_or_else(|e| panic!("pack '{}' taxonomy: {}", reg.language_id, e));

            let lock_path = pack_dir.join("taxonomy.lock");
            let lock_content = std::fs::read_to_string(&lock_path).unwrap_or_else(|e| {
                panic!(
                    "pack '{}' is missing its append-only golden lock at {}: {} — every \
                     registered pack must have one (T18 R0)",
                    reg.language_id,
                    lock_path.display(),
                    e
                )
            });

            for (slug, category) in parse_taxonomy_lock(&lock_content) {
                let live = taxonomy.iter().find(|c| c.slug == slug);
                match live {
                    None => panic!(
                        "pack '{}': slug '{}' was removed or renamed from taxonomy.json, but \
                         it is locked in {} — taxonomy slugs are FOREVER (they persist as \
                         concept_id in concept_memory/cards/suppressions; see \
                         specs/tasks/T18-pack-pipeline.md, \"The crux — slugs are forever\"). \
                         Add a new slug instead of renaming; never remove a shipped one.",
                        reg.language_id,
                        slug,
                        lock_path.display()
                    ),
                    Some(concept) => assert_eq!(
                        concept.category.as_str(),
                        category,
                        "pack '{}': slug '{}' changed category from '{}' (locked in {}) to \
                         '{}' — re-categorizing a shipped slug is a rename in disguise and is \
                         forbidden by the same append-only rule (\"slugs are forever\", \
                         specs/tasks/T18-pack-pipeline.md). Add a new slug under the new \
                         category instead.",
                        reg.language_id,
                        slug,
                        category,
                        lock_path.display(),
                        concept.category.as_str()
                    ),
                }
            }
        }
    }
}
