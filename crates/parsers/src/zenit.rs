use crate::base::{BookmakerParser, ParserResult};
use async_trait::async_trait;
use chrono::{Datelike, NaiveDateTime, TimeZone, Utc};
use reqwest::Client;
use shared::odds::OddsType;
use shared::{
    DiagnosticSeverity, Event, Odd, ParserDiagnosticCheck, ParserReadiness, ParserReadinessStage,
    Sport,
};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::env;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::sleep;
use tracing::{debug, error, info, warn};

/// Zenit API parser — чистые HTTP запросы без Python/Playwright
///
/// API требует заголовки `imprinthash` и `frontversion`, захваченные из браузера.
/// Эндпоинты:
///   - Prematch: `https://zenit.win/ajax/line/printer/react`
///   - Live:     `https://zenit.win/ajax/live/printer/react`
///
/// Ответ: `{ games: { id: { c1_id, c2_id, tid, f_l: [{o, h}, ...] } }, dict: { cmd: { id: name }, tournament: { id: { name } } } }`
#[derive(Debug)]
pub struct ZenitParser {
    client: Arc<Client>,
}

#[derive(Debug, Default)]
struct ParsedPage {
    events: Vec<Event>,
    odds: Vec<Odd>,
    raw_game_ids: Vec<String>,
    discovered_game_ids: Vec<String>,
    /// Games whose first-party response advertises separately loaded markets.
    additional_market_game_ids: Vec<String>,
    /// Valid-priced source rows whose factor ID/header pair is not understood.
    /// These are intentionally not emitted as odds; report them so new markets
    /// can be mapped from real bookmaker payloads rather than guessed.
    unmapped_factor_counts: BTreeMap<String, usize>,
    schema_error: Option<String>,
}

#[derive(Debug, Clone, Copy)]
struct ZenitSport {
    id: u64,
    count: usize,
}

impl ZenitParser {
    const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";
    const DEFAULT_IMPRINT_HASH: &str = "d01d68e5a9775b90a0c7239e7f078895";
    const DEFAULT_FRONT_VERSION: &str = "1.72.1";
    const IMPRINT_HASH_ENV: &str = "ZENIT_IMPRINT_HASH";
    const FRONT_VERSION_ENV: &str = "ZENIT_FRONT_VERSION";
    const STRICT_LIVE_KPI_TARGET: usize = 150;
    const STRICT_PREMATCH_KPI_TARGET: usize = 3000;
    const RECENT_RUNTIME_LIVE_EVENTS: usize = 182;
    const RECENT_RUNTIME_PREMATCH_EVENTS: usize = 3497;

    // Retry configuration
    const MAX_RETRIES: u32 = 3;
    const INITIAL_BACKOFF_MS: u64 = 500;
    const MAX_BACKOFF_MS: u64 = 5000;
    const REQUEST_TIMEOUT_SECS: u64 = 30;

    // Sport IDs
    const SPORT_FOOTBALL: u64 = 1;
    const SPORT_HOCKEY: u64 = 2;
    const SPORT_BASKETBALL: u64 = 3;
    const SPORT_VOLLEYBALL: u64 = 4;
    const SPORT_TENNIS: u64 = 5;
    const SPORT_TABLE_TENNIS: u64 = 6;
    const SPORT_ESPORTS: u64 = 7;
    const SPORT_FUTSAL: u64 = 8;
    const SPORT_HANDBALL: u64 = 9;
    const SPORT_BADMINTON: u64 = 11;
    const SPORT_BASEBALL: u64 = 12;
    const SPORT_MMA: u64 = 13;
    const SPORT_BOXING: u64 = 14;
    const PAGE_STEP: usize = 3000;
    const PAGE_LENGTH: &str = "3000";
    const GAME_BATCH_SIZE: usize = 200;

    pub fn new(client: Arc<Client>) -> Self {
        Self { client }
    }

    fn readiness_snapshot() -> ParserReadiness {
        ParserReadiness {
            stage: ParserReadinessStage::RolloutReady,
            production_enabled: false,
            self_check_available: true,
            checks: vec![
                ParserDiagnosticCheck {
                    code: "http_runtime_path_registered".to_string(),
                    severity: DiagnosticSeverity::Pass,
                    message: "Zenit is registered in ParserFactory and runtime diagnostics through direct line/live JSON endpoints backed by imprinthash/frontversion headers instead of a browser bridge.".to_string(),
                },
                ParserDiagnosticCheck {
                    code: "runtime_kpi_previously_met".to_string(),
                    severity: DiagnosticSeverity::Pass,
                    message: format!(
                        "A recent Zenit runtime snapshot observed {} live and {} prematch events, proving the HTTP path can clear the older 100/2000 nightly gate.",
                        Self::RECENT_RUNTIME_LIVE_EVENTS,
                        Self::RECENT_RUNTIME_PREMATCH_EVENTS,
                    ),
                },
                ParserDiagnosticCheck {
                    code: "strict_nightly_regressed_to_zero".to_string(),
                    severity: DiagnosticSeverity::Warn,
                    message: format!(
                        "Production promotion stays blocked because the latest strict nightly run returned zero Zenit events against the current {} live / {} prematch targets without a transport error, so readiness is rollout-only until the runtime regression is explained.",
                        Self::STRICT_LIVE_KPI_TARGET,
                        Self::STRICT_PREMATCH_KPI_TARGET,
                    ),
                },
            ],
        }
    }

    fn sport_referer(sport_id: u64, is_live: bool) -> &'static str {
        match (sport_id, is_live) {
            (Self::SPORT_FOOTBALL, false) => "https://zenit.win/line/football",
            (Self::SPORT_FOOTBALL, true) => "https://zenit.win/live/football",
            (Self::SPORT_HOCKEY, false) => "https://zenit.win/line/hockey",
            (Self::SPORT_HOCKEY, true) => "https://zenit.win/live/hockey",
            (Self::SPORT_BASKETBALL, false) => "https://zenit.win/line/basketball",
            (Self::SPORT_BASKETBALL, true) => "https://zenit.win/live/basketball",
            (Self::SPORT_VOLLEYBALL, false) => "https://zenit.win/line/volleyball",
            (Self::SPORT_VOLLEYBALL, true) => "https://zenit.win/live/volleyball",
            (Self::SPORT_TENNIS, false) => "https://zenit.win/line/tennis",
            (Self::SPORT_TENNIS, true) => "https://zenit.win/live/tennis",
            (Self::SPORT_TABLE_TENNIS, false) => "https://zenit.win/line/table-tennis",
            (Self::SPORT_TABLE_TENNIS, true) => "https://zenit.win/live/table-tennis",
            (Self::SPORT_ESPORTS, false) => "https://zenit.win/line/esports",
            (Self::SPORT_ESPORTS, true) => "https://zenit.win/live/esports",
            (Self::SPORT_FUTSAL, false) => "https://zenit.win/line/futsal",
            (Self::SPORT_FUTSAL, true) => "https://zenit.win/live/futsal",
            (Self::SPORT_HANDBALL, false) => "https://zenit.win/line/handball",
            (Self::SPORT_HANDBALL, true) => "https://zenit.win/live/handball",
            (Self::SPORT_BADMINTON, false) => "https://zenit.win/line/badminton",
            (Self::SPORT_BADMINTON, true) => "https://zenit.win/live/badminton",
            (Self::SPORT_BASEBALL, false) => "https://zenit.win/line/baseball",
            (Self::SPORT_BASEBALL, true) => "https://zenit.win/live/baseball",
            (Self::SPORT_MMA, false) => "https://zenit.win/line/mma",
            (Self::SPORT_MMA, true) => "https://zenit.win/live/mma",
            (Self::SPORT_BOXING, false) => "https://zenit.win/line/boxing",
            (Self::SPORT_BOXING, true) => "https://zenit.win/live/boxing",
            _ if is_live => "https://zenit.win/live/football",
            _ => "https://zenit.win/line/football",
        }
    }

    fn collect_game_ids(value: &serde_json::Value, game_ids: &mut HashSet<String>) {
        match value {
            serde_json::Value::Object(map) => {
                if let Some(games) = map.get("games").and_then(|value| value.as_array()) {
                    for game_id in games {
                        if let Some(game_id) = game_id.as_u64() {
                            game_ids.insert(game_id.to_string());
                        } else if let Some(game_id) = game_id.as_str() {
                            game_ids.insert(game_id.to_string());
                        }
                    }
                }

                for child in map.values() {
                    Self::collect_game_ids(child, game_ids);
                }
            }
            serde_json::Value::Array(items) => {
                for item in items {
                    Self::collect_game_ids(item, game_ids);
                }
            }
            _ => {}
        }
    }

    fn imprinthash() -> String {
        env::var(Self::IMPRINT_HASH_ENV).unwrap_or_else(|_| Self::DEFAULT_IMPRINT_HASH.to_string())
    }

    fn frontversion() -> String {
        env::var(Self::FRONT_VERSION_ENV)
            .unwrap_or_else(|_| Self::DEFAULT_FRONT_VERSION.to_string())
    }

    fn line_query(offset: usize, sport_id: u64, games: Option<&str>) -> Vec<(String, String)> {
        vec![
            ("all".to_string(), "0".to_string()),
            ("onlyview".to_string(), "0".to_string()),
            ("timeline".to_string(), "0".to_string()),
            ("tournaments_mode".to_string(), "1".to_string()),
            ("sport".to_string(), sport_id.to_string()),
            ("tournament".to_string(), String::new()),
            ("tournament_region".to_string(), String::new()),
            ("tournament_info".to_string(), String::new()),
            ("league".to_string(), String::new()),
            ("games".to_string(), games.unwrap_or_default().to_string()),
            ("ross".to_string(), "0".to_string()),
            ("lang_id".to_string(), "1".to_string()),
            ("timezone".to_string(), "3".to_string()),
            ("offset".to_string(), offset.to_string()),
            ("show_from_main".to_string(), "0".to_string()),
            // The official line frontend sends its actual build version.
            // An empty client_v and forced popular=1 do not match the normal
            // sport-line request and can restrict the returned market catalogue.
            ("client_v".to_string(), Self::frontversion()),
            ("length".to_string(), Self::PAGE_LENGTH.to_string()),
            ("sort_mode".to_string(), "2".to_string()),
            ("b_id".to_string(), String::new()),
        ]
    }

    fn live_query() -> Vec<(String, String)> {
        vec![
            ("ross".to_string(), "0".to_string()),
            ("all".to_string(), "0".to_string()),
            ("timezone".to_string(), "3".to_string()),
            ("lang_id".to_string(), "1".to_string()),
            ("sort_mode".to_string(), "2".to_string()),
            ("onlyview".to_string(), "0".to_string()),
            ("print_mode".to_string(), "react".to_string()),
            ("show_from_main".to_string(), "0".to_string()),
        ]
    }

    fn left_menu_query() -> [(&'static str, &'static str); 3] {
        [
            ("lang_id", "1"),
            ("sort_mode", "2"),
            ("tournaments_mode", "1"),
        ]
    }

    /// Check if error is transient and worth retrying
    fn is_transient_error(error: &str) -> bool {
        let error = error.to_ascii_lowercase();
        error.contains("timeout")
            || error.contains("timed out")
            || error.contains("connection")
            || error.contains("connecterror")
            || error.contains("429")
            || error.contains("502")
            || error.contains("503")
            || error.contains("504")
            || error.contains("temporary failure")
            || error.contains("too many requests")
    }

    /// Calculate backoff duration with exponential growth
    fn backoff_duration(attempt: u32) -> Duration {
        let base_ms = Self::INITIAL_BACKOFF_MS;
        let backoff_ms = base_ms * 2_u64.pow(attempt);
        let capped_ms = backoff_ms.min(Self::MAX_BACKOFF_MS);
        Duration::from_millis(capped_ms)
    }

    /// Retry helper for network operations with exponential backoff
    async fn retry_with_backoff<F, Fut, T>(
        &self,
        description: &str,
        mut operation: F,
    ) -> Result<T, Box<dyn std::error::Error + Send + Sync>>
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = Result<T, Box<dyn std::error::Error + Send + Sync>>>,
    {
        let mut last_error: Option<String> = None;

        for attempt in 0..Self::MAX_RETRIES {
            debug!(attempt, description, "Zenit retry attempt");

            match operation().await {
                Ok(result) => {
                    if attempt > 0 {
                        info!(
                            attempt,
                            description, "Zenit operation succeeded after retries"
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
                            "Zenit permanent error (not retrying)"
                        );
                        return Err(err);
                    }

                    if attempt < Self::MAX_RETRIES - 1 {
                        let backoff = Self::backoff_duration(attempt);
                        warn!(
                            attempt,
                            error = &err_str,
                            backoff_ms = backoff.as_millis(),
                            description,
                            "Zenit transient error, retrying after backoff"
                        );
                        sleep(backoff).await;
                    } else {
                        error!(
                            attempt,
                            error = &err_str,
                            max_retries = Self::MAX_RETRIES,
                            description,
                            "Zenit operation failed after all retries"
                        );
                    }
                }
            }
        }

        Err(format!(
            "Zenit {}: {} (failed after {} retries)",
            description,
            last_error.unwrap_or_else(|| "unknown error".to_string()),
            Self::MAX_RETRIES
        )
        .into())
    }

    fn parse_u64_value(value: &serde_json::Value) -> Option<u64> {
        match value {
            serde_json::Value::Number(number) => number.as_u64(),
            serde_json::Value::String(text) => text.trim().parse::<u64>().ok(),
            _ => None,
        }
    }

    fn parse_string_id(value: &serde_json::Value) -> Option<String> {
        match value {
            serde_json::Value::Number(number) => Some(number.to_string()),
            serde_json::Value::String(text) => {
                let trimmed = text.trim();
                (!trimmed.is_empty()).then(|| trimmed.to_string())
            }
            _ => None,
        }
    }

    fn parse_start_time(
        game_obj: &serde_json::Map<String, serde_json::Value>,
    ) -> Option<chrono::DateTime<Utc>> {
        game_obj
            .get("time")
            .and_then(Self::parse_i64_value)
            .and_then(|value| Utc.timestamp_opt(value, 0).single())
            .or_else(|| game_obj.get("date").and_then(Self::parse_date_value))
    }

    fn parse_i64_value(value: &serde_json::Value) -> Option<i64> {
        match value {
            serde_json::Value::Number(number) => number.as_i64(),
            serde_json::Value::String(text) => text.trim().parse::<i64>().ok(),
            _ => None,
        }
    }

    fn parse_date_value(value: &serde_json::Value) -> Option<chrono::DateTime<Utc>> {
        let text = value.as_str()?.trim();
        if text.is_empty() {
            return None;
        }

        if let Ok(timestamp) = text.parse::<i64>() {
            if timestamp > 0 {
                return Utc.timestamp_opt(timestamp, 0).single();
            }
        }

        if let Ok(date_time) = chrono::DateTime::parse_from_rfc3339(text) {
            return Some(date_time.with_timezone(&Utc));
        }

        for format in [
            "%Y-%m-%d %H:%M:%S",
            "%Y-%m-%d %H:%M",
            "%Y/%m/%d %H:%M:%S",
            "%Y/%m/%d %H:%M",
        ] {
            if let Ok(date_time) = NaiveDateTime::parse_from_str(text, format) {
                return Some(Utc.from_utc_datetime(&date_time));
            }
        }

        Self::parse_short_date(text)
    }

    fn parse_short_date(text: &str) -> Option<chrono::DateTime<Utc>> {
        let mut parts = text.split_whitespace();
        let raw_date = parts.next()?;
        let raw_time = parts.next()?;
        if parts.next().is_some() {
            return None;
        }

        let mut date_parts = raw_date.split('/');
        let first = date_parts.next()?.trim().parse::<u32>().ok()?;
        let second = date_parts.next()?.trim().parse::<u32>().ok()?;
        if date_parts.next().is_some() {
            return None;
        }

        let mut time_parts = raw_time.split(':');
        let hour = time_parts.next()?.trim().parse::<u32>().ok()?;
        let minute = time_parts.next()?.trim().parse::<u32>().ok()?;
        if time_parts.next().is_some() {
            return None;
        }

        let now = Utc::now();
        let year = now.year();
        let candidates = if first > 12 {
            [(second, first), (first, second)]
        } else if second > 12 {
            [(first, second), (second, first)]
        } else {
            [(second, first), (first, second)]
        };

        for (month, day) in candidates {
            if let Some(date) = chrono::NaiveDate::from_ymd_opt(year, month, day)
                .and_then(|date| date.and_hms_opt(hour, minute, 0))
            {
                let mut parsed = Utc.from_utc_datetime(&date);
                if parsed < now - chrono::TimeDelta::days(180) {
                    if let Some(next_year) = chrono::NaiveDate::from_ymd_opt(year + 1, month, day)
                        .and_then(|date| date.and_hms_opt(hour, minute, 0))
                    {
                        parsed = Utc.from_utc_datetime(&next_year);
                    }
                }
                return Some(parsed);
            }
        }

        None
    }

    fn adjacent_line_value(
        bets: &[serde_json::Value],
        index: usize,
        prefer_previous: bool,
    ) -> Option<f64> {
        let previous = index
            .checked_sub(1)
            .and_then(|prev| bets.get(prev))
            .and_then(|value| value.get("h"))
            .and_then(Self::parse_numeric_value);
        let next = bets
            .get(index + 1)
            .and_then(|value| value.get("h"))
            .and_then(Self::parse_numeric_value);

        if prefer_previous {
            previous.or(next)
        } else {
            next.or(previous)
        }
    }

    async fn fetch_page(
        &self,
        base_url: &str,
        sport_id: u64,
        is_live: bool,
        offset: usize,
        games: Option<&str>,
    ) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
        let referer = Self::sport_referer(sport_id, is_live);
        let imprinthash = Self::imprinthash();
        let frontversion = Self::frontversion();
        let query = Self::line_query(offset, sport_id, games);

        let operation = || async {
            debug!(
                base_url,
                sport = sport_id,
                is_live,
                offset,
                has_games = games.is_some(),
                imprinthash,
                frontversion,
                "Zenit fetch_page request"
            );

            let resp = self
                .client
                .get(base_url)
                .timeout(Duration::from_secs(Self::REQUEST_TIMEOUT_SECS))
                .header("User-Agent", Self::USER_AGENT)
                .header("Accept", "application/json, text/plain, */*")
                .header("Accept-Language", "ru-RU,ru;q=0.9,en-US;q=0.8,en;q=0.7")
                .header("Accept-Encoding", "gzip, deflate, br")
                .header(
                    "sec-ch-ua",
                    "\"Not:A-Brand\";v=\"99\", \"Chromium\";v=\"145\", \"HeadlessChrome\";v=\"145\"",
                )
                .header("sec-ch-ua-mobile", "?0")
                .header("sec-ch-ua-platform", "\"Windows\"")
                .header("Referer", referer)
                .header("X-Requested-With", "XMLHttpRequest")
                .header("imprinthash", &imprinthash)
                .header("frontversion", &frontversion)
                .query(&query)
                .send()
                .await
                .map_err(|e| {
                    error!(error = %e, base_url, sport = sport_id, "Zenit fetch_page HTTP error");
                    Box::new(e) as Box<dyn std::error::Error + Send + Sync>
                })?;

            let status = resp.status();
            debug!(status = %status, sport = sport_id, offset, "Zenit fetch_page response");

            if !status.is_success() {
                let body = resp
                    .text()
                    .await
                    .unwrap_or_else(|_| "<failed to read body>".to_string());
                let error_msg = format!(
                    "Zenit API returned HTTP {} for sport {} at offset {}. Body: {}",
                    status, sport_id, offset, body
                );
                error!(error = &error_msg, "Zenit fetch_page HTTP error");
                return Err(error_msg.into());
            }

            resp.json::<serde_json::Value>().await.map_err(|e| {
                error!(error = %e, sport = sport_id, "Zenit fetch_page JSON parse error");
                Box::new(e) as Box<dyn std::error::Error + Send + Sync>
            })
        };

        self.retry_with_backoff(
            &format!("fetch_page(sport={}, offset={})", sport_id, offset),
            operation,
        )
        .await
    }

    fn ross_details_url(is_live: bool, imprinthash: &str) -> String {
        if is_live {
            format!("https://zenit.win/ajax/live/ross/data/{imprinthash}/")
        } else {
            "https://zenit.win/ajax/line/ross/data/".to_string()
        }
    }

    fn ross_details_query(game_ids: &[String], frontversion: &str) -> Vec<(String, String)> {
        vec![
            ("onlyview".to_string(), "0".to_string()),
            ("gid".to_string(), game_ids.join("-")),
            ("lang_id".to_string(), "1".to_string()),
            ("client_v".to_string(), frontversion.to_string()),
            ("show_from_main".to_string(), "0".to_string()),
        ]
    }

    /// Fetch the bookmaker's own event-specific expanded market payload.
    /// The official frontend calls this endpoint only after expanding the
    /// additional-market counter for the corresponding match.
    async fn fetch_ross_details(
        &self,
        is_live: bool,
        game_ids: &[String],
    ) -> Result<HashMap<String, serde_json::Value>, Box<dyn std::error::Error + Send + Sync>> {
        if game_ids.is_empty() {
            return Ok(HashMap::new());
        }

        let frontversion = Self::frontversion();
        let endpoint = Self::ross_details_url(is_live, &Self::imprinthash());
        let query = Self::ross_details_query(game_ids, &frontversion);
        let referer = if is_live {
            "https://zenit.win/live/football"
        } else {
            "https://zenit.win/line/football"
        };

        let operation = || async {
            let response = self
                .client
                .get(&endpoint)
                .timeout(Duration::from_secs(Self::REQUEST_TIMEOUT_SECS))
                .header("User-Agent", Self::USER_AGENT)
                .header("Accept", "application/json, text/plain, */*")
                .header("Accept-Language", "ru-RU,ru;q=0.9,en-US;q=0.8,en;q=0.7")
                .header("Accept-Encoding", "gzip, deflate, br")
                .header("Referer", referer)
                .header("X-Requested-With", "XMLHttpRequest")
                .header("imprinthash", Self::imprinthash())
                .header("frontversion", &frontversion)
                .query(&query)
                .send()
                .await
                .map_err(|error| {
                    error!(
                        error = %error,
                        is_live,
                        game_count = game_ids.len(),
                        "Zenit additional-market HTTP request failed"
                    );
                    Box::new(error) as Box<dyn std::error::Error + Send + Sync>
                })?;

            let status = response.status();
            if !status.is_success() {
                let body = response
                    .text()
                    .await
                    .unwrap_or_else(|_| "<failed to read body>".to_string())
                    .chars()
                    .take(500)
                    .collect::<String>();
                return Err(std::io::Error::other(format!(
                    "Zenit additional-market API returned HTTP {status}: {body}"
                ))
                .into());
            }

            let payload: serde_json::Value = response.json().await?;
            if payload.get("errorCode").is_some() {
                let message = payload
                    .get("msg")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("unknown");
                return Err(std::io::Error::other(format!(
                    "Zenit additional-market API application error: {message}"
                ))
                .into());
            }

            let table_by_game = payload
                .get("t_b")
                .and_then(serde_json::Value::as_object)
                .ok_or_else(|| {
                    std::io::Error::other(
                        "Zenit additional-market response is missing the t_b object",
                    )
                })?;

            Ok(table_by_game
                .iter()
                .map(|(game_id, data)| (game_id.clone(), data.clone()))
                .collect::<HashMap<_, _>>())
        };

        self.retry_with_backoff(
            &format!("fetch_ross_details(live={is_live}, games={})", game_ids.len()),
            operation,
        )
        .await
    }

    async fn attach_additional_market_details(
        &self,
        events: &mut [Event],
        game_ids: &HashSet<String>,
        is_live: bool,
    ) {
        if game_ids.is_empty() {
            return;
        }

        let mut ordered_game_ids = game_ids.iter().cloned().collect::<Vec<_>>();
        ordered_game_ids.sort_unstable();
        let mut payloads = HashMap::new();
        let mut failed_batches = 0usize;

        for chunk in ordered_game_ids.chunks(Self::GAME_BATCH_SIZE) {
            match self.fetch_ross_details(is_live, chunk).await {
                Ok(batch) => payloads.extend(batch),
                Err(error) => {
                    failed_batches += 1;
                    warn!(
                        error = %error,
                        is_live,
                        batch_size = chunk.len(),
                        "Zenit additional-market batch failed; main event and odds data are retained"
                    );
                }
            }
        }

        let response_game_count = payloads.len();
        let attached = Self::attach_ross_payloads(events, payloads);
        info!(
            is_live,
            advertised_games = ordered_game_ids.len(),
            returned_games = response_game_count,
            attached_games = attached,
            failed_batches,
            "Zenit additional-market expansion completed"
        );
    }

    fn attach_ross_payloads(
        events: &mut [Event],
        payloads: HashMap<String, serde_json::Value>,
    ) -> usize {
        let mut payloads = payloads;
        let mut attached = 0usize;

        for event in events {
            let Some(source_game_id) = event.id.rsplit('-').next() else {
                continue;
            };
            let Some(payload) = payloads.remove(source_game_id) else {
                continue;
            };

            // Preserve the bookmaker's original market tables, filters,
            // first-line rows and "other" bets as raw data. They remain
            // non-actionable until their market/selection/period semantics
            // are normalized and tested against other bookmakers.
            event.extra.insert(
                "zenit_additional_markets".to_string(),
                payload,
            );
            attached += 1;
        }

        attached
    }

    async fn fetch_live_page(
        &self,
    ) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
        let query = Self::live_query();

        let operation = || async {
            debug!("Zenit fetch_live_page request");

            let resp = self
                .client
                .get("https://zenit.win/ajax/live/printer/react")
                .timeout(Duration::from_secs(Self::REQUEST_TIMEOUT_SECS))
                .header("User-Agent", Self::USER_AGENT)
                .header("Accept", "application/json, text/plain, */*")
                .header("Accept-Language", "ru-RU,ru;q=0.9,en-US;q=0.8,en;q=0.7")
                .header("Accept-Encoding", "gzip, deflate, br")
                .header(
                    "sec-ch-ua",
                    "\"Not:A-Brand\";v=\"99\", \"Chromium\";v=\"145\", \"HeadlessChrome\";v=\"145\"",
                )
                .header("sec-ch-ua-mobile", "?0")
                .header("sec-ch-ua-platform", "\"Windows\"")
                .header("Referer", "https://zenit.win/live/football")
                .header("X-Requested-With", "XMLHttpRequest")
                .header("imprinthash", Self::imprinthash())
                .header("frontversion", Self::frontversion())
                .query(&query)
                .send()
                .await
                .map_err(|e| {
                    error!(error = %e, "Zenit fetch_live_page HTTP error");
                    Box::new(e) as Box<dyn std::error::Error + Send + Sync>
                })?;

            let status = resp.status();
            debug!(status = %status, "Zenit fetch_live_page response");

            if !status.is_success() {
                let body = resp
                    .text()
                    .await
                    .unwrap_or_else(|_| "<failed to read body>".to_string());
                let error_msg = format!("Zenit live API returned HTTP {}. Body: {}", status, body);
                error!(error = &error_msg, "Zenit fetch_live_page HTTP error");
                return Err(error_msg.into());
            }

            resp.json::<serde_json::Value>().await.map_err(|e| {
                error!(error = %e, "Zenit fetch_live_page JSON parse error");
                Box::new(e) as Box<dyn std::error::Error + Send + Sync>
            })
        };

        self.retry_with_backoff("fetch_live_page", operation).await
    }

    async fn fetch_available_sports(
        &self,
    ) -> Result<Vec<ZenitSport>, Box<dyn std::error::Error + Send + Sync>> {
        let operation = || async {
            debug!("Zenit fetch_available_sports request");

            let resp = self
                .client
                .get("https://zenit.win/ajax/line/left_menu/get")
                .timeout(Duration::from_secs(Self::REQUEST_TIMEOUT_SECS))
                .header("User-Agent", Self::USER_AGENT)
                .header("Accept", "application/json, text/plain, */*")
                .header("Accept-Language", "ru-RU,ru;q=0.9,en-US;q=0.8,en;q=0.7")
                .header("Accept-Encoding", "gzip, deflate, br")
                .header(
                    "sec-ch-ua",
                    "\"Not:A-Brand\";v=\"99\", \"Chromium\";v=\"145\", \"HeadlessChrome\";v=\"145\"",
                )
                .header("sec-ch-ua-mobile", "?0")
                .header("sec-ch-ua-platform", "\"Windows\"")
                .header("Referer", "https://zenit.win/line/football")
                .header("X-Requested-With", "XMLHttpRequest")
                .header("imprinthash", Self::imprinthash())
                .header("frontversion", Self::frontversion())
                .query(&Self::left_menu_query())
                .send()
                .await
                .map_err(|e| {
                    error!(error = %e, "Zenit fetch_available_sports HTTP error");
                    Box::new(e) as Box<dyn std::error::Error + Send + Sync>
                })?;

            let status = resp.status();
            debug!(status = %status, "Zenit fetch_available_sports response");

            if !status.is_success() {
                let body = resp
                    .text()
                    .await
                    .unwrap_or_else(|_| "<failed to read body>".to_string());
                let error_msg = format!("Zenit left menu returned HTTP {}. Body: {}", status, body);
                error!(
                    error = &error_msg,
                    "Zenit fetch_available_sports HTTP error"
                );
                return Err(error_msg.into());
            }

            resp.json::<serde_json::Value>().await.map_err(|e| {
                error!(error = %e, "Zenit fetch_available_sports JSON parse error");
                Box::new(e) as Box<dyn std::error::Error + Send + Sync>
            })
        };

        let json = self
            .retry_with_backoff("fetch_available_sports", operation)
            .await?;

        let sports = Self::parse_available_sports(&json)?;
        debug!(sports_count = sports.len(), "Zenit available sports parsed");
        Ok(sports)
    }

    fn parse_available_sports(
        json: &serde_json::Value,
    ) -> Result<Vec<ZenitSport>, Box<dyn std::error::Error + Send + Sync>> {
        let sports_data = json
            .get("result")
            .and_then(|value| value.get("sport"))
            .and_then(|value| value.as_array())
            .ok_or_else(|| {
                std::io::Error::other("Zenit left-menu response is missing result.sport array")
            })?;

        let mut sports = Vec::new();
        for (index, sport) in sports_data.iter().enumerate() {
            let id = sport
                .get("id")
                .and_then(Self::parse_u64_value)
                .ok_or_else(|| {
                    std::io::Error::other(format!(
                        "Zenit left-menu sport entry {index} has no valid ID"
                    ))
                })?;
            let count = sport
                .get("count")
                .and_then(Self::parse_u64_value)
                .ok_or_else(|| {
                    std::io::Error::other(format!(
                        "Zenit left-menu sport entry {index} has no valid count"
                    ))
                })? as usize;
            if count > 0 {
                sports.push(ZenitSport { id, count });
            }
        }

        if sports.is_empty() {
            return Err(std::io::Error::other(
                "Zenit left-menu response contained no sports with events",
            )
            .into());
        }
        Ok(sports)
    }

    fn parse_numeric_value(value: &serde_json::Value) -> Option<f64> {
        match value {
            serde_json::Value::Number(number) => number.as_f64(),
            serde_json::Value::String(text) => text.trim().replace(',', ".").parse::<f64>().ok(),
            _ => None,
        }
    }

    fn parse_game_dict(
        dict: &serde_json::Map<String, serde_json::Value>,
    ) -> HashMap<String, String> {
        dict.iter()
            .filter_map(|(key, value)| {
                value
                    .as_str()
                    .map(|name| (key.clone(), name.to_string()))
                    .or_else(|| {
                        value
                            .get("name")
                            .and_then(|name| name.as_str())
                            .map(|name| (key.clone(), name.to_string()))
                    })
            })
            .collect()
    }

    fn resolve_league(
        game_obj: &serde_json::Map<String, serde_json::Value>,
        league_names: &HashMap<String, String>,
        tournament_names: &HashMap<String, String>,
        region_names: &HashMap<String, String>,
        info_names: &HashMap<String, String>,
    ) -> String {
        let lid = game_obj.get("lid").and_then(|value| value.as_u64());
        let rid = game_obj.get("rid").and_then(|value| value.as_u64());
        let tid = game_obj.get("tid").and_then(|value| value.as_u64());
        let ti_id = game_obj.get("ti_id").and_then(|value| value.as_u64());

        if let Some(name) = lid
            .map(|value| value.to_string())
            .and_then(|key| league_names.get(&key).cloned())
        {
            return name;
        }

        let mut parts = Vec::new();

        if let Some(region) = rid
            .map(|value| value.to_string())
            .and_then(|key| region_names.get(&key).cloned())
        {
            parts.push(region);
        }

        if let Some(tournament) = tid
            .map(|value| value.to_string())
            .and_then(|key| tournament_names.get(&key).cloned())
        {
            if !parts.iter().any(|part| part == &tournament) {
                parts.push(tournament);
            }
        }

        if let Some(info) = ti_id
            .map(|value| value.to_string())
            .and_then(|key| info_names.get(&key).cloned())
        {
            if !parts.iter().any(|part| part == &info) {
                parts.push(info);
            }
        }

        if parts.is_empty() {
            "Unknown".to_string()
        } else {
            parts.join(". ")
        }
    }

    /// Fetch events from a single endpoint (line or live) for a specific sport
    async fn fetch_sport(
        &self,
        base_url: &str,
        sport_id: u64,
        is_live: bool,
    ) -> Result<(Vec<Event>, Vec<Odd>), Box<dyn std::error::Error + Send + Sync>> {
        let mut all_events = Vec::new();
        let mut all_odds = Vec::new();
        let mut seen_raw_games = HashSet::new();
        let mut seen_events = HashSet::new();
        let mut seen_odds = HashSet::new();
        let mut discovered_game_ids = HashSet::new();
        let mut additional_market_game_ids = HashSet::new();
        let mut offset = 0usize;
        let mut expected_total: Option<usize> = None;

        loop {
            let json = self
                .fetch_page(base_url, sport_id, is_live, offset, None)
                .await?;
            if json.is_null() {
                return Err(std::io::Error::other(format!(
                    "Zenit {mode} response is null for sport {sport_id} at offset {offset}",
                    mode = if is_live { "live" } else { "prematch" }
                ))
                .into());
            }

            if json.get("errorCode").is_some() {
                let msg = json
                    .get("msg")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                return Err(std::io::Error::other(format!(
                    "Zenit API application error for sport {sport_id} at offset {offset}: {msg}"
                ))
                .into());
            }

            if expected_total.is_none() {
                expected_total = json
                    .get("sport")
                    .and_then(|value| value.as_array())
                    .and_then(|sports| {
                        sports.iter().find_map(|sport| {
                            let id = sport.get("id").and_then(Self::parse_u64_value)?;
                            if id == sport_id {
                                sport
                                    .get("count")
                                    .and_then(Self::parse_u64_value)
                                    .map(|value| value as usize)
                            } else {
                                None
                            }
                        })
                    });
            }

            let parsed = Self::parse_response(&json, Some(sport_id), is_live);
            if let Some(schema_error) = parsed.schema_error.as_deref() {
                return Err(std::io::Error::other(format!(
                    "Zenit {mode} response schema error for sport {sport_id} at offset {offset}: {schema_error}",
                    mode = if is_live { "live" } else { "prematch" }
                ))
                .into());
            }
            let previous_raw_count = seen_raw_games.len();

            for game_id in &parsed.discovered_game_ids {
                discovered_game_ids.insert(game_id.clone());
            }
            for game_id in &parsed.additional_market_game_ids {
                additional_market_game_ids.insert(game_id.clone());
            }

            for raw_game_id in parsed.raw_game_ids {
                seen_raw_games.insert(raw_game_id);
            }

            for event in parsed.events {
                if seen_events.insert(event.id.clone()) {
                    all_events.push(event);
                }
            }

            for odd in parsed.odds {
                if seen_odds.insert(odd.id.clone()) {
                    all_odds.push(odd);
                }
            }

            if is_live {
                break;
            }

            let current_raw_count = seen_raw_games.len();
            let reached_expected_total = expected_total
                .map(|expected| current_raw_count >= expected)
                .unwrap_or(false);
            let page_added_new_games = current_raw_count > previous_raw_count;

            if reached_expected_total || !page_added_new_games {
                break;
            }

            offset += Self::PAGE_STEP;
            if expected_total
                .map(|expected| offset > expected + Self::PAGE_STEP)
                .unwrap_or(offset > 2000)
            {
                break;
            }
        }

        let missing_game_ids = discovered_game_ids
            .iter()
            .filter(|game_id| !seen_raw_games.contains(*game_id))
            .cloned()
            .collect::<Vec<_>>();

        if !is_live && !missing_game_ids.is_empty() {
            debug!(
                sport = sport_id,
                missing = missing_game_ids.len(),
                discovered = discovered_game_ids.len(),
                parsed = seen_raw_games.len(),
                "Zenit fetching missing prematch games"
            );

            for chunk in missing_game_ids.chunks(Self::GAME_BATCH_SIZE) {
                let games = chunk.join("-");
                let json = match self
                    .fetch_page(base_url, sport_id, false, 0, Some(&games))
                    .await
                {
                    Ok(json) => json,
                    Err(error) => {
                        warn!(error = %error, sport = sport_id, batch = chunk.len(), "Zenit missing games fetch failed");
                        continue;
                    }
                };

                if json.is_null() {
                    continue;
                }

                let parsed = Self::parse_response(&json, Some(sport_id), false);
                if let Some(schema_error) = parsed.schema_error.as_deref() {
                    return Err(std::io::Error::other(format!(
                        "Zenit missing-game batch schema error for sport {sport_id}: {schema_error}"
                    ))
                    .into());
                }

                for game_id in &parsed.additional_market_game_ids {
                    additional_market_game_ids.insert(game_id.clone());
                }
                for raw_game_id in parsed.raw_game_ids {
                    seen_raw_games.insert(raw_game_id);
                }

                for event in parsed.events {
                    if seen_events.insert(event.id.clone()) {
                        all_events.push(event);
                    }
                }

                for odd in parsed.odds {
                    if seen_odds.insert(odd.id.clone()) {
                        all_odds.push(odd);
                    }
                }
            }
        }

        let unresolved_game_ids = discovered_game_ids.difference(&seen_raw_games).count();
        if unresolved_game_ids > 0 {
            return Err(std::io::Error::other(format!(
                "Zenit prematch pagination left {unresolved_game_ids} discovered games unresolved for sport {sport_id}"
            ))
            .into());
        }

        self.attach_additional_market_details(
            &mut all_events,
            &additional_market_game_ids,
            is_live,
        )
        .await;

        Ok((all_events, all_odds))
    }

    async fn fetch_live(
        &self,
    ) -> Result<(Vec<Event>, Vec<Odd>), Box<dyn std::error::Error + Send + Sync>> {
        let json = self.fetch_live_page().await?;
        if json.is_null() {
            return Err(std::io::Error::other("Zenit live API returned a null response").into());
        }

        if json.get("errorCode").is_some() {
            let msg = json
                .get("msg")
                .and_then(|value| value.as_str())
                .unwrap_or("unknown");
            return Err(format!("Zenit live API application error: {msg}").into());
        }

        let mut parsed = Self::parse_response(&json, None, true);
        if let Some(schema_error) = parsed.schema_error.as_deref() {
            return Err(std::io::Error::other(format!(
                "Zenit live response schema error: {schema_error}"
            ))
            .into());
        }
        let additional_market_game_ids = parsed
            .additional_market_game_ids
            .iter()
            .cloned()
            .collect::<HashSet<_>>();
        self.attach_additional_market_details(
            &mut parsed.events,
            &additional_market_game_ids,
            true,
        )
        .await;

        let linked_odds = parsed
            .odds
            .iter()
            .filter(|odd| parsed.events.iter().any(|event| event.id == odd.event_id))
            .count();
        info!(
            events = parsed.events.len(),
            odds = parsed.odds.len(),
            linked_odds,
            "Zenit live snapshot parsed"
        );
        Ok((parsed.events, parsed.odds))
    }

    /// Parse the JSON response from Zenit API
    fn parse_response(
        json: &serde_json::Value,
        sport_id: Option<u64>,
        is_live: bool,
    ) -> ParsedPage {
        let mut parsed = ParsedPage::default();
        let now = Utc::now();

        let games = match json.get("games").and_then(|value| value.as_object()) {
            Some(games) => games,
            None => {
                parsed.schema_error = Some("response is missing the games object".to_string());
                return parsed;
            }
        };

        let dict = match json.get("dict").and_then(|value| value.as_object()) {
            Some(dict) => dict,
            None => {
                parsed.schema_error = Some("response is missing the dict object".to_string());
                return parsed;
            }
        };

        let team_names = dict
            .get("cmd")
            .and_then(|v| v.as_object())
            .map(Self::parse_game_dict)
            .unwrap_or_default();

        // The bookmaker's sport IDs are not the same as the legacy IDs this
        // parser originally hard-coded. Resolve them from each API response.
        let sport_names = dict
            .get("sport")
            .and_then(|v| v.as_object())
            .map(Self::parse_game_dict)
            .unwrap_or_default();

        let league_names = dict
            .get("league")
            .and_then(|v| v.as_object())
            .map(Self::parse_game_dict)
            .unwrap_or_default();
        let tournament_names = dict
            .get("tournament")
            .and_then(|v| v.as_object())
            .map(Self::parse_game_dict)
            .unwrap_or_default();
        let region_names = dict
            .get("tournament_region")
            .and_then(|v| v.as_object())
            .map(Self::parse_game_dict)
            .unwrap_or_default();
        let info_names = dict
            .get("tournament_info")
            .and_then(|v| v.as_object())
            .map(Self::parse_game_dict)
            .unwrap_or_default();

        let mut discovered_game_ids = HashSet::new();

        if let Some(sports) = json.get("sport") {
            Self::collect_game_ids(sports, &mut discovered_game_ids);
        }

        for (game_id, game) in games {
            let game_obj = match game.as_object() {
                Some(g) => g,
                None => continue,
            };

            parsed.raw_game_ids.push(game_id.clone());

            let advertises_additional_markets = [
                "ross",
                "ross_stats_count",
                "ross_count_with_double_chances",
            ]
            .iter()
            .any(|field| {
                game_obj
                    .get(*field)
                    .and_then(Self::parse_u64_value)
                    .is_some_and(|count| count > 0)
            });
            if advertises_additional_markets {
                parsed.additional_market_game_ids.push(game_id.clone());
            }

            let c1_id = game_obj.get("c1_id").and_then(Self::parse_string_id);
            let c2_id = game_obj.get("c2_id").and_then(Self::parse_string_id);

            let (home, away) = match (c1_id, c2_id) {
                (Some(id1), Some(id2)) => {
                    let h = team_names.get(&id1).cloned().unwrap_or_default();
                    let a = team_names.get(&id2).cloned().unwrap_or_default();
                    if h.is_empty() || a.is_empty() {
                        continue;
                    }
                    (h, a)
                }
                _ => continue,
            };

            let league = Self::resolve_league(
                game_obj,
                &league_names,
                &tournament_names,
                &region_names,
                &info_names,
            );

            let event_sport_id = game_obj
                .get("sid")
                .and_then(Self::parse_u64_value)
                .or(sport_id)
                .unwrap_or_default();
            let sport = sport_names
                .get(&event_sport_id.to_string())
                .map(|name| Self::sport_name_to_sport(name))
                .unwrap_or_else(|| Self::sport_id_to_sport(event_sport_id));

            let start_time = Self::parse_start_time(game_obj);

            let mode = if is_live { "live" } else { "prematch" };
            let event_id = format!("zenit-{}-{}", mode, game_id);

            let headers: Vec<String> = game_obj
                .get("hd")
                .and_then(|value| value.as_array())
                .map(|items| {
                    items
                        .iter()
                        .map(|item| {
                            item.get("n")
                                .and_then(|value| value.as_str())
                                .unwrap_or_default()
                                .to_string()
                        })
                        .collect()
                })
                .unwrap_or_default();

            let odds_data = game_obj.get("f_l").and_then(|value| value.as_array());

            let mut event_odds = Vec::new();

            if let Some(bets) = odds_data {
                for (index, bet) in bets.iter().enumerate() {
                    let bet_obj = match bet.as_object() {
                        Some(b) => b,
                        None => continue,
                    };

                    let header = headers
                        .get(index)
                        .map(|value| value.as_str())
                        .unwrap_or_default();
                    let odd_value = bet_obj
                        .get("h")
                        .and_then(Self::parse_numeric_value)
                        .filter(|value| value.is_finite() && *value > 1.0);

                    let Some(odd_value) = odd_value else {
                        continue;
                    };

                    let odd_id = bet_obj.get("id").and_then(Self::parse_string_id);

                    // Zenit serializes factor codes as strings in prematch responses
                    // but as numbers in the live response. Normalize both wire types.
                    let factor_code = bet_obj.get("o").and_then(Self::parse_string_id);

                    let line = match factor_code.as_deref() {
                        Some("7" | "8" | "10") => Self::adjacent_line_value(bets, index, true),
                        Some("9") => Self::adjacent_line_value(bets, index, false),
                        _ => None,
                    };

                    let mapped = match factor_code.as_deref() {
                        Some("1") if header == "1" => Some(("1X2", "1", OddsType::Home, None)),
                        Some("2") if header == "X" || header == "Х" => {
                            Some(("1X2", "X", OddsType::Draw, None))
                        }
                        Some("3") if header == "2" => Some(("1X2", "2", OddsType::Away, None)),
                        // Live and prematch feeds expose double-chance outcomes as
                        // factors 4/5/6. The first and third headers may use Cyrillic
                        // Х, so accept both spellings while keeping factor/header
                        // agreement strict to avoid mislabelling unrelated markets.
                        Some("4") if matches!(header.trim(), "1X" | "1Х") => {
                            Some(("DoubleChance", "1X", OddsType::Custom, None))
                        }
                        Some("5") if header.trim() == "12" => {
                            Some(("DoubleChance", "12", OddsType::Custom, None))
                        }
                        Some("6") if matches!(header.trim(), "X2" | "Х2") => {
                            Some(("DoubleChance", "X2", OddsType::Custom, None))
                        }
                        Some("7") if header == "1" => {
                            Some(("Handicap", "1", OddsType::Handicap, line))
                        }
                        Some("8") if header == "2" => {
                            Some(("Handicap", "2", OddsType::Handicap, line))
                        }
                        Some("9") if header == "М" => Some(("Total", "U", OddsType::Under, line)),
                        Some("10") if header == "Б" => Some(("Total", "O", OddsType::Over, line)),
                        _ => None,
                    };

                    let Some((market, selection, odds_type, line)) = mapped else {
                        let factor = factor_code.as_deref().unwrap_or("<missing>");
                        let column = if header.trim().is_empty() {
                            "<empty>"
                        } else {
                            header.trim()
                        };
                        let key = format!("factor={factor};header={column}");
                        *parsed.unmapped_factor_counts.entry(key).or_insert(0) += 1;
                        continue;
                    };

                    let line_key = line
                        .map(|value| format!("{value:.6}").replace('-', "m").replace('.', "p"))
                        .unwrap_or_else(|| "no-line".to_string());
                    let source_id = odd_id.unwrap_or_else(|| {
                        factor_code
                            .clone()
                            .unwrap_or_else(|| format!("index-{index}"))
                    });
                    let stable_odd_id = format!(
                        "{event_id}-{source_id}-{market}-{}-{line_key}",
                        selection.to_lowercase().replace([' ', '/'], "_")
                    );

                    event_odds.push(Odd {
                        id: stable_odd_id,
                        event_id: event_id.clone(),
                        bookmaker_slug: "zenit".to_string(),
                        market: market.into(),
                        selection: selection.into(),
                        odds: odd_value,
                        odds_type,
                        line,
                        timestamp: now,
                    });
                }
            }

            parsed.odds.extend(event_odds);

            parsed.events.push(Event {
                id: event_id.clone(),
                sport,
                league,
                home_team: home.clone(),
                away_team: away.clone(),
                start_time,
                is_live,
                bookmaker_slug: "zenit".to_string(),
                raw_url: Some("https://zenit.win".to_string()),
                extra: HashMap::new(),
            });
        }

        parsed.discovered_game_ids = discovered_game_ids.into_iter().collect();

        for (factor_context, count) in &parsed.unmapped_factor_counts {
            warn!(
                sport_id = ?sport_id,
                is_live,
                factor_context,
                count,
                "Zenit direct feed contains a priced factor that is not yet classified; quote omitted"
            );
        }

        parsed
    }

    pub(crate) async fn fetch_runtime_data(
        &self,
    ) -> Result<(Vec<Event>, Vec<Odd>), Box<dyn std::error::Error + Send + Sync>> {
        let result = self.fetch_all().await?;
        Ok((result.events, result.odds))
    }

    fn sport_name_to_sport(name: &str) -> Sport {
        match name.trim().to_lowercase().as_str() {
            "бои" => Sport::Mma,
            "футзал" => Sport::Futsal,
            "велоспорт" => Sport::Cycling,
            "формула 1" => Sport::Motorsport,
            "горные лыжи" | "фристайл" => Sport::WinterSports,
            "баскетбол 3x3" | "баскетбол 3х3" => Sport::Basketball,
            "киберфутбол"
            | "кибербаскетбол"
            | "кибербои"
            | "кибертеннис"
            | "киберволейбол"
            | "киберхоккей" => Sport::Esports,
            "падел-теннис" => Sport::Tennis,
            "автогонки" | "мотогонки" => Sport::Motorsport,
            "кикбоксинг" | "тайский бокс" => Sport::Boxing,
            "австралийский футбол" => Sport::AussieRules,
            "хоккей с мячом"
            | "американский футбол"
            | "шахматы"
            | "вольная борьба"
            | "херлинг"
            | "пелота"
            | "пул"
            | "олимпиада" => Sport::Other,
            _ => Sport::from_str(name),
        }
    }

    // Fallback for payload variants that omit dict.sport. These IDs match
    // Zenit's current source catalogue; dynamic names above remain authoritative.
    fn sport_id_to_sport(id: u64) -> Sport {
        match id {
            1 => Sport::Football,
            2 => Sport::Hockey,
            3 | 701 => Sport::Basketball,
            4 => Sport::Baseball,
            6 => Sport::Tennis,
            7 | 561 | 563 | 565 | 567 | 570 | 572 => Sport::Esports,
            8 => Sport::Mma,
            9 | 536 | 555 => Sport::Motorsport,
            11 | 671 | 684 => Sport::Boxing,
            12 => Sport::Cycling,
            13 => Sport::Futsal,
            14 => Sport::Volleyball,
            17 => Sport::Handball,
            22 | 50 | 58 | 331 => Sport::WinterSports,
            34 => Sport::TableTennis,
            43 => Sport::WaterPolo,
            176 | 708 => Sport::Other,
            225 => Sport::Rugby,
            264 => Sport::Darts,
            335 => Sport::Badminton,
            532 => Sport::Cricket,
            540 => Sport::Floorball,
            673 => Sport::Tennis,
            _ => Sport::Other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ZenitParser;
    use chrono::{Datelike, Timelike, Utc};
    use serde_json::json;
    use std::sync::Arc;
    use std::time::Duration;

    #[test]
    fn additional_market_counters_are_collected_only_when_positive() {
        let payload = json!({
            "games": {
                "77": {
                    "sid": 1,
                    "c1_id": "101",
                    "c2_id": "202",
                    "ross": "4",
                    "ross_stats_count": 0,
                    "ross_count_with_double_chances": "6",
                    "hd": [],
                    "f_l": []
                },
                "78": {
                    "sid": 1,
                    "c1_id": "101",
                    "c2_id": "202",
                    "ross": 0,
                    "ross_stats_count": 0,
                    "ross_count_with_double_chances": 0,
                    "hd": [],
                    "f_l": []
                }
            },
            "dict": {
                "cmd": {"101": "Home", "202": "Away"},
                "sport": {"1": "Футбол"}
            }
        });

        let parsed = ZenitParser::parse_response(&payload, Some(1), false);
        assert_eq!(parsed.schema_error, None);
        assert_eq!(parsed.events.len(), 2);
        assert_eq!(parsed.additional_market_game_ids, vec!["77".to_string()]);
    }

    #[test]
    fn additional_market_endpoint_matches_first_party_frontend_routes() {
        assert_eq!(
            ZenitParser::ross_details_url(false, "imprint"),
            "https://zenit.win/ajax/line/ross/data/"
        );
        assert_eq!(
            ZenitParser::ross_details_url(true, "imprint"),
            "https://zenit.win/ajax/live/ross/data/imprint/"
        );

        let query = ZenitParser::ross_details_query(
            &["77".to_string(), "88".to_string()],
            "1.72.1",
        )
        .into_iter()
        .collect::<HashMap<_, _>>();
        assert_eq!(query.get("onlyview").map(String::as_str), Some("0"));
        assert_eq!(query.get("gid").map(String::as_str), Some("77-88"));
        assert_eq!(query.get("lang_id").map(String::as_str), Some("1"));
        assert_eq!(query.get("client_v").map(String::as_str), Some("1.72.1"));
        assert_eq!(query.get("show_from_main").map(String::as_str), Some("0"));
    }

    #[test]
    fn attaches_expanded_market_payload_to_the_matching_source_event() {
        let source = json!({
            "games": {"77": {
                "sid": 1,
                "c1_id": "101",
                "c2_id": "202",
                "ross": 2,
                "hd": [],
                "f_l": []
            }},
            "dict": {
                "cmd": {"101": "Home", "202": "Away"},
                "sport": {"1": "Футбол"}
            }
        });
        let mut parsed = ZenitParser::parse_response(&source, Some(1), false);
        let expanded = json!({
            "data": {"order": ["total"], "data": {}},
            "other": [{"id": "bet-1", "name": "Test market", "cf": "2.10"}],
            "filter": {"77": []}
        });
        let payloads = HashMap::from([("77".to_string(), expanded.clone())]);

        assert_eq!(
            ZenitParser::attach_ross_payloads(&mut parsed.events, payloads),
            1
        );
        assert_eq!(
            parsed.events[0].extra.get("zenit_additional_markets"),
            Some(&expanded)
        );
    }

    #[test]
    fn is_transient_error_detects_timeout() {
        assert!(ZenitParser::is_transient_error("timeout"));
        assert!(ZenitParser::is_transient_error("operation timed out"));
        assert!(ZenitParser::is_transient_error("request timeout"));
    }

    #[test]
    fn is_transient_error_detects_connection_errors() {
        assert!(ZenitParser::is_transient_error("connection reset"));
        assert!(ZenitParser::is_transient_error("ConnectError"));
        assert!(ZenitParser::is_transient_error(
            "Temporary failure in name resolution"
        ));
    }

    #[test]
    fn is_transient_error_detects_server_errors() {
        assert!(ZenitParser::is_transient_error("429"));
        assert!(ZenitParser::is_transient_error("502"));
        assert!(ZenitParser::is_transient_error("503"));
        assert!(ZenitParser::is_transient_error("504"));
        assert!(ZenitParser::is_transient_error("Too Many Requests"));
    }

    #[test]
    fn is_transient_error_rejects_permanent_errors() {
        assert!(!ZenitParser::is_transient_error("404 Not Found"));
        assert!(!ZenitParser::is_transient_error("400 Bad Request"));
        assert!(!ZenitParser::is_transient_error("401 Unauthorized"));
        assert!(!ZenitParser::is_transient_error("JSON parsing error"));
    }

    #[test]
    fn backoff_duration_increases_exponentially() {
        let d0 = ZenitParser::backoff_duration(0).as_millis() as u64;
        let d1 = ZenitParser::backoff_duration(1).as_millis() as u64;
        let d2 = ZenitParser::backoff_duration(2).as_millis() as u64;

        assert_eq!(d0, 500); // INITIAL_BACKOFF_MS
        assert_eq!(d1, 1000); // 500 * 2^1
        assert_eq!(d2, 2000); // 500 * 2^2

        // Test that it caps at MAX_BACKOFF_MS
        let d_high = ZenitParser::backoff_duration(10).as_millis() as u64;
        assert_eq!(d_high, 5000); // MAX_BACKOFF_MS
    }

    #[test]
    fn line_query_matches_browser_capture_shape() {
        let query = ZenitParser::line_query(150, 3, Some("111-222"));
        let query = query
            .into_iter()
            .collect::<std::collections::HashMap<_, _>>();

        assert_eq!(query.get("all").map(String::as_str), Some("0"));
        assert_eq!(query.get("onlyview").map(String::as_str), Some("0"));
        assert!(
            !query.contains_key("popular"),
            "normal sport-line requests must not be forced into the popular-only view"
        );
        let expected_front_version = ZenitParser::frontversion();
        assert_eq!(
            query.get("client_v"),
            Some(&expected_front_version),
            "Zenit line requests must use the configured browser build version"
        );
        assert_eq!(query.get("length").map(String::as_str), Some("3000"));
        assert_eq!(query.get("offset").map(String::as_str), Some("150"));
        assert_eq!(query.get("sport").map(String::as_str), Some("3"));
        assert_eq!(query.get("games").map(String::as_str), Some("111-222"));
        assert!(!query.contains_key("dict"));
        assert!(!query.contains_key("pagination"));
    }

    #[test]
    fn rejects_missing_zenit_games_or_dictionary_objects() {
        let missing_games = ZenitParser::parse_response(&json!({"dict": {}}), Some(1), false);
        assert_eq!(
            missing_games.schema_error.as_deref(),
            Some("response is missing the games object")
        );
        let missing_dict = ZenitParser::parse_response(&json!({"games": {}}), Some(1), false);
        assert_eq!(
            missing_dict.schema_error.as_deref(),
            Some("response is missing the dict object")
        );
    }

    #[test]
    fn available_sports_requires_complete_inventory_and_parses_string_numbers() {
        let payload = json!({
            "result": {"sport": [
                {"id": "5", "count": "17"},
                {"id": 1, "count": 0}
            ]}
        });
        let sports = ZenitParser::parse_available_sports(&payload).expect("valid menu");
        assert_eq!(sports.len(), 1);
        assert_eq!(sports[0].id, 5);
        assert_eq!(sports[0].count, 17);

        let missing = ZenitParser::parse_available_sports(&json!({"result": {}}))
            .expect_err("missing inventory must fail");
        assert!(missing.to_string().contains("result.sport array"));

        let empty = ZenitParser::parse_available_sports(&json!({
            "result": {"sport": [{"id": 1, "count": 0}]}
        }))
        .expect_err("empty inventory must fail");
        assert!(empty.to_string().contains("no sports with events"));
    }

    #[test]
    fn odd_ids_are_scoped_by_event_market_selection_and_line() {
        let payload = json!({
            "games": {"77": {
                "sid": 1,
                "c1_id": "101",
                "c2_id": "202",
                "f_l": [
                    {"o": "10", "h": "1.90", "id": "same-source-id"},
                    {"h": "1.5"},
                    {},
                    {"h": "2.5"},
                    {"o": "10", "h": "1.95", "id": "same-source-id"}
                ],
                "hd": [
                    {"n": "Б"}, {"n": ""}, {"n": ""}, {"n": ""}, {"n": "Б"}
                ]
            }},
            "dict": {"cmd": {"101": "Home", "202": "Away"}}
        });

        let parsed = ZenitParser::parse_response(&payload, Some(1), false);
        assert_eq!(parsed.schema_error, None);
        assert_eq!(parsed.odds.len(), 2);
        assert!(parsed.odds.iter().any(|odd| odd.line == Some(1.5)));
        assert!(parsed.odds.iter().any(|odd| odd.line == Some(2.5)));
        let ids = parsed
            .odds
            .iter()
            .map(|odd| &odd.id)
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(ids.len(), 2);
        assert!(parsed
            .odds
            .iter()
            .all(|odd| odd.id.contains("zenit-prematch-77")));
    }

    #[test]
    fn reports_unknown_priced_factor_codes_without_fabricating_market_types() {
        let payload = json!({
            "games": {
                "77": {
                    "sid": 1,
                    "c1_id": "101",
                    "c2_id": "202",
                    "f_l": [
                        {"o": "42", "h": "1.90", "id": "unknown-factor-row"}
                    ],
                    "hd": [{"n": "Корнеры"}]
                }
            },
            "dict": {
                "cmd": {"101": "Home", "202": "Away"}
            }
        });

        let parsed = ZenitParser::parse_response(&payload, Some(1), false);
        assert_eq!(parsed.schema_error, None);
        assert_eq!(parsed.events.len(), 1);
        assert!(
            parsed.odds.is_empty(),
            "unclassified factor codes must not become actionable odds"
        );
        assert_eq!(
            parsed.unmapped_factor_counts.get("factor=42;header=Корнеры"),
            Some(&1)
        );
    }

    #[test]
    fn parse_response_supports_string_dates_and_numeric_strings() {
        let payload = json!({
            "games": {
                "42": {
                    "sid": "4",
                    "lid": 100,
                    "rid": "7",
                    "tid": "12",
                    "time": "1776031200",
                    "date": "13/04 01:00",
                    "c1_id": "101",
                    "c2_id": 202,
                    "f_l": [
                        {"o": "1", "h": "1.70", "id": "o1"},
                        {},
                        {"o": "3", "h": "2.05", "id": "o3"},
                        {},
                        {},
                        {},
                        {"h": "-2.5"},
                        {"o": "7", "h": "1.90", "id": "o7"},
                        {"h": "2.5"},
                        {"o": "8", "h": "1.85", "id": "o8"},
                        {"o": "9", "h": "1.80", "id": "o9"},
                        {"h": "22.5"},
                        {"o": "10", "h": "1.95", "id": "o10"}
                    ],
                    "hd": [
                        {"n": "1"}, {"n": "Х"}, {"n": "2"}, {"n": "1Х"}, {"n": "12"}, {"n": "Х2"},
                        {"n": "Фора"}, {"n": "1"}, {"n": "Фора"}, {"n": "2"}, {"n": "М"}, {"n": "Тотал"}, {"n": "Б"}
                    ]
                }
            },
            "dict": {
                "cmd": {"101": "Home", "202": "Away"},
                "sport": {"4": "Бейсбол"},
                "league": {"100": "League"},
                "tournament": {"12": {"name": "Tournament"}},
                "tournament_region": {"7": {"name": "Region"}},
                "tournament_info": {}
            }
        });

        let parsed = ZenitParser::parse_response(&payload, Some(5), false);

        assert_eq!(parsed.events.len(), 1);
        assert_eq!(parsed.odds.len(), 6);
        assert_eq!(parsed.events[0].home_team, "Home");
        assert_eq!(parsed.events[0].away_team, "Away");
        assert_eq!(parsed.events[0].sport, shared::Sport::Baseball);
        assert_eq!(parsed.events[0].league, "League");
        assert_eq!(
            parsed.events[0].start_time.map(|dt| dt.timestamp()),
            Some(1776031200)
        );
        assert!(parsed
            .odds
            .iter()
            .any(|odd| odd.market == "1X2" && odd.selection == "1"));
        assert!(parsed
            .odds
            .iter()
            .any(|odd| odd.market == "1X2" && odd.selection == "2"));
        assert!(parsed
            .odds
            .iter()
            .any(|odd| odd.market == "Handicap" && odd.selection == "1" && odd.line == Some(-2.5)));
        assert!(parsed
            .odds
            .iter()
            .any(|odd| odd.market == "Handicap" && odd.selection == "2" && odd.line == Some(2.5)));
        assert!(parsed
            .odds
            .iter()
            .any(|odd| odd.market == "Total" && odd.selection == "U" && odd.line == Some(22.5)));
        assert!(parsed
            .odds
            .iter()
            .any(|odd| odd.market == "Total" && odd.selection == "O" && odd.line == Some(22.5)));
    }

    #[test]
    fn maps_sports_from_the_current_bookmaker_dictionary() {
        for (source_name, expected) in [
            ("Футбол", shared::Sport::Football),
            ("Баскетбол", shared::Sport::Basketball),
            ("Бокс", shared::Sport::Boxing),
            ("Бои", shared::Sport::Mma),
            ("Теннис", shared::Sport::Tennis),
            ("Крикет", shared::Sport::Cricket),
            ("Футзал", shared::Sport::Futsal),
            ("Австралийский футбол", shared::Sport::AussieRules),
            ("Хоккей", shared::Sport::Hockey),
            ("Киберспорт", shared::Sport::Esports),
            ("Гандбол", shared::Sport::Handball),
            ("Регби", shared::Sport::Rugby),
            ("Водное поло", shared::Sport::WaterPolo),
            ("Волейбол", shared::Sport::Volleyball),
            ("Формула 1", shared::Sport::Motorsport),
            ("Настольный теннис", shared::Sport::TableTennis),
            ("Падел-теннис", shared::Sport::Tennis),
            ("Киберфутбол", shared::Sport::Esports),
            ("Кибербаскетбол", shared::Sport::Esports),
            ("Автогонки", shared::Sport::Motorsport),
            ("Флорбол", shared::Sport::Floorball),
            ("Горные лыжи", shared::Sport::WinterSports),
        ] {
            assert_eq!(
                ZenitParser::sport_name_to_sport(source_name),
                expected,
                "unexpected sport mapping for {source_name}"
            );
        }
    }

    #[test]
    fn fallback_sport_ids_match_the_current_bookmaker_catalogue() {
        for (source_id, expected) in [
            (1, shared::Sport::Football),
            (2, shared::Sport::Hockey),
            (3, shared::Sport::Basketball),
            (4, shared::Sport::Baseball),
            (6, shared::Sport::Tennis),
            (7, shared::Sport::Esports),
            (8, shared::Sport::Mma),
            (9, shared::Sport::Motorsport),
            (11, shared::Sport::Boxing),
            (12, shared::Sport::Cycling),
            (13, shared::Sport::Futsal),
            (14, shared::Sport::Volleyball),
            (17, shared::Sport::Handball),
            (22, shared::Sport::WinterSports),
            (34, shared::Sport::TableTennis),
            (43, shared::Sport::WaterPolo),
            (225, shared::Sport::Rugby),
            (264, shared::Sport::Darts),
            (335, shared::Sport::Badminton),
            (532, shared::Sport::Cricket),
            (540, shared::Sport::Floorball),
            (673, shared::Sport::Tennis),
            (701, shared::Sport::Basketball),
        ] {
            assert_eq!(
                ZenitParser::sport_id_to_sport(source_id),
                expected,
                "unexpected fallback mapping for source ID {source_id}"
            );
        }
    }

    #[test]
    fn parses_double_chance_factors_with_latin_and_cyrillic_headers() {
        let payload = json!({
            "games": {
                "9001": {
                    "sid": 1,
                    "c1_id": 101,
                    "c2_id": 202,
                    "f_l": [
                        {"o": 4, "h": 1.31, "id": "dc-1x"},
                        {"o": "5", "h": "1.42", "id": "dc-12"},
                        {"o": 6, "h": 1.29, "id": "dc-x2"}
                    ],
                    "hd": [
                        {"n": "1Х"},
                        {"n": "12"},
                        {"n": "Х2"}
                    ]
                }
            },
            "dict": {
                "cmd": {"101": "Home", "202": "Away"}
            }
        });

        let parsed = ZenitParser::parse_response(&payload, Some(1), true);

        assert_eq!(parsed.events.len(), 1);
        assert_eq!(parsed.odds.len(), 3);
        for (selection, expected_id) in [("1X", "dc-1x"), ("12", "dc-12"), ("X2", "dc-x2")] {
            let odd = parsed
                .odds
                .iter()
                .find(|odd| odd.selection == selection)
                .unwrap_or_else(|| panic!("missing double-chance selection {selection}"));
            assert_eq!(odd.market, "DoubleChance");
            assert_eq!(odd.odds_type, shared::odds::OddsType::Custom);
            assert!(odd.id.contains(expected_id));
        }
    }

    #[test]
    fn parse_date_value_accepts_short_formats() {
        let now = Utc::now();

        let parsed =
            ZenitParser::parse_date_value(&json!("04/13 01:00")).expect("short date parses");

        assert_eq!(parsed.hour(), 1);
        assert_eq!(parsed.minute(), 0);
        assert_eq!(parsed.month(), 4);
        assert_eq!(parsed.day(), 13);
        assert!(parsed.year() == now.year() || parsed.year() == now.year() + 1);
    }

    #[tokio::test]
    #[ignore = "runtime-only network diagnostic"]
    async fn zenit_runtime_counts_against_live_output() {
        let client = Arc::new(
            reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .user_agent(ZenitParser::USER_AGENT)
                .build()
                .expect("client"),
        );
        let parser = ZenitParser::new(client);
        let (events, _odds) = parser.fetch_runtime_data().await.expect("runtime fetch");
        let live = events.iter().filter(|event| event.is_live).count();
        let prematch = events.len().saturating_sub(live);

        println!(
            "zenit runtime counts: live={}, prematch={}, total={}",
            live,
            prematch,
            events.len()
        );
    }

    #[tokio::test]
    #[ignore = "runtime-only network diagnostic"]
    async fn zenit_runtime_request_branch_probe() {
        let client = Arc::new(
            reqwest::Client::builder()
                .timeout(Duration::from_secs(40))
                .user_agent(ZenitParser::USER_AGENT)
                .build()
                .expect("client"),
        );
        let parser = ZenitParser::new(client);

        let sports = parser
            .fetch_available_sports()
            .await
            .expect("left menu fetch");
        let line = parser
            .fetch_page(
                "https://zenit.win/ajax/line/printer/react",
                ZenitParser::SPORT_FOOTBALL,
                false,
                0,
                None,
            )
            .await
            .expect("football line fetch");
        let live = parser.fetch_live_page().await.expect("live fetch");

        let line_raw_games = line
            .get("games")
            .and_then(|value| value.as_object())
            .map(|games| games.len())
            .unwrap_or_default();
        let live_raw_games = live
            .get("games")
            .and_then(|value| value.as_object())
            .map(|games| games.len())
            .unwrap_or_default();
        let line_parsed =
            ZenitParser::parse_response(&line, Some(ZenitParser::SPORT_FOOTBALL), false);
        let live_parsed = ZenitParser::parse_response(&live, None, true);

        println!(
            "zenit branch probe: sports={}, football_line_raw={}, football_line_events={}, football_line_odds={}, live_raw={}, live_events={}, live_odds={}",
            sports.len(),
            line_raw_games,
            line_parsed.events.len(),
            line_parsed.odds.len(),
            live_raw_games,
            live_parsed.events.len(),
            live_parsed.odds.len()
        );

        assert!(
            !sports.is_empty(),
            "left_menu branch returned zero sports; prematch fanout would be skipped"
        );
        assert!(
            !line_parsed.events.is_empty(),
            "football line branch returned zero parsed events"
        );
        assert!(
            !live_parsed.events.is_empty(),
            "live branch returned zero parsed events"
        );
    }
}

#[async_trait]
impl BookmakerParser for ZenitParser {
    fn name(&self) -> &str {
        "Zenit"
    }
    fn slug(&self) -> &str {
        "zenit"
    }
    fn is_enabled(&self) -> bool {
        true
    }

    fn readiness(&self) -> Option<ParserReadiness> {
        Some(Self::readiness_snapshot())
    }

    async fn fetch_events(&self) -> Result<Vec<Event>, Box<dyn std::error::Error + Send + Sync>> {
        let sport_ids = self.fetch_available_sports().await?;
        let mut all_events = Vec::new();
        let mut errors = Vec::new();

        for sport in sport_ids {
            match self
                .fetch_sport("https://zenit.win/ajax/line/printer/react", sport.id, false)
                .await
            {
                Ok((events, _)) => all_events.extend(events),
                Err(error) => errors.push(format!("prematch sport {}: {error}", sport.id)),
            }
        }

        match self.fetch_live().await {
            Ok((events, _)) => all_events.extend(events),
            Err(error) => errors.push(format!("live: {error}")),
        }

        if !errors.is_empty() {
            return Err(std::io::Error::other(errors.join("; ")).into());
        }
        info!(count = all_events.len(), "Zenit events parsed");
        Ok(all_events)
    }

    async fn fetch_odds(
        &self,
        _event_id: &str,
    ) -> Result<Vec<Odd>, Box<dyn std::error::Error + Send + Sync>> {
        let sport_ids = self.fetch_available_sports().await?;
        let mut all_odds = Vec::new();
        let mut errors = Vec::new();

        for sport in sport_ids {
            match self
                .fetch_sport("https://zenit.win/ajax/line/printer/react", sport.id, false)
                .await
            {
                Ok((_, odds)) => all_odds.extend(odds),
                Err(error) => errors.push(format!("prematch sport {}: {error}", sport.id)),
            }
        }

        match self.fetch_live().await {
            Ok((_, odds)) => all_odds.extend(odds),
            Err(error) => errors.push(format!("live: {error}")),
        }

        if !errors.is_empty() {
            return Err(std::io::Error::other(errors.join("; ")).into());
        }
        Ok(all_odds)
    }

    async fn fetch_all(&self) -> Result<ParserResult, Box<dyn std::error::Error + Send + Sync>> {
        let start = std::time::Instant::now();
        let sport_ids = self.fetch_available_sports().await?;
        let mut all_events = Vec::new();
        let mut all_odds = Vec::new();
        let mut errors = Vec::new();

        for sport in sport_ids {
            match self
                .fetch_sport("https://zenit.win/ajax/line/printer/react", sport.id, false)
                .await
            {
                Ok((events, odds)) => {
                    debug!(
                        sport_id = sport.id,
                        advertised_event_count = sport.count,
                        parsed_event_count = events.len(),
                        "Zenit sport catalogue completeness"
                    );
                    all_events.extend(events);
                    all_odds.extend(odds);
                }
                Err(error) => errors.push(format!("prematch sport {}: {error}", sport.id)),
            }
        }

        match self.fetch_live().await {
            Ok((events, odds)) => {
                all_events.extend(events);
                all_odds.extend(odds);
            }
            Err(error) => errors.push(format!("live: {error}")),
        }

        if !errors.is_empty() {
            return Err(std::io::Error::other(errors.join("; ")).into());
        }

        let elapsed = start.elapsed().as_millis() as u64;
        debug!(
            events = all_events.len(),
            odds = all_odds.len(),
            time_ms = elapsed,
            "Zenit fetch complete"
        );
        Ok(ParserResult::new("zenit", all_events, all_odds, elapsed))
    }

    fn base_url(&self) -> &str {
        "https://zenit.win"
    }
    fn user_agent(&self) -> &str {
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36"
    }
}
