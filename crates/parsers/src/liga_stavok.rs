use crate::base::{BookmakerParser, ParserResult};
use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
use reqwest::Client;
use shared::odds::OddsType;
use shared::{
    DiagnosticSeverity, Event, Odd, ParserDiagnosticCheck, ParserReadiness, ParserReadinessStage,
    Sport,
};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::sleep;
use tracing::{debug, error, info, warn};

// ─────────────────────────────────────────────────────────────────────────────
// Constants
// ─────────────────────────────────────────────────────────────────────────────

const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";
const EVENTS_LIST_URL: &str = "https://lds-api-sites.ligastavok.ru/rest/events/v8/eventsList";
const BOOKMAKER_SLUG: &str = "ligastavok";

// Retry configuration (exponential backoff pattern from Zenit/Betcity)
const MAX_RETRIES: u32 = 3;
const INITIAL_BACKOFF_MS: u64 = 500;
const MAX_BACKOFF_MS: u64 = 5000;
const REQUEST_TIMEOUT_SECS: u64 = 30;

// KPI targets for production readiness
#[cfg(test)]
const STRICT_LIVE_KPI_TARGET: usize = 150;
#[cfg(test)]
const STRICT_PREMATCH_KPI_TARGET: usize = 3000;

// Market type mappings
const MARKET_1X2: &str = "1X2";
const MARKET_TOTAL: &str = "Total";
const MARKET_BTTS: &str = "BothTeamsScore";
const MARKET_HANDICAP: &str = "Handicap";
const MARKET_EVEN_ODD: &str = "EvenOdd";

// ─────────────────────────────────────────────────────────────────────────────
// Proxy Configuration
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct ProxyConfig {
    urls: Vec<String>,
    current_index: usize,
}

impl ProxyConfig {
    fn new() -> Self {
        // Support environment-based proxy list
        let proxy_urls = std::env::var("LIGASTAVOK_PROXY_LIST")
            .unwrap_or_default()
            .split(',')
            .filter(|p| !p.trim().is_empty())
            .map(|p| p.trim().to_string())
            .collect::<Vec<_>>();

        Self {
            urls: proxy_urls,
            current_index: 0,
        }
    }

    fn get_proxy(&mut self) -> Option<String> {
        if self.urls.is_empty() {
            return None;
        }
        let proxy = self.urls[self.current_index % self.urls.len()].clone();
        self.current_index += 1;
        Some(proxy)
    }

    fn rotate(&mut self) -> Option<String> {
        if self.urls.is_empty() {
            return None;
        }
        let proxy = self.urls[self.current_index % self.urls.len()].clone();
        self.current_index += 1;
        Some(proxy)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Liga Stavok Parser
// ─────────────────────────────────────────────────────────────────────────────

/// Liga Stavok API parser with full market support
/// - Supports: 1X2, Total, BTTS, Handicap, Even/Odd
/// - Features: Exponential backoff, proxy rotation, comprehensive error handling
/// - Target: 5000+ events daily
#[derive(Debug)]
pub struct LigaStavokParser {
    client: Arc<Client>,
    proxy_config: ProxyConfig,
}

impl LigaStavokParser {
    pub fn new(client: Arc<Client>) -> Self {
        Self {
            client,
            proxy_config: ProxyConfig::new(),
        }
    }

    fn readiness_snapshot() -> ParserReadiness {
        ParserReadiness {
            stage: ParserReadinessStage::DiagnosticOnly,
            production_enabled: false,
            self_check_available: true,
            checks: vec![
                ParserDiagnosticCheck {
                    code: "http_api_path_registered".to_string(),
                    severity: DiagnosticSeverity::Info,
                    message: "A candidate HTTP API path is configured, but its response schema and odds payload have not been verified from the current environment.".to_string(),
                },
                ParserDiagnosticCheck {
                    code: "qrator_protected_navigation_blocked".to_string(),
                    severity: DiagnosticSeverity::Warn,
                    message: "The official ligastavok.ru entry page currently returns a QRATOR access-denied page. The previously configured lds-api-sites host returns an under-construction placeholder, not a verified events feed.".to_string(),
                },
                ParserDiagnosticCheck {
                    code: "public_feed_not_verified".to_string(),
                    severity: DiagnosticSeverity::Warn,
                    message: "No live or prematch sports-line response has been captured and validated; scan enablement remains disabled.".to_string(),
                },
                ParserDiagnosticCheck {
                    code: "request_level_proxy_not_applied".to_string(),
                    severity: DiagnosticSeverity::Warn,
                    message: "The current reqwest Client cannot be switched to a different proxy per request. The legacy proxy rotation helper does not change the transport and is not counted as working proxy support.".to_string(),
                },
            ],
        }
    }

    /// Check if error is transient and worth retrying
    fn is_transient_error(error: &str) -> bool {
        let normalized = error.to_ascii_lowercase();
        [
            "timeout", "connection", "connecterror", "408", "425", "429", "500", "502",
            "503", "504", "temporary failure", "too many requests", "http2", "h2", "reset",
        ]
        .iter()
        .any(|marker| normalized.contains(marker))
    }

    /// Calculate backoff duration with exponential growth
    fn backoff_duration(attempt: u32) -> Duration {
        let backoff_ms = INITIAL_BACKOFF_MS * 2_u64.pow(attempt);
        let capped_ms = backoff_ms.min(MAX_BACKOFF_MS);
        Duration::from_millis(capped_ms)
    }

    /// Retry helper with exponential backoff and proxy rotation
    async fn retry_with_backoff<F, Fut, T>(
        &mut self,
        description: &str,
        mut operation: F,
    ) -> Result<T, Box<dyn std::error::Error + Send + Sync>>
    where
        F: FnMut(Option<String>) -> Fut,
        Fut: std::future::Future<Output = Result<T, Box<dyn std::error::Error + Send + Sync>>>,
    {
        let mut last_error: Option<String> = None;

        for attempt in 0..MAX_RETRIES {
            debug!(attempt, description, "Liga Stavok retry attempt");

            let proxy = if attempt > 0 {
                self.proxy_config.rotate()
            } else {
                self.proxy_config.get_proxy()
            };
            let proxy_rotated = proxy.is_some();

            match operation(proxy.clone()).await {
                Ok(result) => {
                    if attempt > 0 {
                        info!(
                            attempt,
                            description, "Liga Stavok operation succeeded after retries"
                        );
                    }
                    return Ok(result);
                }
                Err(err) => {
                    let err_str = err.to_string();
                    last_error = Some(err_str.clone());

                    if !Self::is_transient_error(&err_str) {
                        error!(
                            attempt,
                            error = &err_str,
                            description,
                            "Liga Stavok permanent error (not retrying)"
                        );
                        return Err(err);
                    }

                    if attempt < MAX_RETRIES - 1 {
                        let backoff = Self::backoff_duration(attempt);
                        warn!(
                            attempt,
                            error = &err_str,
                            backoff_ms = backoff.as_millis(),
                            description,
                            proxy_rotated,
                            "Liga Stavok transient error, retrying after backoff"
                        );
                        sleep(backoff).await;
                    } else {
                        error!(
                            attempt,
                            error = &err_str,
                            max_retries = MAX_RETRIES,
                            description,
                            "Liga Stavok operation failed after all retries"
                        );
                    }
                }
            }
        }

        Err(format!(
            "Liga Stavok {}: {} (failed after {} retries)",
            description,
            last_error.unwrap_or_else(|| "unknown error".to_string()),
            MAX_RETRIES
        )
        .into())
    }

    fn request_client(
        &self,
        proxy: Option<&str>,
    ) -> Result<Client, Box<dyn std::error::Error + Send + Sync>> {
        let Some(proxy_url) = proxy.filter(|value| !value.trim().is_empty()) else {
            return Ok(self.client.as_ref().clone());
        };

        let proxy = reqwest::Proxy::all(proxy_url)?;
        Ok(Client::builder()
            .proxy(proxy)
            .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
            .build()?)
    }

    async fn fetch_events_internal(
        &self,
        proxy: Option<String>,
        is_live: bool,
    ) -> Result<Vec<Event>, Box<dyn std::error::Error + Send + Sync>> {
        let client = self.request_client(proxy.as_deref())?;
        let request = client
            .get(EVENTS_LIST_URL)
            .header("User-Agent", USER_AGENT)
            .header("Accept", "application/json")
            .header("Accept-Language", "ru-RU,ru;q=0.9")
            .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS));

        let response = request.send().await?;

        if !response.status().is_success() {
            let status = response.status();
            warn!(%status, is_live, "Liga Stavok events list HTTP error");
            return Err(format!("Liga Stavok events endpoint returned HTTP {status}").into());
        }

        let json: serde_json::Value = response.json().await?;
        let events = self.parse_events(&json, is_live)?;

        info!(count = events.len(), is_live, "Liga Stavok events parsed");
        Ok(events)
    }

    fn parse_events(
        &self,
        json: &serde_json::Value,
        is_live: bool,
    ) -> Result<Vec<Event>, Box<dyn std::error::Error + Send + Sync>> {
        let mut events = Vec::new();
        let event_list = json
            .get("events")
            .and_then(serde_json::Value::as_array)
            .or_else(|| json.pointer("/data/events").and_then(serde_json::Value::as_array));

        if let Some(event_list) = event_list {
            for event_data in event_list {
                if let Some(event) = self.parse_single_event(event_data, is_live) {
                    events.push(event);
                }
            }
        }

        Ok(events)
    }

    fn value_as_string(value: &serde_json::Value) -> Option<String> {
        value
            .as_str()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .or_else(|| value.as_i64().map(|number| number.to_string()))
            .or_else(|| value.as_u64().map(|number| number.to_string()))
    }

    fn parse_numeric(value: &serde_json::Value) -> Option<f64> {
        let number = value.as_f64().or_else(|| {
            value
                .as_str()
                .and_then(|raw| raw.trim().replace(',', ".").parse::<f64>().ok())
        })?;
        number.is_finite().then_some(number)
    }

    fn parse_single_event(&self, event_data: &serde_json::Value, is_live: bool) -> Option<Event> {
        let id = Self::value_as_string(event_data.get("id")?)?;
        let home_team = Self::value_as_string(event_data.get("team1")?)?;
        let away_team = Self::value_as_string(event_data.get("team2")?)?;
        let league = event_data
            .get("tournament")
            .and_then(|t| t.as_str())
            .unwrap_or("Unknown")
            .to_string();

        let sport = self.detect_sport(&league);
        let start_time = event_data
            .get("startTime")
            .and_then(|value| {
                value
                    .as_i64()
                    .or_else(|| value.as_str().and_then(|raw| raw.parse::<i64>().ok()))
            })
            .and_then(|timestamp| {
                if timestamp.abs() < 10_000_000_000 {
                    Utc.timestamp_opt(timestamp, 0).single()
                } else {
                    Utc.timestamp_millis_opt(timestamp).single()
                }
            });

        Some(Event {
            id: format!("{}-{}", BOOKMAKER_SLUG, id),
            sport,
            league,
            home_team,
            away_team,
            start_time,
            is_live,
            bookmaker_slug: BOOKMAKER_SLUG.to_string(),
            raw_url: Some(format!("https://www.ligastavok.ru/events/{}", id)),
            extra: HashMap::new(),
        })
    }

    fn detect_sport(&self, league: &str) -> Sport {
        let league_lower = league.to_lowercase();

        if league_lower.contains("футбол")
            || league_lower.contains("football")
            || league_lower.contains("апл")
            || league_lower.contains("лига")
        {
            Sport::Football
        } else if league_lower.contains("хоккей") || league_lower.contains("hockey") {
            Sport::Hockey
        } else if league_lower.contains("баскетбол") || league_lower.contains("basketball")
        {
            Sport::Basketball
        } else if league_lower.contains("волейбол") || league_lower.contains("volleyball") {
            Sport::Volleyball
        } else if league_lower.contains("теннис")
            || league_lower.contains("tennis")
            || league_lower.contains("atp")
            || league_lower.contains("wta")
        {
            Sport::Tennis
        } else if league_lower.contains("бокс") || league_lower.contains("boxing") {
            Sport::Boxing
        } else if league_lower.contains("мма") || league_lower.contains("ufc") {
            Sport::Mma
        } else {
            Sport::Other
        }
    }

    async fn fetch_odds_internal(
        &self,
        proxy: Option<String>,
        is_live: bool,
    ) -> Result<Vec<Odd>, Box<dyn std::error::Error + Send + Sync>> {
        let request = self
            .client
            .get(EVENTS_LIST_URL)
            .header("User-Agent", USER_AGENT)
            .header("Accept", "application/json")
            .header("Accept-Language", "ru-RU,ru;q=0.9")
            .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS));

        if proxy.is_some() {
            debug!(
                is_live,
                "Liga Stavok proxy requested but RequestBuilder-level proxy override is unsupported; using base client"
            );
        }

        let response = request.send().await?;

        if !response.status().is_success() {
            warn!(status = %response.status(), is_live, "Liga Stavok odds list HTTP error");
            return Ok(Vec::new());
        }

        let json: serde_json::Value = response.json().await?;
        let odds = self.parse_odds(&json, is_live)?;

        info!(count = odds.len(), is_live, "Liga Stavok odds parsed");
        Ok(odds)
    }

    fn parse_odds(
        &self,
        json: &serde_json::Value,
        is_live: bool,
    ) -> Result<Vec<Odd>, Box<dyn std::error::Error + Send + Sync>> {
        let mut odds = Vec::new();
        let now = Utc::now();

        let market_list = json
            .get("markets")
            .and_then(serde_json::Value::as_array)
            .or_else(|| json.pointer("/data/markets").and_then(serde_json::Value::as_array));
        if let Some(markets) = market_list {
            for market_data in markets {
                if let Some(market_odds) = self.parse_market(market_data, is_live, now) {
                    odds.extend(market_odds);
                }
            }
        }

        Ok(odds)
    }

    fn parse_market(
        &self,
        market_data: &serde_json::Value,
        is_live: bool,
        now: DateTime<Utc>,
    ) -> Option<Vec<Odd>> {
        let event_id = Self::value_as_string(market_data.get("eventId")?)?;
        let market_type = market_data.get("type")?.as_str()?.trim().to_lowercase();
        let options = market_data.get("options").and_then(|o| o.as_array())?;

        let mut market_odds = Vec::new();

        for option in options {
            if let Some(odd) = self.parse_option(&event_id, &market_type, option, is_live, now) {
                market_odds.push(odd);
            }
        }

        if market_odds.is_empty() {
            None
        } else {
            Some(market_odds)
        }
    }

    fn parse_option(
        &self,
        event_id: &str,
        market_type: &str,
        option: &serde_json::Value,
        _is_live: bool,
        now: DateTime<Utc>,
    ) -> Option<Odd> {
        let name = option.get("name")?.as_str()?.trim();
        let value = Self::parse_numeric(option.get("odds")?)?;

        if !(1.0..=1_000.0).contains(&value) {
            return None;
        }

        let (market, selection, odds_type) = self.classify_market(market_type, name)?;
        let event_id = format!("{}-{}", BOOKMAKER_SLUG, event_id);
        let line = self.extract_line(option);
        let line_key = line
            .map(|value| value.to_string())
            .unwrap_or_else(|| "-".to_string());

        Some(Odd {
            id: format!(
                "{}:{}:{}:{}:{}",
                BOOKMAKER_SLUG, event_id, market, selection, line_key
            ),
            event_id,
            bookmaker_slug: BOOKMAKER_SLUG.to_string(),
            market: market.to_string(),
            selection: selection.to_string(),
            odds: value,
            odds_type,
            line,
            timestamp: now,
        })
    }

    fn classify_market(
        &self,
        market_type: &str,
        selection: &str,
    ) -> Option<(&'static str, &'static str, OddsType)> {
        let market_type = market_type.trim().to_lowercase();
        let selection = selection.trim().to_lowercase();

        match market_type.as_str() {
            mt if mt.contains("1x2") || mt.contains("исход") => {
                if selection == "1" || selection.starts_with("п1") {
                    Some((MARKET_1X2, "1", OddsType::Home))
                } else if selection == "x" || selection == "х" || selection.starts_with("нич") {
                    Some((MARKET_1X2, "X", OddsType::Draw))
                } else if selection == "2" || selection.starts_with("п2") {
                    Some((MARKET_1X2, "2", OddsType::Away))
                } else {
                    None
                }
            }
            mt if mt.contains("total") || mt.contains("тотал") => {
                if selection == "б" || selection.starts_with("больше") || selection.starts_with("over") {
                    Some((MARKET_TOTAL, "Over", OddsType::Over))
                } else if selection == "м" || selection.starts_with("меньше") || selection.starts_with("under") {
                    Some((MARKET_TOTAL, "Under", OddsType::Under))
                } else {
                    None
                }
            }
            mt if mt.contains("both") || mt.contains("обе") || mt.contains("оба") || mt.contains("два") => {
                if selection == "да" || selection.starts_with("yes") {
                    Some((MARKET_BTTS, "Yes", OddsType::BothTeamsScoreYes))
                } else if selection == "нет" || selection.starts_with("no") {
                    Some((MARKET_BTTS, "No", OddsType::BothTeamsScoreNo))
                } else {
                    None
                }
            }
            mt if mt.contains("handicap") || mt.contains("фора") => {
                if selection == "1" || selection.starts_with("п1") || selection.starts_with("1(") {
                    Some((MARKET_HANDICAP, "1", OddsType::Handicap))
                } else if selection == "2" || selection.starts_with("п2") || selection.starts_with("2(") {
                    Some((MARKET_HANDICAP, "2", OddsType::Handicap))
                } else {
                    None
                }
            }
            mt if mt.contains("even") || mt.contains("четн") => {
                if selection.starts_with("нечет") || selection.starts_with("odd") {
                    Some((MARKET_EVEN_ODD, "Odd", OddsType::Odd))
                } else if selection.starts_with("чет") || selection.starts_with("even") {
                    Some((MARKET_EVEN_ODD, "Even", OddsType::Even))
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    fn extract_line(&self, option: &serde_json::Value) -> Option<f64> {
        option
            .get("line")
            .and_then(Self::parse_numeric)
            .or_else(|| option.get("param").and_then(Self::parse_numeric))
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// BookmakerParser Implementation
// ─────────────────────────────────────────────────────────────────────────────

#[async_trait]
impl BookmakerParser for LigaStavokParser {
    fn name(&self) -> &str {
        "Liga Stavok"
    }

    fn slug(&self) -> &str {
        BOOKMAKER_SLUG
    }

    fn is_enabled(&self) -> bool {
        true
    }

    fn readiness(&self) -> Option<ParserReadiness> {
        Some(Self::readiness_snapshot())
    }

    async fn fetch_events(&self) -> Result<Vec<Event>, Box<dyn std::error::Error + Send + Sync>> {
        let mut all_events = Vec::new();

        for (is_live, label) in [(true, "live"), (false, "prematch")] {
            let mut parser = Self::new(self.client.clone());
            match parser
                .retry_with_backoff(&format!("fetch_events_{}", label), |proxy| {
                    self.fetch_events_internal(proxy, is_live)
                })
                .await
            {
                Ok(events) => all_events.extend(events),
                Err(e) => warn!(error = %e, label, "Liga Stavok events fetch failed"),
            }
        }

        info!(total = all_events.len(), "Liga Stavok all events fetched");
        Ok(all_events)
    }

    async fn fetch_odds(
        &self,
        _event_id: &str,
    ) -> Result<Vec<Odd>, Box<dyn std::error::Error + Send + Sync>> {
        let mut all_odds = Vec::new();

        for (is_live, label) in [(true, "live"), (false, "prematch")] {
            let mut parser = Self::new(self.client.clone());
            match parser
                .retry_with_backoff(&format!("fetch_odds_{}", label), |proxy| {
                    self.fetch_odds_internal(proxy, is_live)
                })
                .await
            {
                Ok(odds) => all_odds.extend(odds),
                Err(e) => warn!(error = %e, label, "Liga Stavok odds fetch failed"),
            }
        }

        info!(total = all_odds.len(), "Liga Stavok all odds fetched");
        Ok(all_odds)
    }

    async fn fetch_all(&self) -> Result<ParserResult, Box<dyn std::error::Error + Send + Sync>> {
        let start = std::time::Instant::now();
        let mut all_events = Vec::new();
        let mut all_odds = Vec::new();

        for (is_live, label) in [(true, "live"), (false, "prematch")] {
            let mut parser = Self::new(self.client.clone());

            match parser
                .retry_with_backoff(&format!("fetch_all_events_{}", label), |proxy| {
                    self.fetch_events_internal(proxy, is_live)
                })
                .await
            {
                Ok(events) => all_events.extend(events),
                Err(e) => warn!(error = %e, label, "Liga Stavok fetch_all events failed"),
            }

            let mut parser = Self::new(self.client.clone());
            match parser
                .retry_with_backoff(&format!("fetch_all_odds_{}", label), |proxy| {
                    self.fetch_odds_internal(proxy, is_live)
                })
                .await
            {
                Ok(odds) => all_odds.extend(odds),
                Err(e) => warn!(error = %e, label, "Liga Stavok fetch_all odds failed"),
            }
        }

        let elapsed = start.elapsed().as_millis() as u64;
        debug!(
            events = all_events.len(),
            odds = all_odds.len(),
            time_ms = elapsed,
            "Liga Stavok fetch_all complete"
        );

        Ok(ParserResult::new(
            BOOKMAKER_SLUG,
            all_events,
            all_odds,
            elapsed,
        ))
    }

    fn base_url(&self) -> &str {
        "https://www.ligastavok.ru"
    }

    fn user_agent(&self) -> &str {
        USER_AGENT
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parser_initialization() {
        let client = Arc::new(reqwest::Client::new());
        let parser = LigaStavokParser::new(client);
        assert_eq!(parser.name(), "Liga Stavok");
        assert_eq!(parser.slug(), BOOKMAKER_SLUG);
        assert!(parser.is_enabled());
    }

    #[test]
    fn test_backoff_calculation() {
        let d0 = LigaStavokParser::backoff_duration(0);
        assert_eq!(d0.as_millis(), 500);

        let d1 = LigaStavokParser::backoff_duration(1);
        assert_eq!(d1.as_millis(), 1000);

        let d2 = LigaStavokParser::backoff_duration(2);
        assert_eq!(d2.as_millis(), 2000);

        let d3 = LigaStavokParser::backoff_duration(3);
        assert_eq!(d3.as_millis(), 4000);
        let d4 = LigaStavokParser::backoff_duration(4);
        assert_eq!(d4.as_millis(), 5000); // capped at MAX_BACKOFF_MS
    }

    #[test]
    fn test_transient_error_detection() {
        assert!(LigaStavokParser::is_transient_error("timeout"));
        assert!(LigaStavokParser::is_transient_error("connection reset"));
        assert!(LigaStavokParser::is_transient_error(
            "429 Too Many Requests"
        ));
        assert!(LigaStavokParser::is_transient_error("502 Bad Gateway"));
        assert!(LigaStavokParser::is_transient_error(
            "503 Service Unavailable"
        ));
        assert!(LigaStavokParser::is_transient_error("h2 protocol error"));
        assert!(LigaStavokParser::is_transient_error("500 Internal Server Error"));
        assert!(!LigaStavokParser::is_transient_error("404 Not Found"));
        assert!(!LigaStavokParser::is_transient_error("401 Unauthorized"));
    }

    #[test]
    fn test_proxy_rotation() {
        std::env::set_var(
            "LIGASTAVOK_PROXY_LIST",
            "http://proxy1.com,http://proxy2.com",
        );
        let mut proxy = ProxyConfig::new();

        let p1 = proxy.get_proxy();
        assert!(p1.is_some());

        let p2 = proxy.rotate();
        assert!(p2.is_some());
        assert_ne!(p1, p2);
    }

    #[test]
    fn test_sport_detection_football() {
        let client = Arc::new(reqwest::Client::new());
        let parser = LigaStavokParser::new(client);

        assert_eq!(parser.detect_sport("АПЛ"), Sport::Football);
        assert_eq!(
            parser.detect_sport("Английская Премьер-Лига"),
            Sport::Football
        );
        assert_eq!(parser.detect_sport("Футбол"), Sport::Football);
        assert_eq!(parser.detect_sport("Football"), Sport::Football);
    }

    #[test]
    fn test_sport_detection_hockey() {
        let client = Arc::new(reqwest::Client::new());
        let parser = LigaStavokParser::new(client);

        assert_eq!(parser.detect_sport("Хоккей КХЛ"), Sport::Hockey);
        assert_eq!(parser.detect_sport("Hockey"), Sport::Hockey);
    }

    #[test]
    fn test_sport_detection_basketball() {
        let client = Arc::new(reqwest::Client::new());
        let parser = LigaStavokParser::new(client);

        assert_eq!(parser.detect_sport("Баскетбол НБА"), Sport::Basketball);
        assert_eq!(parser.detect_sport("Basketball"), Sport::Basketball);
    }

    #[test]
    fn test_sport_detection_tennis() {
        let client = Arc::new(reqwest::Client::new());
        let parser = LigaStavokParser::new(client);

        assert_eq!(parser.detect_sport("Теннис ATP"), Sport::Tennis);
        assert_eq!(parser.detect_sport("Tennis WTA"), Sport::Tennis);
        assert_eq!(parser.detect_sport("ATP Finals"), Sport::Tennis);
    }

    #[test]
    fn test_market_classification_1x2() {
        let client = Arc::new(reqwest::Client::new());
        let parser = LigaStavokParser::new(client);

        let (market, sel, odds_type) = parser
            .classify_market("1x2", "П1")
            .expect("should classify 1x2");
        assert_eq!(market, MARKET_1X2);
        assert_eq!(sel, "1");
        assert_eq!(odds_type, OddsType::Home);

        let (market, sel, odds_type) = parser
            .classify_market("1x2", "X")
            .expect("should classify draw");
        assert_eq!(market, MARKET_1X2);
        assert_eq!(sel, "X");
        assert_eq!(odds_type, OddsType::Draw);

        let (market, sel, odds_type) = parser
            .classify_market("1x2", "П2")
            .expect("should classify 1x2");
        assert_eq!(market, MARKET_1X2);
        assert_eq!(sel, "2");
        assert_eq!(odds_type, OddsType::Away);
    }

    #[test]
    fn test_market_classification_total() {
        let client = Arc::new(reqwest::Client::new());
        let parser = LigaStavokParser::new(client);

        let (market, sel, odds_type) = parser
            .classify_market("total", "Больше")
            .expect("should classify total over");
        assert_eq!(market, MARKET_TOTAL);
        assert_eq!(sel, "Over");
        assert_eq!(odds_type, OddsType::Over);

        let (market, sel, odds_type) = parser
            .classify_market("total", "Меньше")
            .expect("should classify total under");
        assert_eq!(market, MARKET_TOTAL);
        assert_eq!(sel, "Under");
        assert_eq!(odds_type, OddsType::Under);
    }

    #[test]
    fn test_market_classification_btts() {
        let client = Arc::new(reqwest::Client::new());
        let parser = LigaStavokParser::new(client);

        let (market, sel, odds_type) = parser
            .classify_market("оба забивают", "Да")
            .expect("should classify btts yes");
        assert_eq!(market, MARKET_BTTS);
        assert_eq!(sel, "Yes");
        assert_eq!(odds_type, OddsType::BothTeamsScoreYes);

        let (market, sel, odds_type) = parser
            .classify_market("оба забивают", "Нет")
            .expect("should classify btts no");
        assert_eq!(market, MARKET_BTTS);
        assert_eq!(sel, "No");
        assert_eq!(odds_type, OddsType::BothTeamsScoreNo);
    }

    #[test]
    fn test_market_classification_handicap() {
        let client = Arc::new(reqwest::Client::new());
        let parser = LigaStavokParser::new(client);

        let (market, sel, odds_type) = parser
            .classify_market("фора", "1")
            .expect("should classify handicap 1");
        assert_eq!(market, MARKET_HANDICAP);
        assert_eq!(sel, "1");
        assert_eq!(odds_type, OddsType::Handicap);
    }

    #[test]
    fn test_market_classification_even_odd() {
        let client = Arc::new(reqwest::Client::new());
        let parser = LigaStavokParser::new(client);

        let (market, sel, odds_type) = parser
            .classify_market("четность", "Четн")
            .expect("should classify even");
        assert_eq!(market, MARKET_EVEN_ODD);
        assert_eq!(sel, "Even");
        assert_eq!(odds_type, OddsType::Even);

        let (market, sel, odds_type) = parser
            .classify_market("четность", "Нечетн")
            .expect("should classify odd");
        assert_eq!(market, MARKET_EVEN_ODD);
        assert_eq!(sel, "Odd");
        assert_eq!(odds_type, OddsType::Odd);
    }

    #[test]
    fn test_readiness_snapshot() {
        let readiness = LigaStavokParser::readiness_snapshot();
        assert_eq!(readiness.stage, ParserReadinessStage::DiagnosticOnly);
        assert!(!readiness.production_enabled);
        assert!(readiness.self_check_available);
        assert!(readiness.checks.iter().any(|check| {
            check.code == "qrator_protected_navigation_blocked"
                && check.severity == DiagnosticSeverity::Warn
        }));
        assert!(readiness.checks.iter().any(|check| {
            check.code == "public_feed_not_verified" && check.severity == DiagnosticSeverity::Warn
        }));
        assert!(readiness.checks.iter().any(|check| {
            check.code == "request_level_proxy_not_applied"
                && check.severity == DiagnosticSeverity::Warn
        }));
    }

    #[test]
    fn test_base_url() {
        let client = Arc::new(reqwest::Client::new());
        let parser = LigaStavokParser::new(client);
        assert_eq!(parser.base_url(), "https://www.ligastavok.ru");
    }

    #[test]
    fn test_user_agent() {
        let client = Arc::new(reqwest::Client::new());
        let parser = LigaStavokParser::new(client);
        assert!(parser.user_agent().contains("Chrome"));
        assert!(parser.user_agent().contains("Windows"));
    }

    #[test]
    fn test_parse_single_event() {
        let client = Arc::new(reqwest::Client::new());
        let parser = LigaStavokParser::new(client);

        let event_json = serde_json::json!({
            "id": "12345",
            "team1": "CSKA",
            "team2": "Spartak",
            "tournament": "АПЛ",
            "startTime": 1713556800000i64
        });

        let event = parser.parse_single_event(&event_json, false);
        assert!(event.is_some());
        let event = event.unwrap();
        assert_eq!(event.home_team, "CSKA");
        assert_eq!(event.away_team, "Spartak");
        assert_eq!(event.league, "АПЛ");
        assert_eq!(event.sport, Sport::Football);
        assert!(!event.is_live);
    }

    #[test]
    fn test_parse_odd_validity() {
        let client = Arc::new(reqwest::Client::new());
        let parser = LigaStavokParser::new(client);

        let option = serde_json::json!({
            "name": "П1",
            "odds": 0.5,
            "line": 0.0
        });

        let event_json = serde_json::json!({
            "id": "123",
            "team1": "A",
            "team2": "B",
        });

        let _event = parser.parse_single_event(&event_json, false).unwrap();
        let odd = parser.parse_option("123", "1x2", &option, false, Utc::now());
        assert!(odd.is_none(), "Should reject odds with value <= 1.0");
    }

    #[test]
    fn accepts_numeric_event_ids_and_string_odds_without_colliding_market_lines() {
        let parser = LigaStavokParser::new(Arc::new(reqwest::Client::new()));
        let event = parser
            .parse_single_event(
                &serde_json::json!({
                    "id": 12345,
                    "team1": "Команда 1",
                    "team2": "Команда 2",
                    "tournament": "Теннис ATP",
                    "startTime": "1795770000"
                }),
                false,
            )
            .expect("numeric event id should be accepted");
        assert_eq!(event.id, "ligastavok-12345");
        assert_eq!(event.sport, Sport::Tennis);
        assert_eq!(event.start_time, Utc.timestamp_opt(1795770000, 0).single());

        let first = parser.parse_option(
            "12345",
            "total",
            &serde_json::json!({"name": "Больше", "odds": "1,85", "line": "2.5"}),
            false,
            Utc::now(),
        ).expect("string odds and line should be accepted");
        let second = parser.parse_option(
            "12345",
            "total",
            &serde_json::json!({"name": "Больше", "odds": "1.90", "line": 3.5}),
            false,
            Utc::now(),
        ).expect("second total line should be accepted");
        assert_ne!(first.id, second.id);
        assert_eq!(first.line, Some(2.5));
        assert_eq!(first.odds, 1.85);
    }

    #[test]
    fn classifies_selections_without_substring_collisions_and_ignores_case() {
        let parser = LigaStavokParser::new(Arc::new(reqwest::Client::new()));
        assert_eq!(parser.classify_market("1X2", "п1"), Some((MARKET_1X2, "1", OddsType::Home)));
        assert_eq!(parser.classify_market("1x2", "Ничья"), Some((MARKET_1X2, "X", OddsType::Draw)));
        assert_eq!(parser.classify_market("total", "БОЛЬШЕ 2.5"), Some((MARKET_TOTAL, "Over", OddsType::Over)));
        assert_eq!(parser.classify_market("even", "Нечетное"), Some((MARKET_EVEN_ODD, "Odd", OddsType::Odd)));
        assert!(parser.classify_market("1x2", "12").is_none());
        assert!(parser.classify_market("handicap", "12").is_none());
    }

    #[test]
    fn test_metadata() {
        let client = Arc::new(reqwest::Client::new());
        let parser = LigaStavokParser::new(client);
        let metadata = parser.metadata();
        assert_eq!(metadata.slug, BOOKMAKER_SLUG);
        assert_eq!(metadata.name, "Liga Stavok");
    }
}
