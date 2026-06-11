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
    // A numeric query may be a profile ID — try a direct lookup first
    if let Ok(id) = query.parse::<u64>() {
        if let Ok(resp) = client.get(format!("{BASE}/players/{id}")).send().await {
            if let Ok(p) = resp.json::<Value>().await {
                if let (Some(profile_id), Some(name)) =
                    (p["profile_id"].as_u64(), p["name"].as_str())
                {
                    return Ok(vec![PlayerSearchResult {
                        profile_id,
                        name: name.to_string(),
                        rating: p["leaderboards"]["rm_solo"]["rating"]
                            .as_i64()
                            .or_else(|| p["modes"]["rm_1v1"]["rating"].as_i64()),
                    }]);
                }
            }
        }
    }
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

pub async fn get_match_history(
    client: &reqwest::Client,
    profile_id: u64,
    limit: u32,
) -> Result<Value, String> {
    let url = format!("{BASE}/players/{profile_id}/games?limit={limit}");
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
        "duration": game["duration"],
        "season": game["season"],
        "patch": game["patch"],
        "avg_rating": game["average_rating"],
        "avg_mmr": game["average_mmr"],
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
    let mut civ_win_median = String::new();
    if let Some(civs) = modes["civilizations"].as_array() {
        if let Some(c) = civs.iter().find(|c| c["civilization"].as_str() == Some(civ_raw)) {
            civ_games = c["games_count"].as_i64().map(|v| v.to_string()).unwrap_or_default();
            civ_winrate = c["win_rate"]
                .as_f64()
                .map(|v| format!("{v:.0}%"))
                .unwrap_or_default();
            civ_win_median = c["game_length"]["wins_median"]
                .as_f64()
                .map(|s| format!("{}:{:02}", (s as i64) / 60, (s as i64) % 60))
                .unwrap_or_default();
        }
    }

    // Rating history (oldest -> newest) for a per-player sparkline
    let mut history: Vec<(u64, i64)> = modes["rating_history"]
        .as_object()
        .map(|h| {
            h.iter()
                .filter_map(|(ts, v)| Some((ts.parse::<u64>().ok()?, v["rating"].as_i64()?)))
                .collect()
        })
        .unwrap_or_default();
    history.sort_by_key(|(ts, _)| *ts);
    let skip = history.len().saturating_sub(24);
    let rating_history: Vec<i64> = history.into_iter().skip(skip).map(|(_, r)| r).collect();

    serde_json::json!({
        "name": p["name"],
        "civ": civ,
        "team": team,
        "country": p["country"],
        "profile_id": p["profile_id"],
        "result": p["result"],
        "rating_diff": p["rating_diff"],
        "input_type": p["input_type"],
        "avatar": p["avatars"]["medium"],
        "random_civ": p["civilization_randomized"].as_bool().unwrap_or(false),
        "twitch": p["social"]["twitch"],
        "rank_level": modes["rank_level"],
        "streak": modes["streak"],
        "max_rating": modes["max_rating"],
        "games_count": modes["games_count"],
        "rating_history": rating_history,
        "rating": modes["rating"].as_i64().map(|v| v.to_string()).unwrap_or_else(|| "-".into()),
        "rank": modes["rank"].as_i64().map(|r| format!("{label}#{r}")).unwrap_or_default(),
        "wins": modes["wins_count"].as_i64().unwrap_or(0).to_string(),
        "losses": modes["losses_count"].as_i64().unwrap_or(0).to_string(),
        "winrate": modes["win_rate"].as_f64().map(|v| format!("{v:.0}%")).unwrap_or_default(),
        "civ_games": civ_games,
        "civ_winrate": civ_winrate,
        "civ_win_length_median": civ_win_median,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_game() -> Value {
        serde_json::json!({
            "game_id": 123,
            "kind": "rm_1v1",
            "map": "Dry Arabia",
            "server": "eu",
            "ongoing": false,
            "started_at": "2026-06-11T10:00:00.000Z",
            "duration": 1930,
            "season": 13,
            "average_rating": 1350,
            "teams": [
                [ { "player": {
                    "profile_id": 1, "name": "Opponent", "country": "de",
                    "civilization": "holy_roman_empire",
                    "result": "win", "rating_diff": 12, "input_type": "keyboard",
                    "avatars": { "medium": "https://example.com/a_medium.jpg" },
                    "modes": { "rm_1v1": {
                        "rating": 1500, "rank": 100, "wins_count": 60, "losses_count": 40,
                        "win_rate": 60.0, "rank_level": "conqueror_2", "streak": 4,
                        "max_rating": 1550, "games_count": 100,
                        "rating_history": {
                            "1780500845": { "rating": 1480 },
                            "1780498882": { "rating": 1470 },
                            "1780579715": { "rating": 1500 }
                        },
                        "civilizations": [
                            { "civilization": "holy_roman_empire", "games_count": 25, "win_rate": 64.0,
                              "game_length": { "wins_median": 1130.5 } }
                        ]
                    }}
                }} ],
                [ { "player": {
                    "profile_id": 2, "name": "Me", "country": "gb",
                    "civilization": "english",
                    // Only quick-match stats — exercises the rm->qm fallback
                    "modes": { "qm_1v1": {
                        "rating": 1200, "rank": 500, "wins_count": 10, "losses_count": 10,
                        "win_rate": 50.0
                    }}
                }} ]
            ]
        })
    }

    #[test]
    fn process_game_reorders_main_player_team_first() {
        let out = process_game(&fixture_game(), 2);
        let players = out["players"].as_array().unwrap();
        assert_eq!(players.len(), 2);
        assert_eq!(players[0]["name"], "Me", "main player's team sorts first");
        assert_eq!(players[0]["team"], 1);
        assert_eq!(players[1]["name"], "Opponent");
        assert_eq!(players[1]["team"], 2);
        assert_eq!(out["map"], "Dry Arabia");
        assert_eq!(out["match_id"], 123);
    }

    #[test]
    fn process_game_extracts_mode_and_civ_stats() {
        let out = process_game(&fixture_game(), 2);
        let opp = &out["players"][1];
        assert_eq!(opp["civ"], "Holy Roman Empire");
        assert_eq!(opp["rating"], "1500");
        assert_eq!(opp["rank"], "RM#100");
        assert_eq!(opp["winrate"], "60%");
        assert_eq!(opp["wins"], "60");
        assert_eq!(opp["losses"], "40");
        assert_eq!(opp["civ_games"], "25");
        assert_eq!(opp["civ_winrate"], "64%");
        assert_eq!(opp["civ_win_length_median"], "18:50");
    }

    #[test]
    fn process_game_passes_through_enriched_fields() {
        let out = process_game(&fixture_game(), 2);
        assert_eq!(out["duration"], 1930);
        assert_eq!(out["season"], 13);
        assert_eq!(out["avg_rating"], 1350);
        let opp = &out["players"][1];
        assert_eq!(opp["result"], "win");
        assert_eq!(opp["rating_diff"], 12);
        assert_eq!(opp["rank_level"], "conqueror_2");
        assert_eq!(opp["streak"], 4);
        assert_eq!(opp["max_rating"], 1550);
        assert_eq!(opp["avatar"], "https://example.com/a_medium.jpg");
        // history sorted by timestamp, oldest first
        assert_eq!(opp["rating_history"], serde_json::json!([1470, 1480, 1500]));
        // absent on the other player -> null / empty, not a crash
        let me = &out["players"][0];
        assert!(me["rank_level"].is_null());
        assert_eq!(me["rating_history"].as_array().unwrap().len(), 0);
        assert_eq!(me["random_civ"], false);
    }

    #[test]
    fn process_game_falls_back_rm_to_qm_stats() {
        let out = process_game(&fixture_game(), 2);
        let me = &out["players"][0];
        assert_eq!(me["rating"], "1200");
        assert_eq!(me["rank"], "QM#500", "fell back to quick-match stats and label");
    }

    #[test]
    fn process_game_handles_missing_stats() {
        let game = serde_json::json!({
            "kind": "custom", "map": "Test", "ongoing": true,
            "teams": [[ { "player": { "profile_id": 9, "name": "X", "civilization": "rus", "modes": {} } } ]]
        });
        let p = &process_game(&game, 9)["players"][0];
        assert_eq!(p["rating"], "-");
        assert_eq!(p["rank"], "");
        assert_eq!(p["civ"], "Rus");
    }

    #[test]
    fn team_mode_kinds_map_to_rm_team() {
        assert_eq!(resolve_mode_key("rm_3v3"), "rm_team");
        assert_eq!(resolve_mode_key("rm_1v1"), "rm_1v1");
        assert_eq!(resolve_mode_key("qm_4v4"), "qm_4v4");
    }

    #[test]
    fn urlencode_escapes_specials() {
        assert_eq!(urlencode("a b+ü"), "a+b%2B%C3%BC");
    }

    #[tokio::test]
    async fn live_search_and_process() {
        let client = reqwest::Client::new();
        let players = search_players(&client, "Beastyqt").await.expect("search");
        assert!(!players.is_empty(), "no players found");
        let pid = players[0].profile_id;
        let game = get_last_game(&client, pid).await.expect("last game");
        let processed = process_game(&game, pid);
        println!("{}", serde_json::to_string_pretty(&processed).unwrap());
        let ps = processed["players"].as_array().unwrap();
        assert!(!ps.is_empty());
        assert_eq!(ps[0]["team"], 1);
        assert!(ps.iter().any(|p| p["rating"] != "-"));
    }
}
