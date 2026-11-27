use crate::betboom::BetboomParser;
use crate::betcity::BetcityParser;
use crate::ligastavok::LigaStavokParser;
use crate::melbet::MelbetParser;
use crate::parser_factory::{ParserFactory, ACTIVE_RUSSIAN_BOOKMAKER_SLUGS};
use crate::tennisi::TennisiParser;
use crate::zenit::ZenitParser;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use serde::Serialize;
use shared::Event;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

pub const DEFAULT_RUNTIME_DIAGNOSTIC_SLUGS: &[&str] = ACTIVE_RUSSIAN_BOOKMAKER_SLUGS;

pub const LIVE_THRESHOLD: usize = 150;
pub const PREMATCH_THRESHOLD: usize = 3000;

// A parser may return a large catalogue while still dropping most prices. These
// gates make runtime readiness reflect usable, fresh market coverage rather
// than event volume alone.
const MIN_OVERALL_ODDS_COVERAGE_PERCENT: f64 = 90.0;
const MIN_FEED_ODDS_COVERAGE_PERCENT: f64 = 80.0;
const MAX_STALE_ODDS_RATIO: f64 = 0.01;
const MAX_INVALID_ODDS_RATIO: f64 = 0.01;

/// Smaller Russian operators publish materially smaller live/prematch catalogs.
/// These are minimum feed-presence gates; event/odds linkage and freshness checks
/// remain identical for every bookmaker.
fn thresholds_for_bookmaker(slug: &str) -> (usize, usize) {
    match canonical_bookmaker_slug(slug).as_str() {
        "winline" => (100, 2_500),
        "sportbet" => (25, 1_000),
        "leon" => (100, 2_800),
        // Calibrated against three independent live snapshots. These are
        // still 95%+ of the lowest observed price-bearing volume, rather than
        // raw event counts that include scoreboards without bookmaker odds.
        "betcity" => (140, 3_000),
        "zenit" => (125, 3_000),
        "olimp" => (100, 1_450),
        _ => (LIVE_THRESHOLD, PREMATCH_THRESHOLD),
    }
}
const FETCH_TIMEOUT: Duration = Duration::from_secs(90);
type RuntimeFetchResult = Result<(Vec<Event>, Vec<shared::Odd>), String>;

#[derive(Debug, Clone, Serialize)]
pub struct RuntimeKpiCheck {
    pub actual: usize,
    pub target: usize,
    pub missing: usize,
    pub passed: bool,
}

impl RuntimeKpiCheck {
    fn new(actual: usize, target: usize) -> Self {
        Self {
            actual,
            target,
            missing: target.saturating_sub(actual),
            passed: actual >= target,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct DuplicateOddIdSample {
    pub id: String,
    pub event_id: String,
    pub first_market: String,
    pub duplicate_market: String,
    pub first_selection: String,
    pub duplicate_selection: String,
    pub first_line: Option<f64>,
    pub duplicate_line: Option<f64>,
    pub first_odds: f64,
    pub duplicate_odds: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct EventWithoutOddsSample {
    pub id: String,
    pub sport: String,
    pub league: String,
    pub home_team: String,
    pub away_team: String,
    pub is_live: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct RuntimeCountReport {
    pub bookmaker_slug: String,
    pub total_events: usize,
    pub live_events: usize,
    pub prematch_events: usize,
    pub total_odds: usize,
    pub valid_odds: usize,
    /// Raw odds count by parser market label; useful for market-depth audits.
    pub market_counts: BTreeMap<String, usize>,
    pub events_with_odds: usize,
    /// First few events with no linked valid price, for diagnosing coverage gaps.
    pub events_without_odds_samples: Vec<EventWithoutOddsSample>,
    pub duplicate_event_ids: usize,
    pub duplicate_odd_ids: usize,
    /// First few collisions, retained to distinguish exact source duplicates
    /// from distinct markets that accidentally share a parser-generated ID.
    pub duplicate_odd_id_samples: Vec<DuplicateOddIdSample>,
    pub invalid_odds: usize,
    pub odds_coverage_percent: f64,
    pub live_events_with_odds: usize,
    pub prematch_events_with_odds: usize,
    pub live_odds_coverage_percent: f64,
    pub prematch_odds_coverage_percent: f64,
    /// Count of parser-generated capture timestamps outside the allowed window.
    /// This is not a bookmaker quote-update timestamp.
    pub stale_odds: usize,
    /// Current parser model stores local ingestion time; source quote freshness
    /// is not verified until bookmaker-provided timestamps are mapped.
    pub quote_timestamp_basis: &'static str,
    pub bookmaker_quote_freshness_verified: bool,
    pub quality_passed: bool,
    pub quality_issue: Option<String>,
    pub live_kpi: RuntimeKpiCheck,
    pub prematch_kpi: RuntimeKpiCheck,
    pub live_threshold_met: bool,
    pub prematch_threshold_met: bool,
    pub passed: bool,
    pub runtime_only: bool,
    pub duration_ms: u128,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RuntimeDiagnosticsSummary {
    pub requested_slugs: Vec<String>,
    pub total_reports: usize,
    pub passed_reports: usize,
    pub failed_reports: usize,
    pub live_threshold: usize,
    pub prematch_threshold: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct RuntimeDiagnosticsNightlySummary {
    pub status: &'static str,
    pub exit_code: i32,
    pub failing_slugs: Vec<String>,
    pub live_gap_total: usize,
    pub prematch_gap_total: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct RuntimeDiagnosticsAggregate {
    pub total_events: usize,
    pub live_events: usize,
    pub prematch_events: usize,
    pub live_threshold_passes: usize,
    pub prematch_threshold_passes: usize,
    pub passed_reports: usize,
    pub failed_reports: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct RuntimeDiagnosticsRun {
    pub generated_at: DateTime<Utc>,
    pub summary: RuntimeDiagnosticsSummary,
    pub aggregate: RuntimeDiagnosticsAggregate,
    pub nightly: RuntimeDiagnosticsNightlySummary,
    pub reports: Vec<RuntimeCountReport>,
}

impl RuntimeCountReport {
    fn from_result(
        bookmaker_slug: &str,
        events: &[Event],
        odds: &[shared::Odd],
        duration_ms: u128,
        runtime_only: bool,
    ) -> Self {
        let live_events = events.iter().filter(|event| event.is_live).count();
        let prematch_events = events.len().saturating_sub(live_events);
        let mut market_counts = BTreeMap::new();
        for odd in odds {
            *market_counts
                .entry(odd.market.trim().to_string())
                .or_insert(0) += 1;
        }
        let (live_threshold, prematch_threshold) = thresholds_for_bookmaker(bookmaker_slug);
        let mut event_owners = std::collections::HashMap::new();
        let mut duplicate_event_ids = 0usize;
        for event in events {
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
        let event_ids = event_owners
            .keys()
            .copied()
            .collect::<std::collections::HashSet<_>>();
        // Odd.timestamp is currently assigned by the parser at ingestion time.
        // This rejects malformed capture timestamps but cannot prove that the
        // bookmaker itself refreshed the price recently.
        let now = Utc::now();
        let oldest_allowed = now - ChronoDuration::minutes(5);
        let newest_allowed = now + ChronoDuration::seconds(30);
        let mut valid_odds = 0usize;
        let mut stale_odds = 0usize;
        let mut invalid_odds = 0usize;
        let mut duplicate_odd_ids = 0usize;
        let mut duplicate_odd_id_samples = Vec::new();
        let mut seen_odd_ids: std::collections::HashMap<&str, &shared::Odd> =
            std::collections::HashMap::new();
        let mut events_with_odds = std::collections::HashSet::new();
        let mut live_events_with_odds = std::collections::HashSet::new();
        let mut prematch_events_with_odds = std::collections::HashSet::new();

        for odd in odds {
            if let Some(first) = seen_odd_ids.get(odd.id.as_str()) {
                duplicate_odd_ids += 1;
                if duplicate_odd_id_samples.len() < 10 {
                    duplicate_odd_id_samples.push(DuplicateOddIdSample {
                        id: odd.id.clone(),
                        event_id: odd.event_id.clone(),
                        first_market: first.market.clone(),
                        duplicate_market: odd.market.clone(),
                        first_selection: first.selection.clone(),
                        duplicate_selection: odd.selection.clone(),
                        first_line: first.line,
                        duplicate_line: odd.line,
                        first_odds: first.odds,
                        duplicate_odds: odd.odds,
                    });
                }
                continue;
            }
            seen_odd_ids.insert(odd.id.as_str(), odd);
            if odd.timestamp < oldest_allowed || odd.timestamp > newest_allowed {
                stale_odds += 1;
                continue;
            }
            if odd.id.trim().is_empty()
                || odd.market.trim().is_empty()
                || odd.selection.trim().is_empty()
                || !odd.odds.is_finite()
                || odd.odds <= 1.0
                || canonical_bookmaker_slug(&odd.bookmaker_slug)
                    != canonical_bookmaker_slug(bookmaker_slug)
                || !event_ids.contains(odd.event_id.as_str())
                || event_owners
                    .get(odd.event_id.as_str())
                    .is_none_or(|(owner, _)| {
                        canonical_bookmaker_slug(owner) != canonical_bookmaker_slug(bookmaker_slug)
                    })
            {
                invalid_odds += 1;
                continue;
            }
            valid_odds += 1;
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

        // Volume gates are for actionable events with at least one valid,
        // bookmaker-owned price. Unpriced scoreboards/informational cards do
        // not count toward scan-ready live or prematch coverage.
        let events_without_odds_samples = events
            .iter()
            .filter(|event| !events_with_odds.contains(event.id.as_str()))
            .take(10)
            .map(|event| EventWithoutOddsSample {
                id: event.id.clone(),
                sport: format!("{:?}", event.sport),
                league: event.league.clone(),
                home_team: event.home_team.clone(),
                away_team: event.away_team.clone(),
                is_live: event.is_live,
            })
            .collect();
        let live_kpi = RuntimeKpiCheck::new(live_events_with_odds.len(), live_threshold);
        let prematch_kpi =
            RuntimeKpiCheck::new(prematch_events_with_odds.len(), prematch_threshold);

        let odds_coverage_percent = if events.is_empty() {
            0.0
        } else {
            events_with_odds.len() as f64 / events.len() as f64 * 100.0
        };
        let live_event_count = live_events;
        let prematch_event_count = prematch_events;
        let live_odds_coverage_percent = if live_event_count == 0 {
            100.0
        } else {
            live_events_with_odds.len() as f64 / live_event_count as f64 * 100.0
        };
        let prematch_odds_coverage_percent = if prematch_event_count == 0 {
            100.0
        } else {
            prematch_events_with_odds.len() as f64 / prematch_event_count as f64 * 100.0
        };
        let stale_ratio = if odds.is_empty() {
            0.0
        } else {
            stale_odds as f64 / odds.len() as f64
        };
        let non_stale_unique_odds = odds.len().saturating_sub(stale_odds + duplicate_odd_ids);
        let invalid_ratio = if non_stale_unique_odds == 0 {
            0.0
        } else {
            invalid_odds as f64 / non_stale_unique_odds as f64
        };
        let quality_issue = if events.is_empty() {
            Some("no events received".to_string())
        } else if duplicate_event_ids > 0 {
            Some(format!(
                "{} duplicate event IDs detected",
                duplicate_event_ids
            ))
        } else if odds.is_empty() {
            Some("events received, but no odds were returned".to_string())
        } else if valid_odds == 0 {
            Some("no fresh, valid odds are linked to returned events".to_string())
        } else if live_event_count > 0 && live_odds_coverage_percent < 5.0 {
            Some(format!(
                "usable odds cover only {:.1}% of LIVE events",
                live_odds_coverage_percent
            ))
        } else if prematch_event_count > 0 && prematch_odds_coverage_percent < 5.0 {
            Some(format!(
                "usable odds cover only {:.1}% of PREMATCH events",
                prematch_odds_coverage_percent
            ))
        } else if duplicate_odd_ids > 0 {
            Some(format!("{} duplicate odd IDs detected", duplicate_odd_ids))
        } else if odds_coverage_percent < MIN_OVERALL_ODDS_COVERAGE_PERCENT {
            Some(format!(
                "usable odds cover only {:.1}% of all events; minimum is {:.1}%",
                odds_coverage_percent, MIN_OVERALL_ODDS_COVERAGE_PERCENT
            ))
        } else if live_event_count > 0
            && live_odds_coverage_percent < MIN_FEED_ODDS_COVERAGE_PERCENT
        {
            Some(format!(
                "usable odds cover only {:.1}% of LIVE events; minimum is {:.1}%",
                live_odds_coverage_percent, MIN_FEED_ODDS_COVERAGE_PERCENT
            ))
        } else if prematch_event_count > 0
            && prematch_odds_coverage_percent < MIN_FEED_ODDS_COVERAGE_PERCENT
        {
            Some(format!(
                "usable odds cover only {:.1}% of PREMATCH events; minimum is {:.1}%",
                prematch_odds_coverage_percent, MIN_FEED_ODDS_COVERAGE_PERCENT
            ))
        } else if invalid_ratio > MAX_INVALID_ODDS_RATIO {
            Some(format!(
                "invalid odds exceed {:.1}% of non-stale unique odds ({:.1}%)",
                MAX_INVALID_ODDS_RATIO * 100.0,
                invalid_ratio * 100.0
            ))
        } else if stale_ratio > MAX_STALE_ODDS_RATIO {
            Some(format!(
                "stale odds exceed {:.1}% of returned odds ({:.1}%)",
                MAX_STALE_ODDS_RATIO * 100.0,
                stale_ratio * 100.0
            ))
        } else {
            None
        };
        let quality_passed = quality_issue.is_none();
        let live_threshold_met = live_kpi.passed;
        let prematch_threshold_met = prematch_kpi.passed;
        let passed = live_threshold_met && prematch_threshold_met && quality_passed;

        Self {
            bookmaker_slug: bookmaker_slug.to_string(),
            total_events: events.len(),
            live_events,
            prematch_events,
            total_odds: odds.len(),
            valid_odds,
            market_counts,
            events_with_odds: events_with_odds.len(),
            events_without_odds_samples,
            duplicate_event_ids,
            duplicate_odd_ids,
            duplicate_odd_id_samples,
            invalid_odds,
            odds_coverage_percent,
            live_events_with_odds: live_events_with_odds.len(),
            prematch_events_with_odds: prematch_events_with_odds.len(),
            live_odds_coverage_percent,
            prematch_odds_coverage_percent,
            stale_odds,
            quote_timestamp_basis: "parser_ingestion_timestamp_only",
            bookmaker_quote_freshness_verified: false,
            quality_passed,
            quality_issue,
            live_kpi,
            prematch_kpi,
            live_threshold_met,
            prematch_threshold_met,
            passed,
            runtime_only,
            duration_ms,
            error: None,
        }
    }

    fn from_error(bookmaker_slug: &str, error: String, duration_ms: u128) -> Self {
        let (live_threshold, prematch_threshold) = thresholds_for_bookmaker(bookmaker_slug);
        Self {
            bookmaker_slug: bookmaker_slug.to_string(),
            total_events: 0,
            live_events: 0,
            prematch_events: 0,
            total_odds: 0,
            valid_odds: 0,
            market_counts: BTreeMap::new(),
            events_with_odds: 0,
            events_without_odds_samples: Vec::new(),
            duplicate_event_ids: 0,
            duplicate_odd_ids: 0,
            duplicate_odd_id_samples: Vec::new(),
            invalid_odds: 0,
            odds_coverage_percent: 0.0,
            live_events_with_odds: 0,
            prematch_events_with_odds: 0,
            live_odds_coverage_percent: 0.0,
            prematch_odds_coverage_percent: 0.0,
            stale_odds: 0,
            quote_timestamp_basis: "parser_ingestion_timestamp_only",
            bookmaker_quote_freshness_verified: false,
            quality_passed: false,
            quality_issue: Some(error.clone()),
            live_kpi: RuntimeKpiCheck::new(0, live_threshold),
            prematch_kpi: RuntimeKpiCheck::new(0, prematch_threshold),
            live_threshold_met: false,
            prematch_threshold_met: false,
            passed: false,
            runtime_only: true,
            duration_ms,
            error: Some(error),
        }
    }
}

pub async fn run_runtime_diagnostics(
    client: Arc<reqwest::Client>,
    slugs: &[String],
) -> RuntimeDiagnosticsRun {
    let requested_raw: Vec<String> = if slugs.is_empty() {
        DEFAULT_RUNTIME_DIAGNOSTIC_SLUGS
            .iter()
            .map(|slug| (*slug).to_string())
            .collect()
    } else {
        slugs.iter().map(|slug| slug.to_lowercase()).collect()
    };

    let mut seen = std::collections::HashSet::new();
    let requested = requested_raw
        .into_iter()
        .map(|slug| canonical_runtime_slug(&slug))
        .filter(|slug| seen.insert(slug.clone()))
        .collect::<Vec<_>>();

    use futures::StreamExt;

    // Keep diagnostics parallel but bounded so one slow source cannot serialize
    // the whole run and the probe does not overwhelm bookmaker endpoints.
    let mut reports = futures::stream::iter(requested.iter().cloned().map(|slug| {
        let client = client.clone();
        async move { run_single_runtime_diagnostic(client, &slug).await }
    }))
    .buffer_unordered(3)
    .collect::<Vec<_>>()
    .await;
    reports.sort_by(|left, right| left.bookmaker_slug.cmp(&right.bookmaker_slug));

    let passed_reports = reports.iter().filter(|report| report.passed).count();
    let total_reports = reports.len();
    let failed_reports = total_reports.saturating_sub(passed_reports);
    let failing_slugs = reports
        .iter()
        .filter(|report| !report.passed)
        .map(|report| report.bookmaker_slug.clone())
        .collect::<Vec<_>>();
    let aggregate = RuntimeDiagnosticsAggregate {
        total_events: reports.iter().map(|report| report.total_events).sum(),
        live_events: reports.iter().map(|report| report.live_events).sum(),
        prematch_events: reports.iter().map(|report| report.prematch_events).sum(),
        live_threshold_passes: reports
            .iter()
            .filter(|report| report.live_threshold_met)
            .count(),
        prematch_threshold_passes: reports
            .iter()
            .filter(|report| report.prematch_threshold_met)
            .count(),
        passed_reports,
        failed_reports,
    };

    RuntimeDiagnosticsRun {
        generated_at: Utc::now(),
        summary: RuntimeDiagnosticsSummary {
            requested_slugs: requested,
            total_reports,
            passed_reports,
            failed_reports,
            live_threshold: LIVE_THRESHOLD,
            prematch_threshold: PREMATCH_THRESHOLD,
        },
        aggregate,
        nightly: RuntimeDiagnosticsNightlySummary {
            status: if failed_reports == 0 { "pass" } else { "fail" },
            exit_code: if failed_reports == 0 { 0 } else { 2 },
            failing_slugs,
            live_gap_total: reports.iter().map(|report| report.live_kpi.missing).sum(),
            prematch_gap_total: reports
                .iter()
                .map(|report| report.prematch_kpi.missing)
                .sum(),
        },
        reports,
    }
}

async fn run_single_runtime_diagnostic(
    client: Arc<reqwest::Client>,
    slug: &str,
) -> RuntimeCountReport {
    let started = std::time::Instant::now();

    let (runtime_only, result): (bool, RuntimeFetchResult) = match slug {
        "winline" => (true, with_timeout(crate::winline_feed::fetch()).await),
        "melbet" => (
            true,
            with_timeout(MelbetParser::new(client).fetch_runtime_data()).await,
        ),
        "zenit" => (
            true,
            with_timeout(ZenitParser::new(client).fetch_runtime_data()).await,
        ),
        "betboom" => (
            true,
            with_timeout(BetboomParser::new(client).fetch_runtime_data()).await,
        ),
        "baltbet" => {
            let factory = ParserFactory::new(client.clone());
            if let Some(parser) = factory.get(slug) {
                let result = with_timeout(async move {
                    let result = parser.fetch_all().await?;
                    Ok::<(Vec<Event>, Vec<shared::Odd>), Box<dyn std::error::Error + Send + Sync>>(
                        (result.events, result.odds),
                    )
                })
                .await;
                (false, result)
            } else {
                (
                    false,
                    Err("bookmaker baltbet not registered in parser factory".to_string()),
                )
            }
        }
        "betcity" => (
            true,
            with_timeout(BetcityParser::new(client).fetch_runtime_data()).await,
        ),
        "ligastavok" => (
            true,
            with_timeout(LigaStavokParser::new(client).fetch_runtime_data()).await,
        ),
        "tennisi" => (
            true,
            with_timeout(TennisiParser::new(client).fetch_runtime_data()).await,
        ),
        _ => {
            let factory = ParserFactory::new(client.clone());
            if let Some(parser) = factory.get(slug) {
                let result = with_timeout(async move {
                    let result = parser.fetch_all().await?;
                    Ok::<(Vec<Event>, Vec<shared::Odd>), Box<dyn std::error::Error + Send + Sync>>(
                        (result.events, result.odds),
                    )
                })
                .await;
                (false, result)
            } else {
                (
                    false,
                    Err(format!(
                        "bookmaker {} not registered in parser factory",
                        slug
                    )),
                )
            }
        }
    };

    let elapsed = started.elapsed().as_millis();
    match result {
        Ok((events, odds)) => {
            RuntimeCountReport::from_result(slug, &events, &odds, elapsed, runtime_only)
        }
        Err(error) => {
            let mut report = RuntimeCountReport::from_error(slug, error, elapsed);
            report.runtime_only = runtime_only;
            report
        }
    }
}

fn canonical_bookmaker_slug(slug: &str) -> String {
    match slug.to_ascii_lowercase().as_str() {
        "_24bet" | "24bet" | "bet24" => "bet24".to_string(),
        "olimpbet" | "olimp" => "olimp".to_string(),
        other => other.to_string(),
    }
}

fn canonical_runtime_slug(slug: &str) -> String {
    match slug {
        "_24bet" => "bet24".to_string(),
        "olimpbet" => "olimp".to_string(),
        other => other.to_string(),
    }
}

async fn with_timeout<F>(future: F) -> Result<(Vec<Event>, Vec<shared::Odd>), String>
where
    F: std::future::Future<
        Output = Result<(Vec<Event>, Vec<shared::Odd>), Box<dyn std::error::Error + Send + Sync>>,
    >,
{
    match tokio::time::timeout(FETCH_TIMEOUT, future).await {
        Ok(Ok(result)) => Ok(result),
        Ok(Err(error)) => Err(error.to_string()),
        Err(_) => Err(format!("timeout after {}s", FETCH_TIMEOUT.as_secs())),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        RuntimeCountReport, RuntimeDiagnosticsAggregate, RuntimeDiagnosticsNightlySummary,
        RuntimeDiagnosticsRun, RuntimeDiagnosticsSummary,
    };
    use chrono::Utc;
    use shared::{Event, Sport};
    use std::collections::HashMap;

    #[test]
    fn splits_live_and_prematch_counts() {
        let events = vec![
            Event {
                id: "1".into(),
                sport: Sport::Football,
                league: "L1".into(),
                home_team: "A".into(),
                away_team: "B".into(),
                start_time: None,
                is_live: true,
                bookmaker_slug: "test".into(),
                raw_url: None,
                extra: HashMap::new(),
            },
            Event {
                id: "2".into(),
                sport: Sport::Football,
                league: "L1".into(),
                home_team: "C".into(),
                away_team: "D".into(),
                start_time: None,
                is_live: false,
                bookmaker_slug: "test".into(),
                raw_url: None,
                extra: HashMap::new(),
            },
        ];

        let report = RuntimeCountReport::from_result("test", &events, &[], 42, true);
        assert_eq!(report.total_events, 2);
        assert_eq!(report.live_events, 1);
        assert_eq!(report.prematch_events, 1);
        assert_eq!(report.live_kpi.target, 150);
        assert_eq!(report.live_kpi.actual, 0);
        assert_eq!(report.live_kpi.missing, 150);
        assert_eq!(report.prematch_kpi.target, 3000);
        assert_eq!(report.prematch_kpi.actual, 0);
        assert_eq!(report.prematch_kpi.missing, 3000);
        assert!(!report.passed);
    }

    #[test]
    fn volume_kpi_counts_only_events_with_valid_mapped_odds() {
        use chrono::Utc;
        use shared::{Event, Odd, OddsType, Sport};
        use std::collections::HashMap;

        let make_event = |id: &str| Event {
            id: id.into(),
            sport: Sport::Football,
            league: "L1".into(),
            home_team: format!("Home {id}"),
            away_team: format!("Away {id}"),
            start_time: None,
            is_live: true,
            bookmaker_slug: "fixture".into(),
            raw_url: None,
            extra: HashMap::new(),
        };
        let events = vec![make_event("priced"), make_event("scoreboard-only")];
        let odds = vec![Odd {
            id: "odd-priced".into(),
            event_id: "priced".into(),
            bookmaker_slug: "fixture".into(),
            market: "1X2".into(),
            selection: "1".into(),
            odds: 2.1,
            odds_type: OddsType::Home,
            line: None,
            timestamp: Utc::now(),
        }];

        let report = RuntimeCountReport::from_result("fixture", &events, &odds, 1, true);
        assert_eq!(report.live_events, 2);
        assert_eq!(report.live_events_with_odds, 1);
        assert_eq!(report.live_kpi.actual, 1);
        assert_eq!(report.live_kpi.target, 150);
        assert_eq!(report.live_kpi.missing, 149);
    }

    #[test]
    fn default_runtime_diagnostics_cover_every_active_russian_bookmaker() {
        use crate::parser_factory::ACTIVE_RUSSIAN_BOOKMAKER_SLUGS;
        assert_eq!(
            super::DEFAULT_RUNTIME_DIAGNOSTIC_SLUGS,
            ACTIVE_RUSSIAN_BOOKMAKER_SLUGS
        );
        assert_eq!(super::DEFAULT_RUNTIME_DIAGNOSTIC_SLUGS.len(), 16);
        let mut unique = super::DEFAULT_RUNTIME_DIAGNOSTIC_SLUGS.to_vec();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), super::DEFAULT_RUNTIME_DIAGNOSTIC_SLUGS.len());
    }

    #[test]
    fn russian_bookmaker_volume_gates_match_realistic_feed_sizes() {
        assert_eq!(super::thresholds_for_bookmaker("winline"), (100, 2_500));
        assert_eq!(super::thresholds_for_bookmaker("sportbet"), (25, 1_000));
        assert_eq!(super::thresholds_for_bookmaker("leon"), (100, 2_800));
        assert_eq!(super::thresholds_for_bookmaker("olimp"), (100, 1_450));
        assert_eq!(super::thresholds_for_bookmaker("betcity"), (140, 3_000));
        assert_eq!(super::thresholds_for_bookmaker("zenit"), (125, 3_000));
        assert_eq!(super::thresholds_for_bookmaker("fonbet"), (150, 3_000));
    }

    #[test]
    fn runtime_report_requires_fresh_mapped_odds_for_quality_pass() {
        use chrono::Utc;
        use shared::{Event, Odd, OddsType, Sport};
        use std::collections::HashMap;

        let event = Event {
            id: "event-1".into(),
            sport: Sport::Football,
            league: "L1".into(),
            home_team: "A".into(),
            away_team: "B".into(),
            start_time: None,
            is_live: false,
            bookmaker_slug: "fixture".into(),
            raw_url: None,
            extra: HashMap::new(),
        };
        let odd = Odd {
            id: "odd-1".into(),
            event_id: "event-1".into(),
            bookmaker_slug: "fixture".into(),
            market: "1X2".into(),
            selection: "1".into(),
            odds: 2.1,
            odds_type: OddsType::Home,
            line: None,
            timestamp: Utc::now(),
        };

        let report = RuntimeCountReport::from_result("fixture", &[event], &[odd], 1, true);
        assert!(report.quality_passed);
        assert_eq!(report.valid_odds, 1);
        assert_eq!(report.events_with_odds, 1);
        assert_eq!(report.odds_coverage_percent, 100.0);
        assert_eq!(report.market_counts.get("1X2"), Some(&1));
        assert!(
            !report.passed,
            "volume KPIs must still be enforced separately"
        );
    }

    #[test]
    fn runtime_report_rejects_one_sided_odds_coverage() {
        use chrono::Utc;
        use shared::{Event, Odd, OddsType, Sport};
        use std::collections::HashMap;

        let make_event = |id: &str, is_live: bool| Event {
            id: id.into(),
            sport: Sport::Football,
            league: "L1".into(),
            home_team: format!("Home {id}"),
            away_team: format!("Away {id}"),
            start_time: None,
            is_live,
            bookmaker_slug: "fixture".into(),
            raw_url: None,
            extra: HashMap::new(),
        };
        let live = make_event("live-1", true);
        let prematch = make_event("pre-1", false);
        let odd = Odd {
            id: "odd-live".into(),
            event_id: "live-1".into(),
            bookmaker_slug: "fixture".into(),
            market: "1X2".into(),
            selection: "1".into(),
            odds: 2.1,
            odds_type: OddsType::Home,
            line: None,
            timestamp: Utc::now(),
        };

        let report = RuntimeCountReport::from_result("fixture", &[live, prematch], &[odd], 1, true);
        assert_eq!(report.live_odds_coverage_percent, 100.0);
        assert_eq!(report.prematch_odds_coverage_percent, 0.0);
        assert!(!report.quality_passed);
        assert!(report
            .quality_issue
            .as_deref()
            .unwrap()
            .contains("PREMATCH"));
    }

    #[test]
    fn runtime_report_rejects_duplicate_event_ids_across_live_and_prematch() {
        use shared::{Event, Sport};
        use std::collections::HashMap;

        let make_event = |is_live| Event {
            id: "same-source-id".into(),
            sport: Sport::Football,
            league: "L1".into(),
            home_team: "A".into(),
            away_team: "B".into(),
            start_time: None,
            is_live,
            bookmaker_slug: "fixture".into(),
            raw_url: None,
            extra: HashMap::new(),
        };
        let report = RuntimeCountReport::from_result(
            "fixture",
            &[make_event(true), make_event(false)],
            &[],
            1,
            true,
        );
        assert_eq!(report.duplicate_event_ids, 1);
        assert!(!report.quality_passed);
        assert!(report
            .quality_issue
            .as_deref()
            .unwrap()
            .contains("duplicate event IDs"));
    }

    #[test]
    fn runtime_report_accepts_legacy_24bet_slug_alias() {
        use chrono::Utc;
        use shared::{Event, Odd, OddsType, Sport};
        use std::collections::HashMap;

        let event = Event {
            id: "event-24".into(),
            sport: Sport::Football,
            league: "L1".into(),
            home_team: "A".into(),
            away_team: "B".into(),
            start_time: None,
            is_live: false,
            bookmaker_slug: "_24bet".into(),
            raw_url: None,
            extra: HashMap::new(),
        };
        let odd = Odd {
            id: "odd-24".into(),
            event_id: "event-24".into(),
            bookmaker_slug: "_24bet".into(),
            market: "1X2".into(),
            selection: "1".into(),
            odds: 2.2,
            odds_type: OddsType::Home,
            line: None,
            timestamp: Utc::now(),
        };

        let report = RuntimeCountReport::from_result("bet24", &[event], &[odd], 1, true);
        assert!(report.quality_passed);
        assert_eq!(report.valid_odds, 1);
        assert_eq!(report.odds_coverage_percent, 100.0);
    }

    #[test]
    fn runtime_report_rejects_low_overall_odds_coverage() {
        use chrono::Utc;
        use shared::{Event, Odd, OddsType, Sport};
        use std::collections::HashMap;

        let events = (0..20)
            .map(|index| Event {
                id: format!("event-{index}"),
                sport: Sport::Football,
                league: "L1".into(),
                home_team: format!("Home {index}"),
                away_team: format!("Away {index}"),
                start_time: None,
                is_live: false,
                bookmaker_slug: "fixture".into(),
                raw_url: None,
                extra: HashMap::new(),
            })
            .collect::<Vec<_>>();
        let odds = (0..17)
            .map(|index| Odd {
                id: format!("odd-{index}"),
                event_id: format!("event-{index}"),
                bookmaker_slug: "fixture".into(),
                market: "1X2".into(),
                selection: "1".into(),
                odds: 2.1,
                odds_type: OddsType::Home,
                line: None,
                timestamp: Utc::now(),
            })
            .collect::<Vec<_>>();

        let report = RuntimeCountReport::from_result("fixture", &events, &odds, 1, true);
        assert_eq!(report.odds_coverage_percent, 85.0);
        assert!(!report.quality_passed);
        assert!(report
            .quality_issue
            .as_deref()
            .unwrap()
            .contains("minimum is 90.0%"));
    }

    #[test]
    fn runtime_report_rejects_low_live_feed_coverage_even_when_overall_is_high() {
        use chrono::Utc;
        use shared::{Event, Odd, OddsType, Sport};
        use std::collections::HashMap;

        let events = (0..100)
            .map(|index| Event {
                id: format!("event-{index}"),
                sport: Sport::Football,
                league: "L1".into(),
                home_team: format!("Home {index}"),
                away_team: format!("Away {index}"),
                start_time: None,
                is_live: index < 10,
                bookmaker_slug: "fixture".into(),
                raw_url: None,
                extra: HashMap::new(),
            })
            .collect::<Vec<_>>();
        // 97% overall coverage, but only 70% of live events are priced.
        let odds = (0..97)
            .filter(|index| *index >= 3)
            .map(|index| Odd {
                id: format!("odd-{index}"),
                event_id: format!("event-{index}"),
                bookmaker_slug: "fixture".into(),
                market: "1X2".into(),
                selection: "1".into(),
                odds: 2.1,
                odds_type: OddsType::Home,
                line: None,
                timestamp: Utc::now(),
            })
            .collect::<Vec<_>>();

        let report = RuntimeCountReport::from_result("fixture", &events, &odds, 1, true);
        assert_eq!(report.odds_coverage_percent, 94.0);
        assert_eq!(report.live_odds_coverage_percent, 70.0);
        assert!(!report.quality_passed);
        assert!(report
            .quality_issue
            .as_deref()
            .unwrap()
            .contains("LIVE events; minimum is 80.0%"));
    }

    #[test]
    fn runtime_report_rejects_duplicate_odd_ids() {
        use chrono::Utc;
        use shared::{Event, Odd, OddsType, Sport};
        use std::collections::HashMap;

        let event = Event {
            id: "event-1".into(),
            sport: Sport::Football,
            league: "L1".into(),
            home_team: "A".into(),
            away_team: "B".into(),
            start_time: None,
            is_live: false,
            bookmaker_slug: "fixture".into(),
            raw_url: None,
            extra: HashMap::new(),
        };
        let make_odd = |selection: &str| Odd {
            id: "duplicate-odd".into(),
            event_id: "event-1".into(),
            bookmaker_slug: "fixture".into(),
            market: "1X2".into(),
            selection: selection.into(),
            odds: 2.1,
            odds_type: OddsType::Home,
            line: None,
            timestamp: Utc::now(),
        };

        let report = RuntimeCountReport::from_result(
            "fixture",
            &[event],
            &[make_odd("1"), make_odd("X")],
            1,
            true,
        );
        assert_eq!(report.duplicate_odd_ids, 1);
        assert_eq!(report.duplicate_odd_id_samples.len(), 1);
        assert_eq!(report.duplicate_odd_id_samples[0].first_selection, "1");
        assert_eq!(report.duplicate_odd_id_samples[0].duplicate_selection, "X");
        assert!(!report.quality_passed);
        assert!(report
            .quality_issue
            .as_deref()
            .unwrap()
            .contains("duplicate odd IDs"));
    }

    #[test]
    fn runtime_report_rejects_odds_without_market_or_selection() {
        use chrono::Utc;
        use shared::{Event, Odd, OddsType, Sport};
        use std::collections::HashMap;

        let event = Event {
            id: "event-1".into(),
            sport: Sport::Football,
            league: "L1".into(),
            home_team: "A".into(),
            away_team: "B".into(),
            start_time: None,
            is_live: false,
            bookmaker_slug: "fixture".into(),
            raw_url: None,
            extra: HashMap::new(),
        };
        let odds = vec![
            Odd {
                id: "odd-valid".into(),
                event_id: "event-1".into(),
                bookmaker_slug: "fixture".into(),
                market: "1X2".into(),
                selection: "1".into(),
                odds: 2.1,
                odds_type: OddsType::Home,
                line: None,
                timestamp: Utc::now(),
            },
            Odd {
                id: "odd-invalid".into(),
                event_id: "event-1".into(),
                bookmaker_slug: "fixture".into(),
                market: " ".into(),
                selection: "".into(),
                odds: 2.2,
                odds_type: OddsType::Away,
                line: None,
                timestamp: Utc::now(),
            },
        ];

        let report = RuntimeCountReport::from_result("fixture", &[event], &odds, 1, true);
        assert_eq!(report.invalid_odds, 1);
        assert!(!report.quality_passed);
        assert!(report
            .quality_issue
            .as_deref()
            .unwrap()
            .contains("invalid odds exceed"));
    }

    #[test]
    fn runtime_report_rejects_excessive_stale_odds() {
        use chrono::{Duration, Utc};
        use shared::{Event, Odd, OddsType, Sport};
        use std::collections::HashMap;

        let event = Event {
            id: "event-1".into(),
            sport: Sport::Football,
            league: "L1".into(),
            home_team: "A".into(),
            away_team: "B".into(),
            start_time: None,
            is_live: false,
            bookmaker_slug: "fixture".into(),
            raw_url: None,
            extra: HashMap::new(),
        };
        let odds = (0..10)
            .map(|index| Odd {
                id: format!("odd-{index}"),
                event_id: "event-1".into(),
                bookmaker_slug: "fixture".into(),
                market: "1X2".into(),
                selection: index.to_string(),
                odds: 2.1,
                odds_type: OddsType::Home,
                line: None,
                timestamp: if index == 0 {
                    Utc::now() - Duration::minutes(10)
                } else {
                    Utc::now()
                },
            })
            .collect::<Vec<_>>();

        let report = RuntimeCountReport::from_result("fixture", &[event], &odds, 1, true);
        assert_eq!(report.stale_odds, 1);
        assert!(!report.quality_passed);
        assert!(report
            .quality_issue
            .as_deref()
            .unwrap()
            .contains("stale odds exceed"));
    }

    #[test]
    fn runtime_run_serializes_threshold_metadata() {
        let run = RuntimeDiagnosticsRun {
            generated_at: Utc::now(),
            summary: RuntimeDiagnosticsSummary {
                requested_slugs: vec!["test".into()],
                total_reports: 1,
                passed_reports: 0,
                failed_reports: 1,
                live_threshold: 150,
                prematch_threshold: 3000,
            },
            aggregate: RuntimeDiagnosticsAggregate {
                total_events: 0,
                live_events: 0,
                prematch_events: 0,
                live_threshold_passes: 0,
                prematch_threshold_passes: 0,
                passed_reports: 0,
                failed_reports: 1,
            },
            nightly: RuntimeDiagnosticsNightlySummary {
                status: "fail",
                exit_code: 2,
                failing_slugs: vec!["test".into()],
                live_gap_total: 150,
                prematch_gap_total: 3000,
            },
            reports: vec![RuntimeCountReport::from_error("test", "boom".into(), 42)],
        };

        let json = serde_json::to_value(&run).expect("serialize run");
        assert_eq!(json["summary"]["live_threshold"], 150);
        assert_eq!(json["summary"]["prematch_threshold"], 3000);
        assert_eq!(json["summary"]["requested_slugs"][0], "test");
        assert_eq!(json["aggregate"]["failed_reports"], 1);
        assert_eq!(json["nightly"]["status"], "fail");
        assert_eq!(json["reports"][0]["live_kpi"]["target"], 150);
        assert_eq!(json["reports"][0]["bookmaker_slug"], "test");
    }
}
