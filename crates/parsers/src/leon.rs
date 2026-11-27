use crate::base::{BookmakerParser, ParserResult};
use async_trait::async_trait;
use chrono::Utc;
use reqwest::Client;
use shared::odds::OddsType;
use shared::{Event, Odd, Sport};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, info};

/// Leon API parser
/// API: leon.ru/api-2/betline/events/inplay and prematch
#[derive(Debug)]
pub struct LeonParser {
    client: Arc<Client>,
    live_url: String,
    prematch_url: String,
}

impl LeonParser {
    pub fn new(client: Arc<Client>) -> Self {
        Self {
            client,
            live_url: "https://leon.bet/api-2/betline/events/inplay".to_string(),
            prematch_url: "https://leon.bet/api-2/betline/events/prematch".to_string(),
        }
    }
}

#[async_trait]
impl BookmakerParser for LeonParser {
    fn name(&self) -> &str {
        "Leon"
    }
    fn slug(&self) -> &str {
        "leon"
    }
    fn is_enabled(&self) -> bool {
        true
    }

    async fn fetch_events(&self) -> Result<Vec<Event>, Box<dyn std::error::Error + Send + Sync>> {
        let (events, _) = self.fetch_both_feeds().await?;
        info!(count = events.len(), "Leon events parsed");
        Ok(events)
    }

    async fn fetch_odds(
        &self,
        _event_id: &str,
    ) -> Result<Vec<Odd>, Box<dyn std::error::Error + Send + Sync>> {
        let (_, odds) = self.fetch_both_feeds().await?;
        Ok(odds)
    }

    async fn fetch_all(&self) -> Result<ParserResult, Box<dyn std::error::Error + Send + Sync>> {
        let start = std::time::Instant::now();
        let (events, odds) = self.fetch_both_feeds().await?;
        if events.is_empty() || odds.is_empty() {
            return Err(std::io::Error::other(format!(
                "Leon combined feeds returned {} events / {} odds",
                events.len(),
                odds.len()
            ))
            .into());
        }
        let elapsed = start.elapsed().as_millis() as u64;
        info!(
            events = events.len(),
            odds = odds.len(),
            time_ms = elapsed,
            "Leon fetch complete"
        );
        Ok(ParserResult::new("leon", events, odds, elapsed))
    }

    fn base_url(&self) -> &str {
        "https://leon.ru"
    }
    fn user_agent(&self) -> &str {
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36"
    }
}

impl LeonParser {
    async fn fetch_both_feeds(
        &self,
    ) -> Result<(Vec<Event>, Vec<Odd>), Box<dyn std::error::Error + Send + Sync>> {
        let mut events = Vec::new();
        let mut odds = Vec::new();
        let mut errors = Vec::new();

        for (url, is_live) in [(&self.live_url, true), (&self.prematch_url, false)] {
            match self.fetch_api(url, is_live).await {
                Ok((feed_events, feed_odds)) => {
                    events.extend(feed_events);
                    odds.extend(feed_odds);
                }
                Err(error) => errors.push(format!(
                    "{} feed: {error}",
                    if is_live { "live" } else { "prematch" }
                )),
            }
        }

        if !errors.is_empty() {
            return Err(std::io::Error::other(format!(
                "Leon is missing one or more required feeds: {}",
                errors.join("; ")
            ))
            .into());
        }
        Ok((events, odds))
    }

    async fn fetch_api(
        &self,
        url: &str,
        is_live: bool,
    ) -> Result<(Vec<Event>, Vec<Odd>), Box<dyn std::error::Error + Send + Sync>> {
        let ctag_response = self
            .client
            .get("https://leon.bet/api-2/betline/ctag")
            .query(&[("curLang", "ru-ru")])
            .header("User-Agent", self.user_agent())
            .header("Accept", "application/json, text/plain, */*")
            .header("Accept-Language", "ru-RU,ru;q=0.9")
            .header("Referer", "https://leon.bet/ru-ru/")
            .header("Origin", "https://leon.bet")
            .timeout(Duration::from_secs(20))
            .send()
            .await?;

        if !ctag_response.status().is_success() {
            return Err(std::io::Error::other(format!(
                "Leon ctag endpoint returned HTTP {}",
                ctag_response.status()
            ))
            .into());
        }
        let ctag_payload: serde_json::Value = ctag_response.json().await?;
        let ctag = ctag_payload
            .get("ctag")
            .and_then(|value| value.as_str())
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| std::io::Error::other("Leon ctag response is missing ctag"))?;

        let response = self
            .client
            .get(url)
            .query(&[
                ("ctag", ctag),
                ("curLang", "ru-ru"),
                ("limit", "3000"),
                ("hideClosed", "true"),
            ])
            .header("User-Agent", self.user_agent())
            .header("Accept", "application/json, text/plain, */*")
            .header("Accept-Language", "ru-RU,ru;q=0.9")
            .header("Referer", "https://leon.bet/ru-ru/")
            .header("Origin", "https://leon.bet")
            .timeout(Duration::from_secs(30))
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(std::io::Error::other(format!(
                "Leon events endpoint returned HTTP {}",
                response.status()
            ))
            .into());
        }

        let json: serde_json::Value = response.json().await?;
        Self::parse_api_response(&json, is_live)
    }

    fn parse_api_response(
        json: &serde_json::Value,
        is_live: bool,
    ) -> Result<(Vec<Event>, Vec<Odd>), Box<dyn std::error::Error + Send + Sync>> {
        let events_array = json
            .get("events")
            .and_then(|events| events.as_array())
            .ok_or_else(|| {
                std::io::Error::other("Leon API response is missing the events array")
            })?;

        let mut events = Vec::with_capacity(events_array.len());
        let mut all_odds = Vec::new();
        let now = Utc::now();
        let mode = if is_live { "live" } else { "prematch" };

        for event_data in events_array {
            let Some((source_id, home, away, league_name)) = Self::extract_event_info(event_data)
            else {
                continue;
            };

            let event_key = format!("leon-{mode}-{source_id}");
            let league = league_name
                .unwrap_or_else(|| if is_live { "Live" } else { "Prematch" }.to_string());
            let sport = Self::sport_from_event(event_data);
            let start_time = event_data
                .get("kickoff")
                .and_then(Self::parse_numeric_value)
                .and_then(|value| chrono::DateTime::from_timestamp_millis(value as i64));

            let event = Event {
                id: event_key.clone(),
                sport,
                league,
                home_team: home,
                away_team: away,
                start_time,
                is_live,
                bookmaker_slug: "leon".to_string(),
                raw_url: event_data
                    .get("url")
                    .and_then(|value| value.as_str())
                    .map(|path| format!("https://leon.bet{path}")),
                extra: HashMap::new(),
            };
            let event_odds_start = all_odds.len();

            if let Some(markets) = event_data.get("markets").and_then(|value| value.as_array()) {
                for market in markets {
                    if market.get("open").and_then(|value| value.as_bool()) == Some(false) {
                        continue;
                    }
                    let market_name = market
                        .get("name")
                        .and_then(|value| value.as_str())
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .unwrap_or("unknown");
                    let market_id = Self::source_id(market.get("id"))
                        .unwrap_or_else(|| market_name.to_lowercase().replace([' ', '/'], "_"));
                    let market_line = market
                        .get("specifiers")
                        .and_then(|value| value.as_object())
                        .and_then(|specifiers| {
                            specifiers.get("hcp").or_else(|| specifiers.get("total"))
                        })
                        .and_then(Self::parse_line_value);

                    let Some(runners) = market.get("runners").and_then(|value| value.as_array())
                    else {
                        continue;
                    };
                    for runner in runners {
                        if runner.get("open").and_then(|value| value.as_bool()) == Some(false) {
                            continue;
                        }
                        let price = runner
                            .get("price")
                            .and_then(Self::parse_numeric_value)
                            .or_else(|| runner.get("priceStr").and_then(Self::parse_numeric_value));
                        let Some(price) = price.filter(|price| price.is_finite() && *price > 1.0)
                        else {
                            continue;
                        };
                        let selection = runner
                            .get("name")
                            .and_then(|value| value.as_str())
                            .map(str::trim)
                            .filter(|value| !value.is_empty())
                            .unwrap_or("unknown");
                        let odds_type = Self::runner_odds_type(runner, selection, market_name);
                        let line = runner
                            .get("handicap")
                            .and_then(Self::parse_line_value)
                            .or(market_line);
                        let runner_id = Self::source_id(runner.get("id"))
                            .unwrap_or_else(|| format!("{}-{}", market_name, selection));

                        let line_key = line
                            .map(|value| format!("{value:.6}").replace('-', "m").replace('.', "p"))
                            .unwrap_or_else(|| "no-line".to_string());
                        all_odds.push(Odd {
                            id: format!(
                                "{event_key}-market-{market_id}-runner-{runner_id}-line-{line_key}"
                            ),
                            event_id: event_key.clone(),
                            bookmaker_slug: "leon".to_string(),
                            market: market_name.to_string(),
                            selection: selection.to_string(),
                            odds: price,
                            odds_type,
                            line,
                            timestamp: now,
                        });
                    }
                }
            }

            // Events with no open, valid runners are not useful to the surebet
            // engine. Do not publish them as matchable events without prices.
            if all_odds.len() > event_odds_start {
                events.push(event);
            }
        }

        debug!(events = events.len(), odds = all_odds.len(), "Leon: parsed");
        Ok((events, all_odds))
    }

    fn source_id(value: Option<&serde_json::Value>) -> Option<String> {
        match value? {
            serde_json::Value::String(text) => {
                let text = text.trim();
                (!text.is_empty()).then(|| text.to_string())
            }
            serde_json::Value::Number(number) => Some(number.to_string()),
            _ => None,
        }
    }

    fn parse_numeric_value(value: &serde_json::Value) -> Option<f64> {
        match value {
            serde_json::Value::Number(number) => number.as_f64(),
            serde_json::Value::String(text) => text.trim().replace(',', ".").parse().ok(),
            _ => None,
        }
    }

    fn parse_line_value(value: &serde_json::Value) -> Option<f64> {
        Self::parse_numeric_value(value)
    }

    fn extract_event_info(
        data: &serde_json::Value,
    ) -> Option<(String, String, String, Option<String>)> {
        let event_id = data
            .get("id")
            .filter(|value| !value.is_null())?
            .to_string()
            .trim_matches('"')
            .to_string();
        if event_id.is_empty() {
            return None;
        }
        let competitors = data.get("competitors").and_then(|value| value.as_array())?;
        if competitors.len() < 2 {
            return None;
        }

        let home = competitors
            .iter()
            .find(|competitor| {
                competitor.get("homeAway").and_then(|value| value.as_str()) == Some("HOME")
            })
            .or_else(|| competitors.first())?
            .get("name")?
            .as_str()?
            .to_string();
        let away = competitors
            .iter()
            .find(|competitor| {
                competitor.get("homeAway").and_then(|value| value.as_str()) == Some("AWAY")
            })
            .or_else(|| competitors.get(1))?
            .get("name")?
            .as_str()?
            .to_string();
        if home.trim().is_empty() || away.trim().is_empty() || home == away {
            return None;
        }

        let league = data
            .get("league")
            .and_then(|value| value.get("name"))
            .and_then(|value| value.as_str())
            .map(str::to_string);
        Some((event_id, home, away, league))
    }

    fn sport_from_event(event: &serde_json::Value) -> Sport {
        let family = event
            .get("league")
            .and_then(|league| league.get("sport"))
            .and_then(|sport| sport.get("family"))
            .and_then(|family| family.as_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        match family.as_str() {
            "soccer" | "football" => Sport::Football,
            "tennis" => Sport::Tennis,
            "basketball" => Sport::Basketball,
            "icehockey" | "hockey" => Sport::Hockey,
            "volleyball" => Sport::Volleyball,
            "tabletennis" | "table_tennis" => Sport::TableTennis,
            "baseball" => Sport::Baseball,
            "handball" => Sport::Handball,
            "mma" => Sport::Mma,
            "boxing" => Sport::Boxing,
            "esports" => Sport::Esports,
            "futsal" => Sport::Futsal,
            "badminton" => Sport::Badminton,
            "darts" => Sport::Darts,
            "snooker" => Sport::Snooker,
            "waterpolo" | "water_polo" => Sport::WaterPolo,
            "rugby" => Sport::Rugby,
            "beachvolleyball" | "beach_volleyball" => Sport::BeachVolleyball,
            "floorball" => Sport::Floorball,
            "golf" => Sport::Golf,
            "motorsport" => Sport::Motorsport,
            _ => Sport::Other,
        }
    }

    fn runner_odds_type(
        runner: &serde_json::Value,
        selection: &str,
        market_name: &str,
    ) -> OddsType {
        let tags = runner
            .get("tags")
            .and_then(|value| value.as_array())
            .into_iter()
            .flatten()
            .filter_map(|tag| tag.as_str())
            .collect::<Vec<_>>();
        if tags.contains(&"HOME") {
            OddsType::Home
        } else if tags.contains(&"DRAW") {
            OddsType::Draw
        } else if tags.contains(&"AWAY") {
            OddsType::Away
        } else if tags.contains(&"OVER") {
            OddsType::Over
        } else if tags.contains(&"UNDER") {
            OddsType::Under
        } else {
            Self::selection_to_odds_type(selection, market_name)
        }
    }

    fn selection_to_odds_type(selection: &str, market_name: &str) -> OddsType {
        let lower = selection.to_lowercase();
        let market_lower = market_name.to_lowercase();

        if market_lower.contains("btts")
            || market_lower.contains("both teams score")
            || market_lower.contains("обе забьют")
            || market_lower.contains("обе команды забьют")
        {
            return match lower.as_str() {
                "yes" | "да" => OddsType::BothTeamsScoreYes,
                "no" | "нет" => OddsType::BothTeamsScoreNo,
                _ => OddsType::Custom,
            };
        }

        if market_lower.contains("even/odd")
            || market_lower.contains("even_odd")
            || market_lower.contains("чёт/нечёт")
            || market_lower.contains("чет/нечет")
        {
            return match lower.as_str() {
                "even" | "чёт" | "чет" => OddsType::Even,
                "odd" | "нечёт" | "нечет" => OddsType::Odd,
                _ => OddsType::Custom,
            };
        }

        if market_lower.contains("тотал") || market_lower.contains("total") {
            if lower.contains("больше") || lower.contains("over") || lower == "тб" || lower == "tb"
            {
                return OddsType::Over;
            } else if lower.contains("меньше")
                || lower.contains("under")
                || lower == "тм"
                || lower == "tm"
            {
                return OddsType::Under;
            }
        }

        if market_lower.contains("фора") || market_lower.contains("handicap") {
            return OddsType::Handicap;
        }

        match lower.as_str() {
            "1" | "п1" | "home" => OddsType::Home,
            "x" | "х" | "ничья" | "draw" | "tie" => OddsType::Draw,
            "2" | "п2" | "away" => OddsType::Away,
            "тб" | "tb" | "over" | "больше" => OddsType::Over,
            "тм" | "tm" | "under" | "меньше" => OddsType::Under,
            _ => OddsType::Custom,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::LeonParser;
    use serde_json::json;
    use shared::odds::OddsType;

    #[test]
    fn maps_btts_and_even_odd_runner_types_using_market_context() {
        assert_eq!(
            LeonParser::selection_to_odds_type("Yes", "Both Teams Score"),
            OddsType::BothTeamsScoreYes
        );
        assert_eq!(
            LeonParser::selection_to_odds_type("Нет", "Обе команды забьют"),
            OddsType::BothTeamsScoreNo
        );
        assert_eq!(
            LeonParser::selection_to_odds_type("Even", "Even/Odd"),
            OddsType::Even
        );
        assert_eq!(
            LeonParser::selection_to_odds_type("Нечёт", "Чёт/нечёт"),
            OddsType::Odd
        );
    }

    #[test]
    fn runner_ids_include_market_and_line_identity() {
        let payload = json!({
            "events": [{
                "id": 88,
                "competitors": [
                    {"name": "Home", "homeAway": "HOME"},
                    {"name": "Away", "homeAway": "AWAY"}
                ],
                "league": {"name": "League", "sport": {"family": "Soccer"}},
                "markets": [
                    {"id": 10, "name": "Тотал", "open": true,
                     "specifiers": {"total": "1.5"},
                     "runners": [{"id": 1, "name": "Over", "open": true, "price": 1.8, "handicap": "1.5"}]},
                    {"id": 11, "name": "Тотал", "open": true,
                     "specifiers": {"total": "2.5"},
                     "runners": [{"id": 1, "name": "Over", "open": true, "price": 2.0, "handicap": "2.5"}]}
                ]
            }]
        });
        let (events, odds) = LeonParser::parse_api_response(&payload, false).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(odds.len(), 2);
        assert!(odds.iter().any(|odd| odd.line == Some(1.5)));
        assert!(odds.iter().any(|odd| odd.line == Some(2.5)));
        let ids = odds
            .iter()
            .map(|odd| &odd.id)
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(ids.len(), 2);
    }

    #[test]
    fn normalizes_market_and_runner_ids_when_source_values_are_strings_or_null() {
        let payload = serde_json::json!({
            "events": [{
                "id": 900,
                "competitors": [
                    {"id": 1, "name": "Home FC", "homeAway": "HOME"},
                    {"id": 2, "name": "Away FC", "homeAway": "AWAY"}
                ],
                "league": {
                    "name": "Test League",
                    "sport": {"family": "Soccer"}
                },
                "markets": [
                    {
                        "id": " 100 ",
                        "name": " Total ",
                        "open": true,
                        "specifiers": {"total": "2.5"},
                        "runners": [
                            {"id": " 1001 ", "name": " Over ", "open": true, "price": " 2,05 ", "handicap": "2.5"}
                        ]
                    },
                    {
                        "id": null,
                        "name": "Winner",
                        "open": true,
                        "runners": [
                            {"id": null, "name": "Home", "open": true, "price": 1.8}
                        ]
                    }
                ]
            }]
        });

        let (events, odds) =
            LeonParser::parse_api_response(&payload, false).expect("valid Leon response");
        assert_eq!(events.len(), 1);
        assert_eq!(odds.len(), 2);
        assert!(odds.iter().any(|odd| {
            odd.market == "Total"
                && odd.selection == "Over"
                && (odd.odds - 2.05).abs() < 1e-9
                && odd.line == Some(2.5)
                && odd.id.contains("market-100-runner-1001")
        }));
        assert!(odds.iter().any(|odd| {
            odd.market == "Winner"
                && odd.selection == "Home"
                && odd.id.contains("market-winner-runner-Winner-Home")
        }));
        let ids = odds.iter().map(|odd| &odd.id).collect::<std::collections::HashSet<_>>();
        assert_eq!(ids.len(), odds.len());
    }

    #[test]
    fn parses_current_leon_event_market_and_live_identity() {
        let payload = json!({
            "events": [{
                "id": 42,
                "kickoff": 1795608000000_i64,
                "url": "/events/Soccer/league/42-match",
                "competitors": [
                    {"id": 2, "name": "Away FC", "homeAway": "AWAY"},
                    {"id": 1, "name": "Home FC", "homeAway": "HOME"}
                ],
                "league": {
                    "name": "Premier League",
                    "sport": {"family": "Soccer"}
                },
                "markets": [{
                    "id": 100,
                    "name": "Тотал",
                    "open": true,
                    "specifiers": {"total": "2.5"},
                    "runners": [
                        {"id": 1001, "name": "Больше (2.5)", "open": true, "tags": ["OVER"], "price": 1.91, "handicap": "2.5"},
                        {"id": 1002, "name": "Меньше (2.5)", "open": false, "tags": ["UNDER"], "price": 1.95, "handicap": "2.5"},
                        {"id": 1003, "name": "Invalid", "open": true, "tags": ["OVER"], "price": 1.0}
                    ]
                }]
            }, {
                "id": 43,
                "kickoff": 1795608000000_i64,
                "competitors": [
                    {"id": 3, "name": "No Odds Home", "homeAway": "HOME"},
                    {"id": 4, "name": "No Odds Away", "homeAway": "AWAY"}
                ],
                "league": {
                    "name": "Test League",
                    "sport": {"family": "Soccer"}
                },
                "markets": []
            }]
        });

        let (live_events, live_odds) = LeonParser::parse_api_response(&payload, true).unwrap();
        let (pre_events, _) = LeonParser::parse_api_response(&payload, false).unwrap();

        assert_eq!(live_events.len(), 1, "events without open odds are omitted");
        assert_eq!(live_events[0].id, "leon-live-42");
        assert!(live_events[0].is_live);
        assert_eq!(live_events[0].home_team, "Home FC");
        assert_eq!(live_events[0].away_team, "Away FC");
        assert_eq!(live_events[0].sport, shared::Sport::Football);
        assert_eq!(live_events[0].league, "Premier League");
        assert!(live_events[0].start_time.is_some());
        assert_eq!(
            live_odds.len(),
            1,
            "closed and invalid runners are excluded"
        );
        assert_eq!(live_odds[0].event_id, live_events[0].id);
        assert_eq!(live_odds[0].odds_type, OddsType::Over);
        assert_eq!(live_odds[0].line, Some(2.5));
        assert_eq!(pre_events[0].id, "leon-prematch-42");
        assert!(!pre_events[0].is_live);
    }
}
