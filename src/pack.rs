//! Language-pack data loading (C9, T1 pre-seam).
//!
//! T1 hardcodes the Rust pack's *location* (T6 extracts a real seam later),
//! but the pack's content — taxonomy, canon, surface, prompt framing — lives
//! in data files under `packs/rust/`, never inline in engine .rs files.

use std::path::{Path, PathBuf};

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

/// Default location of the Rust pack in this repo checkout. T6 will replace
/// this with a real discovery seam; T1 hardcodes it per spec (line 17-18).
pub fn default_pack_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("packs/rust")
}

pub fn load_taxonomy(pack_dir: &Path) -> Result<Vec<TaxonomyConcept>, String> {
    let path = pack_dir.join("taxonomy.json");
    let content = std::fs::read_to_string(&path)
        .map_err(|e| format!("Failed to read {}: {}", path.display(), e))?;
    let parsed: TaxonomyFile =
        serde_json::from_str(&content).map_err(|e| format!("Failed to parse taxonomy: {}", e))?;
    Ok(parsed.concepts)
}

pub fn load_canon(pack_dir: &Path) -> Result<Vec<CanonEntry>, String> {
    let path = pack_dir.join("canon.json");
    let content = std::fs::read_to_string(&path)
        .map_err(|e| format!("Failed to read {}: {}", path.display(), e))?;
    let parsed: CanonFile =
        serde_json::from_str(&content).map_err(|e| format!("Failed to parse canon: {}", e))?;
    Ok(parsed.entries)
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
        assert!(surface
            .on_hold_patterns
            .contains(&"once merged".to_string()));
    }

    #[test]
    fn test_is_valid_slug() {
        let taxonomy = load_taxonomy(&default_pack_dir()).unwrap();
        assert!(is_valid_slug(&taxonomy, "borrow-vs-clone"));
        assert!(!is_valid_slug(&taxonomy, "not-a-real-concept"));
    }
}
