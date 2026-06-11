use serde::Serialize;
use serde_json::Value;

const BASE: &str = "https://aoe4world.com/api/v0";

#[derive(Debug, Clone, Serialize)]
pub struct PlayerSearchResult {
    pub profile_id: u64,
    pub name: String,
    pub rating: Option<i64>,
}

pub async fn search_players(
    client: &reqwest::Client,
    query: &str,
) -> Result<Vec<PlayerSearchResult>, String> {
    let url = format!("{BASE}/players/search?query={}", urlencode(query));
    let data: Value = client
        .get(&url)
        .send()
        .await
        .map_err(|e| e.to_string())?
        .json()
        .await
        .map_err(|e| e.to_string())?;

    let players = data["players"].as_array().cloned().unwrap_or_default();
    Ok(players
        .iter()
        .take(20)
        .filter_map(|p| {
            Some(PlayerSearchResult {
                profile_id: p["profile_id"].as_u64()?,
                name: p["name"].as_str()?.to_string(),
                rating: p["leaderboards"]["rm_solo"]["rating"]
                    .as_i64()
                    .or_else(|| p["leaderboards"]["rm_team"]["rating"].as_i64()),
            })
        })
        .collect())
}

pub async fn get_last_game(client: &reqwest::Client, profile_id: u64) -> Result<Value, String> {
    let url = format!("{BASE}/players/{profile_id}/games/last");
    let data: Value = client
        .get(&url)
        .send()
        .await
        .map_err(|e| e.to_string())?
        .json()
        .await
        .map_err(|e| e.to_string())?;
    if data.get("error").is_some() {
        return Err(data["error"].to_string());
    }
    Ok(data)
}

/// Transform a raw /games/last response into the overlay payload.
/// Mirrors `process_game()` from the original Python app.
pub fn process_game(game: &Value, main_profile_id: u64) -> Value {
    let kind = game["kind"].as_str().unwrap_or("");
    let mode_key = resolve_mode_key(kind);
    let mode_label = if kind.starts_with("rm") { "RM" } else { "QM" };

    // Flatten teams, remembering each player's team index
    let mut players: Vec<Value> = Vec::new();
    let mut main_team: i64 = 0;
    if let Some(teams) = game["teams"].as_array() {
        for (ti, team) in teams.iter().enumerate() {
            if let Some(members) = team.as_array() {
                for member in members {
                    // members are either {player: {...}} or the player object itself
                    let p = member.get("player").unwrap_or(member);
                    if p["profile_id"].as_u64() == Some(main_profile_id) {
                        main_team = ti as i64 + 1;
                    }
                    players.push(process_player(p, ti as i64 + 1, &mode_key, mode_label));
                }
            }
        }
    }

    // Main player's team listed first (team value 1)
    if main_team > 1 {
        for p in players.iter_mut() {
            let t = p["team"].as_i64().unwrap_or(1);
            p["team"] = Value::from(if t == main_team { 1 } else { t + 1 });
        }
    }
    players.sort_by_key(|p| p["team"].as_i64().unwrap_or(99));

    serde_json::json!({
        "map": game["map"],
        "kind": kind,
        "started": game["started_at"],
        "server": game["server"],
        "match_id": game["game_id"],
        "ongoing": game["ongoing"],
        "players": players,
    })
}

fn process_player(p: &Value, team: i64, mode_key: &str, mode_label: &str) -> Value {
    let civ_raw = p["civilization"].as_str().unwrap_or("");
    let civ = title_case(&civ_raw.replace('_', " "));
    // Fall back between ranked and quick-match stats when one is missing
    let mut key = mode_key.to_string();
    let mut label = mode_label;
    if p["modes"][&key].is_null() {
        let swapped = if key.starts_with("rm_") {
            key.replacen("rm_", "qm_", 1)
        } else {
            key.replacen("qm_", "rm_", 1)
        };
        if !p["modes"][&swapped].is_null() {
            label = if swapped.starts_with("rm") { "RM" } else { "QM" };
            key = swapped;
        }
    }
    let modes = &p["modes"][&key];

    let mut civ_games = String::new();
    let mut civ_winrate = String::new();
    if let Some(civs) = modes["civilizations"].as_array() {
        if let Some(c) = civs.iter().find(|c| c["civilization"].as_str() == Some(civ_raw)) {
            civ_games = c["games_count"].as_i64().map(|v| v.to_string()).unwrap_or_default();
            civ_winrate = c["win_rate"]
                .as_f64()
                .map(|v| format!("{v:.0}%"))
                .unwrap_or_default();
        }
    }

    serde_json::json!({
        "name": p["name"],
        "civ": civ,
        "team": team,
        "country": p["country"],
        "rating": modes["rating"].as_i64().map(|v| v.to_string()).unwrap_or_else(|| "-".into()),
        "rank": modes["rank"].as_i64().map(|r| format!("{label}#{r}")).unwrap_or_default(),
        "wins": modes["wins_count"].as_i64().unwrap_or(0).to_string(),
        "losses": modes["losses_count"].as_i64().unwrap_or(0).to_string(),
        "winrate": modes["win_rate"].as_f64().map(|v| format!("{v:.0}%")).unwrap_or_default(),
        "civ_games": civ_games,
        "civ_winrate": civ_winrate,
    })
}

/// Map a game `kind` (e.g. "rm_2v2") to the key used in the player's `modes` object.
fn resolve_mode_key(kind: &str) -> String {
    match kind {
        "rm_2v2" | "rm_3v3" | "rm_4v4" => "rm_team".into(),
        k => k.into(),
    }
}

fn title_case(s: &str) -> String {
    s.split_whitespace()
        .map(|w| {
            let mut c = w.chars();
            match c.next() {
                Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            b' ' => "+".to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}
