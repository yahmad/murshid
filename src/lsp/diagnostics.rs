use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct Position {
    pub line: u32,
    pub character: u32,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct Range {
    pub start: Position,
    pub end: Position,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct Diagnostic {
    pub range: Range,
    pub severity: Option<u32>, // 3 = Information, 4 = Hint
    pub code: Option<String>,
    pub source: Option<String>,
    pub message: String,
    pub tags: Option<Vec<u32>>, // 1 = Unnecessary (clear/fade tag)
}

pub struct DiagnosticsManager {
    pub active_diagnostics: Mutex<HashMap<String, Vec<Diagnostic>>>,
}

impl DiagnosticsManager {
    pub fn new() -> Self {
        Self {
            active_diagnostics: Mutex::new(HashMap::new()),
        }
    }

    pub fn set_diagnostics(&self, uri: String, diagnostics: Vec<Diagnostic>) {
        let mut map = self.active_diagnostics.lock().unwrap();
        map.insert(uri, diagnostics);
    }

    pub fn get_diagnostics(&self, uri: &str) -> Vec<Diagnostic> {
        let map = self.active_diagnostics.lock().unwrap();
        map.get(uri).cloned().unwrap_or_default()
    }

    /// Processes didChange event contentChanges.
    /// If any edited range overlaps with a diagnostic, that diagnostic is immediately cleared.
    /// Returns the updated list of diagnostics if any were cleared.
    pub fn handle_did_change(
        &self,
        uri: &str,
        changes: &[serde_json::Value],
    ) -> Option<Vec<Diagnostic>> {
        let mut map = self.active_diagnostics.lock().unwrap();
        let diags = map.get_mut(uri)?;

        let mut cleared = false;

        for change in changes {
            if let Some(range_val) = change.get("range") {
                if let Ok(range) = serde_json::from_value::<Range>(range_val.clone()) {
                    let start_line = range.start.line;
                    let end_line = range.end.line;

                    let initial_len = diags.len();
                    diags.retain(|d| {
                        let d_start = d.range.start.line;
                        let d_end = d.range.end.line;

                        // Check for overlap between [d_start, d_end] and [start_line, end_line]
                        let overlap = (d_start >= start_line && d_start <= end_line)
                            || (d_end >= start_line && d_end <= end_line)
                            || (start_line >= d_start && start_line <= d_end);

                        !overlap
                    });

                    if diags.len() != initial_len {
                        cleared = true;
                    }
                }
            } else {
                // No range means whole document replaced
                if !diags.is_empty() {
                    diags.clear();
                    cleared = true;
                }
            }
        }

        if cleared {
            Some(diags.clone())
        } else {
            None
        }
    }
}

pub fn format_publish_diagnostics(uri: &str, diagnostics: &[Diagnostic]) -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "method": "textDocument/publishDiagnostics",
        "params": {
            "uri": uri,
            "diagnostics": diagnostics,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_diagnostics_spec_compliance() {
        let diag = Diagnostic {
            range: Range {
                start: Position { line: 10, character: 0 },
                end: Position { line: 10, character: 15 },
            },
            severity: Some(4), // Hint
            code: Some("murshid-pedagogy".to_string()),
            source: Some("murshid".to_string()),
            message: "Have you considered trait bounds?".to_string(),
            tags: Some(vec![1]), // Unnecessary / Fade tag
        };

        assert_eq!(diag.source.as_deref(), Some("murshid"));
        assert!(diag.severity == Some(3) || diag.severity == Some(4));
        assert_eq!(diag.tags.as_ref().unwrap()[0], 1);
    }

    #[test]
    fn test_did_change_clears_overlapping_ranges() {
        let manager = DiagnosticsManager::new();
        let uri = "file:///src/main.rs".to_string();

        let diags = vec![
            Diagnostic {
                range: Range {
                    start: Position { line: 5, character: 0 },
                    end: Position { line: 5, character: 20 },
                },
                severity: Some(4),
                code: None,
                source: Some("murshid".to_string()),
                message: "Hint at line 5".to_string(),
                tags: None,
            },
            Diagnostic {
                range: Range {
                    start: Position { line: 15, character: 0 },
                    end: Position { line: 15, character: 20 },
                },
                severity: Some(4),
                code: None,
                source: Some("murshid".to_string()),
                message: "Hint at line 15".to_string(),
                tags: None,
            },
        ];

        manager.set_diagnostics(uri.clone(), diags);

        // Edit at line 5 (character 2 to 5)
        let change = serde_json::json!({
            "range": {
                "start": { "line": 5, "character": 2 },
                "end": { "line": 5, "character": 5 }
            },
            "text": "foo"
        });

        let start = Instant::now();
        let updated = manager.handle_did_change(&uri, &[change]).unwrap();
        let elapsed = start.elapsed();

        // 1. Should compile check/run in under 50ms
        assert!(elapsed.as_millis() < 50, "Did change event processing took too long: {:?}", elapsed);

        // 2. Line 5 diagnostic should be cleared, line 15 diagnostic should remain
        assert_eq!(updated.len(), 1);
        assert_eq!(updated[0].range.start.line, 15);
    }

    #[test]
    fn test_did_change_no_overlap_remains() {
        let manager = DiagnosticsManager::new();
        let uri = "file:///src/main.rs".to_string();

        let diags = vec![
            Diagnostic {
                range: Range {
                    start: Position { line: 10, character: 0 },
                    end: Position { line: 10, character: 20 },
                },
                severity: Some(4),
                code: None,
                source: Some("murshid".to_string()),
                message: "Hint at line 10".to_string(),
                tags: None,
            },
        ];

        manager.set_diagnostics(uri.clone(), diags);

        // Edit at line 12
        let change = serde_json::json!({
            "range": {
                "start": { "line": 12, "character": 0 },
                "end": { "line": 12, "character": 5 }
            },
            "text": "bar"
        });

        let updated = manager.handle_did_change(&uri, &[change]);
        assert!(updated.is_none(), "Expected no diagnostics to be cleared");
    }
}
