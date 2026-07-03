//! Line-oriented unified diff (C2 session diff / I1 advice window).
//!
//! Standard-library only LCS-based diff. Produces unified hunks; advice (per
//! I1) attaches only to changed (added) lines, never to context.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffOp {
    Context {
        old_line: usize,
        new_line: usize,
        text: String,
    },
    Added {
        new_line: usize,
        text: String,
    },
    Removed {
        old_line: usize,
        text: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    pub old_start: usize,
    pub old_lines: usize,
    pub new_start: usize,
    pub new_lines: usize,
    pub ops: Vec<DiffOp>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RawOp {
    Same,
    Added,
    Removed,
}

/// Number of context lines kept around a changed region when grouping into
/// hunks.
const CONTEXT: usize = 1;

fn lcs_table(old: &[&str], new: &[&str]) -> Vec<Vec<u32>> {
    let n = old.len();
    let m = new.len();
    let mut table = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            table[i][j] = if old[i] == new[j] {
                table[i + 1][j + 1] + 1
            } else {
                table[i + 1][j].max(table[i][j + 1])
            };
        }
    }
    table
}

/// Computes the raw (old_idx, new_idx, op) edit script between `old` and
/// `new` line slices, using an LCS backtrack.
fn edit_script(old: &[&str], new: &[&str]) -> Vec<(Option<usize>, Option<usize>, RawOp)> {
    let table = lcs_table(old, new);
    let mut ops = Vec::new();
    let (mut i, mut j) = (0usize, 0usize);
    let n = old.len();
    let m = new.len();

    while i < n && j < m {
        if old[i] == new[j] {
            ops.push((Some(i), Some(j), RawOp::Same));
            i += 1;
            j += 1;
        } else if table[i + 1][j] >= table[i][j + 1] {
            ops.push((Some(i), None, RawOp::Removed));
            i += 1;
        } else {
            ops.push((None, Some(j), RawOp::Added));
            j += 1;
        }
    }
    while i < n {
        ops.push((Some(i), None, RawOp::Removed));
        i += 1;
    }
    while j < m {
        ops.push((None, Some(j), RawOp::Added));
        j += 1;
    }
    ops
}

/// Computes unified diff hunks between `old` and `new` content, line-oriented.
pub fn diff_lines(old: &str, new: &str) -> Vec<Hunk> {
    let old_lines: Vec<&str> = if old.is_empty() {
        Vec::new()
    } else {
        old.lines().collect()
    };
    let new_lines: Vec<&str> = if new.is_empty() {
        Vec::new()
    } else {
        new.lines().collect()
    };

    let raw_ops = edit_script(&old_lines, &new_lines);

    // Find indices (into raw_ops) of non-Same ops.
    let change_indices: Vec<usize> = raw_ops
        .iter()
        .enumerate()
        .filter(|(_, (_, _, op))| *op != RawOp::Same)
        .map(|(idx, _)| idx)
        .collect();

    if change_indices.is_empty() {
        return Vec::new();
    }

    // Group change indices into clusters where consecutive changes are within
    // 2*CONTEXT of each other (so their context windows overlap/touch).
    let mut clusters: Vec<(usize, usize)> = Vec::new();
    let mut cluster_start = change_indices[0];
    let mut cluster_end = change_indices[0];
    for &idx in &change_indices[1..] {
        if idx > cluster_end + 2 * CONTEXT {
            clusters.push((cluster_start, cluster_end));
            cluster_start = idx;
        }
        cluster_end = idx;
    }
    clusters.push((cluster_start, cluster_end));

    let mut hunks = Vec::new();
    for (start, end) in clusters {
        let range_start = start.saturating_sub(CONTEXT);
        let range_end = std::cmp::min(end + CONTEXT, raw_ops.len() - 1);

        let mut ops = Vec::new();
        let mut old_start = None;
        let mut new_start = None;
        let mut old_lines_count = 0usize;
        let mut new_lines_count = 0usize;

        for raw in &raw_ops[range_start..=range_end] {
            match raw {
                (Some(oi), Some(ni), RawOp::Same) => {
                    if old_start.is_none() {
                        old_start = Some(*oi + 1);
                    }
                    if new_start.is_none() {
                        new_start = Some(*ni + 1);
                    }
                    old_lines_count += 1;
                    new_lines_count += 1;
                    ops.push(DiffOp::Context {
                        old_line: *oi + 1,
                        new_line: *ni + 1,
                        text: old_lines[*oi].to_string(),
                    });
                }
                (Some(oi), None, RawOp::Removed) => {
                    if old_start.is_none() {
                        old_start = Some(*oi + 1);
                    }
                    old_lines_count += 1;
                    ops.push(DiffOp::Removed {
                        old_line: *oi + 1,
                        text: old_lines[*oi].to_string(),
                    });
                }
                (None, Some(ni), RawOp::Added) => {
                    if new_start.is_none() {
                        new_start = Some(*ni + 1);
                    }
                    new_lines_count += 1;
                    ops.push(DiffOp::Added {
                        new_line: *ni + 1,
                        text: new_lines[*ni].to_string(),
                    });
                }
                _ => unreachable!("invalid raw op combination"),
            }
        }

        hunks.push(Hunk {
            old_start: old_start.unwrap_or(0),
            old_lines: old_lines_count,
            new_start: new_start.unwrap_or(0),
            new_lines: new_lines_count,
            ops,
        });
    }

    hunks
}

/// T2 review fix / C6 "unchanged... never re-judged": a content signature of
/// a hunk set. When a file's session-diff hunks are byte-identical to the
/// last time this session dispatched stage-1 for it, nothing new could have
/// been learned — safely determinable without an LLM call, so the caller
/// can skip re-dispatching stage-1 instead of re-burning the screen model.
pub fn hunks_signature(hunks: &[Hunk]) -> String {
    crate::sha256::sha256_hex(format!("{:?}", hunks).as_bytes())
}

/// Returns the (new-file, 1-indexed) line numbers that were added/changed —
/// per I1, advice only ever attaches to these.
pub fn changed_line_numbers(hunks: &[Hunk]) -> Vec<usize> {
    let mut lines = Vec::new();
    for hunk in hunks {
        for op in &hunk.ops {
            if let DiffOp::Added { new_line, .. } = op {
                lines.push(*new_line);
            }
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_no_diff_on_identical_content() {
        let content = "fn main() {\n    println!(\"hi\");\n}\n";
        assert!(diff_lines(content, content).is_empty());
    }

    #[test]
    fn test_single_line_change() {
        let old = "fn main() {\n    let x = 1;\n    println!(\"{}\", x);\n}\n";
        let new = "fn main() {\n    let x = 2;\n    println!(\"{}\", x);\n}\n";
        let hunks = diff_lines(old, new);
        assert_eq!(hunks.len(), 1);

        let changed = changed_line_numbers(&hunks);
        assert_eq!(changed, vec![2]);

        // The changed line's content should be the new value.
        let added_texts: Vec<&str> = hunks[0]
            .ops
            .iter()
            .filter_map(|op| match op {
                DiffOp::Added { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(added_texts, vec!["    let x = 2;"]);
    }

    #[test]
    fn test_added_line_only() {
        let old = "fn main() {\n}\n";
        let new = "fn main() {\n    println!(\"new\");\n}\n";
        let hunks = diff_lines(old, new);
        let changed = changed_line_numbers(&hunks);
        assert_eq!(changed, vec![2]);
    }

    #[test]
    fn test_two_distant_changes_produce_two_hunks() {
        let old_lines: Vec<String> = (1..=20).map(|i| format!("line {}", i)).collect();
        let mut new_lines = old_lines.clone();
        new_lines[1] = "line 2 CHANGED".to_string();
        new_lines[17] = "line 18 CHANGED".to_string();

        let old = old_lines.join("\n") + "\n";
        let new = new_lines.join("\n") + "\n";

        let hunks = diff_lines(&old, &new);
        assert_eq!(hunks.len(), 2, "distant changes should be separate hunks");
    }

    #[test]
    fn test_unchanged_lines_produce_no_hunks() {
        let old = "a\nb\nc\n";
        let new = "a\nb\nc\n";
        assert!(diff_lines(old, new).is_empty());
    }

    // --- review fix: hunks_signature (dispatch-cap skip-if-unchanged) ---

    #[test]
    fn test_hunks_signature_identical_for_same_hunks() {
        let old = "fn a() {}\n";
        let new = "fn a() {}\nfn b() {}\n";
        let h1 = diff_lines(old, new);
        let h2 = diff_lines(old, new);
        assert_eq!(hunks_signature(&h1), hunks_signature(&h2));
    }

    #[test]
    fn test_hunks_signature_differs_for_different_hunks() {
        let old = "fn a() {}\n";
        let h1 = diff_lines(old, "fn a() {}\nfn b() {}\n");
        let h2 = diff_lines(old, "fn a() {}\nfn c() {}\n");
        assert_ne!(hunks_signature(&h1), hunks_signature(&h2));
    }

    #[test]
    fn test_hunks_signature_empty_is_stable() {
        assert_eq!(hunks_signature(&[]), hunks_signature(&[]));
    }
}
