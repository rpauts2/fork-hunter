use crate::base::{BookmakerParser, ParserResult};
use async_trait::async_trait;
use chrono::Utc;
use reqwest::Client;
use shared::odds::OddsType;
use shared::{Event, Odd, Sport};
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{info, warn};

/// Pari API parser — handles the actual API structure
/// API returns: { events: [...], customFactors: [...], ... }
/// events: [{ id, team1, team2, sportId, startTime, ... }]
/// customFactors: [{ e: event_id, factors: [{ f: factor_id, v: odds, p: line*100, pt: "line" }] }]
fn parse_u64_value(value: &serde_json::Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_str().and_then(|text| text.trim().parse().ok()))
}

fn parse_f64_value(value: &serde_json::Value) -> Option<f64> {
    value.as_f64().or_else(|| {
        value
            .as_str()
            .and_then(|text| text.trim().replace(',', ".").parse().ok())
    })
}

fn id_label(value: &str) -> String {
    let normalized = value
        .trim()
        .to_lowercase()
        .chars()
        .map(|ch| if ch.is_alphanumeric() { ch } else { '_' })
        .collect::<String>();
    normalized
        .split('_')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("_")
}

fn line_id_component(line: Option<f64>) -> String {
    line.map(|value| format!("{value:.6}").replace('-', "m").replace('.', "p"))
        .unwrap_or_else(|| "no-line".to_string())
}

#[derive(Debug)]
pub struct PariParser {
    client: Arc<Client>,
    live_url: String,
    prematch_url: String,
}

impl PariParser {
    pub fn new(client: Arc<Client>) -> Self {
        Self {
            client,
            live_url:
                "https://line-lb51-w.pb06e2-resources.ru/events/list?lang=ru&scopeMarket=2300"
                    .to_string(),
            prematch_url:
                "https://line-lb51-w.pb06e2-resources.ru/events/listBase?lang=ru&scopeMarket=2300"
                    .to_string(),
        }
    }
}

#[async_trait]
impl BookmakerParser for PariParser {
    fn name(&self) -> &str {
        "Pari"
    }
    fn slug(&self) -> &str {
        "pari"
    }
    fn is_enabled(&self) -> bool {
        true
    }

    async fn fetch_events(&self) -> Result<Vec<Event>, Box<dyn std::error::Error + Send + Sync>> {
        let mut all_events = Vec::new();
        let mut errors = Vec::new();
        for (url, is_live) in [(&self.live_url, true), (&self.prematch_url, false)] {
            match self.fetch_api(url, is_live).await {
                Ok((events, _)) => all_events.extend(events),
                Err(error) => {
                    warn!(error = %error, "Pari fetch events failed");
                    errors.push(format!("{url}: {error}"));
                }
            }
        }
        if !errors.is_empty() {
            return Err(std::io::Error::other(errors.join("; ")).into());
        }
        info!(count = all_events.len(), "Pari events parsed");
        Ok(all_events)
    }

    async fn fetch_odds(
        &self,
        _event_id: &str,
    ) -> Result<Vec<Odd>, Box<dyn std::error::Error + Send + Sync>> {
        let mut all_odds = Vec::new();
        let mut errors = Vec::new();
        for (url, is_live) in [(&self.live_url, true), (&self.prematch_url, false)] {
            match self.fetch_api(url, is_live).await {
                Ok((_, odds)) => all_odds.extend(odds),
                Err(error) => {
                    warn!(error = %error, "Pari fetch odds failed");
                    errors.push(format!("{url}: {error}"));
                }
            }
        }
        if !errors.is_empty() {
            return Err(std::io::Error::other(errors.join("; ")).into());
        }
        Ok(all_odds)
    }

    async fn fetch_all(&self) -> Result<ParserResult, Box<dyn std::error::Error + Send + Sync>> {
        let start = std::time::Instant::now();
        let mut all_events = Vec::new();
        let mut all_odds = Vec::new();
        let mut errors = Vec::new();

        for (url, is_live) in [(&self.live_url, true), (&self.prematch_url, false)] {
            match self.fetch_api(url, is_live).await {
                Ok((events, odds)) => {
                    all_events.extend(events);
                    all_odds.extend(odds);
                }
                Err(error) => {
                    warn!(error = %error, "Pari fetch failed");
                    errors.push(format!("{url}: {error}"));
                }
            }
        }

        if !errors.is_empty() {
            return Err(std::io::Error::other(errors.join("; ")).into());
        }

        let elapsed = start.elapsed().as_millis() as u64;
        info!(
            events = all_events.len(),
            odds = all_odds.len(),
            failed_feeds = errors.len(),
            time_ms = elapsed,
            "Pari fetch complete"
        );
        Ok(ParserResult::new("pari", all_events, all_odds, elapsed))
    }

    fn base_url(&self) -> &str {
        "https://pari.ru"
    }
    fn user_agent(&self) -> &str {
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36"
    }
}

impl PariParser {
    async fn fetch_api(
        &self,
        url: &str,
        is_live: bool,
    ) -> Result<(Vec<Event>, Vec<Odd>), Box<dyn std::error::Error + Send + Sync>> {
        let resp = self.client.get(url)
            .header("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36")
            .header("Accept", "application/json, text/plain, */*")
            .header("Accept-Language", "ru-RU,ru;q=0.9")
            .header("Origin", "https://pari.ru")
            .header("Referer", "https://pari.ru/sports")
            // Do not set Accept-Encoding manually: reqwest's configured decoder
            // must negotiate and transparently decompress the API response.
            .send()
            .await?;

        if !resp.status().is_success() {
            return Err(
                std::io::Error::other(format!("Pari API returned HTTP {}", resp.status())).into(),
            );
        }

        let json: serde_json::Value = resp.json().await?;
        let result = Self::parse_api_response(&json, is_live);
        result
    }

    fn parse_api_response(
        json: &serde_json::Value,
        is_live: bool,
    ) -> Result<(Vec<Event>, Vec<Odd>), Box<dyn std::error::Error + Send + Sync>> {
        // Parse events
        let events_data = match json.get("events").and_then(|e| e.as_array()) {
            Some(events) => events,
            None => {
                return Err(
                    std::io::Error::other("Pari API response is missing the events array").into(),
                );
            }
        };

        // Resolve tournament/segment IDs through the source sports hierarchy.
        // In the live feed, events often have an empty `name` and `sportId` points
        // to a tournament segment rather than the root sport ID.
        let sports = json
            .get("sports")
            .and_then(|value| value.as_array())
            .map(Vec::as_slice)
            .unwrap_or_default();

        // Build event lookup: id -> (sport, league, team1, team2, start_time)
        let mut event_map: HashMap<u64, (Sport, String, String, String, Option<i64>)> =
            HashMap::new();
        for event_data in events_data {
            if !crate::marathon::event_place_matches_feed(event_data, is_live) {
                continue;
            }
            if let (Some(id), Some(team1), Some(team2)) = (
                event_data.get("id").and_then(|v| v.as_u64()),
                event_data.get("team1").and_then(|v| v.as_str()),
                event_data.get("team2").and_then(|v| v.as_str()),
            ) {
                let sport_id = event_data
                    .get("sportId")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let start_time = event_data.get("startTime").and_then(|v| v.as_i64());
                let event_name = event_data
                    .get("name")
                    .and_then(|value| value.as_str())
                    .unwrap_or_default()
                    .to_string();
                let (sport, league) = Self::source_sport_and_league(sports, sport_id, &event_name);
                event_map.insert(
                    id,
                    (
                        sport,
                        league,
                        team1.to_string(),
                        team2.to_string(),
                        start_time,
                    ),
                );
            }
        }

        // Parse customFactors into odds. A missing collection signals schema drift.
        if json
            .get("customFactors")
            .and_then(|value| value.as_array())
            .is_none()
        {
            return Err(std::io::Error::other(
                "Pari API response is missing the customFactors array",
            )
            .into());
        }
        let mut all_odds = Vec::new();
        let mut event_indices: HashMap<u64, usize> = HashMap::new();
        let now = Utc::now();

        if let Some(custom_factors) = json.get("customFactors").and_then(|f| f.as_array()) {
            for cf in custom_factors {
                if let (Some(event_id), Some(factors)) = (
                    cf.get("e").and_then(|v| v.as_u64()),
                    cf.get("factors").and_then(|f| f.as_array()),
                ) {
                    if let Some((sport, league, team1, team2, start_time)) =
                        event_map.get(&event_id)
                    {
                        // Create event if not seen
                        if !event_indices.contains_key(&event_id) {
                            let start_dt = start_time.and_then(|timestamp| {
                                chrono::DateTime::from_timestamp(timestamp, 0)
                            });

                            all_odds.push((
                                Event {
                                    id: format!(
                                        "pari-{}-{}",
                                        if is_live { "live" } else { "pre" },
                                        event_id
                                    ),
                                    sport: *sport,
                                    league: league.clone(),
                                    home_team: team1.clone(),
                                    away_team: team2.clone(),
                                    start_time: start_dt,
                                    is_live,
                                    bookmaker_slug: "pari".to_string(),
                                    raw_url: None,
                                    extra: HashMap::new(),
                                },
                                Vec::new(),
                            ));
                            event_indices.insert(event_id, all_odds.len() - 1);
                        }

                        // Parse factors into odds
                        let event_idx = event_indices.get(&event_id).copied();
                        if let Some(idx) = event_idx {
                            for factor in factors {
                                if let (Some(fid), Some(val)) = (
                                    factor.get("f").and_then(parse_u64_value),
                                    factor.get("v").and_then(parse_f64_value),
                                ) {
                                    if val <= 1.0 {
                                        continue;
                                    }

                                    let line =
                                        factor.get("p").and_then(|v| v.as_f64()).map(|p| p / 100.0);
                                    let (market, selection, odds_type) =
                                        Self::factor_to_market(fid);

                                    let (_, odds_vec) = &mut all_odds[idx];
                                    odds_vec.push(Odd {
                                        id: format!(
                                            "pari-{}-{}-{}-{}-{}-{}",
                                            if is_live { "live" } else { "pre" },
                                            event_id,
                                            fid,
                                            id_label(&market),
                                            id_label(&selection),
                                            line_id_component(line)
                                        ),
                                        event_id: format!(
                                            "pari-{}-{}",
                                            if is_live { "live" } else { "pre" },
                                            event_id
                                        ),
                                        bookmaker_slug: "pari".to_string(),
                                        market,
                                        selection,
                                        odds: val,
                                        odds_type,
                                        line,
                                        timestamp: now,
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }

        let events = all_odds.iter().map(|(e, _)| e.clone()).collect();
        let odds = all_odds.into_iter().flat_map(|(_, o)| o).collect();
        Ok((events, odds))
    }

    fn factor_to_market(fid: u64) -> (String, String, OddsType) {
        match fid {
            // 1X2
            921 => ("1X2".into(), "1".into(), OddsType::Home),
            922 => ("1X2".into(), "X".into(), OddsType::Draw),
            923 => ("1X2".into(), "2".into(), OddsType::Away),
            // Totals
            930 => ("Total".into(), "Over".into(), OddsType::Over),
            931 => ("Total".into(), "Under".into(), OddsType::Under),
            924 | 1002 | 1010 | 1054 => ("Total".into(), "Over".into(), OddsType::Over),
            925 | 1003 | 1011 | 1055 => ("Total".into(), "Under".into(), OddsType::Under),
            // Handicaps
            910 | 912 | 1004 | 1006 | 1012 => ("Handicap".into(), "1".into(), OddsType::Handicap),
            1005 | 1013 => ("Handicap".into(), "2".into(), OddsType::Handicap),
            // BTTS (Both Teams Score)
            926 => ("BTTS".into(), "Yes".into(), OddsType::BothTeamsScoreYes),
            927 => ("BTTS".into(), "No".into(), OddsType::BothTeamsScoreNo),
            // Even/Odd
            928 => ("EvenOdd".into(), "Even".into(), OddsType::Even),
            929 => ("EvenOdd".into(), "Odd".into(), OddsType::Odd),
            // Double Chance
            1014 => ("DoubleChance".into(), "1X".into(), OddsType::Custom),
            1015 => ("DoubleChance".into(), "12".into(), OddsType::Custom),
            1016 => ("DoubleChance".into(), "X2".into(), OddsType::Custom),
            // Individual Totals (ИТБ/ИТМ)
            1020 | 1022 | 1024 => ("IndividualTotal".into(), "Over".into(), OddsType::Over),
            1021 | 1023 | 1025 => ("IndividualTotal".into(), "Under".into(), OddsType::Under),
            // 1H/2H Results
            1030 => ("1H_Result".into(), "1".into(), OddsType::Home),
            1031 => ("1H_Result".into(), "X".into(), OddsType::Draw),
            1032 => ("1H_Result".into(), "2".into(), OddsType::Away),
            1033 => ("2H_Result".into(), "1".into(), OddsType::Home),
            1034 => ("2H_Result".into(), "X".into(), OddsType::Draw),
            1035 => ("2H_Result".into(), "2".into(), OddsType::Away),
            // Correct Score (partial)
            1040..=1050 => (
                "CorrectScore".into(),
                format!("score_{}", fid),
                OddsType::Custom,
            ),
            // Fallback
            _ => (
                format!("factor_{}", fid),
                format!("{}", fid),
                OddsType::Custom,
            ),
        }
    }

    fn source_sport_and_league(
        sports: &[serde_json::Value],
        sport_id: u64,
        event_name: &str,
    ) -> (Sport, String) {
        let by_id = sports
            .iter()
            .filter_map(|item| Some((parse_u64_value(item.get("id")?)?, item)))
            .collect::<HashMap<_, _>>();

        let mut current = by_id.get(&sport_id).copied();
        let mut visited = std::collections::HashSet::new();
        let mut league = String::new();
        let mut resolved_sport = None;

        for _ in 0..8 {
            let Some(item) = current else { break };
            let Some(id) = item.get("id").and_then(parse_u64_value) else {
                break;
            };
            if !visited.insert(id) {
                break;
            }

            let kind = item
                .get("kind")
                .and_then(|value| value.as_str())
                .unwrap_or_default();
            let name = item
                .get("name")
                .and_then(|value| value.as_str())
                .unwrap_or_default();
            if kind == "segment" && league.is_empty() {
                league = name.trim().to_string();
            }
            if kind == "sport" {
                resolved_sport = Some(Sport::from_str(name));
                break;
            }

            current = item
                .get("parentId")
                .and_then(parse_u64_value)
                .and_then(|parent_id| by_id.get(&parent_id).copied());
        }

        let sport = resolved_sport
            .filter(|sport| *sport != Sport::Other)
            .unwrap_or_else(|| Self::sport_id_to_sport(sport_id, event_name));
        let event_league = crate::marathon::extract_league(event_name);
        if !event_league.trim().is_empty() {
            league = event_league;
        }

        (sport, league)
    }

    fn sport_id_to_sport(id: u64, event_name: &str) -> Sport {
        // Prefer the bookmaker's own sport label when it is recognizable.
        // Numeric IDs can overlap across provider API generations.
        let sport_label = event_name
            .split_once(':')
            .map(|(sport, _)| sport.trim())
            .unwrap_or(event_name.trim());
        let by_name = Sport::from_str(sport_label);
        if by_name != Sport::Other {
            return by_name;
        }

        match id {
            15870 | 15869 | 15871 => Sport::Football,
            15872 | 15873 => Sport::Basketball,
            15874 | 15875 => Sport::Hockey,
            15876 | 15877 => Sport::Tennis,
            15878 | 15879 => Sport::Volleyball,
            _ => Sport::Other,
        }
    }
}

#[cfg(test)]
mod parser_fixture_tests {
    use super::PariParser;
    use shared::odds::OddsType;
    use shared::Sport;

    #[test]
    fn factor_mapping_uses_semantic_btts_and_even_odd_types() {
        assert_eq!(
            PariParser::factor_to_market(926),
            ("BTTS".into(), "Yes".into(), OddsType::BothTeamsScoreYes)
        );
        assert_eq!(
            PariParser::factor_to_market(927),
            ("BTTS".into(), "No".into(), OddsType::BothTeamsScoreNo)
        );
        assert_eq!(
            PariParser::factor_to_market(928),
            ("EvenOdd".into(), "Even".into(), OddsType::Even)
        );
        assert_eq!(
            PariParser::factor_to_market(929),
            ("EvenOdd".into(), "Odd".into(), OddsType::Odd)
        );
    }

    #[test]
    fn recognized_source_sport_label_overrides_legacy_numeric_id() {
        assert_eq!(
            PariParser::sport_id_to_sport(4, "Теннис: ATP"),
            Sport::Tennis
        );
        assert_eq!(
            PariParser::sport_id_to_sport(2, "Хоккей: КХЛ"),
            Sport::Hockey
        );
    }

    fn sample_payload() -> serde_json::Value {
        serde_json::json!({
            "events": [{
                "id": 12345,
                "team1": "Home FC",
                "team2": "Away FC",
                "place": "line",
                "sportId": 1,
                "startTime": 1795618800_i64,
                "name": "Football: Premier League"
            }],
            "customFactors": [{
                "e": 12345,
                "factors": [
                    {"f": 921, "v": 2.10},
                    {"f": 930, "v": 1.80, "p": 150},
                    {"f": 930, "v": 2.00, "p": 250}
                ]
            }]
        })
    }

    #[test]
    fn parses_event_and_multiple_total_lines_without_id_collisions() {
        let payload = sample_payload();
        let (events, odds) =
            PariParser::parse_api_response(&payload, false).expect("valid fixture");

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].id, "pari-pre-12345");
        assert_eq!(events[0].home_team, "Home FC");
        assert_eq!(events[0].away_team, "Away FC");
        assert_eq!(events[0].sport, Sport::Football);
        assert_eq!(events[0].league, "Premier League");
        assert_eq!(odds.len(), 3);
        assert_eq!(odds.iter().filter(|odd| odd.market == "Total").count(), 2);
        assert!(odds.iter().any(|odd| odd.line == Some(1.5)));
        assert!(odds.iter().any(|odd| odd.line == Some(2.5)));
        assert!(odds.iter().all(|odd| odd.event_id == events[0].id));
        assert!(odds.iter().all(|odd| odd.bookmaker_slug == "pari"));
        let unique_ids = odds
            .iter()
            .map(|odd| &odd.id)
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(unique_ids.len(), odds.len());
    }

    #[test]
    fn filters_combined_catalogue_by_authoritative_place_marker() {
        let mut payload = sample_payload();
        payload["events"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({
                "id": 12346,
                "team1": "Live Home FC",
                "team2": "Live Away FC",
                "place": "live",
                "sportId": 1,
                "startTime": 1795618800_i64,
                "name": "Football: Live League"
            }));
        payload["customFactors"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({
                "e": 12346,
                "factors": [{"f": 921, "v": 2.2}]
            }));

        let (live_events, _) = PariParser::parse_api_response(&payload, true)
            .expect("combined catalogue should parse live rows");
        let (prematch_events, _) = PariParser::parse_api_response(&payload, false)
            .expect("combined catalogue should parse prematch rows");

        assert_eq!(live_events.len(), 1);
        assert_eq!(live_events[0].id, "pari-live-12346");
        assert!(live_events[0].is_live);
        assert_eq!(prematch_events.len(), 1);
        assert!(prematch_events[0].id.ends_with("12345"));
        assert!(!prematch_events[0].is_live);
    }

    #[test]
    fn resolves_pari_sport_and_league_from_source_segment_hierarchy() {
        let payload = serde_json::json!({
            "sports": [
                {"id": 11634, "kind": "sport", "name": "Крикет", "alias": "cricket"},
                {"id": 99307, "kind": "segment", "parentId": 11634, "name": "One-Day. Зимбабве. Чемпионат Про50"}
            ],
            "events": [{
                "id": 70230668,
                "sportId": 99307,
                "team1": "Матабелленд Таскерс",
                "team2": "Саутерн Рокс",
                "name": "",
                "place": "live"
            }],
            "customFactors": [{"e": 70230668, "factors": []}]
        });

        let (events, odds) = PariParser::parse_api_response(&payload, true).expect("parse");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].sport, Sport::Cricket);
        assert_eq!(events[0].league, "One-Day. Зимбабве. Чемпионат Про50");
        assert!(
            odds.is_empty(),
            "an empty source factor list must not fabricate odds"
        );
    }

    #[test]
    fn unknown_sport_id_uses_source_sport_name_not_football_fallback() {
        let mut payload = sample_payload();
        payload["events"][0]["sportId"] = serde_json::json!(999_999);
        payload["events"][0]["name"] = serde_json::json!("Tennis: ATP Tour");
        let (events, _) =
            PariParser::parse_api_response(&payload, false).expect("valid event payload");
        assert_eq!(events[0].sport, Sport::Tennis);
        assert_eq!(events[0].league, "ATP Tour");

        payload["events"][0]["name"] = serde_json::json!("Unmapped: Competition");
        let (events, _) = PariParser::parse_api_response(&payload, false)
            .expect("unknown sport is still a valid event");
        assert_eq!(events[0].sport, Sport::Other);
    }

    #[test]
    fn rejects_payload_without_required_arrays() {
        let missing_events =
            PariParser::parse_api_response(&serde_json::json!({"customFactors": []}), false)
                .expect_err("missing events must be a schema error");
        assert!(missing_events.to_string().contains("events array"));
        let missing_factors =
            PariParser::parse_api_response(&serde_json::json!({"events": []}), false)
                .expect_err("missing factors must be a schema error");
        assert!(missing_factors.to_string().contains("customFactors array"));
    }

    #[test]
    fn rejects_payload_without_events_array() {
        let error =
            PariParser::parse_api_response(&serde_json::json!({"customFactors": []}), false)
                .expect_err("missing events must be a schema error");
        assert!(error.to_string().contains("events array"));
    }
}
