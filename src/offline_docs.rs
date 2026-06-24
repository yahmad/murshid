use std::collections::HashSet;
use std::path::PathBuf;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct ReferenceDoc {
    pub error_code: String,
    pub title: String,
    pub summary: String,
    pub doc_url: String,
    pub tokens: Vec<String>,
}

pub fn get_static_references() -> Vec<ReferenceDoc> {
    vec![
        ReferenceDoc {
            error_code: "E0382".to_string(),
            title: "Use of moved value".to_string(),
            summary: "This error occurs when trying to use a value after it has been moved, transferring ownership.".to_string(),
            doc_url: "https://doc.rust-lang.org/error_codes/E0382.html".to_string(),
            tokens: vec!["use".to_string(), "moved".to_string(), "value".to_string(), "ownership".to_string(), "transfer".to_string()],
        },
        ReferenceDoc {
            error_code: "E0499".to_string(),
            title: "Cannot borrow as mutable more than once".to_string(),
            summary: "This error occurs when trying to borrow a variable as mutable more than once at the same time.".to_string(),
            doc_url: "https://doc.rust-lang.org/error_codes/E0499.html".to_string(),
            tokens: vec!["borrow".to_string(), "mutable".to_string(), "multiple".to_string(), "aliasing".to_string()],
        },
        ReferenceDoc {
            error_code: "E0507".to_string(),
            title: "Cannot move out of a shared reference".to_string(),
            summary: "This error occurs when trying to move a value out of a shared (immutable) reference.".to_string(),
            doc_url: "https://doc.rust-lang.org/error_codes/E0507.html".to_string(),
            tokens: vec!["move".to_string(), "shared".to_string(), "reference".to_string(), "immutable".to_string()],
        },
        ReferenceDoc {
            error_code: "E0502".to_string(),
            title: "Cannot borrow as mutable because it is also borrowed as immutable".to_string(),
            summary: "This error occurs when trying to borrow a variable as mutable while it is already borrowed as immutable.".to_string(),
            doc_url: "https://doc.rust-lang.org/error_codes/E0502.html".to_string(),
            tokens: vec!["borrow".to_string(), "mutable".to_string(), "immutable".to_string(), "conflict".to_string()],
        },
        ReferenceDoc {
            error_code: "E0106".to_string(),
            title: "Lifetime parameter expected".to_string(),
            summary: "This error occurs when a lifetime is required in a type signature but has been omitted or cannot be inferred.".to_string(),
            doc_url: "https://doc.rust-lang.org/error_codes/E0106.html".to_string(),
            tokens: vec!["lifetime".to_string(), "parameter".to_string(), "expected".to_string(), "signature".to_string(), "elision".to_string()],
        },
    ]
}

pub fn get_cache_file_path() -> Option<PathBuf> {
    crate::config::get_home_dir().map(|h| {
        #[cfg(target_os = "macos")]
        {
            h.join("Library/Application Support/murshid/reference_docs.json")
        }
        #[cfg(target_os = "windows")]
        {
            h.join("AppData\\Roaming\\murshid\\reference_docs.json")
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        {
            h.join(".config/murshid/reference_docs.json")
        }
    })
}

pub fn load_references() -> Vec<ReferenceDoc> {
    if let Some(path) = get_cache_file_path() {
        if path.exists() {
            if let Ok(content) = std::fs::read_to_string(&path) {
                if let Ok(docs) = serde_json::from_str::<Vec<ReferenceDoc>>(&content) {
                    return docs;
                }
            }
        } else {
            let defaults = get_static_references();
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Ok(json) = serde_json::to_string_pretty(&defaults) {
                let _ = std::fs::write(&path, json);
            }
        }
    }
    get_static_references()
}

pub fn lookup_error_code(error_code: &str) -> Option<ReferenceDoc> {
    load_references()
        .into_iter()
        .find(|r| r.error_code == error_code)
}

pub fn tokenize(text: &str) -> HashSet<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_lowercase())
        .collect()
}

pub fn calculate_jaccard(set1: &HashSet<String>, set2: &HashSet<String>) -> f64 {
    let intersection: HashSet<_> = set1.intersection(set2).cloned().collect();
    let union: HashSet<_> = set1.union(set2).cloned().collect();
    if union.is_empty() {
        0.0
    } else {
        intersection.len() as f64 / union.len() as f64
    }
}

pub fn search_references(query: &str) -> Option<(ReferenceDoc, f64)> {
    let query_tokens = tokenize(query);
    if query_tokens.is_empty() {
        return None;
    }

    let mut best_match: Option<(ReferenceDoc, f64)> = None;

    for doc in load_references() {
        let doc_tokens: HashSet<String> = doc.tokens.iter().cloned().collect();
        let sim = calculate_jaccard(&query_tokens, &doc_tokens);
        if sim > 0.0 {
            if let Some((_, best_sim)) = best_match {
                if sim > best_sim {
                    best_match = Some((doc, sim));
                }
            } else {
                best_match = Some((doc, sim));
            }
        }
    }

    best_match
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lookup_error_code() {
        let doc = lookup_error_code("E0382").unwrap();
        assert_eq!(doc.title, "Use of moved value");
        assert!(lookup_error_code("E9999").is_none());
    }

    #[test]
    fn test_tokenize() {
        let text = "Use of a moved value!";
        let tokens = tokenize(text);
        assert!(tokens.contains("use"));
        assert!(tokens.contains("moved"));
        assert!(tokens.contains("value"));
    }

    #[test]
    fn test_calculate_jaccard() {
        let mut set1 = HashSet::new();
        set1.insert("moved".to_string());
        set1.insert("value".to_string());

        let mut set2 = HashSet::new();
        set2.insert("moved".to_string());
        set2.insert("ownership".to_string());

        let sim = calculate_jaccard(&set1, &set2);
        // Intersection is {"moved"} (1 element). Union is {"moved", "value", "ownership"} (3 elements).
        // Jaccard similarity is 1 / 3 = 0.3333333333333333
        assert!((sim - 0.3333333).abs() < 1e-5);
    }

    #[test]
    fn test_jaccard_fallback_search() {
        // Query containing "ownership transfer" should match E0382 because it contains both "ownership" and "transfer" tokens in its summary tokens
        let (doc, sim) = search_references("how does ownership transfer work?").unwrap();
        assert_eq!(doc.error_code, "E0382");
        assert!(sim > 0.0);
    }
}
