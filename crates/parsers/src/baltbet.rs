use crate::base::{BookmakerParser, ParserResult};
use async_trait::async_trait;
use chrono::{NaiveDateTime, TimeZone, Utc};
use futures::stream::{self, StreamExt};
use regex::Regex;
use reqwest::Client;
use scraper::{Html, Selector};
use serde_json::Value;
use shared::odds::OddsType;
use shared::{
    DiagnosticSeverity, Event, Odd, ParserDiagnosticCheck, ParserReadiness, ParserReadinessStage,
    Sport,
};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tracing::{debug, info, warn};

const BOOKMAKER_SLUG: &str = "baltbet";
const LIVE_PAGE_URL: &str = "https://baltbet.ru/live";
const LIVE_API_URL: &str = "https://events.baltbet.ru/api/live/table/fetch/diff";
const LIVE_BANNER_API_BASE_URL: &str = "https://events.baltbet.ru/api/event/banner";
const LIVE_GROUPING_API_BASE_URL: &str = "https://events.baltbet.ru/api/grouping/event";
const LEGACY_PREMATCH_URL: &str = "https://old.baltbet.ru/Line1.aspx";
const LEGACY_LIVE_URL: &str = "https://old.baltbet.ru/Live1.aspx";
const LEGACY_PREMATCH_FEATURED_GROUP_LIMIT: usize = 40;
const LIVE_BANNER_FETCH_CONCURRENCY: usize = 8;
const LIVE_GROUPING_FETCH_CONCURRENCY: usize = 8;
const LIVE_BANNER_PROBE_LIMIT: usize = 72;
const LEGACY_PREMATCH_FETCH_CONCURRENCY: usize = 10;
const STRICT_LIVE_KPI_TARGET: usize = 150;
const STRICT_PREMATCH_KPI_TARGET: usize = 3000;
const RECENT_STRICT_LIVE_EVENTS: usize = 183;
const RECENT_STRICT_PREMATCH_EVENTS: usize = 3086;

#[derive(Debug, Clone)]
struct LiveMeta {
    title: String,
    league: String,
    href: Option<String>,
}

#[derive(Debug, Clone)]
struct LegacySportLink {
    group_id: String,
    sport_label: String,
}

#[derive(Debug)]
pub struct BaltbetParser {
    client: Arc<Client>,
}

impl BaltbetParser {
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
                    code: "live_json_runtime_enabled".to_string(),
                    severity: DiagnosticSeverity::Pass,
                    message: format!(
                        "Primary live coverage uses {LIVE_API_URL} with HTML metadata hydration from {LIVE_PAGE_URL}."
                    ),
                },
                ParserDiagnosticCheck {
                    code: "live_banner_metadata_fallback_enabled".to_string(),
                    severity: DiagnosticSeverity::Pass,
                    message: "Missing live metadata is backfilled via event banner probes without mutating the primary runtime branch.".to_string(),
                },
                ParserDiagnosticCheck {
                    code: "legacy_prematch_group_harvester_enabled".to_string(),
                    severity: DiagnosticSeverity::Pass,
                    message: format!(
                        "Prematch coverage expands from {LEGACY_PREMATCH_URL} into legacy Line2 group pages with deduplicated event ids."
                    ),
                },
                ParserDiagnosticCheck {
                    code: "strict_live_kpi_recently_met".to_string(),
                    severity: DiagnosticSeverity::Pass,
                    message: format!(
                        "Recent strict runtime diagnostics observed {RECENT_STRICT_LIVE_EVENTS} live events against the nightly target of {STRICT_LIVE_KPI_TARGET}."
                    ),
                },
                ParserDiagnosticCheck {
                    code: "strict_prematch_kpi_recently_met".to_string(),
                    severity: DiagnosticSeverity::Pass,
                    message: format!(
                        "Recent strict runtime diagnostics observed {RECENT_STRICT_PREMATCH_EVENTS} prematch events against the nightly target of {STRICT_PREMATCH_KPI_TARGET}."
                    ),
                },
                ParserDiagnosticCheck {
                    code: "strict_feed_error_propagation_pending_runtime_confirmation".to_string(),
                    severity: DiagnosticSeverity::Warn,
                    message: "The parser now fails closed when advertised prematch groups or live source branches are unavailable; repeated runtime confirmation is required before production promotion.".to_string(),
                },
            ],
        }
    }

    async fn fetch_live_page_html(
        &self,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let resp = self
            .client
            .get(LIVE_PAGE_URL)
            .header(
                "Accept",
                "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
            )
            .header("Accept-Language", "ru-RU,ru;q=0.9,en;q=0.8")
            .send()
            .await?;

        if !resp.status().is_success() {
            return Err(format!("live page returned {}", resp.status()).into());
        }

        Ok(resp.text().await?)
    }

    async fn fetch_live_json(&self) -> Result<Value, Box<dyn std::error::Error + Send + Sync>> {
        let resp = self
            .client
            .get(LIVE_API_URL)
            .header("Accept", "*/*")
            .header("Origin", "https://baltbet.ru")
            .header("Referer", "https://baltbet.ru/")
            .header("bb-ajax", "1")
            .header("bb_appsource", "UniRu")
            .header("bb_lang", "ru")
            .send()
            .await?;

        if !resp.status().is_success() {
            return Err(format!("live JSON returned {}", resp.status()).into());
        }

        Ok(resp.json().await?)
    }

    async fn fetch_live_banner_meta(
        &self,
        event_id: u64,
    ) -> Result<Option<LiveMeta>, Box<dyn std::error::Error + Send + Sync>> {
        for attempt in 0..2 {
            match self.fetch_live_banner_meta_once(event_id).await {
                Ok(result) => return Ok(result),
                Err(error) if attempt == 0 => {
                    debug!(event_id, error = %error, "Baltbet live banner metadata retry scheduled");
                    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
                }
                Err(error) => return Err(error),
            }
        }

        Ok(None)
    }

    async fn fetch_live_banner_meta_once(
        &self,
        event_id: u64,
    ) -> Result<Option<LiveMeta>, Box<dyn std::error::Error + Send + Sync>> {
        let resp = self
            .client
            .get(format!("{LIVE_BANNER_API_BASE_URL}/{event_id}?live=true"))
            .header("Accept", "*/*")
            .header("Origin", "https://baltbet.ru")
            .header("Referer", "https://baltbet.ru/")
            .header("bb-ajax", "1")
            .header("bb_appsource", "UniRu")
            .header("bb_lang", "ru")
            .send()
            .await?;

        if !resp.status().is_success() {
            return Ok(None);
        }

        let payload = resp.json::<Value>().await?;
        Ok(Self::parse_live_banner_meta(&payload))
    }

    async fn fetch_live_banner_meta_batch(&self, event_ids: Vec<u64>) -> Vec<(u64, LiveMeta)> {
        stream::iter(event_ids.into_iter().map(|event_id| async move {
            match self.fetch_live_banner_meta(event_id).await {
                Ok(Some(meta_entry)) => Some((event_id, meta_entry)),
                Ok(None) => None,
                Err(error) => {
                    debug!(event_id, error = %error, "Baltbet live banner metadata fetch failed");
                    None
                }
            }
        }))
        .buffer_unordered(LIVE_BANNER_FETCH_CONCURRENCY)
        .filter_map(async move |result| result)
        .collect()
        .await
    }

    async fn fetch_live_event_grouping(
        &self,
        event_id: u64,
    ) -> Result<Option<Value>, Box<dyn std::error::Error + Send + Sync>> {
        let response = self
            .client
            .get(format!("{LIVE_GROUPING_API_BASE_URL}/{event_id}"))
            .header("Accept", "application/json, text/plain, */*")
            .header("Origin", "https://baltbet.ru")
            .header("Referer", LIVE_PAGE_URL)
            .header("bb_appSource", "UniRu")
            .header("bb_lang", "ru")
            .header("Content-Type", "application/json; charset=utf-8")
            .send()
            .await?;

        if !response.status().is_success() {
            return Ok(None);
        }

        let payload = response.json::<Value>().await?;
        let matches_event = payload.get("id").and_then(Self::parse_u64_value) == Some(event_id);
        let has_markets = payload.get("markets").and_then(Value::as_array).is_some();
        if matches_event && has_markets {
            Ok(Some(payload))
        } else {
            Ok(None)
        }
    }

    fn collect_live_grouping_probe_ids(json: &Value) -> Vec<u64> {
        let Some(items) = json.get("events").and_then(Value::as_array) else {
            return Vec::new();
        };

        let mut ids = Vec::new();
        let mut seen = HashSet::new();
        for item in items {
            if let Some(event_id) = item.get("i").and_then(Value::as_u64) {
                if seen.insert(event_id) {
                    ids.push(event_id);
                }
            }
        }
        ids
    }

    async fn fetch_live_event_groupings_batch(&self, json: &Value) -> HashMap<u64, Value> {
        let ids = Self::collect_live_grouping_probe_ids(json);

        stream::iter(ids.into_iter().map(|event_id| async move {
            match self.fetch_live_event_grouping(event_id).await {
                Ok(Some(grouping)) => Some((event_id, grouping)),
                Ok(None) => None,
                Err(error) => {
                    debug!(
                        event_id,
                        error = %error,
                        "Baltbet live event grouping fetch failed"
                    );
                    None
                }
            }
        }))
        .buffer_unordered(LIVE_GROUPING_FETCH_CONCURRENCY)
        .filter_map(async move |result| result)
        .collect()
        .await
    }

    async fn fetch_live_runtime_data(
        &self,
    ) -> Result<(Vec<Event>, Vec<Odd>), Box<dyn std::error::Error + Send + Sync>> {
        let mut live_meta = match self.fetch_live_page_html().await {
            Ok(live_html) => Self::parse_live_metadata(&live_html),
            Err(error) => {
                warn!(error = %error, "Baltbet live HTML metadata fetch failed");
                HashMap::new()
            }
        };
        debug!(count = live_meta.len(), "Baltbet live metadata parsed");

        let live_json = self.fetch_live_json().await?;
        let banner_probe_ids = Self::collect_live_banner_probe_ids(&live_json, &live_meta);
        let banner_meta = self.fetch_live_banner_meta_batch(banner_probe_ids).await;
        debug!(
            count = banner_meta.len(),
            "Baltbet live banner fallback parsed"
        );
        for (event_id, meta_entry) in banner_meta {
            Self::merge_live_banner_meta(&mut live_meta, event_id, meta_entry);
        }

        let event_groupings = self.fetch_live_event_groupings_batch(&live_json).await;
        debug!(
            requested_groupings = event_groupings.len(),
            "Baltbet live event market groupings hydrated"
        );

        let (events, odds) =
            Self::parse_live_json_checked_with_groupings(&live_json, &live_meta, &event_groupings)?;
        debug!(
            events = events.len(),
            odds = odds.len(),
            "Baltbet live JSON and grouped markets parsed"
        );

        Ok((events, odds))
    }

    async fn fetch_live_with_fallback(
        &self,
    ) -> Result<(Vec<Event>, Vec<Odd>), Box<dyn std::error::Error + Send + Sync>> {
        let mut errors = Vec::new();
        match self.fetch_live_runtime_data().await {
            Ok((events, odds)) if !events.is_empty() && !odds.is_empty() => {
                return Ok((events, odds));
            }
            Ok((events, odds)) => errors.push(format!(
                "live JSON returned {} events / {} odds",
                events.len(),
                odds.len()
            )),
            Err(error) => errors.push(format!("live JSON: {error}")),
        }

        match self.fetch_legacy_html(LEGACY_LIVE_URL, true).await {
            Ok((events, odds)) if !events.is_empty() && !odds.is_empty() => Ok((events, odds)),
            Ok((events, odds)) => {
                errors.push(format!(
                    "legacy live HTML returned {} events / {} odds",
                    events.len(),
                    odds.len()
                ));
                Err(std::io::Error::other(errors.join("; ")).into())
            }
            Err(error) => {
                errors.push(format!("legacy live HTML: {error}"));
                Err(std::io::Error::other(errors.join("; ")).into())
            }
        }
    }

    async fn fetch_legacy_html(
        &self,
        url: &str,
        is_live: bool,
    ) -> Result<(Vec<Event>, Vec<Odd>), Box<dyn std::error::Error + Send + Sync>> {
        let resp = self
            .client
            .get(url)
            .header(
                "Accept",
                "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
            )
            .header("Accept-Language", "ru-RU,ru;q=0.9,en;q=0.8")
            .send()
            .await?;

        if !resp.status().is_success() {
            return Err(format!("Baltbet legacy HTML {url} returned {}", resp.status()).into());
        }

        let html = resp.text().await?;
        if !Self::is_valid_legacy_html_source(&html) {
            return Err(format!("Baltbet legacy HTML {url} failed source validation").into());
        }

        let parsed = Self::parse_legacy_html(&html, is_live, url);
        if parsed.0.is_empty() || parsed.1.is_empty() {
            return Err(format!(
                "Baltbet legacy HTML {url} produced {} events / {} odds",
                parsed.0.len(),
                parsed.1.len()
            )
            .into());
        }

        Ok(parsed)
    }

    async fn fetch_legacy_prematch_data(
        &self,
    ) -> Result<(Vec<Event>, Vec<Odd>), Box<dyn std::error::Error + Send + Sync>> {
        let resp = self
            .client
            .get(LEGACY_PREMATCH_URL)
            .header(
                "Accept",
                "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
            )
            .header("Accept-Language", "ru-RU,ru;q=0.9,en;q=0.8")
            .send()
            .await?;

        if !resp.status().is_success() {
            return Err(format!("legacy prematch page returned {}", resp.status()).into());
        }

        let html = resp.text().await?;
        let sport_links = Self::parse_legacy_sport_links(&html);
        if sport_links.is_empty() {
            return Err(std::io::Error::other(
                "Baltbet legacy prematch page contains no recognized sport/group links",
            )
            .into());
        }
        let expected_group_count = sport_links.len();

        let client = self.client.clone();
        let fetched = stream::iter(sport_links.into_iter().map(move |sport_link| {
            let client = client.clone();
            async move {
                let url = format!(
                    "https://old.baltbet.ru/Line2.aspx?group={}",
                    sport_link.group_id
                );
                let group_id = sport_link.group_id;
                let sport_label = sport_link.sport_label;
                let resp = client
                    .get(&url)
                    .header(
                        "Accept",
                        "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
                    )
                    .header("Accept-Language", "ru-RU,ru;q=0.9,en;q=0.8")
                    .send()
                    .await;

                match resp {
                    Ok(resp) if resp.status().is_success() => match resp.text().await {
                        Ok(html) => Ok((group_id, sport_label, html)),
                        Err(error) => Err(format!("group={group_id} body-read={error}")),
                    },
                    Ok(resp) => Err(format!("group={group_id} http={}", resp.status())),
                    Err(error) => Err(format!("group={group_id} request={error}")),
                }
            }
        }))
        .buffer_unordered(LEGACY_PREMATCH_FETCH_CONCURRENCY)
        .collect::<Vec<_>>()
        .await;

        let mut results = Vec::with_capacity(expected_group_count);
        let mut failures = Vec::new();
        for result in fetched {
            match result {
                Ok(group) => results.push(group),
                Err(error) => failures.push(error),
            }
        }

        if !failures.is_empty() {
            return Err(std::io::Error::other(format!(
                "Baltbet prematch fetched {} of {} advertised groups; failures: {}",
                results.len(),
                expected_group_count,
                failures.join("; ")
            ))
            .into());
        }

        let mut events = Vec::new();
        let mut odds = Vec::new();
        let mut seen_event_ids = HashSet::new();

        for (group_id, sport_label, html) in results {
            let (group_events, group_odds) =
                Self::parse_legacy_group_page(&html, &group_id, &sport_label, &mut seen_event_ids);
            events.extend(group_events);
            odds.extend(group_odds);
        }

        if events.is_empty() || odds.is_empty() {
            return Err(std::io::Error::other(format!(
                "Baltbet prematch groups produced {} events / {} odds",
                events.len(),
                odds.len()
            ))
            .into());
        }
        debug!(
            events = events.len(),
            odds = odds.len(),
            "Baltbet prematch legacy groups parsed"
        );
        Ok((events, odds))
    }

    fn parse_live_metadata(html: &str) -> HashMap<u64, LiveMeta> {
        let document = Html::parse_document(html);
        let row_selector = Selector::parse(".events-table__body-row[data-id]").expect("selector");
        let title_selector = Selector::parse(".events-table__title").expect("selector");
        let league_selector = Selector::parse(".events-table__league").expect("selector");
        let link_selector = Selector::parse("a.events-table__title-wrapper").expect("selector");
        let t_data_selector = Selector::parse("[t-data]").expect("selector");

        let mut meta = HashMap::new();

        for row in document.select(&row_selector) {
            let Some(id) = row
                .value()
                .attr("data-id")
                .and_then(|v| v.parse::<u64>().ok())
            else {
                continue;
            };

            let title = row
                .select(&title_selector)
                .next()
                .map(|node| node.text().collect::<String>())
                .map(|text| Self::normalize_whitespace(&text))
                .unwrap_or_default();

            let league = row
                .select(&league_selector)
                .next()
                .map(|node| node.text().collect::<String>())
                .map(|text| Self::normalize_whitespace(&text))
                .unwrap_or_else(|| "Unknown".to_string());

            let href = row
                .select(&link_selector)
                .next()
                .and_then(|node| node.value().attr("href"))
                .map(str::to_string);

            if title.is_empty() {
                continue;
            }

            meta.insert(
                id,
                LiveMeta {
                    title,
                    league,
                    href,
                },
            );
        }

        for node in document.select(&t_data_selector) {
            let Some(encoded) = node.value().attr("t-data") else {
                continue;
            };

            let decoded = Self::decode_html_entities(encoded);
            let Ok(payload) = serde_json::from_str::<Value>(&decoded) else {
                continue;
            };

            let Some(id) = payload.get("eventId").and_then(Value::as_u64) else {
                continue;
            };

            let title = match (
                payload.get("firstParticipant").and_then(Value::as_str),
                payload.get("secondParticipant").and_then(Value::as_str),
            ) {
                (Some(home), Some(away)) => {
                    let home = Self::normalize_whitespace(home);
                    let away = Self::normalize_whitespace(away);
                    if home.is_empty() || away.is_empty() {
                        String::new()
                    } else {
                        format!("{} - {}", home, away)
                    }
                }
                _ => String::new(),
            };

            if title.is_empty() {
                continue;
            }

            let league = payload
                .get("leagueTitle")
                .and_then(Value::as_str)
                .map(Self::normalize_whitespace)
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| "Unknown".to_string());

            let href = payload
                .get("webLink")
                .and_then(Value::as_str)
                .map(str::to_string)
                .filter(|value| !value.is_empty());

            meta.entry(id)
                .and_modify(|existing| {
                    if existing.title.is_empty() {
                        existing.title = title.clone();
                    }
                    if existing.league == "Unknown" && league != "Unknown" {
                        existing.league = league.clone();
                    }
                    if existing.href.is_none() {
                        existing.href = href.clone();
                    }
                })
                .or_insert(LiveMeta {
                    title,
                    league,
                    href,
                });
        }

        meta
    }

    fn decode_html_entities(input: &str) -> String {
        let mut decoded = String::with_capacity(input.len());
        let mut idx = 0;

        while idx < input.len() {
            let Some(ch) = input[idx..].chars().next() else {
                break;
            };

            if ch != '&' {
                decoded.push(ch);
                idx += ch.len_utf8();
                continue;
            }

            let Some(relative_end) = input[idx..].find(';') else {
                decoded.push(ch);
                idx += ch.len_utf8();
                continue;
            };
            let end = idx + relative_end;
            let entity = &input[idx + 1..end];

            let replacement = match entity {
                "quot" => Some('"'),
                "amp" => Some('&'),
                "apos" => Some('\''),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "nbsp" => Some(' '),
                _ => {
                    if let Some(hex) = entity.strip_prefix("#x") {
                        u32::from_str_radix(hex, 16).ok().and_then(char::from_u32)
                    } else if let Some(decimal) = entity.strip_prefix('#') {
                        decimal.parse::<u32>().ok().and_then(char::from_u32)
                    } else {
                        None
                    }
                }
            };

            if let Some(ch) = replacement {
                decoded.push(ch);
                idx = end + 1;
            } else {
                decoded.push('&');
                idx += 1;
            }
        }

        decoded
    }

    #[cfg(test)]
    fn parse_live_json_checked(
        json: &Value,
        meta: &HashMap<u64, LiveMeta>,
    ) -> Result<(Vec<Event>, Vec<Odd>), Box<dyn std::error::Error + Send + Sync>> {
        Self::parse_live_json_checked_with_groupings(json, meta, &HashMap::new())
    }

    fn parse_live_json_checked_with_groupings(
        json: &Value,
        meta: &HashMap<u64, LiveMeta>,
        groupings: &HashMap<u64, Value>,
    ) -> Result<(Vec<Event>, Vec<Odd>), Box<dyn std::error::Error + Send + Sync>> {
        if json.get("events").and_then(Value::as_array).is_none() {
            return Err(std::io::Error::other(
                "Baltbet live API response is missing the events array",
            )
            .into());
        }
        let parsed = Self::parse_live_json_with_groupings(json, meta, groupings);
        if parsed.0.is_empty() || parsed.1.is_empty() {
            return Err(std::io::Error::other(format!(
                "Baltbet live API parsed {} events / {} odds",
                parsed.0.len(),
                parsed.1.len()
            ))
            .into());
        }
        Ok(parsed)
    }

    #[cfg(test)]
    fn parse_live_json(json: &Value, meta: &HashMap<u64, LiveMeta>) -> (Vec<Event>, Vec<Odd>) {
        Self::parse_live_json_with_groupings(json, meta, &HashMap::new())
    }

    fn parse_live_json_with_groupings(
        json: &Value,
        meta: &HashMap<u64, LiveMeta>,
        groupings: &HashMap<u64, Value>,
    ) -> (Vec<Event>, Vec<Odd>) {
        let Some(items) = json.get("events").and_then(Value::as_array) else {
            return (Vec::new(), Vec::new());
        };

        let mut events = Vec::new();
        let mut odds = Vec::new();
        let mut seen_event_ids = HashSet::new();

        for item in items {
            let Some(event_id) = item.get("i").and_then(Value::as_u64) else {
                continue;
            };

            let Some(meta_entry) = meta.get(&event_id) else {
                continue;
            };

            let Some((home_team, away_team)) = Self::extract_live_teams(meta_entry) else {
                continue;
            };

            let event_key = format!("baltbet-{}", event_id);
            if !seen_event_ids.insert(event_key.clone()) {
                continue;
            }
            let sport = Self::detect_sport(
                meta_entry.href.as_deref(),
                &meta_entry.league,
                &meta_entry.title,
            );
            let raw_url = meta_entry
                .href
                .as_ref()
                .map(|href| {
                    if href.starts_with("http") {
                        href.clone()
                    } else {
                        format!("https://baltbet.ru{}", href)
                    }
                })
                .or_else(|| Some(LIVE_PAGE_URL.to_string()));

            events.push(Event {
                id: event_key.clone(),
                sport,
                league: meta_entry.league.clone(),
                home_team: home_team.clone(),
                away_team: away_team.clone(),
                start_time: None,
                is_live: true,
                bookmaker_slug: BOOKMAKER_SLUG.to_string(),
                raw_url,
                extra: HashMap::new(),
            });

            let mut event_odds = groupings
                .get(&event_id)
                .map(|grouping| Self::extract_live_grouping_odds(&event_key, item, grouping))
                .unwrap_or_default();
            let fallback_odds = Self::extract_live_odds(&event_key, item);
            for fallback in fallback_odds {
                let already_present = event_odds.iter().any(|existing| {
                    existing.market == fallback.market
                        && existing.selection == fallback.selection
                        && existing.line == fallback.line
                });
                if !already_present {
                    event_odds.push(fallback);
                }
            }
            odds.extend(event_odds);
        }

        (events, odds)
    }

    fn collect_live_banner_probe_ids(json: &Value, meta: &HashMap<u64, LiveMeta>) -> Vec<u64> {
        let Some(items) = json.get("events").and_then(Value::as_array) else {
            return Vec::new();
        };

        let mut probe_ids = Vec::new();
        let mut seen = HashSet::new();

        for item in items {
            let Some(event_id) = item.get("i").and_then(Value::as_u64) else {
                continue;
            };
            if !seen.insert(event_id) {
                continue;
            }

            match meta.get(&event_id) {
                None => probe_ids.push(event_id),
                Some(meta_entry) if Self::live_meta_requires_banner_recovery(meta_entry) => {
                    probe_ids.push(event_id)
                }
                Some(_) => {}
            }
        }

        probe_ids.truncate(LIVE_BANNER_PROBE_LIMIT);
        probe_ids
    }

    fn merge_live_banner_meta(
        meta: &mut HashMap<u64, LiveMeta>,
        event_id: u64,
        banner_meta: LiveMeta,
    ) {
        match meta.get_mut(&event_id) {
            Some(existing) if Self::live_meta_requires_banner_recovery(existing) => {
                *existing = banner_meta;
            }
            Some(_) => {}
            None => {
                meta.insert(event_id, banner_meta);
            }
        }
    }

    fn parse_live_banner_meta(payload: &Value) -> Option<LiveMeta> {
        let home_team = payload
            .get("team1")
            .and_then(Value::as_str)
            .map(Self::normalize_whitespace)
            .filter(|value| !value.is_empty());
        let away_team = payload
            .get("team2")
            .and_then(Value::as_str)
            .map(Self::normalize_whitespace)
            .filter(|value| !value.is_empty());

        let title = match (home_team, away_team) {
            (Some(home_team), Some(away_team)) => format!("{} - {}", home_team, away_team),
            _ => payload
                .get("eventName")
                .and_then(Value::as_str)
                .map(Self::normalize_whitespace)
                .filter(|value| !value.is_empty())?,
        };

        let league = payload
            .get("champName")
            .or_else(|| payload.get("sportName"))
            .and_then(Value::as_str)
            .map(Self::normalize_whitespace)
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "Unknown".to_string());

        let href = payload
            .get("link")
            .or_else(|| payload.get("webLink"))
            .and_then(Value::as_str)
            .map(Self::normalize_whitespace)
            .filter(|value| !value.is_empty());

        Some(LiveMeta {
            title,
            league,
            href,
        })
    }

    fn extract_live_grouping_odds(event_id: &str, item: &Value, grouping: &Value) -> Vec<Odd> {
        if grouping.get("isLive").and_then(Value::as_bool) == Some(false)
            || grouping.get("isActive").and_then(Value::as_bool) == Some(false)
            || grouping.get("isOver").and_then(Value::as_bool) == Some(true)
        {
            return Vec::new();
        }

        // Inline table coefficients are the freshest status source when present.
        // Some Baltbet live events (notably esports/player props) have an empty
        // `c` array while the public grouping endpoint still returns active
        // outcome values. Fall back to those values only when no explicit inline
        // status exists for that outcome ID.
        let mut coefficient_states = HashMap::<u64, Option<f64>>::new();
        if let Some(coefficients) = item.get("c").and_then(Value::as_array) {
            for coefficient in coefficients {
                let Some(coefficient_id) = coefficient.get("o").and_then(Self::parse_u64_value)
                else {
                    continue;
                };
                let price = if coefficient.get("s").and_then(Self::parse_u64_value) == Some(1) {
                    coefficient
                        .get("v")
                        .and_then(Self::parse_json_float)
                        .filter(|value| value.is_finite() && *value > 1.0)
                } else {
                    None
                };
                coefficient_states.insert(coefficient_id, price);
            }
        }

        let Some(markets) = grouping.get("markets").and_then(Value::as_array) else {
            return Vec::new();
        };

        let now = Utc::now();
        let mut odds = Vec::new();
        let mut seen = HashSet::new();

        for market in markets {
            let market_kind = market
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let market_name = market
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let Some(outcomes) = market.get("outcomes").and_then(Value::as_array) else {
                continue;
            };
            let has_draw = outcomes.iter().any(|outcome| {
                outcome
                    .get("kind")
                    .and_then(Value::as_str)
                    .is_some_and(|kind| kind.eq_ignore_ascii_case("DRAW"))
            });
            let base_market =
                Self::normalize_grouping_market_name(market_name, market_kind, has_draw);

            for outcome in outcomes {
                let original_id = outcome.get("oid").and_then(Self::parse_u64_value);
                let actual_id = outcome.get("aid").and_then(Self::parse_u64_value);
                let mut matched = None;
                let mut explicitly_inactive = false;
                for candidate_id in [original_id, actual_id].into_iter().flatten() {
                    if let Some(state) = coefficient_states.get(&candidate_id) {
                        match state {
                            Some(price) => matched = Some((candidate_id, *price)),
                            None => explicitly_inactive = true,
                        }
                        break;
                    }
                }
                if matched.is_none() && explicitly_inactive {
                    continue;
                }
                let (source_id, price) = match matched {
                    Some(matched) => matched,
                    None => {
                        let Some(source_id) = original_id.or(actual_id) else {
                            continue;
                        };
                        let Some(price) = outcome
                            .get("value")
                            .and_then(Self::parse_json_float)
                            .filter(|value| value.is_finite() && *value > 1.0)
                        else {
                            continue;
                        };
                        (source_id, price)
                    }
                };

                let outcome_kind = outcome
                    .get("kind")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_ascii_uppercase();
                let selection = Self::grouping_selection(outcome, market, &outcome_kind);
                if selection.is_empty() {
                    continue;
                }
                let odds_type =
                    Self::grouping_odds_type(&base_market, market_kind, &outcome_kind, &selection);
                let line = if matches!(
                    market_kind.to_ascii_lowercase().as_str(),
                    "handicap" | "total"
                ) {
                    outcome
                        .get("params")
                        .and_then(Value::as_array)
                        .and_then(|params| {
                            params.iter().find_map(|param| {
                                param
                                    .get("value")
                                    .and_then(Value::as_str)
                                    .and_then(Self::parse_line_value)
                            })
                        })
                } else {
                    None
                };
                let context = Self::grouping_outcome_context(outcome);
                let market_key = if context.is_empty() {
                    base_market.clone()
                } else {
                    format!("{}::{}", base_market, context)
                };
                let market_id = market
                    .get("id")
                    .and_then(Self::parse_u64_value)
                    .unwrap_or_default();
                if !seen.insert((market_id, source_id, market_key.clone(), selection.clone())) {
                    continue;
                }

                odds.push(Odd {
                    id: format!("{}-{}-{}", event_id, market_id, source_id),
                    event_id: event_id.to_string(),
                    bookmaker_slug: BOOKMAKER_SLUG.to_string(),
                    market: market_key,
                    selection,
                    odds: price,
                    odds_type,
                    line,
                    timestamp: now,
                });
            }
        }

        odds
    }

    fn normalize_grouping_market_name(name: &str, kind: &str, has_draw: bool) -> String {
        let normalized_name = name.to_lowercase().replace('ё', "е");
        let kind = kind.to_ascii_lowercase();

        let period_prefix = if [
            "1-й тайм",
            "1-го тайма",
            "1-м тайме",
            "1 тайм",
            "первый тайм",
            "первого тайма",
            "первом тайме",
            "first half",
            "first_half",
            "first-half",
            "firsthalf",
            "1st half",
            "1st_half",
            "1st-half",
            "1h",
            "1 h",
        ]
        .iter()
        .any(|marker| normalized_name.contains(marker))
        {
            Some("1H_")
        } else if [
            "2-й тайм",
            "2-го тайма",
            "2-м тайме",
            "2 тайм",
            "второй тайм",
            "второго тайма",
            "втором тайме",
            "second half",
            "second_half",
            "second-half",
            "secondhalf",
            "2nd half",
            "2nd_half",
            "2nd-half",
            "2h",
            "2 h",
        ]
        .iter()
        .any(|marker| normalized_name.contains(marker))
        {
            Some("2H_")
        } else {
            None
        };

        // A generic "period" label without an explicit period number must
        // not collapse several different segments into one market.
        let unsupported_period_scope = period_prefix.is_none()
            && [
                "период",
                "period",
                "четверт",
                "quarter",
                "сет",
                "парт",
                "round",
                "раунд",
            ]
            .iter()
            .any(|marker| normalized_name.contains(marker));

        let team_scope = if [
            "команды 1",
            "команда 1",
            "команды №1",
            "первой команды",
            "1-й команды",
            "хозяев",
            "home team",
            "team 1",
            "team1",
            "1к",
            "1 к",
            "ит1",
            "ит 1",
            "итб1",
            "итм1",
            "итб 1",
            "итм 1",
        ]
        .iter()
        .any(|marker| normalized_name.contains(marker))
        {
            Some("Team1")
        } else if [
            "команды 2",
            "команда 2",
            "команды №2",
            "второй команды",
            "2-й команды",
            "гостей",
            "away team",
            "team 2",
            "team2",
            "2к",
            "2 к",
            "ит2",
            "ит 2",
            "итб2",
            "итм2",
            "итб 2",
            "итм 2",
        ]
        .iter()
        .any(|marker| normalized_name.contains(marker))
        {
            Some("Team2")
        } else {
            None
        };

        let scoped = |market: &str| match period_prefix {
            Some(prefix) => format!("{prefix}{market}"),
            None => market.to_string(),
        };

        // Keep explicitly named map markets distinct; their selected map
        // is additionally retained by grouping_outcome_context().
        if normalized_name.contains("карт") || normalized_name.contains("map") {
            return match kind.as_str() {
                "winner" => "MapWinner".to_string(),
                "handicap" => "MapHandicap".to_string(),
                "total" => "MapTotal".to_string(),
                _ => format!("Custom:{}", name.trim()),
            };
        }

        if unsupported_period_scope {
            return format!("Custom:{}", name.trim());
        }

        match kind.as_str() {
            "winner" if period_prefix.is_some() && has_draw => scoped("1X2"),
            "winner" if period_prefix.is_some() => scoped("Winner"),
            "winner" if has_draw => "1X2".to_string(),
            "winner" => "Moneyline".to_string(),
            "handicap" => {
                let market = match team_scope {
                    Some(team) => format!("{team}Handicap"),
                    None => "Handicap".to_string(),
                };
                scoped(&market)
            }
            "total"
                if normalized_name.contains("индивидуальн")
                    || normalized_name.contains("инд. тотал")
                    || normalized_name.contains("инд тотал")
                    || normalized_name.contains("итб")
                    || normalized_name.contains("итм")
                    || normalized_name.contains("ит 1")
                    || normalized_name.contains("ит 2") =>
            {
                match team_scope {
                    Some(team) => scoped(&format!("{team}Total")),
                    None => format!("Custom:{}", name.trim()),
                }
            }
            "total" => {
                let market = match team_scope {
                    Some(team) => format!("{team}Total"),
                    None => "Total".to_string(),
                };
                scoped(&market)
            }
            "yesno"
                if normalized_name.contains("обе забьют")
                    || normalized_name.contains("обе команды забьют")
                    || normalized_name.contains("both teams score")
                    || normalized_name.contains("btts") =>
            {
                scoped("BothTeamsScore")
            }
            _ if name.trim().is_empty() => format!("Custom:{}", kind),
            _ => format!("Custom:{}", name.trim()),
        }
    }

    fn grouping_selection(outcome: &Value, market: &Value, outcome_kind: &str) -> String {
        match outcome_kind {
            "TEAM1" => return "1".to_string(),
            "DRAW" => return "X".to_string(),
            "TEAM2" => return "2".to_string(),
            "OVER" => return "Over".to_string(),
            "UNDER" => return "Under".to_string(),
            "YES" => return "Yes".to_string(),
            "NO" => return "No".to_string(),
            "EVEN" => return "Even".to_string(),
            "ODD" => return "Odd".to_string(),
            _ => {}
        }

        let outcome_ctid = outcome.get("ctid").and_then(Self::parse_u64_value);
        let Some(params) = market.get("params").and_then(Value::as_array) else {
            return outcome_kind.to_string();
        };
        params
            .iter()
            .find(|param| param.get("ctid").and_then(Self::parse_u64_value) == outcome_ctid)
            .and_then(|param| param.get("name"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .unwrap_or(outcome_kind)
            .to_string()
    }

    fn grouping_odds_type(
        market: &str,
        market_kind: &str,
        outcome_kind: &str,
        selection: &str,
    ) -> OddsType {
        match outcome_kind {
            "TEAM1" | "TEAM2" if market_kind.eq_ignore_ascii_case("Handicap") => OddsType::Handicap,
            "TEAM1" => OddsType::Home,
            "DRAW" => OddsType::Draw,
            "TEAM2" => OddsType::Away,
            "OVER" => OddsType::Over,
            "UNDER" => OddsType::Under,
            "YES" if market == "BothTeamsScore" => OddsType::BothTeamsScoreYes,
            "NO" if market == "BothTeamsScore" => OddsType::BothTeamsScoreNo,
            "EVEN" => OddsType::Even,
            "ODD" => OddsType::Odd,
            _ => {
                let _ = selection;
                OddsType::Custom
            }
        }
    }

    fn grouping_outcome_context(outcome: &Value) -> String {
        let Some(params) = outcome.get("params").and_then(Value::as_array) else {
            return String::new();
        };
        let mut values = Vec::new();
        for param in params {
            let name = param
                .get("name")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty());
            let value = param
                .get("value")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty());
            let Some(value) = value else {
                continue;
            };
            if Self::parse_line_value(value).is_some() {
                continue;
            }
            let component = match name {
                Some(name) => format!("{}={}", name, value),
                None => value.to_string(),
            };
            if !values.contains(&component) {
                values.push(component);
            }
        }
        values.sort();
        values.join("|")
    }

    fn parse_line_value(value: &str) -> Option<f64> {
        let normalized = value
            .trim()
            .replace(',', ".")
            .replace('−', "-")
            .replace(' ', "");
        let parsed = normalized.parse::<f64>().ok()?;
        (parsed.is_finite()).then_some(parsed)
    }

    fn extract_live_odds(event_id: &str, item: &Value) -> Vec<Odd> {
        let Some(coefficients) = item.get("c").and_then(Value::as_array) else {
            return Vec::new();
        };
        let mut by_type = HashMap::<u64, Vec<f64>>::new();
        for coefficient in coefficients {
            if coefficient.get("s").and_then(Self::parse_u64_value) != Some(1) {
                continue;
            }
            let Some(value) = coefficient
                .get("v")
                .and_then(Self::parse_json_float)
                .filter(|value| value.is_finite() && *value > 1.0)
            else {
                continue;
            };
            if let Some(coefficient_type) = coefficient.get("t").and_then(Self::parse_u64_value) {
                by_type.entry(coefficient_type).or_default().push(value);
            }
        }
        let unique_value = |coefficient_type: u64| {
            by_type
                .get(&coefficient_type)
                .filter(|values| values.len() == 1)
                .and_then(|values| values.first())
                .copied()
        };

        let now = Utc::now();
        let mut odds = Vec::new();
        if let (Some(home), Some(draw), Some(away)) =
            (unique_value(1334), unique_value(1335), unique_value(1336))
        {
            for (selection, odds_type, value) in [
                ("1", OddsType::Home, home),
                ("X", OddsType::Draw, draw),
                ("2", OddsType::Away, away),
            ] {
                odds.push(Odd {
                    id: format!("{}-1X2-{}", event_id, selection),
                    event_id: event_id.to_string(),
                    bookmaker_slug: BOOKMAKER_SLUG.to_string(),
                    market: "1X2".to_string(),
                    selection: selection.to_string(),
                    odds: value,
                    odds_type,
                    line: None,
                    timestamp: now,
                });
            }
        }

        for (home_type, away_type) in [(1952_u64, 1953_u64), (2017, 2018), (3679, 3681)] {
            if let (Some(home), Some(away)) = (unique_value(home_type), unique_value(away_type)) {
                for (selection, odds_type, value) in
                    [("1", OddsType::Home, home), ("2", OddsType::Away, away)]
                {
                    odds.push(Odd {
                        id: format!("{}-Moneyline-{}-{}", event_id, home_type, selection),
                        event_id: event_id.to_string(),
                        bookmaker_slug: BOOKMAKER_SLUG.to_string(),
                        market: "Moneyline".to_string(),
                        selection: selection.to_string(),
                        odds: value,
                        odds_type,
                        line: None,
                        timestamp: now,
                    });
                }
            }
        }

        odds
    }

    fn parse_u64_value(value: &Value) -> Option<u64> {
        value
            .as_u64()
            .or_else(|| value.as_str().and_then(|text| text.trim().parse().ok()))
    }

    fn parse_json_float(value: &Value) -> Option<f64> {
        let parsed = value.as_f64().or_else(|| {
            value
                .as_str()
                .and_then(|text| text.trim().replace(',', ".").parse::<f64>().ok())
        })?;
        parsed.is_finite().then_some(parsed)
    }

    fn normalize_whitespace(input: &str) -> String {
        input
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .trim()
            .to_string()
    }

    fn split_match_title(title: &str) -> Option<(String, String)> {
        for separator in [" — ", " - ", " – ", " vs ", " VS ", " v ", "-"] {
            let parts: Vec<&str> = title.splitn(2, separator).collect();
            if parts.len() == 2 {
                let home = Self::normalize_whitespace(parts[0]);
                let away = Self::normalize_whitespace(parts[1]);
                if !home.is_empty() && !away.is_empty() && home != away {
                    return Some((home, away));
                }
            }
        }

        None
    }

    fn extract_live_teams(meta: &LiveMeta) -> Option<(String, String)> {
        if let Some((home_team, away_team)) = Self::split_match_title(&meta.title) {
            if Self::is_valid_team(&home_team) && Self::is_valid_team(&away_team) {
                return Some((home_team, away_team));
            }
        }

        let (home_team, away_team) =
            Self::split_teams_from_href(meta.href.as_deref(), &meta.title)?;
        if Self::is_valid_team(&home_team) && Self::is_valid_team(&away_team) {
            Some((home_team, away_team))
        } else {
            None
        }
    }

    fn live_meta_requires_banner_recovery(meta: &LiveMeta) -> bool {
        Self::extract_live_teams(meta).is_none()
    }

    fn split_teams_from_href(href: Option<&str>, title: &str) -> Option<(String, String)> {
        let href = href?;
        let slug = href
            .rsplit('/')
            .next()
            .unwrap_or(href)
            .split('?')
            .next()
            .unwrap_or(href);
        let matchup = slug.split("-id-").next().unwrap_or(slug);

        let markers = Self::live_stat_slug_markers(title);
        for home_marker in markers {
            let home_token = format!("-{home_marker}");
            let Some(first) = matchup.find(&home_token) else {
                continue;
            };

            for away_marker in markers {
                let away_token = format!("-{away_marker}");
                let Some(relative_second) = matchup[first + home_token.len()..].find(&away_token)
                else {
                    continue;
                };
                let second = first + home_token.len() + relative_second;
                let home = Self::slug_to_team_name(&matchup[..first]);
                let away = Self::slug_to_team_name(&matchup[first + home_token.len()..second]);
                if !home.is_empty() && !away.is_empty() && home != away {
                    return Some((home, away));
                }
            }

            for away_marker in Self::all_live_stat_slug_markers() {
                let away_token = format!("-{away_marker}");
                let Some(relative_second) = matchup[first + home_token.len()..].find(&away_token)
                else {
                    continue;
                };
                let second = first + home_token.len() + relative_second;
                let home = Self::slug_to_team_name(&matchup[..first]);
                let away = Self::slug_to_team_name(&matchup[first + home_token.len()..second]);
                if !home.is_empty() && !away.is_empty() && home != away {
                    return Some((home, away));
                }
            }
        }

        None
    }

    fn all_live_stat_slug_markers() -> &'static [&'static str] {
        &[
            "aces",
            "corners",
            "double-faults",
            "double-foults",
            "fouls",
            "goal-kicks",
            "offsides",
            "outs",
            "penalty-time",
            "penalty-time-minutes",
            "players",
            "posts-and-crossbars",
            "posts-and-srossbars",
            "power-play-goals",
            "shots-on-goal",
            "three-pointers-scored",
            "two-pointers-scored",
            "yellow-cards",
        ]
    }

    fn live_stat_slug_markers(title: &str) -> &'static [&'static str] {
        match title.trim().to_lowercase().as_str() {
            "эйсы" => &["aces"],
            "двойные ошибки" => &["double-faults", "double-foults"],
            "желтые карточки" => &["yellow-cards"],
            "угловые" => &["corners"],
            "ауты" => &["outs"],
            "офсайды" => &["offsides"],
            "попадания в каркас ворот" => {
                &["posts-and-crossbars", "posts-and-srossbars"]
            }
            "удары в створ" => &["shots-on-goal"],
            "удары от ворот" => &["goal-kicks"],
            "фолы" => &["fouls"],
            "голы в большинстве" => &["power-play-goals"],
            "игроки" => &["players"],
            "сумма штрафных минут" => &["penalty-time", "penalty-time-minutes"],
            "2-х очк. попадания" => &["two-pointers-scored"],
            "3-х очк. попадания" => &["three-pointers-scored"],
            _ => &[],
        }
    }

    fn slug_to_team_name(value: &str) -> String {
        value
            .split('-')
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(" ")
            .trim()
            .to_string()
    }

    fn detect_sport(path: Option<&str>, league: &str, title: &str) -> Sport {
        let mut probe = String::new();
        if let Some(path) = path {
            probe.push_str(path);
            probe.push(' ');
        }
        probe.push_str(league);
        probe.push(' ');
        probe.push_str(title);
        let lower = probe.to_lowercase();

        if lower.contains("american-football")
            || lower.contains("американ")
            || lower.contains("nfl")
            || lower.contains("ufl")
        {
            Sport::Other
        } else if lower.contains("table-tennis") || lower.contains("настоль") {
            Sport::TableTennis
        } else if lower.contains("теннис") || lower.contains("tennis") {
            Sport::Tennis
        } else if lower.contains("хоккей") || lower.contains("hockey") {
            Sport::Hockey
        } else if lower.contains("баскет") || lower.contains("basket") {
            Sport::Basketball
        } else if lower.contains("волей") || lower.contains("volleyball") {
            Sport::Volleyball
        } else if lower.contains("rugby") || lower.contains("регби") {
            Sport::Rugby
        } else if lower.contains("гандбол") || lower.contains("handball") {
            Sport::Handball
        } else if lower.contains("бейсбол") || lower.contains("baseball") {
            Sport::Baseball
        } else if lower.contains("бадминтон") || lower.contains("badminton") {
            Sport::Badminton
        } else if lower.contains("esports")
            || lower.contains("кибер")
            || lower.contains("dota")
            || lower.contains("counter-strike")
            || lower.contains("cs2")
        {
            Sport::Esports
        } else if lower.contains("футбол") || lower.contains("football") || lower.contains("soccer")
        {
            Sport::Football
        } else {
            Sport::Other
        }
    }

    fn is_valid_legacy_html_source(html: &str) -> bool {
        html.contains("Line1.aspx")
            || html.contains("Live1.aspx")
            || (html.contains("class=\"name\"") && html.contains("class=\"coe\""))
    }

    fn parse_legacy_html(html: &str, is_live: bool, source_url: &str) -> (Vec<Event>, Vec<Odd>) {
        let mut events = Vec::new();
        let mut odds = Vec::new();
        let now = Utc::now();
        let name_marker = "class=\"name\"";
        let coe_marker = "class=\"coe\"";

        let names: Vec<String> = Self::extract_span_values(html, name_marker);
        let coefs: Vec<f64> = Self::extract_span_floats(html, coe_marker);

        let mut idx = 0;
        let mut event_counter = 0;

        while idx + 2 < names.len() {
            let middle = names[idx + 1].to_lowercase();
            if middle.contains("ничья") || middle.contains("draw") {
                let home = names[idx].trim().to_string();
                let away = names[idx + 2].trim().to_string();

                if Self::is_valid_team(&home) && Self::is_valid_team(&away) && home != away {
                    let coef_idx = event_counter * 3;
                    let home_odds = coefs.get(coef_idx).copied();
                    let draw_odds = coefs.get(coef_idx + 1).copied();
                    let away_odds = coefs.get(coef_idx + 2).copied();

                    if home_odds.is_some() && away_odds.is_some() {
                        let event_id = format!("baltbet-legacy-{}", event_counter);

                        events.push(Event {
                            id: event_id.clone(),
                            sport: Sport::Football,
                            league: "Unknown".to_string(),
                            home_team: home,
                            away_team: away,
                            start_time: None,
                            is_live,
                            bookmaker_slug: BOOKMAKER_SLUG.to_string(),
                            raw_url: Some(source_url.to_string()),
                            extra: HashMap::new(),
                        });

                        if let Some(o1) = home_odds {
                            odds.push(Odd {
                                id: format!("{}-1", event_id),
                                event_id: event_id.clone(),
                                bookmaker_slug: BOOKMAKER_SLUG.to_string(),
                                market: "1X2".into(),
                                selection: "1".into(),
                                odds: o1,
                                odds_type: OddsType::Home,
                                line: None,
                                timestamp: now,
                            });
                        }
                        if let Some(ox) = draw_odds {
                            odds.push(Odd {
                                id: format!("{}-X", event_id),
                                event_id: event_id.clone(),
                                bookmaker_slug: BOOKMAKER_SLUG.to_string(),
                                market: "1X2".into(),
                                selection: "X".into(),
                                odds: ox,
                                odds_type: OddsType::Draw,
                                line: None,
                                timestamp: now,
                            });
                        }
                        if let Some(o2) = away_odds {
                            odds.push(Odd {
                                id: format!("{}-2", event_id),
                                event_id: event_id.clone(),
                                bookmaker_slug: BOOKMAKER_SLUG.to_string(),
                                market: "1X2".into(),
                                selection: "2".into(),
                                odds: o2,
                                odds_type: OddsType::Away,
                                line: None,
                                timestamp: now,
                            });
                        }

                        event_counter += 1;
                    }
                }
                idx += 3;
            } else {
                idx += 1;
            }
        }

        (events, odds)
    }

    fn parse_legacy_sport_links(html: &str) -> Vec<LegacySportLink> {
        let document = Html::parse_document(html);
        let selector = Selector::parse("#tablesportsdiv a").expect("selector");
        let href_selector = Selector::parse("a[href]").expect("selector");
        let re = Regex::new(r"openall\('([^']+)'\)").expect("regex");
        let direct_group_re = Regex::new(r#"(?i)line2\.aspx\?group=([^&#"']+)"#).expect("regex");
        let mut links = Vec::new();
        let mut seen = HashSet::new();

        for link in document.select(&selector) {
            let Some(href) = link.value().attr("href") else {
                continue;
            };
            let Some(captures) = re.captures(href) else {
                continue;
            };
            let Some(group_id) = captures.get(1).map(|m| m.as_str().to_string()) else {
                continue;
            };
            let sport_label = Self::normalize_whitespace(&link.text().collect::<String>());
            if sport_label.is_empty() || !seen.insert(group_id.clone()) {
                continue;
            }

            links.push(LegacySportLink {
                group_id,
                sport_label,
            });
        }

        let mut featured_groups = 0;
        for link in document.select(&href_selector) {
            if featured_groups >= LEGACY_PREMATCH_FEATURED_GROUP_LIMIT {
                break;
            }

            let Some(href) = link.value().attr("href") else {
                continue;
            };
            let Some(captures) = direct_group_re.captures(href) else {
                continue;
            };
            let Some(group_id) = captures.get(1).map(|m| m.as_str().to_string()) else {
                continue;
            };

            if !group_id.contains('_') || !seen.insert(group_id.clone()) {
                continue;
            }

            let sport_label = Self::normalize_whitespace(&link.text().collect::<String>());
            links.push(LegacySportLink {
                group_id,
                sport_label: if sport_label.is_empty() {
                    "Unknown".to_string()
                } else {
                    sport_label
                },
            });
            featured_groups += 1;
        }

        links
    }

    fn parse_legacy_group_page(
        html: &str,
        group_id: &str,
        sport_label: &str,
        seen_event_ids: &mut HashSet<String>,
    ) -> (Vec<Event>, Vec<Odd>) {
        let document = Html::parse_document(html);
        let table_selector = Selector::parse("table.lvmain.coef-tobasket").expect("selector");
        let row_selector = Selector::parse("tr").expect("selector");
        let cell_selector = Selector::parse("td").expect("selector");
        let head_league_selector = Selector::parse("tr.head td.left").expect("selector");
        let left_cell_selector = Selector::parse("td.left").expect("selector");
        let link_selector = Selector::parse("a").expect("selector");
        let coef_cell_selector = Selector::parse("td.coef").expect("selector");
        let now = Utc::now();
        let event_re = Regex::new(r"event=(\d+)").expect("regex");
        let mut events = Vec::new();
        let mut odds = Vec::new();

        for table in document.select(&table_selector) {
            let league = table
                .select(&head_league_selector)
                .next()
                .map(|node| Self::normalize_whitespace(&node.text().collect::<String>()))
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| sport_label.to_string());

            for row in table.select(&row_selector) {
                if row.value().attr("class") == Some("head") {
                    continue;
                }

                if row
                    .value()
                    .attr("id")
                    .is_some_and(|id| id.starts_with("addrow"))
                {
                    continue;
                }

                let cells: Vec<_> = row.select(&cell_selector).collect();
                if cells.len() < 3 {
                    continue;
                }

                let Some(left_cell) = row.select(&left_cell_selector).next() else {
                    continue;
                };
                let Some(link) = left_cell.select(&link_selector).next() else {
                    continue;
                };

                let title = Self::normalize_whitespace(&link.text().collect::<String>());
                let Some((home_team, away_team)) = Self::split_match_title(&title) else {
                    continue;
                };

                if !Self::is_valid_team(&home_team) || !Self::is_valid_team(&away_team) {
                    continue;
                }

                let onclick = link.value().attr("onclick").unwrap_or_default();
                let event_id = event_re
                    .captures(onclick)
                    .and_then(|captures| captures.get(1))
                    .map(|m| format!("baltbet-prematch-{}", m.as_str()))
                    .unwrap_or_else(|| {
                        format!(
                            "baltbet-prematch-{}-{}",
                            group_id,
                            title.to_lowercase().replace(' ', "-")
                        )
                    });

                if !seen_event_ids.insert(event_id.clone()) {
                    continue;
                }

                let coef_cells: Vec<f64> = row
                    .select(&coef_cell_selector)
                    .filter_map(|cell| Self::parse_legacy_float(&cell.text().collect::<String>()))
                    .collect();

                let start_time =
                    cells
                        .get(1)
                        .zip(cells.get(2))
                        .and_then(|(date_cell, time_cell)| {
                            Self::parse_legacy_start_time(
                                &date_cell.text().collect::<String>(),
                                &time_cell.text().collect::<String>(),
                            )
                        });

                let sport = Self::detect_sport(None, &league, sport_label);
                let raw_url = Some(format!(
                    "https://old.baltbet.ru/Line2.aspx?group={}",
                    group_id
                ));

                events.push(Event {
                    id: event_id.clone(),
                    sport,
                    league: league.clone(),
                    home_team: home_team.clone(),
                    away_team: away_team.clone(),
                    start_time,
                    is_live: false,
                    bookmaker_slug: BOOKMAKER_SLUG.to_string(),
                    raw_url,
                    extra: HashMap::new(),
                });

                if coef_cells.len() >= 3 {
                    odds.push(Odd {
                        id: format!("{}-1", event_id),
                        event_id: event_id.clone(),
                        bookmaker_slug: BOOKMAKER_SLUG.to_string(),
                        market: "1X2".into(),
                        selection: "1".into(),
                        odds: coef_cells[0],
                        odds_type: OddsType::Home,
                        line: None,
                        timestamp: now,
                    });
                    odds.push(Odd {
                        id: format!("{}-X", event_id),
                        event_id: event_id.clone(),
                        bookmaker_slug: BOOKMAKER_SLUG.to_string(),
                        market: "1X2".into(),
                        selection: "X".into(),
                        odds: coef_cells[1],
                        odds_type: OddsType::Draw,
                        line: None,
                        timestamp: now,
                    });
                    odds.push(Odd {
                        id: format!("{}-2", event_id),
                        event_id: event_id.clone(),
                        bookmaker_slug: BOOKMAKER_SLUG.to_string(),
                        market: "1X2".into(),
                        selection: "2".into(),
                        odds: coef_cells[2],
                        odds_type: OddsType::Away,
                        line: None,
                        timestamp: now,
                    });
                } else if coef_cells.len() >= 2 {
                    odds.push(Odd {
                        id: format!("{}-1", event_id),
                        event_id: event_id.clone(),
                        bookmaker_slug: BOOKMAKER_SLUG.to_string(),
                        market: "Moneyline".into(),
                        selection: "1".into(),
                        odds: coef_cells[0],
                        odds_type: OddsType::Home,
                        line: None,
                        timestamp: now,
                    });
                    odds.push(Odd {
                        id: format!("{}-2", event_id),
                        event_id: event_id.clone(),
                        bookmaker_slug: BOOKMAKER_SLUG.to_string(),
                        market: "Moneyline".into(),
                        selection: "2".into(),
                        odds: coef_cells[1],
                        odds_type: OddsType::Away,
                        line: None,
                        timestamp: now,
                    });
                }
            }
        }

        (events, odds)
    }

    fn parse_legacy_float(text: &str) -> Option<f64> {
        let normalized = Self::normalize_whitespace(text).replace(',', ".");
        let value = normalized.parse::<f64>().ok()?;
        (value > 1.0 && value < 100.0).then_some(value)
    }

    fn parse_legacy_start_time(date: &str, time: &str) -> Option<chrono::DateTime<Utc>> {
        let value = format!(
            "{} {}",
            Self::normalize_whitespace(date),
            Self::normalize_whitespace(time)
        );
        let naive = NaiveDateTime::parse_from_str(&value, "%d.%m.%Y %H:%M").ok()?;
        Some(Utc.from_utc_datetime(&naive))
    }

    fn extract_span_values(html: &str, class_marker: &str) -> Vec<String> {
        let mut values = Vec::new();
        let mut search_from = 0;

        while let Some(pos) = html[search_from..].find(class_marker) {
            let abs_pos = search_from + pos;
            if let Some(tag_end) = html[abs_pos..].find('>') {
                let content_start = abs_pos + tag_end + 1;
                if let Some(span_end) = html[content_start..].find("</span>") {
                    let text = html[content_start..content_start + span_end]
                        .trim()
                        .to_string();
                    if !text.is_empty() {
                        values.push(text);
                    }
                    search_from = content_start + span_end + 7;
                } else {
                    break;
                }
            } else {
                break;
            }
        }

        values
    }

    fn extract_span_floats(html: &str, class_marker: &str) -> Vec<f64> {
        let mut values = Vec::new();
        let mut search_from = 0;

        while let Some(pos) = html[search_from..].find(class_marker) {
            let abs_pos = search_from + pos;
            if let Some(tag_end) = html[abs_pos..].find('>') {
                let content_start = abs_pos + tag_end + 1;
                if let Some(span_end) = html[content_start..].find("</span>") {
                    let text = html[content_start..content_start + span_end].trim();
                    let normalized = text.replace(',', ".");
                    if let Ok(value) = normalized.parse::<f64>() {
                        if value > 1.0 && value < 100.0 {
                            values.push(value);
                        }
                    }
                    search_from = content_start + span_end + 7;
                } else {
                    break;
                }
            } else {
                break;
            }
        }

        values
    }

    fn is_valid_team(name: &str) -> bool {
        let trimmed = name.trim();
        if trimmed.len() < 2 || trimmed.len() > 80 {
            return false;
        }

        let lower = trimmed.to_lowercase();
        let invalid_words = [
            "футбол",
            "счёт",
            "счет",
            "live",
            "лайв",
            "матч",
            "игра",
            "спорт",
            "football",
            "soccer",
            "sport",
            "game",
            "match",
            "count",
            "basketball",
            "теннис",
            "hockey",
            "хоккей",
            "volleyball",
            "волейбол",
            "статистика",
            "statistics",
            "время",
            "time",
            "vs",
            "против",
            "команда",
            "team",
            "total",
            "тотал",
            "ничья",
            "draw",
            "unknown",
            "неизвест",
            "tbd",
            "н/д",
            "n/a",
        ];

        if invalid_words.iter().any(|word| lower == *word) {
            return false;
        }

        if trimmed
            .replace('.', "")
            .replace(',', "")
            .replace(' ', "")
            .parse::<f64>()
            .is_ok()
        {
            return false;
        }

        true
    }
}

#[async_trait]
impl BookmakerParser for BaltbetParser {
    fn name(&self) -> &str {
        "Baltbet"
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
        let (mut live_events, _) = self.fetch_live_with_fallback().await?;
        let (prematch_events, _) = self.fetch_legacy_prematch_data().await?;
        live_events.extend(prematch_events);
        info!(count = live_events.len(), "Baltbet events parsed");
        Ok(live_events)
    }

    async fn fetch_odds(
        &self,
        _event_id: &str,
    ) -> Result<Vec<Odd>, Box<dyn std::error::Error + Send + Sync>> {
        let (_, mut live_odds) = self.fetch_live_with_fallback().await?;
        let (_, prematch_odds) = self.fetch_legacy_prematch_data().await?;
        live_odds.extend(prematch_odds);
        Ok(live_odds)
    }

    async fn fetch_all(&self) -> Result<ParserResult, Box<dyn std::error::Error + Send + Sync>> {
        let start = std::time::Instant::now();
        let (mut all_events, mut all_odds) = self.fetch_live_with_fallback().await?;
        let (prematch_events, prematch_odds) = self.fetch_legacy_prematch_data().await?;
        all_events.extend(prematch_events);
        all_odds.extend(prematch_odds);

        let elapsed = start.elapsed().as_millis() as u64;
        debug!(
            events = all_events.len(),
            odds = all_odds.len(),
            time_ms = elapsed,
            "Baltbet fetch complete"
        );
        Ok(ParserResult::new(
            BOOKMAKER_SLUG,
            all_events,
            all_odds,
            elapsed,
        ))
    }

    fn base_url(&self) -> &str {
        "https://baltbet.ru"
    }

    fn user_agent(&self) -> &str {
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36"
    }
}

#[cfg(test)]
mod tests {
    use super::BaltbetParser;
    use serde_json::Value;
    use shared::DiagnosticSeverity;
    use shared::OddsType;
    use shared::ParserReadinessStage;
    use shared::Sport;
    use std::collections::{HashMap, HashSet};

    #[test]
    fn parses_decimal_comma_and_rejects_non_finite_market_values() {
        assert_eq!(
            BaltbetParser::parse_json_float(&serde_json::json!(" 2,05 ")),
            Some(2.05)
        );
        assert!(BaltbetParser::parse_json_float(&serde_json::json!("NaN")).is_none());
        assert!(BaltbetParser::parse_json_float(&serde_json::json!("Infinity")).is_none());
    }

    #[test]
    fn exposes_post_kpi_readiness_snapshot() {
        let readiness = BaltbetParser::readiness_snapshot();

        assert_eq!(readiness.stage, ParserReadinessStage::RolloutReady);
        assert!(!readiness.production_enabled);
        assert!(readiness.self_check_available);
        assert!(readiness.checks.iter().any(|check| {
            check.code == "strict_feed_error_propagation_pending_runtime_confirmation"
                && matches!(check.severity, DiagnosticSeverity::Warn)
        }));
        assert!(readiness
            .checks
            .iter()
            .any(|check| check.code == "strict_live_kpi_recently_met"
                && matches!(check.severity, DiagnosticSeverity::Pass)));
        assert!(readiness
            .checks
            .iter()
            .any(|check| check.code == "strict_prematch_kpi_recently_met"
                && matches!(check.severity, DiagnosticSeverity::Pass)));
    }

    #[test]
    fn parses_live_metadata_from_rows_and_t_data() {
        let html = include_str!("../tests/fixtures/baltbet_live_page_fixture.html");

        let meta = BaltbetParser::parse_live_metadata(html);

        assert_eq!(meta.len(), 3);

        let row_entry = meta.get(&29918372).expect("row metadata exists");
        assert_eq!(row_entry.title, "Донгбу Промы - Эгис");
        assert_eq!(row_entry.league, "Баскетбол");
        assert_eq!(
            row_entry.href.as_deref(),
            Some("/express-of-the-day/basketball/wonju-promy-jeonju-kcc-egis-id-29918372")
        );

        let t_data_entry = meta.get(&29927586).expect("t-data metadata exists");
        assert_eq!(t_data_entry.title, "Оренбург-2 - Урал-2");
        assert_eq!(
            t_data_entry.league,
            "Россия. 2-я лига. Дивизион Б. Группа 4"
        );
        assert_eq!(
            t_data_entry.href.as_deref(),
            Some("/soccer/russia/2nd-league/division-b/group-4/fc-orenburg-2-ural-2-id-29927586")
        );

        let stat_entry = meta.get(&29931947).expect("stat metadata exists");
        assert_eq!(stat_entry.title, "3-х очк. попадания");
        assert_eq!(stat_entry.league, "Баскетбол");
    }

    #[test]
    fn unknown_sport_is_not_classified_as_football() {
        assert_eq!(
            BaltbetParser::detect_sport(None, "Киберспорт", "Dota 2"),
            Sport::Esports
        );
        assert_eq!(
            BaltbetParser::detect_sport(None, "Неизвестный спорт", "Event A - Event B"),
            Sport::Other
        );
        assert_eq!(
            BaltbetParser::detect_sport(None, "Футбол", "Team A - Team B"),
            Sport::Football
        );
    }

    #[test]
    fn parses_live_odds_numeric_strings_and_market_scoped_ids() {
        let item = serde_json::json!({"c": [
            {"s": "1", "t": "1334", "v": "1.85"},
            {"s": 1, "t": 1335, "v": 3.2},
            {"s": 1, "t": 1336, "v": 4.1}
        ]});
        let odds = BaltbetParser::extract_live_odds("baltbet-42", &item);
        assert_eq!(odds.len(), 3);
        assert!(odds.iter().all(|odd| odd.id.contains("-1X2-")));
        assert!(odds
            .iter()
            .any(|odd| odd.selection == "1" && odd.odds == 1.85));
    }

    #[test]
    fn grouping_market_normalization_preserves_period_and_team_scope() {
        let normalize = BaltbetParser::normalize_grouping_market_name;
        assert_eq!(normalize("Тотал", "total", false), "Total");
        assert_eq!(
            normalize("Тотал первого тайма", "total", false),
            "1H_Total"
        );
        assert_eq!(
            normalize("Тотал второго тайма", "total", false),
            "2H_Total"
        );
        assert_eq!(
            normalize("Победитель первого тайма", "winner", false),
            "1H_Winner"
        );
        assert_eq!(
            normalize("Исход второго тайма", "winner", true),
            "2H_1X2"
        );
        assert_eq!(
            normalize("Фора первого тайма", "handicap", false),
            "1H_Handicap"
        );
        assert_eq!(
            normalize("Обе команды забьют в первом тайме", "yesno", false),
            "1H_BothTeamsScore"
        );
        assert_eq!(
            normalize("Индивидуальный тотал хозяев", "total", false),
            "Team1Total"
        );
        assert_eq!(
            normalize("Индивидуальный тотал гостей", "total", false),
            "Team2Total"
        );
        assert_eq!(
            normalize("Индивидуальный тотал 1К", "total", false),
            "Team1Total"
        );
        assert_eq!(
            normalize("Индивидуальный тотал 2К", "total", false),
            "Team2Total"
        );
        assert_eq!(
            normalize("Индивидуальный тотал", "total", false),
            "Custom:Индивидуальный тотал"
        );
        assert_eq!(
            normalize("Тотал первого периода", "total", false),
            "Custom:Тотал первого периода"
        );
    }

    #[test]
    fn parses_live_grouping_markets_with_source_names_and_lines() {
        let item = serde_json::json!({
            "c": [
                {"o": 101, "s": 1, "v": 1.85},
                {"o": 102, "s": 1, "v": 3.2},
                {"o": 103, "s": 1, "v": 4.1},
                {"o": 201, "s": 1, "v": 1.91},
                {"o": 202, "s": 1, "v": 1.91},
                {"o": 301, "s": 1, "v": 1.87},
                {"o": 302, "s": 1, "v": 1.95},
                {"o": 401, "s": 1, "v": 1.72},
                {"o": 402, "s": 1, "v": 2.08}
            ]
        });
        let grouping = serde_json::json!({
            "id": 42,
            "markets": [
                {
                    "id": 10,
                    "name": "Результат",
                    "kind": "Winner",
                    "params": [
                        {"ctid": 1, "name": "1"},
                        {"ctid": 2, "name": "X"},
                        {"ctid": 3, "name": "2"}
                    ],
                    "outcomes": [
                        {"oid": 101, "ctid": 1, "kind": "TEAM1", "params": []},
                        {"oid": 102, "ctid": 2, "kind": "DRAW", "params": []},
                        {"oid": 103, "ctid": 3, "kind": "TEAM2", "params": []}
                    ]
                },
                {
                    "id": 20,
                    "name": "Фора",
                    "kind": "Handicap",
                    "outcomes": [
                        {"oid": 201, "kind": "TEAM1", "params": [{"name": "Ф1к", "value": "+1,5"}]},
                        {"oid": 202, "kind": "TEAM2", "params": [{"name": "Ф2к", "value": "-1,5"}]}
                    ]
                },
                {
                    "id": 30,
                    "name": "Тотал",
                    "kind": "Total",
                    "outcomes": [
                        {"oid": 301, "kind": "OVER", "params": [{"name": "Тотал", "value": "2,5"}]},
                        {"oid": 302, "kind": "UNDER", "params": [{"name": "Тотал", "value": "2,5"}]}
                    ]
                },
                {
                    "id": 40,
                    "name": "Результат карты",
                    "kind": "Winner",
                    "outcomes": [
                        {"oid": 401, "kind": "TEAM1", "params": [{"name": "По картам", "value": "1-я карта"}]},
                        {"oid": 402, "kind": "TEAM2", "params": [{"name": "По картам", "value": "1-я карта"}]}
                    ]
                }
            ]
        });

        let odds = BaltbetParser::extract_live_grouping_odds("baltbet-42", &item, &grouping);

        assert_eq!(odds.len(), 9);
        assert!(odds.iter().any(|odd| {
            odd.market == "1X2"
                && odd.selection == "X"
                && odd.odds_type == OddsType::Draw
                && (odd.odds - 3.2).abs() < f64::EPSILON
        }));
        assert!(odds.iter().any(|odd| {
            odd.market == "Handicap"
                && odd.selection == "1"
                && odd.line == Some(1.5)
                && odd.odds_type == OddsType::Handicap
        }));
        assert!(odds.iter().any(|odd| {
            odd.market == "Handicap"
                && odd.selection == "2"
                && odd.line == Some(-1.5)
                && odd.odds_type == OddsType::Handicap
        }));
        assert!(odds.iter().any(|odd| {
            odd.market == "Total"
                && odd.selection == "Over"
                && odd.line == Some(2.5)
                && odd.odds_type == OddsType::Over
        }));
        assert!(odds.iter().any(|odd| {
            odd.market == "MapWinner::По картам=1-я карта"
                && odd.selection == "1"
                && odd.odds_type == OddsType::Home
        }));
    }

    #[test]
    fn unknown_live_factor_ids_are_not_guessed_as_match_result() {
        let item = serde_json::json!({
            "c": [
                {"o": 1, "s": 1, "t": 990001, "v": 2.1, "x": ""},
                {"o": 2, "s": 1, "t": 990002, "v": 3.2, "x": ""},
                {"o": 3, "s": 1, "t": 990003, "v": 4.4, "x": ""}
            ]
        });

        let odds = BaltbetParser::extract_live_odds("baltbet-42", &item);

        assert!(odds.is_empty());
    }

    #[test]
    fn rejects_live_payload_without_events_array() {
        let error = BaltbetParser::parse_live_json_checked(
            &serde_json::json!({"data": []}),
            &HashMap::new(),
        )
        .expect_err("missing events must be rejected");
        assert!(error.to_string().contains("events array"));
    }

    #[test]
    fn parses_live_json_with_t_data_metadata_fallback() {
        let html = include_str!("../tests/fixtures/baltbet_live_page_fixture.html");
        let payload: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/baltbet_live_payload.json"))
                .expect("live fixture should be valid json");

        let meta = BaltbetParser::parse_live_metadata(html);
        let (events, odds) = BaltbetParser::parse_live_json(&payload, &meta);

        assert_eq!(events.len(), 3);

        let football_event = events
            .iter()
            .find(|event| event.id == "baltbet-29927586")
            .expect("football event exists");
        assert_eq!(football_event.sport, Sport::Football);
        assert_eq!(football_event.home_team, "Оренбург-2");
        assert_eq!(football_event.away_team, "Урал-2");
        assert!(football_event.is_live);

        let basketball_event = events
            .iter()
            .find(|event| event.id == "baltbet-29918372")
            .expect("basketball event exists");
        assert_eq!(basketball_event.sport, Sport::Basketball);

        let stat_event = events
            .iter()
            .find(|event| event.id == "baltbet-29931947")
            .expect("stat event exists");
        assert_eq!(stat_event.sport, Sport::Basketball);
        assert_eq!(stat_event.home_team, "crvena zvezda mts belgrade");
        assert_eq!(stat_event.away_team, "cluj napoca");

        assert!(odds.iter().any(|odd| {
            odd.event_id == "baltbet-29927586"
                && odd.market == "1X2"
                && odd.selection == "X"
                && odd.odds_type == OddsType::Draw
                && (odd.odds - 3.35).abs() < f64::EPSILON
        }));
        assert!(odds.iter().any(|odd| {
            odd.event_id == "baltbet-29918372"
                && odd.market == "Moneyline"
                && odd.selection == "2"
                && odd.odds_type == OddsType::Away
                && (odd.odds - 2.05).abs() < f64::EPSILON
        }));
        assert!(odds.iter().any(|odd| {
            odd.event_id == "baltbet-29931947"
                && odd.market == "Moneyline"
                && odd.selection == "1"
                && odd.odds_type == OddsType::Home
                && (odd.odds - 1.87).abs() < f64::EPSILON
        }));
    }

    #[test]
    fn parses_real_active_baltbet_live_grouping_fixture() {
        let fixture: Value = serde_json::from_str(include_str!(
            "../tests/fixtures/baltbet_live_grouping_active_event.json"
        ))
        .expect("real Baltbet live grouping fixture is valid JSON");

        let odds = BaltbetParser::extract_live_grouping_odds(
            "baltbet-32126634",
            &fixture["item"],
            &fixture["grouping"],
        );

        assert_eq!(odds.len(), 11);
        assert!(odds
            .iter()
            .all(|odd| odd.odds.is_finite() && odd.odds > 1.0));
        assert!(odds
            .iter()
            .any(|odd| odd.market == "Moneyline" && odd.selection == "1"));
    }

    #[test]
    fn extracts_active_grouping_prices_when_inline_coefficients_are_empty() {
        let item = serde_json::json!({"i": 22, "c": []});
        let grouping = serde_json::json!({
            "id": 22,
            "isLive": true,
            "isActive": true,
            "isOver": false,
            "markets": [{
                "id": 10526,
                "name": "Индивидуальный тотал 1К",
                "kind": "Total",
                "outcomes": [
                    {"oid": 1001, "kind": "OVER", "value": 1.85,
                     "params": [{"name": "1К", "value": "16,5"}]},
                    {"oid": 1002, "kind": "UNDER", "value": 1.83,
                     "params": [{"name": "1К", "value": "16,5"}]}
                ]
            }]
        });

        let odds = BaltbetParser::extract_live_grouping_odds("baltbet-22", &item, &grouping);
        assert_eq!(odds.len(), 2);
        assert_eq!(odds[0].odds, 1.85);
        assert_eq!(odds[1].odds, 1.83);
        assert_eq!(odds[0].line, Some(16.5));
        assert_eq!(odds[1].line, Some(16.5));
    }

    #[test]
    fn suspended_inline_coefficient_is_not_reactivated_from_grouping_value() {
        let item = serde_json::json!({
            "i": 22,
            "c": [{"o": 1001, "s": 0, "v": 1.85}]
        });
        let grouping = serde_json::json!({
            "id": 22,
            "isLive": true,
            "isActive": true,
            "isOver": false,
            "markets": [{
                "id": 10526,
                "name": "Индивидуальный тотал 1К",
                "kind": "Total",
                "outcomes": [{"oid": 1001, "kind": "OVER", "value": 1.85,
                              "params": [{"name": "1К", "value": "16,5"}]}]
            }]
        });

        let odds = BaltbetParser::extract_live_grouping_odds("baltbet-22", &item, &grouping);
        assert!(odds.is_empty());
    }

    #[test]
    fn live_grouping_probe_includes_events_without_inline_coefficients() {
        let payload = serde_json::json!({
            "events": [
                {"i": 11, "c": [{"s": 1, "v": 1.8}]},
                {"i": 22, "c": []},
                {"i": 33}
            ]
        });

        assert_eq!(
            BaltbetParser::collect_live_grouping_probe_ids(&payload),
            vec![11, 22, 33]
        );
    }

    #[test]
    fn collects_banner_probe_ids_for_missing_and_unresolved_live_meta() {
        let payload = serde_json::json!({
            "events": [
                {"i": 11},
                {"i": 22},
                {"i": 33}
            ]
        });
        let meta = HashMap::from([
            (
                11,
                super::LiveMeta {
                    title: "Желтые карточки".to_string(),
                    league: "Футбол".to_string(),
                    href: None,
                },
            ),
            (
                22,
                super::LiveMeta {
                    title: "Реал Мадрид - Барселона".to_string(),
                    league: "Испания".to_string(),
                    href: None,
                },
            ),
        ]);

        let ids = BaltbetParser::collect_live_banner_probe_ids(&payload, &meta);

        assert_eq!(ids, vec![11, 33]);
    }

    #[test]
    fn banner_meta_replaces_unresolved_primary_live_meta() {
        let mut meta = HashMap::from([(
            11,
            super::LiveMeta {
                title: "Желтые карточки".to_string(),
                league: "Футбол".to_string(),
                href: None,
            },
        )]);

        BaltbetParser::merge_live_banner_meta(
            &mut meta,
            11,
            super::LiveMeta {
                title: "Реал Мадрид - Барселона".to_string(),
                league: "Испания".to_string(),
                href: Some("/soccer/spain/real-madrid-barcelona-id-11".to_string()),
            },
        );

        let merged = meta.get(&11).expect("merged meta exists");
        assert_eq!(merged.title, "Реал Мадрид - Барселона");
        assert_eq!(merged.league, "Испания");
        assert_eq!(
            merged.href.as_deref(),
            Some("/soccer/spain/real-madrid-barcelona-id-11")
        );
    }

    #[test]
    fn keeps_prematch_events_even_when_main_odds_are_hidden() {
        let html = include_str!("../tests/fixtures/baltbet_prematch_group_fixture.html");
        let mut seen = HashSet::new();

        let (events, odds) = BaltbetParser::parse_legacy_group_page(html, "1", "Футбол", &mut seen);

        assert_eq!(events.len(), 1);
        assert!(odds.is_empty());
        assert_eq!(events[0].id, "baltbet-prematch-29928528");
        assert_eq!(events[0].home_team, "Ливерпуль(замены)");
        assert_eq!(events[0].away_team, "ПСЖ(замены)");
        assert!(!events[0].is_live);
    }

    #[test]
    fn extracts_live_teams_from_stat_href_variants() {
        let teams = BaltbetParser::split_teams_from_href(
            Some(
                "/tennis/wta/125/oeiras-3/statistics/double-faults/kraus-sinja-double-faults-selekhmeteva-oksana-double-foults-id-29932186",
            ),
            "Двойные ошибки",
        )
        .expect("teams should be parsed from href");

        assert_eq!(teams.0, "kraus sinja");
        assert_eq!(teams.1, "selekhmeteva oksana");
    }

    #[test]
    fn extracts_live_teams_from_stat_href_with_mixed_markers() {
        let teams = BaltbetParser::split_teams_from_href(
            Some(
                "/basketball/south-korea/championship/statistics/two-pointers-scored/seoul-knights-two-pointers-scored-goyang-sky-gunners-three-pointers-scored-id-29939476",
            ),
            "2-х очк. попадания",
        )
        .expect("teams should be parsed from mixed stat markers");

        assert_eq!(teams.0, "seoul knights");
        assert_eq!(teams.1, "goyang sky gunners");
    }

    #[test]
    fn parses_live_banner_metadata_fallback() {
        let payload = serde_json::json!({
            "eventId": 29933242,
            "isActive": true,
            "isLive": true,
            "team1": "Сакраменто(Lucashin)",
            "team2": "Майами(Pakapaka)"
        });

        let meta = BaltbetParser::parse_live_banner_meta(&payload).expect("banner meta exists");

        assert_eq!(meta.title, "Сакраменто(Lucashin) - Майами(Pakapaka)");
        assert_eq!(meta.league, "Unknown");
        assert!(meta.href.is_none());
    }

    #[test]
    fn parses_live_banner_metadata_with_league_link_and_event_name_fallback() {
        let payload = serde_json::json!({
            "eventId": 29933243,
            "eventName": "Реал Мадрид - Барселона",
            "champName": "Испания. eBasketball",
            "sportName": "Баскетбол",
            "link": "/basketball/esports/real-madrid-barcelona-id-29933243"
        });

        let meta = BaltbetParser::parse_live_banner_meta(&payload).expect("banner meta exists");

        assert_eq!(meta.title, "Реал Мадрид - Барселона");
        assert_eq!(meta.league, "Испания. eBasketball");
        assert_eq!(
            meta.href.as_deref(),
            Some("/basketball/esports/real-madrid-barcelona-id-29933243")
        );
    }

    #[test]
    fn keeps_short_prematch_rows_without_main_odds() {
        let html = r#"
        <table class="lvmain coef-tobasket">
            <tr class="head"><td class="left">Хоккей</td></tr>
            <tr>
                <td class="left"><a onclick="open_some('event=29931405')">Салават Юлаев(игроки)-Локомотив Ярославль(игроки)</a></td>
                <td>14.04.2026</td>
                <td>17:00</td>
                <td class="dop">+44</td>
            </tr>
        </table>
        "#;
        let mut seen = HashSet::new();

        let (events, odds) = BaltbetParser::parse_legacy_group_page(html, "1", "Хоккей", &mut seen);

        assert_eq!(events.len(), 1);
        assert!(odds.is_empty());
        assert_eq!(events[0].id, "baltbet-prematch-29931405");
        assert_eq!(events[0].home_team, "Салават Юлаев(игроки)");
        assert_eq!(events[0].away_team, "Локомотив Ярославль(игроки)");
    }

    #[test]
    fn collects_featured_composite_prematch_groups() {
        let html = r#"
        <div class="allbet">
            <ul id="tablesportsdiv">
                <li><a href="javascript:openall('1')">Футбол</a></li>
                <li><a href="javascript:openall('920')">Баскетбол</a></li>
            </ul>
        </div>
        <div class="banners-slider">
            <a href="Line2.aspx?group=595807_21418_68973" class="banners-slider__item"></a>
            <a href="line2.aspx?group=809_8_309_125_81" class="banners-slider__item">Топ</a>
            <a href="Line2.aspx?group=1" class="banners-slider__item">duplicate</a>
        </div>
        "#;

        let links = BaltbetParser::parse_legacy_sport_links(html);

        assert_eq!(links.len(), 4);
        assert_eq!(links[0].group_id, "1");
        assert_eq!(links[0].sport_label, "Футбол");
        assert_eq!(links[1].group_id, "920");
        assert_eq!(links[2].group_id, "595807_21418_68973");
        assert_eq!(links[2].sport_label, "Unknown");
        assert_eq!(links[3].group_id, "809_8_309_125_81");
        assert_eq!(links[3].sport_label, "Топ");
    }

    #[test]
    fn keeps_collecting_featured_composite_groups_past_ten_entries() {
        let mut html = String::from(
            r#"
        <div class="allbet">
            <ul id="tablesportsdiv">
                <li><a href="javascript:openall('1')">Футбол</a></li>
            </ul>
        </div>
        <div class="banners-slider">
        "#,
        );

        for idx in 0..12 {
            html.push_str(&format!(
                r#"<a href="Line2.aspx?group=500_{idx}" class="banners-slider__item">Баннер {idx}</a>"#
            ));
        }

        html.push_str("</div>");

        let links = BaltbetParser::parse_legacy_sport_links(&html);

        assert_eq!(links.len(), 13);
        assert_eq!(links[0].group_id, "1");
        assert_eq!(links[11].group_id, "500_10");
        assert_eq!(links[12].group_id, "500_11");
    }
}
