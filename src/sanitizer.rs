//! Best-effort secret and path redaction applied to every prompt before it
//! leaves the machine for an LLM (non-negotiable, C6). Scrubs known API-key
//! shapes (Claude/Google/AWS), DB URIs, and absolute paths. Context-free by
//! nature, so it errs toward over-redaction; see the per-pattern notes.

pub fn redact_secrets(input: &str) -> String {
    // Scan the ENTIRE input. `scan_secrets_dfa` is a single-pass linear DFA
    // (no backtracking), so there is no cost reason to cap it — and an earlier
    // 10k-char cap that appended the tail verbatim silently leaked any secret
    // past the boundary (in a large diff/diagnostic) to the third-party LLM.
    let chars: Vec<char> = input.chars().collect();
    scan_secrets_dfa(&chars)
}

pub fn sanitize_paths(input: &str) -> String {
    // 1. Get the current user's home directory.
    let mut home_dirs = Vec::new();
    if let Ok(home) = std::env::var("HOME") {
        if !home.is_empty() {
            home_dirs.push(home);
        }
    }
    if let Ok(userprofile) = std::env::var("USERPROFILE") {
        if !userprofile.is_empty() {
            home_dirs.push(userprofile);
        }
    }

    // Sort home_dirs by length descending to match the longest path first
    home_dirs.sort_by_key(|b| std::cmp::Reverse(b.len()));

    let mut result = input.to_string();
    for home in home_dirs {
        result = result.replace(&home, "[USER_HOME]");

        let home_backslashes = home.replace("/", "\\");
        if home_backslashes != home {
            result = result.replace(&home_backslashes, "[USER_HOME]");
        }
    }

    // 2. Also replace general home patterns like /Users/username/ or /home/username/
    let chars: Vec<char> = result.chars().collect();
    let mut i = 0;
    let mut sanitized = String::new();

    while i < chars.len() {
        if let Some((len, _is_backslash)) = match_general_home_pattern(&chars, i) {
            sanitized.push_str("[USER_HOME]");
            i += len;
            continue;
        }

        sanitized.push(chars[i]);
        i += 1;
    }

    sanitized
}

pub fn sanitize_diagnostics(input: &str) -> String {
    let scrubbed_secrets = redact_secrets(input);
    sanitize_paths(&scrubbed_secrets)
}

fn scan_secrets_dfa(chars: &[char]) -> String {
    let mut result = String::new();
    let mut i = 0;
    while i < chars.len() {
        // Check for Database URIs
        if let Some(len) = match_db_uri(chars, i) {
            result.push_str("[REDACTED]");
            i += len;
            continue;
        }

        // Check for Google API Key: AIza + 35 chars of [A-Za-z0-9\-_]
        if let Some(len) = match_google_key(chars, i) {
            result.push_str("[REDACTED]");
            i += len;
            continue;
        }

        // Check for Claude API Key: sk-ant- + alphanumeric/dash/underscore
        if let Some(len) = match_claude_key(chars, i) {
            result.push_str("[REDACTED]");
            i += len;
            continue;
        }

        // Check for AWS Access Key: AKIA + 16 chars of [A-Z0-9]
        if let Some(len) = match_aws_access_key(chars, i) {
            result.push_str("[REDACTED]");
            i += len;
            continue;
        }

        // Check for AWS Secret Access Key: standalone 40 chars of base64
        if let Some(len) = match_aws_secret_key(chars, i) {
            result.push_str("[REDACTED]");
            i += len;
            continue;
        }

        result.push(chars[i]);
        i += 1;
    }
    result
}

fn match_db_uri(chars: &[char], i: usize) -> Option<usize> {
    let prefixes = [
        "postgresql://",
        "postgres://",
        "mongodb://",
        "mysql://",
        "redis://",
        "sqlite://",
    ];
    for &prefix in &prefixes {
        if has_prefix_at(chars, i, prefix) {
            let mut j = i + prefix.len();
            while j < chars.len() {
                let c = chars[j];
                if c.is_whitespace()
                    || c == '"'
                    || c == '\''
                    || c == '`'
                    || c == '<'
                    || c == '>'
                    || c == '['
                    || c == ']'
                    || c == '{'
                    || c == '}'
                    || c == '('
                    || c == ')'
                {
                    break;
                }
                j += 1;
            }
            return Some(j - i);
        }
    }
    None
}

fn match_google_key(chars: &[char], i: usize) -> Option<usize> {
    if !has_prefix_at(chars, i, "AIza") {
        return None;
    }
    if i + 4 <= chars.len() {
        let mut count = 0;
        let mut j = i + 4;
        while j < chars.len() && (chars[j].is_alphanumeric() || chars[j] == '-' || chars[j] == '_')
        {
            count += 1;
            j += 1;
            if count == 35 {
                break;
            }
        }
        if count == 35 {
            if j < chars.len() {
                let next_c = chars[j];
                if next_c.is_alphanumeric() || next_c == '-' || next_c == '_' {
                    return None;
                }
            }
            // "AIza" (4) + exactly 35 body chars; `j - i` is that span (== 39),
            // computed rather than hardcoded so it can't drift from the loop.
            return Some(j - i);
        }
    }
    None
}

fn match_claude_key(chars: &[char], i: usize) -> Option<usize> {
    if !has_prefix_at(chars, i, "sk-ant-") {
        return None;
    }
    let mut j = i + 7;
    while j < chars.len() && (chars[j].is_alphanumeric() || chars[j] == '-' || chars[j] == '_') {
        j += 1;
    }
    if j > i + 7 { Some(j - i) } else { None }
}

fn match_aws_access_key(chars: &[char], i: usize) -> Option<usize> {
    if !has_prefix_at(chars, i, "AKIA") {
        return None;
    }
    if i + 20 <= chars.len() {
        for idx in 4..20 {
            let c = chars[i + idx];
            if !(c.is_ascii_uppercase() || c.is_ascii_digit()) {
                return None;
            }
        }
        if i + 20 < chars.len() {
            let next_c = chars[i + 20];
            if next_c.is_ascii_uppercase() || next_c.is_ascii_lowercase() || next_c.is_ascii_digit()
            {
                return None;
            }
        }
        return Some(20);
    }
    None
}

fn match_aws_secret_key(chars: &[char], i: usize) -> Option<usize> {
    if i > 0 && is_base64_char(chars[i - 1]) {
        return None;
    }
    let mut j = i;
    while j < chars.len() && is_base64_char(chars[j]) {
        j += 1;
    }
    let len = j - i;
    if (40..=45).contains(&len) {
        let mut non_hex = 0;
        for &c in &chars[i..j] {
            if !c.is_ascii_hexdigit() {
                non_hex += 1;
            }
        }
        if non_hex > 0 {
            return Some(len);
        }
    }
    None
}

fn is_base64_char(c: char) -> bool {
    c.is_alphanumeric() || c == '/' || c == '+' || c == '='
}

fn has_prefix_at(chars: &[char], i: usize, prefix: &str) -> bool {
    if i + prefix.len() > chars.len() {
        return false;
    }
    for (idx, c) in prefix.chars().enumerate() {
        if chars[i + idx] != c {
            return false;
        }
    }
    true
}

fn match_general_home_pattern(chars: &[char], i: usize) -> Option<(usize, bool)> {
    if has_prefix_at(chars, i, "/Users/") || has_prefix_at(chars, i, "/home/") {
        let prefix_len = if has_prefix_at(chars, i, "/Users/") {
            7
        } else {
            6
        };
        let mut j = i + prefix_len;
        while j < chars.len() && is_username_char(chars[j]) {
            j += 1;
        }
        if j > i + prefix_len
            && (j == chars.len()
                || chars[j] == '/'
                || chars[j] == '\\'
                || chars[j].is_whitespace()
                || chars[j] == '"'
                || chars[j] == '\''
                || chars[j] == '`')
        {
            return Some((j - i, false));
        }
    }

    let win_prefixes = ["C:\\Users\\", "c:\\Users\\", "\\Users\\"];
    for &prefix in &win_prefixes {
        if has_prefix_at(chars, i, prefix) {
            let mut j = i + prefix.len();
            while j < chars.len() && is_username_char(chars[j]) {
                j += 1;
            }
            if j > i + prefix.len()
                && (j == chars.len()
                    || chars[j] == '/'
                    || chars[j] == '\\'
                    || chars[j].is_whitespace()
                    || chars[j] == '"'
                    || chars[j] == '\''
                    || chars[j] == '`')
            {
                return Some((j - i, true));
            }
        }
    }

    None
}

fn is_username_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '-' || c == '.'
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_redact_google_key() {
        let input = "My key is AIzaSyD98734293847293847293847293847293 and it is secret.";
        let expected = "My key is [REDACTED] and it is secret.";
        assert_eq!(redact_secrets(input), expected);
    }

    #[test]
    fn test_redact_claude_key() {
        let input = "Key: sk-ant-sid01-abcdefghijklmnopqrstuvwxyz0123456789ABCDEFghij-klmn_opqrst";
        let expected = "Key: [REDACTED]";
        assert_eq!(redact_secrets(input), expected);
    }

    #[test]
    fn test_redact_aws_keys() {
        let access = "AWS ID is AKIAIOSFODNN7EXAMPLE.";
        let expected_access = "AWS ID is [REDACTED].";
        assert_eq!(redact_secrets(access), expected_access);

        let secret = "AWS Secret is wJalrXUtnFEMI/K7MDENG/bPxRfiCYzEXAMPLEKEY."; // exactly 40 chars
        let expected_secret = "AWS Secret is [REDACTED].";
        assert_eq!(redact_secrets(secret), expected_secret);
    }

    #[test]
    fn test_redact_db_uri() {
        let uri = "Connect to postgresql://username:password@localhost:5432/mydb database";
        let expected = "Connect to [REDACTED] database";
        assert_eq!(redact_secrets(uri), expected);
    }

    #[test]
    fn test_redact_secret_past_10k_boundary() {
        // A secret far past the old 10k-char scan cap must still be redacted:
        // the scanner is linear, so there is no reason to stop, and appending
        // the tail verbatim leaked keys in large diffs/diagnostics to the LLM.
        let mut large_input = String::new();
        for _ in 0..1000 {
            large_input.push_str("some_data ");
        }
        let key_suffix = " AIzaSyD98734293847293847293847293847293";
        let full_input = format!("{}{}", large_input, key_suffix);

        let redacted = redact_secrets(&full_input);
        assert!(
            !redacted.contains("AIzaSyD98734293847293847293847293847293"),
            "a Google API key past the 10k boundary must be redacted, not passed through"
        );
        assert!(redacted.contains("[REDACTED]"));
    }

    #[test]
    fn test_sanitize_paths() {
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .unwrap_or_default();

        if !home.is_empty() {
            let diag = format!("Error in file {}/src/main.rs: cannot compile.", home);
            let expected = "Error in file [USER_HOME]/src/main.rs: cannot compile.";
            assert_eq!(sanitize_paths(&diag), expected);
        }

        let generic_diag = "Error in file /Users/anotheruser/src/lib.rs and /home/ubuntu/test.txt";
        let expected_generic = "Error in file [USER_HOME]/src/lib.rs and [USER_HOME]/test.txt";
        assert_eq!(sanitize_paths(generic_diag), expected_generic);
    }

    // --- ROADMAP item 8: property-based redaction invariants ---

    use proptest::prelude::*;

    /// A well-formed Google API key (`AIza` + 35 chars of [A-Za-z0-9\-_]) —
    /// the exact shape `match_google_key` redacts, built from an arbitrary
    /// 35-char tail so the property doesn't hinge on one fixed secret.
    fn google_key(tail: &str) -> String {
        format!("AIza{}", tail)
    }

    proptest! {
        /// Redaction is idempotent: scrubbing already-scrubbed text is a no-op.
        /// `[REDACTED]` / `[USER_HOME]` contain no secret/path shape, so a
        /// second pass finds nothing new.
        #[test]
        fn prop_redact_secrets_is_idempotent(s in ".*") {
            let once = redact_secrets(&s);
            prop_assert_eq!(redact_secrets(&once), once);
        }

        #[test]
        fn prop_sanitize_diagnostics_is_idempotent(s in ".*") {
            let once = sanitize_diagnostics(&s);
            prop_assert_eq!(sanitize_diagnostics(&once), once);
        }

        /// Never-leak: a key embedded at an arbitrary offset in arbitrary text
        /// is ALWAYS redacted — the output contains `[REDACTED]` and never the
        /// key itself. Surrounding text is space-delimited so no match can span
        /// the boundary (space ∉ the key alphabet).
        #[test]
        fn prop_embedded_key_is_always_redacted(
            prefix in "[ -~]{0,40}",
            suffix in "[ -~]{0,40}",
            tail in "[A-Za-z0-9]{35}",
        ) {
            let key = google_key(&tail);
            let input = format!("{prefix} {key} {suffix}");
            let out = redact_secrets(&input);
            prop_assert!(out.contains("[REDACTED]"), "no redaction marker in {:?}", out);
            prop_assert!(!out.contains(&key), "leaked key in {:?}", out);
        }
    }
}
