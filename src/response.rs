//! Card response capture (T1 req 10, C3): maps the in-pane keys `g`/`u`/`n`
//! to the C3 response-verb enum. Pure logic — the stdin-reading loop that
//! calls this lives in main.rs (untested, like the rest of the watch loop's
//! process glue).

/// Maps a trimmed, case-insensitive single-key input to its C3 response
/// verb. Unknown input (including empty) maps to `None` and is ignored by
/// the caller.
pub fn response_verb_for_key(input: &str) -> Option<&'static str> {
    match input.trim().to_lowercase().as_str() {
        "g" => Some("got_it"),
        "u" => Some("not_useful"),
        "n" => Some("not_now"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_g_maps_to_got_it() {
        assert_eq!(response_verb_for_key("g"), Some("got_it"));
    }

    #[test]
    fn test_u_maps_to_not_useful() {
        assert_eq!(response_verb_for_key("u"), Some("not_useful"));
    }

    #[test]
    fn test_n_maps_to_not_now() {
        assert_eq!(response_verb_for_key("n"), Some("not_now"));
    }

    #[test]
    fn test_case_insensitive_and_trimmed() {
        assert_eq!(response_verb_for_key("  G\n"), Some("got_it"));
        assert_eq!(response_verb_for_key("U"), Some("not_useful"));
    }

    #[test]
    fn test_unknown_key_is_none() {
        assert_eq!(response_verb_for_key("x"), None);
        assert_eq!(response_verb_for_key(""), None);
        assert_eq!(response_verb_for_key("got_it"), None);
    }
}
