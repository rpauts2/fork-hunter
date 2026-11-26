use crate::base::{BookmakerParser, ParserResult};
use chrono::{Duration as ChronoDuration, Utc};
use shared::{HealthStatus, ParserHealth};
use std::collections::HashSet;
use std::sync::Arc;
use tracing::warn;

const MAX_ODDS_AGE_SECS: i64 = 5 * 60;
const MAX_FUTURE_SKEW_SECS: i64 = 30;
const MIN_OVERALL_ODDS_COVERAGE: f64 = 0.90;
const MIN_FEED_ODDS_COVERAGE: f64 = 0.80;
const MAX_STALE_ODDS_RATIO: f64 = 0.01;
const MAX_INVALID_ODDS_RATIO: f64 = 0.01;

#[derive(Clone)]
pub struct HealthChecker {
    #[allow(dead_code)]
    check_interval_secs: u64,
    #[allow(dead_code)]
    max_consecutive_failures: u32,
}

impl HealthChecker {
    pub fn new(check_interval_secs: u64, max_consecutive_failures: u32) -> Self {
        Self {
            check_interval_secs,
            max_consecutive_failures,
        }
    }

    pub async fn check_parser(&self, parser: Arc<dyn BookmakerParser>) -> ParserHealth {
        let start = std::time::Instant::now();
        let readiness = parser.readiness();
        let diagnostics = readiness
            .as_ref()
            .map(|item| item.checks.clone())
            .unwrap_or_default();

        match parser.fetch_all().await {
            Ok(result) => {
                let elapsed = start.elapsed().as_millis() as f64;
                let (healthy, reason) = assess_result(parser.slug(), &result);
                ParserHealth {
                    bookmaker: parser.slug().to_string(),
                    status: if healthy {
                        HealthStatus::Healthy
                    } else {
                        HealthStatus::Degraded
                    },
                    last_success: Some(chrono::Utc::now()),
                    last_error: reason,
                    consecutive_failures: 0,
                    avg_response_time_ms: elapsed,
                    events_parsed: result.events.len() as u64,
                    uptime_percent: if healthy { 100.0 } else { 0.0 },
                    readiness,
                    diagnostics,
                }
            }
            Err(error) => {
                let elapsed = start.elapsed().as_millis() as f64;
                warn!(
                    parser = parser.slug(),
                    error = error.to_string(),
                    "Parser health check failed"
                );
                ParserHealth {
                    bookmaker: parser.slug().to_string(),
                    status: HealthStatus::Unhealthy,
                    last_success: None,
                    last_error: Some(error.to_string()),
                    consecutive_failures: 1,
                    avg_response_time_ms: elapsed,
                    events_parsed: 0,
                    uptime_percent: 0.0,
                    readiness,
                    diagnostics,
                }
            }
        }
    }

    pub async fn check_all(&self, parsers: Vec<Arc<dyn BookmakerParser>>) -> Vec<ParserHealth> {
        use futures::StreamExt;

        // A slow bookmaker must not delay health checks for every other source.
        let mut results =
            futures::stream::iter(parsers.into_iter().map(|parser| self.check_parser(parser)))
                .buffer_unordered(3)
                .collect::<Vec<_>>()
                .await;
        results.sort_by(|left, right| left.bookmaker.cmp(&right.bookmaker));
        results
    }
}

fn assess_result(slug: &str, result: &ParserResult) -> (bool, Option<String>) {
    if !result.bookmaker.eq_ignore_ascii_case(slug) {
        return (
            false,
            Some(format!(
                "parser result bookmaker mismatch: expected {slug}, received {}",
                result.bookmaker
            )),
        );
    }
    if result.events.is_empty() {
        return (false, Some("parser returned no events".to_string()));
    }
    if result.odds.is_empty() {
        return (
            false,
            Some("events received, but no odds were returned".to_string()),
        );
    }

    let mut event_owners = std::collections::HashMap::new();
    let mut duplicate_event_ids = 0usize;
    let mut invalid_event_ids = 0usize;
    let mut wrong_event_owners = 0usize;
    for event in &result.events {
        if event.id.trim().is_empty() {
            invalid_event_ids += 1;
            continue;
        }
        if !event.bookmaker_slug.eq_ignore_ascii_case(slug) {
            wrong_event_owners += 1;
        }
        if event_owners
            .insert(
                event.id.as_str(),
                (event.bookmaker_slug.as_str(), event.is_live),
            )
            .is_some()
        {
            duplicate_event_ids += 1;
        }
    }
    if invalid_event_ids > 0 {
        return (
            false,
            Some(format!("events with empty IDs detected: {invalid_event_ids}")),
        );
    }
    if duplicate_event_ids > 0 {
        return (
            false,
            Some(format!(
                "duplicate event IDs detected: {duplicate_event_ids}"
            )),
        );
    }
    if wrong_event_owners > 0 {
        return (
            false,
            Some(format!(
                "events owned by another bookmaker detected: {wrong_event_owners}"
            )),
        );
    }

    let event_ids: HashSet<&str> = event_owners.keys().copied().collect();
    let now = Utc::now();
    let oldest_allowed = now - ChronoDuration::seconds(MAX_ODDS_AGE_SECS);
    let newest_allowed = now + ChronoDuration::seconds(MAX_FUTURE_SKEW_SECS);

    let mut valid_count = 0usize;
    let mut stale_count = 0usize;
    let mut orphan_count = 0usize;
    let mut wrong_bookmaker_count = 0usize;
    let mut invalid_count = 0usize;
    let mut duplicate_odd_ids = 0usize;
    let mut seen_odd_ids = HashSet::new();
    let mut events_with_odds = HashSet::new();
    let mut live_events_with_odds = HashSet::new();
    let mut prematch_events_with_odds = HashSet::new();

    for odd in &result.odds {
        if !seen_odd_ids.insert(odd.id.as_str()) {
            duplicate_odd_ids += 1;
            continue;
        }
        if !event_ids.contains(odd.event_id.as_str()) {
            orphan_count += 1;
            continue;
        }
        let event_owner_matches = event_owners
            .get(odd.event_id.as_str())
            .is_some_and(|(owner, _)| owner.eq_ignore_ascii_case(slug));
        if !odd.bookmaker_slug.eq_ignore_ascii_case(slug) || !event_owner_matches {
            wrong_bookmaker_count += 1;
            continue;
        }
        if odd.id.trim().is_empty()
            || odd.market.trim().is_empty()
            || odd.selection.trim().is_empty()
            || !odd.odds.is_finite()
            || odd.odds <= 1.0
        {
            invalid_count += 1;
            continue;
        }
        if odd.timestamp < oldest_allowed || odd.timestamp > newest_allowed {
            stale_count += 1;
            continue;
        }
        valid_count += 1;
        events_with_odds.insert(odd.event_id.as_str());
        if event_owners
            .get(odd.event_id.as_str())
            .is_some_and(|(_, is_live)| *is_live)
        {
            live_events_with_odds.insert(odd.event_id.as_str());
        } else {
            prematch_events_with_odds.insert(odd.event_id.as_str());
        }
    }

    if duplicate_odd_ids > 0 {
        return (
            false,
            Some(format!("duplicate odd IDs detected: {duplicate_odd_ids}")),
        );
    }

    if valid_count == 0 {
        let reason = format!(
            "no usable odds: {} total, {} capture-timestamp outliers (bookmaker quote freshness is not available in the model), {} orphaned, {} from another bookmaker, {} invalid",
            result.odds.len(),
            stale_count,
            orphan_count,
            wrong_bookmaker_count,
            invalid_count
        );
        return (false, Some(reason));
    }

    let live_event_ids: HashSet<&str> = result
        .events
        .iter()
        .filter(|event| event.is_live)
        .map(|event| event.id.as_str())
        .collect();
    let prematch_event_ids: HashSet<&str> = result
        .events
        .iter()
        .filter(|event| !event.is_live)
        .map(|event| event.id.as_str())
        .collect();
    let live_coverage = if live_event_ids.is_empty() {
        1.0
    } else {
        live_events_with_odds.len() as f64 / live_event_ids.len() as f64
    };
    let prematch_coverage = if prematch_event_ids.is_empty() {
        1.0
    } else {
        prematch_events_with_odds.len() as f64 / prematch_event_ids.len() as f64
    };
    let overall_coverage = events_with_odds.len() as f64 / result.events.len() as f64;
    let stale_ratio = stale_count as f64 / result.odds.len() as f64;
    let invalid_ratio = (invalid_count + wrong_bookmaker_count) as f64 / result.odds.len() as f64;
    let orphan_ratio = orphan_count as f64 / result.odds.len() as f64;

    if overall_coverage < MIN_OVERALL_ODDS_COVERAGE {
        return (
            false,
            Some(format!(
                "overall odds coverage too low: {:.1}% (minimum {:.0}%)",
                overall_coverage * 100.0,
                MIN_OVERALL_ODDS_COVERAGE * 100.0
            )),
        );
    }
    if !live_event_ids.is_empty() && live_coverage < MIN_FEED_ODDS_COVERAGE {
        return (
            false,
            Some(format!(
                "LIVE odds coverage too low: {:.1}% (minimum {:.0}%)",
                live_coverage * 100.0,
                MIN_FEED_ODDS_COVERAGE * 100.0
            )),
        );
    }
    if !prematch_event_ids.is_empty() && prematch_coverage < MIN_FEED_ODDS_COVERAGE {
        return (
            false,
            Some(format!(
                "PREMATCH odds coverage too low: {:.1}% (minimum {:.0}%)",
                prematch_coverage * 100.0,
                MIN_FEED_ODDS_COVERAGE * 100.0
            )),
        );
    }
    if stale_ratio > MAX_STALE_ODDS_RATIO {
        return (
            false,
            Some(format!(
                "capture-timestamp outliers exceed {:.0}% of odds ({:.1}%); bookmaker quote freshness is not measured",
                MAX_STALE_ODDS_RATIO * 100.0,
                stale_ratio * 100.0
            )),
        );
    }
    if invalid_ratio > MAX_INVALID_ODDS_RATIO {
        return (
            false,
            Some(format!(
                "invalid odds exceed {:.0}% of returned odds ({:.1}%)",
                MAX_INVALID_ODDS_RATIO * 100.0,
                invalid_ratio * 100.0
            )),
        );
    }
    if orphan_ratio > MAX_INVALID_ODDS_RATIO {
        return (
            false,
            Some(format!(
                "orphan odds exceed {:.0}% of returned odds ({:.1}%)",
                MAX_INVALID_ODDS_RATIO * 100.0,
                orphan_ratio * 100.0
            )),
        );
    }
    (true, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::base::ParserResult;
    use async_trait::async_trait;
    use chrono::Utc;
    use shared::{Event, Odd, OddsType, ParserReadiness, Sport};
    use std::collections::HashMap;

    #[derive(Debug)]
    struct FixtureParser {
        result: ParserResult,
    }

    #[async_trait]
    impl BookmakerParser for FixtureParser {
        fn name(&self) -> &str {
            "Fixture"
        }
        fn slug(&self) -> &str {
            "fixture"
        }
        fn is_enabled(&self) -> bool {
            true
        }
        fn readiness(&self) -> Option<ParserReadiness> {
            None
        }
        async fn fetch_events(
            &self,
        ) -> Result<Vec<Event>, Box<dyn std::error::Error + Send + Sync>> {
            Ok(self.result.events.clone())
        }
        async fn fetch_odds(
            &self,
            _event_id: &str,
        ) -> Result<Vec<Odd>, Box<dyn std::error::Error + Send + Sync>> {
            Ok(self.result.odds.clone())
        }
        async fn fetch_all(
            &self,
        ) -> Result<ParserResult, Box<dyn std::error::Error + Send + Sync>> {
            Ok(self.result.clone())
        }
        fn base_url(&self) -> &str {
            "https://fixture.invalid"
        }
        fn user_agent(&self) -> &str {
            "fixture-test"
        }
    }

    fn fixture_result(odds: Vec<Odd>) -> ParserResult {
        let event = Event {
            id: "event-1".into(),
            sport: Sport::Football,
            league: "Test league".into(),
            home_team: "Home".into(),
            away_team: "Away".into(),
            start_time: None,
            is_live: false,
            bookmaker_slug: "fixture".into(),
            raw_url: None,
            extra: HashMap::new(),
        };
        ParserResult::new("fixture", vec![event], odds, 1)
    }

    fn valid_odd() -> Odd {
        Odd {
            id: "odd-1".into(),
            event_id: "event-1".into(),
            bookmaker_slug: "fixture".into(),
            market: "1X2".into(),
            selection: "1".into(),
            odds: 2.1,
            odds_type: OddsType::Home,
            line: None,
            timestamp: Utc::now(),
        }
    }

    async fn check(result: ParserResult) -> ParserHealth {
        HealthChecker::new(30, 3)
            .check_parser(Arc::new(FixtureParser { result }))
            .await
    }

    #[tokio::test]
    async fn events_without_odds_are_degraded_not_healthy() {
        let health = check(fixture_result(vec![])).await;
        assert!(matches!(health.status, HealthStatus::Degraded));
        assert!(health.last_error.as_deref().unwrap().contains("no odds"));
    }

    #[tokio::test]
    async fn fresh_mapped_odds_make_parser_healthy() {
        let health = check(fixture_result(vec![valid_odd()])).await;
        assert!(matches!(health.status, HealthStatus::Healthy));
        assert!(health.last_error.is_none());
    }

    #[tokio::test]
    async fn low_event_odds_coverage_is_degraded() {
        let mut result = fixture_result(vec![valid_odd()]);
        let mut second_event = result.events[0].clone();
        second_event.id = "event-2".into();
        result.events.push(second_event);

        let health = check(result).await;
        assert!(matches!(health.status, HealthStatus::Degraded));
        assert!(health
            .last_error
            .as_deref()
            .unwrap()
            .contains("overall odds coverage"));
    }

    #[tokio::test]
    async fn parser_result_for_another_bookmaker_is_degraded() {
        let mut result = fixture_result(vec![valid_odd()]);
        result.bookmaker = "other".into();

        let health = check(result).await;
        assert!(matches!(health.status, HealthStatus::Degraded));
        assert!(health
            .last_error
            .as_deref()
            .unwrap()
            .contains("parser result bookmaker mismatch"));
    }

    #[tokio::test]
    async fn events_with_empty_ids_are_degraded() {
        let mut result = fixture_result(vec![valid_odd()]);
        result.events[0].id.clear();

        let health = check(result).await;
        assert!(matches!(health.status, HealthStatus::Degraded));
        assert!(health
            .last_error
            .as_deref()
            .unwrap()
            .contains("events with empty IDs"));
    }

    #[tokio::test]
    async fn foreign_event_owner_is_degraded_even_without_linked_odds() {
        let mut result = fixture_result(vec![valid_odd()]);
        let mut foreign_event = result.events[0].clone();
        foreign_event.id = "event-2".into();
        foreign_event.bookmaker_slug = "other".into();
        result.events.push(foreign_event);

        let health = check(result).await;
        assert!(matches!(health.status, HealthStatus::Degraded));
        assert!(health
            .last_error
            .as_deref()
            .unwrap()
            .contains("events owned by another bookmaker"));
    }

    #[tokio::test]
    async fn duplicate_event_ids_are_degraded() {
        let mut result = fixture_result(vec![valid_odd()]);
        result.events.push(result.events[0].clone());

        let health = check(result).await;
        assert!(matches!(health.status, HealthStatus::Degraded));
        assert!(health
            .last_error
            .as_deref()
            .unwrap()
            .contains("duplicate event IDs"));
    }

    #[tokio::test]
    async fn duplicate_odd_ids_are_degraded() {
        let odd = valid_odd();
        let health = check(fixture_result(vec![odd.clone(), odd])).await;

        assert!(matches!(health.status, HealthStatus::Degraded));
        assert!(health
            .last_error
            .as_deref()
            .unwrap()
            .contains("duplicate odd IDs"));
    }

    #[tokio::test]
    async fn stale_odds_do_not_make_parser_healthy() {
        let mut odd = valid_odd();
        odd.timestamp = Utc::now() - ChronoDuration::minutes(20);
        let health = check(fixture_result(vec![odd])).await;
        assert!(matches!(health.status, HealthStatus::Degraded));
        assert!(health
            .last_error
            .as_deref()
            .unwrap()
            .contains("no usable odds"));
    }

    #[tokio::test]
    async fn odds_for_unknown_events_do_not_make_parser_healthy() {
        let mut odd = valid_odd();
        odd.event_id = "missing-event".into();
        let health = check(fixture_result(vec![odd])).await;
        assert!(matches!(health.status, HealthStatus::Degraded));
        assert!(health
            .last_error
            .as_deref()
            .unwrap()
            .contains("no usable odds"));
    }

    #[tokio::test]
    async fn odds_linked_to_an_event_owned_by_another_bookmaker_are_rejected() {
        let mut result = fixture_result(vec![valid_odd()]);
        result.events[0].bookmaker_slug = "other".into();
        let health = check(result).await;
        assert!(matches!(health.status, HealthStatus::Degraded));
        assert!(health
            .last_error
            .as_deref()
            .unwrap()
            .contains("no usable odds"));
    }

    #[tokio::test]
    async fn odds_from_another_bookmaker_do_not_make_parser_healthy() {
        let mut odd = valid_odd();
        odd.bookmaker_slug = "other".into();
        let health = check(fixture_result(vec![odd])).await;
        assert!(matches!(health.status, HealthStatus::Degraded));
    }
}
