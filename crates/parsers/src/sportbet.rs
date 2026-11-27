use crate::base::{BookmakerParser, ParserResult};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use reqwest::Client;
use shared::odds::OddsType;
use shared::{Event, Odd, Sport};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, info};

const LIVE_EVENTS_URL: &str = "https://bthm-server.sportbet.ru/events.table?lang=ru&status=live";
const PREMATCH_EVENTS_URL: &str =
    "https://bthm-server.sportbet.ru/events.table?lang=ru&status=prematch";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const USER_AGENT: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";

/// SportBet's current public line feed.
///
/// The old `/sport/v1/*fixtures-tree*` routes are no longer the line API. The
/// current frontend reads `events.table` from `bthm-server.sportbet.ru` and
/// receives a nested sports -> tournaments -> events -> markets payload.
#[derive(Debug)]
pub struct SportbetParser {
    client: Arc<Client>,
    live_url: String,
    prematch_url: String,
}

impl SportbetParser {
    pub fn new(client: Arc<Client>) -> Self {
        Self {
            client,
            live_url: LIVE_EVENTS_URL.to_string(),
            prematch_url: PREMATCH_EVENTS_URL.to_string(),
        }
    }

    async fn fetch_api(
        &self,
        url: &str,
        is_live: bool,
    ) -> Result<(Vec<Event>, Vec<Odd>), Box<dyn std::error::Error + Send + Sync>> {
        debug!(url, is_live, "SportBet: fetching current events.table feed");

        let referer = if is_live {
            "https://sportbet.ru/live"
        } else {
            "https://sportbet.ru/prematch"
        };

        let response = self
            .client
            .get(url)
            .header("User-Agent", USER_AGENT)
            .header("Accept", "application/json, text/plain, */*")
            .header("Accept-Language", "ru-RU,ru;q=0.9,en;q=0.8")
            .header("Referer", referer)
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await?
            .error_for_status()?;

        let payload: serde_json::Value = response.json().await?;
        if payload.get("status").and_then(|value| value.as_str()) != Some("ok") {
            return Err(format!(
                "SportBet events.table returned an unexpected status: {}",
                payload
                    .get("status")
                    .and_then(|value| value.as_str())
                    .unwrap_or("missing")
            )
            .into());
        }

        Self::parse_api_response(&payload, is_live)
    }

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
                    "{} feed: {}",
                    if is_live { "live" } else { "prematch" },
                    error
                )),
            }
        }

        if !errors.is_empty() {
            return Err(format!(
                "SportBet feed is only partially available: {}",
                errors.join("; ")
            )
            .into());
        }

        Ok((events, odds))
    }

    fn parse_api_response(
        payload: &serde_json::Value,
        is_live: bool,
    ) -> Result<(Vec<Event>, Vec<Odd>), Box<dyn std::error::Error + Send + Sync>> {
        let sports = payload
            .get("data")
            .and_then(|data| data.get("sports"))
            .and_then(|sports| sports.as_array())
            .ok_or("SportBet events.table schema mismatch: data.sports is missing")?;

        let mut events = Vec::new();
        let mut odds = Vec::new();
        let now = Utc::now();
        let mode = if is_live { "live" } else { "prematch" };

        for sport_data in sports {
            let sport_slug = sport_data
                .get("slug")
                .and_then(|value| value.as_str())
                .unwrap_or_default();
            let sport_name = sport_data
                .get("name")
                .and_then(|value| value.as_str())
                .unwrap_or(sport_slug);
            let sport = Self::parse_sport(sport_slug, sport_name);

            let Some(tournaments) = sport_data
                .get("tournaments")
                .and_then(|value| value.as_array())
            else {
                continue;
            };

            for tournament in tournaments {
                let tournament_name = tournament
                    .get("name")
                    .and_then(|value| value.as_str())
                    .unwrap_or("Unknown tournament");
                let category_name = tournament
                    .get("category")
                    .and_then(|category| category.get("name"))
                    .and_then(|value| value.as_str())
                    .unwrap_or_default();
                let league = if category_name.is_empty() {
                    tournament_name.to_string()
                } else {
                    format!("{}: {}", category_name, tournament_name)
                };

                let Some(event_list) = tournament.get("events").and_then(|value| value.as_array())
                else {
                    continue;
                };

                for event_data in event_list {
                    let Some(source_event_id) =
                        event_data.get("id").and_then(Self::parse_u64_value)
                    else {
                        continue;
                    };
                    let Some(teams) = event_data.get("teams") else {
                        continue;
                    };
                    let Some(home_team) = teams
                        .get("team1")
                        .and_then(|team| team.get("name"))
                        .and_then(|value| value.as_str())
                    else {
                        continue;
                    };
                    let Some(away_team) = teams
                        .get("team2")
                        .and_then(|team| team.get("name"))
                        .and_then(|value| value.as_str())
                    else {
                        continue;
                    };
                    if home_team.trim().is_empty() || away_team.trim().is_empty() {
                        continue;
                    }

                    let event_id = format!("sportbet-{}-{}", mode, source_event_id);
                    let start_time = event_data
                        .get("scheduled")
                        .and_then(Self::parse_i64_value)
                        .and_then(DateTime::<Utc>::from_timestamp_millis);

                    let event = Event {
                        id: event_id.clone(),
                        sport,
                        league: league.clone(),
                        home_team: home_team.to_string(),
                        away_team: away_team.to_string(),
                        start_time,
                        is_live,
                        bookmaker_slug: "sportbet".to_string(),
                        raw_url: event_data
                            .get("slug")
                            .and_then(|value| value.as_str())
                            .map(|slug| format!("https://sportbet.ru/{}/{}", mode, slug)),
                        extra: HashMap::new(),
                    };
                    let event_odds_start = odds.len();

                    let Some(markets) =
                        event_data.get("markets").and_then(|value| value.as_array())
                    else {
                        continue;
                    };

                    for market_data in markets {
                        if market_data
                            .get("status")
                            .and_then(|value| value.as_str())
                            .is_some_and(|status| status != "active")
                        {
                            continue;
                        }

                        let market_name = market_data
                            .get("name")
                            .and_then(|value| value.as_str())
                            .unwrap_or("Other");
                        let normalized_market = Self::normalize_market(market_name);
                        // Market IDs can be numeric or strings. Falling back to 0
                        // collapses distinct string-ID markets into the same Odd IDs.
                        let market_id = Self::parse_identifier(market_data.get("id"))
                            .unwrap_or_else(|| {
                                format!(
                                    "name-{}",
                                    market_name
                                        .trim()
                                        .to_lowercase()
                                        .replace([' ', '/'], "_")
                                )
                            });
                        let Some(outcomes) = market_data
                            .get("outcomes")
                            .and_then(|value| value.as_array())
                        else {
                            continue;
                        };

                        for outcome in outcomes {
                            if outcome.get("active").and_then(|value| value.as_bool())
                                == Some(false)
                            {
                                continue;
                            }
                            let Some(decimal_odds) =
                                outcome.get("odd").and_then(Self::parse_f64_value)
                            else {
                                continue;
                            };
                            if !decimal_odds.is_finite() || decimal_odds <= 1.0 {
                                continue;
                            }

                            let outcome_name = outcome
                                .get("name")
                                .and_then(|value| value.as_str())
                                .unwrap_or("Unknown");
                            let outcome_id = outcome
                                .get("id")
                                .and_then(Self::parse_u64_value)
                                .map(|value| value.to_string())
                                .unwrap_or_else(|| {
                                    format!(
                                        "{}-{}",
                                        outcome_name.to_lowercase().replace([' ', '/'], "_"),
                                        Self::parse_line(
                                            outcome
                                                .get("specifiers")
                                                .and_then(|value| value.as_str())
                                                .unwrap_or_default()
                                        )
                                        .map(|value| format!("{value:.6}"))
                                        .unwrap_or_else(|| "no-line".to_string())
                                    )
                                });
                            let participant_id = outcome
                                .get("pId")
                                .and_then(Self::parse_u64_value)
                                .unwrap_or_default();
                            let specifiers = outcome
                                .get("specifiers")
                                .and_then(|value| value.as_str())
                                .unwrap_or_default();
                            let (selection, odds_type) =
                                Self::normalize_outcome(participant_id, outcome_name, market_name);
                            let line = Self::normalize_outcome_line(
                                participant_id,
                                outcome_name,
                                specifiers,
                                market_name,
                            );

                            odds.push(Odd {
                                id: format!(
                                    "{}-market-{}-outcome-{}-line-{}",
                                    event_id,
                                    market_id,
                                    outcome_id,
                                    line.map(|value| format!("{value:.6}")
                                        .replace('-', "m")
                                        .replace('.', "p"))
                                        .unwrap_or_else(|| "no-line".to_string())
                                ),
                                event_id: event_id.clone(),
                                bookmaker_slug: "sportbet".to_string(),
                                market: normalized_market.clone(),
                                selection,
                                odds: decimal_odds,
                                odds_type,
                                line,
                                timestamp: now,
                            });
                        }
                    }
                    if odds.len() > event_odds_start {
                        events.push(event);
                    }
                }
            }
        }

        if events.is_empty() || odds.is_empty() {
            return Err(std::io::Error::other(format!(
                "SportBet {} payload produced {} events / {} odds",
                mode,
                events.len(),
                odds.len()
            ))
            .into());
        }

        debug!(
            is_live,
            events = events.len(),
            odds = odds.len(),
            "SportBet events.table parsed"
        );
        Ok((events, odds))
    }

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

    fn parse_i64_value(value: &serde_json::Value) -> Option<i64> {
        value
            .as_i64()
            .or_else(|| value.as_str().and_then(|text| text.trim().parse().ok()))
    }

    fn parse_identifier(value: Option<&serde_json::Value>) -> Option<String> {
        match value? {
            serde_json::Value::String(text) => {
                let text = text.trim();
                (!text.is_empty()).then(|| text.to_string())
            }
            serde_json::Value::Number(number) => Some(number.to_string()),
            _ => None,
        }
    }

    fn parse_sport(slug: &str, name: &str) -> Sport {
        match slug.to_ascii_lowercase().as_str() {
            "soccer" => Sport::Football,
            "ice-hockey" => Sport::Hockey,
            "table-tennis" => Sport::TableTennis,
            "beach-volleyball" => Sport::BeachVolleyball,
            "american-football" => Sport::Other,
            _ => Sport::from_str(name),
        }
    }

    fn normalize_market(name: &str) -> String {
        let normalized = name.to_lowercase();
        if normalized.contains("победитель карты")
            || normalized.contains("результат карты")
            || normalized.contains("map winner")
        {
            "MapWinner".to_string()
        } else if normalized.contains("победитель сета") || normalized.contains("результат сета")
        {
            "SetWinner".to_string()
        } else if normalized.contains("победитель раунда")
            || normalized.contains("результат раунда")
        {
            "RoundWinner".to_string()
        } else if normalized.contains("индивидуальн")
            || normalized.contains("командный тотал")
            || normalized.contains("team total")
        {
            "IndividualTotal".to_string()
        } else if normalized.contains("исход") || normalized.contains("match result") {
            "1X2".to_string()
        } else if normalized.contains("победител") || normalized.contains("winner") {
            "winner".to_string()
        } else if normalized.contains("тотал") || normalized.contains("total") {
            "Total".to_string()
        } else if normalized.contains("фора") || normalized.contains("handicap") {
            "Handicap".to_string()
        } else if normalized.contains("двойн") || normalized.contains("double chance") {
            "Double Chance".to_string()
        } else {
            name.trim().to_string()
        }
    }

    fn normalize_outcome(
        participant_id: u64,
        source_name: &str,
        market_name: &str,
    ) -> (String, OddsType) {
        let market = market_name.to_lowercase();
        let name = source_name.to_lowercase();

        let is_btts = market.contains("btts")
            || market.contains("both teams score")
            || market.contains("обе забьют")
            || market.contains("обе команды забьют");
        let is_even_odd = market.contains("even/odd")
            || market.contains("even_odd")
            || market.contains("чёт/нечёт")
            || market.contains("чет/нечет")
            || market.contains("чётность")
            || market.contains("четность");

        let selection = if is_btts && (name == "да" || name == "yes") {
            "Yes"
        } else if is_btts && (name == "нет" || name == "no") {
            "No"
        } else if is_even_odd
            && (name.contains("чёт") || name.contains("чет") || name == "even")
            && !name.contains("не")
        {
            "Even"
        } else if is_even_odd && (name.contains("нечёт") || name.contains("нечет") || name == "odd")
        {
            "Odd"
        } else {
            match participant_id {
                1 | 4 | 1714 => "1",
                2 => "X",
                3 | 5 | 1715 => "2",
                9 => "1X",
                10 => "12",
                11 => "X2",
                12 => "Over",
                13 => "Under",
                _ if name.starts_with("поб 1") || name.starts_with("п1") => "1",
                _ if name.starts_with("поб 2") || name.starts_with("п2") => "2",
                _ if name.contains("нич") => "X",
                _ if name.contains("тб") || name.contains("больше") || name.contains("over") => {
                    "Over"
                }
                _ if name.contains("тм") || name.contains("меньше") || name.contains("under") => {
                    "Under"
                }
                _ => source_name.trim(),
            }
        }
        .to_string();

        let odds_type = if is_btts && selection == "Yes" {
            OddsType::BothTeamsScoreYes
        } else if is_btts && selection == "No" {
            OddsType::BothTeamsScoreNo
        } else if is_even_odd && selection == "Even" {
            OddsType::Even
        } else if is_even_odd && selection == "Odd" {
            OddsType::Odd
        } else if market.contains("фора") || market.contains("handicap") {
            OddsType::Handicap
        } else {
            match selection.as_str() {
                "1" => OddsType::Home,
                "X" => OddsType::Draw,
                "2" => OddsType::Away,
                "Over" => OddsType::Over,
                "Under" => OddsType::Under,
                _ => OddsType::Custom,
            }
        };
        (selection, odds_type)
    }

    fn normalize_outcome_line(
        participant_id: u64,
        outcome_name: &str,
        specifiers: &str,
        market_name: &str,
    ) -> Option<f64> {
        let normalized_market = market_name.to_lowercase();
        let is_handicap =
            normalized_market.contains("фора") || normalized_market.contains("handicap");
        if is_handicap {
            if let Some(line) = Self::parse_named_line(outcome_name) {
                return Some(line);
            }

            let source_line = Self::parse_line(specifiers)?;
            return Some(match participant_id {
                1714 => source_line,
                1715 => -source_line,
                _ => source_line,
            });
        }

        Self::parse_line(specifiers)
    }

    fn parse_named_line(outcome_name: &str) -> Option<f64> {
        let open = outcome_name.rfind('(')?;
        let close = outcome_name[open + 1..].find(')')? + open + 1;
        let value = outcome_name[open + 1..close].trim().replace(',', ".");
        let parsed = value.parse::<f64>().ok()?;
        parsed.is_finite().then_some(parsed)
    }

    fn parse_line(specifiers: &str) -> Option<f64> {
        specifiers.split('&').find_map(|part| {
            let (key, value) = part.split_once('=')?;
            if !matches!(key.trim(), "total" | "hcp" | "handicap" | "points") {
                return None;
            }
            let value = value.trim().parse::<f64>().ok()?;
            value.is_finite().then_some(value)
        })
    }
}

#[async_trait]
impl BookmakerParser for SportbetParser {
    fn name(&self) -> &str {
        "Sportbet"
    }

    fn slug(&self) -> &str {
        "sportbet"
    }

    fn is_enabled(&self) -> bool {
        true
    }

    async fn fetch_events(&self) -> Result<Vec<Event>, Box<dyn std::error::Error + Send + Sync>> {
        let (events, _) = self.fetch_both_feeds().await?;
        info!(count = events.len(), "SportBet events parsed");
        Ok(events)
    }

    async fn fetch_odds(
        &self,
        event_id: &str,
    ) -> Result<Vec<Odd>, Box<dyn std::error::Error + Send + Sync>> {
        let (_, odds) = self.fetch_both_feeds().await?;
        if event_id.is_empty() {
            Ok(odds)
        } else {
            Ok(odds
                .into_iter()
                .filter(|odd| odd.event_id == event_id)
                .collect())
        }
    }

    async fn fetch_all(&self) -> Result<ParserResult, Box<dyn std::error::Error + Send + Sync>> {
        let start = std::time::Instant::now();
        let (events, odds) = self.fetch_both_feeds().await?;
        let elapsed = start.elapsed().as_millis() as u64;
        info!(
            events = events.len(),
            odds = odds.len(),
            time_ms = elapsed,
            "SportBet fetch complete"
        );
        Ok(ParserResult::new("sportbet", events, odds, elapsed))
    }

    fn base_url(&self) -> &str {
        "https://sportbet.ru"
    }

    fn user_agent(&self) -> &str {
        USER_AGENT
    }
}

#[cfg(test)]
mod tests {
    use super::SportbetParser;
    use shared::odds::OddsType;
    use shared::Sport;

    fn sample_payload(status: &str) -> serde_json::Value {
        serde_json::json!({
            "status": "ok",
            "data": {
                "sports": [{
                    "id": 1,
                    "name": "Футбол",
                    "slug": "soccer",
                    "tournaments": [{
                        "id": 101,
                        "name": "Премьер-лига",
                        "category": {"id": 1, "name": "Россия", "slug": "russia"},
                        "events": [{
                            "id": 777,
                            "scheduled": 1795618800000_i64,
                            "slug": "team-a-vs-team-b",
                            "status": status,
                            "teams": {
                                "team1": {"id": 11, "name": "Команда А"},
                                "team2": {"id": 22, "name": "Команда Б"}
                            },
                            "markets": [
                                {"id": 1, "name": "Исход", "status": "active", "outcomes": [
                                    {"id": 1001, "pId": 1, "name": "Поб 1", "odd": 2.10, "active": true, "specifiers": "null"},
                                    {"id": 1002, "pId": 2, "name": "Ничья", "odd": 3.40, "active": true, "specifiers": "null"},
                                    {"id": 1003, "pId": 3, "name": "Поб 2", "odd": 3.70, "active": true, "specifiers": "null"}
                                ]},
                                {"id": 16, "name": "Фора", "status": "active", "outcomes": [
                                    {"id": 1004, "pId": 1714, "name": "Фора 1 (-0.5)", "odd": 1.90, "active": true, "specifiers": "hcp=-0.5"},
                                    {"id": 1005, "pId": 1715, "name": "Фора 2 (0.5)", "odd": 1.95, "active": true, "specifiers": "hcp=-0.5"}
                                ]},
                                {"id": 18, "name": "Тотал", "status": "active", "outcomes": [
                                    {"id": 1006, "pId": 12, "name": "ТБ 2.5", "odd": 1.80, "active": true, "specifiers": "total=2.5"},
                                    {"id": 1007, "pId": 13, "name": "ТМ 2.5", "odd": 2.00, "active": true, "specifiers": "total=2.5"}
                                ]},
                                {"id": 99, "name": "Заблокированный рынок", "status": "blocked", "outcomes": [
                                    {"id": 1099, "pId": 1, "name": "Поб 1", "odd": 100.0, "active": true}
                                ]}
                            ]
                        }]
                    }]
                }]
            }
        })
    }

    #[test]
    fn maps_btts_and_even_odd_to_semantic_outcome_types() {
        assert_eq!(
            SportbetParser::normalize_outcome(0, "Да", "Обе команды забьют"),
            ("Yes".to_string(), OddsType::BothTeamsScoreYes)
        );
        assert_eq!(
            SportbetParser::normalize_outcome(0, "Нет", "Обе команды забьют"),
            ("No".to_string(), OddsType::BothTeamsScoreNo)
        );
        assert_eq!(
            SportbetParser::normalize_outcome(0, "Чёт", "Чёт/нечёт"),
            ("Even".to_string(), OddsType::Even)
        );
        assert_eq!(
            SportbetParser::normalize_outcome(0, "Нечёт", "Чёт/нечёт"),
            ("Odd".to_string(), OddsType::Odd)
        );
    }

    #[test]
    fn parses_nested_sports_tournaments_events_and_markets() {
        let payload = sample_payload("not_started");
        let (events, odds) = SportbetParser::parse_api_response(&payload, false).expect("parse");

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].id, "sportbet-prematch-777");
        assert_eq!(events[0].sport, Sport::Football);
        assert_eq!(events[0].league, "Россия: Премьер-лига");
        assert_eq!(events[0].home_team, "Команда А");
        assert_eq!(events[0].away_team, "Команда Б");
        assert!(!events[0].is_live);
        assert!(events[0].start_time.is_some());

        assert_eq!(odds.len(), 7);
        assert!(odds.iter().all(|odd| odd.event_id == events[0].id));
        assert_eq!(odds.iter().filter(|odd| odd.market == "1X2").count(), 3);
        assert_eq!(
            odds.iter().filter(|odd| odd.market == "Handicap").count(),
            2
        );
        assert_eq!(odds.iter().filter(|odd| odd.market == "Total").count(), 2);
        assert!(odds
            .iter()
            .any(|odd| odd.selection == "1" && odd.odds_type == OddsType::Home));
        assert!(odds
            .iter()
            .any(|odd| odd.selection == "X" && odd.odds_type == OddsType::Draw));
        assert!(odds
            .iter()
            .any(|odd| odd.selection == "2" && odd.odds_type == OddsType::Away));
        assert!(odds.iter().any(|odd| {
            odd.market == "Handicap"
                && odd.selection == "1"
                && odd.odds_type == OddsType::Handicap
                && odd.line == Some(-0.5)
        }));
        assert!(odds.iter().any(|odd| {
            odd.market == "Handicap"
                && odd.selection == "2"
                && odd.odds_type == OddsType::Handicap
                && odd.line == Some(0.5)
        }));
        assert!(odds
            .iter()
            .any(|odd| odd.market == "Total" && odd.line == Some(2.5)));
    }

    #[test]
    fn supports_string_ids_odds_and_line_scoped_duplicate_outcome_ids() {
        let mut payload = sample_payload("not_started");
        payload["data"]["sports"][0]["tournaments"][0]["events"][0]["id"] =
            serde_json::json!("777");
        payload["data"]["sports"][0]["tournaments"][0]["events"][0]["markets"][2]["outcomes"][0]
            ["id"] = serde_json::json!("same-outcome-id");
        payload["data"]["sports"][0]["tournaments"][0]["events"][0]["markets"][2]["outcomes"][1]
            ["id"] = serde_json::json!("same-outcome-id");
        payload["data"]["sports"][0]["tournaments"][0]["events"][0]["markets"][2]["outcomes"][0]
            ["odd"] = serde_json::json!("1.80");
        payload["data"]["sports"][0]["tournaments"][0]["events"][0]["markets"][2]["outcomes"][1]
            ["odd"] = serde_json::json!("2.00");

        let (_, odds) = SportbetParser::parse_api_response(&payload, false)
            .expect("string ID/odds fixture should parse");
        let totals = odds
            .iter()
            .filter(|odd| odd.market == "Total")
            .collect::<Vec<_>>();
        assert_eq!(totals.len(), 2);
        assert_ne!(totals[0].id, totals[1].id);
        assert!(totals.iter().any(|odd| odd.odds == 1.8));
        assert!(totals.iter().any(|odd| odd.odds == 2.0));
    }

    #[test]
    fn string_market_ids_and_string_timestamps_remain_distinct() {
        let mut payload = sample_payload("not_started");
        let event = &mut payload["data"]["sports"][0]["tournaments"][0]["events"][0];
        event["scheduled"] = serde_json::json!("1795618800000");
        event["markets"][0]["id"] = serde_json::json!("market-result");
        event["markets"][1]["id"] = serde_json::json!("market-handicap");
        event["markets"][2]["id"] = serde_json::json!("market-total");

        let (events, odds) =
            SportbetParser::parse_api_response(&payload, false).expect("string identifiers parse");
        assert_eq!(
            events[0].start_time.map(|value| value.timestamp_millis()),
            Some(1795618800000)
        );
        assert!(odds.iter().any(|odd| odd.id.contains("market-market-result-")));
        assert!(odds.iter().any(|odd| odd.id.contains("market-market-handicap-")));
        assert!(odds.iter().any(|odd| odd.id.contains("market-market-total-")));
        let ids = odds.iter().map(|odd| &odd.id).collect::<std::collections::HashSet<_>>();
        assert_eq!(ids.len(), odds.len());
    }

    #[test]
    fn namespaces_reused_source_event_ids_between_live_and_prematch() {
        let payload = sample_payload("live");
        let (live_events, live_odds) =
            SportbetParser::parse_api_response(&payload, true).expect("live");
        let (pre_events, pre_odds) =
            SportbetParser::parse_api_response(&payload, false).expect("prematch");

        assert_eq!(live_events[0].id, "sportbet-live-777");
        assert_eq!(pre_events[0].id, "sportbet-prematch-777");
        assert_ne!(live_events[0].id, pre_events[0].id);
        assert!(live_odds
            .iter()
            .all(|odd| odd.event_id == live_events[0].id));
        assert!(pre_odds.iter().all(|odd| odd.event_id == pre_events[0].id));
        assert!(live_events[0].is_live);
        assert!(!pre_events[0].is_live);
    }

    #[test]
    fn ignores_inactive_outcomes_and_non_decimal_prices() {
        let mut payload = sample_payload("not_started");
        let outcomes = payload["data"]["sports"][0]["tournaments"][0]["events"][0]["markets"][0]
            ["outcomes"]
            .as_array_mut()
            .expect("outcomes");
        outcomes[0]["odd"] = serde_json::json!(1.0);
        outcomes[1]["active"] = serde_json::json!(false);
        let (events, odds) = SportbetParser::parse_api_response(&payload, false).expect("parse");
        assert_eq!(events.len(), 1);
        assert_eq!(odds.len(), 5);
        assert!(odds
            .iter()
            .all(|odd| odd.odds.is_finite() && odd.odds > 1.0));
    }

    #[test]
    fn handicap_outcomes_keep_individual_signed_lines_and_types() {
        let payload = serde_json::json!({
            "status": "ok",
            "data": {
                "sports": [{
                    "slug": "baseball",
                    "name": "Бейсбол",
                    "tournaments": [{
                        "name": "Winter League",
                        "events": [{
                            "id": 1140145,
                            "teams": {
                                "team1": {"name": "Taiwan Seas"},
                                "team2": {"name": "Taiwan Mountains"}
                            },
                            "markets": [{
                                "id": 256,
                                "name": "Фора",
                                "status": "active",
                                "outcomes": [
                                    {"id": 1, "pId": 1714, "name": "Фора 1 (3.5)", "odd": 2.65, "active": true, "specifiers": "hcp=3.5"},
                                    {"id": 2, "pId": 1715, "name": "Фора 2 (-3.5)", "odd": 1.40, "active": true, "specifiers": "hcp=3.5"},
                                    {"id": 3, "pId": 1714, "name": "Фора 1 (4,5)", "odd": 1.70, "active": true, "specifiers": "hcp=4.5"},
                                    {"id": 4, "pId": 1715, "name": "Фора 2 (-4,5)", "odd": 2.10, "active": true, "specifiers": "hcp=4.5"}
                                ]
                            }]
                        }]
                    }]
                }]
            }
        });

        let (events, odds) = SportbetParser::parse_api_response(&payload, true)
            .expect("baseball handicap fixture should parse");

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].sport, Sport::Baseball);
        assert_eq!(odds.len(), 4);
        for (selection, line) in [("1", 3.5), ("2", -3.5), ("1", 4.5), ("2", -4.5)] {
            assert!(
                odds.iter().any(|odd| {
                    odd.selection == selection
                        && odd.line == Some(line)
                        && odd.odds_type == OddsType::Handicap
                }),
                "missing handicap side {selection} at line {line}"
            );
        }
    }

    #[test]
    fn normalizes_live_source_market_names_without_collapsing_market_scopes() {
        for (source, expected) in [
            ("Исход", "1X2"),
            ("Двойной шанс", "Double Chance"),
            ("Победитель", "winner"),
            ("Победитель карты", "MapWinner"),
            ("Результат карты", "MapWinner"),
            ("Результат сета", "SetWinner"),
            ("Результат раунда", "RoundWinner"),
            ("Фора", "Handicap"),
            ("Фора очков", "Handicap"),
            ("Фора по сетам", "Handicap"),
            ("Тотал", "Total"),
            ("Тотал очков", "Total"),
            ("Индивидуальный тотал 1К", "IndividualTotal"),
        ] {
            assert_eq!(
                SportbetParser::normalize_market(source),
                expected,
                "{source}"
            );
        }
    }

    #[test]
    fn handicap_line_falls_back_to_signed_source_specifier() {
        assert_eq!(
            SportbetParser::normalize_outcome_line(1714, "Фора 1", "hcp=0.5", "Фора"),
            Some(0.5)
        );
        assert_eq!(
            SportbetParser::normalize_outcome_line(1715, "Фора 2", "hcp=0.5", "Фора"),
            Some(-0.5)
        );
        assert_eq!(
            SportbetParser::normalize_outcome_line(1714, "Фора 1", "hcp=-0.5", "Фора"),
            Some(-0.5)
        );
        assert_eq!(
            SportbetParser::normalize_outcome_line(1715, "Фора 2", "hcp=-0.5", "Фора"),
            Some(0.5)
        );
    }

    #[test]
    fn handicap_outcome_type_is_not_confused_with_match_winner() {
        let (home, home_type) = SportbetParser::normalize_outcome(1714, "Фора 1 (3.5)", "Фора");
        let (away, away_type) = SportbetParser::normalize_outcome(1715, "Фора 2 (-3.5)", "Фора");

        assert_eq!(home, "1");
        assert_eq!(away, "2");
        assert_eq!(home_type, OddsType::Handicap);
        assert_eq!(away_type, OddsType::Handicap);
    }

    #[test]
    fn rejects_unrecognized_response_schema() {
        let payload = serde_json::json!({"status": "ok", "data": {"fixtures": []}});
        let error = SportbetParser::parse_api_response(&payload, true).expect_err("schema error");
        assert!(error.to_string().contains("data.sports"));
    }
}
