// crates/parsers/src/winline_json.rs
//! Winline JSON snapshot parser.
//!
//! Parses externally supplied JSON snapshots. A live Winline feed adapter is not implemented yet,
//! so this parser stays disabled for automatic polling and never substitutes sample events.

use crate::base::{BookmakerParser, ParserResult};
use async_trait::async_trait;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use shared::{Event, Odd, Sport};
use std::collections::HashMap;

const BOOKMAKER_SLUG: &str = "winline_json";

/// Event as stored in JSON file
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WinlineEventJson {
    pub id: String,
    pub sport: String,
    pub league: String,
    pub home_team: String,
    pub away_team: String,
    pub start_time: Option<String>,
    pub is_live: bool,
    pub bookmaker_slug: String,
    pub raw_url: Option<String>,
    #[serde(default)]
    pub extra: HashMap<String, serde_json::Value>,
}

/// JSON file structure
#[derive(Debug, Deserialize, Serialize)]
pub struct WinlineEventsFile {
    pub timestamp: String,
    pub total_events: usize,
    pub live_events: usize,
    pub prematch_events: usize,
    pub events: Vec<WinlineEventJson>,
}

/// Main Winline JSON parser implementation
#[derive(Debug)]
pub struct WinlineJsonParser;

impl WinlineJsonParser {
    /// Load events from JSON file
    pub fn load_from_json(json_content: &str) -> Result<Vec<Event>, String> {
        let data: WinlineEventsFile = serde_json::from_str(json_content)
            .map_err(|e| format!("Failed to parse JSON: {}", e))?;

        let mut events = Vec::new();

        for json_event in data.events {
            let event = Self::convert_json_to_event(json_event)?;
            events.push(event);
        }

        Ok(events)
    }

    /// Parse sport string to Sport enum
    fn parse_sport(sport_str: &str) -> Sport {
        match sport_str.to_lowercase().as_str() {
            "football" | "футбол" => Sport::Football,
            "basketball" | "баскетбол" => Sport::Basketball,
            "hockey" | "хоккей" => Sport::Hockey,
            "tennis" | "теннис" => Sport::Tennis,
            "baseball" | "бейсбол" => Sport::Baseball,
            "cricket" => Sport::Cricket,
            "darts" => Sport::Darts,
            "futsal" => Sport::Futsal,
            "golf" => Sport::Golf,
            "handball" => Sport::Handball,
            "ice_hockey" | "icehockey" => Sport::Hockey,
            "mma" | "ufc" => Sport::Mma,
            "rugby" => Sport::Rugby,
            "snooker" => Sport::Snooker,
            "volleyball" | "волейбол" => Sport::Volleyball,
            _ => Sport::Other,
        }
    }

    /// Convert JSON event to Event struct
    fn convert_json_to_event(json_event: WinlineEventJson) -> Result<Event, String> {
        // Parse start_time string to DateTime<Utc> if present
        let start_time = json_event.start_time.and_then(|ts_str| {
            chrono::DateTime::parse_from_rfc3339(&ts_str)
                .map(|value| value.with_timezone(&Utc))
                .ok()
                .or_else(|| {
                    chrono::NaiveDateTime::parse_from_str(&ts_str, "%Y-%m-%dT%H:%M:%S")
                        .ok()
                        .map(|value| value.and_utc())
                })
        });

        let event = Event {
            id: json_event.id,
            sport: Self::parse_sport(&json_event.sport),
            league: json_event.league,
            home_team: json_event.home_team,
            away_team: json_event.away_team,
            start_time,
            is_live: json_event.is_live,
            bookmaker_slug: BOOKMAKER_SLUG.to_string(),
            raw_url: json_event.raw_url,
            extra: json_event.extra,
        };

        Ok(event)
    }
}

#[async_trait]
impl BookmakerParser for WinlineJsonParser {
    fn name(&self) -> &str {
        "Winline (JSON)"
    }

    fn slug(&self) -> &str {
        BOOKMAKER_SLUG
    }

    fn is_enabled(&self) -> bool {
        false
    }

    fn base_url(&self) -> &str {
        "https://winline.ru"
    }

    fn user_agent(&self) -> &str {
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36"
    }

    async fn fetch_events(&self) -> Result<Vec<Event>, Box<dyn std::error::Error + Send + Sync>> {
        Err(
            "live Winline feed is not implemented; parse a verified snapshot with load_from_json"
                .into(),
        )
    }

    async fn fetch_odds(
        &self,
        _event_id: &str,
    ) -> Result<Vec<Odd>, Box<dyn std::error::Error + Send + Sync>> {
        // Winline JSON parser doesn't provide separate odds
        Ok(Vec::new())
    }

    async fn fetch_all(&self) -> Result<ParserResult, Box<dyn std::error::Error + Send + Sync>> {
        let start = std::time::Instant::now();

        let events = self.fetch_events().await?;
        let _live_count = events.iter().filter(|e| e.is_live).count();
        let _prematch_count = events.iter().filter(|e| !e.is_live).count();

        Ok(ParserResult {
            bookmaker: BOOKMAKER_SLUG.to_string(),
            events,
            odds: Vec::new(),
            fetch_time_ms: start.elapsed().as_millis() as u64,
            timestamp: Utc::now(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn get_sample_json() -> String {
        r#"{
  "timestamp": "2026-04-20T20:15:00",
  "total_events": 2,
  "live_events": 1,
  "prematch_events": 1,
  "events": [
    {
      "id": "live_1",
      "sport": "football",
      "league": "Российская Премьер-лига",
      "home_team": "Спартак",
      "away_team": "ЦСКА",
      "start_time": "2026-04-20T19:38:55",
      "is_live": true,
      "bookmaker_slug": "winline",
      "raw_url": "https://winline.ru/live/match/1",
      "extra": {
        "minutes": 42,
        "score": "2-1",
        "odds_1x2": [1.72, 3.45, 2.18],
        "total_over": 2.45,
        "total_under": 1.65
      }
    },
    {
      "id": "match_17",
      "sport": "football",
      "league": "Ла Лига",
      "home_team": "Реал Мадрид",
      "away_team": "Атлетико",
      "start_time": "2026-04-20T21:00:00",
      "is_live": false,
      "bookmaker_slug": "winline",
      "raw_url": "https://winline.ru/stavki/match/17",
      "extra": {
        "odds_1x2": [1.85, 3.2, 2.05],
        "total_over": 2.45,
        "total_under": 1.65
      }
    }
  ]
}"#
        .to_string()
    }

    #[test]
    fn test_load_from_json() {
        let json = get_sample_json();
        let events = WinlineJsonParser::load_from_json(&json).unwrap();

        assert_eq!(events.len(), 2, "Should load 2 events");
        assert_eq!(events[0].home_team, "Спартак");
        assert_eq!(events[0].away_team, "ЦСКА");
        assert!(events[0].is_live);
        assert!(!events[1].is_live);
    }

    #[test]
    fn test_sport_parsing() {
        assert_eq!(WinlineJsonParser::parse_sport("football"), Sport::Football);
        assert_eq!(WinlineJsonParser::parse_sport("футбол"), Sport::Football);
        assert_eq!(
            WinlineJsonParser::parse_sport("basketball"),
            Sport::Basketball
        );
        assert_eq!(WinlineJsonParser::parse_sport("hockey"), Sport::Hockey);
        assert_eq!(WinlineJsonParser::parse_sport("tennis"), Sport::Tennis);
    }

    #[test]
    fn test_event_structure() {
        let json = get_sample_json();
        let events = WinlineJsonParser::load_from_json(&json).unwrap();
        let event = &events[0];

        assert!(!event.id.is_empty());
        assert_eq!(event.bookmaker_slug, "winline_json");
        assert!(event.raw_url.is_some());
        assert_eq!(
            event.start_time.map(|value| value.to_rfc3339()),
            Some("2026-04-20T19:38:55+00:00".to_string())
        );
    }

    #[test]
    fn test_live_vs_prematch_count() {
        let json = get_sample_json();
        let events = WinlineJsonParser::load_from_json(&json).unwrap();

        let live_count = events.iter().filter(|e| e.is_live).count();
        let prematch_count = events.iter().filter(|e| !e.is_live).count();

        assert!(live_count > 0);
        assert!(prematch_count > 0);
        assert_eq!(live_count + prematch_count, events.len());
    }

    #[test]
    fn test_odds_parsing() {
        let json = get_sample_json();
        let events = WinlineJsonParser::load_from_json(&json).unwrap();
        let event = &events[0];

        // Check that odds are present in extra
        assert!(event.extra.contains_key("odds_1x2"));
    }

    #[tokio::test]
    async fn test_fetch_events_fails_closed_without_a_live_feed() {
        let parser = WinlineJsonParser;
        assert!(!parser.is_enabled());

        let error = parser
            .fetch_events()
            .await
            .expect_err("a sample snapshot must not be reported as a live feed");

        assert!(error
            .to_string()
            .contains("live Winline feed is not implemented"));
    }

    #[tokio::test]
    async fn test_fetch_all_fails_closed_without_a_live_feed() {
        let parser = WinlineJsonParser;
        let error = parser
            .fetch_all()
            .await
            .expect_err("fetch_all must not return synthetic Winline events");

        assert!(error
            .to_string()
            .contains("live Winline feed is not implemented"));
    }

    #[test]
    fn test_invalid_json() {
        let invalid_json = "{ invalid json }";
        let result = WinlineJsonParser::load_from_json(invalid_json);

        assert!(result.is_err());
    }
}
