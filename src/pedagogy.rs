use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SocraticDialogueState {
    pub workspace_hash: String,
    pub file_path_hash: String,
    pub scaffold_level: i32,
    pub consecutive_failures: i32,
    pub repetition_count: i32,
    pub dialogue_context_hash: Option<String>,
}

#[derive(Debug, Clone)]
pub struct DialogueOutcome {
    pub scaffold_level: i32,
    pub consecutive_failures: i32,
    pub threshold: u32,
    pub mastery_score: f64,
    pub ema_score: f64,
}

#[derive(Debug, Clone)]
pub struct CacheEntry {
    pub consecutive_failures: i32,
    pub last_hash: String,
    pub last_seen: Instant,
}

struct FailureCache {
    entries: HashMap<(String, String), CacheEntry>,
}

impl FailureCache {
    fn new() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }

    fn cleanup_expired(&mut self) {
        let now = Instant::now();
        let timeout = Duration::from_secs(300); // 5 minutes
        self.entries.retain(|_, entry| {
            now.duration_since(entry.last_seen) < timeout
        });
    }

    fn evict_oldest(&mut self) {
        let oldest_key = self.entries
            .iter()
            .min_by_key(|(_, entry)| entry.last_seen)
            .map(|(key, _)| key.clone());
            
        if let Some(key) = oldest_key {
            self.entries.remove(&key);
        }
    }
}

fn get_failure_cache() -> &'static Mutex<FailureCache> {
    static CACHE: OnceLock<Mutex<FailureCache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(FailureCache::new()))
}

pub fn get_consecutive_failures(file_path: &str, error_code: &str) -> i32 {
    let mut cache = get_failure_cache().lock().unwrap();
    cache.cleanup_expired();
    
    let key = (file_path.to_string(), error_code.to_string());
    if let Some(entry) = cache.entries.get_mut(&key) {
        entry.last_seen = Instant::now();
        entry.consecutive_failures
    } else {
        0
    }
}

pub fn increment_consecutive_failures(file_path: &str, error_code: &str, current_hash: &str) -> i32 {
    let mut cache = get_failure_cache().lock().unwrap();
    cache.cleanup_expired();
    
    let key = (file_path.to_string(), error_code.to_string());
    
    if let Some(entry) = cache.entries.get_mut(&key) {
        entry.last_seen = Instant::now();
        if entry.last_hash != current_hash {
            entry.consecutive_failures += 1;
            entry.last_hash = current_hash.to_string();
        }
        entry.consecutive_failures
    } else {
        if cache.entries.len() >= 5 {
            cache.evict_oldest();
        }
        let entry = CacheEntry {
            consecutive_failures: 1,
            last_hash: current_hash.to_string(),
            last_seen: Instant::now(),
        };
        cache.entries.insert(key, entry);
        1
    }
}

pub fn reset_consecutive_failures(file_path: &str, error_code: &str) {
    let mut cache = get_failure_cache().lock().unwrap();
    let key = (file_path.to_string(), error_code.to_string());
    cache.entries.remove(&key);
}

pub fn load_dialogue_state(
    conn: &rusqlite::Connection,
    workspace_hash: &str,
    file_path_hash: &str,
) -> rusqlite::Result<Option<SocraticDialogueState>> {
    let mut stmt = conn.prepare(
        "SELECT scaffold_level, consecutive_failures, repetition_count, dialogue_context_hash 
         FROM socratic_dialogues 
         WHERE workspace_hash = ?1 AND file_path_hash = ?2"
    )?;
    let mut rows = stmt.query([workspace_hash, file_path_hash])?;
    if let Some(row) = rows.next()? {
        Ok(Some(SocraticDialogueState {
            workspace_hash: workspace_hash.to_string(),
            file_path_hash: file_path_hash.to_string(),
            scaffold_level: row.get(0)?,
            consecutive_failures: row.get(1)?,
            repetition_count: row.get(2)?,
            dialogue_context_hash: row.get(3)?,
        }))
    } else {
        Ok(None)
    }
}

pub fn save_dialogue_state(conn: &rusqlite::Connection, state: &SocraticDialogueState) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO socratic_dialogues (workspace_hash, file_path_hash, scaffold_level, consecutive_failures, repetition_count, dialogue_context_hash, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, CURRENT_TIMESTAMP)
         ON CONFLICT(workspace_hash, file_path_hash) DO UPDATE SET
            scaffold_level = excluded.scaffold_level,
            consecutive_failures = excluded.consecutive_failures,
            repetition_count = excluded.repetition_count,
            dialogue_context_hash = excluded.dialogue_context_hash,
            updated_at = CURRENT_TIMESTAMP",
        (
            &state.workspace_hash,
            &state.file_path_hash,
            state.scaffold_level,
            state.consecutive_failures,
            state.repetition_count,
            &state.dialogue_context_hash,
        )
    )?;
    Ok(())
}

pub fn load_concept_mastery(conn: &rusqlite::Connection, concept_slug: &str) -> rusqlite::Result<f64> {
    let mut stmt = conn.prepare("SELECT mastery_score FROM concepts WHERE concept_slug = ?1")?;
    let mut rows = stmt.query([concept_slug])?;
    if let Some(row) = rows.next()? {
        let score: f64 = row.get(0)?;
        Ok(score)
    } else {
        Ok(0.5)
    }
}

pub fn save_concept_mastery(conn: &rusqlite::Connection, concept_slug: &str, score: f64) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO concepts (concept_slug, mastery_score, exposure_count, consecutive_successes, last_seen)
         VALUES (?1, ?2, 1, 0, CURRENT_TIMESTAMP)
         ON CONFLICT(concept_slug) DO UPDATE SET
            mastery_score = excluded.mastery_score,
            exposure_count = exposure_count + 1,
            last_seen = CURRENT_TIMESTAMP",
        rusqlite::params![concept_slug, score],
    )?;
    Ok(())
}

pub fn get_pedagogy_coefficients() -> (f64, f64) {
    let mut success_step = 0.20;
    let mut failure_decay = 0.85;
    
    let paths = vec![
        crate::config::get_system_config_path(),
        crate::config::resolve_user_config_path().unwrap_or_default(),
        crate::config::get_project_config_path().unwrap_or_default(),
    ];
    
    for path in paths {
        if path.exists() {
            if let Ok(content) = std::fs::read_to_string(&path) {
                let parsed = crate::config::parse_toml(&content);
                if let Some(section) = parsed.get("pedagogy.coefficients") {
                    if let Some(val) = section.get("success_step") {
                        if let Ok(num) = val.parse::<f64>() {
                            success_step = num;
                        }
                    }
                    if let Some(val) = section.get("failure_decay") {
                        if let Ok(num) = val.parse::<f64>() {
                            failure_decay = num;
                        }
                    }
                }
            }
        }
    }
    
    (success_step, failure_decay)
}

pub fn map_error_to_concept(error_code: &str) -> &str {
    match error_code {
        "E0382" | "E0505" | "E0507" | "E0508" | "E0509" => "ownership",
        "E0502" | "E0499" | "E0503" | "E0506" => "borrowing",
        "E0106" | "E0515" | "E0521" | "E0597" | "E0621" | "E0726" => "lifetimes",
        _ => "ownership",
    }
}

pub fn calculate_threshold(s_t: f64) -> u32 {
    let t_base = 3.0;
    let t_max = 7.0;
    let raw_t = t_base + (1.0 - s_t) * (t_max - t_base);
    let rounded_t = raw_t.round() as u32;
    rounded_t.clamp(3, 7)
}

pub fn get_normalized_content(content: &str) -> String {
    let chars: Vec<char> = content.chars().collect();
    let mut normalized = String::new();
    let mut i = 0;
    let mut in_string = false;
    let mut in_char = false;
    
    while i < chars.len() {
        let c = chars[i];
        
        if in_string {
            if c == '"' && i > 0 && chars[i-1] != '\\' {
                in_string = false;
            }
            if !c.is_whitespace() {
                normalized.push(c);
            }
            i += 1;
            continue;
        }
        
        if in_char {
            if c == '\'' && i > 0 && chars[i-1] != '\\' {
                in_char = false;
            }
            if !c.is_whitespace() {
                normalized.push(c);
            }
            i += 1;
            continue;
        }
        
        if i + 1 < chars.len() && chars[i] == '/' && chars[i+1] == '/' {
            i += 2;
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }
        
        if i + 1 < chars.len() && chars[i] == '/' && chars[i+1] == '*' {
            i += 2;
            let mut depth = 1;
            while i < chars.len() && depth > 0 {
                if i + 1 < chars.len() && chars[i] == '/' && chars[i+1] == '*' {
                    depth += 1;
                    i += 2;
                } else if i + 1 < chars.len() && chars[i] == '*' && chars[i+1] == '/' {
                    depth -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
            continue;
        }
        
        if c == '"' {
            in_string = true;
            normalized.push(c);
            i += 1;
            continue;
        }
        
        if c == '\'' {
            in_char = true;
            normalized.push(c);
            i += 1;
            continue;
        }
        
        if !c.is_whitespace() {
            normalized.push(c);
        }
        i += 1;
    }
    normalized
}

pub fn sha256(data: &[u8]) -> String {
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a,
        0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
    ];

    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
        0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
        0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
        0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
        0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
        0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
        0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
    ];

    let mut padded = data.to_vec();
    let bit_len = (data.len() as u64) * 8;
    padded.push(0x80);
    while (padded.len() + 8) % 64 != 0 {
        padded.push(0x00);
    }
    padded.extend_from_slice(&bit_len.to_be_bytes());

    for chunk in padded.chunks_exact(64) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                chunk[i * 4],
                chunk[i * 4 + 1],
                chunk[i * 4 + 2],
                chunk[i * 4 + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }

        let mut a = h[0];
        let mut b = h[1];
        let mut c = h[2];
        let mut d = h[3];
        let mut e = h[4];
        let mut f = h[5];
        let mut g = h[6];
        let mut h_val = h[7];

        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = h_val
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);

            h_val = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }

        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(h_val);
    }

    let mut result = String::new();
    for &val in &h {
        result.push_str(&format!("{:08x}", val));
    }
    result
}

fn serialize_dialogue_context(context_hash: &str, ema_s_t: f64) -> String {
    format!(r#"{{"context_hash":"{}","ema_s_t":{}}}"#, context_hash, ema_s_t)
}

fn deserialize_dialogue_context(dialogue_context_hash: Option<&str>) -> (Option<String>, f64) {
    if let Some(s) = dialogue_context_hash {
        if s.starts_with('{') && s.ends_with('}') {
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(s) {
                let context_hash = val.get("context_hash").and_then(|v| v.as_str()).map(|v| v.to_string());
                let ema_s_t = val.get("ema_s_t").and_then(|v| v.as_f64()).unwrap_or(0.5);
                return (context_hash, ema_s_t);
            }
        }
        (Some(s.to_string()), 0.5)
    } else {
        (None, 0.5)
    }
}

pub fn handle_compile_check_event(
    conn: &rusqlite::Connection,
    workspace_hash: &str,
    file_path: &Path,
    error_code: &str,
    is_success: bool,
) -> rusqlite::Result<DialogueOutcome> {
    let concept_slug = map_error_to_concept(error_code);
    
    let mut m_t = load_concept_mastery(conn, concept_slug)?;
    
    let file_path_str = file_path.to_string_lossy().to_string();
    let file_path_hash = sha256(file_path_str.as_bytes());
    
    let mut db_state = load_dialogue_state(conn, workspace_hash, &file_path_hash)?
        .unwrap_or_else(|| SocraticDialogueState {
            workspace_hash: workspace_hash.to_string(),
            file_path_hash: file_path_hash.to_string(),
            scaffold_level: 1,
            consecutive_failures: 0,
            repetition_count: 0,
            dialogue_context_hash: None,
        });

    let (context_hash, s_t_prev) = deserialize_dialogue_context(db_state.dialogue_context_hash.as_deref());

    let (success_step, failure_decay) = get_pedagogy_coefficients();
    
    let config = crate::config::load_config();
    let lock_difficulty = config.pedagogy.lock_difficulty;
    
    let consecutive_failures;

    if is_success {
        if !lock_difficulty {
            m_t = m_t + (1.0 - m_t) * success_step;
            m_t = m_t.clamp(0.0, 1.0);
        }
        
        consecutive_failures = 0;
        reset_consecutive_failures(&file_path_str, error_code);
        
        db_state.consecutive_failures = 0;
        db_state.repetition_count = 0;
    } else {
        if !lock_difficulty {
            m_t = m_t * failure_decay;
            m_t = m_t.clamp(0.0, 1.0);
        }
        
        let normalized_hash = if file_path.exists() {
            if let Ok(content) = std::fs::read_to_string(file_path) {
                let norm = get_normalized_content(&content);
                sha256(norm.as_bytes())
            } else {
                String::new()
            }
        } else {
            String::new()
        };
        
        consecutive_failures = increment_consecutive_failures(&file_path_str, error_code, &normalized_hash);
        
        db_state.consecutive_failures = consecutive_failures;
    }

    let alpha = 0.3;
    let s_t = if lock_difficulty {
        0.5
    } else {
        alpha * m_t + (1.0 - alpha) * s_t_prev
    };
    
    save_concept_mastery(conn, concept_slug, m_t)?;
    
    db_state.dialogue_context_hash = Some(serialize_dialogue_context(
        context_hash.as_deref().unwrap_or(""),
        s_t,
    ));
    
    db_state.scaffold_level = if m_t <= 0.4 {
        1
    } else if m_t <= 0.7 {
        2
    } else {
        3
    };
    
    save_dialogue_state(conn, &db_state)?;

    let threshold = if lock_difficulty {
        3
    } else {
        calculate_threshold(s_t)
    };

    Ok(DialogueOutcome {
        scaffold_level: db_state.scaffold_level,
        consecutive_failures,
        threshold,
        mastery_score: m_t,
        ema_score: s_t,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sha256() {
        assert_eq!(sha256(b"hello"), "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824");
    }

    #[test]
    fn test_normalization() {
        let raw = r#"
            // This is a line comment
            fn main() {
                /* block comment
                   nested? */
                println!("hello");
            }
        "#;
        let expected = "fnmain(){println!(\"hello\");}";
        assert_eq!(get_normalized_content(raw), expected);
    }

    #[test]
    fn test_calculate_threshold() {
        assert_eq!(calculate_threshold(0.0), 7);
        assert_eq!(calculate_threshold(0.5), 5);
        assert_eq!(calculate_threshold(1.0), 3);
    }

    #[test]
    fn test_cache_limit_and_eviction() {
        let mut cache = FailureCache::new();
        
        // Add 5 entries
        for i in 1..=5 {
            let key = (format!("file{}.rs", i), "E0382".to_string());
            cache.entries.insert(key, CacheEntry {
                consecutive_failures: 1,
                last_hash: "hash".to_string(),
                last_seen: Instant::now() - Duration::from_secs(6 - i), // i=1 is oldest
            });
        }
        
        assert_eq!(cache.entries.len(), 5);
        
        // Evict oldest (which is file1.rs)
        cache.evict_oldest();
        assert_eq!(cache.entries.len(), 4);
        assert!(!cache.entries.contains_key(&("file1.rs".to_string(), "E0382".to_string())));
    }

    #[test]
    fn test_cache_ttl() {
        let mut cache = FailureCache::new();
        
        cache.entries.insert(("file1.rs".to_string(), "E0382".to_string()), CacheEntry {
            consecutive_failures: 1,
            last_hash: "hash".to_string(),
            last_seen: Instant::now() - Duration::from_secs(301), // expired
        });
        
        cache.entries.insert(("file2.rs".to_string(), "E0382".to_string()), CacheEntry {
            consecutive_failures: 1,
            last_hash: "hash".to_string(),
            last_seen: Instant::now() - Duration::from_secs(10), // valid
        });
        
        cache.cleanup_expired();
        assert_eq!(cache.entries.len(), 1);
        assert!(cache.entries.contains_key(&("file2.rs".to_string(), "E0382".to_string())));
    }

    #[test]
    fn test_db_persistence() {
        let temp_dir = std::env::temp_dir();
        let db_path = temp_dir.join("murshid_pedagogy_test.db");
        let _ = std::fs::remove_file(&db_path);
        
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        
        // Create required tables
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS socratic_dialogues (
                workspace_hash TEXT NOT NULL,
                file_path_hash TEXT NOT NULL,
                scaffold_level INTEGER DEFAULT 1,
                consecutive_failures INTEGER DEFAULT 0,
                repetition_count INTEGER DEFAULT 0,
                dialogue_context_hash TEXT,
                updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                PRIMARY KEY (workspace_hash, file_path_hash)
            );
            CREATE TABLE IF NOT EXISTS concepts (
                concept_slug TEXT PRIMARY KEY,
                mastery_score REAL DEFAULT 0.0 CHECK(mastery_score BETWEEN 0.0 AND 1.0),
                exposure_count INTEGER DEFAULT 0,
                consecutive_successes INTEGER DEFAULT 0,
                last_seen TIMESTAMP DEFAULT CURRENT_TIMESTAMP
            );"
        ).unwrap();

        let state = SocraticDialogueState {
            workspace_hash: "ws_hash".to_string(),
            file_path_hash: "file_hash".to_string(),
            scaffold_level: 2,
            consecutive_failures: 4,
            repetition_count: 1,
            dialogue_context_hash: Some("ctx_hash".to_string()),
        };

        save_dialogue_state(&conn, &state).unwrap();
        
        let loaded = load_dialogue_state(&conn, "ws_hash", "file_hash").unwrap().unwrap();
        assert_eq!(loaded, state);

        let _ = std::fs::remove_file(&db_path);
    }
}
