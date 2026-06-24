#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct Command {
    pub title: String,
    pub command: String,
    pub arguments: Option<Vec<serde_json::Value>>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct CodeAction {
    pub title: String,
    pub kind: Option<String>, // "quickfix.murshid", "refactor.murshid"
    pub diagnostics: Option<Vec<crate::lsp_diagnostics::Diagnostic>>,
    pub command: Option<Command>,
    pub is_preferred: Option<bool>,
}

/// Builds the custom Socratic code action triggering the Socratic panel open command.
pub fn create_murshid_code_action(
    uri: &str,
    line: u32,
    diagnostic: crate::lsp_diagnostics::Diagnostic,
    kind: &str, // "quickfix.murshid" or "refactor.murshid"
) -> CodeAction {
    let title = format!("Ask Murshid: {}", diagnostic.message);
    CodeAction {
        title,
        kind: Some(kind.to_string()),
        diagnostics: Some(vec![diagnostic]),
        command: Some(Command {
            title: "Open Socratic Chat Panel".to_string(),
            command: "murshid.openChatPanel".to_string(),
            arguments: Some(vec![
                serde_json::json!(uri),
                serde_json::json!(line),
            ]),
        }),
        is_preferred: Some(true),
    }
}

/// Appends Socratic guidance to the diagnostics message as clean Markdown fallback.
pub fn format_socratic_guidance_fallback(diagnostic_msg: &str, socratic_hint: &str) -> String {
    format!(
        "{}\n\n---\n\n### 🧭 Socratic Guidance\n\n{}",
        diagnostic_msg, socratic_hint
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lsp_diagnostics::{Diagnostic, Position, Range};

    #[test]
    fn test_create_code_actions_registration() {
        let diag = Diagnostic {
            range: Range {
                start: Position { line: 5, character: 0 },
                end: Position { line: 5, character: 10 },
            },
            severity: Some(4),
            code: None,
            source: Some("murshid".to_string()),
            message: "Variable ownership conflict".to_string(),
            tags: None,
        };

        let action_qf = create_murshid_code_action(
            "file:///src/main.rs",
            5,
            diag.clone(),
            "quickfix.murshid",
        );

        assert_eq!(action_qf.kind.as_deref(), Some("quickfix.murshid"));
        assert_eq!(action_qf.diagnostics.unwrap()[0].message, "Variable ownership conflict");
        
        let cmd = action_qf.command.unwrap();
        assert_eq!(cmd.command, "murshid.openChatPanel");
        assert_eq!(cmd.arguments.unwrap()[0].as_str().unwrap(), "file:///src/main.rs");
    }

    #[test]
    fn test_format_socratic_guidance_fallback_markdown() {
        let msg = "cannot borrow `x` as mutable more than once";
        let hint = "Consider where the first borrow ends. Does it overlap?";
        let fallback = format_socratic_guidance_fallback(msg, hint);

        assert!(fallback.starts_with("cannot borrow `x` as mutable more than once"));
        assert!(fallback.contains("### 🧭 Socratic Guidance"));
        assert!(fallback.contains("Consider where the first borrow ends."));
    }
}
