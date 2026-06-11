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

#[derive(Debug, Clone, Serialize)]
pub struct ImportedBo {
    pub name: String,
    pub content: String,
    pub civilization: Option<String>,
    pub source: String,
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
    let bo: Value = resp
        .json()
        .await
        .map_err(|e| format!("bad JSON from aoe4guides: {e}"))?;
    if bo["build_order"].as_array().is_none_or(|a| a.is_empty()) {
        return Err("aoe4guides returned a build with no steps".into());
    }
    let mut bo = bo;
    if let Some(steps) = bo["build_order"].as_array_mut() {
        for s in steps {
            if let Some(t) = s["time"].as_str() {
                let fixed = normalize_time(t);
                if fixed != t {
                    s["time"] = Value::from(fixed);
                }
            }
        }
    }
    let name = bo["name"].as_str().unwrap_or("Imported build").to_string();
    let civilization = bo["civilization"].as_str().map(str::to_string);
    Ok(ImportedBo {
        name,
        content: serde_json::to_string_pretty(&bo).map_err(|e| e.to_string())?,
        civilization,
        source: format!("https://aoe4guides.com/builds/{id}"),
    })
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
    let bo = convert_aoeivbuilds_txt(&txt, civ.as_deref(), &page_url)?;
    let name = bo["name"].as_str().unwrap_or("Imported build").to_string();
    Ok(ImportedBo {
        name,
        content: serde_json::to_string_pretty(&bo).map_err(|e| e.to_string())?,
        civilization: civ,
        source: page_url,
    })
}

/// Normalize hand-typed step times: "5.30" -> "5:30", "4:5" -> "4:05".
/// Anything that isn't two numeric fields is left untouched.
fn normalize_time(t: &str) -> String {
    let t = t.trim();
    let parts: Vec<&str> = t.split([':', '.']).collect();
    if parts.len() == 2
        && !parts[0].is_empty()
        && parts.iter().all(|p| p.chars().all(|c| c.is_ascii_digit()))
    {
        if let (Ok(m), Ok(s)) = (parts[0].parse::<u32>(), parts[1].parse::<u32>()) {
            if s < 60 {
                return format!("{m}:{s:02}");
            }
        }
    }
    t.to_string()
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

/// Convert the aoeivbuilds plain-text export into RTS_Overlay JSON.
///
/// Step lines look like: `* (food/wood/gold/stone)\ttime\tdescription`
pub fn convert_aoeivbuilds_txt(
    txt: &str,
    civ: Option<&str>,
    source: &str,
) -> Result<Value, String> {
    let mut lines = txt.lines();
    let name = lines
        .next()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .unwrap_or("Imported build")
        .to_string();

    let mut steps = Vec::new();
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
        let nums: Vec<i64> = res
            .trim_start_matches('(')
            .trim_end_matches(')')
            .split('/')
            .map(|n| {
                // values like "6+" or "2/3" degrade to the leading integer
                let digits: String = n
                    .trim()
                    .chars()
                    .take_while(|c| c.is_ascii_digit())
                    .collect();
                digits.parse().unwrap_or(-1)
            })
            .collect();
        let get = |i: usize| nums.get(i).copied().unwrap_or(-1);
        // The four columns are villager allocations, so their sum is the
        // villager count (when every column parsed).
        let vills = if nums.len() == 4 && nums.iter().all(|n| *n >= 0) {
            nums.iter().sum()
        } else {
            -1
        };
        steps.push(json!({
            "age": -1,
            "population_count": -1,
            "villager_count": vills,
            "time": normalize_time(time),
            "resources": { "food": get(0), "wood": get(1), "gold": get(2), "stone": get(3) },
            "notes": [text],
        }));
    }
    if steps.is_empty() {
        return Err("No build steps found in the aoeivbuilds export".into());
    }
    Ok(json!({
        "name": name,
        "civilization": civ.unwrap_or("Any"),
        "author": "",
        "source": source,
        "build_order": steps,
    }))
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
        let steps = bo["build_order"].as_array().unwrap();
        assert_eq!(steps.len(), 3);
        assert_eq!(steps[0]["resources"]["food"], 6);
        assert_eq!(steps[0]["time"], "0:00");
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
    fn normalizes_times() {
        assert_eq!(normalize_time("5.30"), "5:30");
        assert_eq!(normalize_time("4:5"), "4:05");
        assert_eq!(normalize_time("04:20"), "4:20");
        assert_eq!(normalize_time("10:00"), "10:00");
        assert_eq!(normalize_time(""), "");
        assert_eq!(normalize_time("~3 min"), "~3 min");
        assert_eq!(normalize_time("1:99"), "1:99", "bad seconds left alone");
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
