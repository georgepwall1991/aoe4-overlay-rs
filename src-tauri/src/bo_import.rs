//! Import build orders from community sites.
//!
//! - aoe4guides.com — REST API (https://aoe4guides.com/api/api-docs/).
//!   `GET /api/builds/{id}?overlay=true` returns RTS_Overlay JSON directly.
//!   `GET /api/builds?civ=..&orderBy=..` lists up to 10 builds for browsing.
//! - aoeivbuilds.com — no JSON API for the content, but every build has a
//!   plain-text export at `/build_orders/{id}/download.txt` which we convert
//!   into RTS_Overlay JSON steps.

use serde::Serialize;
use serde_json::{json, Value};
use std::time::Duration;

/// Imports are user-initiated; fail fast instead of leaving a button spinning.
const TIMEOUT: Duration = Duration::from_secs(10);

const GUIDES_API: &str = "https://aoe4guides.com/api";
const AOEIVBUILDS: &str = "https://www.aoeivbuilds.com";

const CIV_ALIASES: &[(&str, &str)] = &[
    ("ABB", "Abbasid Dynasty"),
    ("Abbasid", "Abbasid Dynasty"),
    ("Abbasid Dynasty", "Abbasid Dynasty"),
    ("AYY", "Ayyubids"),
    ("Ayyubids", "Ayyubids"),
    ("BYZ", "Byzantines"),
    ("Byzantines", "Byzantines"),
    ("CHI", "Chinese"),
    ("China", "Chinese"),
    ("Chinese", "Chinese"),
    ("DEL", "Delhi Sultanate"),
    ("Delhi", "Delhi Sultanate"),
    ("Delhi Sultanate", "Delhi Sultanate"),
    ("ENG", "English"),
    ("English", "English"),
    ("FRE", "French"),
    ("France", "French"),
    ("French", "French"),
    ("HRE", "Holy Roman Empire"),
    ("Holy Roman Empire", "Holy Roman Empire"),
    ("GOH", "Golden Horde"),
    ("Golden Horde", "Golden Horde"),
    ("HOL", "House of Lancaster"),
    ("House of Lancaster", "House of Lancaster"),
    ("House Of Lancaster", "House of Lancaster"),
    ("JAP", "Japanese"),
    ("Japan", "Japanese"),
    ("Japanese", "Japanese"),
    ("JDA", "Jeanne d'Arc"),
    ("Jeanne d'Arc", "Jeanne d'Arc"),
    ("Jeanne Darc", "Jeanne d'Arc"),
    ("Jeanne", "Jeanne d'Arc"),
    ("JIN", "Jin Dynasty"),
    ("Jin", "Jin Dynasty"),
    ("Jin Dynasty", "Jin Dynasty"),
    ("KTE", "Knights Templar"),
    ("Knights Templar", "Knights Templar"),
    ("MAC", "Macedonian Dynasty"),
    ("Macedonian Dynasty", "Macedonian Dynasty"),
    ("MAL", "Malians"),
    ("Mali", "Malians"),
    ("Malians", "Malians"),
    ("MON", "Mongols"),
    ("Mongols", "Mongols"),
    ("DRA", "Order of the Dragon"),
    ("OOTD", "Order of the Dragon"),
    ("Order of the Dragon", "Order of the Dragon"),
    ("Order Of The Dragon", "Order of the Dragon"),
    ("Order Dragon", "Order of the Dragon"),
    ("OTT", "Ottomans"),
    ("Ottoman", "Ottomans"),
    ("Ottomans", "Ottomans"),
    ("RUS", "Rus"),
    ("Rus", "Rus"),
    ("SEN", "Sengoku Daimyo"),
    ("Sengoku Daimyo", "Sengoku Daimyo"),
    ("TUG", "Tughlaq Dynasty"),
    ("Tughlaq Dynasty", "Tughlaq Dynasty"),
    ("ZXL", "Zhu Xi's Legacy"),
    ("Zhu Xi's Legacy", "Zhu Xi's Legacy"),
    ("Zhu Xis Legacy", "Zhu Xi's Legacy"),
    ("Zhu Xi", "Zhu Xi's Legacy"),
    ("Any", "Any"),
];

#[derive(Debug, Clone, Serialize)]
pub struct ImportedBo {
    pub name: String,
    pub content: String,
    pub civilization: Option<String>,
    pub source: String,
    pub repairs: Vec<String>,
    pub repair_summary: RepairSummary,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct RepairSummary {
    pub total: usize,
    pub times: usize,
    pub notes: usize,
    pub icons: usize,
    pub resources: usize,
    pub steps: usize,
}

/// What the user pasted: a full URL from either site, or a bare aoe4guides id.
enum Source {
    Aoe4Guides(String),
    AoeIvBuilds(String),
}

fn parse_source(input: &str) -> Result<Source, String> {
    let s = input.trim().trim_end_matches('/');
    // aoe4guides.com/builds/{id} (canonical share link)
    if let Some(pos) = s.find("aoe4guides.com/builds/") {
        let id = s[pos + "aoe4guides.com/builds/".len()..]
            .split(['/', '?', '#'])
            .next()
            .unwrap_or("");
        if !id.is_empty() {
            return Ok(Source::Aoe4Guides(id.to_string()));
        }
    }
    // aoeivbuilds.com/build_orders/{id}
    if let Some(pos) = s.find("aoeivbuilds.com/build_orders/") {
        let id = s[pos + "aoeivbuilds.com/build_orders/".len()..]
            .split(['/', '?', '#', '.'])
            .next()
            .unwrap_or("");
        if id.chars().all(|c| c.is_ascii_digit()) && !id.is_empty() {
            return Ok(Source::AoeIvBuilds(id.to_string()));
        }
    }
    // Bare aoe4guides build id (Firestore doc id: 20 alphanumeric chars)
    if !s.contains('/')
        && !s.contains('.')
        && s.len() >= 15
        && s.chars().all(|c| c.is_ascii_alphanumeric())
    {
        return Ok(Source::Aoe4Guides(s.to_string()));
    }
    Err("Unrecognized link. Paste a build URL from aoe4guides.com or aoeivbuilds.com.".into())
}

pub async fn import_from_url(client: &reqwest::Client, url: &str) -> Result<ImportedBo, String> {
    match parse_source(url)? {
        Source::Aoe4Guides(id) => import_aoe4guides(client, &id).await,
        Source::AoeIvBuilds(id) => import_aoeivbuilds(client, &id).await,
    }
}

async fn import_aoe4guides(client: &reqwest::Client, id: &str) -> Result<ImportedBo, String> {
    let url = format!("{GUIDES_API}/builds/{id}?overlay=true");
    let resp = client
        .get(&url)
        .timeout(TIMEOUT)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Err(format!(
            "Build '{id}' was not found on aoe4guides — was it deleted?"
        ));
    }
    if !resp.status().is_success() {
        return Err(format!(
            "aoe4guides returned {} for build '{id}'",
            resp.status()
        ));
    }
    let mut bo: Value = resp
        .json()
        .await
        .map_err(|e| format!("bad JSON from aoe4guides: {e}"))?;
    let mut repairs = normalize_imported_json(&mut bo);
    if bo["build_order"].as_array().is_none_or(|a| a.is_empty()) {
        return Err(empty_build_error(id, &repairs));
    }
    if imported_civilization(&bo).is_none() {
        if let Ok(metadata) = fetch_aoe4guides_metadata(client, id).await {
            fill_missing_civilization_from_metadata(&mut bo, &metadata, &mut repairs);
        }
    }
    let repair_summary = summarize_repairs(&repairs);
    let name = bo["name"].as_str().unwrap_or("Imported build").to_string();
    let civilization = imported_civilization(&bo);
    Ok(ImportedBo {
        name,
        content: serde_json::to_string_pretty(&bo).map_err(|e| e.to_string())?,
        civilization,
        source: format!("https://aoe4guides.com/builds/{id}"),
        repairs,
        repair_summary,
    })
}

fn empty_build_error(id: &str, repairs: &[String]) -> String {
    if repairs
        .iter()
        .any(|repair| repair.starts_with("Removed ") && repair.contains(" blank step"))
    {
        format!("aoe4guides build '{id}' has no usable steps after removing blank steps.")
    } else {
        format!("aoe4guides build '{id}' has no steps.")
    }
}

async fn fetch_aoe4guides_metadata(client: &reqwest::Client, id: &str) -> Result<Value, String> {
    let url = format!("{GUIDES_API}/builds/{id}");
    let resp = client
        .get(&url)
        .timeout(TIMEOUT)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!(
            "aoe4guides returned {} for build metadata '{id}'",
            resp.status()
        ));
    }
    resp.json()
        .await
        .map_err(|e| format!("bad metadata JSON from aoe4guides: {e}"))
}

fn imported_civilization(bo: &Value) -> Option<String> {
    bo["civilization"].as_str().map(str::to_string).or_else(|| {
        bo["civilization"]
            .as_array()
            .and_then(|civs| civs.first())
            .and_then(Value::as_str)
            .map(str::to_string)
    })
}

fn first_civilization(value: Value) -> Option<String> {
    match value {
        Value::String(s) => Some(s),
        Value::Array(civs) => civs.first().and_then(Value::as_str).map(str::to_string),
        _ => None,
    }
}

fn fill_missing_civilization_from_metadata(
    bo: &mut Value,
    metadata: &Value,
    repairs: &mut Vec<String>,
) {
    if imported_civilization(bo).is_some() {
        return;
    }
    let Some(civ) = normalize_civilization_value(&metadata["civ"]).and_then(first_civilization)
    else {
        return;
    };
    bo["civilization"] = Value::from(civ);
    repairs.push("Filled missing civilization from aoe4guides metadata.".to_string());
}

async fn import_aoeivbuilds(client: &reqwest::Client, id: &str) -> Result<ImportedBo, String> {
    let txt_url = format!("{AOEIVBUILDS}/build_orders/{id}/download.txt");
    let resp = client
        .get(&txt_url)
        .timeout(TIMEOUT)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Err(format!(
            "Build '{id}' was not found on aoeivbuilds — was it deleted?"
        ));
    }
    if !resp.status().is_success() {
        return Err(format!(
            "aoeivbuilds returned {} for build '{id}'",
            resp.status()
        ));
    }
    let txt = resp.text().await.map_err(|e| e.to_string())?;

    // The civ only appears on the HTML page, not in the text export.
    let civ = match client
        .get(format!("{AOEIVBUILDS}/build_orders/{id}"))
        .timeout(TIMEOUT)
        .send()
        .await
    {
        Ok(r) if r.status().is_success() => r.text().await.ok().and_then(|html| extract_civ(&html)),
        _ => None,
    };

    let page_url = format!("{AOEIVBUILDS}/build_orders/{id}");
    let (bo, repairs) = convert_aoeivbuilds_txt_with_repairs(&txt, civ.as_deref(), &page_url)?;
    let repair_summary = summarize_repairs(&repairs);
    let name = bo["name"].as_str().unwrap_or("Imported build").to_string();
    Ok(ImportedBo {
        name,
        content: serde_json::to_string_pretty(&bo).map_err(|e| e.to_string())?,
        civilization: civ,
        source: page_url,
        repairs,
        repair_summary,
    })
}

/// Normalize hand-typed step times: "5.30" -> "5:30", "4:5" -> "4:05",
/// "=3:50" -> "3:50", and common approximate markers to "~M:SS".
/// Anything ambiguous is left as readable text.
fn normalize_time(t: &str) -> String {
    let decoded = decode_html_entities(t);
    let t = decoded.trim();
    if t.is_empty() {
        return String::new();
    }
    if t.contains('<') && t.contains('>') {
        return normalize_time(&strip_html_tags(t));
    }

    let mut s = t
        .replace(['‑', '–', '—'], "-")
        .replace('±', "+-")
        .replace('\n', " ")
        .trim()
        .to_string();
    while let Some(rest) = s.strip_prefix(['%', '[', '(']) {
        s = rest.trim_start().to_string();
    }
    while let Some(rest) = s.strip_suffix([']', ')']) {
        s = rest.trim_end().to_string();
    }
    while let Some(rest) = s.strip_prefix('=') {
        s = rest.trim_start().to_string();
    }

    let mut approximate = false;
    for prefix in ["~", "≈", "+-", "+/-"] {
        if let Some(rest) = s.strip_prefix(prefix) {
            approximate = true;
            s = rest.trim_start().to_string();
            break;
        }
    }
    for prefix in ["about ", "around ", "ca. ", "c. "] {
        if s.to_ascii_lowercase().starts_with(prefix) {
            approximate = true;
            s = s[prefix.len()..].trim_start().to_string();
            break;
        }
    }
    for suffix in ["ish", "min", "mins", "分"] {
        if s.to_ascii_lowercase().ends_with(suffix) {
            approximate |= suffix == "ish";
            s.truncate(s.len() - suffix.len());
            s = s.trim_end().to_string();
            break;
        }
    }
    for suffix in [" seconds", " second", " secs", " sec", "s"] {
        let lower = s.to_ascii_lowercase();
        if lower.ends_with(suffix) {
            let seconds_text = s[..s.len() - suffix.len()].trim();
            if !seconds_text.is_empty() && seconds_text.chars().all(|c| c.is_ascii_digit()) {
                if let Ok(seconds) = seconds_text.parse::<u32>() {
                    return format!(
                        "{}{}:{:02}",
                        if approximate { "~" } else { "" },
                        seconds / 60,
                        seconds % 60
                    );
                }
            }
        }
    }
    for suffix in [" onwards", " onward", " on"] {
        let lower = s.to_ascii_lowercase();
        if lower.ends_with(suffix) {
            approximate = true;
            s.truncate(s.len() - suffix.len());
            s = s.trim_end().to_string();
            break;
        }
    }
    for suffix in ["+-", "+/-", "+-", "+", "~", ">"] {
        if let Some(rest) = s.strip_suffix(suffix) {
            approximate = true;
            s = rest.trim_end().to_string();
            break;
        }
    }
    if s.is_empty() || s.chars().all(|c| matches!(c, '-' | '~' | '>' | '<')) {
        return String::new();
    }
    if let Some((a, b)) = s.split_once(" + ") {
        if let (Some(start), Some(end)) = (normalize_clock(a), normalize_clock(b)) {
            return format!("{start}-{end}");
        }
    }

    if let Some((a, b)) = s.split_once('-') {
        if let Some((start, end)) = normalize_clock_range(a, b) {
            return format!("{start}-{end}");
        }
    }

    if let Some(clock) = normalize_clock(&s) {
        return if approximate {
            format!("~{clock}")
        } else {
            clock
        };
    }
    if let Some(clock) = first_clock_from_noisy_time(&s) {
        return clock;
    }
    if approximate && s != t {
        return format!("~{s}");
    }
    t.to_string()
}

fn normalize_clock(s: &str) -> Option<String> {
    let trimmed = s.trim();
    if let Some(clock) = normalize_stopwatch_time(trimmed) {
        return Some(clock);
    }
    let compact = if trimmed.chars().any(|c| matches!(c, ':' | '.' | ';' | ',')) {
        trimmed.chars().filter(|c| !c.is_whitespace()).collect()
    } else {
        trimmed.to_string()
    };
    let with_colon = if compact.contains(';') || compact.contains(',') {
        compact.replace([';', ','], ":")
    } else if let Some((m, sec)) = compact.split_once('m') {
        format!("{m}:{sec}")
    } else {
        compact
    };
    let parts: Vec<&str> = with_colon.trim().split([':', '.']).collect();
    match parts.as_slice() {
        [hhmm] if hhmm.len() == 2 && hhmm.chars().all(|c| c == '0') => Some("0:00".to_string()),
        [mmss] if mmss.len() == 3 && mmss.chars().all(|c| c.is_ascii_digit()) => {
            let m = mmss[..1].parse::<u32>().ok()?;
            let sec = mmss[1..].parse::<u32>().ok()?;
            (sec < 60).then(|| format!("{m}:{sec:02}"))
        }
        [m] if m.chars().all(|c| c.is_ascii_digit()) => {
            Some(format!("{}:00", m.parse::<u32>().ok()?))
        }
        ["", sec] if sec.chars().all(|c| c.is_ascii_digit()) => {
            let sec = sec.parse::<u32>().ok()?;
            (sec < 60).then(|| format!("0:{sec:02}"))
        }
        [m, ""] if m.chars().all(|c| c.is_ascii_digit()) => {
            Some(format!("{}:00", m.parse::<u32>().ok()?))
        }
        [m, sec]
            if !m.is_empty() && parts.iter().all(|p| p.chars().all(|c| c.is_ascii_digit())) =>
        {
            let m = m.parse::<u32>().ok()?;
            let sec = sec.parse::<u32>().ok()?;
            (sec < 60).then(|| format!("{m}:{sec:02}"))
        }
        // Seen from real imports as "0:2:40" and "00:00:22"; the leading zero
        // hour is accidental.
        [h, m, sec]
            if !h.is_empty() && parts.iter().all(|p| p.chars().all(|c| c.is_ascii_digit())) =>
        {
            let h = h.parse::<u32>().ok()?;
            if h != 0 {
                return None;
            }
            let m = m.parse::<u32>().ok()?;
            let sec = sec.parse::<u32>().ok()?;
            (sec < 60).then(|| format!("{m}:{sec:02}"))
        }
        _ => None,
    }
}

fn normalize_stopwatch_time(s: &str) -> Option<String> {
    let compact = s
        .chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect::<String>();
    let Some(m_pos) = compact.find('m') else {
        return None;
    };
    let minutes = compact[..m_pos].parse::<u32>().ok()?;
    let mut rest = compact[m_pos + 1..].trim_start_matches("in").to_string();
    rest = rest
        .trim_start_matches("ute")
        .trim_start_matches("utes")
        .trim_start_matches('s')
        .to_string();
    let seconds_text = rest
        .trim_end_matches("seconds")
        .trim_end_matches("second")
        .trim_end_matches("secs")
        .trim_end_matches("sec")
        .trim_end_matches('s');
    if seconds_text.is_empty() || !seconds_text.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let seconds = seconds_text.parse::<u32>().ok()?;
    (seconds < 60).then(|| format!("{minutes}:{seconds:02}"))
}

fn first_clock_from_noisy_time(s: &str) -> Option<String> {
    let clocks = s
        .split(|c: char| c.is_whitespace() || matches!(c, '|' | '/' | '(' | ')' | '[' | ']'))
        .map(|part| part.trim_matches([',', ';']))
        .filter_map(normalize_clock)
        .collect::<Vec<_>>();
    (clocks.len() > 1).then(|| clocks[0].clone())
}

fn normalize_clock_range(a: &str, b: &str) -> Option<(String, String)> {
    let a = a.trim();
    let b = b.trim();
    if !a.contains(':') && b.contains(':') && a.chars().all(|c| c.is_ascii_digit()) {
        let end = normalize_clock(b)?;
        return Some((format!("{}:00", a.parse::<u32>().ok()?), end));
    }
    if a.contains(':') && !b.contains(':') && b.chars().all(|c| c.is_ascii_digit()) {
        let start = normalize_clock(a)?;
        let minute = a.split([':', '.']).next()?.parse::<u32>().ok()?;
        let n = b.parse::<u32>().ok()?;
        let end = if n < 60 {
            format!("{minute}:{n:02}")
        } else {
            format!("{n}:00")
        };
        return Some((start, end));
    }
    if let (Some(start), Some(end)) = (normalize_clock(a), normalize_clock(b)) {
        return Some((start, end));
    }
    None
}

fn strip_html_tags(s: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '<' if chars.peek().is_some_and(|next| {
                next.is_ascii_alphabetic() || matches!(next, '/' | '!' | '?')
            }) =>
            {
                in_tag = true;
            }
            '>' if in_tag => in_tag = false,
            '>' => out.push(c),
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.trim().to_string()
}

fn decode_html_entities(s: &str) -> String {
    s.replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
}

fn normalize_note_with_flags(note: &str) -> (String, bool, bool) {
    let decoded = decode_html_entities(note)
        .replace("<br>", "\n")
        .replace("<br/>", "\n")
        .replace("<br />", "\n");
    let with_img_tokens = replace_img_tags_with_icon_tokens(&decoded);
    let stripped = strip_html_tags(&with_img_tokens);
    let normalized_icons = normalize_note_icon_tokens(&stripped);
    let normalized = normalized_icons
        .lines()
        .map(normalize_note_line_spacing)
        .flat_map(|line| split_note_action_clauses(&line))
        .filter(|l| !is_decorative_note_separator(l))
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    (
        normalized,
        decoded != note || with_img_tokens != decoded || stripped != with_img_tokens,
        normalized_icons != stripped,
    )
}

fn split_note_action_clauses(line: &str) -> Vec<String> {
    let split_semicolon = split_note_delimiter(line, ';');
    let mut out = Vec::new();
    for part in split_semicolon {
        out.extend(split_spaced_pipe_notes(&part));
    }
    out
}

fn split_note_delimiter(line: &str, delimiter: char) -> Vec<String> {
    let parts = line
        .split(delimiter)
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    if parts.len() < 2 {
        return vec![line.trim().to_string()];
    }
    parts.into_iter().map(str::to_string).collect()
}

fn split_spaced_pipe_notes(line: &str) -> Vec<String> {
    let parts = line
        .split(" | ")
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    if parts.len() < 2 {
        return vec![line.trim().to_string()];
    }
    parts.into_iter().map(str::to_string).collect()
}

fn is_decorative_note_separator(line: &str) -> bool {
    let s = line.trim();
    s.len() >= 3
        && s.chars().all(|c| {
            c.is_whitespace()
                || matches!(
                    c,
                    '-' | '–' | '—' | '―' | '_' | '=' | '*' | '·' | '•' | '|' | '+' | '~'
                )
        })
        && s.chars().filter(|c| !c.is_whitespace()).count() >= 3
}

fn normalize_note_line_spacing(line: &str) -> String {
    let mut out = String::new();
    let mut pending_space = false;
    let cleaned = strip_note_leading_arrow_marker(strip_note_list_marker(line));
    let chars: Vec<char> = cleaned.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            pending_space = true;
            i += 1;
            continue;
        }
        if c == '@' {
            if let Some(close_rel) = chars[i + 1..].iter().position(|ch| *ch == '@') {
                let close = i + 1 + close_rel;
                let token: String = chars[i + 1..close].iter().collect();
                if is_plausible_local_icon_token(&token) || token == "time/time.png" {
                    if !out.is_empty()
                        && !out.ends_with(' ')
                        && !out.ends_with('(')
                        && !out.ends_with('[')
                    {
                        out.push(' ');
                    }
                    out.push('@');
                    out.push_str(&token);
                    out.push('@');
                    if chars.get(close + 1).is_some_and(|next| {
                        !next.is_whitespace()
                            && !matches!(next, ',' | '.' | ';' | ':' | ')' | ']' | '!' | '?')
                    }) {
                        pending_space = true;
                    } else {
                        pending_space = false;
                    }
                    i = close + 1;
                    continue;
                }
            }
        }
        if pending_space && !out.is_empty() {
            out.push(' ');
        }
        out.push(c);
        pending_space = false;
        i += 1;
    }
    normalize_common_note_terms(&normalize_note_punctuation_spacing(&out))
}

fn normalize_note_punctuation_spacing(line: &str) -> String {
    let line = normalize_note_arrows(&trim_decorative_note_edges(line));
    let chars: Vec<char> = line.chars().collect();
    let mut out = String::new();
    for (i, c) in chars.iter().enumerate() {
        out.push(*c);
        if *c == ',' {
            let Some(next) = chars.get(i + 1) else {
                continue;
            };
            if !next.is_whitespace()
                && !matches!(next, ',' | '.' | ';' | ':' | ')' | ']' | '!' | '?')
            {
                out.push(' ');
            }
        }
    }
    out.replace(" ,", ",")
        .replace(" :", ":")
        .replace("::", ":")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn trim_decorative_note_edges(line: &str) -> String {
    let mut s = line.trim().to_string();
    if is_decorative_note_separator(&s) {
        return s;
    }
    s = trim_decorative_note_start(&s).to_string();
    trim_decorative_note_end(&s).to_string()
}

fn trim_decorative_note_start(s: &str) -> &str {
    let mut end = 0;
    let mut count = 0;
    for (idx, c) in s.char_indices() {
        if matches!(c, '-' | '–' | '—' | '―' | '_' | '=' | '*' | '~') {
            count += 1;
            end = idx + c.len_utf8();
        } else {
            break;
        }
    }
    if count >= 3 {
        let rest = s[end..].trim_start();
        if !rest.is_empty() {
            return rest;
        }
    }
    s
}

fn trim_decorative_note_end(s: &str) -> &str {
    let mut start = s.len();
    let mut count = 0;
    for (idx, c) in s.char_indices().rev() {
        if matches!(c, '-' | '–' | '—' | '―' | '_' | '=' | '*' | '~') {
            count += 1;
            start = idx;
        } else {
            break;
        }
    }
    if count >= 3 {
        let rest = s[..start].trim_end();
        if !rest.is_empty() {
            return rest;
        }
    }
    s
}

fn normalize_note_arrows(line: &str) -> String {
    let mut s = line.replace("-->", "->").replace(">>", "->");
    for arrow in ["=>", "->", "⮞", "→"] {
        s = s
            .split(arrow)
            .map(str::trim)
            .collect::<Vec<_>>()
            .join(&format!(" {arrow} "));
    }
    s
}

fn normalize_common_note_terms(line: &str) -> String {
    line.split_whitespace()
        .map(|word| {
            let leading_len = word
                .chars()
                .take_while(|c| !c.is_alphanumeric())
                .map(char::len_utf8)
                .sum::<usize>();
            let trailing_len = word
                .chars()
                .rev()
                .take_while(|c| !c.is_alphanumeric())
                .map(char::len_utf8)
                .sum::<usize>();
            let core_end = word.len().saturating_sub(trailing_len);
            let leading = &word[..leading_len.min(word.len())];
            let core = &word[leading_len.min(core_end)..core_end];
            let trailing = &word[core_end..];
            let replacement = match core.to_ascii_lowercase().as_str() {
                "hordinculture" => Some("Horticulture"),
                "wheelbarror" => Some("Wheelbarrow"),
                "wheebarrow" => Some("Wheelbarrow"),
                "lumbercamp" => Some("lumber camp"),
                "possibel" => Some("possible"),
                "ennemie" => Some("enemy"),
                "contruire" => Some("construire"),
                "villnext" => Some("Vill next"),
                "deers" => Some("deer"),
                "berrys" => Some("berries"),
                "concil" => Some("Council"),
                "harrass" => Some("harass"),
                "cowboom" => Some("Cow boom"),
                _ => None,
            };
            replacement
                .map(|fixed| format!("{leading}{fixed}{trailing}"))
                .unwrap_or_else(|| word.to_string())
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn strip_note_leading_arrow_marker(line: &str) -> &str {
    let mut s = line.trim_start();
    for marker in ["=>", "->", "<-", "→", "⮞", ">"] {
        if let Some(rest) = s.strip_prefix(marker) {
            let trimmed = rest.trim_start();
            if !trimmed.is_empty() {
                s = trimmed;
            }
            break;
        }
    }
    s
}

fn strip_note_list_marker(line: &str) -> &str {
    let s = line.trim();
    let Some(first) = s.chars().next() else {
        return s;
    };
    if matches!(first, '-' | '*' | '•') {
        let rest = s[first.len_utf8()..].trim_start();
        if rest
            .chars()
            .next()
            .is_some_and(|c| !matches!(c, '-' | '*' | '•' | '–' | '—' | '―' | '_' | '=' | '|'))
        {
            return strip_note_task_marker(rest);
        }
        return s;
    }
    let mut digits_end = 0;
    for (idx, c) in s.char_indices() {
        if c.is_ascii_digit() {
            digits_end = idx + c.len_utf8();
        } else {
            break;
        }
    }
    if digits_end == 0 || digits_end > 3 {
        return s;
    }
    let rest = &s[digits_end..];
    let Some(marker) = rest.chars().next() else {
        return s;
    };
    if matches!(marker, '.' | ')') {
        let after = rest[marker.len_utf8()..].trim_start();
        if !after.is_empty() {
            return strip_note_task_marker(after);
        }
    }
    strip_note_task_marker(s)
}

fn strip_note_task_marker(s: &str) -> &str {
    let rest = s
        .strip_prefix("[ ]")
        .or_else(|| s.strip_prefix("[x]"))
        .or_else(|| s.strip_prefix("[X]"));
    rest.map(str::trim_start)
        .filter(|rest| !rest.is_empty())
        .unwrap_or(s)
}

fn replace_img_tags_with_icon_tokens(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(open_rel) = rest.to_ascii_lowercase().find("<img") {
        let open = open_rel;
        out.push_str(&rest[..open]);
        let Some(close_rel) = rest[open..].find('>') else {
            out.push_str(&rest[open..]);
            return out;
        };
        let close = open + close_rel + 1;
        let tag = &rest[open..close];
        if let Some(src) = html_attr_value(tag, "src").and_then(|src| normalize_icon_token(&src)) {
            out.push('@');
            out.push_str(&src);
            out.push('@');
        }
        rest = &rest[close..];
    }
    out.push_str(rest);
    out
}

fn html_attr_value(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let mut search_from = 0;
    while let Some(pos_rel) = lower[search_from..].find(name) {
        let pos = search_from + pos_rel;
        let before = tag[..pos].chars().next_back();
        let after = tag[pos + name.len()..].chars().next();
        if before.is_some_and(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
            || after.is_some_and(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
        {
            search_from = pos + name.len();
            continue;
        }
        let mut rest = tag[pos + name.len()..].trim_start();
        rest = rest.strip_prefix('=')?.trim_start();
        let quote = rest.chars().next()?;
        if quote == '"' || quote == '\'' {
            let value = &rest[quote.len_utf8()..];
            let end = value.find(quote)?;
            return Some(value[..end].to_string());
        }
        let end = rest
            .find(|c: char| c.is_whitespace() || c == '>')
            .unwrap_or(rest.len());
        return Some(rest[..end].to_string());
    }
    None
}

fn normalize_note_icon_tokens(note: &str) -> String {
    let mut out = String::new();
    let mut i = 0;
    while let Some(open_rel) = note[i..].find('@') {
        let open = i + open_rel;
        out.push_str(&note[i..open]);
        let Some(close_rel) = note[open + 1..].find('@') else {
            i = open + 1;
            continue;
        };
        let close = open + 1 + close_rel;
        let token = &note[open + 1..close];
        if let Some(token) = normalize_icon_token(token) {
            out.push('@');
            out.push_str(&token);
            out.push('@');
            i = close + 1;
        } else {
            out.push_str(token);
            if let Some((token, next_i)) = inline_icon_after_invalid_token(note, close + 1) {
                out.push('@');
                out.push_str(&token);
                out.push('@');
                i = next_i;
            } else {
                i = close + 1;
            }
        }
    }
    out.push_str(&note[i..]);
    out
}

fn inline_icon_after_invalid_token(note: &str, start: usize) -> Option<(String, usize)> {
    let rest = note.get(start..)?;
    let mut end = start;
    for (idx, c) in rest.char_indices() {
        if c.is_whitespace() || c == '@' {
            break;
        }
        end = start + idx + c.len_utf8();
    }
    if end == start {
        return None;
    }
    let token = normalize_icon_token(note.get(start..end)?)?;
    let next_i = if note.get(end..).is_some_and(|tail| tail.starts_with('@')) {
        end + 1
    } else {
        end
    };
    Some((token, next_i))
}

fn normalize_icon_token(token: &str) -> Option<String> {
    let clean = token.trim().trim_matches(['"', '\'', '<', '>']);
    if let Some(asset_token) = aoe4guides_asset_icon_token(clean) {
        return Some(asset_token);
    }
    let file = clean
        .rsplit('/')
        .next()?
        .split(['?', '#'])
        .next()?
        .trim()
        .to_ascii_lowercase();
    let stem = file
        .strip_suffix(".webp")
        .or_else(|| file.strip_suffix(".png"))
        .or_else(|| file.strip_suffix(".jpg"))
        .unwrap_or(&file);
    if let Some(alias) = ICON_ALIASES
        .iter()
        .find_map(|(from, to)| (*from == stem).then_some(*to))
    {
        return Some(alias.to_string());
    }
    let lower = clean.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        return Some(clean.to_string());
    }
    if !is_plausible_local_icon_token(clean) {
        return None;
    }
    strip_local_icon_extension(clean).or_else(|| bare_local_icon_token(clean))
}

fn time_age_icon_value(time: &str) -> Option<i64> {
    let lower = time.to_ascii_lowercase();
    if !lower.contains("<img") && !time.trim().starts_with('@') {
        return None;
    }
    let decoded = decode_html_entities(time);
    let with_img_tokens = replace_img_tags_with_icon_tokens(&decoded);
    let stripped = strip_html_tags(&with_img_tokens);
    let normalized = normalize_note_icon_tokens(&stripped).to_ascii_lowercase();
    (1..=4).find(|age| {
        normalized.contains(&format!("age/age_{age}"))
            || normalized.contains(&format!("age_{age}.webp"))
            || normalized.contains(&format!("age_{age}.png"))
    })
}

fn aoe4guides_asset_icon_token(token: &str) -> Option<String> {
    let path = token.split(['?', '#']).next()?.replace('\\', "/");
    let lower = path.to_ascii_lowercase();
    let marker = "/assets/pictures/";
    let tail = if let Some(pos) = lower.find(marker) {
        &path[pos + marker.len()..]
    } else if lower.starts_with("assets/pictures/") {
        &path["assets/pictures/".len()..]
    } else {
        return None;
    };
    if !is_plausible_local_icon_token(tail) {
        return None;
    }
    strip_local_icon_extension(tail)
}

fn bare_local_icon_token(token: &str) -> Option<String> {
    token
        .chars()
        .any(|c| c.is_ascii_alphabetic())
        .then(|| token.to_string())
}

fn is_plausible_local_icon_token(token: &str) -> bool {
    token.contains('/')
        && !token.chars().any(char::is_whitespace)
        && token
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '_' | '-' | '.'))
}

fn strip_local_icon_extension(token: &str) -> Option<String> {
    let lower = token.to_ascii_lowercase();
    if lower == "time/time.png" {
        return Some(token.to_string());
    }
    for suffix in [".webp", ".png", ".jpg", ".jpeg"] {
        if lower.ends_with(suffix) {
            return Some(token[..token.len() - suffix.len()].to_string());
        }
    }
    None
}

const ICON_ALIASES: &[(&str, &str)] = &[
    ("villagerad", "unit_worker/villager-abbasid"),
    ("villagerch", "unit_worker/villager-china"),
    ("villagerma", "unit_worker/villager-malians"),
    ("villagermo", "unit_worker/villager-mongols"),
    ("villager", "unit_worker/villager"),
    ("villagera", "unit_worker/villager"),
    ("villagerfr", "unit_worker/villager"),
    ("villagerru", "unit_worker/villager"),
    ("resourcewoodicon", "resource/resource_wood"),
    ("resourcewood", "resource/resource_wood"),
    ("resourcegoldicon", "resource/resource_gold"),
    ("resourcegold", "resource/resource_gold"),
    ("resourcestoneicon", "resource/resource_stone"),
    ("resourcestone", "resource/resource_stone"),
    ("resourcefoodicon", "resource/resource_food"),
    ("resourcefood", "resource/resource_food"),
    ("sheep", "resource/sheep"),
    ("berrybush", "resource/berrybush"),
    ("berry", "resource/berrybush"),
    ("rally", "resource/rally"),
    ("age2dark", "age/age_2"),
    ("age2", "age/age_2"),
    ("age3", "age/age_3"),
    ("age4", "age/age_4"),
    ("goldenagetier1", "age/goldenagetier1"),
    ("house", "building_economy/house"),
    ("town_center", "building_economy/town-center"),
    ("towncenter", "building_economy/town-center"),
    ("mill", "building_economy/mill"),
    ("lumbercamp", "building_economy/lumber-camp"),
    ("lumber_camp", "building_economy/lumber-camp"),
    ("miningcamp", "building_economy/mining-camp"),
    ("mining_camp", "building_economy/mining-camp"),
    ("farm", "building_economy/farm"),
    ("buildingovoomon", "building_mongols/ovoo"),
    ("ovoo", "building_mongols/ovoo"),
    ("ger", "building_mongols/ger"),
    ("outpost", "building_defensive/outpost"),
    ("spearman1", "unit_infantry/spearman-1"),
    ("spearman", "unit_infantry/spearman-1"),
    ("freshfoodstuffs", "technology_abbasid/fresh-foodstuffs"),
    ("fertilecrescent", "technology_abbasid/fertile-crescent-2"),
    (
        "fertile-crescent-2",
        "technology_abbasid/fertile-crescent-2",
    ),
    ("wheelbarrow", "technology_economy/wheelbarrow"),
    ("doublebroadaxe", "technology_economy/double-broadaxe"),
    ("double-broadaxe", "technology_economy/double-broadaxe"),
    ("horticulture", "technology_economy/horticulture"),
    (
        "professionalscouts",
        "technology_economy/professional-scouts",
    ),
    (
        "professional-scouts",
        "technology_economy/professional-scouts",
    ),
    ("gristmill", "building_economy/mill"),
    ("goldminingcamp", "building_economy/mining-camp"),
    ("imperialofficial", "unit_chinese/imperial-official"),
    ("stable", "building_military/stable"),
    ("archer2", "unit_infantry/archer-2"),
    (
        "buildinglandmarkage1academy",
        "landmark_chinese/imperial-academy",
    ),
    (
        "buildinglandmarkhouseofwisdom",
        "landmark_abbasid/house-of-wisdom",
    ),
    ("woodgather1", "technology_economy/forestry"),
    ("monastery", "building_religious/monastery"),
    ("supervise", "ability_chinese/supervise"),
    ("blacksmith", "building_technology/blacksmith"),
    (
        "buildinglandmarkage3zhengyangmen",
        "landmark_chinese/enclave-of-the-emperor",
    ),
    ("abbey-of-kings", "landmark_english/abbey-of-kings"),
    ("deer", "resource/deer"),
    ("horseman1", "unit_cavalry/horseman-1"),
    ("village", "building_chinese/village"),
    ("barracks", "building_military/barracks"),
    ("boar", "resource/boar"),
    ("age3dark", "age/age_3"),
    ("huntingvillager", "unit_worker/villager"),
    (
        "buildingwonderage1deerstonesmon",
        "landmark_mongols/deer-stones",
    ),
];

fn is_blank_step(s: &Value) -> bool {
    let has_note = s["notes"].as_array().is_some_and(|notes| {
        notes
            .iter()
            .any(|n| n.as_str().is_some_and(|n| !n.trim().is_empty()))
    });
    has_note
        || s.get("time").is_some()
        || s["age"].as_i64().is_some_and(|v| v > 0)
        || s["villager_count"].as_i64().is_some_and(|v| v >= 0)
        || s["population_count"].as_i64().is_some_and(|v| v >= 0)
        || ["food", "wood", "gold", "stone"]
            .iter()
            .any(|key| s["resources"][key].as_i64().is_some_and(|v| v > 0))
        || s["resources"]["builder"].as_i64().is_some_and(|v| v >= 0)
}

fn prepend_step_note(step: &mut Value, note: &str) {
    let note = note.trim();
    if note.is_empty() {
        return;
    }
    if !step["notes"].is_array() {
        step["notes"] = Value::Array(Vec::new());
    }
    if let Some(notes) = step["notes"].as_array_mut() {
        if !notes.iter().any(|n| n.as_str() == Some(note)) {
            notes.insert(0, Value::from(note.to_string()));
        }
    }
}

fn normalize_imported_json(bo: &mut Value) -> Vec<String> {
    let mut repairs = Vec::new();
    normalize_build_shape(bo, &mut repairs);
    let Some(steps) = bo["build_order"].as_array_mut() else {
        return repairs;
    };
    for (i, s) in steps.iter_mut().enumerate() {
        let step_no = i + 1;
        if let Some(t) = s["time"].as_str().map(str::to_string) {
            if let Some(age) = time_age_icon_value(&t) {
                if parse_ageish(&s["age"]) != Some(age) {
                    s["age"] = Value::from(age);
                }
                if let Some(obj) = s.as_object_mut() {
                    obj.remove("time");
                }
                repairs.push(format!(
                    "Moved age icon from time into age on step {step_no}."
                ));
            } else {
                let fixed = normalize_time(&t);
                if fixed.is_empty() {
                    if let Some(obj) = s.as_object_mut() {
                        obj.remove("time");
                    }
                    repairs.push(format!("Removed invalid HTML time from step {step_no}."));
                } else if !is_normalized_time(&fixed) {
                    if let Some(obj) = s.as_object_mut() {
                        obj.remove("time");
                    }
                    prepend_step_note(s, &fixed);
                    repairs.push(format!(
                        "Moved non-time time value into notes on step {step_no}."
                    ));
                } else if fixed != t {
                    s["time"] = Value::from(fixed);
                    repairs.push(format!("Normalized time on step {step_no}."));
                }
            }
        }
        if let Some(notes) = s["notes"].as_array_mut() {
            let before = notes.len();
            let mut split_multiline = false;
            let mut normalized_notes = Vec::new();
            for note in std::mem::take(notes) {
                if let Some(raw) = note.as_str() {
                    let (fixed, cleaned_markup, mapped_icon) = normalize_note_with_flags(raw);
                    if fixed != raw {
                        if cleaned_markup {
                            repairs.push(format!("Cleaned note markup on step {step_no}."));
                        }
                        if mapped_icon {
                            repairs.push(format!("Mapped icon token on step {step_no}."));
                        }
                    }
                    let mut lines = fixed
                        .lines()
                        .filter(|line| !line.trim().is_empty())
                        .peekable();
                    if lines.peek().is_some() {
                        let mut count = 0;
                        for line in lines {
                            count += 1;
                            normalized_notes.push(Value::from(line.to_string()));
                        }
                        if count > 1 {
                            split_multiline = true;
                        }
                    }
                } else {
                    normalized_notes.push(note);
                }
            }
            *notes = normalized_notes;
            if notes.len() < before {
                repairs.push(format!("Removed blank note on step {step_no}."));
            }
            if split_multiline {
                repairs.push(format!("Split multiline note on step {step_no}."));
            }
        }
        normalize_step_counts(s, step_no, &mut repairs);
        normalize_step_resources(s, step_no, &mut repairs);
        fill_resource_counts_from_notes(s, step_no, &mut repairs);
        fill_time_and_age_from_notes(s, step_no, &mut repairs);
        cleanup_duplicate_note_prefixes(s, step_no, &mut repairs);
    }
    let before = steps.len();
    steps.retain(is_blank_step);
    if steps.len() != before {
        repairs.push(format!(
            "Removed {} blank step{}.",
            before - steps.len(),
            if before - steps.len() == 1 { "" } else { "s" }
        ));
    }
    repairs.sort();
    repairs.dedup();
    repairs
}

fn fill_time_and_age_from_notes(step: &mut Value, step_no: usize, repairs: &mut Vec<String>) {
    let current_time = step["time"].as_str().map(str::to_string);
    let current_age = parse_ageish(&step["age"]);
    let mut inferred_time = None;
    let mut inferred_age = current_age;
    let mut changed_notes = false;

    let before_after = {
        let Some(notes) = step["notes"].as_array_mut() else {
            return;
        };
        let before = notes.len();
        let mut kept = Vec::new();
        for note in std::mem::take(notes) {
            let Some(raw) = note.as_str() else {
                kept.push(note);
                continue;
            };
            if current_time.is_none() && inferred_time.is_none() {
                if let Some(time) = pure_clock_note(raw) {
                    inferred_time = Some(time);
                    changed_notes = true;
                    continue;
                }
            }
            if inferred_age.is_none() {
                if let Some(age) = age_icon_in_note(raw) {
                    inferred_age = Some(age);
                    if pure_age_icon_note(raw) {
                        changed_notes = true;
                        continue;
                    }
                }
            } else if pure_age_icon_note(raw) {
                changed_notes = true;
                continue;
            }
            kept.push(Value::from(raw.to_string()));
        }
        *notes = kept;
        (before, notes.len())
    };

    if let Some(time) = inferred_time {
        step["time"] = Value::from(time);
        repairs.push(format!("Filled step {step_no} time from note hint."));
    }
    if current_age.is_none() {
        if let Some(age) = inferred_age {
            step["age"] = Value::from(age);
            repairs.push(format!("Filled step {step_no} age from note hint."));
        }
    }
    if changed_notes || before_after.1 != before_after.0 {
        repairs.push(format!("Removed duplicate timing note on step {step_no}."));
    }
}

fn pure_clock_note(note: &str) -> Option<String> {
    let raw = note.trim();
    if !raw
        .chars()
        .all(|c| c.is_ascii_digit() || matches!(c, ':' | '.' | '-' | '–' | '—' | '‑' | ' '))
    {
        return None;
    }
    let fixed = normalize_time(raw);
    (!fixed.is_empty() && is_normalized_time(&fixed)).then_some(fixed)
}

fn age_icon_in_note(note: &str) -> Option<i64> {
    let lower = note.to_ascii_lowercase();
    for age in 1..=4 {
        if lower.contains(&format!("@age/age_{age}@")) {
            return Some(age);
        }
    }
    None
}

fn pure_age_icon_note(note: &str) -> bool {
    let stripped = note
        .replace("@age/age_1@", "")
        .replace("@age/age_2@", "")
        .replace("@age/age_3@", "")
        .replace("@age/age_4@", "");
    stripped.chars().all(|c| {
        c.is_whitespace() || matches!(c, '{' | '}' | '[' | ']' | '(' | ')' | ':' | '-' | '–' | '—')
    })
}

fn fill_resource_counts_from_notes(step: &mut Value, step_no: usize, repairs: &mut Vec<String>) {
    let counts = note_resource_counts(step);
    if counts.is_empty() {
        return;
    }
    if !step["resources"].is_object() {
        step["resources"] = json!({
            "food": -1,
            "wood": -1,
            "gold": -1,
            "stone": -1,
            "builder": -1,
        });
    }
    for (key, count) in counts {
        if step["resources"][key]
            .as_i64()
            .is_none_or(|value| value <= 0)
        {
            step["resources"][key] = Value::from(count);
            repairs.push(format!(
                "Filled step {step_no} {key} allocation from note hint."
            ));
        }
    }
}

fn note_resource_counts(step: &Value) -> Vec<(&'static str, i64)> {
    let Some(notes) = step["notes"].as_array() else {
        return Vec::new();
    };
    let mut counts = Vec::new();
    for note in notes.iter().filter_map(Value::as_str) {
        for key in ["food", "wood", "gold", "stone"] {
            let icon = format!("@resource/resource_{key}@");
            if let Some(count) = resource_icon_colon_count(note, &icon) {
                counts.push((key, count));
            } else if let Some(count) = rally_resource_icon_count(note, &icon) {
                counts.push((key, count));
            }
        }
    }
    counts
}

fn resource_icon_colon_count(note: &str, icon: &str) -> Option<i64> {
    let start = note.find(icon)? + icon.len();
    let rest = note[start..].trim_start();
    let rest = rest.strip_prefix(':')?.trim_start();
    leading_positive_int(rest)
}

fn rally_resource_icon_count(note: &str, icon: &str) -> Option<i64> {
    let trimmed = note.trim();
    let rest = trimmed.strip_prefix("@resource/rally@")?.trim_start();
    let rest = rest.strip_prefix(icon)?.trim_start();
    let count = leading_positive_int(rest)?;
    let after = rest
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .map(char::len_utf8)
        .sum::<usize>();
    (rest[after..].trim().is_empty()).then_some(count)
}

fn leading_positive_int(s: &str) -> Option<i64> {
    let digits = s
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect::<String>();
    let value = digits.parse::<i64>().ok()?;
    (value > 0).then_some(value)
}

fn cleanup_duplicate_note_prefixes(step: &mut Value, step_no: usize, repairs: &mut Vec<String>) {
    let current_age = parse_ageish(&step["age"]);
    let resources = step["resources"].clone();
    let mut changed = false;
    let mut new_age = current_age;
    let Some((before, after)) = (|| {
        let notes = step["notes"].as_array_mut()?;
        let before = notes.len();
        let mut cleaned_notes = Vec::new();
        for note in std::mem::take(notes) {
            let Some(raw) = note.as_str() else {
                cleaned_notes.push(note);
                continue;
            };
            let mut fixed = raw.to_string();
            if let Some(cleaned) = strip_duplicate_allocation_note_prefix(&fixed, &resources) {
                fixed = cleaned;
                changed = true;
            }
            if let Some((age, cleaned)) = strip_duplicate_age_note_prefix(&fixed, new_age) {
                fixed = cleaned;
                if new_age.is_none() {
                    new_age = Some(age);
                }
                changed = true;
            }
            let fixed = fixed.trim().to_string();
            if !fixed.is_empty() {
                cleaned_notes.push(Value::from(fixed));
            }
        }
        *notes = cleaned_notes;
        Some((before, notes.len()))
    })() else {
        return;
    };
    if let Some(age) = new_age {
        if current_age.is_none() {
            step["age"] = Value::from(age);
        }
    }
    if changed {
        repairs.push(format!(
            "Removed duplicate note metadata on step {step_no}."
        ));
    }
    if after < before {
        repairs.push(format!("Removed blank note on step {step_no}."));
    }
}

fn strip_duplicate_allocation_note_prefix(line: &str, resources: &Value) -> Option<String> {
    let trimmed = line.trim_start();
    let token_end = trimmed
        .char_indices()
        .find_map(|(idx, c)| c.is_whitespace().then_some(idx))
        .unwrap_or(trimmed.len());
    let candidate = &trimmed[..token_end];
    let parsed = parse_resource_allocations(candidate)?;
    if !resource_allocations_match(&parsed, resources) {
        return None;
    }
    let rest = trimmed[token_end..].trim_start();
    let lower = rest.to_ascii_lowercase();
    let Some(after_split) = lower.strip_prefix("split") else {
        return None;
    };
    let consumed = rest.len() - after_split.len();
    let cleaned = rest[consumed..]
        .trim_start_matches(|c: char| {
            c.is_whitespace() || matches!(c, ',' | ':' | ';' | '-' | '–' | '—' | '>')
        })
        .trim_start()
        .to_string();
    (!cleaned.is_empty()).then_some(cleaned)
}

fn resource_allocations_match(parsed: &Value, resources: &Value) -> bool {
    ["food", "wood", "gold", "stone"].iter().all(|key| {
        parsed[*key].as_i64().is_some()
            && resources[*key].as_i64().is_some()
            && parsed[*key].as_i64() == resources[*key].as_i64()
    }) && (parsed["builder"].as_i64().unwrap_or(-1) < 0
        || parsed["builder"].as_i64() == resources["builder"].as_i64())
}

fn strip_duplicate_age_note_prefix(line: &str, current_age: Option<i64>) -> Option<(i64, String)> {
    let trimmed = line.trim_start();
    for width in [2usize, 1] {
        let Some((candidate, consumed)) = leading_token_prefix(trimmed, width) else {
            continue;
        };
        if !is_note_age_prefix_candidate(&candidate) {
            continue;
        }
        if candidate.is_empty() {
            continue;
        }
        let Some(age) = parse_age_heading_text(&candidate) else {
            continue;
        };
        if current_age.is_some_and(|existing| existing != age) {
            continue;
        }
        let rest = trimmed[consumed..]
            .trim_start_matches(|c: char| {
                c.is_whitespace() || matches!(c, ':' | '-' | '–' | '—' | '>')
            })
            .trim_start()
            .to_string();
        if !rest.is_empty() {
            return Some((age, rest));
        }
    }
    None
}

fn is_note_age_prefix_candidate(candidate: &str) -> bool {
    let lower = candidate.trim().to_ascii_lowercase();
    lower.starts_with("age ")
        || matches!(
            lower.as_str(),
            "dark"
                | "dark age"
                | "feudal"
                | "feudal age"
                | "castle"
                | "castle age"
                | "imperial"
                | "imperial age"
        )
}

fn parse_intish(v: &Value) -> Option<i64> {
    if let Some(n) = v.as_i64() {
        return Some(n);
    }
    let s = v.as_str()?.trim();
    let digits: String = s
        .chars()
        .skip_while(|c| c.is_whitespace())
        .take_while(|c| c.is_ascii_digit() || *c == '-')
        .collect();
    digits.parse().ok()
}

fn parse_ageish(v: &Value) -> Option<i64> {
    if let Some(n) = parse_intish(v) {
        return (1..=4).contains(&n).then_some(n);
    }
    let raw = v.as_str()?.trim();
    parse_ageish_text(raw)
}

fn parse_ageish_text(raw: &str) -> Option<i64> {
    let key: String = raw
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect();
    match key.as_str() {
        "i" | "agei" | "age1" | "ageone" | "one" | "dark" | "darkage" => Some(1),
        "ii" | "ageii" | "age2" | "agetwo" | "two" | "feudal" | "feudalage" => Some(2),
        "iii" | "ageiii" | "age3" | "agethree" | "three" | "castle" | "castleage" => Some(3),
        "iv" | "ageiv" | "age4" | "agefour" | "four" | "imperial" | "imperialage" => Some(4),
        _ => parse_ageish_phrase(&key),
    }
}

fn parse_ageish_phrase(key: &str) -> Option<i64> {
    if ["imperialage", "ageimperial", "agefour", "age4", "iv"]
        .iter()
        .any(|token| key.contains(token))
    {
        return Some(4);
    }
    if ["castleage", "agecastle", "agethree", "age3", "iii"]
        .iter()
        .any(|token| key.contains(token))
    {
        return Some(3);
    }
    if ["feudalage", "agefeudal", "agetwo", "age2", "ii"]
        .iter()
        .any(|token| key.contains(token))
    {
        return Some(2);
    }
    if ["darkage", "agedark", "ageone", "age1", "i"]
        .iter()
        .any(|token| key.contains(token))
    {
        return Some(1);
    }
    None
}

fn note_alias_value(v: &Value) -> Option<Value> {
    if let Some(s) = v.as_str() {
        return Some(json!([s]));
    }
    let items = v.as_array()?;
    let notes = items
        .iter()
        .filter_map(|item| {
            if item.is_null() {
                None
            } else if let Some(s) = item.as_str() {
                Some(Value::from(s))
            } else if item.is_number() || item.is_boolean() {
                Some(Value::from(item.to_string()))
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    (!notes.is_empty()).then_some(Value::Array(notes))
}

fn normalized_time_value(v: &Value) -> Option<String> {
    let raw = v.as_str()?;
    let fixed = normalize_time(raw);
    is_normalized_time(&fixed).then_some(fixed)
}

fn is_normalized_time(s: &str) -> bool {
    let s = s.strip_prefix('~').unwrap_or(s);
    !s.is_empty() && s.split('-').all(is_clock_time)
}

fn is_clock_time(s: &str) -> bool {
    let Some((m, sec)) = s.split_once(':') else {
        return false;
    };
    !m.is_empty()
        && !sec.is_empty()
        && m.chars().all(|c| c.is_ascii_digit())
        && sec.chars().all(|c| c.is_ascii_digit())
        && sec.parse::<u32>().is_ok_and(|v| v < 60)
}

fn tuple_step_value(step: &Value) -> Option<Value> {
    let parts = step.as_array()?;
    match parts.as_slice() {
        [time, note] => Some(json!({
            "time": normalized_time_value(time)?,
            "notes": note_alias_value(note)?,
        })),
        [time, resources, note] => {
            if let (Some(time), Some(resources), Some(notes)) = (
                normalized_time_value(time),
                parse_resource_allocations_value(resources),
                note_alias_value(note),
            ) {
                return Some(json!({ "time": time, "resources": resources, "notes": notes }));
            }
            if let (Some(resources), Some(time), Some(notes)) = (
                parse_resource_allocations_value(time),
                normalized_time_value(resources),
                note_alias_value(note),
            ) {
                return Some(json!({ "time": time, "resources": resources, "notes": notes }));
            }
            if let (Some(time), Some(notes), Some(resources)) = (
                normalized_time_value(time),
                note_alias_value(resources),
                parse_resource_allocations_value(note),
            ) {
                return Some(json!({ "time": time, "resources": resources, "notes": notes }));
            }
            None
        }
        _ => None,
    }
}

fn normalize_step_counts(step: &mut Value, step_no: usize, repairs: &mut Vec<String>) {
    if let Some(n) = parse_ageish(&step["age"]) {
        if step["age"] != Value::from(n) {
            step["age"] = Value::from(n);
            repairs.push(format!("Normalized age on step {step_no}."));
        }
    }
    remove_negative_sentinel(step, "age", repairs);
    for alias in [
        "age_name",
        "ageName",
        "ageNumber",
        "age_number",
        "phase",
        "era",
    ] {
        if step["age"].is_null() {
            if let Some(n) = parse_ageish(&step[alias]) {
                step["age"] = Value::from(n);
                if let Some(obj) = step.as_object_mut() {
                    obj.remove(alias);
                }
                repairs.push(format!("Moved step {step_no} {alias} into age."));
            }
        } else if let (Some(alias_n), Some(age_n)) =
            (parse_ageish(&step[alias]), parse_ageish(&step["age"]))
        {
            if alias_n == age_n {
                if let Some(obj) = step.as_object_mut() {
                    obj.remove(alias);
                }
                repairs.push(format!("Removed duplicate step {step_no} {alias} age."));
            }
        }
    }

    for key in ["population_count", "villager_count"] {
        if let Some(n) = parse_intish(&step[key]) {
            if step[key] != Value::from(n) {
                step[key] = Value::from(n);
                repairs.push(format!("Normalized {key} on step {step_no}."));
            }
            remove_negative_sentinel(step, key, repairs);
        }
    }

    for (alias, canonical) in [
        ("population", "population_count"),
        ("populationCount", "population_count"),
        ("pop", "population_count"),
        ("pop_count", "population_count"),
        ("popCount", "population_count"),
        ("pop_cap", "population_count"),
        ("popCap", "population_count"),
        ("supply", "population_count"),
        ("supply_count", "population_count"),
        ("supplyCount", "population_count"),
        ("supply_used", "population_count"),
        ("supplyUsed", "population_count"),
        ("vill", "villager_count"),
        ("villager", "villager_count"),
        ("villagers", "villager_count"),
        ("vill_count", "villager_count"),
        ("villCount", "villager_count"),
        ("villagerCount", "villager_count"),
        ("vills", "villager_count"),
        ("worker", "villager_count"),
        ("workers", "villager_count"),
        ("worker_count", "villager_count"),
        ("workerCount", "villager_count"),
    ] {
        if step[alias]
            .as_str()
            .is_some_and(|value| value.trim().is_empty())
        {
            if let Some(obj) = step.as_object_mut() {
                obj.remove(alias);
            }
            repairs.push(format!("Removed blank step {step_no} {alias}."));
            continue;
        }
        if step[canonical].is_null() {
            if let Some(n) = parse_intish(&step[alias]) {
                step[canonical] = Value::from(n);
                if let Some(obj) = step.as_object_mut() {
                    obj.remove(alias);
                }
                repairs.push(format!("Moved step {step_no} {alias} into {canonical}."));
            }
        } else if let (Some(alias_n), Some(canonical_n)) =
            (parse_intish(&step[alias]), parse_intish(&step[canonical]))
        {
            if alias_n == canonical_n {
                if let Some(obj) = step.as_object_mut() {
                    obj.remove(alias);
                }
                repairs.push(format!("Removed duplicate step {step_no} {alias} count."));
            }
        }
    }
    for key in ["population_count", "villager_count"] {
        remove_negative_sentinel(step, key, repairs);
    }
}

fn remove_negative_sentinel(step: &mut Value, key: &str, repairs: &mut Vec<String>) {
    if step[key].as_i64().is_some_and(|n| n < 0) {
        if let Some(obj) = step.as_object_mut() {
            obj.remove(key);
        }
        repairs.push(format!("Removed hidden {key} sentinel."));
    }
}

fn normalize_step_resources(step: &mut Value, step_no: usize, repairs: &mut Vec<String>) {
    if let Some(resources) = parse_resource_allocations_value(&step["resources"]) {
        if step["resources"] != resources {
            step["resources"] = resources;
            repairs.push(format!("Converted step {step_no} resources."));
        }
    } else if step["resources"].is_null() {
        for alias in [
            "resource",
            "resource_allocation",
            "resourceAllocation",
            "resource_allocations",
            "resourceAllocations",
            "allocation",
            "allocations",
            "villager_allocation",
            "villagerAllocation",
            "villagers_on",
            "villagersOn",
            "eco",
            "economy",
        ] {
            if let Some(resources) = parse_resource_allocations_value(&step[alias]) {
                step["resources"] = resources;
                if let Some(obj) = step.as_object_mut() {
                    obj.remove(alias);
                }
                repairs.push(format!("Moved step {step_no} {alias} into resources."));
                break;
            }
        }
    }

    normalize_loose_step_resources(step, step_no, repairs);

    if !step["resources"].is_object() {
        return;
    }

    for (alias, canonical) in [
        ("f", "food"),
        ("w", "wood"),
        ("g", "gold"),
        ("s", "stone"),
        ("build", "builder"),
        ("builders", "builder"),
    ] {
        if step["resources"][canonical].is_null() {
            if let Some(n) = parse_intish(&step["resources"][alias]) {
                step["resources"][canonical] = Value::from(n);
                if let Some(obj) = step["resources"].as_object_mut() {
                    obj.remove(alias);
                }
                repairs.push(format!(
                    "Moved step {step_no} resource {alias} into {canonical}."
                ));
            }
        } else if let (Some(alias_n), Some(canonical_n)) = (
            parse_intish(&step["resources"][alias]),
            parse_intish(&step["resources"][canonical]),
        ) {
            if alias_n == canonical_n {
                if let Some(obj) = step["resources"].as_object_mut() {
                    obj.remove(alias);
                }
                repairs.push(format!(
                    "Removed duplicate step {step_no} resource {alias}."
                ));
            }
        }
    }

    for key in ["food", "wood", "gold", "stone", "builder"] {
        if step["resources"][key].is_null() {
            step["resources"][key] = Value::from(-1);
            repairs.push(format!(
                "Filled missing {key} allocation on step {step_no}."
            ));
        } else if let Some(n) = parse_intish(&step["resources"][key]) {
            if step["resources"][key] != Value::from(n) {
                step["resources"][key] = Value::from(n);
                repairs.push(format!("Normalized {key} allocation on step {step_no}."));
            }
        }
    }
}

fn normalize_loose_step_resources(step: &mut Value, step_no: usize, repairs: &mut Vec<String>) {
    let mut moves = Vec::new();
    for (alias, canonical) in [
        ("food", "food"),
        ("food_count", "food"),
        ("foodCount", "food"),
        ("f", "food"),
        ("wood", "wood"),
        ("wood_count", "wood"),
        ("woodCount", "wood"),
        ("w", "wood"),
        ("gold", "gold"),
        ("gold_count", "gold"),
        ("goldCount", "gold"),
        ("g", "gold"),
        ("stone", "stone"),
        ("stone_count", "stone"),
        ("stoneCount", "stone"),
        ("s", "stone"),
        ("builder", "builder"),
        ("builders", "builder"),
        ("build", "builder"),
        ("builder_count", "builder"),
        ("builderCount", "builder"),
    ] {
        if step[alias]
            .as_str()
            .is_some_and(|value| value.trim().is_empty())
        {
            if let Some(obj) = step.as_object_mut() {
                obj.remove(alias);
            }
            repairs.push(format!("Removed blank step {step_no} {alias}."));
            continue;
        }
        if step["resources"][canonical].is_null() {
            if let Some(n) = parse_intish(&step[alias]) {
                moves.push((alias, canonical, n));
            }
        } else if let (Some(alias_n), Some(canonical_n)) = (
            parse_intish(&step[alias]),
            parse_intish(&step["resources"][canonical]),
        ) {
            if alias_n == canonical_n {
                if let Some(obj) = step.as_object_mut() {
                    obj.remove(alias);
                }
                repairs.push(format!(
                    "Removed duplicate step {step_no} loose {alias} resource."
                ));
            }
        }
    }

    if moves.is_empty() {
        return;
    }

    if !step["resources"].is_object() {
        step["resources"] = json!({});
    }
    for (alias, canonical, n) in moves {
        if step["resources"][canonical].is_null() {
            step["resources"][canonical] = Value::from(n);
            if let Some(obj) = step.as_object_mut() {
                obj.remove(alias);
            }
            repairs.push(format!(
                "Moved step {step_no} loose {alias} into resources.{canonical}."
            ));
        }
    }
}

fn resource_allocation_label(label: &str) -> Option<&'static str> {
    let key = label
        .trim()
        .trim_matches(|c: char| !c.is_ascii_alphanumeric())
        .to_ascii_lowercase();
    match key.as_str() {
        "f" | "food" | "foods" => Some("food"),
        "w" | "wood" => Some("wood"),
        "g" | "gold" => Some("gold"),
        "s" | "stone" => Some("stone"),
        "b" | "build" | "builder" | "builders" => Some("builder"),
        _ => None,
    }
}

fn resource_allocation_tokens(s: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut token = String::new();
    for c in decode_html_entities(s).chars() {
        if c.is_ascii_alphanumeric() || (c == '-' && token.is_empty()) {
            token.push(c.to_ascii_lowercase());
        } else if !token.is_empty() {
            tokens.push(std::mem::take(&mut token));
        }
    }
    if !token.is_empty() {
        tokens.push(token);
    }
    tokens
}

fn parse_compact_resource_token(token: &str) -> Option<(&str, i64)> {
    let value_end = token
        .char_indices()
        .take_while(|(idx, c)| c.is_ascii_digit() || (*idx == 0 && *c == '-'))
        .last()
        .map(|(idx, c)| idx + c.len_utf8())
        .unwrap_or(0);
    if value_end > 0 && value_end < token.len() {
        let value = token[..value_end].parse::<i64>().ok()?;
        let label = resource_allocation_label(&token[value_end..])?;
        return Some((label, value));
    }

    let value_start = token.find(|c: char| c.is_ascii_digit() || c == '-')?;
    if value_start > 0 && value_start < token.len() {
        let label = resource_allocation_label(&token[..value_start])?;
        let value = token[value_start..].parse::<i64>().ok()?;
        return Some((label, value));
    }

    None
}

fn assign_resource_allocation_value(
    label: &str,
    value: i64,
    food: &mut Option<i64>,
    wood: &mut Option<i64>,
    gold: &mut Option<i64>,
    stone: &mut Option<i64>,
    builder: &mut Option<i64>,
) -> bool {
    match label {
        "food" => *food = Some(value),
        "wood" => *wood = Some(value),
        "gold" => *gold = Some(value),
        "stone" => *stone = Some(value),
        "builder" => *builder = Some(value),
        _ => return false,
    }
    true
}

fn parse_labeled_resource_allocations(s: &str) -> Option<Value> {
    let tokens = resource_allocation_tokens(s);
    if tokens.len() < 2 {
        return None;
    }

    let mut food = None;
    let mut wood = None;
    let mut gold = None;
    let mut stone = None;
    let mut builder = None;
    let mut matched = false;
    let mut i = 0;
    while i < tokens.len() {
        if let Some((label, value)) = parse_compact_resource_token(&tokens[i]) {
            matched |= assign_resource_allocation_value(
                label,
                value,
                &mut food,
                &mut wood,
                &mut gold,
                &mut stone,
                &mut builder,
            );
            i += 1;
            continue;
        }

        if let Ok(value) = tokens[i].parse::<i64>() {
            if let Some(next) = tokens.get(i + 1).and_then(|t| resource_allocation_label(t)) {
                matched |= assign_resource_allocation_value(
                    next,
                    value,
                    &mut food,
                    &mut wood,
                    &mut gold,
                    &mut stone,
                    &mut builder,
                );
                i += 2;
                continue;
            }
        }

        if let Some(label) = resource_allocation_label(&tokens[i]) {
            if let Some(value) = tokens.get(i + 1).and_then(|t| t.parse::<i64>().ok()) {
                matched |= assign_resource_allocation_value(
                    label,
                    value,
                    &mut food,
                    &mut wood,
                    &mut gold,
                    &mut stone,
                    &mut builder,
                );
                i += 2;
                continue;
            }
        }

        i += 1;
    }

    if !matched {
        return None;
    }

    Some(json!({
        "food": food?,
        "wood": wood?,
        "gold": gold?,
        "stone": stone?,
        "builder": builder.unwrap_or(-1),
    }))
}

fn parse_resource_allocations(s: &str) -> Option<Value> {
    if let Some(resources) = parse_labeled_resource_allocations(s) {
        return Some(resources);
    }

    let clean = s.trim().trim_start_matches('(').trim_end_matches(')');
    let delimiter = if clean.contains('/') {
        '/'
    } else if clean.contains(',') {
        ','
    } else if is_dash_resource_allocation(clean) {
        '-'
    } else {
        return None;
    };
    let nums: Vec<i64> = clean
        .split(delimiter)
        .map(|n| {
            let digits: String = n
                .trim()
                .chars()
                .take_while(|c| c.is_ascii_digit() || *c == '-')
                .collect();
            digits.parse().unwrap_or(-1)
        })
        .collect();
    (nums.len() == 4 || nums.len() == 5).then(|| {
        json!({
            "food": nums[0],
            "wood": nums[1],
            "gold": nums[2],
            "stone": nums[3],
            "builder": nums.get(4).copied().unwrap_or(-1),
        })
    })
}

fn is_dash_resource_allocation(s: &str) -> bool {
    let parts = s
        .split('-')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    (parts.len() == 4 || parts.len() == 5)
        && parts
            .iter()
            .all(|part| part.chars().all(|c| c.is_ascii_digit()))
}

fn parse_resource_allocations_value(v: &Value) -> Option<Value> {
    if let Some(s) = v.as_str() {
        return parse_resource_allocations(s);
    }
    let arr = v.as_array()?;
    if arr.len() != 4 && arr.len() != 5 {
        return None;
    }
    let nums = arr.iter().map(parse_intish).collect::<Option<Vec<_>>>()?;
    Some(json!({
        "food": nums[0],
        "wood": nums[1],
        "gold": nums[2],
        "stone": nums[3],
        "builder": nums.get(4).copied().unwrap_or(-1),
    }))
}

fn parse_step_key(key: &str) -> Option<usize> {
    let s = key.trim();
    if let Ok(n) = s.parse::<usize>() {
        return Some(n);
    }
    let lower = s.to_ascii_lowercase();
    let tail = lower
        .strip_prefix("step")
        .map(|tail| tail.trim_start_matches([' ', '_', '-']))?;
    tail.parse::<usize>().ok()
}

const STEP_COLLECTION_KEYS: &[&str] = &[
    "build_order",
    "buildOrder",
    "build order",
    "build-order",
    "buildorder",
    "build_order_steps",
    "buildOrderSteps",
    "build order steps",
    "build-order-steps",
    "buildordersteps",
    "steps",
    "order",
    "sequence",
    "timeline",
    "plan",
];

const STEP_LIKE_KEYS: &[&str] = &[
    "time",
    "timestamp",
    "timecode",
    "timing",
    "at",
    "start",
    "start_time",
    "startTime",
    "minute",
    "time_text",
    "timeText",
    "age",
    "phase",
    "ageName",
    "era",
    "notes",
    "note",
    "text",
    "description",
    "action",
    "actions",
    "task",
    "tasks",
    "population_count",
    "population",
    "pop",
    "pop_count",
    "popCount",
    "pop_cap",
    "popCap",
    "supply",
    "supply_count",
    "supplyCount",
    "supply_used",
    "supplyUsed",
    "villager_count",
    "vill",
    "villager",
    "villagers",
    "vill_count",
    "villCount",
    "vills",
    "worker",
    "workers",
    "worker_count",
    "workerCount",
    "resources",
    "resource_allocation",
    "villagers_on",
];

fn is_step_like_object(obj: &serde_json::Map<String, Value>) -> bool {
    STEP_LIKE_KEYS.iter().any(|key| obj.contains_key(*key))
}

fn step_lines_from_string(text: &str) -> Option<Value> {
    if let Some(steps) = plain_list_steps_from_string(text) {
        return Some(Value::Array(steps));
    }
    let steps = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(Value::from)
        .collect::<Vec<_>>();
    (!steps.is_empty()).then_some(Value::Array(steps))
}

fn step_collection_from_value(value: Value) -> Option<Value> {
    match value {
        Value::Array(_) => Some(value),
        Value::String(text) => step_lines_from_string(&text),
        Value::Object(mut obj) => {
            for key in STEP_COLLECTION_KEYS {
                if let Some(steps) = obj.remove(*key).and_then(step_collection_from_value) {
                    return Some(steps);
                }
            }
            if obj.is_empty() {
                return None;
            }
            if is_step_like_object(&obj) {
                return Some(Value::Array(vec![Value::Object(obj)]));
            }
            let mut steps = obj
                .into_iter()
                .map(|(key, value)| parse_step_key(&key).map(|order| (order, key, value)))
                .collect::<Option<Vec<_>>>()?;
            steps.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
            Some(Value::Array(
                steps.into_iter().map(|(_, _, value)| value).collect(),
            ))
        }
        _ => None,
    }
}

fn plain_list_steps_from_string(text: &str) -> Option<Vec<Value>> {
    let mut steps = Vec::new();
    let mut current_age = None;
    let mut marked = 0usize;
    let mut seen_marked = false;
    for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
        if let Some(age) = parse_age_heading_text(line) {
            current_age = Some(age);
            continue;
        }
        let body = strip_note_list_marker(line)
            .trim_start_matches('#')
            .trim_matches(|c: char| {
                c.is_whitespace()
                    || matches!(c, ':' | ';' | '|' | ',' | '.' | '-' | '–' | '—' | '>')
            })
            .trim();
        if body.is_empty() || parse_age_heading_text(body).is_some() {
            continue;
        }
        let is_marked = plain_list_marker(line);
        if !seen_marked && !is_marked {
            continue;
        }
        if seen_marked && !is_marked && plain_text_footer_heading(line) {
            break;
        }
        if is_marked {
            marked += 1;
            seen_marked = true;
        }
        let mut step = plain_list_step_from_body(body);
        if let Some(age) = current_age {
            step["age"] = Value::from(age);
        }
        steps.push(step);
    }
    (steps.len() >= 2 && marked >= 2.max(steps.len().div_ceil(2))).then_some(steps)
}

fn plain_text_footer_heading(line: &str) -> bool {
    let lower = line.trim().to_ascii_lowercase();
    lower == "note"
        || lower == "notes"
        || lower.starts_with("notes:")
        || lower.starts_with("link to ")
        || lower.starts_with("created by")
        || lower.starts_with("uploaded by")
        || lower.starts_with("generated by")
}

fn plain_list_step_from_body(body: &str) -> Value {
    let (mut age, rest) = take_leading_age(body);
    let rest = trim_plain_field_prefix(&rest);
    let (mut time, rest) = take_leading_time(&rest);
    let rest = trim_plain_field_prefix(&rest);
    let (late_age, rest) = if age.is_none() {
        take_leading_age(&rest)
    } else {
        (None, rest)
    };
    if age.is_none() {
        age = late_age;
    }
    let rest = trim_plain_field_prefix(&rest);
    let (resources, rest) = take_leading_resource_allocation(&rest);
    let rest = trim_plain_field_prefix(&rest);
    let (late_time, rest) = if time.is_none() {
        take_leading_time(&rest)
    } else {
        (None, rest)
    };
    if time.is_none() {
        time = late_time;
    }
    let rest = trim_plain_field_prefix(&rest);
    let (final_age, rest) = if age.is_none() {
        take_leading_age(&rest)
    } else {
        (None, rest)
    };
    if age.is_none() {
        age = final_age;
    }
    let rest = trim_plain_field_prefix(&rest);
    let (counts, rest) = take_leading_count_aliases(&rest);
    let rest = trim_plain_field_prefix(&rest);
    let mut step = json!({ "notes": [rest.trim()] });
    if let Some(age) = age {
        step["age"] = Value::from(age);
    }
    if let Some(time) = time {
        step["time"] = Value::from(time);
    }
    if let Some(resources) = resources {
        step["resources"] = resources;
    }
    for (key, value) in counts {
        step[key] = Value::from(value);
    }
    step
}

fn trim_plain_field_prefix(s: &str) -> String {
    s.trim_start()
        .trim_start_matches([':', ';', '|', ',', '.', '-', '–', '—', '>', '@'])
        .trim_start()
        .trim_start_matches("->")
        .trim_start_matches("=>")
        .trim_start()
        .to_string()
}

fn take_leading_age(body: &str) -> (Option<i64>, String) {
    let trimmed = body.trim_start();
    for width in [2usize, 1] {
        let candidate = trimmed
            .split_whitespace()
            .take(width)
            .collect::<Vec<_>>()
            .join(" ");
        if candidate.is_empty() || candidate.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let Some(age) = parse_age_heading_text(&candidate) else {
            continue;
        };
        let consumed = trimmed
            .split_whitespace()
            .take(width)
            .map(str::len)
            .sum::<usize>()
            + trimmed
                .split_whitespace()
                .take(width.saturating_sub(1))
                .count();
        return (Some(age), trimmed[consumed..].trim_start().to_string());
    }
    (None, body.to_string())
}

fn take_leading_time(body: &str) -> (Option<String>, String) {
    let trimmed = body.trim_start();
    let mut tokens = trimmed.split_whitespace();
    if let (Some(first), Some(second)) = (tokens.next(), tokens.next()) {
        if first.chars().all(|c| c.is_ascii_digit()) && resource_allocation_label(second).is_some()
        {
            return (None, body.to_string());
        }
    }
    for width in [1usize, 2, 3, 4] {
        let candidate = trimmed
            .split_whitespace()
            .take(width)
            .collect::<Vec<_>>()
            .join(" ");
        if candidate.is_empty() {
            continue;
        }
        if parse_resource_allocations(&candidate).is_some()
            || candidate.contains('/')
            || !starts_like_time(&candidate)
        {
            continue;
        }
        if let Some(time) = normalized_time_value(&Value::from(candidate.as_str())) {
            let consumed = trimmed
                .split_whitespace()
                .take(width)
                .map(str::len)
                .sum::<usize>()
                + trimmed
                    .split_whitespace()
                    .take(width.saturating_sub(1))
                    .count();
            return (Some(time), trimmed[consumed..].trim_start().to_string());
        }
    }
    (None, body.to_string())
}

fn starts_like_time(candidate: &str) -> bool {
    let first = candidate.trim_start_matches(['~', '≈', '=']).trim_start();
    first
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_digit() || matches!(c, ':' | '.'))
}

fn take_leading_resource_allocation(body: &str) -> (Option<Value>, String) {
    let trimmed = body.trim_start();
    if let Some(close) = trimmed.strip_prefix('(').and_then(|s| s.find(')')) {
        let end = close + 2;
        let candidate = &trimmed[..end];
        if let Some(resources) = parse_resource_allocations(candidate) {
            return (Some(resources), trimmed[end..].trim_start().to_string());
        }
    }
    let token_end = trimmed
        .char_indices()
        .find_map(|(idx, c)| c.is_whitespace().then_some(idx))
        .unwrap_or(trimmed.len());
    let candidate = &trimmed[..token_end];
    if let Some(resources) = parse_resource_allocations(candidate) {
        return (
            Some(resources),
            trimmed[token_end..].trim_start().to_string(),
        );
    }
    for width in 2usize..=10 {
        let Some((candidate, consumed)) = leading_token_prefix(trimmed, width) else {
            break;
        };
        if candidate.contains('/')
            || candidate.contains(',')
            || is_dash_resource_allocation(&candidate)
        {
            continue;
        }
        if !candidate
            .chars()
            .any(|c| c.is_ascii_alphabetic() || matches!(c, '=' | ':'))
        {
            continue;
        }
        if let Some(resources) = parse_resource_allocations(&candidate) {
            return (
                Some(resources),
                trimmed[consumed..].trim_start().to_string(),
            );
        }
    }
    (None, body.to_string())
}

fn leading_token_prefix(s: &str, width: usize) -> Option<(String, usize)> {
    let mut consumed = 0usize;
    let mut parts = Vec::new();
    let mut rest = s.trim_start();
    consumed += s.len() - rest.len();
    for i in 0..width {
        let end = rest
            .char_indices()
            .find_map(|(idx, c)| c.is_whitespace().then_some(idx))
            .unwrap_or(rest.len());
        if end == 0 {
            return None;
        }
        parts.push(rest[..end].to_string());
        consumed += end;
        rest = &rest[end..];
        if i + 1 < width {
            let before = rest.len();
            rest = rest.trim_start();
            consumed += before - rest.len();
        }
    }
    Some((parts.join(" "), consumed))
}

fn take_leading_count_aliases(body: &str) -> (Vec<(&'static str, i64)>, String) {
    let mut rest = trim_plain_field_prefix(body);
    let mut counts = Vec::new();
    for _ in 0..3 {
        let Some((key, value, consumed)) = leading_count_alias(&rest) else {
            break;
        };
        if !counts.iter().any(|(existing, _)| *existing == key) {
            counts.push((key, value));
        }
        rest = trim_plain_field_prefix(&rest[consumed..]);
    }
    (counts, rest)
}

fn leading_count_alias(body: &str) -> Option<(&'static str, i64, usize)> {
    let trimmed = body.trim_start();
    let leading_ws = body.len() - trimmed.len();
    let (first, first_end) = next_token(trimmed, 0)?;
    if let Some((used, _cap)) = first.split_once('/') {
        if let Ok(value) = used.parse::<i64>() {
            return Some(("population_count", value, leading_ws + first_end));
        }
    }
    if let Some(key) = count_alias_key(first) {
        let (second, second_end) = next_token(trimmed, first_end)?;
        if let Ok(value) = second.parse::<i64>() {
            return Some((key, value, leading_ws + second_end));
        }
    }
    if let Ok(value) = first.parse::<i64>() {
        let (second, second_end) = next_token(trimmed, first_end)?;
        if let Some(key) = count_alias_key(second) {
            return Some((key, value, leading_ws + second_end));
        }
    }
    None
}

fn next_token(s: &str, start: usize) -> Option<(&str, usize)> {
    let rest = s[start..].trim_start();
    let offset = s.len() - rest.len();
    let end = rest
        .char_indices()
        .find_map(|(idx, c)| c.is_whitespace().then_some(idx))
        .unwrap_or(rest.len());
    (end > 0).then_some((&rest[..end], offset + end))
}

fn count_alias_key(token: &str) -> Option<&'static str> {
    let key: String = token
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect();
    match key.as_str() {
        "vill" | "villager" | "villagers" | "vills" | "worker" | "workers" => {
            Some("villager_count")
        }
        "pop" | "population" | "supply" | "supplyused" | "popcap" => Some("population_count"),
        _ => None,
    }
}

fn plain_list_marker(line: &str) -> bool {
    strip_note_list_marker(line).trim() != line.trim()
}

fn parse_age_heading_text(line: &str) -> Option<i64> {
    let text = strip_note_list_marker(line)
        .trim_start_matches('#')
        .trim_matches(|c: char| c.is_whitespace() || matches!(c, ':' | '-' | '–' | '—'))
        .to_ascii_lowercase();
    match text.as_str() {
        "age i" | "age 1" | "i" | "1" | "dark" | "dark age" => Some(1),
        "age ii" | "age 2" | "ii" | "2" | "feudal" | "feudal age" => Some(2),
        "age iii" | "age 3" | "iii" | "3" | "castle" | "castle age" => Some(3),
        "age iv" | "age 4" | "iv" | "4" | "imperial" | "imperial age" => Some(4),
        _ => None,
    }
}

fn step_collection_field(value: &Value) -> Option<(&'static str, Value)> {
    let obj = value.as_object()?;
    STEP_COLLECTION_KEYS.iter().find_map(|key| {
        obj.get(*key)
            .cloned()
            .and_then(step_collection_from_value)
            .map(|steps| (*key, steps))
    })
}

fn civ_lookup_key(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn normalize_civ_name(raw: &str) -> String {
    let decoded = decode_html_entities(raw);
    let cleaned = strip_html_tags(&decoded).trim().to_string();
    if cleaned.is_empty() {
        return cleaned;
    }
    let key = civ_lookup_key(&cleaned);
    CIV_ALIASES
        .iter()
        .find(|(alias, _)| civ_lookup_key(alias) == key)
        .map(|(_, canonical)| (*canonical).to_string())
        .unwrap_or(cleaned)
}

fn split_civilization_string(raw: &str) -> Vec<String> {
    let cleaned = raw
        .replace(['|', ';', '/'], ",")
        .replace(" and ", ",")
        .replace(" And ", ",")
        .replace(" AND ", ",");
    cleaned
        .split(',')
        .map(normalize_civ_name)
        .filter(|civ| !civ.is_empty())
        .collect()
}

fn normalize_civilization_value(value: &Value) -> Option<Value> {
    if let Some(raw) = value.as_str() {
        let civs = split_civilization_string(raw);
        return match civs.as_slice() {
            [] => None,
            [only] => Some(Value::from(only.clone())),
            _ => Some(Value::Array(civs.into_iter().map(Value::from).collect())),
        };
    }
    if let Some(items) = value.as_array() {
        let civs = items
            .iter()
            .filter_map(Value::as_str)
            .flat_map(split_civilization_string)
            .filter(|civ| !civ.is_empty())
            .map(Value::from)
            .collect::<Vec<_>>();
        return (!civs.is_empty()).then_some(Value::Array(civs));
    }
    None
}

fn normalize_civilization_field(bo: &mut Value, repairs: &mut Vec<String>) {
    if bo["civilization"].is_null() {
        for alias in [
            "civilisation",
            "civ",
            "civilizations",
            "civilisations",
            "civs",
        ] {
            if !bo[alias].is_null() {
                if let Some(obj) = bo.as_object_mut() {
                    if let Some(v) = obj.remove(alias) {
                        obj.insert("civilization".to_string(), v);
                        repairs.push(format!("Renamed {alias} to civilization."));
                    }
                }
                break;
            }
        }
    }

    if let Some(fixed) = normalize_civilization_value(&bo["civilization"]) {
        if bo["civilization"] != fixed {
            bo["civilization"] = fixed;
            repairs.push("Normalized civilization value.".to_string());
        }
    }
}

fn has_direct_step_collection(value: &Value) -> bool {
    match value {
        Value::Array(_) => true,
        Value::Object(_) => {
            step_collection_from_value(value.clone()).is_some()
                || step_collection_field(value).is_some()
        }
        _ => false,
    }
}

fn has_wrapped_build_payload(value: &Value) -> bool {
    if has_direct_step_collection(value) {
        return true;
    }
    let Some(obj) = value.as_object() else {
        return false;
    };
    ["data", "build", "payload", "overlay", "result", "item"]
        .iter()
        .any(|key| obj.get(*key).is_some_and(has_wrapped_build_payload))
}

fn unwrap_build_wrapper(bo: &mut Value, repairs: &mut Vec<String>) {
    if has_direct_step_collection(bo) {
        return;
    }
    for key in ["data", "build", "payload", "overlay", "result", "item"] {
        if has_wrapped_build_payload(&bo[key]) {
            let Some(obj) = bo.as_object_mut() else {
                return;
            };
            let Some(mut nested) = obj.remove(key) else {
                return;
            };
            unwrap_build_wrapper(&mut nested, repairs);
            *bo = nested;
            repairs.push(format!("Unwrapped {key} build payload."));
            return;
        }
    }
}

fn normalize_build_metadata(bo: &mut Value, repairs: &mut Vec<String>) {
    if bo["name"].is_null() {
        for alias in ["title", "buildName", "build_name"] {
            let Some(name) = bo[alias]
                .as_str()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
            else {
                continue;
            };
            if let Some(obj) = bo.as_object_mut() {
                obj.remove(alias);
                obj.insert("name".to_string(), Value::from(name));
            }
            repairs.push(format!("Renamed {alias} to name."));
            break;
        }
    }

    if bo["source"].is_null() {
        for alias in [
            "url",
            "link",
            "buildUrl",
            "build_url",
            "sourceUrl",
            "source_url",
        ] {
            let Some(source) = bo[alias].as_str().and_then(canonical_build_source) else {
                continue;
            };
            if let Some(obj) = bo.as_object_mut() {
                obj.remove(alias);
                obj.insert("source".to_string(), Value::from(source));
            }
            repairs.push(format!("Renamed {alias} to source."));
            remove_blank_build_metadata(bo, repairs);
            return;
        }
    } else if let Some(source) = bo["source"].as_str().and_then(canonical_build_source) {
        if bo["source"] != Value::from(source.clone()) {
            bo["source"] = Value::from(source);
            repairs.push("Normalized source link.".to_string());
        }
        remove_blank_build_metadata(bo, repairs);
        return;
    }

    let Some(id) = bo["id"]
        .as_str()
        .map(str::trim)
        .filter(|id| id.len() == 20 && id.chars().all(|c| c.is_ascii_alphanumeric()))
        .map(str::to_string)
    else {
        remove_blank_build_metadata(bo, repairs);
        return;
    };
    if let Some(obj) = bo.as_object_mut() {
        obj.insert(
            "source".to_string(),
            Value::from(format!("https://aoe4guides.com/builds/{id}")),
        );
    }
    repairs.push("Derived source from aoe4guides id.".to_string());
    remove_blank_build_metadata(bo, repairs);
}

fn remove_blank_build_metadata(bo: &mut Value, repairs: &mut Vec<String>) {
    let Some(obj) = bo.as_object_mut() else {
        return;
    };
    let mut removed = Vec::new();
    for key in [
        "description",
        "author",
        "video",
        "season",
        "map",
        "strategy",
    ] {
        if obj.get(key).is_some_and(is_blank_metadata_value) {
            obj.remove(key);
            removed.push(key);
        }
    }
    if !removed.is_empty() {
        repairs.push(format!("Removed blank metadata: {}.", removed.join(", ")));
    }
}

fn is_blank_metadata_value(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::String(s) => s.trim().is_empty(),
        Value::Array(items) => items.is_empty(),
        Value::Object(obj) => obj.is_empty(),
        _ => false,
    }
}

fn canonical_build_source(raw: &str) -> Option<String> {
    let source = raw.trim();
    if source.is_empty() {
        return None;
    }
    let lower = source.to_ascii_lowercase();
    if let Some(pos) = lower.find("aoe4guides.com/builds/") {
        let id = source[pos + "aoe4guides.com/builds/".len()..]
            .split(['?', '#', '/', '&'])
            .next()?
            .trim();
        if id.len() == 20 && id.chars().all(|c| c.is_ascii_alphanumeric()) {
            return Some(format!("https://aoe4guides.com/builds/{id}"));
        }
    }
    if let Some(pos) = lower.find("aoeivbuilds.com/build_orders/") {
        let id = source[pos + "aoeivbuilds.com/build_orders/".len()..]
            .split(['?', '#', '/', '.'])
            .next()?
            .trim();
        if !id.is_empty() && id.chars().all(|c| c.is_ascii_digit()) {
            return Some(format!("https://www.aoeivbuilds.com/build_orders/{id}"));
        }
    }
    if source.len() == 20 && source.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Some(format!("https://aoe4guides.com/builds/{source}"));
    }
    None
}

fn section_age_value(section: &Value) -> Option<Value> {
    let age = parse_ageish(&section["age"])?;
    let kind = section["type"]
        .as_str()
        .unwrap_or_default()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect::<String>();
    let age = if kind == "ageup" && age < 4 {
        age + 1
    } else {
        age
    };
    Some(Value::from(age))
}

fn section_gameplan_note(section: &Value) -> Option<String> {
    let raw = section["gameplan"].as_str()?.trim();
    if raw.is_empty() {
        return None;
    }
    let (note, _, _) = normalize_note_with_flags(raw);
    (!note.trim().is_empty()).then_some(note)
}

fn prepend_section_note(child: &mut Value, note: &str) {
    let Some(obj) = child.as_object_mut() else {
        return;
    };
    let mut notes = vec![Value::from(note.to_string())];
    if let Some(existing) = obj.remove("notes") {
        if let Some(Value::Array(mut existing_notes)) = note_alias_value(&existing) {
            notes.append(&mut existing_notes);
        }
    } else if let Some(description) = obj.remove("description") {
        if let Some(Value::Array(mut description_notes)) = note_alias_value(&description) {
            notes.append(&mut description_notes);
        } else {
            obj.insert("description".to_string(), description);
        }
    }
    obj.insert("notes".to_string(), Value::Array(notes));
}

fn raw_step_value_has_content(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(_) | Value::Number(_) => true,
        Value::String(s) => !s.trim().is_empty(),
        Value::Array(items) => items.iter().any(raw_step_value_has_content),
        Value::Object(map) => map.values().any(raw_step_value_has_content),
    }
}

fn flatten_sectioned_steps(steps: &mut Vec<Value>, repairs: &mut Vec<String>) {
    let original = std::mem::take(steps);
    let mut flattened = Vec::new();
    let mut changed = false;
    for (section_index, section) in original.into_iter().enumerate() {
        let children = section["steps"].as_array().cloned();
        let is_section = children.as_ref().is_some_and(|items| !items.is_empty())
            && (section["type"].is_string()
                || !section["gameplan"].is_null()
                || !section["age"].is_null());
        let Some(children) = children.filter(|_| is_section) else {
            flattened.push(section);
            continue;
        };
        changed = true;
        let age = section_age_value(&section);
        let gameplan = section_gameplan_note(&section);
        let mut kept_count = 0;
        for mut child in children {
            if !raw_step_value_has_content(&child) {
                repairs.push(format!(
                    "Removed blank step from section {}.",
                    section_index + 1
                ));
                continue;
            }
            let is_first_kept_child = kept_count == 0;
            kept_count += 1;
            if let Some(obj) = child.as_object_mut() {
                if obj.get("age").is_none_or(Value::is_null) {
                    if let Some(age) = &age {
                        obj.insert("age".to_string(), age.clone());
                    }
                }
            }
            if is_first_kept_child {
                if let Some(gameplan) = &gameplan {
                    prepend_section_note(&mut child, gameplan);
                }
            }
            flattened.push(child);
        }
        repairs.push(format!(
            "Flattened section {} into {kept_count} build step{}.",
            section_index + 1,
            if kept_count == 1 { "" } else { "s" }
        ));
    }
    if changed {
        *steps = flattened;
    } else {
        *steps = flattened;
    }
}

fn normalize_build_shape(bo: &mut Value, repairs: &mut Vec<String>) {
    if bo.is_array() {
        let steps = std::mem::take(bo);
        *bo = json!({ "build_order": steps });
        repairs.push("Wrapped top-level step array in build_order.".to_string());
    }

    unwrap_build_wrapper(bo, repairs);
    if bo.is_array() {
        let steps = std::mem::take(bo);
        *bo = json!({ "build_order": steps });
        repairs.push("Wrapped unwrapped step array in build_order.".to_string());
    }
    normalize_civilization_field(bo, repairs);
    normalize_build_metadata(bo, repairs);

    if !bo["build_order"].is_array() {
        if let Some((key, v)) = step_collection_field(bo) {
            if key != "build_order" {
                if let Some(obj) = bo.as_object_mut() {
                    obj.remove(key);
                }
                repairs.push(format!("Renamed {key} to build_order."));
            } else {
                repairs.push("Normalized build_order field.".to_string());
            }
            bo["build_order"] = v;
        } else if let Some(v) = step_collection_from_value(bo.clone()) {
            *bo = json!({ "build_order": v });
            repairs.push("Wrapped top-level step map in build_order.".to_string());
        }
    }

    let Some(steps) = bo["build_order"].as_array_mut() else {
        return;
    };
    flatten_sectioned_steps(steps, repairs);
    apply_step_array_age_headings(steps, repairs);
    for (i, step) in steps.iter_mut().enumerate() {
        let step_no = i + 1;
        if let Some(text) = step.as_str() {
            *step = plain_list_step_from_body(strip_note_list_marker(text));
            repairs.push(format!("Converted step {step_no} text into an object."));
            continue;
        }
        if let Some(tuple_step) = tuple_step_value(step) {
            *step = tuple_step;
            repairs.push(format!("Converted step {step_no} tuple into an object."));
            continue;
        }
        if !step.is_object() {
            continue;
        }
        if let Some(obj) = step.as_object_mut() {
            if obj.remove("_id").is_some() {
                repairs.push(format!("Removed private _id from step {step_no}."));
            }
        }
        if step["time"].is_null() {
            for key in [
                "timestamp",
                "timecode",
                "timing",
                "at",
                "minute",
                "start",
                "start_time",
                "startTime",
                "time_text",
                "timeText",
            ] {
                if let Some(time) = step[key].as_str().map(str::to_string) {
                    if let Some(obj) = step.as_object_mut() {
                        obj.remove(key);
                        obj.insert("time".to_string(), Value::from(time));
                        repairs.push(format!("Moved step {step_no} {key} into time."));
                    }
                    break;
                }
            }
        }
        if !step["notes"].is_array() {
            let note = [
                "notes",
                "note",
                "description",
                "text",
                "action",
                "actions",
                "instruction",
                "instructions",
                "task",
                "tasks",
                "todo",
            ]
            .iter()
            .find_map(|key| note_alias_value(&step[*key]).map(|note| (*key, note)));
            if let Some((key, note)) = note {
                if let Some(obj) = step.as_object_mut() {
                    obj.remove(key);
                    obj.insert("notes".to_string(), note);
                    repairs.push(format!("Moved step {step_no} {key} into notes."));
                }
            }
        }
    }
}

fn apply_step_array_age_headings(steps: &mut Vec<Value>, repairs: &mut Vec<String>) {
    let mut current_age = None;
    let mut out = Vec::with_capacity(steps.len());
    let mut removed = 0usize;
    let mut applied = 0usize;
    for mut step in std::mem::take(steps) {
        if let Some(text) = step.as_str() {
            if let Some(age) = parse_age_heading_text(text) {
                current_age = Some(age);
                removed += 1;
                continue;
            }
        }
        if let Some(age) = current_age {
            if step.as_object().is_some_and(|obj| !obj.contains_key("age")) {
                step["age"] = Value::from(age);
                applied += 1;
            } else if let Some(text) = step.as_str().map(str::to_string) {
                step = plain_list_step_from_body(strip_note_list_marker(&text));
                step["age"] = Value::from(age);
                applied += 1;
            }
        }
        out.push(step);
    }
    if removed > 0 {
        repairs.push(format!(
            "Removed {removed} standalone age heading step{}.",
            if removed == 1 { "" } else { "s" }
        ));
    }
    if applied > 0 {
        repairs.push(format!(
            "Applied age heading to {applied} step{}.",
            if applied == 1 { "" } else { "s" }
        ));
    }
    *steps = out;
}

fn summarize_repairs(repairs: &[String]) -> RepairSummary {
    let mut summary = RepairSummary {
        total: repairs.len(),
        ..RepairSummary::default()
    };
    for repair in repairs {
        if repair.contains("time") {
            summary.times += 1;
        } else if repair.contains("icon") {
            summary.icons += 1;
        } else if repair.contains("note") {
            summary.notes += 1;
        } else if repair.contains("allocation")
            || repair.contains("population_count")
            || repair.contains("villager_count")
            || repair.contains("resource")
        {
            summary.resources += 1;
        } else if repair.starts_with("Removed ") && repair.contains(" blank step")
            || repair.starts_with("Converted step")
            || repair.starts_with("Flattened section")
        {
            summary.steps += 1;
        } else {
            summary.notes += 1;
        }
    }
    summary
}

/// Pull `<h4>Civilization</h4><p>English</p>` out of the build page.
fn extract_civ(html: &str) -> Option<String> {
    let i = html.find("<h4>Civilization</h4>")?;
    let rest = &html[i..];
    let p = rest.find("<p>")? + 3;
    let end = rest[p..].find("</p>")?;
    let civ = rest[p..p + end].trim();
    (!civ.is_empty()).then(|| civ.to_string())
}

fn parse_aoeivbuilds_resource_slot(raw: &str, blank_is_zero: bool) -> i64 {
    let raw = raw.trim();
    if raw.is_empty() {
        return if blank_is_zero { 0 } else { -1 };
    }
    // Values like "6+" or "2/3" degrade to the leading integer.
    let digits: String = raw.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().unwrap_or(-1)
}

/// Convert the aoeivbuilds plain-text export into RTS_Overlay JSON.
///
/// Step lines look like: `* (food/wood/gold/stone)\ttime\tdescription`
#[cfg(test)]
fn convert_aoeivbuilds_txt(txt: &str, civ: Option<&str>, source: &str) -> Result<Value, String> {
    convert_aoeivbuilds_txt_with_repairs(txt, civ, source).map(|(bo, _)| bo)
}

fn convert_aoeivbuilds_txt_with_repairs(
    txt: &str,
    civ: Option<&str>,
    source: &str,
) -> Result<(Value, Vec<String>), String> {
    let mut lines = txt.lines();
    let name = lines
        .next()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .unwrap_or("Imported build")
        .to_string();

    let mut steps = Vec::new();
    let mut repairs = Vec::new();
    for line in lines {
        let line = line.trim();
        let Some(body) = line.strip_prefix("* ") else {
            continue;
        };
        // (f/w/g/s) <tab> time <tab> text — fields are tab-separated
        let mut parts = body.splitn(3, '\t').map(str::trim);
        let res = parts.next().unwrap_or("");
        let time = parts.next().unwrap_or("");
        let text = parts.next().unwrap_or("");
        if text.is_empty() {
            continue;
        }
        let step_no = steps.len() + 1;
        let slots: Vec<&str> = res
            .trim_start_matches('(')
            .trim_end_matches(')')
            .split('/')
            .collect();
        let blank_is_zero = slots.len() == 4;
        if blank_is_zero && slots.iter().any(|slot| slot.trim().is_empty()) {
            repairs.push(format!(
                "Filled blank aoeivbuilds allocation on step {step_no}."
            ));
        }
        let nums: Vec<i64> = slots
            .iter()
            .map(|n| parse_aoeivbuilds_resource_slot(n, blank_is_zero))
            .collect();
        if slots.len() != 4 || nums.iter().any(|n| *n < 0) {
            repairs.push(format!(
                "Kept unknown aoeivbuilds allocation on step {step_no}."
            ));
        }
        let get = |i: usize| nums.get(i).copied().unwrap_or(-1);
        // The four columns are villager allocations, so their sum is the
        // villager count (when every column parsed).
        let vills = if nums.len() == 4 && nums.iter().all(|n| *n >= 0) {
            nums.iter().sum()
        } else {
            -1
        };
        let fixed_time = normalize_time(time);
        let mut notes = Vec::new();
        let time = if fixed_time.is_empty() {
            if !time.trim().is_empty() {
                repairs.push(format!("Removed placeholder time on step {step_no}."));
            }
            None
        } else if is_normalized_time(&fixed_time) {
            if fixed_time != time {
                repairs.push(format!("Normalized time on step {step_no}."));
            }
            Some(fixed_time)
        } else {
            notes.push(Value::from(fixed_time));
            repairs.push(format!(
                "Moved non-time time value into notes on step {step_no}."
            ));
            None
        };
        notes.push(Value::from(text));

        let mut step = json!({
            "age": -1,
            "population_count": -1,
            "villager_count": vills,
            "resources": { "food": get(0), "wood": get(1), "gold": get(2), "stone": get(3) },
            "notes": notes,
        });
        if let Some(time) = time {
            step["time"] = Value::from(time);
        }
        steps.push(step);
    }
    if steps.is_empty() {
        return Err("No build steps found in the aoeivbuilds export".into());
    }
    let mut bo = json!({
        "name": name,
        "civilization": civ.unwrap_or("Any"),
        "author": "",
        "source": source,
        "build_order": steps,
    });
    remove_blank_build_metadata(&mut bo, &mut repairs);
    if let Some(steps) = bo["build_order"].as_array_mut() {
        for (idx, step) in steps.iter_mut().enumerate() {
            normalize_step_counts(step, idx + 1, &mut repairs);
        }
    }
    repairs.sort();
    repairs.dedup();
    Ok((bo, repairs))
}

/// Browse/search aoe4guides: returns up to 10 builds (metadata only).
pub async fn search_aoe4guides(
    client: &reqwest::Client,
    civ: Option<&str>,
    order_by: Option<&str>,
    author: Option<&str>,
) -> Result<Value, String> {
    let mut url = format!("{GUIDES_API}/builds?");
    if let Some(a) = author.filter(|a| !a.is_empty()) {
        url.push_str(&format!("author={a}&"));
    }
    if let Some(c) = civ.filter(|c| !c.is_empty()) {
        url.push_str(&format!("civ={c}&"));
    }
    if let Some(o) = order_by.filter(|o| !o.is_empty()) {
        url.push_str(&format!("orderBy={o}&"));
    }
    let resp = client
        .get(&url)
        .timeout(TIMEOUT)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("aoe4guides returned {}", resp.status()));
    }
    let data: Value = resp.json().await.map_err(|e| e.to_string())?;
    let builds = data.as_array().cloned().unwrap_or_default();
    // Slim the payload: the list view only needs metadata.
    Ok(Value::Array(
        builds
            .iter()
            .map(|b| {
                json!({
                    "id": b["id"],
                    "title": b["title"],
                    "civ": b["civ"],
                    "author": b["author"],
                    "authorUid": b["authorUid"],
                    "likes": b["likes"],
                    "views": b["views"],
                    "season": b["season"],
                    "map": b["map"],
                    "strategy": b["strategy"],
                })
            })
            .collect(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_aoe4guides_urls_and_ids() {
        for u in [
            "https://aoe4guides.com/builds/00I7J47dv26cPbKmXYkO",
            "aoe4guides.com/builds/00I7J47dv26cPbKmXYkO/",
            "https://aoe4guides.com/builds/00I7J47dv26cPbKmXYkO?step=3",
            "00I7J47dv26cPbKmXYkO",
        ] {
            match parse_source(u) {
                Ok(Source::Aoe4Guides(id)) => assert_eq!(id, "00I7J47dv26cPbKmXYkO", "{u}"),
                _ => panic!("failed to parse {u}"),
            }
        }
    }

    #[test]
    fn parses_aoeivbuilds_urls() {
        for u in [
            "https://www.aoeivbuilds.com/build_orders/1296",
            "https://aoeivbuilds.com/build_orders/1296.json",
            "www.aoeivbuilds.com/build_orders/1296/",
        ] {
            match parse_source(u) {
                Ok(Source::AoeIvBuilds(id)) => assert_eq!(id, "1296", "{u}"),
                _ => panic!("failed to parse {u}"),
            }
        }
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_source("https://example.com/whatever").is_err());
        assert!(parse_source("hello").is_err());
        assert!(parse_source("").is_err());
    }

    #[test]
    fn converts_aoeivbuilds_txt() {
        let txt = "English Build Order\n\n\
* (6/0/0/0)\t00:00\tSend all 6 starting vills to sheep\n\
* (7/11/3/0)\t04:20\t2 Spearman - 3 Longbows\n\
* (15/11/5/3)\t10:00\t15 Food, 11 Wood, 6+ Gold, 2/3 Stone\n\n\
Link to the original Build Order: https://aoeivbuilds.com/build_orders/1296\n";
        let bo = convert_aoeivbuilds_txt(txt, Some("English"), "https://x").unwrap();
        assert_eq!(bo["name"], "English Build Order");
        assert_eq!(bo["civilization"], "English");
        assert!(bo["author"].is_null());
        let steps = bo["build_order"].as_array().unwrap();
        assert_eq!(steps.len(), 3);
        assert_eq!(steps[0]["resources"]["food"], 6);
        assert_eq!(steps[0]["time"], "0:00");
        assert!(steps[0]["age"].is_null());
        assert!(steps[0]["population_count"].is_null());
        assert_eq!(steps[1]["resources"]["wood"], 11);
        assert_eq!(steps[2]["resources"]["stone"], 3);
        assert_eq!(steps[0]["notes"][0], "Send all 6 starting vills to sheep");
    }

    #[test]
    fn derives_villager_count_from_allocations() {
        let txt = "T\n* (6/0/0/0)\t00:00\ta\n* (7/11/3/0)\t04:20\tb\n";
        let bo = convert_aoeivbuilds_txt(txt, None, "x").unwrap();
        assert_eq!(bo["build_order"][0]["villager_count"], 6);
        assert_eq!(bo["build_order"][1]["villager_count"], 21);
    }

    #[test]
    fn converts_aoeivbuilds_blank_slots_and_time_labels() {
        let txt = "T\n\
* (/1/0/5)\tCASE 1\tOpen with tower\n\
* (15/20//0)\t00:00:22\tQueue tech\n\
* (///)\t:27\tNo allocation yet\n\
* (6/10/0/)\tFeudal Age\tSwitch plan\n\
* (house of wisdom)\t\tUnknown allocation\n";
        let (bo, repairs) = convert_aoeivbuilds_txt_with_repairs(txt, None, "x").unwrap();
        let steps = bo["build_order"].as_array().unwrap();

        assert_eq!(steps[0]["resources"]["food"], 0);
        assert_eq!(steps[0]["resources"]["stone"], 5);
        assert_eq!(steps[0]["villager_count"], 6);
        assert!(steps[0].get("time").is_none());
        assert_eq!(steps[0]["notes"][0], "CASE 1");
        assert_eq!(steps[0]["notes"][1], "Open with tower");

        assert_eq!(steps[1]["resources"]["gold"], 0);
        assert_eq!(steps[1]["villager_count"], 35);
        assert_eq!(steps[1]["time"], "0:22");

        assert_eq!(steps[2]["villager_count"], 0);
        assert_eq!(steps[2]["time"], "0:27");

        assert_eq!(steps[3]["resources"]["stone"], 0);
        assert_eq!(steps[3]["villager_count"], 16);
        assert!(steps[3].get("time").is_none());
        assert_eq!(steps[3]["notes"][0], "Feudal Age");
        assert_eq!(steps[3]["notes"][1], "Switch plan");

        assert!(steps[4]["villager_count"].is_null());
        assert!(steps[4]["age"].is_null());
        assert!(steps[4]["population_count"].is_null());
        assert_eq!(steps[4]["notes"][0], "Unknown allocation");

        assert!(repairs
            .iter()
            .any(|repair| repair == "Filled blank aoeivbuilds allocation on step 1."));
        assert!(repairs
            .iter()
            .any(|repair| repair == "Normalized time on step 2."));
        assert!(repairs
            .iter()
            .any(|repair| repair == "Moved non-time time value into notes on step 4."));
        let summary = summarize_repairs(&repairs);
        assert_eq!(summary.total, repairs.len());
        assert!(summary.resources >= 4);
        assert_eq!(summary.times, 4);
    }

    #[test]
    fn normalizes_times() {
        assert_eq!(normalize_time("5.30"), "5:30");
        assert_eq!(normalize_time("4:5"), "4:05");
        assert_eq!(normalize_time("04:20"), "4:20");
        assert_eq!(normalize_time("10:00"), "10:00");
        assert_eq!(normalize_time("=3:50"), "3:50");
        assert_eq!(normalize_time("0:2:40"), "2:40");
        assert_eq!(normalize_time("00:00:22"), "0:22");
        assert_eq!(normalize_time(":27"), "0:27");
        assert_eq!(normalize_time("13:~"), "~13:00");
        assert_eq!(normalize_time("~8:00 on"), "~8:00");
        assert_eq!(normalize_time("1: 20"), "1:20");
        assert_eq!(normalize_time("40s"), "0:40");
        assert_eq!(normalize_time("90s"), "1:30");
        assert_eq!(normalize_time("1m30s"), "1:30");
        assert_eq!(normalize_time("1 min 30 sec"), "1:30");
        assert_eq!(normalize_time("05:50 >"), "~5:50");
        assert_eq!(normalize_time("0"), "0:00");
        assert_eq!(normalize_time("~"), "");
        assert_eq!(normalize_time("~~"), "");
        assert_eq!(normalize_time("-"), "");
        assert_eq!(normalize_time("2:20+‑"), "~2:20");
        assert_eq!(normalize_time("5.20+‑"), "~5:20");
        assert_eq!(normalize_time("4:00~"), "~4:00");
        assert_eq!(normalize_time("9:00‑12:00"), "9:00-12:00");
        assert_eq!(normalize_time("&nbsp; 0:21"), "0:21");
        assert_eq!(normalize_time("2m24"), "2:24");
        assert_eq!(normalize_time("+‑2,20"), "~2:20");
        assert_eq!(normalize_time("1;10"), "1:10");
        assert_eq!(normalize_time("10:00 ish"), "~10:00");
        assert_eq!(normalize_time("Around 2:40"), "~2:40");
        assert_eq!(normalize_time("[3:00]"), "3:00");
        assert_eq!(normalize_time("%2:15"), "2:15");
        assert_eq!(normalize_time("7‑8:00"), "7:00-8:00");
        assert_eq!(normalize_time("2:30‑35"), "2:30-2:35");
        assert_eq!(normalize_time("4:06 + 4:11"), "4:06-4:11");
        assert_eq!(normalize_time("9min"), "9:00");
        assert_eq!(normalize_time("12分"), "12:00");
        assert_eq!(normalize_time("<font dir=\"auto\">00:00</font>"), "0:00");
        assert_eq!(normalize_time("00:25\n‑\n01:20"), "0:25-1:20");
        assert_eq!(normalize_time("03:25\n\n\n\n\n04:18"), "3:25");
        assert_eq!(
            normalize_time("04:56\n\n\n\n05:02\n\n06:35\n\n\n\n08:23"),
            "4:56"
        );
        assert_eq!(normalize_time(""), "");
        assert_eq!(normalize_time("~3 min"), "~3:00");
        assert_eq!(normalize_time("1:99"), "1:99", "bad seconds left alone");
    }

    #[test]
    fn normalizes_imported_json_and_reports_repairs() {
        let mut bo = json!({
            "civilisation": "English",
            "buildOrder": [
                "Queue villagers",
                { "time": "1.05", "description": "Scout   <img src=\"/assets/pictures/resource/sheep.webp\" />   @https://age4builder.com/img/deer.png@" }
            ]
        });
        let repairs = normalize_imported_json(&mut bo);
        assert_eq!(bo["civilization"], "English");
        assert!(bo["civilisation"].is_null());
        assert!(bo["buildOrder"].is_null());
        assert_eq!(bo["build_order"][0]["notes"][0], "Queue villagers");
        assert_eq!(
            bo["build_order"][1]["notes"][0],
            "Scout @resource/sheep@ @resource/deer@"
        );
        assert_eq!(bo["build_order"][1]["time"], "1:05");
        assert!(repairs.iter().any(|r| r.contains("civilisation")));
        assert!(repairs.iter().any(|r| r.contains("build_order")));
        assert!(repairs.iter().any(|r| r.contains("Converted step 1")));
        assert!(repairs.iter().any(|r| r.contains("description")));
    }

    #[test]
    fn normalizes_object_keyed_step_maps() {
        let mut bo = json!({
            "build_order": {
                "10": { "description": "Third" },
                "2": "Second",
                "step_1": { "note": "First" }
            }
        });
        let repairs = normalize_imported_json(&mut bo);
        let steps = bo["build_order"].as_array().unwrap();
        assert_eq!(steps.len(), 3);
        assert_eq!(steps[0]["notes"][0], "First");
        assert_eq!(steps[1]["notes"][0], "Second");
        assert_eq!(steps[2]["notes"][0], "Third");
        assert!(repairs
            .iter()
            .any(|repair| repair.contains("Normalized build_order field")));
        assert!(repairs
            .iter()
            .any(|repair| repair.contains("Converted step 2 text")));
    }

    #[test]
    fn normalizes_common_build_order_key_aliases() {
        let mut bo = json!({
            "build order": ["Queue villagers", { "description": "Scout" }]
        });
        let repairs = normalize_imported_json(&mut bo);
        assert!(bo["build order"].is_null());
        assert_eq!(bo["build_order"][0]["notes"][0], "Queue villagers");
        assert_eq!(bo["build_order"][1]["notes"][0], "Scout");
        assert!(repairs
            .iter()
            .any(|repair| repair.contains("Renamed build order to build_order")));

        let mut bo = json!({
            "buildorder": {
                "2": { "description": "Second" },
                "step_1": "First"
            }
        });
        let repairs = normalize_imported_json(&mut bo);
        assert!(bo["buildorder"].is_null());
        assert_eq!(bo["build_order"][0]["notes"][0], "First");
        assert_eq!(bo["build_order"][1]["notes"][0], "Second");
        assert!(repairs
            .iter()
            .any(|repair| repair.contains("Renamed buildorder to build_order")));
    }

    #[test]
    fn wraps_single_step_objects_into_build_order_arrays() {
        let mut bo = json!({
            "name": "Single step",
            "build_order": {
                "time": "0",
                "notes": ["Queue villagers"]
            }
        });
        let repairs = normalize_imported_json(&mut bo);
        let steps = bo["build_order"].as_array().unwrap();
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0]["time"], "0:00");
        assert_eq!(steps[0]["notes"][0], "Queue villagers");
        assert!(repairs
            .iter()
            .any(|repair| repair.contains("Normalized build_order field")));

        let mut bo = json!({
            "buildOrder": {
                "notes": ["Scout"]
            }
        });
        let repairs = normalize_imported_json(&mut bo);
        assert!(bo["buildOrder"].is_null());
        assert_eq!(bo["build_order"].as_array().unwrap().len(), 1);
        assert_eq!(bo["build_order"][0]["notes"][0], "Scout");
        assert!(repairs
            .iter()
            .any(|repair| repair.contains("Renamed buildOrder to build_order")));
    }

    #[test]
    fn converts_string_step_collections_into_step_arrays() {
        let mut bo = json!({
            "build_order": "Queue villagers\nScout\n\nBuild house"
        });
        let repairs = normalize_imported_json(&mut bo);
        assert_eq!(bo["build_order"].as_array().unwrap().len(), 3);
        assert_eq!(bo["build_order"][0]["notes"][0], "Queue villagers");
        assert_eq!(bo["build_order"][1]["notes"][0], "Scout");
        assert_eq!(bo["build_order"][2]["notes"][0], "Build house");
        assert!(repairs
            .iter()
            .any(|repair| repair.contains("Normalized build_order field")));

        let mut bo = json!({
            "steps": "Queue villagers\nScout"
        });
        let repairs = normalize_imported_json(&mut bo);
        assert!(bo["steps"].is_null());
        assert_eq!(bo["build_order"].as_array().unwrap().len(), 2);
        assert!(repairs
            .iter()
            .any(|repair| repair.contains("Renamed steps to build_order")));
    }

    #[test]
    fn converts_plain_list_string_collections_into_aged_steps() {
        let mut bo = json!({
            "build_order": "Dark Age\n- Queue villagers\n- Build house\nFeudal Age\n1. Build archery range\n2. Add farms"
        });
        let repairs = normalize_imported_json(&mut bo);
        let steps = bo["build_order"].as_array().unwrap();
        assert_eq!(steps.len(), 4);
        assert_eq!(steps[0]["age"], 1);
        assert_eq!(steps[0]["notes"][0], "Queue villagers");
        assert_eq!(steps[1]["age"], 1);
        assert_eq!(steps[2]["age"], 2);
        assert_eq!(steps[2]["notes"][0], "Build archery range");
        assert_eq!(steps[3]["notes"][0], "Add farms");
        assert!(repairs
            .iter()
            .any(|repair| repair.contains("Normalized build_order field")));
    }

    #[test]
    fn removes_decorative_note_separators_and_no_space_markers() {
        let mut bo = json!({
            "build_order": [
                {
                    "notes": [
                        "Start",
                        "--------------------------------",
                        "-Académie militaire (Forge)",
                        "---- Fast @age/age_3@ ----",
                        "=>1 @resource/resource_wood@ @unit_worker/villager@ => @building_economy/house@",
                        "> @landmark_french/royal-institute@ for age 3",
                        "&lt;- Ideal economy should look like this",
                        "2.@building_religious/mosque@kur",
                        "@unit_mongols/khan-1@ --> SCOUT",
                        "4x @unit_worker/villager@ >> A/R @resource/resource_wood@",
                        "A 120 @resource/resource_stone@ =>1 @resource/sheep@"
                    ]
                }
            ]
        });
        normalize_imported_json(&mut bo);
        let notes = bo["build_order"][0]["notes"].as_array().unwrap();
        assert_eq!(notes.len(), 10);
        assert_eq!(notes[0], "Start");
        assert_eq!(notes[1], "Académie militaire (Forge)");
        assert_eq!(notes[2], "Fast @age/age_3@");
        assert_eq!(
            notes[3],
            "1 @resource/resource_wood@ @unit_worker/villager@ => @building_economy/house@"
        );
        assert_eq!(notes[4], "@landmark_french/royal-institute@ for age 3");
        assert_eq!(notes[5], "Ideal economy should look like this");
        assert_eq!(notes[6], "@building_religious/mosque@ kur");
        assert_eq!(notes[7], "@unit_mongols/khan-1@ -> SCOUT");
        assert_eq!(
            notes[8],
            "4x @unit_worker/villager@ -> A/R @resource/resource_wood@"
        );
        assert_eq!(
            notes[9],
            "A 120 @resource/resource_stone@ => 1 @resource/sheep@"
        );
    }

    #[test]
    fn removes_duplicate_structured_prefixes_from_notes() {
        let mut bo = json!({
            "build_order": [
                {
                    "age": 1,
                    "resources": { "food": 1, "wood": 3, "gold": 1, "stone": 1 },
                    "notes": ["1/3/1/1 split, @resource/rally@ @resource/resource_food@"]
                },
                {
                    "time": "4:20",
                    "notes": ["Feudal Age - Prelate into Landmark"]
                }
            ]
        });
        normalize_imported_json(&mut bo);
        let steps = bo["build_order"].as_array().unwrap();
        assert_eq!(
            steps[0]["notes"][0],
            "@resource/rally@ @resource/resource_food@"
        );
        assert_eq!(steps[1]["age"], 2);
        assert_eq!(steps[1]["notes"][0], "Prelate into Landmark");
    }

    #[test]
    fn normalizes_high_confidence_note_typos() {
        let mut bo = json!({
            "build_order": [
                {
                    "notes": [
                        "Upgrade Hordinculture.",
                        "Research wheelBarror and wheebarrow",
                        "Build a lumbercamp near the treeline",
                        "Ignore les sheeps in mixed-language notes",
                        "when possibel",
                        "GET ASAP vs exposed ennemie second TC",
                        "Contruire landmark near berrys and deers",
                        "Build Concil Hall and harrass",
                        "Villnext to deer",
                        "CowBoom completed.",
                        "@building_military/barracks@,build @unit_infantry/spearman-1@",
                        "@building_economy/mill@ , @unit_chinese/imperial-official@",
                        "from :: Build @building_rus/wooden-fortress@"
                    ]
                }
            ]
        });
        normalize_imported_json(&mut bo);
        let notes = bo["build_order"][0]["notes"].as_array().unwrap();
        assert_eq!(notes[0], "Upgrade Horticulture.");
        assert_eq!(notes[1], "Research Wheelbarrow and Wheelbarrow");
        assert_eq!(notes[2], "Build a lumber camp near the treeline");
        assert_eq!(notes[3], "Ignore les sheeps in mixed-language notes");
        assert_eq!(notes[4], "when possible");
        assert_eq!(notes[5], "GET ASAP vs exposed enemy second TC");
        assert_eq!(notes[6], "construire landmark near berries and deer");
        assert_eq!(notes[7], "Build Council Hall and harass");
        assert_eq!(notes[8], "Vill next to deer");
        assert_eq!(notes[9], "Cow boom completed.");
        assert_eq!(
            notes[10],
            "@building_military/barracks@, build @unit_infantry/spearman-1@"
        );
        assert_eq!(
            notes[11],
            "@building_economy/mill@, @unit_chinese/imperial-official@"
        );
        assert_eq!(notes[12], "from: Build @building_rus/wooden-fortress@");
    }

    #[test]
    fn splits_packed_action_notes_on_safe_delimiters() {
        let mut bo = json!({
            "build_order": [
                {
                    "notes": [
                        "food vills to landmark; 3 food vills to straggler; rally point food",
                        "@resource/rally@ | 14 @resource/resource_food@ | 4 @resource/resource_gold@",
                        "8 @unit_worker/villager@ on @resource/resource_food@ pour ||: @unit_worker/villager@:||"
                    ]
                }
            ]
        });
        normalize_imported_json(&mut bo);
        let notes = bo["build_order"][0]["notes"].as_array().unwrap();
        assert_eq!(notes[0], "food vills to landmark");
        assert_eq!(notes[1], "3 food vills to straggler");
        assert_eq!(notes[2], "rally point food");
        assert_eq!(notes[3], "@resource/rally@");
        assert_eq!(notes[4], "14 @resource/resource_food@");
        assert_eq!(notes[5], "4 @resource/resource_gold@");
        assert_eq!(
            notes[6],
            "8 @unit_worker/villager@ on @resource/resource_food@ pour ||: @unit_worker/villager@:||"
        );
    }

    #[test]
    fn fills_empty_resource_allocations_from_explicit_note_hints() {
        let mut bo = json!({
            "build_order": [
                {
                    "resources": { "food": 0, "wood": 0, "gold": 0, "stone": 0, "builder": -1 },
                    "notes": ["@resource/resource_wood@: 3 > @resource/rally@ @resource/resource_food@"]
                },
                {
                    "notes": ["@resource/rally@ @resource/resource_wood@ 9"]
                },
                {
                    "resources": { "food": 0, "wood": 5, "gold": 0, "stone": 0, "builder": -1 },
                    "notes": ["@resource/resource_wood@: 12"]
                }
            ]
        });
        normalize_imported_json(&mut bo);
        let steps = bo["build_order"].as_array().unwrap();
        assert_eq!(steps[0]["resources"]["wood"], 3);
        assert_eq!(steps[1]["resources"]["wood"], 9);
        assert_eq!(steps[1]["resources"]["food"], -1);
        assert_eq!(steps[2]["resources"]["wood"], 5);
    }

    #[test]
    fn fills_time_and_age_from_explicit_note_hints() {
        let mut bo = json!({
            "build_order": [
                {
                    "notes": ["2:30", "@age/age_2@ Start age-up"]
                },
                {
                    "notes": ["@age/age_3@"]
                },
                {
                    "time": "4:20",
                    "age": 2,
                    "notes": ["5:00", "@age/age_2@"]
                }
            ]
        });
        normalize_imported_json(&mut bo);
        let steps = bo["build_order"].as_array().unwrap();
        assert_eq!(steps[0]["time"], "2:30");
        assert_eq!(steps[0]["age"], 2);
        assert_eq!(steps[0]["notes"][0], "@age/age_2@ Start age-up");
        assert_eq!(steps[1]["age"], 3);
        assert!(steps[1]["notes"].as_array().unwrap().is_empty());
        assert_eq!(steps[2]["time"], "4:20");
        assert_eq!(steps[2]["age"], 2);
        assert_eq!(steps[2]["notes"][0], "5:00");
    }

    #[test]
    fn extracts_plain_list_allocations_and_counts() {
        let mut bo = json!({
            "build_order": "Dark Age\n- 6/0/0/0 vills 6 pop 6 Queue villagers\n- (5/2/0/0) workers 7 supply 8 Build house"
        });
        normalize_imported_json(&mut bo);
        let steps = bo["build_order"].as_array().unwrap();
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0]["age"], 1);
        assert_eq!(steps[0]["resources"]["food"], 6);
        assert_eq!(steps[0]["resources"]["wood"], 0);
        assert_eq!(steps[0]["villager_count"], 6);
        assert_eq!(steps[0]["population_count"], 6);
        assert_eq!(steps[0]["notes"][0], "Queue villagers");
        assert_eq!(steps[1]["resources"]["food"], 5);
        assert_eq!(steps[1]["resources"]["wood"], 2);
        assert_eq!(steps[1]["villager_count"], 7);
        assert_eq!(steps[1]["population_count"], 8);
        assert_eq!(steps[1]["notes"][0], "Build house");
    }

    #[test]
    fn extracts_allocations_and_counts_from_string_step_arrays() {
        let mut bo = json!({
            "build_order": [
                "6/0/0/0 vills 6 pop 6 Queue villagers",
                "- (5/2/0/0) workers 7 supply 8 Build house"
            ]
        });
        normalize_imported_json(&mut bo);
        let steps = bo["build_order"].as_array().unwrap();
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0]["resources"]["food"], 6);
        assert_eq!(steps[0]["villager_count"], 6);
        assert_eq!(steps[0]["population_count"], 6);
        assert_eq!(steps[0]["notes"][0], "Queue villagers");
        assert_eq!(steps[1]["resources"]["food"], 5);
        assert_eq!(steps[1]["resources"]["wood"], 2);
        assert_eq!(steps[1]["villager_count"], 7);
        assert_eq!(steps[1]["population_count"], 8);
        assert_eq!(steps[1]["notes"][0], "Build house");
    }

    #[test]
    fn extracts_time_from_string_step_arrays() {
        let mut bo = json!({
            "build_order": [
                "0:00 6/0/0/0 vills 6 pop 6 Queue villagers",
                "- 90s (5/2/0/0) workers 7 supply 8 Build house"
            ]
        });
        normalize_imported_json(&mut bo);
        let steps = bo["build_order"].as_array().unwrap();
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0]["time"], "0:00");
        assert_eq!(steps[0]["resources"]["food"], 6);
        assert_eq!(steps[0]["villager_count"], 6);
        assert_eq!(steps[0]["population_count"], 6);
        assert_eq!(steps[0]["notes"][0], "Queue villagers");
        assert_eq!(steps[1]["time"], "1:30");
        assert_eq!(steps[1]["resources"]["food"], 5);
        assert_eq!(steps[1]["resources"]["wood"], 2);
        assert_eq!(steps[1]["villager_count"], 7);
        assert_eq!(steps[1]["population_count"], 8);
        assert_eq!(steps[1]["notes"][0], "Build house");
    }

    #[test]
    fn extracts_time_after_leading_allocations_in_string_steps() {
        let mut bo = json!({
            "build_order": [
                "6/0/0/0 0:00 vills 6 pop 6 Queue villagers",
                "- (5/2/0/0) 90s workers 7 supply 8 Build house"
            ]
        });
        normalize_imported_json(&mut bo);
        let steps = bo["build_order"].as_array().unwrap();
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0]["time"], "0:00");
        assert_eq!(steps[0]["resources"]["food"], 6);
        assert_eq!(steps[0]["villager_count"], 6);
        assert_eq!(steps[0]["population_count"], 6);
        assert_eq!(steps[0]["notes"][0], "Queue villagers");
        assert_eq!(steps[1]["time"], "1:30");
        assert_eq!(steps[1]["resources"]["food"], 5);
        assert_eq!(steps[1]["resources"]["wood"], 2);
        assert_eq!(steps[1]["villager_count"], 7);
        assert_eq!(steps[1]["population_count"], 8);
        assert_eq!(steps[1]["notes"][0], "Build house");
    }

    #[test]
    fn extracts_leading_age_from_string_steps() {
        let mut bo = json!({
            "build_order": [
                "Feudal 0:00 6/0/0/0 vills 6 pop 6 Queue villagers",
                "- (5/2/0/0) Castle workers 7 supply 8 Build house",
                "Age up to Castle"
            ]
        });
        normalize_imported_json(&mut bo);
        let steps = bo["build_order"].as_array().unwrap();
        assert_eq!(steps.len(), 3);
        assert_eq!(steps[0]["age"], 2);
        assert_eq!(steps[0]["time"], "0:00");
        assert_eq!(steps[0]["resources"]["food"], 6);
        assert_eq!(steps[0]["villager_count"], 6);
        assert_eq!(steps[0]["population_count"], 6);
        assert_eq!(steps[0]["notes"][0], "Queue villagers");
        assert_eq!(steps[1]["age"], 3);
        assert_eq!(steps[1]["resources"]["food"], 5);
        assert_eq!(steps[1]["villager_count"], 7);
        assert_eq!(steps[1]["population_count"], 8);
        assert_eq!(steps[1]["notes"][0], "Build house");
        assert!(steps[2]["age"].is_null());
        assert_eq!(steps[2]["notes"][0], "Age up to Castle");
    }

    #[test]
    fn ignores_noisy_separators_between_string_step_fields() {
        let mut bo = json!({
            "build_order": [
                "Feudal => 0:00 -> 6/0/0/0 - vills 6 @ pop 6 - Queue villagers",
                "(5/2/0/0) @ 90s => Castle - workers 7 - supply 8 - Build house"
            ]
        });
        normalize_imported_json(&mut bo);
        let steps = bo["build_order"].as_array().unwrap();
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0]["age"], 2);
        assert_eq!(steps[0]["time"], "0:00");
        assert_eq!(steps[0]["resources"]["food"], 6);
        assert_eq!(steps[0]["villager_count"], 6);
        assert_eq!(steps[0]["population_count"], 6);
        assert_eq!(steps[0]["notes"][0], "Queue villagers");
        assert_eq!(steps[1]["age"], 3);
        assert_eq!(steps[1]["time"], "1:30");
        assert_eq!(steps[1]["resources"]["food"], 5);
        assert_eq!(steps[1]["resources"]["wood"], 2);
        assert_eq!(steps[1]["villager_count"], 7);
        assert_eq!(steps[1]["population_count"], 8);
        assert_eq!(steps[1]["notes"][0], "Build house");
    }

    #[test]
    fn extracts_labeled_allocations_from_string_steps() {
        let mut bo = json!({
            "build_order": [
                "0:00 6f 0w 0g 0s vills 6 Queue villagers",
                "Feudal 6 food 2 wood 0 gold 0 stone pop 8 Build house"
            ]
        });
        normalize_imported_json(&mut bo);
        let steps = bo["build_order"].as_array().unwrap();
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0]["time"], "0:00");
        assert_eq!(steps[0]["resources"]["food"], 6);
        assert_eq!(steps[0]["resources"]["wood"], 0);
        assert_eq!(steps[0]["villager_count"], 6);
        assert_eq!(steps[0]["notes"][0], "Queue villagers");
        assert_eq!(steps[1]["age"], 2);
        assert_eq!(steps[1]["resources"]["food"], 6);
        assert_eq!(steps[1]["resources"]["wood"], 2);
        assert_eq!(steps[1]["resources"]["gold"], 0);
        assert_eq!(steps[1]["resources"]["stone"], 0);
        assert_eq!(steps[1]["population_count"], 8);
        assert_eq!(steps[1]["notes"][0], "Build house");
    }

    #[test]
    fn string_step_collections_skip_export_preamble_and_footer() {
        let mut bo = json!({
            "build_order": "English Man-at-Arms\n\n* (/4/0/0)\t\tSend 4 villagers to WOOD, Send 2 villagers to FARM\n* (2/4/0/0)\t\tBuild a house with 1 Wood villager\n\nNotes\ngood against pressure\n\nGenerated by AOE4 Builds"
        });
        normalize_imported_json(&mut bo);
        let steps = bo["build_order"].as_array().unwrap();
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0]["resources"]["food"], -1);
        assert_eq!(steps[0]["resources"]["wood"], 4);
        assert_eq!(
            steps[0]["notes"][0],
            "Send 4 villagers to WOOD, Send 2 villagers to FARM"
        );
        assert_eq!(steps[1]["resources"]["food"], 2);
        assert_eq!(steps[1]["resources"]["wood"], 4);
        assert_eq!(steps[1]["notes"][0], "Build a house with 1 Wood villager");
    }

    #[test]
    fn applies_age_heading_entries_inside_step_arrays() {
        let mut bo = json!({
            "build_order": [
                "Dark Age",
                "- Queue villagers",
                { "notes": ["Build house"] },
                "Feudal Age",
                "Build archery range",
                { "age": 3, "notes": ["Keep existing age"] }
            ]
        });
        let repairs = normalize_imported_json(&mut bo);
        let steps = bo["build_order"].as_array().unwrap();
        assert_eq!(steps.len(), 4);
        assert_eq!(steps[0]["age"], 1);
        assert_eq!(steps[0]["notes"][0], "Queue villagers");
        assert_eq!(steps[1]["age"], 1);
        assert_eq!(steps[2]["age"], 2);
        assert_eq!(steps[2]["notes"][0], "Build archery range");
        assert_eq!(steps[3]["age"], 3);
        assert!(repairs
            .iter()
            .any(|repair| repair.contains("Removed 2 standalone age heading steps")));
        assert!(repairs
            .iter()
            .any(|repair| repair.contains("Applied age heading to 3 steps")));
    }

    #[test]
    fn removes_common_list_markers_from_notes() {
        let mut bo = json!({
            "build_order": [
                "1. Queue villagers",
                { "notes": ["- Scout", "* Build house", "• Rally sheep", "2) Age up", "1:00 Keep timing"] }
            ]
        });
        normalize_imported_json(&mut bo);
        assert_eq!(bo["build_order"][0]["notes"][0], "Queue villagers");
        assert_eq!(bo["build_order"][1]["notes"][0], "Scout");
        assert_eq!(bo["build_order"][1]["notes"][1], "Build house");
        assert_eq!(bo["build_order"][1]["notes"][2], "Rally sheep");
        assert_eq!(bo["build_order"][1]["notes"][3], "Age up");
        assert_eq!(bo["build_order"][1]["notes"][4], "1:00 Keep timing");
    }

    #[test]
    fn does_not_unwrap_metadata_strings_as_build_payloads() {
        let mut bo = json!({
            "name": "Metadata only",
            "build": "Fast Castle"
        });
        let repairs = normalize_imported_json(&mut bo);
        assert!(bo["build_order"].is_null());
        assert_eq!(bo["build"], "Fast Castle");
        assert!(!repairs
            .iter()
            .any(|repair| repair.contains("Unwrapped build payload")));
    }

    #[test]
    fn flattens_aoe4guides_sectioned_steps_from_api_payloads() {
        let mut bo = json!({
            "id": "00I7J47dv26cPbKmXYkO",
            "title": "Community sectioned build",
            "civ": "ENG",
            "steps": [
                {
                    "type": "age",
                    "age": 1,
                    "gameplan": "<br>",
                    "steps": [
                        {
                            "time": "0:00",
                            "food": "6",
                            "description": "Queue <img src=\"/assets/pictures/resource/sheep.webp\" class=\"icon-none\" />"
                        },
                        {
                            "_id": 1781197881592_i64,
                            "time": "1:10",
                            "wood": "2",
                            "gold": "",
                            "stone": "",
                            "villagers": "",
                            "description": "Build <img src=\"https://aoe4guides.com/assets/pictures/building_economy/house.webp\" />"
                        }
                    ]
                },
                {
                    "type": "ageUp",
                    "age": 1,
                    "gameplan": "Click <img src=\"/assets/pictures/age/age_2.webp\" />",
                    "steps": [
                        {
                            "time": "2:45",
                            "builders": "4",
                            "description": "Start landmark"
                        }
                    ]
                }
            ]
        });
        let repairs = normalize_imported_json(&mut bo);
        let steps = bo["build_order"].as_array().unwrap();
        assert_eq!(bo["name"], "Community sectioned build");
        assert_eq!(
            bo["source"],
            "https://aoe4guides.com/builds/00I7J47dv26cPbKmXYkO"
        );
        assert_eq!(steps.len(), 3);
        assert_eq!(steps[0]["age"], 1);
        assert_eq!(steps[0]["resources"]["food"], 6);
        assert_eq!(steps[0]["notes"][0], "Queue @resource/sheep@");
        assert!(steps[1]["_id"].is_null());
        assert!(steps[1]["gold"].is_null());
        assert!(steps[1]["stone"].is_null());
        assert!(steps[1]["villagers"].is_null());
        assert_eq!(steps[1]["notes"][0], "Build @building_economy/house@");
        assert_eq!(steps[2]["age"], 2);
        assert_eq!(steps[2]["resources"]["builder"], 4);
        assert_eq!(steps[2]["notes"][0], "Click @age/age_2@");
        assert_eq!(steps[2]["notes"][1], "Start landmark");
        assert!(repairs
            .iter()
            .any(|repair| repair.contains("Flattened section 1 into 2 build steps")));
        assert!(repairs
            .iter()
            .any(|repair| repair.contains("Flattened section 2 into 1 build step")));
        assert!(repairs
            .iter()
            .any(|repair| repair.contains("Renamed title to name")));
        assert!(repairs
            .iter()
            .any(|repair| repair.contains("Derived source from aoe4guides id")));
        assert!(repairs
            .iter()
            .any(|repair| repair.contains("Removed private _id from step 2")));
    }

    #[test]
    fn normalizes_build_source_aliases_and_links() {
        let mut bo = json!({
            "name": "Alias source",
            "url": "https://aoe4guides.com/builds/00I7J47dv26cPbKmXYkO?view=overlay",
            "build_order": ["Scout"]
        });
        let repairs = normalize_imported_json(&mut bo);
        assert_eq!(
            bo["source"],
            "https://aoe4guides.com/builds/00I7J47dv26cPbKmXYkO"
        );
        assert!(bo["url"].is_null());
        assert!(repairs
            .iter()
            .any(|repair| repair.contains("Renamed url to source")));

        let mut bo = json!({
            "name": "Messy source",
            "source": "https://aoeivbuilds.com/build_orders/1296.json",
            "description": "",
            "video": "",
            "map": null,
            "strategy": {},
            "author": "Community author",
            "season": "Season 10",
            "build_order": ["Scout"]
        });
        let repairs = normalize_imported_json(&mut bo);
        assert_eq!(
            bo["source"],
            "https://www.aoeivbuilds.com/build_orders/1296"
        );
        assert!(bo["description"].is_null());
        assert!(bo["video"].is_null());
        assert!(bo["map"].is_null());
        assert!(bo["strategy"].is_null());
        assert_eq!(bo["author"], "Community author");
        assert_eq!(bo["season"], "Season 10");
        assert!(repairs
            .iter()
            .any(|repair| repair.contains("Normalized source link")));
        assert!(repairs
            .iter()
            .any(|repair| repair.contains("Removed blank metadata")));
    }

    #[test]
    fn wraps_top_level_object_keyed_step_maps() {
        let mut bo = json!({
            "2": { "description": "Second" },
            "step_1": "First"
        });
        let repairs = normalize_imported_json(&mut bo);
        assert_eq!(bo["build_order"][0]["notes"][0], "First");
        assert_eq!(bo["build_order"][1]["notes"][0], "Second");
        assert!(repairs
            .iter()
            .any(|repair| repair.contains("Wrapped top-level step map")));
    }

    #[test]
    fn unwraps_api_payload_wrappers() {
        let mut bo = json!({
            "data": {
                "build": {
                    "civ": "ENG",
                    "steps": ["Queue villagers", { "description": "Scout" }]
                }
            }
        });
        let repairs = normalize_imported_json(&mut bo);
        assert_eq!(bo["civilization"], "English");
        assert_eq!(bo["build_order"][0]["notes"][0], "Queue villagers");
        assert_eq!(bo["build_order"][1]["notes"][0], "Scout");
        assert!(repairs
            .iter()
            .any(|repair| repair.contains("Unwrapped build build payload")));
        assert!(repairs
            .iter()
            .any(|repair| repair.contains("Unwrapped data build payload")));

        let mut bo = json!({
            "payload": {
                "overlay": ["Queue villagers", { "description": "Scout" }]
            }
        });
        let repairs = normalize_imported_json(&mut bo);
        assert_eq!(bo["build_order"][0]["notes"][0], "Queue villagers");
        assert_eq!(bo["build_order"][1]["notes"][0], "Scout");
        assert!(repairs
            .iter()
            .any(|repair| repair.contains("Wrapped unwrapped step array")));
    }

    #[test]
    fn wraps_top_level_step_arrays() {
        let mut bo = json!(["Queue villagers", { "description": "Scout" }]);
        let repairs = normalize_imported_json(&mut bo);
        let steps = bo["build_order"].as_array().unwrap();
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0]["notes"][0], "Queue villagers");
        assert_eq!(steps[1]["notes"][0], "Scout");
        assert!(repairs
            .iter()
            .any(|repair| repair.contains("Wrapped top-level step array")));
        assert!(repairs
            .iter()
            .any(|repair| repair.contains("Converted step 1 text")));
    }

    #[test]
    fn normalizes_tuple_steps() {
        let mut bo = json!({
            "build_order": [
                ["0", "Queue villagers"],
                ["1.05", "6/2/0/0", "Scout"],
                [[8, 3, 0, 0], "2m24", ["Build house", "Move scout"]],
                ["3:00", "Research wheelbarrow", [10, 4, 0, 0, 1]]
            ]
        });
        let repairs = normalize_imported_json(&mut bo);
        assert_eq!(bo["build_order"][0]["time"], "0:00");
        assert_eq!(bo["build_order"][0]["notes"][0], "Queue villagers");
        assert_eq!(bo["build_order"][1]["time"], "1:05");
        assert_eq!(bo["build_order"][1]["resources"]["food"], 6);
        assert_eq!(bo["build_order"][1]["resources"]["wood"], 2);
        assert_eq!(bo["build_order"][1]["notes"][0], "Scout");
        assert_eq!(bo["build_order"][2]["time"], "2:24");
        assert_eq!(bo["build_order"][2]["resources"]["food"], 8);
        assert_eq!(bo["build_order"][2]["notes"][1], "Move scout");
        assert_eq!(bo["build_order"][3]["resources"]["builder"], 1);
        assert_eq!(bo["build_order"][3]["notes"][0], "Research wheelbarrow");
        assert!(repairs
            .iter()
            .any(|repair| repair.contains("Converted step 1 tuple")));
        assert!(repairs
            .iter()
            .any(|repair| repair.contains("Converted step 4 tuple")));
    }

    #[test]
    fn normalizes_step_text_and_time_aliases() {
        let mut bo = json!({
            "build_order": [
                { "timestamp": "1.05", "action": "Scout the map" },
                { "minute": "2m24", "instruction": "Build house" },
                { "startTime": "90s", "task": "Queue more villagers" },
                { "time_text": "1 min 30 sec", "todo": "Rally sheep" }
            ]
        });
        let repairs = normalize_imported_json(&mut bo);
        assert_eq!(bo["build_order"][0]["time"], "1:05");
        assert_eq!(bo["build_order"][0]["notes"][0], "Scout the map");
        assert!(bo["build_order"][0]["timestamp"].is_null());
        assert!(bo["build_order"][0]["action"].is_null());
        assert_eq!(bo["build_order"][1]["time"], "2:24");
        assert_eq!(bo["build_order"][1]["notes"][0], "Build house");
        assert_eq!(bo["build_order"][2]["time"], "1:30");
        assert_eq!(bo["build_order"][2]["notes"][0], "Queue more villagers");
        assert!(bo["build_order"][2]["startTime"].is_null());
        assert_eq!(bo["build_order"][3]["time"], "1:30");
        assert_eq!(bo["build_order"][3]["notes"][0], "Rally sheep");
        assert!(bo["build_order"][3]["time_text"].is_null());
        assert!(repairs
            .iter()
            .any(|repair| repair.contains("timestamp into time")));
        assert!(repairs
            .iter()
            .any(|repair| repair.contains("action into notes")));
        assert!(repairs
            .iter()
            .any(|repair| repair.contains("minute into time")));
        assert!(repairs
            .iter()
            .any(|repair| repair.contains("instruction into notes")));
        assert!(repairs
            .iter()
            .any(|repair| repair.contains("startTime into time")));
        assert!(repairs
            .iter()
            .any(|repair| repair.contains("time_text into time")));
    }

    #[test]
    fn moves_non_time_time_values_into_notes() {
        let mut bo = json!({
            "build_order": [
                {
                    "time": "TC Spawn",
                    "villager_count": 3,
                    "resources": { "food": 3, "wood": 0, "gold": 0, "stone": 0 },
                    "notes": ["Queue scout"]
                },
                { "time": "<b>Rally point</b>" }
            ]
        });
        let repairs = normalize_imported_json(&mut bo);
        assert!(bo["build_order"][0]["time"].is_null());
        assert_eq!(bo["build_order"][0]["notes"][0], "TC Spawn");
        assert_eq!(bo["build_order"][0]["notes"][1], "Queue scout");
        assert!(bo["build_order"][1]["time"].is_null());
        assert_eq!(bo["build_order"][1]["notes"][0], "Rally point");
        assert!(repairs
            .iter()
            .any(|repair| repair.contains("Moved non-time time value")));
    }

    #[test]
    fn moves_time_age_icons_into_step_age() {
        let mut bo = json!({
            "build_order": [
                {
                    "age": 1,
                    "time": "<img class=\"v-img__img\" src=\"https://aoe4guides.com/assets/pictures/age/age_2.webp\">",
                    "notes": ["Click up"]
                },
                {
                    "time": "@age2@",
                    "notes": ["Castle transition"]
                }
            ]
        });
        let repairs = normalize_imported_json(&mut bo);
        assert_eq!(bo["build_order"][0]["age"], 2);
        assert!(bo["build_order"][0]["time"].is_null());
        assert_eq!(bo["build_order"][1]["age"], 2);
        assert!(bo["build_order"][1]["time"].is_null());
        assert!(repairs
            .iter()
            .any(|repair| repair.contains("Moved age icon from time into age on step 1")));
    }

    #[test]
    fn normalizes_decoded_arrow_notes_while_stripping_html_tags() {
        let mut bo = json!({
            "build_order": [
                {
                    "notes": [
                        "&lt;- Remacro <b>now</b>",
                        "Use &quot;wheelbarrow&quot; &amp; keep macro"
                    ]
                }
            ]
        });
        let repairs = normalize_imported_json(&mut bo);
        assert_eq!(bo["build_order"][0]["notes"][0], "Remacro now");
        assert_eq!(
            bo["build_order"][0]["notes"][1],
            "Use \"wheelbarrow\" & keep macro"
        );
        assert!(repairs
            .iter()
            .any(|repair| repair.contains("Cleaned note markup")));
    }

    #[test]
    fn normalizes_note_alias_arrays() {
        let mut bo = json!({
            "build_order": [
                { "actions": ["Queue villager", "Send scout"], "time": "0" },
                { "tasks": ["Build house", 2, true, null], "time": "1.00" }
            ]
        });
        let repairs = normalize_imported_json(&mut bo);
        assert_eq!(bo["build_order"][0]["notes"][0], "Queue villager");
        assert_eq!(bo["build_order"][0]["notes"][1], "Send scout");
        assert!(bo["build_order"][0]["actions"].is_null());
        assert_eq!(bo["build_order"][1]["notes"][0], "Build house");
        assert_eq!(bo["build_order"][1]["notes"][1], "2");
        assert_eq!(bo["build_order"][1]["notes"][2], "true");
        assert!(bo["build_order"][1]["tasks"].is_null());
        assert!(repairs
            .iter()
            .any(|repair| repair.contains("actions into notes")));
        assert!(repairs
            .iter()
            .any(|repair| repair.contains("tasks into notes")));
    }

    #[test]
    fn normalizes_local_png_icon_tokens_to_asset_stems() {
        let mut bo = json!({
            "build_order": [
                {
                    "notes": [
                        "@resource/resource_wood.png@ @building_economy/house.png@ @unit_worker/villager.png@ @time/time.png@"
                    ]
                }
            ]
        });
        let repairs = normalize_imported_json(&mut bo);
        assert_eq!(
            bo["build_order"][0]["notes"][0],
            "@resource/resource_wood@ @building_economy/house@ @unit_worker/villager@ @time/time.png@"
        );
        assert!(repairs
            .iter()
            .any(|repair| repair.contains("Mapped icon token")));
    }

    #[test]
    fn keeps_non_icon_at_segments_as_note_text() {
        let mut bo = json!({
            "build_order": [
                {
                    "notes": [
                        "@ennemy @resource/resource_gold.png@ and @50 @resource/resource_wood.png@",
                        "@3900/4000 or 200@resource/resource_gold.png   @unit_worker/villager.png@@landmark_templar/abbey-of-kings@upgrades"
                    ]
                }
            ]
        });
        normalize_imported_json(&mut bo);
        assert_eq!(
            bo["build_order"][0]["notes"][0],
            "ennemy @resource/resource_gold@ and 50 @resource/resource_wood@"
        );
        assert_eq!(
            bo["build_order"][0]["notes"][1],
            "3900/4000 or 200 @resource/resource_gold@ @unit_worker/villager@ @landmark_english/abbey-of-kings@ upgrades"
        );
    }

    #[test]
    fn normalizes_age_names_and_aliases() {
        let mut bo = json!({
            "build_order": [
                { "age": "2", "phase": "Feudal Age", "notes": ["age name"] },
                { "ageName": "Castle Age", "notes": ["alias name"] },
                { "era": "IV", "notes": ["roman alias"] },
                { "age": "Age II - Feudal", "notes": ["phrase age"] },
                { "phase": "Castle Age (III)", "notes": ["parenthetical age"] },
                { "era": "Feudal (Age 2)", "notes": ["suffix age"] }
            ]
        });
        let repairs = normalize_imported_json(&mut bo);
        assert_eq!(bo["build_order"][0]["age"], 2);
        assert!(bo["build_order"][0]["phase"].is_null());
        assert_eq!(bo["build_order"][1]["age"], 3);
        assert!(bo["build_order"][1]["ageName"].is_null());
        assert_eq!(bo["build_order"][2]["age"], 4);
        assert!(bo["build_order"][2]["era"].is_null());
        assert_eq!(bo["build_order"][3]["age"], 2);
        assert_eq!(bo["build_order"][4]["age"], 3);
        assert!(bo["build_order"][4]["phase"].is_null());
        assert_eq!(bo["build_order"][5]["age"], 2);
        assert!(bo["build_order"][5]["era"].is_null());
        assert!(repairs
            .iter()
            .any(|repair| repair.contains("Normalized age on step 1")));
        assert!(repairs
            .iter()
            .any(|repair| repair.contains("Removed duplicate step 1 phase age")));
        assert!(repairs
            .iter()
            .any(|repair| repair.contains("ageName into age")));
        assert!(repairs.iter().any(|repair| repair.contains("era into age")));
    }

    #[test]
    fn normalizes_civilization_aliases_and_values() {
        let mut bo = json!({
            "civ": "ENG / zhu xis legacy / Order Of The Dragon",
            "build_order": ["Queue villagers"]
        });
        let repairs = normalize_imported_json(&mut bo);
        assert_eq!(
            bo["civilization"],
            json!(["English", "Zhu Xi's Legacy", "Order of the Dragon"])
        );
        assert!(bo["civ"].is_null());
        assert!(repairs
            .iter()
            .any(|repair| repair.contains("Renamed civ to civilization")));
        assert!(repairs
            .iter()
            .any(|repair| repair.contains("Normalized civilization value")));

        let mut bo = json!({
            "civilisations": ["JDA", "house_of_lancaster"],
            "build_order": ["Scout"]
        });
        let repairs = normalize_imported_json(&mut bo);
        assert_eq!(
            bo["civilization"],
            json!(["Jeanne d'Arc", "House of Lancaster"])
        );
        assert!(bo["civilisations"].is_null());
        assert!(repairs
            .iter()
            .any(|repair| repair.contains("Renamed civilisations to civilization")));

        let mut bo = json!({
            "civ": "KTE / MAC / SEN / TUG / GOH / HOL",
            "build_order": ["Scout"]
        });
        normalize_imported_json(&mut bo);
        assert_eq!(
            bo["civilization"],
            json!([
                "Knights Templar",
                "Macedonian Dynasty",
                "Sengoku Daimyo",
                "Tughlaq Dynasty",
                "Golden Horde",
                "House of Lancaster"
            ])
        );
    }

    #[test]
    fn fills_missing_aoe4guides_civilization_from_metadata() {
        for (code, expected) in [
            ("KTE", "Knights Templar"),
            ("MAC", "Macedonian Dynasty"),
            ("SEN", "Sengoku Daimyo"),
            ("TUG", "Tughlaq Dynasty"),
            ("GOH", "Golden Horde"),
            ("HOL", "House of Lancaster"),
        ] {
            let mut bo = json!({
                "name": format!("Missing civ {code}"),
                "build_order": ["Queue villagers"]
            });
            let metadata = json!({ "civ": code });
            let mut repairs = Vec::new();
            fill_missing_civilization_from_metadata(&mut bo, &metadata, &mut repairs);
            assert_eq!(bo["civilization"], expected, "{code}");
            assert!(repairs
                .iter()
                .any(|repair| repair.contains("Filled missing civilization")));
        }

        let mut bo = json!({
            "name": "Fast 2 TC Knights Templar!",
            "civilization": "Knights Templar",
            "build_order": ["Queue villagers"]
        });
        let mut repairs = Vec::new();
        let metadata = json!({ "civ": "MAC" });
        fill_missing_civilization_from_metadata(&mut bo, &metadata, &mut repairs);
        assert_eq!(bo["civilization"], "Knights Templar");
    }

    #[test]
    fn normalizes_imported_json_values_and_reports_repairs() {
        let mut bo = json!({
            "build_order": [
                { "time": "=3:50", "age": "1", "population_count": "7", "population": "7 pop", "villager_count": "6", "vills": "6", "resources": { "food": "6", "w": "0", "gold": "0", "stone": "0", "builder": null }, "notes": ["- [ ] build", ""] },
                { "time": "<img src=\"age_2.webp\">", "resources": { "food": 0, "wood": 0, "gold": 0, "stone": 0, "builder": -1 }, "notes": ["age<br><b>up</b> @https://age4builder.com/img/villagerAD.png@ @https://age4builder.com/img/resourcewoodicon.png@ @https://age4builder.com/img/gristmill.png@ @https://age4builder.com/img/imperialofficial.png@ @https://age4builder.com/img/stable.png@"] },
                { "resources": "8/3/2/0", "note": "compact resources" },
                { "food": "10", "woodCount": "4", "gold_count": "2", "stone": "0", "builders": "1" },
                { "age": -1, "population_count": -1, "villager_count": "-1", "time": "1:00", "notes": ["sentinels"] },
                { "age": -1, "population_count": -1, "villager_count": -1, "resources": { "food": 0, "wood": 0, "gold": 0, "stone": 0, "builder": -1 }, "notes": [""] }
            ]
        });
        let repairs = normalize_imported_json(&mut bo);
        assert_eq!(bo["build_order"][0]["time"], "3:50");
        assert_eq!(bo["build_order"][0]["age"], 1);
        assert_eq!(bo["build_order"][0]["population_count"], 7);
        assert_eq!(bo["build_order"][0]["villager_count"], 6);
        assert!(bo["build_order"][0]["population"].is_null());
        assert!(bo["build_order"][0]["vills"].is_null());
        assert_eq!(bo["build_order"][0]["resources"]["food"], 6);
        assert_eq!(bo["build_order"][0]["resources"]["wood"], 0);
        assert!(bo["build_order"][0]["resources"]["w"].is_null());
        assert_eq!(bo["build_order"][0]["resources"]["builder"], -1);
        assert_eq!(bo["build_order"][0]["notes"].as_array().unwrap().len(), 1);
        assert_eq!(bo["build_order"][0]["notes"][0], "build");
        assert!(bo["build_order"][1].get("time").is_none());
        assert_eq!(bo["build_order"][1]["notes"][0], "age");
        assert_eq!(
            bo["build_order"][1]["notes"][1],
            "up @unit_worker/villager-abbasid@ @resource/resource_wood@ @building_economy/mill@ @unit_chinese/imperial-official@ @building_military/stable@"
        );
        assert_eq!(bo["build_order"][2]["resources"]["food"], 8);
        assert_eq!(bo["build_order"][2]["resources"]["wood"], 3);
        assert_eq!(bo["build_order"][2]["resources"]["gold"], 2);
        assert_eq!(bo["build_order"][2]["resources"]["stone"], 0);
        assert_eq!(bo["build_order"][2]["resources"]["builder"], -1);
        assert_eq!(bo["build_order"][3]["resources"]["food"], 10);
        assert_eq!(bo["build_order"][3]["resources"]["wood"], 4);
        assert_eq!(bo["build_order"][3]["resources"]["gold"], 2);
        assert_eq!(bo["build_order"][3]["resources"]["stone"], 0);
        assert_eq!(bo["build_order"][3]["resources"]["builder"], 1);
        assert!(bo["build_order"][3]["food"].is_null());
        assert!(bo["build_order"][3]["woodCount"].is_null());
        assert!(bo["build_order"][3]["gold_count"].is_null());
        assert!(bo["build_order"][4]["age"].is_null());
        assert!(bo["build_order"][4]["population_count"].is_null());
        assert!(bo["build_order"][4]["villager_count"].is_null());
        assert_eq!(bo["build_order"].as_array().unwrap().len(), 5);
        assert!(repairs.iter().any(|r| r.contains("Normalized time")));
        assert!(repairs
            .iter()
            .any(|r| r.contains("Removed invalid HTML time")));
        assert!(repairs.iter().any(|r| r.contains("Removed blank note")));
        assert!(repairs.iter().any(|r| r.contains("Mapped icon token")));
        assert!(repairs
            .iter()
            .any(|r| r.contains("Split multiline note on step 2")));
        assert!(repairs
            .iter()
            .any(|r| r.contains("Removed duplicate step 1 population count")));
        assert!(repairs
            .iter()
            .any(|r| r.contains("Removed duplicate step 1 vills count")));
        assert!(repairs
            .iter()
            .any(|r| r.contains("Converted step 3 resources")));
        assert!(repairs
            .iter()
            .any(|r| r.contains("loose woodCount into resources.wood")));
        assert!(repairs
            .iter()
            .any(|r| r.contains("Removed hidden age sentinel")));
        assert!(repairs
            .iter()
            .any(|r| r.contains("Removed hidden population_count sentinel")));
        assert!(repairs
            .iter()
            .any(|r| r.contains("Removed hidden villager_count sentinel")));
        assert!(repairs.iter().any(|r| r.contains("Removed 1 blank step")));
        let summary = summarize_repairs(&repairs);
        assert_eq!(summary.total, repairs.len());
        assert_eq!(summary.times, 2);
        assert_eq!(summary.icons, 1);
        assert!(summary.notes >= 1);
        assert!(summary.resources >= 4);
        assert_eq!(summary.steps, 1);
    }

    #[test]
    fn normalizes_count_aliases_on_imported_steps() {
        let mut bo = json!({
            "build_order": [
                { "worker_count": "7", "supplyUsed": "8", "notes": ["worker aliases"] },
                { "villCount": "12", "popCap": "20", "notes": ["count aliases"] },
                { "worker": "9", "supply_count": "10", "notes": ["singular aliases"] },
                { "villager": "5", "pop_count": "6", "notes": ["short aliases"] }
            ]
        });
        let repairs = normalize_imported_json(&mut bo);
        assert_eq!(bo["build_order"][0]["villager_count"], 7);
        assert_eq!(bo["build_order"][0]["population_count"], 8);
        assert_eq!(bo["build_order"][1]["villager_count"], 12);
        assert_eq!(bo["build_order"][1]["population_count"], 20);
        assert_eq!(bo["build_order"][2]["villager_count"], 9);
        assert_eq!(bo["build_order"][2]["population_count"], 10);
        assert_eq!(bo["build_order"][3]["villager_count"], 5);
        assert_eq!(bo["build_order"][3]["population_count"], 6);
        for alias in [
            "worker_count",
            "supplyUsed",
            "villCount",
            "popCap",
            "worker",
            "supply_count",
            "villager",
            "pop_count",
        ] {
            assert!(bo["build_order"]
                .as_array()
                .unwrap()
                .iter()
                .all(|step| step[alias].is_null()));
        }
        assert!(repairs
            .iter()
            .any(|r| r.contains("worker_count into villager_count")));
        assert!(repairs
            .iter()
            .any(|r| r.contains("supplyUsed into population_count")));
    }

    #[test]
    fn normalizes_resource_arrays_and_alias_fields() {
        let mut bo = json!({
            "build_order": [
                { "resources": [6, "2", 0, 0, "1"], "notes": ["array resources"] },
                { "resource_allocation": "8/3/2/0/1", "notes": ["alias text"] },
                { "villagers_on": [10, 4, 2, 0], "notes": ["alias array"] },
                { "resources": "9,3,1,0,2", "notes": ["comma text"] },
                { "resources": "11-5-3-0", "notes": ["dash text"] },
                { "woodCount": "4", "resources": { "wood": "4", "food": 6, "f": "6", "builder": "1", "builders": "1" }, "notes": ["duplicate aliases"] }
            ]
        });
        let repairs = normalize_imported_json(&mut bo);
        assert_eq!(bo["build_order"][0]["resources"]["food"], 6);
        assert_eq!(bo["build_order"][0]["resources"]["wood"], 2);
        assert_eq!(bo["build_order"][0]["resources"]["builder"], 1);
        assert_eq!(bo["build_order"][1]["resources"]["food"], 8);
        assert_eq!(bo["build_order"][1]["resources"]["wood"], 3);
        assert_eq!(bo["build_order"][1]["resources"]["builder"], 1);
        assert!(bo["build_order"][1]["resource_allocation"].is_null());
        assert_eq!(bo["build_order"][2]["resources"]["food"], 10);
        assert_eq!(bo["build_order"][2]["resources"]["gold"], 2);
        assert!(bo["build_order"][2]["villagers_on"].is_null());
        assert_eq!(bo["build_order"][3]["resources"]["food"], 9);
        assert_eq!(bo["build_order"][3]["resources"]["builder"], 2);
        assert_eq!(bo["build_order"][4]["resources"]["food"], 11);
        assert_eq!(bo["build_order"][4]["resources"]["wood"], 5);
        assert_eq!(bo["build_order"][4]["resources"]["gold"], 3);
        assert_eq!(bo["build_order"][4]["resources"]["stone"], 0);
        assert!(bo["build_order"][5]["woodCount"].is_null());
        assert!(bo["build_order"][5]["resources"]["f"].is_null());
        assert!(bo["build_order"][5]["resources"]["builders"].is_null());
        assert_eq!(bo["build_order"][5]["resources"]["wood"], 4);
        assert_eq!(bo["build_order"][5]["resources"]["food"], 6);
        assert_eq!(bo["build_order"][5]["resources"]["builder"], 1);
        assert!(repairs
            .iter()
            .any(|r| r.contains("Converted step 1 resources")));
        assert!(repairs
            .iter()
            .any(|r| r.contains("resource_allocation into resources")));
        assert!(repairs
            .iter()
            .any(|r| r.contains("villagers_on into resources")));
        assert!(repairs
            .iter()
            .any(|r| r.contains("Removed duplicate step 6 loose woodCount resource")));
        assert!(repairs
            .iter()
            .any(|r| r.contains("Removed duplicate step 6 resource builders")));
    }

    #[test]
    fn normalizes_labeled_resource_allocation_strings() {
        let mut bo = json!({
            "build_order": [
                { "resources": "food=6 wood=2 gold=0 stone=0 builder=1", "notes": ["label equals"] },
                { "resource_allocation": "6f 2w 0g 0s", "notes": ["compact suffix"] },
                { "villagers_on": "f8 w3 g2 s0 b1", "notes": ["compact prefix"] },
                { "resources": "6 food 2 wood 1 gold 0 stone", "notes": ["number label"] }
            ]
        });
        let repairs = normalize_imported_json(&mut bo);
        assert_eq!(bo["build_order"][0]["resources"]["food"], 6);
        assert_eq!(bo["build_order"][0]["resources"]["wood"], 2);
        assert_eq!(bo["build_order"][0]["resources"]["builder"], 1);
        assert_eq!(bo["build_order"][1]["resources"]["food"], 6);
        assert_eq!(bo["build_order"][1]["resources"]["wood"], 2);
        assert_eq!(bo["build_order"][1]["resources"]["builder"], -1);
        assert!(bo["build_order"][1]["resource_allocation"].is_null());
        assert_eq!(bo["build_order"][2]["resources"]["food"], 8);
        assert_eq!(bo["build_order"][2]["resources"]["gold"], 2);
        assert_eq!(bo["build_order"][2]["resources"]["builder"], 1);
        assert!(bo["build_order"][2]["villagers_on"].is_null());
        assert_eq!(bo["build_order"][3]["resources"]["food"], 6);
        assert_eq!(bo["build_order"][3]["resources"]["gold"], 1);
        assert!(repairs
            .iter()
            .any(|r| r.contains("resource_allocation into resources")));
        assert!(repairs
            .iter()
            .any(|r| r.contains("villagers_on into resources")));
    }

    #[test]
    fn explains_empty_builds_after_blank_step_cleanup() {
        let err = empty_build_error(
            "blank",
            &[
                "Removed 1 blank step.".to_string(),
                "Normalized time on step 1.".to_string(),
            ],
        );
        assert_eq!(
            err,
            "aoe4guides build 'blank' has no usable steps after removing blank steps."
        );
        assert_eq!(
            empty_build_error("empty", &[]),
            "aoe4guides build 'empty' has no steps."
        );
    }

    #[test]
    fn ui_time_normalizers_cover_backend_clock_quirks() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap();
        let index = std::fs::read_to_string(root.join("ui/index.html")).unwrap();
        let buildorder = std::fs::read_to_string(root.join("ui/buildorder.html")).unwrap();
        let three_part_clock = r"^0+[.:](\d+)[.:](\d+)$";
        let missing_minute_clock = r"^[:.](\d+)$";
        let seconds_clock = r"^(\d+)\s*(s|sec|secs|second|seconds)$";
        let onward_suffix = "on|onward|onwards";
        assert!(
            index.contains(three_part_clock),
            "index.html missing zero-prefixed three-part clock normalization"
        );
        assert!(
            buildorder.contains(three_part_clock),
            "buildorder.html missing zero-prefixed three-part clock normalization"
        );
        assert!(
            index.contains(missing_minute_clock),
            "index.html missing shorthand second clock normalization"
        );
        assert!(
            buildorder.contains(missing_minute_clock),
            "buildorder.html missing shorthand second clock normalization"
        );
        assert!(
            index.contains(seconds_clock),
            "index.html missing seconds-only clock normalization"
        );
        assert!(
            buildorder.contains(seconds_clock),
            "buildorder.html missing seconds-only clock normalization"
        );
        assert!(
            index.contains(onward_suffix),
            "index.html missing onward suffix clock normalization"
        );
        assert!(
            buildorder.contains(onward_suffix),
            "buildorder.html missing onward suffix clock normalization"
        );
    }

    #[test]
    fn ui_normalizers_keep_blank_step_filtering_in_parity() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap();
        let index = std::fs::read_to_string(root.join("ui/index.html")).unwrap();
        let buildorder = std::fs::read_to_string(root.join("ui/buildorder.html")).unwrap();
        assert!(
            index.contains("Removed blank step"),
            "index.html missing editor blank-step cleanup"
        );
        assert!(
            buildorder.contains("function isNonBlankStep")
                && buildorder.contains(".filter(isNonBlankStep)"),
            "buildorder.html missing overlay blank-step cleanup"
        );
    }

    #[test]
    fn ui_json_repair_helpers_keep_editor_overlay_parity() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap();
        let index = std::fs::read_to_string(root.join("ui/index.html")).unwrap();
        let buildorder = std::fs::read_to_string(root.join("ui/buildorder.html")).unwrap();
        assert!(
            index.contains("jsonc|json|javascript|js"),
            "index.html must match jsonc fences before json fences"
        );
        assert!(
            buildorder.contains("jsonc|json|javascript|js"),
            "buildorder.html must match jsonc fences before json fences"
        );
        assert!(
            index.contains("function boStripJsonComments"),
            "index.html missing quote-aware JSON comment stripping"
        );
        assert!(
            index.contains("function boExtractJsonLike")
                && index.contains("Extracted JSON ${open === '{' ? 'object' : 'array'}"),
            "index.html missing wrapped JSON extraction"
        );
        assert!(
            buildorder.contains("function extractJsonLike")
                && buildorder.contains("extractJsonLike(stripJsonFence"),
            "buildorder.html missing wrapped JSON extraction"
        );
        assert!(
            buildorder.contains("function stripJsonComments"),
            "buildorder.html missing quote-aware JSON comment stripping"
        );
        assert!(
            index.contains("function boInsertMissingJsonCommas")
                && index.contains("Inserted missing commas"),
            "index.html missing missing-comma JSON repair"
        );
        assert!(
            buildorder.contains("function insertMissingJsonCommas"),
            "buildorder.html missing missing-comma JSON repair"
        );
        assert!(
            index.contains("nums.length !== 4 && nums.length !== 5")
                && index.contains("builder: nums[4] ?? -1"),
            "index.html missing five-slot resource string parsing"
        );
        assert!(
            buildorder.contains("nums.length !== 4 && nums.length !== 5")
                && buildorder.contains("builder: nums[4] ?? -1"),
            "buildorder.html missing five-slot resource string parsing"
        );
        assert!(
            index.matches("Quoted bare object keys").count() >= 2,
            "index.html missing post-comma bare-key JSON repair"
        );
        assert!(
            buildorder
                .matches("([A-Za-z_][A-Za-z0-9_]*)(\\s*[:=])")
                .count()
                >= 2,
            "buildorder.html missing post-comma bare-key JSON repair"
        );
        assert!(
            index.contains("function boNormalizeJsonKeyEquals")
                && index
                    .matches("Converted key equals signs to colons")
                    .count()
                    >= 2,
            "index.html missing key-equals JSON repair"
        );
        assert!(
            buildorder.contains("function normalizeJsonKeyEquals")
                && buildorder.matches("normalizeJsonKeyEquals").count() >= 3,
            "buildorder.html missing key-equals JSON repair"
        );
        assert!(
            index.contains("function boConvertSingleQuotedJsonStrings")
                && index.contains("function boIsWordApostrophe"),
            "index.html missing apostrophe-aware single-quoted JSON repair"
        );
        assert!(
            buildorder.contains("function convertSingleQuotedJsonStrings")
                && buildorder.contains("function isWordApostrophe"),
            "buildorder.html missing apostrophe-aware single-quoted JSON repair"
        );
        assert!(
            index.contains("function boConvertNonJsonLiterals")
                && index.contains("Converted Python/JS-style literals"),
            "index.html missing JS/Python literal JSON repair"
        );
        assert!(
            buildorder.contains("function convertNonJsonLiterals")
                && buildorder.contains("convertNonJsonLiterals(fixed)"),
            "buildorder.html missing JS/Python literal JSON repair"
        );
        assert!(
            index.contains("function boQuoteKnownBareStringValues")
                && index.contains("Quoted bare text values"),
            "index.html missing known-field bare string value repair"
        );
        assert!(
            buildorder.contains("function quoteKnownBareStringValues")
                && buildorder.contains("quoteKnownBareStringValues(fixed)"),
            "buildorder.html missing known-field bare string value repair"
        );
        assert!(
            index.contains("function boQuoteKnownBareStringArrayValues")
                && index.contains("Quoted bare text array values"),
            "index.html missing known-field bare string array repair"
        );
        assert!(
            buildorder.contains("function quoteKnownBareStringArrayValues")
                && buildorder.contains("quoteKnownBareStringArrayValues(fixed)"),
            "buildorder.html missing known-field bare string array repair"
        );
        assert!(
            index.contains("function boIsStepLikeObject")
                && index.contains("const BO_STEP_LIKE_KEYS"),
            "index.html missing single-step object wrapping"
        );
        assert!(
            buildorder.contains("function isStepLikeObject")
                && buildorder.contains("const STEP_LIKE_KEYS"),
            "buildorder.html missing single-step object wrapping"
        );
        assert!(
            index.contains("function boStepLinesFromString"),
            "index.html missing string step collection splitting"
        );
        assert!(
            buildorder.contains("function stepLinesFromString"),
            "buildorder.html missing string step collection splitting"
        );
        assert!(
            index.contains("return Array.isArray(value) || !!(value && typeof value === 'object' && boStepCollection(value))"),
            "index.html must not treat bare wrapper strings as direct step collections"
        );
        assert!(
            buildorder.contains("return Array.isArray(value) || !!(value && typeof value === 'object' && stepCollection(value))"),
            "buildorder.html must not treat bare wrapper strings as direct step collections"
        );
        assert!(
            index.contains("function boNormalizeJsonSemicolons")
                && index.contains("Normalized semicolon separators"),
            "index.html missing semicolon JSON repair"
        );
        assert!(
            buildorder.contains("function normalizeJsonSemicolons"),
            "buildorder.html missing semicolon JSON repair"
        );
        assert!(
            buildorder.contains("function parseBuildJsonContent"),
            "buildorder.html missing overlay repaired JSON parsing"
        );
        assert!(
            index.contains("boFlattenSectionedSteps"),
            "index.html missing aoe4guides section flattening"
        );
        assert!(
            buildorder.contains("function flattenSectionedSteps"),
            "buildorder.html missing aoe4guides section flattening"
        );
        assert!(
            index.contains("boReplaceImgTagsWithIconTokens"),
            "index.html missing aoe4guides img icon preservation"
        );
        assert!(
            buildorder.contains("function replaceImgTagsWithIconTokens"),
            "buildorder.html missing aoe4guides img icon preservation"
        );
        assert!(
            index.contains("function boNormalizeNoteLineSpacing"),
            "index.html missing note whitespace normalization"
        );
        assert!(
            buildorder.contains("function normalizeNoteLineSpacing"),
            "buildorder.html missing note whitespace normalization"
        );
        assert!(
            index.contains("function boStripNoteListMarker"),
            "index.html missing note list-marker cleanup"
        );
        assert!(
            buildorder.contains("function stripNoteListMarker"),
            "buildorder.html missing note list-marker cleanup"
        );
        assert!(
            index.contains("Removed blank metadata")
                && index.contains("description', 'author', 'video', 'season', 'map', 'strategy"),
            "index.html missing blank metadata cleanup"
        );
        assert!(
            buildorder.contains("description', 'author', 'video', 'season', 'map', 'strategy"),
            "buildorder.html missing blank metadata cleanup"
        );
        assert!(
            index.contains("Split multiline note") && index.contains(".flatMap(note =>"),
            "index.html missing multiline note splitting"
        );
        assert!(
            buildorder.contains(".flatMap(note => normalizeNoteValue(note)"),
            "buildorder.html missing multiline note splitting"
        );
        assert!(
            index.contains("Removed hidden ${key} sentinel"),
            "index.html missing hidden count sentinel cleanup"
        );
        assert!(
            buildorder.contains("if (step[key] < 0) delete step[key]"),
            "buildorder.html missing hidden count sentinel cleanup"
        );
        assert!(
            index.contains("function boTimeAgeIconValue")
                && index.contains("Moved age icon from step"),
            "index.html missing time age-icon repair"
        );
        assert!(
            buildorder.contains("function timeAgeIconValue"),
            "buildorder.html missing time age-icon repair"
        );
        assert!(
            buildorder.contains("delete step.note")
                && buildorder.contains("delete step.description"),
            "buildorder.html must remove note/description aliases after moving them to notes"
        );
        assert!(
            buildorder.contains("delete step.resources[alias]"),
            "buildorder.html must remove resource aliases after canonicalizing them"
        );
        assert!(
            index.contains("Removed duplicate step") && index.contains("aliasN === canonicalN"),
            "index.html missing duplicate count alias cleanup"
        );
        assert!(
            buildorder.contains("aliasN === canonicalN"),
            "buildorder.html missing duplicate count alias cleanup"
        );
        assert!(
            index.contains("loose ${alias} resource") && index.contains("resource ${alias}"),
            "index.html missing duplicate resource alias cleanup"
        );
        assert!(
            buildorder.matches("delete step.resources[alias]").count() >= 2,
            "buildorder.html missing duplicate resource alias cleanup"
        );
        assert!(
            index.contains("Removed duplicate step ${i + 1} ${key} age")
                && index.contains("aliasAge === canonicalAge"),
            "index.html missing duplicate age alias cleanup"
        );
        assert!(
            buildorder.contains("aliasAge === canonicalAge"),
            "buildorder.html missing duplicate age alias cleanup"
        );
        assert!(
            index.contains("Renamed ${alias} to name"),
            "index.html missing build title/name normalization repair"
        );
        assert!(
            buildorder.contains("build_name"),
            "buildorder.html missing build title/name normalization"
        );
        assert!(
            index.contains("Derived source from aoe4guides id")
                && index.contains("https://aoe4guides.com/builds/${j.id.trim()}"),
            "index.html missing aoe4guides source derivation"
        );
        assert!(
            buildorder.contains("https://aoe4guides.com/builds/${bo.id.trim()}"),
            "buildorder.html missing aoe4guides source derivation"
        );
        assert!(
            index.contains("function boCanonicalBuildSource")
                && index.contains("Renamed ${alias} to source"),
            "index.html missing build source alias normalization"
        );
        assert!(
            buildorder.contains("function canonicalBuildSource")
                && buildorder.contains("sourceUrl"),
            "buildorder.html missing build source alias normalization"
        );
        assert!(
            index.contains("Removed private _id from step"),
            "index.html missing private step id cleanup"
        );
        assert!(
            buildorder.contains("delete step._id"),
            "buildorder.html missing private step id cleanup"
        );
    }

    #[test]
    fn ui_icon_alias_maps_cover_backend_aliases() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap();
        let index = std::fs::read_to_string(root.join("ui/index.html")).unwrap();
        let buildorder = std::fs::read_to_string(root.join("ui/buildorder.html")).unwrap();
        for (alias, target) in ICON_ALIASES {
            let unquoted_key = format!("{alias}:");
            let quoted_key = format!("'{alias}':");
            let value = format!("'{target}'");
            assert!(
                index.contains(&unquoted_key) || index.contains(&quoted_key),
                "index.html missing alias {alias}"
            );
            assert!(
                buildorder.contains(&unquoted_key) || buildorder.contains(&quoted_key),
                "buildorder.html missing alias {alias}"
            );
            assert!(index.contains(&value), "index.html missing target {target}");
            assert!(
                buildorder.contains(&value),
                "buildorder.html missing target {target}"
            );
        }
    }

    #[test]
    fn ui_builder_allocation_chips_are_explicit_and_positive() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap();
        let buildorder = std::fs::read_to_string(root.join("ui/buildorder.html")).unwrap();
        assert!(
            root.join("ui/bo_img/abilities/repair.webp").exists(),
            "builder allocation icon asset missing"
        );
        assert!(
            buildorder.contains("builder: 'bo_img/abilities/repair.webp'"),
            "builder allocations should use the repair/building icon"
        );
        assert!(
            buildorder.contains("builder: 'Building villagers'"),
            "builder allocations should be labelled as building villagers"
        );
        assert!(
            buildorder.contains("if ((r.builder ?? -1) > 0) keys.push('builder');"),
            "builder allocation chips should only render for positive builder counts"
        );
    }

    #[test]
    fn ui_delta_chips_show_signed_amounts() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap();
        let buildorder = std::fs::read_to_string(root.join("ui/buildorder.html")).unwrap();
        assert!(
            buildorder.contains("content: attr(data-delta)"),
            "delta chips should render signed amounts, not direction-only arrows"
        );
        assert!(
            buildorder.contains("function numericDelta")
                && buildorder.contains("Number.isFinite(value)")
                && buildorder.contains("Number.isFinite(previous)")
                && buildorder.contains("value < 0 || previous < 0"),
            "delta comparisons should only use explicit nonnegative numeric values"
        );
        assert!(
            buildorder.contains("function deltaText")
                && buildorder.contains("`${delta > 0 ? '+' : ''}${delta}`"),
            "delta text should include a leading plus for increases"
        );
        assert!(
            buildorder.contains("deltaText(r[k], pr[k])")
                && buildorder.contains("deltaText(villagers, prevVillagers)")
                && buildorder.contains("deltaText(population, prevPopulation)"),
            "resource, villager, and population chips should receive signed deltas"
        );
    }

    #[test]
    fn ui_villager_drop_tooltips_explain_builder_pulls() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap();
        let buildorder = std::fs::read_to_string(root.join("ui/buildorder.html")).unwrap();
        assert!(
            buildorder.contains("function builderPullDetail")
                && buildorder.contains("delta >= 0 || !action")
                && buildorder.contains("Likely ${count} villager")
                && buildorder.contains("pulled from gathering for ${target}"),
            "builder-pull hints should only appear on villager drops with a structure action"
        );
        assert!(
            buildorder.contains("function villagerDeltaTitle")
                && buildorder.contains("deltaTitle(value, previous)")
                && buildorder.contains("builderPullDetail(value, previous, action)"),
            "current villager chip tooltips should combine exact deltas with builder-pull context"
        );
        assert!(
            buildorder.contains("function previewVillagerDeltaTitle")
                && buildorder.contains("previewDeltaTitle(value, previous)")
                && buildorder
                    .contains("previewVillagerDeltaTitle(villagers, previousVillagers, action)"),
            "next-step villager chip tooltips should describe upcoming builder pulls"
        );
        assert!(
            buildorder.contains("villagerDeltaTitle(villagers, prevVillagers, action)")
                && buildorder.contains("deltaText(villagers, prevVillagers)")
                && buildorder.contains("deltaText(villagers, previousVillagers)"),
            "builder-pull context should preserve the existing signed villager delta chips"
        );
    }

    #[test]
    fn ui_next_preview_chips_show_signed_amounts() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap();
        let buildorder = std::fs::read_to_string(root.join("ui/buildorder.html")).unwrap();
        assert!(
            buildorder.contains("#next .nx-chip.up::after")
                && buildorder.contains("#next .nx-chip.down::after")
                && buildorder.contains("content: attr(data-delta)"),
            "next-step chips should render signed delta amounts"
        );
        assert!(
            buildorder.contains("function previewDeltaTitle")
                && buildorder.contains("Will increase by")
                && buildorder.contains("from current"),
            "next-step delta titles should describe changes from the current step"
        );
        assert!(
            buildorder.contains("function stepPreviewHtml(step, previousStep = null)")
                && buildorder.contains("const pr = previousStep?.resources || {}")
                && buildorder.contains("stepPreviewHtml(nx, s)"),
            "next-step previews should compare the upcoming step to the current step"
        );
        assert!(
            buildorder.contains("deltaText(r[k], pr[k])")
                && buildorder.contains("deltaText(villagers, previousVillagers)")
                && buildorder.contains("deltaText(population, previousPopulation)"),
            "next-step resource, villager, and population chips should receive signed deltas"
        );
    }

    #[test]
    fn ui_next_preview_shows_time_until_next_step() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap();
        let buildorder = std::fs::read_to_string(root.join("ui/buildorder.html")).unwrap();
        assert!(
            buildorder.contains("#next .nx-until") && buildorder.contains("Time until next step"),
            "next-step previews should include a distinct time-until chip"
        );
        assert!(
            buildorder.contains("function exactTimeSeconds")
                && buildorder.contains("time.match(/^(\\d{1,3}):(\\d{2})$/)")
                && buildorder.contains("seconds < 60"),
            "time-until helper should only use exact normalized m:ss times"
        );
        assert!(
            buildorder.contains("function durationText") && buildorder.contains("seconds <= 0"),
            "time-until helper should suppress zero or negative durations"
        );
        assert!(
            buildorder.contains("function nextTimeGapChipHtml")
                && buildorder.contains("const gap = nextSeconds !== null && previousSeconds !== null ? nextSeconds - previousSeconds : 0")
                && buildorder.contains("if (gap) bits.push(gap);"),
            "next-step previews should add a countdown chip only when a positive gap exists"
        );
    }

    #[test]
    fn ui_next_preview_highlights_age_ups() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap();
        let buildorder = std::fs::read_to_string(root.join("ui/buildorder.html")).unwrap();
        assert!(
            buildorder.contains("#next .nx-age-up") && buildorder.contains("rgba(231,192,89,.55)"),
            "next-step age-up chips should have distinct styling"
        );
        assert!(
            buildorder.contains("function nextAgeChipHtml")
                && buildorder.contains("const ageUp = previousAge !== null && age > previousAge")
                && buildorder.contains("Age up!")
                && buildorder.contains("Will advance from"),
            "next-step age-up chips should be labelled from the current age comparison"
        );
        assert!(
            buildorder.contains("const previousAge = ageForDisplay(previousStep?.age)")
                && buildorder.contains("bits.push(nextAgeChipHtml(age, previousAge))"),
            "next-step previews should compare upcoming age to the current step age"
        );
    }

    #[test]
    fn ui_next_preview_hides_unchanged_zero_resources() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap();
        let buildorder = std::fs::read_to_string(root.join("ui/buildorder.html")).unwrap();
        assert!(
            buildorder.contains("function previewResourceKeysForDisplay")
                && buildorder.contains("value > 0 || numericDelta(value, pr[k]) !== 0"),
            "next-step resource previews should keep positive or changed resources"
        );
        assert!(
            buildorder.contains(
                "builder > 0 || (builder >= 0 && numericDelta(builder, pr.builder) !== 0)"
            ),
            "next-step builder previews should keep positive builders or changed builder zeroes"
        );
        assert!(
            buildorder.contains("for (const k of previewResourceKeysForDisplay(r, pr))"),
            "next-step previews should use the compact preview resource filter"
        );
    }

    #[test]
    fn ui_build_action_chips_surface_structure_steps() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap();
        let buildorder = std::fs::read_to_string(root.join("ui/buildorder.html")).unwrap();
        assert!(
            buildorder.contains("function buildActionKind")
                && buildorder.contains("@landmark_[^@]+@")
                && buildorder.contains("@building_[^@]+@")
                && buildorder.contains("villagerToStructure")
                && buildorder.contains("structureOnMap"),
            "overlay should infer structure-action intent from clear build-order note patterns"
        );
        assert!(
            buildorder.contains("function shouldShowBuildActionChip")
                && buildorder.contains("kind === 'Landmark'")
                && buildorder.contains("builder ?? -1"),
            "generic build chips should stay suppressed when an explicit builder allocation already exists"
        );
        assert!(
            buildorder.contains("res action action-${action.toLowerCase()}")
                && buildorder.contains("nx-action nx-action-${action.toLowerCase()}"),
            "current and next-step rows should render the inferred build action chip"
        );
        assert!(
            buildorder.contains("Inferred from the current instruction text")
                && buildorder.contains("Inferred from the next instruction text"),
            "build action chips should explain that they are display-only inference"
        );
    }

    #[test]
    fn ui_rally_chips_surface_rally_targets() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap();
        let buildorder = std::fs::read_to_string(root.join("ui/buildorder.html")).unwrap();
        assert!(
            buildorder.contains("rally: 'bo_img/resource/rally.webp'")
                && buildorder.contains("rally: 'Rally point'"),
            "rally chips should use the rally icon and label"
        );
        assert!(
            buildorder.contains("function rallyCueIndex")
                && buildorder.contains("@resource\\/rally")
                && buildorder.contains("\\brall(?:y|ied|ying)\\b"),
            "rally target inference should be anchored to explicit rally cues"
        );
        assert!(
            buildorder.contains("function rallyTargetFromSegment")
                && buildorder.contains("beforeTarget")
                && buildorder.contains("build|construct|drop|place|make|add")
                && buildorder.contains("resource_food|sheep|deer|berrybush|farm")
                && buildorder.contains("resource_wood|gaiatreeprototypetree"),
            "rally target inference should map common targets without mistaking build actions for rally targets"
        );
        assert!(
            buildorder.contains("rallyTargetKey(step)")
                && buildorder.contains("rallyTargetKey(s)")
                && buildorder.contains("nx-rally nx-rally-${rally}")
                && buildorder.contains("res rally rally-${rally}"),
            "current and next-step rows should render inferred rally target chips"
        );
    }

    #[test]
    fn ui_resource_shift_chips_summarize_reallocations() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap();
        let buildorder = std::fs::read_to_string(root.join("ui/buildorder.html")).unwrap();
        assert!(
            buildorder.contains("shift: 'bo_img/unit_worker/villager.webp'")
                && buildorder.contains("shift: 'Resource shift'"),
            "resource shift chips should use a worker icon and label"
        );
        assert!(
            buildorder.contains("function resourceShiftSummary")
                && buildorder.contains("numericDelta(resources?.[key], previousResources?.[key])")
                && buildorder.contains("const hasUp = changes.some")
                && buildorder.contains("const hasDown = changes.some")
                && buildorder.contains("return hasUp && hasDown ? changes : []"),
            "resource shift chips should only summarize mixed up/down resource reallocations"
        );
        assert!(
            buildorder.contains("function resourceShiftDetail")
                && buildorder.contains("Resource shift:")
                && buildorder.contains("shortResourceLabel(key)"),
            "resource shift chip tooltips should list exact resource deltas"
        );
        assert!(
            buildorder.contains("resourceShiftSummary(r, pr)")
                && buildorder.contains("nx-shift")
                && buildorder.contains("res shift"),
            "current and next-step rows should render resource shift chips"
        );
    }

    #[test]
    fn txt_without_steps_errors() {
        assert!(convert_aoeivbuilds_txt("Title only\n\nno bullets here", None, "x").is_err());
    }

    #[test]
    fn extracts_civ_from_html() {
        let html = "<h4>Map Type</h4><p>Open</p><h4>Civilization</h4>\n  <p>English</p>";
        assert_eq!(extract_civ(html).as_deref(), Some("English"));
        assert_eq!(extract_civ("<h4>Other</h4>"), None);
    }

    #[tokio::test]
    async fn live_import_aoe4guides() {
        let client = reqwest::Client::new();
        let bo = import_from_url(
            &client,
            "https://aoe4guides.com/builds/00I7J47dv26cPbKmXYkO",
        )
        .await
        .expect("import");
        assert!(!bo.name.is_empty());
        let v: Value = serde_json::from_str(&bo.content).unwrap();
        assert!(v["build_order"].as_array().unwrap().len() > 3);
    }

    #[tokio::test]
    async fn live_import_aoeivbuilds() {
        let client = reqwest::Client::new();
        let bo = import_from_url(&client, "https://www.aoeivbuilds.com/build_orders/1296")
            .await
            .expect("import");
        assert_eq!(bo.civilization.as_deref(), Some("English"));
        assert_eq!(bo.repair_summary.total, bo.repairs.len());
        assert!(
            bo.repair_summary.total > 0,
            "aoeivbuilds import should surface converter repairs"
        );
        let v: Value = serde_json::from_str(&bo.content).unwrap();
        assert!(v["build_order"].as_array().unwrap().len() >= 3);
    }

    #[tokio::test]
    async fn live_missing_build_gives_friendly_error() {
        let client = reqwest::Client::new();
        let err = import_from_url(
            &client,
            "https://aoe4guides.com/builds/zzzzzzzzzzzzzzzzzzzz",
        )
        .await
        .unwrap_err();
        assert!(
            err.contains("not found") || err.contains("no steps"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn live_search_by_author() {
        let client = reqwest::Client::new();
        let list = search_aoe4guides(&client, None, None, Some("vOiAUO06vkMXuPuYb92APdyLDUO2"))
            .await
            .expect("author search");
        let arr = list.as_array().unwrap();
        assert!(!arr.is_empty());
        assert!(arr.iter().all(|b| b["author"] == "Cherihawn"));
    }

    #[tokio::test]
    async fn live_search_aoe4guides() {
        let client = reqwest::Client::new();
        let list = search_aoe4guides(&client, Some("ENG"), Some("score"), None)
            .await
            .expect("search");
        let arr = list.as_array().unwrap();
        assert!(!arr.is_empty());
        assert!(arr[0]["id"].is_string());
    }
}
