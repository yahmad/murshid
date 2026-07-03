//! Site identity and the quiescence-gate parse check (C2, D8), via
//! tree-sitter. T6: the grammar (tree-sitter `Language`) and node-kind
//! vocabulary (which node kinds are "items"/"containers") are pack DATA
//! (payload 6, `crate::pack::GrammarSpec`), passed in by the caller — this
//! module contains no language-specific literals.

use tree_sitter::{Node, Parser, Point};

use crate::pack::{GrammarSpec, ItemKindDef};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Site {
    pub file: String,
    pub enclosing_item: String,
    pub anchor_hash: String,
}

fn make_parser(grammar: &GrammarSpec) -> Option<Parser> {
    let mut parser = Parser::new();
    parser.set_language(&grammar.ts_language).ok()?;
    Some(parser)
}

/// D8 quiescence gate's parse-OK check: a parse error means "wait", never
/// judge a file mid-syntax-error.
pub fn parses_without_errors(source: &str, grammar: &GrammarSpec) -> bool {
    let mut parser = match make_parser(grammar) {
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

fn is_item_kind(kind: &str, grammar: &GrammarSpec) -> bool {
    grammar.item_kinds.iter().any(|d| d.kind == kind)
}

fn find_enclosing_item<'tree>(node: Node<'tree>, grammar: &GrammarSpec) -> Option<Node<'tree>> {
    let mut cur = Some(node);
    while let Some(n) = cur {
        if is_item_kind(n.kind(), grammar) {
            return Some(n);
        }
        cur = n.parent();
    }
    None
}

/// Renders an item node's name from its pack-supplied [`ItemKindDef`]:
/// trait-implementing items ("impl X for Y") first, then plain
/// type-implementing items ("impl Y"), then simple named items ("fn foo").
/// A node kind with no matching def (shouldn't happen for a well-formed
/// pack) falls back to its raw tree-sitter kind string.
fn item_name(node: Node, source: &str, grammar: &GrammarSpec) -> String {
    let Some(def) = grammar.item_kinds.iter().find(|d| d.kind == node.kind()) else {
        return node.kind().to_string();
    };
    render_item_name(node, source, def)
}

fn render_item_name(node: Node, source: &str, def: &ItemKindDef) -> String {
    if let Some(type_field) = &def.type_field {
        let ty = node
            .child_by_field_name(type_field.as_str())
            .map(|n| node_text(n, source))
            .unwrap_or("");
        if let Some(trait_field) = &def.trait_field {
            if let Some(tr) = node.child_by_field_name(trait_field.as_str()) {
                return format!("{} {} for {}", def.label, node_text(tr, source), ty);
            }
        }
        return format!("{} {}", def.label, ty);
    }
    if let Some(name_field) = &def.name_field {
        let name = node
            .child_by_field_name(name_field.as_str())
            .map(|n| node_text(n, source))
            .unwrap_or("");
        return format!("{} {}", def.label, name);
    }
    def.label.clone()
}

fn is_container_kind(kind: &str, grammar: &GrammarSpec) -> bool {
    grammar.container_kinds.iter().any(|k| k == kind)
}

/// Climbs from `leaf` to the nearest ancestor whose parent is a "container"
/// (block/field list/declaration list) — the smallest statement-like node
/// enclosing the target position. Line-number independent by construction.
fn find_anchor_node<'tree>(leaf: Node<'tree>, grammar: &GrammarSpec) -> Node<'tree> {
    let mut node = leaf;
    while let Some(parent) = node.parent() {
        if is_container_kind(parent.kind(), grammar) {
            break;
        }
        node = parent;
    }
    node
}

/// Parses `source` and locates the leaf node at 1-indexed `line`.
///
/// T4 gating-fix note: the range passed to `descendant_for_point_range`
/// must be the line's TRIMMED (non-whitespace) span, not the raw
/// column-0-to-end span. A raw span starting at column 0 on an indented
/// line (the common case — every statement inside a block) doesn't fit
/// fully inside the statement node (the leading whitespace belongs to the
/// enclosing block, not the statement), so tree-sitter would return that
/// enclosing block/item as the "leaf" instead — which then made
/// `find_anchor_node` bubble all the way up to the whole item, silently
/// turning every anchor into a hash of the ENTIRE enclosing item's text.
/// That made the site-recheck fix (scanning the item for the specific
/// anchor) meaningless: any unrelated edit anywhere in the item changed the
/// "anchor", not just edits to the flagged statement itself.
fn locate_leaf<'tree>(
    tree: &'tree tree_sitter::Tree,
    source: &str,
    line: usize,
) -> Option<Node<'tree>> {
    let root = tree.root_node();
    let row = line.saturating_sub(1);
    let line_text = source.lines().nth(row)?;
    let trimmed = line_text.trim();
    if trimmed.is_empty() {
        let point = Point { row, column: 0 };
        return root.descendant_for_point_range(point, point);
    }
    let start_col = line_text.len() - line_text.trim_start().len();
    let end_col = start_col + trimmed.len();
    let start_point = Point { row, column: start_col };
    let end_point = Point { row, column: end_col };
    root.descendant_for_point_range(start_point, end_point)
}

/// Returns the full source text of the item enclosing 1-indexed `line`, for
/// use as stage-2's "full enclosing item" input (C6).
pub fn enclosing_item_text(source: &str, line: usize, grammar: &GrammarSpec) -> Option<String> {
    let mut parser = make_parser(grammar)?;
    let tree = parser.parse(source, None)?;
    let leaf = locate_leaf(&tree, source, line)?;
    let item_node = find_enclosing_item(leaf, grammar)?;
    Some(node_text(item_node, source).to_string())
}

/// Computes the C2 site for the given 1-indexed `line` in `source`, tagged
/// with `rel_file`. Returns `None` if the position has no enclosing item per
/// the pack's grammar (e.g. a top-level import statement), or if the source
/// fails to parse.
pub fn compute_site(
    rel_file: &str,
    source: &str,
    line: usize,
    grammar: &GrammarSpec,
) -> Option<Site> {
    let mut parser = make_parser(grammar)?;
    let tree = parser.parse(source, None)?;
    let leaf = locate_leaf(&tree, source, line)?;

    let enclosing_item_node = find_enclosing_item(leaf, grammar)?;
    let enclosing_item = item_name(enclosing_item_node, source, grammar);

    let anchor_node = find_anchor_node(leaf, grammar);
    let anchor_text = normalize_whitespace(node_text(anchor_node, source));
    let anchor_hash = crate::sha256::sha256_hex(anchor_text.as_bytes());

    Some(Site {
        file: rel_file.to_string(),
        enclosing_item,
        anchor_hash,
    })
}

/// Anchor-scheme version, baked into every advice-fingerprint so any future
/// change to site/anchor hashing semantics is detectable and migratable
/// instead of silently invalidating the cross-session ledger (the v1→v2
/// trimmed-span fix did that once, by necessity — this tag makes it the
/// last silent one).
const ANCHOR_SCHEME_VERSION: &str = "as2";

/// D11's dedup key: engine-computed `(concept_id, site)` (C2).
pub fn advice_fingerprint(concept: &str, site: &Site) -> String {
    let raw = format!(
        "{}|{}|{}|{}|{}",
        ANCHOR_SCHEME_VERSION, concept, site.file, site.enclosing_item, site.anchor_hash
    );
    crate::sha256::sha256_hex(raw.as_bytes())
}

/// Enumerates every "anchor-candidate" node inside `node`'s subtree — every
/// node whose immediate parent's kind is a container kind (per the pack's
/// grammar). This is exactly the set [`find_anchor_node`] can ever return
/// (for ANY leaf position within this subtree), so scanning it for
/// `target_hash` answers "is this anchor present ANYWHERE in this item"
/// without depending on a specific (possibly now-stale) line number.
fn anchor_hash_present(node: Node, source: &str, target_hash: &str, grammar: &GrammarSpec) -> bool {
    if let Some(parent) = node.parent() {
        if is_container_kind(parent.kind(), grammar) {
            let text = normalize_whitespace(node_text(node, source));
            if crate::sha256::sha256_hex(text.as_bytes()) == target_hash {
                return true;
            }
        }
    }
    let mut cursor = node.walk();
    if cursor.goto_first_child() {
        loop {
            if anchor_hash_present(cursor.node(), source, target_hash, grammar) {
                return true;
            }
            if !cursor.goto_next_sibling() {
                break;
            }
        }
    }
    false
}

/// Finds the item in `root` whose [`item_name`] equals `target_name` — a
/// name-based lookup (as opposed to [`find_enclosing_item`]'s
/// position-based one), used to relocate a site without trusting a stored
/// line number that may have shifted.
fn find_item_by_name<'a>(
    root: Node<'a>,
    source: &str,
    target_name: &str,
    grammar: &GrammarSpec,
) -> Option<Node<'a>> {
    if is_item_kind(root.kind(), grammar) && item_name(root, source, grammar) == target_name {
        return Some(root);
    }
    let mut cursor = root.walk();
    if cursor.goto_first_child() {
        loop {
            if let Some(found) = find_item_by_name(cursor.node(), source, target_name, grammar) {
                return Some(found);
            }
            if !cursor.goto_next_sibling() {
                break;
            }
        }
    }
    None
}

/// T4 req 1's mechanical applied-detection, relocated by item identity
/// rather than line number (fix for the gating review finding: an edit
/// ABOVE the site shifts its line, which made the old line-pinned recompute
/// see a different statement and falsely call it "applied" while the
/// flagged code was still there).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SiteRecheckOutcome {
    /// The stored anchor is still present SOMEWHERE in the enclosing item —
    /// the flagged pattern hasn't been fixed (regardless of which line it's
    /// now on).
    StillPresent,
    /// The enclosing item exists but the anchor is nowhere in it anymore —
    /// applied.
    Applied,
    /// The enclosing item itself is gone (renamed, per C2 "an item rename
    /// retires the site") — NOT evidence of a fix; the caller should expire
    /// the card, not mark it applied.
    ItemGone,
}

/// Re-locates the card's site by scanning its stored ENCLOSING ITEM (by
/// name, found anywhere in `current_source`) for the stored `anchor_hash`.
/// Fails safe: a parse failure never claims the anchor is gone.
pub fn recheck_site_in_enclosing_item(
    current_source: &str,
    stored_enclosing_item: &str,
    stored_anchor_hash: &str,
    grammar: &GrammarSpec,
) -> SiteRecheckOutcome {
    let Some(mut parser) = make_parser(grammar) else {
        return SiteRecheckOutcome::StillPresent;
    };
    let Some(tree) = parser.parse(current_source, None) else {
        return SiteRecheckOutcome::StillPresent;
    };
    let root = tree.root_node();

    let Some(item_node) = find_item_by_name(root, current_source, stored_enclosing_item, grammar)
    else {
        return SiteRecheckOutcome::ItemGone;
    };

    if anchor_hash_present(item_node, current_source, stored_anchor_hash, grammar) {
        SiteRecheckOutcome::StillPresent
    } else {
        SiteRecheckOutcome::Applied
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test-only stand-in for a loaded pack grammar — matches the bundled
    /// pack's own grammar reference exactly (see
    /// `pack::tests::test_grammar_default_matches_loaded_pack`).
    fn grammar() -> GrammarSpec {
        GrammarSpec::default()
    }

    #[test]
    fn test_parses_without_errors_valid() {
        assert!(parses_without_errors(
            "fn main() {\n    let x = 1;\n}\n",
            &grammar()
        ));
    }

    #[test]
    fn test_parses_without_errors_invalid() {
        assert!(!parses_without_errors(
            "fn main() {\n    let x = 1;\n",
            &grammar()
        ));
    }

    #[test]
    fn test_enclosing_item_function() {
        let src = "fn foo() {\n    let x = y.clone();\n}\n";
        let site = compute_site("src/lib.rs", src, 2, &grammar()).unwrap();
        assert_eq!(site.enclosing_item, "fn foo");
    }

    #[test]
    fn test_enclosing_item_struct() {
        let src = "struct Point {\n    x: i32,\n    y: i32,\n}\n";
        let site = compute_site("src/lib.rs", src, 2, &grammar()).unwrap();
        assert_eq!(site.enclosing_item, "struct Point");
    }

    #[test]
    fn test_enclosing_item_impl() {
        let src = "struct Point;\nimpl Point {\n    fn new() -> Self { Point }\n}\n";
        // Line 2 ("impl Point {") is the impl's own header, not inside the
        // nested `fn new` — the smallest enclosing item is the impl itself.
        let site = compute_site("src/lib.rs", src, 2, &grammar()).unwrap();
        assert_eq!(site.enclosing_item, "impl Point");
    }

    /// A position genuinely INSIDE the nested fn resolves to that fn — the
    /// smallest enclosing item, not the outer impl (locate_leaf's gating
    /// fix: a raw-line-span leaf lookup used to bubble past nested items).
    #[test]
    fn test_enclosing_item_impl_nested_fn_is_the_smallest_enclosing_item() {
        let src = "struct Point;\nimpl Point {\n    fn new() -> Self { Point }\n}\n";
        let site = compute_site("src/lib.rs", src, 3, &grammar()).unwrap();
        assert_eq!(site.enclosing_item, "fn new");
    }

    #[test]
    fn test_enclosing_item_mod() {
        let src = "mod things {\n    fn helper() {}\n}\n";
        // Line 1 ("mod things {") is the mod's own header, not inside the
        // nested `fn helper` — the smallest enclosing item is the mod itself.
        let site = compute_site("src/lib.rs", src, 1, &grammar()).unwrap();
        assert_eq!(site.enclosing_item, "mod things");
    }

    /// A position genuinely INSIDE the nested fn resolves to that fn — the
    /// smallest enclosing item, not the outer mod.
    #[test]
    fn test_enclosing_item_mod_nested_fn_is_the_smallest_enclosing_item() {
        let src = "mod things {\n    fn helper() {}\n}\n";
        let site = compute_site("src/lib.rs", src, 2, &grammar()).unwrap();
        assert_eq!(site.enclosing_item, "fn helper");
    }

    #[test]
    fn test_no_enclosing_item_at_top_level() {
        let src = "use std::fmt;\n";
        assert!(compute_site("src/lib.rs", src, 1, &grammar()).is_none());
    }

    #[test]
    fn test_enclosing_item_text_returns_full_function() {
        let src = "fn foo() {\n    let x = y.clone();\n    println!(\"{}\", x);\n}\n";
        let text = enclosing_item_text(src, 2, &grammar()).unwrap();
        assert_eq!(
            text,
            "fn foo() {\n    let x = y.clone();\n    println!(\"{}\", x);\n}"
        );
    }

    #[test]
    fn test_advice_fingerprint_identity_across_line_shift() {
        let src_a = "fn foo() {\n    let x = y.clone();\n    println!(\"{}\", x);\n}\n";
        let src_b = "// a comment\n// another comment\n\nfn foo() {\n    let x = y.clone();\n    println!(\"{}\", x);\n}\n";

        let site_a = compute_site("src/lib.rs", src_a, 2, &grammar()).unwrap();
        // Same statement, shifted down by 3 lines in src_b.
        let site_b = compute_site("src/lib.rs", src_b, 5, &grammar()).unwrap();

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

        let site_a = compute_site("src/lib.rs", src_a, 2, &grammar()).unwrap();
        let site_b = compute_site("src/lib.rs", src_b, 2, &grammar()).unwrap();

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
        let site = compute_site("src/lib.rs", src, 2, &grammar()).unwrap();
        let fp_a = advice_fingerprint("borrow-vs-clone", &site);
        let fp_b = advice_fingerprint("string-vs-str", &site);
        assert_ne!(fp_a, fp_b);
    }

    // --- T4 req 1 (gating fix): mechanical applied-detection, relocated by
    // enclosing-item identity rather than a possibly-stale line number ---

    /// (i) An edit ABOVE the site shifts its line number, but the anchor
    /// itself (and the enclosing item) is untouched — must NOT be applied.
    /// This is exactly the false-positive the line-pinned recompute used to
    /// produce.
    #[test]
    fn test_recheck_not_applied_when_edit_above_shifts_the_line() {
        let before = "fn foo(name: String) {\n    let x = name.clone();\n}\n";
        let site_before = compute_site("src/lib.rs", before, 2, &grammar()).unwrap();

        // Several lines inserted ABOVE `fn foo` — the flagged statement is
        // now on a different line, but its text (and the item) are unchanged.
        let after = "// a\n// b\n// c\n// d\n\nfn foo(name: String) {\n    let x = name.clone();\n}\n";

        let outcome = recheck_site_in_enclosing_item(
            after,
            &site_before.enclosing_item,
            &site_before.anchor_hash,
            &grammar(),
        );
        assert_eq!(outcome, SiteRecheckOutcome::StillPresent);
    }

    /// (ii) The anchor is genuinely removed (the fix landed) — applied.
    #[test]
    fn test_recheck_applied_when_anchor_genuinely_removed() {
        let before = "fn foo(name: String) {\n    let x = name.clone();\n}\n";
        let site_before = compute_site("src/lib.rs", before, 2, &grammar()).unwrap();

        let after = "fn foo(name: String) {\n    let x = &name;\n}\n";
        let outcome = recheck_site_in_enclosing_item(
            after,
            &site_before.enclosing_item,
            &site_before.anchor_hash,
            &grammar(),
        );
        assert_eq!(outcome, SiteRecheckOutcome::Applied);
    }

    /// (iii) The enclosing item itself was renamed — C2 retires the site;
    /// this must be reported distinctly (ItemGone), not as "applied".
    #[test]
    fn test_recheck_item_gone_when_enclosing_item_renamed() {
        let before = "fn foo(name: String) {\n    let x = name.clone();\n}\n";
        let site_before = compute_site("src/lib.rs", before, 2, &grammar()).unwrap();

        let after = "fn bar(name: String) {\n    let x = name.clone();\n}\n";
        let outcome = recheck_site_in_enclosing_item(
            after,
            &site_before.enclosing_item,
            &site_before.anchor_hash,
            &grammar(),
        );
        assert_eq!(outcome, SiteRecheckOutcome::ItemGone);
    }

    #[test]
    fn test_recheck_still_present_even_when_edit_is_elsewhere_in_same_item() {
        let before =
            "fn foo(name: String) {\n    let x = name.clone();\n    let y = 1;\n}\n";
        let site_before = compute_site("src/lib.rs", before, 2, &grammar()).unwrap();

        // Edit a DIFFERENT statement in the same item; the flagged one is
        // untouched (and, incidentally, shifted zero lines here — the real
        // regression case is covered by the "edit above" test).
        let after =
            "fn foo(name: String) {\n    let x = name.clone();\n    let y = 2;\n}\n";
        let outcome = recheck_site_in_enclosing_item(
            after,
            &site_before.enclosing_item,
            &site_before.anchor_hash,
            &grammar(),
        );
        assert_eq!(outcome, SiteRecheckOutcome::StillPresent);
    }

    #[test]
    fn test_recheck_still_present_when_anchor_moved_to_a_different_line_in_same_item() {
        // The anchor itself is still somewhere in the item, just not on the
        // originally-stored line (e.g. reordered statements) — must still
        // read as present, not applied.
        let before = "fn foo(name: String) {\n    let x = name.clone();\n    let y = 1;\n}\n";
        let site_before = compute_site("src/lib.rs", before, 2, &grammar()).unwrap();

        let after = "fn foo(name: String) {\n    let y = 1;\n    let x = name.clone();\n}\n";
        let outcome = recheck_site_in_enclosing_item(
            after,
            &site_before.enclosing_item,
            &site_before.anchor_hash,
            &grammar(),
        );
        assert_eq!(outcome, SiteRecheckOutcome::StillPresent);
    }
}
