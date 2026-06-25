use std::collections::HashMap;
use std::fs;
use std::io::BufRead;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeDefinition {
    pub type_name: String,
    pub file_path: PathBuf,
    pub definition_block: String,
}

pub fn get_line_span(file_path: &Path, error_line: usize) -> Result<String, String> {
    let file = fs::File::open(file_path).map_err(|e| e.to_string())?;
    let reader = std::io::BufReader::new(file);
    let lines: Vec<String> = reader.lines().map_while(Result::ok).collect();

    if lines.is_empty() {
        return Ok(String::new());
    }

    let mid = if error_line > 0 { error_line - 1 } else { 0 };
    let mid = std::cmp::min(mid, lines.len() - 1);
    let start = mid.saturating_sub(25);
    let end = std::cmp::min(mid + 25, lines.len() - 1);

    let span_lines = lines[start..=end].to_vec();
    Ok(span_lines.join("\n"))
}

pub fn sanitize_xml(content: &str) -> String {
    content
        .replace("&", "&amp;")
        .replace("<", "&lt;")
        .replace(">", "&gt;")
}

pub fn index_workspace_types(root: &Path, exclude: &[String]) -> HashMap<String, TypeDefinition> {
    let mut index = HashMap::new();
    index_types_recursive(root, exclude, &mut index);
    index
}

fn index_types_recursive(
    dir: &Path,
    exclude: &[String],
    index: &mut HashMap<String, TypeDefinition>,
) {
    if crate::watcher::is_excluded(dir, exclude) {
        return;
    }
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if crate::watcher::is_excluded(&path, exclude) {
                continue;
            }
            if let Ok(metadata) = entry.metadata() {
                if metadata.is_dir() {
                    index_types_recursive(&path, exclude, index);
                } else if metadata.is_file() && path.extension().is_some_and(|ext| ext == "rs") {
                    if let Ok(content) = fs::read_to_string(&path) {
                        extract_types_from_file(&content, &path, index);
                    }
                }
            }
        }
    }
}

fn extract_types_from_file(
    content: &str,
    path: &Path,
    index: &mut HashMap<String, TypeDefinition>,
) {
    let chars: Vec<char> = content.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        if chars[i].is_whitespace() {
            i += 1;
            continue;
        }

        let keywords = ["struct", "enum", "trait", "type"];
        let mut found_keyword = None;

        for &kw in &keywords {
            if has_keyword_at(&chars, i, kw) {
                found_keyword = Some(kw);
                break;
            }
        }

        if let Some(kw) = found_keyword {
            let _start_index = i;
            i += kw.len();

            while i < chars.len() && chars[i].is_whitespace() {
                i += 1;
            }

            let mut name = String::new();
            while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                name.push(chars[i]);
                i += 1;
            }

            if !name.is_empty() {
                let mut block = String::new();
                block.push_str(kw);
                block.push(' ');
                block.push_str(&name);

                let mut brace_count = 0;
                let mut found_brace = false;
                let mut found_semicolon = false;

                let mut j = i;
                while j < chars.len() {
                    let c = chars[j];
                    block.push(c);
                    j += 1;

                    if c == '{' {
                        brace_count += 1;
                        found_brace = true;
                    } else if c == '}' {
                        brace_count -= 1;
                        if brace_count == 0 && found_brace {
                            break;
                        }
                    } else if c == ';' && !found_brace {
                        found_semicolon = true;
                        break;
                    }
                }

                if found_brace || found_semicolon {
                    index.insert(
                        name.clone(),
                        TypeDefinition {
                            type_name: name,
                            file_path: path.to_path_buf(),
                            definition_block: block.trim().to_string(),
                        },
                    );
                }
                i = j;
            }
        } else {
            i += 1;
        }
    }
}

fn has_keyword_at(chars: &[char], i: usize, kw: &str) -> bool {
    if i + kw.len() > chars.len() {
        return false;
    }
    for (idx, c) in kw.chars().enumerate() {
        if chars[i + idx] != c {
            return false;
        }
    }
    let next_idx = i + kw.len();
    if next_idx < chars.len() {
        let next_c = chars[next_idx];
        next_c.is_whitespace() || next_c == '<' || next_c == '(' || next_c == '{' || next_c == ';'
    } else {
        true
    }
}

pub fn find_referenced_types(
    error_message: &str,
    code_context: &str,
    type_index: &HashMap<String, TypeDefinition>,
) -> Vec<TypeDefinition> {
    let mut referenced = Vec::new();
    let mut seen = std::collections::HashSet::new();

    let mut identifiers = std::collections::HashSet::new();
    collect_identifiers(error_message, &mut identifiers);
    collect_identifiers(code_context, &mut identifiers);

    for id in identifiers {
        if let Some(def) = type_index.get(&id) {
            if seen.insert(id) {
                referenced.push(def.clone());
            }
        }
    }

    // Sort to keep output deterministic
    referenced.sort_by(|a, b| a.type_name.cmp(&b.type_name));
    referenced
}

fn collect_identifiers(text: &str, set: &mut std::collections::HashSet<String>) {
    let mut current = String::new();
    for c in text.chars() {
        if c.is_alphanumeric() || c == '_' {
            current.push(c);
        } else if !current.is_empty() {
            set.insert(current.clone());
            current.clear();
        }
    }
    if !current.is_empty() {
        set.insert(current);
    }
}

pub fn generate_context_payload(
    project_root: &Path,
    error_file: &Path,
    error_line: usize,
    error_message: &str,
    exclude_patterns: &[String],
) -> Result<String, String> {
    let raw_code_context = get_line_span(error_file, error_line)?;
    let type_index = index_workspace_types(project_root, exclude_patterns);
    let referenced_types = find_referenced_types(error_message, &raw_code_context, &type_index);

    let mut payload = String::new();
    payload.push_str("<developer_code_context>\n");

    payload.push_str("  <file_path>");
    payload.push_str(&sanitize_xml(&error_file.to_string_lossy()));
    payload.push_str("</file_path>\n");

    payload.push_str("  <error_line>");
    payload.push_str(&error_line.to_string());
    payload.push_str("</error_line>\n");

    payload.push_str("  <code_span>\n");
    payload.push_str(&sanitize_xml(&raw_code_context));
    payload.push_str("\n  </code_span>\n");

    if !referenced_types.is_empty() {
        payload.push_str("  <referenced_types>\n");
        for def in referenced_types {
            payload.push_str("    <type_definition>\n");
            payload.push_str("      <name>");
            payload.push_str(&sanitize_xml(&def.type_name));
            payload.push_str("</name>\n");
            payload.push_str("      <definition>\n");
            payload.push_str(&sanitize_xml(&def.definition_block));
            payload.push_str("\n      </definition>\n");
            payload.push_str("    </type_definition>\n");
        }
        payload.push_str("  </referenced_types>\n");
    }

    payload.push_str("</developer_code_context>");

    Ok(payload)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_sanitize_xml_injection() {
        let input = "<tag attribute=\"value\">Hello & World</tag>";
        let expected = "&lt;tag attribute=\"value\"&gt;Hello &amp; World&lt;/tag&gt;";
        assert_eq!(sanitize_xml(input), expected);
    }

    #[test]
    fn test_get_line_span_boundaries() {
        let temp_dir = std::env::temp_dir();
        let test_file = temp_dir.join("test_get_line_span.rs");

        // Write a mock file with 100 lines
        let lines: Vec<String> = (1..=100).map(|i| format!("Line {}", i)).collect();
        fs::write(&test_file, lines.join("\n")).unwrap();

        // Test error line 1 (1-indexed). Expected: lines 1 to 26 (0 to 25)
        let span1 = get_line_span(&test_file, 1).unwrap();
        let expected1: Vec<String> = (1..=26).map(|i| format!("Line {}", i)).collect();
        assert_eq!(span1, expected1.join("\n"));

        // Test error line 50. Expected: lines 25 to 75 (mid = 49, start = 24, end = 74 -> lines 25 to 75)
        let span50 = get_line_span(&test_file, 50).unwrap();
        let expected50: Vec<String> = (25..=75).map(|i| format!("Line {}", i)).collect();
        assert_eq!(span50, expected50.join("\n"));

        // Test error line 100. Expected: lines 75 to 100
        let span100 = get_line_span(&test_file, 100).unwrap();
        let expected100: Vec<String> = (75..=100).map(|i| format!("Line {}", i)).collect();
        assert_eq!(span100, expected100.join("\n"));

        // Cleanup
        let _ = fs::remove_file(&test_file);
    }

    #[test]
    fn test_extract_types_from_file() {
        let content = r#"
            struct MyStruct {
                a: i32,
            }

            pub enum MyEnum {
                VariantA,
            }

            trait MyTrait {
                fn run(&self);
            }

            type MyAlias = Result<String, MyStruct>;
        "#;

        let mut index = HashMap::new();
        let path = PathBuf::from("mock.rs");
        extract_types_from_file(content, &path, &mut index);

        assert!(index.contains_key("MyStruct"));
        assert!(index.contains_key("MyEnum"));
        assert!(index.contains_key("MyTrait"));
        assert!(index.contains_key("MyAlias"));

        assert_eq!(
            index.get("MyStruct").unwrap().definition_block,
            "struct MyStruct {\n                a: i32,\n            }"
        );
        assert_eq!(
            index.get("MyEnum").unwrap().definition_block,
            "enum MyEnum {\n                VariantA,\n            }"
        );
    }

    #[test]
    fn test_find_referenced_types() {
        let mut index = HashMap::new();
        index.insert(
            "MyStruct".to_string(),
            TypeDefinition {
                type_name: "MyStruct".to_string(),
                file_path: PathBuf::from("mock.rs"),
                definition_block: "struct MyStruct {}".to_string(),
            },
        );
        index.insert(
            "MyEnum".to_string(),
            TypeDefinition {
                type_name: "MyEnum".to_string(),
                file_path: PathBuf::from("mock.rs"),
                definition_block: "enum MyEnum {}".to_string(),
            },
        );

        let error_message = "error: MyStruct is not found";
        let code_context = "fn foo(x: MyEnum) {}";

        let referenced = find_referenced_types(error_message, code_context, &index);
        assert_eq!(referenced.len(), 2);
        assert_eq!(referenced[0].type_name, "MyEnum");
        assert_eq!(referenced[1].type_name, "MyStruct");
    }

    #[test]
    fn test_generate_context_payload() {
        let temp_dir = std::env::temp_dir();
        let test_root = temp_dir.join("murshid_test_ctx_root");
        let src_dir = test_root.join("src");
        fs::create_dir_all(&src_dir).unwrap();

        let lib_rs = src_dir.join("lib.rs");
        let code = r#"
            pub struct Config {
                pub port: u16,
            }
            
            pub fn run(cfg: Config) {
                // error line is here
                println!("hello");
            }
        "#;
        fs::write(&lib_rs, code).unwrap();

        let payload = generate_context_payload(
            &test_root,
            &lib_rs,
            8,
            "error: struct Config layout mismatch",
            &[],
        )
        .unwrap();

        // Check tags are correctly nested
        assert!(payload.starts_with("<developer_code_context>"));
        assert!(payload.contains("<file_path>"));
        assert!(payload.contains("<error_line>8</error_line>"));
        assert!(payload.contains("<code_span>"));
        assert!(payload.contains("<referenced_types>"));
        assert!(payload.contains("<name>Config</name>"));
        assert!(payload.contains("      <definition>\nstruct Config {\n                pub port: u16,\n            }\n      </definition>"));
        assert!(payload.ends_with("</developer_code_context>"));

        // Cleanup
        let _ = fs::remove_dir_all(&test_root);
    }
}
