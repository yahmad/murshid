use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::Path;

// Repetitive clean helper to stay self-contained
fn clean_string(s: &str) -> String {
    let s = s.trim();
    if s.starts_with('"') && s.ends_with('"') && s.len() >= 2 {
        s[1..s.len() - 1].to_string()
    } else {
        s.to_string()
    }
}

fn load_hmac_secret_env_list() -> String {
    let mut hmac_secret_env = String::new();

    let paths = vec![
        Some(crate::config::get_system_config_path()),
        crate::config::resolve_user_config_path(),
        crate::config::get_project_config_path(),
    ];

    for path_opt in paths {
        if let Some(path) = path_opt {
            if path.exists() {
                if let Ok(content) = fs::read_to_string(&path) {
                    let parsed = crate::config::parse_toml(&content);
                    if let Some(values) = parsed.get("pedagogy.team") {
                        if let Some(v) = values.get("hmac_secret_env") {
                            hmac_secret_env = clean_string(v);
                        }
                    }
                }
            }
        }
    }

    if hmac_secret_env.is_empty() {
        "MURSHID_HMAC_SECRET".to_string()
    } else {
        hmac_secret_env
    }
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
pub struct ConceptSummary {
    pub average_mastery: f64,
    pub total_exposure: u32,
    pub num_developers: usize,
    pub novice_count: usize,
    pub competent_count: usize,
    pub proficient_count: usize,
    pub expert_count: usize,
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
pub struct TeamSummaryMatrix {
    pub total_developers: usize,
    pub concepts: BTreeMap<String, ConceptSummary>,
}

pub fn compile_dashboard(source_dir: &str) -> Result<String, String> {
    let hmac_env_list = load_hmac_secret_env_list();
    let mut valid_digests = Vec::new();

    let dir_path = Path::new(source_dir);
    if !dir_path.exists() {
        return Err(format!("Source directory does not exist: {}", source_dir));
    }

    for entry in fs::read_dir(dir_path).map_err(|e| format!("Failed to read source directory: {}", e))? {
        let entry = entry.map_err(|e| format!("Directory entry error: {}", e))?;
        let path = entry.path();
        if path.is_file() {
            if let Some(filename) = path.file_name().and_then(|s| s.to_str()) {
                if filename.starts_with("digest_") && filename.ends_with(".json") {
                    let content = fs::read_to_string(&path)
                        .map_err(|e| format!("Failed to read file {}: {}", path.display(), e))?;

                    match crate::team_verifier::verify_digest_from_env(&content, &hmac_env_list) {
                        Ok(()) => {
                            match serde_json::from_str::<serde_json::Value>(&content) {
                                Ok(val) => {
                                    valid_digests.push(val);
                                }
                                Err(e) => {
                                    eprintln!("[WARNING] Malformed JSON in {}: {}", path.display(), e);
                                }
                            }
                        }
                        Err(e) => {
                            eprintln!("[WARNING] Signature verification failed for {}: {}", path.display(), e);
                        }
                    }
                }
            }
        }
    }

    // Aggregate by developer_identity and concept_slug, keeping the latest info
    // (developer_identity, concept_slug) -> (mastery_score, exposure_count, timestamp)
    let mut developer_concepts: HashMap<(String, String), (f64, u32, u64)> = HashMap::new();
    let mut unique_devs = HashSet::new();

    for digest in valid_digests {
        let identity = match digest.get("developer_identity").and_then(|v| v.as_str()) {
            Some(id) => id.to_string(),
            None => continue,
        };
        unique_devs.insert(identity.clone());

        let timestamp_str = digest.get("timestamp").and_then(|v| v.as_str()).unwrap_or("0");
        let timestamp: u64 = timestamp_str.parse().unwrap_or(0);

        if let Some(concepts_arr) = digest.get("concepts").and_then(|v| v.as_array()) {
            for c_val in concepts_arr {
                let slug = match c_val.get("concept_slug").and_then(|v| v.as_str()) {
                    Some(s) => s.to_string(),
                    None => continue,
                };
                let mastery = c_val.get("mastery_score").and_then(|v| v.as_f64()).unwrap_or(0.0);
                let exposure = c_val.get("exposure_count").and_then(|v| v.as_u64()).unwrap_or(0) as u32;

                let key = (identity.clone(), slug.clone());
                if let Some(&(_, _, existing_ts)) = developer_concepts.get(&key) {
                    if timestamp > existing_ts {
                        developer_concepts.insert(key, (mastery, exposure, timestamp));
                    }
                } else {
                    developer_concepts.insert(key, (mastery, exposure, timestamp));
                }
            }
        }
    }

    // Now aggregate developer concepts into a global summary per concept
    let mut concept_dev_values: HashMap<String, Vec<(f64, u32)>> = HashMap::new();
    for ((_, slug), (mastery, exposure, _)) in developer_concepts {
        concept_dev_values.entry(slug).or_default().push((mastery, exposure));
    }

    let mut concepts_summary = BTreeMap::new();
    for (slug, dev_values) in concept_dev_values {
        let num_devs = dev_values.len();
        if num_devs == 0 {
            continue;
        }

        let mut sum_mastery = 0.0;
        let mut total_exp = 0;
        let mut novice = 0;
        let mut competent = 0;
        let mut proficient = 0;
        let mut expert = 0;

        for (m, e) in dev_values {
            sum_mastery += m;
            total_exp += e;

            if m <= 0.30 {
                novice += 1;
            } else if m <= 0.60 {
                competent += 1;
            } else if m <= 0.85 {
                proficient += 1;
            } else {
                expert += 1;
            }
        }

        let avg_mastery = sum_mastery / (num_devs as f64);
        concepts_summary.insert(
            slug,
            ConceptSummary {
                average_mastery: avg_mastery,
                total_exposure: total_exp,
                num_developers: num_devs,
                novice_count: novice,
                competent_count: competent,
                proficient_count: proficient,
                expert_count: expert,
            },
        );
    }

    let summary_matrix = TeamSummaryMatrix {
        total_developers: unique_devs.len(),
        concepts: concepts_summary,
    };

    let summary_json = serde_json::to_string_pretty(&summary_matrix).unwrap_or_default();
    let html = generate_html(&summary_matrix, &summary_json);
    Ok(html)
}

fn generate_concentric_rings_svg(score: f64) -> String {
    // Mappings:
    // Novice: [0.0, 0.30] -> r=20. Range = 0.30.
    let novice_frac = (score / 0.30).min(1.0).max(0.0);
    // Competent: [0.31, 0.60] -> r=28. Range = 0.30.
    let comp_frac = if score <= 0.30 { 0.0 } else { ((score - 0.30) / 0.30).min(1.0).max(0.0) };
    // Proficient: [0.61, 0.85] -> r=36. Range = 0.25.
    let prof_frac = if score <= 0.60 { 0.0 } else { ((score - 0.60) / 0.25).min(1.0).max(0.0) };
    // Expert: [0.86, 1.00] -> r=44. Range = 0.15.
    let exp_frac = if score <= 0.85 { 0.0 } else { ((score - 0.85) / 0.15).min(1.0).max(0.0) };

    let r1 = 20.0; let c1 = 2.0 * std::f64::consts::PI * r1;
    let r2 = 28.0; let c2 = 2.0 * std::f64::consts::PI * r2;
    let r3 = 36.0; let c3 = 2.0 * std::f64::consts::PI * r3;
    let r4 = 44.0; let c4 = 2.0 * std::f64::consts::PI * r4;

    let off1 = c1 * (1.0 - novice_frac);
    let off2 = c2 * (1.0 - comp_frac);
    let off3 = c3 * (1.0 - prof_frac);
    let off4 = c4 * (1.0 - exp_frac);

    format!(
        r##"<svg class="concentric-ring-svg" viewBox="0 0 100 100" width="140" height="140">
  <!-- Track rings (0.1 opacity) -->
  <circle cx="50" cy="50" r="{r1}" fill="none" stroke="#9CA3AF" stroke-width="4.5" stroke-linecap="round" opacity="0.1" />
  <circle cx="50" cy="50" r="{r2}" fill="none" stroke="#3B82F6" stroke-width="4.5" stroke-linecap="round" opacity="0.1" />
  <circle cx="50" cy="50" r="{r3}" fill="none" stroke="#8B5CF6" stroke-width="4.5" stroke-linecap="round" opacity="0.1" />
  <circle cx="50" cy="50" r="{r4}" fill="none" stroke="#10B981" stroke-width="4.5" stroke-linecap="round" opacity="0.1" />
  
  <!-- Active progress fills -->
  <circle cx="50" cy="50" r="{r1}" fill="none" stroke="#9CA3AF" stroke-width="4.5" stroke-dasharray="{c1:.2}" stroke-dashoffset="{off1:.2}" stroke-linecap="round" transform="rotate(-90 50 50)" />
  <circle cx="50" cy="50" r="{r2}" fill="none" stroke="#3B82F6" stroke-width="4.5" stroke-dasharray="{c2:.2}" stroke-dashoffset="{off2:.2}" stroke-linecap="round" transform="rotate(-90 50 50)" />
  <circle cx="50" cy="50" r="{r3}" fill="none" stroke="#8B5CF6" stroke-width="4.5" stroke-dasharray="{c3:.2}" stroke-dashoffset="{off3:.2}" stroke-linecap="round" transform="rotate(-90 50 50)" />
  <circle cx="50" cy="50" r="{r4}" fill="none" stroke="#10B981" stroke-width="4.5" stroke-dasharray="{c4:.2}" stroke-dashoffset="{off4:.2}" stroke-linecap="round" transform="rotate(-90 50 50)" />
  
  <text x="50" y="55" text-anchor="middle" font-size="13" font-weight="bold" fill="#F3F4F6">{pct:.0}%</text>
</svg>"##,
        r1=r1, c1=c1, off1=off1,
        r2=r2, c2=c2, off2=off2,
        r3=r3, c3=c3, off3=off3,
        r4=r4, c4=c4, off4=off4,
        pct=score * 100.0
    )
}

fn generate_html(matrix: &TeamSummaryMatrix, json_string: &str) -> String {
    let mut table_rows = String::new();
    let mut distribution_cards = String::new();
    let mut rings_cards = String::new();

    let mut sum_average_mastery = 0.0;
    let mut active_concepts = 0;

    for (concept, summary) in &matrix.concepts {
        sum_average_mastery += summary.average_mastery;
        active_concepts += 1;

        // Determine tier badge
        let tier_name = if summary.average_mastery <= 0.30 {
            "Novice"
        } else if summary.average_mastery <= 0.60 {
            "Competent"
        } else if summary.average_mastery <= 0.85 {
            "Proficient"
        } else {
            "Expert"
        };
        let tier_class = tier_name.to_lowercase();

        // 1. Skill Matrix Row
        table_rows.push_str(&format!(
            r##"<tr class="matrix-row">
  <td class="concept-cell">{concept}</td>
  <td data-value="{mastery_val:.4}"><span class="badge badge-{tier_class}">{tier_name} ({mastery_pct:.0}%)</span></td>
  <td class="num-cell" data-value="{exposure}">{exposure}</td>
  <td class="num-cell" data-value="{devs}">{devs}</td>
  <td class="num-cell" data-value="{novice}">{novice}</td>
  <td class="num-cell" data-value="{competent}">{competent}</td>
  <td class="num-cell" data-value="{proficient}">{proficient}</td>
  <td class="num-cell" data-value="{expert}">{expert}</td>
</tr>"##,
            concept = concept,
            tier_class = tier_class,
            tier_name = tier_name,
            mastery_val = summary.average_mastery,
            mastery_pct = summary.average_mastery * 100.0,
            exposure = summary.total_exposure,
            devs = summary.num_developers,
            novice = summary.novice_count,
            competent = summary.competent_count,
            proficient = summary.proficient_count,
            expert = summary.expert_count,
        ));

        // 2. Concept Distribution Card
        let total_concept_devs = (summary.novice_count + summary.competent_count + summary.proficient_count + summary.expert_count) as f64;
        let get_pct = |count: usize| {
            if total_concept_devs > 0.0 {
                (count as f64 / total_concept_devs) * 100.0
            } else {
                0.0
            }
        };

        distribution_cards.push_str(&format!(
            r##"<div class="glass-card dist-card">
  <h3 class="card-title">{concept}</h3>
  <div class="card-subtitle">Developer Count: {devs} | Avg Mastery: {mastery_pct:.0}%</div>
  
  <div class="tier-bar-row">
    <span class="tier-label">Novice</span>
    <div class="bar-container">
      <div class="bar bar-novice" style="width: {novice_pct:.1}%"></div>
    </div>
    <span class="tier-count">{novice}</span>
  </div>
  
  <div class="tier-bar-row">
    <span class="tier-label">Competent</span>
    <div class="bar-container">
      <div class="bar bar-competent" style="width: {competent_pct:.1}%"></div>
    </div>
    <span class="tier-count">{competent}</span>
  </div>
  
  <div class="tier-bar-row">
    <span class="tier-label">Proficient</span>
    <div class="bar-container">
      <div class="bar bar-proficient" style="width: {proficient_pct:.1}%"></div>
    </div>
    <span class="tier-count">{proficient}</span>
  </div>
  
  <div class="tier-bar-row">
    <span class="tier-label">Expert</span>
    <div class="bar-container">
      <div class="bar bar-expert" style="width: {expert_pct:.1}%"></div>
    </div>
    <span class="tier-count">{expert}</span>
  </div>
</div>"##,
            concept = concept,
            devs = summary.num_developers,
            mastery_pct = summary.average_mastery * 100.0,
            novice = summary.novice_count,
            novice_pct = get_pct(summary.novice_count),
            competent = summary.competent_count,
            competent_pct = get_pct(summary.competent_count),
            proficient = summary.proficient_count,
            proficient_pct = get_pct(summary.proficient_count),
            expert = summary.expert_count,
            expert_pct = get_pct(summary.expert_count),
        ));

        // 3. Rings Card
        let rings_svg = generate_concentric_rings_svg(summary.average_mastery);
        rings_cards.push_str(&format!(
            r##"<div class="glass-card rings-card">
  <h3 class="card-title">{concept}</h3>
  <div class="rings-wrapper">
    {svg}
  </div>
  <div class="rings-legend">
    <div class="legend-item"><span class="legend-dot dot-novice"></span>Novice (r=20)</div>
    <div class="legend-item"><span class="legend-dot dot-competent"></span>Competent (r=28)</div>
    <div class="legend-item"><span class="legend-dot dot-proficient"></span>Proficient (r=36)</div>
    <div class="legend-item"><span class="legend-dot dot-expert"></span>Expert (r=44)</div>
  </div>
</div>"##,
            concept = concept,
            svg = rings_svg,
        ));
    }

    let global_avg = if active_concepts > 0 {
        (sum_average_mastery / (active_concepts as f64)) * 100.0
    } else {
        0.0
    };

    let placeholder_message = if matrix.concepts.is_empty() {
        r#"<tr class="empty-row"><td colspan="8" style="text-align: center; padding: 3rem; color: var(--text-muted);">No concept digests discovered or verified in the source folder.</td></tr>"#
    } else {
        ""
    };

    let empty_cards_message = if matrix.concepts.is_empty() {
        r#"<div style="grid-column: 1 / -1; text-align: center; padding: 3rem; color: var(--text-muted);">No data available to display.</div>"#
    } else {
        ""
    };

    format!(
        r###"<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8">
  <meta name="viewport" content="width=device-width, initial-scale=1.0">
  <title>Murshid Team Analytics Dashboard</title>
  <style>
    :root {{
      --bg-main: #090d16;
      --bg-card: rgba(17, 24, 39, 0.7);
      --border-card: rgba(255, 255, 255, 0.08);
      --text-main: #f3f4f6;
      --text-muted: #9ca3af;
      --accent: #6366f1;
      --accent-gradient: linear-gradient(135deg, #6366f1, #a855f7);
      --success: #10b981;
      --warning: #f59e0b;
      --danger: #ef4444;
      
      --tier-novice: #9ca3af;
      --tier-competent: #3b82f6;
      --tier-proficient: #8b5cf6;
      --tier-expert: #10b981;
    }}

    * {{
      box-sizing: border-box;
      margin: 0;
      padding: 0;
    }}

    body {{
      background-color: var(--bg-main);
      color: var(--text-main);
      font-family: system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, "Helvetica Neue", Arial, sans-serif;
      line-height: 1.5;
      padding: 2.5rem 1.5rem;
      min-height: 100vh;
      display: flex;
      flex-direction: column;
      align-items: center;
    }}

    .container {{
      max-width: 1200px;
      width: 100%;
    }}

    header {{
      margin-bottom: 2.5rem;
      text-align: center;
      position: relative;
    }}

    .header-glow {{
      position: absolute;
      top: -60px;
      left: 50%;
      transform: translateX(-50%);
      width: 300px;
      height: 150px;
      background: radial-gradient(circle, rgba(99, 102, 241, 0.15) 0%, rgba(99, 102, 241, 0) 70%);
      filter: blur(20px);
      z-index: -1;
      pointer-events: none;
    }}

    h1 {{
      font-size: 2.5rem;
      font-weight: 800;
      letter-spacing: -0.05em;
      margin-bottom: 0.5rem;
      background: var(--accent-gradient);
      -webkit-background-clip: text;
      -webkit-text-fill-color: transparent;
      text-shadow: 0 0 30px rgba(99, 102, 241, 0.2);
    }}

    .subtitle {{
      color: var(--text-muted);
      font-size: 1rem;
      font-weight: 500;
    }}

    /* Stat Cards */
    .stats-grid {{
      display: grid;
      grid-template-columns: repeat(auto-fit, minmax(280px, 1fr));
      gap: 1.5rem;
      margin-bottom: 2.5rem;
    }}

    .glass-card {{
      background: var(--bg-card);
      border: 1px solid var(--border-card);
      border-radius: 1rem;
      padding: 1.5rem;
      backdrop-filter: blur(16px);
      box-shadow: 0 4px 30px rgba(0, 0, 0, 0.3);
      transition: transform 0.3s cubic-bezier(0.4, 0, 0.2, 1), border-color 0.3s ease;
    }}

    .glass-card:hover {{
      transform: translateY(-4px);
      border-color: rgba(255, 255, 255, 0.15);
    }}

    .stat-label {{
      font-size: 0.875rem;
      font-weight: 600;
      text-transform: uppercase;
      letter-spacing: 0.05em;
      color: var(--text-muted);
      margin-bottom: 0.5rem;
    }}

    .stat-value {{
      font-size: 2.25rem;
      font-weight: 700;
      color: #fff;
    }}

    /* Navigation Tabs */
    .nav-tabs {{
      display: flex;
      justify-content: center;
      gap: 0.75rem;
      margin-bottom: 2rem;
      background: rgba(255, 255, 255, 0.03);
      padding: 0.4rem;
      border-radius: 0.75rem;
      width: fit-content;
      margin-left: auto;
      margin-right: auto;
      border: 1px solid rgba(255, 255, 255, 0.05);
    }}

    .tab-btn {{
      background: transparent;
      border: none;
      color: var(--text-muted);
      padding: 0.6rem 1.2rem;
      font-size: 0.875rem;
      font-weight: 600;
      border-radius: 0.5rem;
      cursor: pointer;
      transition: all 0.2s ease;
    }}

    .tab-btn:hover {{
      color: #fff;
      background: rgba(255, 255, 255, 0.05);
    }}

    .tab-btn.active {{
      color: #fff;
      background: var(--accent);
      box-shadow: 0 4px 12px rgba(99, 102, 241, 0.3);
    }}

    .tab-content {{
      display: none;
    }}

    .tab-content.active {{
      display: block;
      animation: fadeIn 0.4s ease;
    }}

    @keyframes fadeIn {{
      from {{ opacity: 0; transform: translateY(8px); }}
      to {{ opacity: 1; transform: translateY(0); }}
    }}

    /* Table styles */
    .table-container {{
      overflow-x: auto;
      border-radius: 1rem;
      border: 1px solid var(--border-card);
    }}

    table {{
      width: 100%;
      border-collapse: collapse;
      background: var(--bg-card);
      text-align: left;
      font-size: 0.95rem;
    }}

    th, td {{
      padding: 1rem 1.25rem;
      border-bottom: 1px solid rgba(255, 255, 255, 0.05);
    }}

    th {{
      background: rgba(255, 255, 255, 0.02);
      color: var(--text-muted);
      font-weight: 600;
      cursor: pointer;
      user-select: none;
      transition: background-color 0.2s ease, color 0.2s ease;
    }}

    th:hover {{
      background: rgba(255, 255, 255, 0.06);
      color: #fff;
    }}

    .sort-arrow {{
      font-size: 0.75rem;
      opacity: 0.3;
      transition: opacity 0.2s ease;
    }}

    tr.matrix-row:hover {{
      background: rgba(255, 255, 255, 0.02);
    }}

    .concept-cell {{
      font-weight: 600;
      color: #fff;
    }}

    .num-cell {{
      text-align: right;
    }}

    th.num-header {{
      text-align: right;
    }}

    .badge {{
      display: inline-flex;
      align-items: center;
      padding: 0.25rem 0.6rem;
      border-radius: 9999px;
      font-size: 0.75rem;
      font-weight: 700;
      text-transform: uppercase;
      letter-spacing: 0.02em;
    }}

    .badge-novice {{ background: rgba(156, 163, 175, 0.15); color: var(--tier-novice); }}
    .badge-competent {{ background: rgba(59, 130, 246, 0.15); color: var(--tier-competent); }}
    .badge-proficient {{ background: rgba(139, 92, 246, 0.15); color: var(--tier-proficient); }}
    .badge-expert {{ background: rgba(16, 185, 129, 0.15); color: var(--tier-expert); }}

    /* Cards Grid layouts */
    .cards-grid {{
      display: grid;
      grid-template-columns: repeat(auto-fill, minmax(320px, 1fr));
      gap: 1.5rem;
    }}

    .card-title {{
      font-size: 1.2rem;
      font-weight: 700;
      color: #fff;
      margin-bottom: 0.25rem;
    }}

    .card-subtitle {{
      font-size: 0.8rem;
      color: var(--text-muted);
      margin-bottom: 1.25rem;
    }}

    /* Distribution bar charts */
    .tier-bar-row {{
      display: flex;
      align-items: center;
      margin-bottom: 0.75rem;
      font-size: 0.85rem;
    }}

    .tier-label {{
      width: 85px;
      font-weight: 600;
      color: var(--text-muted);
    }}

    .bar-container {{
      flex: 1;
      height: 8px;
      background: rgba(255, 255, 255, 0.05);
      border-radius: 9999px;
      margin: 0 1rem;
      overflow: hidden;
    }}

    .bar {{
      height: 100%;
      border-radius: 9999px;
      transition: width 0.6s cubic-bezier(0.4, 0, 0.2, 1);
    }}

    .bar-novice {{ background: var(--tier-novice); }}
    .bar-competent {{ background: var(--tier-competent); }}
    .bar-proficient {{ background: var(--tier-proficient); }}
    .bar-expert {{ background: var(--tier-expert); }}

    .tier-count {{
      width: 25px;
      text-align: right;
      font-weight: 700;
      color: #fff;
    }}

    /* Concentric Rings Visuals */
    .rings-wrapper {{
      display: flex;
      justify-content: center;
      align-items: center;
      margin: 1.5rem 0;
    }}

    .concentric-ring-svg {{
      filter: drop-shadow(0 4px 12px rgba(0, 0, 0, 0.4));
    }}

    .rings-legend {{
      margin-top: 1rem;
      display: grid;
      grid-template-columns: 1fr 1fr;
      gap: 0.5rem;
      font-size: 0.75rem;
    }}

    .legend-item {{
      display: flex;
      align-items: center;
      color: var(--text-muted);
      font-weight: 500;
    }}

    .legend-dot {{
      width: 8px;
      height: 8px;
      border-radius: 50%;
      margin-right: 0.5rem;
    }}

    .dot-novice {{ background: var(--tier-novice); }}
    .dot-competent {{ background: var(--tier-competent); }}
    .dot-proficient {{ background: var(--tier-proficient); }}
    .dot-expert {{ background: var(--tier-expert); }}

  </style>
</head>
<body>
  <div class="container">
    <header>
      <div class="header-glow"></div>
      <h1>Murshid Team Mastery</h1>
      <div class="subtitle">Offline-compiled zero-telemetry analytics dashboard</div>
    </header>

    <div class="stats-grid">
      <div class="glass-card">
        <div class="stat-label">Total Developers</div>
        <div class="stat-value">{total_devs}</div>
      </div>
      <div class="glass-card">
        <div class="stat-label">Average Team Mastery</div>
        <div class="stat-value">{global_avg:.1}%</div>
      </div>
      <div class="glass-card">
        <div class="stat-label">Active Concepts</div>
        <div class="stat-value">{active_concepts}</div>
      </div>
    </div>

    <div class="nav-tabs">
      <button class="tab-btn active" onclick="switchTab('matrix-tab')">Skill Matrix</button>
      <button class="tab-btn" onclick="switchTab('dist-tab')">Concept Distribution</button>
      <button class="tab-btn" onclick="switchTab('rings-tab')">Visual Mastery Rings</button>
    </div>

    <!-- TAB 1: SKILL MATRIX -->
    <div id="matrix-tab" class="tab-content active">
      <div class="table-container">
        <table id="matrix-table">
          <thead>
            <tr>
              <th onclick="sortTable(0, false)">Concept <span class="sort-arrow">↕</span></th>
              <th onclick="sortTable(1, true)">Avg Mastery <span class="sort-arrow">↕</span></th>
              <th class="num-header" onclick="sortTable(2, true)">Total Exposure <span class="sort-arrow">↕</span></th>
              <th class="num-header" onclick="sortTable(3, true)">Active Devs <span class="sort-arrow">↕</span></th>
              <th class="num-header" onclick="sortTable(4, true)">Novice <span class="sort-arrow">↕</span></th>
              <th class="num-header" onclick="sortTable(5, true)">Competent <span class="sort-arrow">↕</span></th>
              <th class="num-header" onclick="sortTable(6, true)">Proficient <span class="sort-arrow">↕</span></th>
              <th class="num-header" onclick="sortTable(7, true)">Expert <span class="sort-arrow">↕</span></th>
            </tr>
          </thead>
          <tbody>
            {table_rows}
            {placeholder_message}
          </tbody>
        </table>
      </div>
    </div>

    <!-- TAB 2: CONCEPT DISTRIBUTION -->
    <div id="dist-tab" class="tab-content">
      <div class="cards-grid">
        {distribution_cards}
        {empty_cards_message}
      </div>
    </div>

    <!-- TAB 3: VISUAL MASTERY RINGS -->
    <div id="rings-tab" class="tab-content">
      <div class="cards-grid">
        {rings_cards}
        {empty_cards_message}
      </div>
    </div>
  </div>

  <script id="dashboard-data" type="application/json">
{json_string}
  </script>

  <script>
    let sortDirection = 1;
    let currentSortCol = -1;

    function switchTab(tabId) {{
      document.querySelectorAll('.tab-content').forEach(el => {{
        el.classList.remove('active');
      }});
      document.querySelectorAll('.tab-btn').forEach(el => {{
        el.classList.remove('active');
      }});
      
      document.getElementById(tabId).classList.add('active');
      
      // Mark clicked button as active
      const btnText = tabId === 'matrix-tab' ? 'Skill Matrix' : 
                      tabId === 'dist-tab' ? 'Concept Distribution' : 'Visual Mastery Rings';
      
      Array.from(document.querySelectorAll('.tab-btn')).forEach(btn => {{
        if (btn.textContent === btnText) {{
          btn.classList.add('active');
        }}
      }});
    }}

    function sortTable(colIndex, numeric) {{
      const tbody = document.querySelector('#matrix-table tbody');
      const rows = Array.from(tbody.querySelectorAll('tr.matrix-row'));
      if (rows.length === 0) return;
      
      if (currentSortCol === colIndex) {{
        sortDirection *= -1;
      }} else {{
        sortDirection = 1;
        currentSortCol = colIndex;
      }}
      
      rows.sort((a, b) => {{
        let aVal = a.cells[colIndex].getAttribute('data-value') || a.cells[colIndex].textContent.trim();
        let bVal = b.cells[colIndex].getAttribute('data-value') || b.cells[colIndex].textContent.trim();
        
        if (numeric) {{
          return (parseFloat(aVal) - parseFloat(bVal)) * sortDirection;
        }} else {{
          return aVal.localeCompare(bVal) * sortDirection;
        }}
      }});
      
      tbody.innerHTML = '';
      rows.forEach(row => tbody.appendChild(row));
      
      // Update UI indicators
      document.querySelectorAll('th').forEach((th, idx) => {{
        const arrow = th.querySelector('.sort-arrow');
        if (!arrow) return;
        if (idx === colIndex) {{
          arrow.textContent = sortDirection === 1 ? ' ▲' : ' ▼';
          arrow.style.opacity = 1;
        }} else {{
          arrow.textContent = ' ↕';
          arrow.style.opacity = 0.3;
        }}
      }});
    }}
  </script>
</body>
</html>
"###,
        total_devs = matrix.total_developers,
        global_avg = global_avg,
        active_concepts = active_concepts,
        table_rows = table_rows,
        placeholder_message = placeholder_message,
        distribution_cards = distribution_cards,
        empty_cards_message = empty_cards_message,
        rings_cards = rings_cards,
        json_string = json_string
    )
}

pub fn run_team_dashboard_cli(args: &[String]) -> i32 {
    let mut source_dir = None;
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--source" {
            if i + 1 < args.len() {
                source_dir = Some(&args[i + 1]);
                i += 2;
            } else {
                eprintln!("Error: --source requires a directory path argument");
                return 78;
            }
        } else {
            eprintln!("Error: Unrecognized argument: {}", args[i]);
            return 78;
        }
    }

    let source = match source_dir {
        Some(s) => s,
        None => {
            eprintln!("Usage: murshid team-dashboard --source <shared-dir>");
            return 78;
        }
    };

    match compile_dashboard(source) {
        Ok(html) => {
            println!("{}", html);
            0
        }
        Err(e) => {
            eprintln!("Error compiling dashboard: {}", e);
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_empty_directory_dashboard() {
        let temp_dir = std::env::temp_dir().join("murshid_empty_dash");
        let _ = fs::remove_dir_all(&temp_dir);
        fs::create_dir_all(&temp_dir).unwrap();

        let html = compile_dashboard(&temp_dir.to_string_lossy()).unwrap();
        assert!(html.contains("Murshid Team Mastery"));
        assert!(html.contains("Total Developers"));
        assert!(html.contains(">0</div>"));
        assert!(html.contains("0.0%"));

        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_valid_digests_aggregation() {
        let temp_dir = std::env::temp_dir().join("murshid_valid_dash");
        let _ = fs::remove_dir_all(&temp_dir);
        fs::create_dir_all(&temp_dir).unwrap();

        unsafe {
            std::env::set_var("MURSHID_TEST_HMAC_DASH", "secret");
        }

        // Create mock config to look up the hmac_secret_env
        let config_dir = temp_dir.join(".murshid");
        fs::create_dir_all(&config_dir).unwrap();
        fs::write(
            config_dir.join("config.toml"),
            r#"
            [pedagogy.team]
            hmac_secret_env = "MURSHID_TEST_HMAC_DASH"
            "#,
        )
        .unwrap();

        // Create mock digests using export_digest from team_exporter
        let entries_a = vec![crate::team_exporter::DigestEntry {
            concept_slug: "ownership".to_string(),
            mastery_score: 0.8,
            exposure_count: 5,
        }];
        let (_, digest_a) = crate::team_exporter::export_digest(
            "dev1",
            "project1",
            "dev1@test.com",
            "anonymous",
            "",
            &entries_a,
            b"secret",
        );

        let entries_b = vec![crate::team_exporter::DigestEntry {
            concept_slug: "ownership".to_string(),
            mastery_score: 0.4,
            exposure_count: 10,
        }];
        let (_, digest_b) = crate::team_exporter::export_digest(
            "dev2",
            "project1",
            "dev2@test.com",
            "anonymous",
            "",
            &entries_b,
            b"secret",
        );

        fs::write(temp_dir.join("digest_dev1_proj1.json"), &digest_a).unwrap();
        fs::write(temp_dir.join("digest_dev2_proj1.json"), &digest_b).unwrap();

        // Run compile_dashboard in a context where get_project_config_path is mocked or by setting current dir
        let original_dir = std::env::current_dir().unwrap();
        std::env::set_current_dir(&temp_dir).unwrap();

        let html = compile_dashboard(&temp_dir.to_string_lossy()).unwrap();

        std::env::set_current_dir(original_dir).unwrap();
        unsafe {
            std::env::remove_var("MURSHID_TEST_HMAC_DASH");
        }

        assert!(html.contains("Total Developers"));
        assert!(html.contains(">2</div>"));
        assert!(html.contains("ownership"));
        assert!(html.contains("60%")); // average of 0.8 and 0.4 is 0.6 = 60%
        assert!(html.contains("Total Exposure <span"));

        let _ = fs::remove_dir_all(&temp_dir);
    }
}
