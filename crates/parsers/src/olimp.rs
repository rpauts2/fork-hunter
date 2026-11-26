use crate::base::{BookmakerParser, ParserResult};
use crate::circuit_breaker::CircuitBreaker;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use reqwest::Client;
use shared::odds::OddsType;
use shared::{
    DiagnosticSeverity, Event, Odd, ParserDiagnosticCheck, ParserReadiness, ParserReadinessStage,
    Sport,
};
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, info, warn};

/// Olimp API parser v4 with bounded retries and a circuit breaker.
/// Access-control responses are surfaced without proxy rotation or retries.
///
/// API: olimp.bet/api/v4/{sport_id}/live|line/sports-with-competitions-with-events
pub struct OlimpParser {
    client: Arc<Client>,
    base_api_url: String,
    circuit_breaker: Arc<CircuitBreaker>,
}

impl fmt::Debug for OlimpParser {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OlimpParser")
            .field("base_api_url", &self.base_api_url)
            .finish()
    }
}

const RUNTIME_PROBE_AT: &str = "2026-11-26 11:04 UTC";
const RUNTIME_PROBE_LIVE_EVENTS: usize = 164;
const RUNTIME_PROBE_PREMATCH_EVENTS: usize = 1509;

// Retry configuration
const MAX_RETRIES: u32 = 3;
const INITIAL_BACKOFF_MS: u64 = 100;
const MAX_BACKOFF_MS: u64 = 5000;
const BACKOFF_MULTIPLIER: f64 = 2.0;
// Sport ID 0 is the bookmaker's all-sports catalogue. The currently exposed
// sport-specific IDs return empty payloads on this endpoint; sweeping them
// adds latency without increasing coverage.
const PREMATCH_SPORT_ID_SWEEP: &[u32] = &[0];
const REQUEST_TIMEOUT_SECS: u64 = 12;
const LIVE_SECTION_BUDGET_SECS: u64 = 24;
// One all-sports catalogue request is authoritative; keep room for retries
// without spending the runtime budget on empty per-sport endpoints.
const PREMATCH_SECTION_BUDGET_SECS: u64 = 45;
const PREMATCH_TARGET_EVENTS: usize = 3500;

impl OlimpParser {
    pub fn new(client: Arc<Client>) -> Self {
        Self {
            client,
            base_api_url: "https://www.olimp.bet/api/v4".to_string(),
            circuit_breaker: Arc::new(CircuitBreaker::new(
                3,  // failure_threshold
                60, // recovery_timeout_secs
                2,  // half_open_max
            )),
        }
    }

    fn readiness_snapshot() -> ParserReadiness {
        ParserReadiness {
            stage: ParserReadinessStage::RolloutReady,
            production_enabled: false,
            self_check_available: true,
            checks: vec![
                ParserDiagnosticCheck {
                    code: "competitions_with_events_runtime_path_enabled".to_string(),
                    severity: DiagnosticSeverity::Pass,
                    message: "Olimp is re-enabled in ParserFactory through the direct /api/v4/0/live and /api/v4/0/line/top competitions-with-events JSON path instead of the old disabled/runtime-unknown state.".to_string(),
                },
                ParserDiagnosticCheck {
                    code: "historical_runtime_sample_observed".to_string(),
                    severity: DiagnosticSeverity::Pass,
                    message: format!(
                        "A historical direct-transport sample at {RUNTIME_PROBE_AT} observed {RUNTIME_PROBE_LIVE_EVENTS} live and {RUNTIME_PROBE_PREMATCH_EVENTS} prematch events with odds. This is evidence of a successful sample, not a live availability guarantee."
                    ),
                },
                ParserDiagnosticCheck {
                    code: "access_controls_respected".to_string(),
                    severity: DiagnosticSeverity::Pass,
                    message: "Olimp HTTP 401/403/429 responses are surfaced as access/rate restrictions; the parser does not rotate proxies or retry those responses.".to_string(),
                },
                ParserDiagnosticCheck {
                    code: "production_volume_still_unlocked".to_string(),
                    severity: DiagnosticSeverity::Warn,
                    message: "The new runtime truth proves the public live/prematch path is real, but production promotion stays off until strict Rust-side diagnostics establish repeatable event and odds volume under normal scanner execution.".to_string(),
                },
            ],
        }
    }

    fn section_url(&self, sport_id: u32, section: &str) -> String {
        format!(
            "{}/{}/{}/sports-with-competitions-with-events?vids%5B%5D=",
            self.base_api_url, sport_id, section
        )
    }
}

#[async_trait]
impl BookmakerParser for OlimpParser {
    fn name(&self) -> &str {
        "Olimp"
    }

    fn slug(&self) -> &str {
        "olimp"
    }

    fn is_enabled(&self) -> bool {
        true
    }

    fn readiness(&self) -> Option<ParserReadiness> {
        Some(Self::readiness_snapshot())
    }

    async fn fetch_events(&self) -> Result<Vec<Event>, Box<dyn std::error::Error + Send + Sync>> {
        let mut all_events = Vec::new();
        for (section, is_live) in [("live", true), ("line/top", false)] {
            match self.fetch_section(section, is_live).await {
                Ok((events, _)) => all_events.extend(events),
                Err(e) => warn!(error = %e, "Olimp fetch events failed"),
            }
        }
        info!(count = all_events.len(), "Olimp events parsed");
        Ok(all_events)
    }

    async fn fetch_odds(
        &self,
        _event_id: &str,
    ) -> Result<Vec<Odd>, Box<dyn std::error::Error + Send + Sync>> {
        let mut all_odds = Vec::new();
        for (section, is_live) in [("live", true), ("line/top", false)] {
            match self.fetch_section(section, is_live).await {
                Ok((_, odds)) => all_odds.extend(odds),
                Err(e) => warn!(error = %e, "Olimp fetch odds failed"),
            }
        }
        Ok(all_odds)
    }

    async fn fetch_all(&self) -> Result<ParserResult, Box<dyn std::error::Error + Send + Sync>> {
        let start = std::time::Instant::now();
        let mut all_events = Vec::new();
        let mut all_odds = Vec::new();
        let mut seen_events = HashSet::new();
        let mut seen_odds = HashSet::new();

        // These are the only two confirmed public catalogue sections. Older
        // /line and /line/all probes return HTTP 400/404 and duplicate neither
        // events nor prices; avoid spending every runtime snapshot on them.
        let live_fut = self.fetch_section("live", true);
        let prematch_fut = self.fetch_section("line/top", false);
        let (live_res, prematch_res) = tokio::join!(live_fut, prematch_fut);

        let results = [("live", true, live_res), ("line/top", true, prematch_res)];
        let mut mandatory_errors = Vec::new();
        for (section, mandatory, result) in results {
            match result {
                Ok((events, odds)) => {
                    for event in events {
                        if seen_events.insert(event.id.clone()) {
                            all_events.push(event);
                        }
                    }
                    for odd in odds {
                        if seen_odds.insert(odd.id.clone()) {
                            all_odds.push(odd);
                        }
                    }
                }
                Err(error) if mandatory => {
                    mandatory_errors.push(format!("{section}: {error}"));
                }
                Err(error) => {
                    warn!(section, error = %error, "Olimp optional feed failed");
                }
            }
        }

        if !mandatory_errors.is_empty() {
            return Err(std::io::Error::other(mandatory_errors.join("; ")).into());
        }

        let elapsed = start.elapsed().as_millis() as u64;
        info!(
            events = all_events.len(),
            odds = all_odds.len(),
            time_ms = elapsed,
            "Olimp fetch complete"
        );
        Ok(ParserResult::new("olimp", all_events, all_odds, elapsed))
    }

    fn base_url(&self) -> &str {
        "https://www.olimp.bet"
    }

    fn user_agent(&self) -> &str {
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36"
    }
}

impl OlimpParser {
    /// Fetch section with bounded retries for transient failures.
    async fn fetch_section(
        &self,
        section: &str,
        is_live: bool,
    ) -> Result<(Vec<Event>, Vec<Odd>), Box<dyn std::error::Error + Send + Sync>> {
        if !self.circuit_breaker.allow_request() {
            return Err("Circuit breaker is open - service temporarily unavailable".into());
        }

        let mut all_events = Vec::new();
        let mut all_odds = Vec::new();
        let mut seen_events = HashSet::new();
        let mut seen_odds = HashSet::new();
        let section_started = std::time::Instant::now();
        let mut last_error: Option<String> = None;
        let section_budget = Duration::from_secs(if is_live {
            LIVE_SECTION_BUDGET_SECS
        } else {
            PREMATCH_SECTION_BUDGET_SECS
        });
        let sport_ids: Vec<u32> = if is_live {
            vec![0]
        } else {
            PREMATCH_SPORT_ID_SWEEP.to_vec()
        };

        for sport_id in sport_ids {
            if section_started.elapsed() >= section_budget {
                warn!(
                    section = section,
                    is_live,
                    events = all_events.len(),
                    odds = all_odds.len(),
                    "Olimp: section budget exhausted, returning partial snapshot"
                );
                break;
            }

            for attempt in 0..MAX_RETRIES {
                match self.fetch_section_direct(sport_id, section, is_live).await {
                    Ok((events, odds)) => {
                        self.circuit_breaker.record_success();
                        for event in events {
                            if seen_events.insert(event.id.clone()) {
                                all_events.push(event);
                            }
                        }
                        for odd in odds {
                            if seen_odds.insert(odd.id.clone()) {
                                all_odds.push(odd);
                            }
                        }
                        if !is_live && all_events.len() >= PREMATCH_TARGET_EVENTS {
                            debug!(
                                section = section,
                                events = all_events.len(),
                                "Olimp: reached prematch target budget, stopping sweep early"
                            );
                            return Ok((all_events, all_odds));
                        }
                        break;
                    }
                    Err(e) => {
                        last_error = Some(e.to_string());
                        self.circuit_breaker.record_failure();

                        if !is_retryable_error(e.as_ref()) {
                            warn!(
                                error = %e,
                                section = section,
                                sport_id = sport_id,
                                "Olimp: access/rate restriction; stopping retries"
                            );
                            break;
                        }

                        if attempt < MAX_RETRIES - 1 {
                            let backoff_ms = ((INITIAL_BACKOFF_MS as f64)
                                * BACKOFF_MULTIPLIER.powi(attempt as i32))
                            .min(MAX_BACKOFF_MS as f64)
                                as u64;

                            warn!(
                                error = %e,
                                section = section,
                                sport_id = sport_id,
                                attempt = attempt + 1,
                                backoff_ms = backoff_ms,
                                "Olimp: fetch failed, retrying in {}ms",
                                backoff_ms
                            );
                            tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
                        } else {
                            warn!(
                                error = %e,
                                section = section,
                                sport_id = sport_id,
                                "Olimp: fetch failed after retries for sport_id"
                            );
                        }
                    }
                }
            }
        }

        if all_events.is_empty() && all_odds.is_empty() {
            if let Some(error) = last_error {
                return Err(std::io::Error::other(format!(
                    "Olimp {section} feed failed for all requested sport IDs: {error}"
                ))
                .into());
            }
        }

        Ok((all_events, all_odds))
    }

    /// Fetch only through the configured client. Do not attempt alternate
    /// network identities when the bookmaker refuses access.
    async fn fetch_section_direct(
        &self,
        sport_id: u32,
        section: &str,
        is_live: bool,
    ) -> Result<(Vec<Event>, Vec<Odd>), Box<dyn std::error::Error + Send + Sync>> {
        let url = self.section_url(sport_id, section);
        debug!(url = url, "Olimp: fetching section");
        let text = self.execute_request(&url).await?;
        self.parse_response(&text, is_live)
    }

    /// Execute a request using the caller-configured HTTP client.
    async fn execute_request(
        &self,
        url: &str,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let resp = self
            .client
            .get(url)
            .header(
                "User-Agent",
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36",
            )
            .header("Accept", "application/json, text/plain, */*")
            .header("Accept-Language", "ru-RU,ru;q=0.9")
            .header("Referer", "https://www.olimp.bet/")
            .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            return Err(format!(
                "HTTP {}: {}",
                status.as_u16(),
                status.canonical_reason().unwrap_or("Unknown")
            )
            .into());
        }

        Ok(resp.text().await?)
    }

    /// Parse response JSON
    fn parse_response(
        &self,
        text: &str,
        is_live: bool,
    ) -> Result<(Vec<Event>, Vec<Odd>), Box<dyn std::error::Error + Send + Sync>> {
        debug!(response_length = text.len(), "Olimp: received data");

        let json: serde_json::Value = serde_json::from_str(text).map_err(|e| {
            debug!(error = %e, "Olimp: JSON parse failed");
            format!("JSON parse error: {}", e)
        })?;

        Self::parse_api_response_all_sports(&json, is_live)
    }

    fn parse_api_response_all_sports(
        json: &serde_json::Value,
        is_live: bool,
    ) -> Result<(Vec<Event>, Vec<Odd>), Box<dyn std::error::Error + Send + Sync>> {
        let mut events = Vec::new();
        let mut all_odds = Vec::new();
        let now = Utc::now();

        let sports_array = json
            .as_array()
            .ok_or_else(|| std::io::Error::other("Olimp API response is not a sports array"))?;

        for sport_obj in sports_array {
            let payload = sport_obj
                .get("payload")
                .ok_or_else(|| std::io::Error::other("Olimp API sport item is missing payload"))?;

            let sport_info = payload.get("sport").unwrap_or(&serde_json::Value::Null);
            // The public API has returned both string and numeric sport IDs.
            // Treating numeric IDs as "0" silently classified valid events as Other.
            let sport_id = sport_info
                .get("id")
                .and_then(Self::parse_identifier)
                .unwrap_or_else(|| "0".to_string());
            let sport_name = sport_info
                .get("name")
                .or_else(|| sport_info.get("names").and_then(|n| n.get("0")))
                .and_then(|v| v.as_str())
                .unwrap_or("");

            let sport = Self::sport_from_api(&sport_id, sport_name);

            let competitions = payload
                .get("competitionsWithEvents")
                .and_then(|value| value.as_array())
                .ok_or_else(|| {
                    std::io::Error::other(
                        "Olimp API payload is missing competitionsWithEvents array",
                    )
                })?;

            for comp in competitions {
                let league_name = Self::competition_name(comp);

                if let Some(events_array) = comp.get("events").and_then(|e| e.as_array()) {
                    for event_data in events_array {
                        let (event_id, home, away) = match Self::extract_event_info(event_data) {
                            Some(info) => info,
                            None => continue,
                        };

                        let event_key = format!(
                            "olimp-{}-{}",
                            if is_live { "live" } else { "prematch" },
                            event_id
                        );

                        let event = Event {
                            id: event_key.clone(),
                            sport,
                            league: if !league_name.is_empty() {
                                league_name.to_string()
                            } else {
                                sport_name.to_string()
                            },
                            home_team: home.clone(),
                            away_team: away.clone(),
                            start_time: Self::parse_start_time(event_data),
                            is_live,
                            bookmaker_slug: "olimp".to_string(),
                            raw_url: None,
                            extra: HashMap::new(),
                        };
                        let mut event_odds = Vec::new();

                        if let Some(outcomes) =
                            event_data.get("outcomes").and_then(|o| o.as_array())
                        {
                            for outcome in outcomes {
                                if let Some(selection) = outcome
                                    .get("shortName")
                                    .and_then(|value| value.as_str())
                                    .filter(|value| !value.trim().is_empty())
                                {
                                    let probability =
                                        outcome.get("probability").and_then(|value| {
                                            value.as_f64().or_else(|| {
                                                value.as_str().and_then(|text| text.parse().ok())
                                            })
                                        });
                                    if let Some(prob) = probability
                                        .filter(|value| value.is_finite() && *value > 1.0)
                                    {
                                        let Some(market) = outcome
                                            .get("groupName")
                                            .and_then(|value| value.as_str())
                                            .filter(|value| !value.trim().is_empty())
                                        else {
                                            continue;
                                        };
                                        let odds_type =
                                            Self::selection_to_odds_type(selection, market);
                                        let line = outcome.get("param").and_then(|value| {
                                            value.as_f64().or_else(|| {
                                                value.as_str().and_then(|text| text.parse().ok())
                                            })
                                        });
                                        let line_key = line
                                            .map(|value| {
                                                format!("{value:.6}")
                                                    .replace('-', "m")
                                                    .replace('.', "p")
                                            })
                                            .unwrap_or_else(|| "no-line".to_string());

                                        event_odds.push(Odd {
                                            id: format!(
                                                "{}-{}-{}-{}",
                                                event_key, market, selection, line_key
                                            ),
                                            event_id: event_key.clone(),
                                            bookmaker_slug: "olimp".to_string(),
                                            market: market.to_string(),
                                            selection: selection.to_string(),
                                            odds: prob,
                                            odds_type,
                                            line,
                                            timestamp: now,
                                        });
                                    }
                                }
                            }
                        }

                        // Olimp also returns informational/statistics/result cards
                        // in the competitions endpoint. They are not actionable
                        // betting events when the provider supplies no priced
                        // outcome. Do not inflate scanner coverage with them.
                        if !event_odds.is_empty() {
                            events.push(event);
                            all_odds.extend(event_odds);
                        }
                    }
                }
            }
        }

        debug!(
            sports = sports_array.len(),
            events = events.len(),
            odds = all_odds.len(),
            "Olimp: parsed"
        );
        Ok((events, all_odds))
    }

    fn sport_from_api(source_id: &str, source_name: &str) -> Sport {
        let normalized = source_name.trim().to_lowercase();
        if normalized.is_empty() {
            return source_id
                .trim()
                .parse::<u32>()
                .map(Self::sport_id_to_sport)
                .unwrap_or(Sport::Other);
        }

        match normalized.as_str() {
            "авто/мотоспорт" | "формула 1" | "автогонки" | "мотогонки" => {
                Sport::Motorsport
            }
            "регби-союз" | "регби-лига" => Sport::Rugby,
            "австралийский футбол" => Sport::AussieRules,
            "футзал" => Sport::Futsal,
            "кулачные бои" | "бои" => Sport::Mma,
            "падел-теннис" => Sport::Tennis,
            "киберфутбол" | "кибербаскетбол" | "киберхоккей" | "киберспорт" => {
                Sport::Esports
            }
            "кикбоксинг" | "тайский бокс" => Sport::Boxing,
            "горные лыжи" | "биатлон" | "фристайл" | "лыжи" => {
                Sport::WinterSports
            }
            "хоккей с мячом"
            | "американский футбол"
            | "бильярд"
            | "шахматы"
            | "вольная борьба"
            | "херлинг"
            | "пул"
            | "олимпиада" => Sport::Other,
            _ => Sport::from_str(source_name),
        }
    }

    /// Fallback for older or incomplete API responses that omit sport.name.
    /// These IDs are taken from the current public Olimp catalogue snapshot.
    fn sport_id_to_sport(id: u32) -> Sport {
        match id {
            1 => Sport::Football,
            2 => Sport::Hockey,
            3 | 148 => Sport::Tennis,
            5 => Sport::Basketball,
            7 | 75 => Sport::WinterSports,
            9 => Sport::Handball,
            10 => Sport::Volleyball,
            11 => Sport::Futsal,
            12 => Sport::Boxing,
            29 => Sport::Baseball,
            33 | 117 => Sport::Motorsport,
            40 => Sport::TableTennis,
            41 => Sport::WaterPolo,
            44 => Sport::Cycling,
            46 | 85 => Sport::Rugby,
            47 => Sport::Darts,
            51 => Sport::Badminton,
            73 => Sport::Cricket,
            91 => Sport::AussieRules,
            96 | 143 => Sport::Mma,
            112 | 138 | 140 | 141 => Sport::Esports,
            _ => Sport::Other,
        }
    }

    fn extract_event_info(data: &serde_json::Value) -> Option<(String, String, String)> {
        let event_id = Self::parse_identifier(data.get("id")?)?;

        let home = data
            .get("team1Name")
            .or_else(|| data.get("competitor1"))
            .and_then(|v| v.as_str())?
            .trim();

        let away = data
            .get("team2Name")
            .or_else(|| data.get("competitor2"))
            .and_then(|v| v.as_str())?
            .trim();

        if home.is_empty() || away.is_empty() {
            return None;
        }

        Some((event_id, home.to_string(), away.to_string()))
    }

    fn parse_identifier(value: &serde_json::Value) -> Option<String> {
        match value {
            serde_json::Value::String(value) => {
                let value = value.trim();
                (!value.is_empty()).then(|| value.to_string())
            }
            serde_json::Value::Number(value) => Some(value.to_string()),
            _ => None,
        }
    }

    fn competition_name(comp: &serde_json::Value) -> &str {
        comp.get("name")
            .or_else(|| comp.get("competitionName"))
            .or_else(|| comp.get("competition").and_then(|value| value.get("name")))
            .or_else(|| {
                comp.get("competition")
                    .and_then(|value| value.get("names"))
                    .and_then(|value| value.get("0"))
            })
            .and_then(|value| value.as_str())
            .unwrap_or("")
    }

    fn parse_start_time(data: &serde_json::Value) -> Option<DateTime<Utc>> {
        data.get("startDateTime")
            .and_then(|value| value.as_str())
            .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
            .map(|value| value.with_timezone(&Utc))
    }

    fn selection_to_odds_type(selection: &str, market: &str) -> OddsType {
        let selection = selection.trim().to_lowercase();
        let market = market.trim().to_lowercase();

        if market.contains("btts")
            || market.contains("обе забьют")
            || market.contains("обе команды забьют")
            || market.contains("both teams score")
        {
            return match selection.as_str() {
                "да" | "yes" => OddsType::BothTeamsScoreYes,
                "нет" | "no" => OddsType::BothTeamsScoreNo,
                _ => OddsType::Custom,
            };
        }

        if market.contains("чёт")
            || market.contains("чет")
            || market.contains("even")
            || market.contains("odd")
        {
            return match selection.as_str() {
                "чёт" | "чет" | "even" => OddsType::Even,
                "нечёт" | "нечет" | "odd" => OddsType::Odd,
                _ => OddsType::Custom,
            };
        }

        match selection.as_str() {
            "п1" | "1" | "home" => OddsType::Home,
            "х" | "x" | "draw" | "ничья" => OddsType::Draw,
            "п2" | "2" | "away" => OddsType::Away,
            "тб" | "tb" | "over" | "больше" => OddsType::Over,
            "тм" | "tm" | "under" | "меньше" => OddsType::Under,
            "ф1" | "ф2" | "handicap" => OddsType::Handicap,
            _ => OddsType::Custom,
        }
    }
}

/// Access-control and rate-limit responses must not trigger retry/failover.
fn is_retryable_error(error: &dyn std::error::Error) -> bool {
    !matches!(extract_status_code(error), Some(401 | 403 | 429))
}

/// Extract HTTP status code from error message
fn extract_status_code(error: &dyn std::error::Error) -> Option<u16> {
    let msg = error.to_string();
    if let Some(index) = msg.find("HTTP ") {
        let slice = &msg[index + 5..];
        return slice
            .split(|ch: char| !ch.is_ascii_digit())
            .find(|segment| !segment.is_empty())
            .and_then(|code_str| code_str.parse::<u16>().ok());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{extract_status_code, OlimpParser, PREMATCH_SPORT_ID_SWEEP};
    use shared::{DiagnosticSeverity, OddsType, ParserReadinessStage, Sport};
    use std::sync::Arc;

    #[test]
    fn prematch_sweep_uses_only_the_confirmed_all_sports_endpoint() {
        assert_eq!(PREMATCH_SPORT_ID_SWEEP, &[0]);
    }

    #[test]
    fn builds_live_section_url_without_duplicate_version_segment() {
        let client = Arc::new(reqwest::Client::new());
        let parser = OlimpParser::new(client);

        assert_eq!(
            parser.section_url(0, "live"),
            "https://www.olimp.bet/api/v4/0/live/sports-with-competitions-with-events?vids%5B%5D="
        );
    }

    #[test]
    fn parses_competition_wrapped_payload_fixture() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../tests/fixtures/olimp_competitions_with_events_fixture.json"
        ))
        .expect("fixture should be valid json");

        let (events, odds) = OlimpParser::parse_api_response_all_sports(&fixture, false)
            .expect("fixture should parse");

        assert_eq!(events.len(), 1);
        assert_eq!(odds.len(), 3);

        let event = &events[0];
        assert_eq!(event.bookmaker_slug, "olimp");
        assert_eq!(event.sport, Sport::Football);
        assert_eq!(
            event.league,
            "Лига Чемпионов UEFA. 1/2 финала. Первые матчи"
        );
        assert_eq!(event.home_team, "ПСЖ");
        assert_eq!(event.away_team, "Арсенал");
        assert_eq!(
            event.start_time.map(|value| value.to_rfc3339()),
            Some("2026-04-30T19:00:00+00:00".to_string())
        );

        assert!(odds.iter().any(|odd| {
            odd.market == "Исход матча (основное время)"
                && odd.selection == "П1"
                && odd.odds_type == OddsType::Home
        }));
        assert!(odds.iter().any(|odd| {
            odd.market == "Исход матча (основное время)"
                && odd.selection == "Х"
                && odd.odds_type == OddsType::Draw
        }));
        assert!(odds.iter().any(|odd| {
            odd.market == "Исход матча (основное время)"
                && odd.selection == "П2"
                && odd.odds_type == OddsType::Away
        }));
    }

    #[test]
    fn parser_separates_live_prematch_and_market_line_ids() {
        let payload = serde_json::json!([
            {
                "payload": {
                    "sport": {"id": "3", "name": "Теннис"},
                    "competitionsWithEvents": [{
                        "name": "ATP Tour",
                        "events": [{
                            "id": 77,
                            "team1Name": "Player A",
                            "team2Name": "Player B",
                            "startDateTime": "2026-11-25T18:00:00+00:00",
                            "outcomes": [
                                {"shortName": "ТБ", "probability": 1.8, "groupName": "Тотал", "param": 1.5},
                                {"shortName": "ТБ", "probability": "2.0", "groupName": "Тотал", "param": 2.5}
                            ]
                        }]
                    }]
                }
            }
        ]);

        let (prematch_events, prematch_odds) =
            OlimpParser::parse_api_response_all_sports(&payload, false)
                .expect("prematch fixture should parse");
        let (live_events, live_odds) = OlimpParser::parse_api_response_all_sports(&payload, true)
            .expect("live fixture should parse");

        assert_eq!(prematch_events[0].sport, Sport::Tennis);
        assert_eq!(prematch_events[0].id, "olimp-prematch-77");
        assert_eq!(live_events[0].id, "olimp-live-77");
        assert_ne!(prematch_events[0].id, live_events[0].id);
        assert_eq!(prematch_odds.len(), 2);
        assert_eq!(live_odds.len(), 2);
        assert!(prematch_odds.iter().any(|odd| odd.line == Some(1.5)));
        assert!(prematch_odds.iter().any(|odd| odd.line == Some(2.5)));
        let unique_ids = prematch_odds
            .iter()
            .map(|odd| &odd.id)
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(unique_ids.len(), prematch_odds.len());
        assert!(prematch_odds
            .iter()
            .all(|odd| odd.event_id == "olimp-prematch-77"));
        assert!(live_odds.iter().all(|odd| odd.event_id == "olimp-live-77"));
    }

    #[test]
    fn accepts_numeric_sport_ids_and_rejects_malformed_event_identity() {
        let payload = serde_json::json!([{
            "payload": {
                "sport": {"id": 1, "name": "Футбол"},
                "competitionsWithEvents": [{"name": "Тестовая лига", "events": [
                    {"id": 777, "team1Name": "  Домашняя  ", "team2Name": "Гостевая", "outcomes": [
                        {"shortName": "п1", "probability": 1.9, "groupName": "Исход"}
                    ]},
                    {"id": null, "team1Name": "A", "team2Name": "B", "outcomes": [
                        {"shortName": "П1", "probability": 1.9, "groupName": "Исход"}
                    ]},
                    {"id": 779, "team1Name": "A", "team2Name": "   ", "outcomes": [
                        {"shortName": "П1", "probability": 1.9, "groupName": "Исход"}
                    ]}
                ]}]
            }
        }]);

        let (events, odds) = OlimpParser::parse_api_response_all_sports(&payload, false)
            .expect("payload with valid and malformed event rows should parse");

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].id, "olimp-prematch-777");
        assert_eq!(events[0].sport, Sport::Football);
        assert_eq!(events[0].home_team, "Домашняя");
        assert_eq!(events[0].away_team, "Гостевая");
        assert_eq!(odds.len(), 1);
        assert_eq!(odds[0].odds_type, OddsType::Home);
    }

    #[test]
    fn selection_mapping_is_case_and_whitespace_insensitive() {
        assert_eq!(
            OlimpParser::selection_to_odds_type(" HOME ", "1X2"),
            OddsType::Home
        );
        assert_eq!(
            OlimpParser::selection_to_odds_type("п2", "1X2"),
            OddsType::Away
        );
        assert_eq!(
            OlimpParser::selection_to_odds_type("ТБ", "Тотал"),
            OddsType::Over
        );
        assert_eq!(
            OlimpParser::selection_to_odds_type("x", "1X2"),
            OddsType::Draw
        );
        assert_eq!(
            OlimpParser::selection_to_odds_type("Да", "Обе команды забьют"),
            OddsType::BothTeamsScoreYes
        );
        assert_eq!(
            OlimpParser::selection_to_odds_type("Нет", "BTTS"),
            OddsType::BothTeamsScoreNo
        );
        assert_eq!(
            OlimpParser::selection_to_odds_type("Чёт", "Чёт/нечёт"),
            OddsType::Even
        );
        assert_eq!(
            OlimpParser::selection_to_odds_type("Нечёт", "Чёт/нечёт"),
            OddsType::Odd
        );
    }

    #[test]
    fn excludes_informational_and_result_cards_without_priced_outcomes() {
        let payload = serde_json::json!([
            {
                "payload": {
                    "sport": {"id": "1", "name": "Футбол"},
                    "competitionsWithEvents": [{
                        "name": "Россия. Премьер-лига",
                        "events": [
                            {
                                "id": 101,
                                "team1Name": "Команда А",
                                "team2Name": "Команда Б",
                                "outcomes": [
                                    {"shortName": "П1", "probability": 1.8, "groupName": "Исход матча"},
                                    {"shortName": "П2", "probability": 2.1, "groupName": "Исход матча"}
                                ]
                            },
                            {
                                "id": 102,
                                "team1Name": "Команда А (замены)",
                                "team2Name": "Команда Б (замены)",
                                "outcomes": []
                            },
                            {
                                "id": 103,
                                "team1Name": "",
                                "team2Name": "",
                                "outcomes": []
                            }
                        ]
                    }]
                }
            }
        ]);

        let (events, odds) = OlimpParser::parse_api_response_all_sports(&payload, false)
            .expect("payload should parse");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].id, "olimp-prematch-101");
        assert_eq!(odds.len(), 2);
        assert!(odds.iter().all(|odd| odd.event_id == events[0].id));
    }

    #[test]
    fn parser_rejects_non_array_and_incomplete_api_schema() {
        let non_array =
            OlimpParser::parse_api_response_all_sports(&serde_json::json!({"payload": {}}), false)
                .expect_err("non-array API root must be rejected");
        assert!(non_array.to_string().contains("sports array"));

        let missing_competitions = OlimpParser::parse_api_response_all_sports(
            &serde_json::json!([{"payload": {"sport": {"id": "1"}}}]),
            false,
        )
        .expect_err("missing competitions array must be rejected");
        assert!(missing_competitions
            .to_string()
            .contains("competitionsWithEvents array"));
    }

    #[test]
    fn readiness_snapshot_confirms_access_controls_are_respected() {
        let readiness = OlimpParser::readiness_snapshot();

        assert_eq!(readiness.stage, ParserReadinessStage::RolloutReady);
        assert!(!readiness.production_enabled);
        assert!(readiness.self_check_available);

        assert!(readiness.checks.iter().any(|check| {
            check.code == "access_controls_respected"
                && matches!(check.severity, DiagnosticSeverity::Pass)
                && check.message.contains("does not rotate proxies")
        }));
    }

    #[test]
    fn readiness_snapshot_locks_runtime_event_volume_truth() {
        let readiness = OlimpParser::readiness_snapshot();

        assert_eq!(readiness.stage, ParserReadinessStage::RolloutReady);
        assert!(!readiness.production_enabled);
        assert!(readiness.self_check_available);
        assert!(readiness.checks.iter().any(|check| {
            check.code == "historical_runtime_sample_observed"
                && matches!(check.severity, DiagnosticSeverity::Pass)
                && check.message.contains("164 live")
                && check.message.contains("1509 prematch")
                && check.message.contains("historical direct-transport sample")
        }));
        assert!(readiness.checks.iter().any(|check| {
            check.code == "production_volume_still_unlocked"
                && matches!(check.severity, DiagnosticSeverity::Warn)
        }));
    }

    #[test]
    fn circuit_breaker_starts_closed() {
        let client = Arc::new(reqwest::Client::new());
        let parser = OlimpParser::new(client);

        assert!(parser.circuit_breaker.allow_request());
    }

    #[test]
    fn maps_sports_from_live_olimp_catalogue_names() {
        for (id, name, expected) in [
            ("1", "Футбол", Sport::Football),
            ("2", "Хоккей", Sport::Hockey),
            ("3", "Теннис", Sport::Tennis),
            ("5", "Баскетбол", Sport::Basketball),
            ("6", "Американский футбол", Sport::Other),
            ("7", "Биатлон", Sport::WinterSports),
            ("9", "Гандбол", Sport::Handball),
            ("10", "Волейбол", Sport::Volleyball),
            ("11", "Футзал", Sport::Futsal),
            ("12", "Бокс", Sport::Boxing),
            ("29", "Бейсбол", Sport::Baseball),
            ("33", "Формула 1", Sport::Motorsport),
            ("40", "Настольный теннис", Sport::TableTennis),
            ("41", "Водное поло", Sport::WaterPolo),
            ("44", "Велоспорт", Sport::Cycling),
            ("46", "Регби-Союз", Sport::Rugby),
            ("47", "Дартс", Sport::Darts),
            ("51", "Бадминтон", Sport::Badminton),
            ("73", "Крикет", Sport::Cricket),
            ("91", "Австралийский футбол", Sport::AussieRules),
            ("96", "MMA", Sport::Mma),
            ("112", "Киберспорт", Sport::Esports),
            ("138", "Киберфутбол", Sport::Esports),
            ("148", "Падел-теннис", Sport::Tennis),
            ("4", "Бильярд", Sport::Other),
            ("8", "Хоккей с мячом", Sport::Other),
        ] {
            assert_eq!(
                OlimpParser::sport_from_api(id, name),
                expected,
                "unexpected sport mapping for Olimp source {id} ({name})"
            );
        }
    }

    #[test]
    fn falls_back_to_current_olimp_ids_only_when_sport_name_is_missing() {
        assert_eq!(OlimpParser::sport_from_api("3", ""), Sport::Tennis);
        assert_eq!(OlimpParser::sport_from_api("40", "  "), Sport::TableTennis);
        assert_eq!(OlimpParser::sport_from_api("9999", ""), Sport::Other);
    }

    #[test]
    fn status_code_extraction() {
        let error = std::io::Error::other("HTTP 403: Forbidden");
        let code = extract_status_code(&error);

        assert_eq!(code, Some(403));
    }

    #[test]
    fn access_and_rate_limit_statuses_are_not_retried() {
        for status in [401, 403, 429] {
            let error = std::io::Error::other(format!("HTTP {status}: refused"));
            assert!(!super::is_retryable_error(&error), "status {status}");
        }
        for status in [408, 500, 502, 503, 504] {
            let error = std::io::Error::other(format!("HTTP {status}: transient"));
            assert!(super::is_retryable_error(&error), "status {status}");
        }
        let transport_error = std::io::Error::other("connection reset");
        assert!(super::is_retryable_error(&transport_error));
    }
}
