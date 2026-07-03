//! Site identity and the quiescence-gate parse check (C2, D8), via tree-sitter.
//!
//! Site = (file path, enclosing item name via tree-sitter [fn/struct/impl/
//! mod], normalized-anchor-expression hash). Survives line shifts and edits
//! elsewhere in the file; an item rename retires the site.

use tree_sitter::{Node, Parser, Point};

const ITEM_KINDS: &[&str] = &["function_item", "struct_item", "impl_item", "mod_item"];

const CONTAINER_KINDS: &[&str] = &[
    "block",
    "field_declaration_list",
    "declaration_list",
    "source_file",
    "match_block",
    "enum_variant_list",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Site {
    pub file: String,
    pub enclosing_item: String,
    pub anchor_hash: String,
}

fn make_parser() -> Option<Parser> {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_rust::LANGUAGE.into())
        .ok()?;
    Some(parser)
}

/// D8 quiescence gate's parse-OK check: a parse error means "wait", never
/// judge a file mid-syntax-error.
pub fn parses_without_errors(source: &str) -> bool {
    let mut parser = match make_parser() {
        Some(p) => p,
        None => return false,
    };
    match parser.parse(source, None) {
        Some(tree) => !tree.root_node().has_error(),
        None => false,
    }
}

fn node_text<'a>(node: Node, source: &'a str) -> &'a str {
    &source[node.start_byte()..node.end_byte()]
}

fn normalize_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn find_enclosing_item(node: Node) -> Option<Node> {
    let mut cur = Some(node);
    while let Some(n) = cur {
        if ITEM_KINDS.contains(&n.kind()) {
            return Some(n);
        }
        cur = n.parent();
    }
    None
}

fn item_name(node: Node, source: &str) -> String {
    match node.kind() {
        "function_item" => {
            let name = node
                .child_by_field_name("name")
                .map(|n| node_text(n, source))
                .unwrap_or("");
            format!("fn {}", name)
        }
        "struct_item" => {
            let name = node
                .child_by_field_name("name")
                .map(|n| node_text(n, source))
                .unwrap_or("");
            format!("struct {}", name)
        }
        "mod_item" => {
            let name = node
                .child_by_field_name("name")
                .map(|n| node_text(n, source))
                .unwrap_or("");
            format!("mod {}", name)
        }
        "impl_item" => {
            let ty = node
                .child_by_field_name("type")
                .map(|n| node_text(n, source))
                .unwrap_or("");
            if let Some(tr) = node.child_by_field_name("trait") {
                format!("impl {} for {}", node_text(tr, source), ty)
            } else {
                format!("impl {}", ty)
            }
        }
        other => other.to_string(),
    }
}

/// Climbs from `leaf` to the nearest ancestor whose parent is a "container"
/// (block/field list/declaration list) — the smallest statement-like node
/// enclosing the target position. Line-number independent by construction.
fn find_anchor_node(leaf: Node) -> Node {
    let mut node = leaf;
    while let Some(parent) = node.parent() {
        if CONTAINER_KINDS.contains(&parent.kind()) {
            break;
        }
        node = parent;
    }
    node
}

/// Parses `source` and locates the leaf node at 1-indexed `line`.
fn locate_leaf<'tree>(
    tree: &'tree tree_sitter::Tree,
    source: &str,
    line: usize,
) -> Option<Node<'tree>> {
    let root = tree.root_node();
    let row = line.saturating_sub(1);
    let line_text = source.lines().nth(row).unwrap_or("");
    let start_point = Point { row, column: 0 };
    let end_point = Point {
        row,
        column: line_text.len(),
    };
    root.descendant_for_point_range(start_point, end_point)
}

/// Returns the full source text of the fn/struct/impl/mod item enclosing
/// 1-indexed `line`, for use as stage-2's "full enclosing item" input (C6).
pub fn enclosing_item_text(source: &str, line: usize) -> Option<String> {
    let mut parser = make_parser()?;
    let tree = parser.parse(source, None)?;
    let leaf = locate_leaf(&tree, source, line)?;
    let item_node = find_enclosing_item(leaf)?;
    Some(node_text(item_node, source).to_string())
}

/// Computes the C2 site for the given 1-indexed `line` in `source`, tagged
/// with `rel_file`. Returns `None` if the position has no enclosing
/// fn/struct/impl/mod item (e.g. top-level `use` statements), or if the
/// source fails to parse.
pub fn compute_site(rel_file: &str, source: &str, line: usize) -> Option<Site> {
    let mut parser = make_parser()?;
    let tree = parser.parse(source, None)?;
    let leaf = locate_leaf(&tree, source, line)?;

    let enclosing_item_node = find_enclosing_item(leaf)?;
    let enclosing_item = item_name(enclosing_item_node, source);

    let anchor_node = find_anchor_node(leaf);
    let anchor_text = normalize_whitespace(node_text(anchor_node, source));
    let anchor_hash = crate::sha256::sha256_hex(anchor_text.as_bytes());

    Some(Site {
        file: rel_file.to_string(),
        enclosing_item,
        anchor_hash,
    })
}

/// D11's dedup key: engine-computed `(concept_id, site)` (C2).
pub fn advice_fingerprint(concept: &str, site: &Site) -> String {
    let raw = format!(
        "{}|{}|{}|{}",
        concept, site.file, site.enclosing_item, site.anchor_hash
    );
    crate::sha256::sha256_hex(raw.as_bytes())
}

/// T4 req 1: mechanical applied-detection. At a LATER quiescence diff of the
/// card's own file, recompute the advice-fingerprint at the card's stored
/// (concept, site) line. `recomputed` is `None` when the position no longer
/// resolves to a matching site at all (e.g. the flagged statement is gone
/// entirely). The pattern is applied when the recomputed fingerprint no
/// longer matches the stored one AND this sweep didn't re-raise the SAME
/// fingerprint as a fresh finding (`fresh_finding_advice_fps` — otherwise a
/// no-op re-judge of unrelated nearby edits could look like "fixed").
pub fn is_applied_by_site_recheck(
    stored_advice_fp: &str,
    recomputed_advice_fp: Option<&str>,
    fresh_finding_advice_fps: &[String],
) -> bool {
    let anchor_gone = recomputed_advice_fp != Some(stored_advice_fp);
    let no_new_finding = !fresh_finding_advice_fps
        .iter()
        .any(|fp| fp == stored_advice_fp);
    anchor_gone && no_new_finding
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parses_without_errors_valid() {
        assert!(parses_without_errors("fn main() {\n    let x = 1;\n}\n"));
    }

    #[test]
    fn test_parses_without_errors_invalid() {
        assert!(!parses_without_errors("fn main() {\n    let x = 1;\n"));
    }

    #[test]
    fn test_enclosing_item_function() {
        let src = "fn foo() {\n    let x = y.clone();\n}\n";
        let site = compute_site("src/lib.rs", src, 2).unwrap();
        assert_eq!(site.enclosing_item, "fn foo");
    }

    #[test]
    fn test_enclosing_item_struct() {
        let src = "struct Point {\n    x: i32,\n    y: i32,\n}\n";
        let site = compute_site("src/lib.rs", src, 2).unwrap();
        assert_eq!(site.enclosing_item, "struct Point");
    }

    #[test]
    fn test_enclosing_item_impl() {
        let src = "struct Point;\nimpl Point {\n    fn new() -> Self { Point }\n}\n";
        let site = compute_site("src/lib.rs", src, 3).unwrap();
        assert_eq!(site.enclosing_item, "impl Point");
    }

    #[test]
    fn test_enclosing_item_mod() {
        let src = "mod things {\n    fn helper() {}\n}\n";
        let site = compute_site("src/lib.rs", src, 2).unwrap();
        assert_eq!(site.enclosing_item, "mod things");
    }

    #[test]
    fn test_no_enclosing_item_at_top_level() {
        let src = "use std::fmt;\n";
        assert!(compute_site("src/lib.rs", src, 1).is_none());
    }

    #[test]
    fn test_enclosing_item_text_returns_full_function() {
        let src = "fn foo() {\n    let x = y.clone();\n    println!(\"{}\", x);\n}\n";
        let text = enclosing_item_text(src, 2).unwrap();
        assert_eq!(
            text,
            "fn foo() {\n    let x = y.clone();\n    println!(\"{}\", x);\n}"
        );
    }

    #[test]
    fn test_advice_fingerprint_identity_across_line_shift() {
        let src_a = "fn foo() {\n    let x = y.clone();\n    println!(\"{}\", x);\n}\n";
        let src_b = "// a comment\n// another comment\n\nfn foo() {\n    let x = y.clone();\n    println!(\"{}\", x);\n}\n";

        let site_a = compute_site("src/lib.rs", src_a, 2).unwrap();
        // Same statement, shifted down by 3 lines in src_b.
        let site_b = compute_site("src/lib.rs", src_b, 5).unwrap();

        assert_eq!(site_a.enclosing_item, site_b.enclosing_item);
        assert_eq!(site_a.anchor_hash, site_b.anchor_hash);

        let fp_a = advice_fingerprint("borrow-vs-clone", &site_a);
        let fp_b = advice_fingerprint("borrow-vs-clone", &site_b);
        assert_eq!(fp_a, fp_b);
    }

    #[test]
    fn test_advice_fingerprint_differs_after_item_rename() {
        let src_a = "fn foo() {\n    let x = y.clone();\n}\n";
        let src_b = "fn bar() {\n    let x = y.clone();\n}\n";

        let site_a = compute_site("src/lib.rs", src_a, 2).unwrap();
        let site_b = compute_site("src/lib.rs", src_b, 2).unwrap();

        let fp_a = advice_fingerprint("borrow-vs-clone", &site_a);
        let fp_b = advice_fingerprint("borrow-vs-clone", &site_b);
        assert_ne!(
            fp_a, fp_b,
            "renaming the enclosing item should retire the site"
        );
    }

    #[test]
    fn test_advice_fingerprint_differs_across_concepts() {
        let src = "fn foo() {\n    let x = y.clone();\n}\n";
        let site = compute_site("src/lib.rs", src, 2).unwrap();
        let fp_a = advice_fingerprint("borrow-vs-clone", &site);
        let fp_b = advice_fingerprint("string-vs-str", &site);
        assert_ne!(fp_a, fp_b);
    }

    // --- T4 req 1: mechanical applied-detection via site re-check ---

    #[test]
    fn test_is_applied_when_anchor_gone_and_no_fresh_finding() {
        // The flagged clone() at the site is gone (borrowed instead) and no
        // fresh finding re-raised the same fingerprint this sweep.
        assert!(is_applied_by_site_recheck("old-fp", Some("new-fp"), &[]));
        assert!(is_applied_by_site_recheck("old-fp", None, &[]));
    }

    #[test]
    fn test_not_applied_when_anchor_unchanged() {
        assert!(!is_applied_by_site_recheck(
            "same-fp",
            Some("same-fp"),
            &[]
        ));
    }

    #[test]
    fn test_not_applied_when_a_fresh_finding_reraises_the_same_fingerprint() {
        // The anchor text changed (a nearby edit shifted things) but the
        // SAME advice-fp was re-raised as a fresh finding this sweep —
        // not a fix, just still-flagged.
        assert!(!is_applied_by_site_recheck(
            "old-fp",
            Some("different-fp"),
            &["old-fp".to_string()]
        ));
    }

    #[test]
    fn test_full_flow_site_re_check_after_fix() {
        // Simulates the acceptance scenario: original flagged clone(), then
        // a later edit borrows instead.
        let before = "fn foo(name: String) {\n    let x = name.clone();\n}\n";
        let after = "fn foo(name: String) {\n    let x = &name;\n}\n";

        let site_before = compute_site("src/lib.rs", before, 2).unwrap();
        let stored_fp = advice_fingerprint("borrow-vs-clone", &site_before);

        // Later quiescence diff: recompute at the same line in the new content.
        let recomputed = compute_site("src/lib.rs", after, 2)
            .map(|s| advice_fingerprint("borrow-vs-clone", &s));

        assert!(is_applied_by_site_recheck(
            &stored_fp,
            recomputed.as_deref(),
            &[]
        ));
    }
}
