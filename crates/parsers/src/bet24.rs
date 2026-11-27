use crate::base::{BookmakerParser, ParserResult};
use crate::factors_catalog::FactorsCatalog;
use async_trait::async_trait;
use chrono::Utc;
use reqwest::Client;
use shared::odds::OddsType;
use shared::{Event, Odd, Sport};
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{debug, info, warn};

/// 24bet API parser — shared platform (same as MarathonBet)
/// API: line51.tf39be-resources.com/events/list?scopeMarket=3000
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

#[derive(Debug)]
pub struct _24betParser {
    factors: Arc<FactorsCatalog>,
    client: Arc<Client>,
    live_url: String,
    prematch_url: String,
}

impl _24betParser {
    pub fn new(client: Arc<Client>) -> Self {
        Self {
            factors: Arc::new(FactorsCatalog::new(
                client.clone(),
                "https://line51.tf39be-resources.com",
                3000,
            )),
            client,
            live_url: "https://line51.tf39be-resources.com/events/list?lang=ru&scopeMarket=3000"
                .to_string(),
            prematch_url:
                "https://line51.tf39be-resources.com/events/listBase?lang=ru&scopeMarket=3000"
                    .to_string(),
        }
    }

    fn detect_sport(name: &str) -> Sport {
        let lower = name.to_lowercase();
        if lower.contains("теннис") || lower.contains("tennis") {
            Sport::Tennis
        } else if lower.contains("хоккей") || lower.contains("hockey") {
            Sport::Hockey
        } else if lower.contains("баскет") || lower.contains("basket") {
            Sport::Basketball
        } else if lower.contains("волей") || lower.contains("volley") {
            Sport::Volleyball
        } else if lower.contains("бейсбол") || lower.contains("baseball") {
            Sport::Baseball
        } else if lower.contains("гандбол") || lower.contains("handball") {
            Sport::Handball
        } else if lower.contains("футбол") || lower.contains("football") || lower.contains("soccer")
        {
            Sport::Football
        } else if lower.contains("регби") || lower.contains("rugby") {
            Sport::Rugby
        } else if lower.contains("бадминтон") || lower.contains("badminton") {
            Sport::Badminton
        } else if lower.contains("настольн") || lower.contains("table") {
            Sport::TableTennis
        } else {
            Sport::Other
        }
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

    async fn fetch_api(
        &self,
        url: &str,
        is_live: bool,
    ) -> Result<(Vec<Event>, Vec<Odd>), Box<dyn std::error::Error + Send + Sync>> {
        debug!(url = url, "24bet: fetching");

        let resp = self
            .client
            .get(url)
            .timeout(std::time::Duration::from_secs(30))
            .header("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36")
            .header("Accept", "application/json, text/plain, */*")
            .header("Accept-Language", "ru-RU,ru;q=0.9")
            .header("Referer", "https://24bet.ru/")
            .header("Origin", "https://24bet.ru")
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body_preview = resp
                .text()
                .await
                .unwrap_or_default()
                .chars()
                .take(240)
                .collect::<String>();
            return Err(std::io::Error::other(format!(
                "24bet API returned HTTP {status}: {body_preview}"
            ))
            .into());
        }

        match self.factors.load().await {
            Ok(count) => debug!(factor_count = count, "24bet direct factor catalog loaded"),
            Err(error) => warn!(error = %error, "24bet direct factor catalog unavailable; using legacy fallback mappings for factors absent from the direct catalog"),
        }

        let json: serde_json::Value = resp.json().await?;
        Self::parse_api_response_with_catalog(&json, is_live, Some(&self.factors))
    }

    fn parse_api_response(
        json: &serde_json::Value,
        is_live: bool,
    ) -> Result<(Vec<Event>, Vec<Odd>), Box<dyn std::error::Error + Send + Sync>> {
        Self::parse_api_response_with_catalog(json, is_live, None)
    }

    fn parse_api_response_with_catalog(
        json: &serde_json::Value,
        is_live: bool,
        factor_catalog: Option<&FactorsCatalog>,
    ) -> Result<(Vec<Event>, Vec<Odd>), Box<dyn std::error::Error + Send + Sync>> {
        let events_data = match json.get("events").and_then(|e| e.as_array()) {
            Some(e) => e,
            None => {
                return Err(std::io::Error::other(
                    "24bet API response is missing the events array",
                )
                .into());
            }
        };
        if json
            .get("customFactors")
            .and_then(|value| value.as_array())
            .is_none()
        {
            return Err(std::io::Error::other(
                "24bet API response is missing the customFactors array",
            )
            .into());
        }

        // Build event lookup: id -> (team1, team2, name, sport)
        let mut event_map: HashMap<u64, (String, String, String, Option<i64>, Option<u64>)> = HashMap::new();
        for event_data in events_data {
            if !crate::marathon::event_place_matches_feed(event_data, is_live) {
                continue;
            }
            if let (Some(id), Some(team1), Some(team2)) = (
                event_data.get("id").and_then(parse_u64_value),
                event_data.get("team1").and_then(|v| v.as_str()),
                event_data.get("team2").and_then(|v| v.as_str()),
            ) {
                let name = event_data
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let start_time = event_data.get("startTime").and_then(|value| value.as_i64());
                let source_sport_id = event_data.get("sportId").and_then(parse_u64_value);
                event_map.insert(id, (team1.to_string(), team2.to_string(), name, start_time, source_sport_id));
            }
        }

        let mut all_odds = Vec::new();
        let mut event_indices: HashMap<u64, usize> = HashMap::new();
        let now = Utc::now();

        if let Some(custom_factors) = json.get("customFactors").and_then(|f| f.as_array()) {
            for cf in custom_factors {
                if let (Some(event_id), Some(factors)) = (
                    cf.get("e").and_then(parse_u64_value),
                    cf.get("factors").and_then(|f| f.as_array()),
                ) {
                    if let Some((team1, team2, league_name, start_time, source_sport_id)) = event_map.get(&event_id)
                    {
                        if !event_indices.contains_key(&event_id) {
                            let sport = Self::detect_sport(league_name);

                            all_odds.push((
                                Event {
                                    id: format!(
                                        "_24bet-{}-{}",
                                        if is_live { "live" } else { "pre" },
                                        event_id
                                    ),
                                    sport,
                                    league: crate::marathon::extract_league(league_name),
                                    home_team: team1.clone(),
                                    away_team: team2.clone(),
                                    start_time: start_time.and_then(|timestamp| {
                                        chrono::DateTime::from_timestamp(timestamp, 0)
                                    }),
                                    is_live,
                                    bookmaker_slug: "_24bet".to_string(),
                                    raw_url: None,
                                    extra: source_sport_id
                                        .map(|id| HashMap::from([(
                                            "source_sport_id".to_string(),
                                            serde_json::Value::String(id.to_string()),
                                        )]))
                                        .unwrap_or_default(),
                                },
                                Vec::new(),
                            ));
                            event_indices.insert(event_id, all_odds.len() - 1);
                        }

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
                                        factor.get("p").and_then(parse_f64_value).map(|p| p / 100.0);

                                                                        if let Some(catalog) = factor_catalog {
                                        let source_sport_id =
                                            source_sport_id.map(|id| id.to_string());
                                        if catalog.contains_factor_id(fid) {
                                            if let Some(definition) = catalog.get_factor_for_sport(
                                                fid,
                                                source_sport_id.as_deref(),
                                            ) {
                                                let market_type =
                                                    definition.market_type.to_lowercase();
                                                let selection_name =
                                                    definition.selection_name.to_lowercase();
                                                let catalog_odd =
                                                    crate::marathon::classify_catalog_factor(
                                                        "_24bet",
                                                        &all_odds[idx].0,
                                                        fid,
                                                        val,
                                                        line,
                                                        &market_type,
                                                        &selection_name,
                                                        now,
                                                    );
                                                if let Some(odd) = catalog_odd {
                                                    all_odds[idx].1.push(odd);
                                                }
                                            }
                                            continue;
                                        }
                                    }

                                    let (market, selection, odds_type) =
                                        Self::factor_to_market(fid);
                                    let odd = crate::marathon::make_market_odd(
                                        "_24bet",
                                        &all_odds[idx].0,
                                        &market,
                                        &selection,
                                        val,
                                        line,
                                        odds_type,
                                        now,
                                    );
                                    all_odds[idx].1.push(odd);
                                }
                            }
                        }
                    }
                }
            }
        }

        let events = all_odds.iter().map(|(event, _)| event.clone()).collect();
        let odds = all_odds.into_iter().flat_map(|(_, odds)| odds).collect();
        let (odds, collapsed_duplicates) =
            crate::marathon::deduplicate_same_market_quotes(odds);
        if collapsed_duplicates > 0 {
            warn!(
                bookmaker = "_24bet",
                collapsed_duplicates,
                "Collapsed duplicate direct-source quotes for the same market, selection and line"
            );
        }
        Ok((events, odds))
    }
}

#[async_trait]
impl BookmakerParser for _24betParser {
    fn name(&self) -> &str {
        "24bet"
    }
    fn slug(&self) -> &str {
        "_24bet"
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
                    warn!(error = %error, "24bet fetch events failed");
                    errors.push(format!("{url}: {error}"));
                }
            }
        }
        if !errors.is_empty() {
            return Err(std::io::Error::other(errors.join("; ")).into());
        }
        info!(count = all_events.len(), "24bet events parsed");
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
                    warn!(error = %error, "24bet fetch odds failed");
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
                    warn!(error = %error, "24bet fetch failed");
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
            time_ms = elapsed,
            "24bet fetch complete"
        );
        Ok(ParserResult::new("_24bet", all_events, all_odds, elapsed))
    }

    fn base_url(&self) -> &str {
        "https://24bet.ru"
    }
    fn user_agent(&self) -> &str {
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36"
    }
}

#[cfg(test)]
mod parser_fixture_tests {
    use super::_24betParser;
    use crate::factors_catalog::FactorsCatalog;
    use std::sync::Arc;
    use shared::odds::OddsType;
    use shared::Sport;

    fn catalog_with_sport_scoped_totals() -> FactorsCatalog {
        let catalog = FactorsCatalog::new(
            Arc::new(reqwest::Client::new()),
            "https://example.invalid",
            3000,
        );
        let parsed = catalog.parse_catalog(&serde_json::json!({
            "configs": [{
                "sports": ["1"],
                "markets": [{
                    "caption": "Тотал",
                    "tag": "total",
                    "marketEventKindsSets": [{
                        "factorSets": [{
                            "columns": [
                                {"caption": "Больше", "columnType": 1, "factors": ["1696"]},
                                {"caption": "Меньше", "columnType": 1, "factors": ["1697"]}
                            ]
                        }]
                    }]
                }]
            }]
        }));
        assert_eq!(parsed, 2);
        catalog
    }

    #[test]
    fn direct_factor_catalog_resolves_unknown_ids_by_sport_and_selection() {
        let catalog = catalog_with_sport_scoped_totals();
        let payload = serde_json::json!({
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
                    {"f": 1696, "v": 1.85, "p": 150},
                    {"f": 1697, "v": 1.95, "p": 150}
                ]
            }]
        });

        let (events, odds) = _24betParser::parse_api_response_with_catalog(
            &payload,
            false,
            Some(&catalog),
        )
        .expect("direct catalog fixture should parse");

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].id, "_24bet-pre-12345");
        assert_eq!(odds.len(), 2);
        assert!(odds.iter().any(|odd| {
            odd.market == "Total"
                && odd.selection == "Over"
                && odd.line == Some(1.5)
                && (odd.odds - 1.85).abs() < 1e-9
        }));
        assert!(odds.iter().any(|odd| {
            odd.market == "Total"
                && odd.selection == "Under"
                && odd.line == Some(1.5)
                && (odd.odds - 1.95).abs() < 1e-9
        }));
        assert!(odds.iter().all(|odd| !odd.market.starts_with("factor_")));
    }

    #[test]
    fn ambiguous_sport_scoped_factor_id_is_quarantined_before_legacy_fallback() {
        let catalog = FactorsCatalog::new(
            Arc::new(reqwest::Client::new()),
            "https://example.invalid",
            3000,
        );
        catalog.parse_catalog(&serde_json::json!({
            "configs": [
                {
                    "sports": ["1"],
                    "markets": [{
                        "caption": "Исходы",
                        "tag": "result",
                        "marketEventKindsSets": [{"factorSets": [{"columns": [
                            {"caption": "1", "columnType": 1, "factors": ["921"]}
                        ]}]}]
                    }]
                },
                {
                    "sports": ["2"],
                    "markets": [{
                        "caption": "Победы",
                        "tag": "to_win",
                        "marketEventKindsSets": [{"factorSets": [{"columns": [
                            {"caption": "1", "columnType": 1, "factors": ["921"]}
                        ]}]}]
                    }]
                }
            ]
        }));

        assert!(catalog.contains_factor_id(921));
        assert!(catalog.get_factor_for_sport(921, Some("3")).is_none());

        let payload = serde_json::json!({
            "events": [{
                "id": 12345,
                "team1": "Home FC",
                "team2": "Away FC",
                "place": "line",
                "sportId": 3,
                "startTime": 1795618800_i64,
                "name": "Unknown Sport: Competition"
            }],
            "customFactors": [{
                "e": 12345,
                "factors": [{"f": 921, "v": 2.10}]
            }]
        });

        let (events, odds) = _24betParser::parse_api_response_with_catalog(
            &payload,
            false,
            Some(&catalog),
        )
        .expect("ambiguous factor should not fail the event feed");

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].id, "_24bet-pre-12345");
        assert!(
            odds.is_empty(),
            "a catalog-known but sport-ambiguous factor must not use numeric fallback"
        );
    }

    #[test]
    fn factor_mapping_uses_semantic_btts_and_even_odd_types() {
        assert_eq!(
            _24betParser::factor_to_market(926),
            ("BTTS".into(), "Yes".into(), OddsType::BothTeamsScoreYes)
        );
        assert_eq!(
            _24betParser::factor_to_market(927),
            ("BTTS".into(), "No".into(), OddsType::BothTeamsScoreNo)
        );
        assert_eq!(
            _24betParser::factor_to_market(928),
            ("EvenOdd".into(), "Even".into(), OddsType::Even)
        );
        assert_eq!(
            _24betParser::factor_to_market(929),
            ("EvenOdd".into(), "Odd".into(), OddsType::Odd)
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
        let (events, odds) = _24betParser::parse_api_response(&sample_payload(), false)
            .expect("valid 24bet fixture");

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].id, "_24bet-pre-12345");
        assert_eq!(events[0].home_team, "Home FC");
        assert_eq!(events[0].away_team, "Away FC");
        assert_eq!(events[0].sport, Sport::Football);
        assert_eq!(events[0].league, "Premier League");
        assert_eq!(events[0].start_time.unwrap().timestamp(), 1795618800);
        assert_eq!(odds.len(), 3);
        assert_eq!(odds.iter().filter(|odd| odd.market == "Total").count(), 2);
        assert!(odds.iter().any(|odd| odd.line == Some(1.5)));
        assert!(odds.iter().any(|odd| odd.line == Some(2.5)));
        assert!(odds.iter().all(|odd| odd.event_id == events[0].id));
        assert!(odds.iter().all(|odd| odd.bookmaker_slug == "_24bet"));
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

        let (live_events, _) = _24betParser::parse_api_response(&payload, true)
            .expect("combined catalogue should parse live rows");
        let (prematch_events, _) = _24betParser::parse_api_response(&payload, false)
            .expect("combined catalogue should parse prematch rows");

        assert_eq!(live_events.len(), 1);
        assert_eq!(live_events[0].id, "_24bet-live-12346");
        assert!(live_events[0].is_live);
        assert_eq!(prematch_events.len(), 1);
        assert!(prematch_events[0].id.ends_with("12345"));
        assert!(!prematch_events[0].is_live);
    }

    #[test]
    fn unknown_event_sport_is_not_mislabeled_as_football() {
        let mut payload = sample_payload();
        payload["events"][0]["name"] = serde_json::json!("Unknown Sport: Competition");
        let (events, _) =
            _24betParser::parse_api_response(&payload, false).expect("valid event payload");
        assert_eq!(events[0].sport, Sport::Other);
        assert_eq!(events[0].league, "Competition");
    }

    #[test]
    fn rejects_payload_without_required_api_arrays() {
        let missing_events =
            _24betParser::parse_api_response(&serde_json::json!({"customFactors": []}), false)
                .expect_err("missing events must be a schema error");
        assert!(missing_events.to_string().contains("events array"));

        let missing_factors =
            _24betParser::parse_api_response(&serde_json::json!({"events": []}), false)
                .expect_err("missing factors must be a schema error");
        assert!(missing_factors.to_string().contains("customFactors array"));
    }
}
