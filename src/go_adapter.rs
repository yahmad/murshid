//! The Go pack's diagnostics adapter (T7 payload 1 / I27): shells
//! `go vet ./...` and maps its native output to the engine's
//! [`crate::pack::NormalizedRecord`] shape (I28, amended by C9). This is the
//! ONE piece of code the Go pack contributes — every other Go-specific
//! concern lives in `packs/go/` data files. Together with `pack.rs` (the
//! loader/registry), this module is the seam's other allowed home for
//! "go"/"go vet" literals.
//!
//! Unlike `cargo check --message-format=json` (structured JSON per
//! diagnostic), `go vet` emits plain text-lines on stderr —
//! `file:line:col: message` — with no separate analyzer/rule name, so there
//! is no native "code" field to namespace off of the way `rust/E0425` does.
//! [`namespaced_rule_id`] derives a deterministic `go/vet::<slug>` id from
//! the message text instead.

use std::path::Path;
use std::process::Command;

pub struct GoVetInterceptor;

impl Default for GoVetInterceptor {
    fn default() -> Self {
        Self::new()
    }
}

impl GoVetInterceptor {
    pub fn new() -> Self {
        Self
    }
}

/// One parsed `go vet` diagnostic line: `file:line:col: message`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VetDiagnostic {
    pub file: String,
    pub line: usize,
    pub column: usize,
    pub message: String,
}

/// Parses one line of `go vet` stderr output. Skips package-header lines
/// (`# pkg`, `# [pkg]`) `go vet` prints ahead of a failing package's
/// diagnostics, and strips the `vet: ` prefix it prepends when a package
/// fails to type-check before any analyzer runs (so the underlying
/// type-checker error surfaces the same shape a normal analyzer finding
/// does). Returns `None` for lines that aren't `file:line:col: message`
/// shaped (blank lines, `go` toolchain/module errors, ...).
pub fn parse_go_vet_line(line: &str) -> Option<VetDiagnostic> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let line = line.strip_prefix("vet: ").unwrap_or(line);
    let parts: Vec<&str> = line.splitn(4, ':').collect();
    if parts.len() < 4 {
        return None;
    }
    let file = parts[0].trim();
    let line_no: usize = parts[1].trim().parse().ok()?;
    let col_no: usize = parts[2].trim().parse().ok()?;
    let message = parts[3].trim();
    if file.is_empty() || message.is_empty() {
        return None;
    }
    Some(VetDiagnostic {
        file: file.to_string(),
        line: line_no,
        column: col_no,
        message: message.to_string(),
    })
}

/// Parses the full stderr text of a `go vet ./...` run into its diagnostics,
/// line by line, via [`parse_go_vet_line`].
pub fn parse_go_vet_output(output: &str) -> Vec<VetDiagnostic> {
    output.lines().filter_map(parse_go_vet_line).collect()
}

/// Deterministic slug for a vet message, used as the tail of the namespaced
/// rule id (I28: `go/vet::<slug>`) — plain-text `go vet` output carries no
/// analyzer name to namespace off of, unlike cargo's JSON diagnostic codes.
fn slugify(message: &str) -> String {
    let mut slug = String::new();
    let mut last_was_dash = false;
    for ch in message.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            last_was_dash = false;
        } else if !last_was_dash && !slug.is_empty() {
            slug.push('-');
            last_was_dash = true;
        }
    }
    while slug.ends_with('-') {
        slug.pop();
    }
    slug.truncate(40);
    while slug.ends_with('-') {
        slug.pop();
    }
    if slug.is_empty() {
        "diagnostic".to_string()
    } else {
        slug
    }
}

/// Namespaces a `go vet` diagnostic per I28 (e.g. `go/vet::declared-and-not-used-x`).
fn namespaced_rule_id(message: &str) -> String {
    format!("go/vet::{}", slugify(message))
}

/// SARIF-style finding-fingerprint (I28/C2): identity for this finding
/// across runs, derived from its namespaced rule id and location.
fn record_fingerprint(rule_id: &str, file: &str, range: &crate::pack::FileRange) -> String {
    let raw = format!(
        "{}|{}|{}|{}|{}|{}",
        rule_id, file, range.line_start, range.column_start, range.line_end, range.column_end
    );
    crate::sha256::sha256_hex(raw.as_bytes())
}

/// Maps one native `go vet` diagnostic to the engine's normalized record
/// shape (I28, amended by C9). `go vet` text lines carry no extra
/// tool-native detail beyond file/range/message, so the opaque `data`
/// payload stays null (contrast the Rust adapter's `spans` blob, sourced
/// from cargo's richer JSON diagnostics).
fn normalize_diagnostic(diag: &VetDiagnostic) -> crate::pack::NormalizedRecord {
    let range = crate::pack::FileRange {
        line_start: diag.line,
        line_end: diag.line,
        column_start: diag.column,
        column_end: diag.column,
    };
    let rule_id = namespaced_rule_id(&diag.message);
    let fingerprint = record_fingerprint(&rule_id, &diag.file, &range);

    crate::pack::NormalizedRecord {
        rule_id,
        source_tool: "go vet".to_string(),
        tool_level: "warning".to_string(),
        message: diag.message.clone(),
        file: diag.file.clone(),
        range,
        suggested_fix: None,
        doc_ref: None,
        fingerprint,
        data: serde_json::Value::Null,
    }
}

/// Infra-vs-finding split (mirrors the Rust adapter's
/// `determine_is_infra_error`): a failing `go vet` run that produced zero
/// parsed diagnostics means the command itself couldn't run (no `go.mod`,
/// module resolution failure, `go` missing, ...), not a finding.
fn determine_is_infra_error(success: bool, has_diagnostics: bool) -> bool {
    !success && !has_diagnostics
}

impl crate::pack::DiagnosticsAdapter for GoVetInterceptor {
    fn run_check(
        &self,
        project_root: &Path,
        _active_file: &Path,
    ) -> Result<crate::pack::AdapterCheckOutput, String> {
        let output = Command::new("go")
            .args(["vet", "./..."])
            .current_dir(project_root)
            .output()
            .map_err(|e| format!("Failed to spawn go vet: {}", e))?;

        let stderr_str = String::from_utf8_lossy(&output.stderr);
        let diagnostics = parse_go_vet_output(&stderr_str);
        let is_infra = determine_is_infra_error(output.status.success(), !diagnostics.is_empty());

        Ok(crate::pack::AdapterCheckOutput {
            success: output.status.success(),
            is_infra_error: is_infra,
            records: diagnostics.iter().map(normalize_diagnostic).collect(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pack::DiagnosticsAdapter;
    use std::fs;

    #[test]
    fn test_parse_go_vet_line_basic() {
        let diag = parse_go_vet_line(
            "main.go:6:14: fmt.Printf format %d has arg \"not a number\" of wrong type string",
        )
        .unwrap();
        assert_eq!(diag.file, "main.go");
        assert_eq!(diag.line, 6);
        assert_eq!(diag.column, 14);
        assert_eq!(
            diag.message,
            "fmt.Printf format %d has arg \"not a number\" of wrong type string"
        );
    }

    /// Recorded fixture text from a real `go vet ./...` run on a package
    /// with an unused variable: `go vet` prints a package header (`#
    /// pkg`/`# [pkg]`) and a `vet: `-prefixed line before the type-checker
    /// diverges from a normal analyzer finding — both must be handled.
    #[test]
    fn test_parse_go_vet_output_skips_package_header_and_strips_vet_prefix() {
        let fixture = "# example.com/test\n\
                        # [example.com/test]\n\
                        vet: ./main.go:6:2: declared and not used: x\n";
        let diagnostics = parse_go_vet_output(fixture);
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].file, "./main.go");
        assert_eq!(diagnostics[0].line, 6);
        assert_eq!(diagnostics[0].column, 2);
        assert_eq!(diagnostics[0].message, "declared and not used: x");
    }

    /// Recorded fixture text from a real `go vet ./...` run flagging two
    /// `fmt.Printf` format/argument mismatches — the common multi-finding
    /// case, plain lines with no header/prefix.
    #[test]
    fn test_parse_go_vet_output_multi_diagnostic_fixture() {
        let fixture = "main.go:6:14: fmt.Printf format %d has arg \"not a number\" of wrong type string\n\
                        main.go:7:14: fmt.Printf format %s has arg 5 of wrong type int\n";
        let diagnostics = parse_go_vet_output(fixture);
        assert_eq!(diagnostics.len(), 2);
        assert_eq!(diagnostics[0].line, 6);
        assert_eq!(diagnostics[1].line, 7);
    }

    /// A `go vet` invocation outside any module fails with a plain
    /// toolchain message carrying no `file:line:col` shape at all — zero
    /// diagnostics parsed, which `determine_is_infra_error` must treat as
    /// infra, not "no findings".
    #[test]
    fn test_parse_go_vet_output_no_module_message_yields_no_diagnostics() {
        let fixture = "pattern ./...: directory prefix . does not contain main module or its selected dependencies\n";
        let diagnostics = parse_go_vet_output(fixture);
        assert!(diagnostics.is_empty());
    }

    #[test]
    fn test_determine_is_infra_error() {
        assert!(!determine_is_infra_error(true, false));
        assert!(!determine_is_infra_error(false, true));
        assert!(determine_is_infra_error(false, false));
    }

    #[test]
    fn test_namespaced_rule_id_is_go_vet_namespaced_and_deterministic() {
        let a = namespaced_rule_id("declared and not used: x");
        let b = namespaced_rule_id("declared and not used: x");
        assert!(a.starts_with("go/vet::"));
        assert_eq!(a, b);
    }

    #[test]
    fn test_normalize_diagnostic_produces_go_vet_namespaced_record() {
        let diag = VetDiagnostic {
            file: "main.go".to_string(),
            line: 6,
            column: 14,
            message: "fmt.Printf format %d has arg \"x\" of wrong type string".to_string(),
        };
        let record = normalize_diagnostic(&diag);
        assert!(record.rule_id.starts_with("go/vet::"));
        assert_eq!(record.source_tool, "go vet");
        assert_eq!(record.file, "main.go");
        assert_eq!(record.range.line_start, 6);
        assert_eq!(record.range.column_start, 14);
        assert!(!record.fingerprint.is_empty());
    }

    /// CI-safety guard (T7 review): the live-subprocess tests below need a
    /// real Go toolchain. Unlike `cargo` (guaranteed present under
    /// `cargo test`), `go` may be absent — skip with a notice instead of
    /// panicking, keeping the suite hermetic. Deliberately a runtime probe,
    /// not `#[ignore]`, so the tests still run wherever Go exists.
    fn go_toolchain_available() -> bool {
        Command::new("go")
            .arg("version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// Live integration test (mirrors the Rust adapter's
    /// `test_compiler_run_check_and_cancel`): a real `go vet` run against a
    /// generated module, clean then with a genuine `go vet`-flagged issue.
    #[test]
    fn test_go_vet_run_check_against_real_module() {
        if !go_toolchain_available() {
            eprintln!("skipping test_go_vet_run_check_against_real_module: no go toolchain on PATH");
            return;
        }
        let temp_dir = std::env::temp_dir();
        let project_dir = temp_dir.join("test_murshid_go_vet_project");
        let _ = fs::remove_dir_all(&project_dir);
        fs::create_dir_all(&project_dir).unwrap();

        let init_status = Command::new("go")
            .args(["mod", "init", "example.com/murshidtest"])
            .current_dir(&project_dir)
            .status()
            .unwrap();
        assert!(init_status.success());

        let main_go = project_dir.join("main.go");
        fs::write(
            &main_go,
            "package main\n\nimport \"fmt\"\n\nfunc main() {\n\tfmt.Println(\"hi\")\n}\n",
        )
        .unwrap();

        let interceptor = GoVetInterceptor::new();
        let output1 = interceptor.run_check(&project_dir, &main_go).unwrap();
        assert!(output1.success);
        assert!(output1.records.is_empty());
        assert!(!output1.is_infra_error);

        fs::write(
            &main_go,
            "package main\n\nimport \"fmt\"\n\nfunc main() {\n\tfmt.Printf(\"%d\\n\", \"not a number\")\n}\n",
        )
        .unwrap();

        let output2 = interceptor.run_check(&project_dir, &main_go).unwrap();
        assert!(!output2.success);
        assert!(!output2.records.is_empty());
        assert!(!output2.is_infra_error);
        assert!(output2.records[0].rule_id.starts_with("go/vet::"));

        let _ = fs::remove_dir_all(&project_dir);
    }

    /// A directory with no `go.mod` produces zero parseable diagnostics —
    /// `run_check` must classify that as an infra error, not "clean pass".
    #[test]
    fn test_go_vet_run_check_no_module_is_infra_error() {
        if !go_toolchain_available() {
            eprintln!("skipping test_go_vet_run_check_no_module_is_infra_error: no go toolchain on PATH");
            return;
        }
        let temp_dir = std::env::temp_dir();
        let project_dir = temp_dir.join("test_murshid_go_vet_no_module");
        let _ = fs::remove_dir_all(&project_dir);
        fs::create_dir_all(&project_dir).unwrap();
        fs::write(project_dir.join("main.go"), "package main\n").unwrap();

        let interceptor = GoVetInterceptor::new();
        let output = interceptor
            .run_check(&project_dir, &project_dir.join("main.go"))
            .unwrap();
        assert!(!output.success);
        assert!(output.records.is_empty());
        assert!(output.is_infra_error);

        let _ = fs::remove_dir_all(&project_dir);
    }

    // --- T7 acceptance: fixture-driven stage1/stage2 -> card proof over a
    // Go diff, exactly like the Rust pack's (see `pipeline.rs`'s fixture
    // tests) — built entirely from already-generic `crate::pack`/
    // `crate::judge` functions plus this pack's own `packs/go/` data, so no
    // engine module needs a Go-specific edit to prove the pipeline.

    fn go_pack_dir() -> std::path::PathBuf {
        crate::pack::resolve_packs_dir().join("go")
    }

    /// A minimal Go snippet demonstrating the `error-wrapping` concept: the
    /// enclosing function returns the underlying error unwrapped instead of
    /// wrapping it with `fmt.Errorf(..., %w, err)`.
    const GO_DIFF_FILE_CONTENT: &str = "package config\n\nimport \"os\"\n\nfunc Load(path string) ([]byte, error) {\n\tdata, err := os.ReadFile(path)\n\tif err != nil {\n\t\treturn nil, err\n\t}\n\treturn data, nil\n}\n";

    #[test]
    fn test_go_taxonomy_and_canon_load_with_ten_concepts() {
        let taxonomy = crate::pack::load_taxonomy(&go_pack_dir()).unwrap();
        assert_eq!(taxonomy.len(), 10);
        assert!(crate::pack::is_valid_slug(&taxonomy, "error-wrapping"));
        let canon = crate::pack::load_canon(&go_pack_dir()).unwrap();
        assert_eq!(canon.len(), taxonomy.len());
        for concept in &taxonomy {
            assert!(
                crate::pack::find_canon_for_concept(&canon, &concept.slug).is_some(),
                "missing canon entry for {}",
                concept.slug
            );
        }
    }

    /// Stage 1 (screen) over a recorded fixture response for the Go diff
    /// above: the screen model flags the enclosing function as a candidate
    /// `error-wrapping` moment. Mirrors `judge::tests::test_parse_stage1_output_fixture`.
    #[test]
    fn test_stage1_fixture_over_go_diff_yields_error_wrapping_candidate() {
        let stage1_fixture =
            r#"[{"site_hint": "func Load", "slugs": ["error-wrapping"]}]"#.to_string();
        let candidates = crate::judge::parse_stage1_output(&stage1_fixture).unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].site_hint, "func Load");
        assert_eq!(candidates[0].slugs, vec!["error-wrapping".to_string()]);

        let taxonomy = crate::pack::load_taxonomy(&go_pack_dir()).unwrap();
        for slug in &candidates[0].slugs {
            assert!(crate::pack::is_valid_slug(&taxonomy, slug));
        }
    }

    /// Stage 2 (judge) over a recorded fixture response for the same Go
    /// diff: validates the full output contract (I28/C6) and produces a
    /// card naming a Go concept, grounded by a verbatim quote from the file.
    /// Mirrors `judge::tests::test_validate_stage2_output_valid_fixture`.
    #[test]
    fn test_stage2_fixture_over_go_diff_produces_grounded_error_wrapping_card() {
        let stage2_fixture = r#"{
            "concept": "error-wrapping",
            "grounding_quote": "return nil, err",
            "why": "The read failure is returned bare, with no call-site context for the caller to log or match on.",
            "rule": "Wrap errors with fmt.Errorf(\"...: %w\", err) so callers can add context and still errors.Is/As the original.",
            "worked_diff": "- return nil, err\n+ return nil, fmt.Errorf(\"load config %s: %w\", path, err)",
            "category": "idiom",
            "likely_bug": false
        }"#;

        let taxonomy = crate::pack::load_taxonomy(&go_pack_dir()).unwrap();
        let raw = crate::judge::parse_stage2_output(stage2_fixture).unwrap();
        let card =
            crate::judge::validate_stage2_output(&raw, &taxonomy, GO_DIFF_FILE_CONTENT).unwrap();

        // The card names a Go concept from the pack's own taxonomy...
        assert_eq!(card.concept, "error-wrapping");
        assert!(crate::pack::is_valid_slug(&taxonomy, &card.concept));

        // ...grounded by a quote that appears verbatim in the Go file...
        assert!(GO_DIFF_FILE_CONTENT.contains(&card.grounding_quote));

        // ...and traceable to the pack's canon entry for that concept.
        let canon = crate::pack::load_canon(&go_pack_dir()).unwrap();
        let canon_entry = crate::pack::find_canon_for_concept(&canon, &card.concept).unwrap();
        assert_eq!(canon_entry.concept, "error-wrapping");
        assert!(!canon_entry.refs.is_empty());
    }

    /// T7 acceptance sketch's "grounded card" also depends on the prompt
    /// fragments and surface trivia loading correctly for the Go pack (same
    /// payloads the Rust pack's screen/judge dispatch consumes).
    #[test]
    fn test_go_prompts_and_surface_load() {
        let prompts = crate::pack::load_prompt_fragments(&go_pack_dir()).unwrap();
        assert!(prompts.stage1.contains("Go"));
        assert!(prompts.stage2.contains("Go"));

        let surface = crate::pack::load_surface(&go_pack_dir()).unwrap();
        assert_eq!(surface.comment_token, "//");
        assert_eq!(surface.check_command, "go vet ./...");
        assert_eq!(surface.file_extensions, vec!["go".to_string()]);
    }

    /// tree-sitter-go, via the pack's `grammar.json`, parses real Go and
    /// resolves the enclosing-function site for the same diff used above —
    /// the site/quiescence-gate proof, done here (calling the already-
    /// generic `site`/`quiescence` modules, never edited) instead of in
    /// those engine files.
    #[test]
    fn test_go_grammar_parses_real_go_and_resolves_enclosing_function() {
        let grammar = crate::pack::load_grammar(&go_pack_dir()).unwrap();
        assert_eq!(grammar.language_id, "go");
        assert!(crate::site::parses_without_errors(
            GO_DIFF_FILE_CONTENT,
            &grammar
        ));

        // line 8 is `return nil, err` inside `func Load`.
        let enclosing = crate::site::enclosing_item_text(GO_DIFF_FILE_CONTENT, 8, &grammar);
        let enclosing = enclosing.expect("expected an enclosing item for line 8");
        assert!(enclosing.contains("func Load"));
        assert!(enclosing.contains("return nil, err"));

        let broken_source = "package config\n\nfunc Load(path string) ([]byte, error) {\n";
        assert!(!crate::site::parses_without_errors(broken_source, &grammar));
    }

    /// The D8 quiescence gate (pause elapsed + clean parse) against the Go
    /// grammar — proves `should_judge` works pack-agnostically for Go too.
    #[test]
    fn test_go_quiescence_gate_waits_on_parse_error_and_passes_when_clean() {
        use std::time::{Duration, UNIX_EPOCH};

        let grammar = crate::pack::load_grammar(&go_pack_dir()).unwrap();
        let t0 = UNIX_EPOCH + Duration::from_secs(1000);
        let now = t0 + Duration::from_secs(3);

        let broken_source = "package config\n\nfunc Load(path string) ([]byte, error) {\n";
        assert!(!crate::quiescence::should_judge(
            t0,
            now,
            broken_source,
            &grammar
        ));

        assert!(crate::quiescence::should_judge(
            t0,
            now,
            GO_DIFF_FILE_CONTENT,
            &grammar
        ));
    }
}
