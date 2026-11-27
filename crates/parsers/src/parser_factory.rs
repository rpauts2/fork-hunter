use crate::base::BookmakerParser;
use crate::{
    baltbet, bet24, betboom, betcity, betm, bettery, fonbet, leon, ligastavok, marathon, melbet,
    olimp, pari, sportbet, tennisi, winline, zenit,
};
use shared::{BookmakerMetadata, HealthStatus, ParserCoverage, ParserHealth};
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Clone, Copy)]
struct BookmakerRegistryEntry {
    slug: &'static str,
    name: &'static str,
    aliases: &'static [&'static str],
    parser_type: &'static str,
    source: &'static str,
    execution_supported: bool,
    notes: Option<&'static str>,
}

/// Russian bookmaker operators currently targeted by the scanner.
/// All operators present in the latest FNS official bookmaker-site list.
pub const RUSSIAN_BOOKMAKER_TARGET_SLUGS: &[&str] = &[
    "sportbet",
    "marathon",
    "baltbet",
    "betboom",
    "olimp",
    "zenit",
    "leon",
    "ligastavok",
    "tennisi",
    "winline",
    "betcity",
    "melbet",
    "poolbet",
    "bettery",
    "fonbet",
    "pari",
    "goldenbet",
    "betm",
    "bet24",
    "abet",
];

/// Active Russian operator targets from the current 1Cupis partner/rates list (16 of 20 FNS-listed sites).
/// Tennisi is excluded because 1Cupis explicitly marks it as paused; the other three
/// non-active FNS entries have no confirmed public interactive line.
pub const ACTIVE_RUSSIAN_BOOKMAKER_SLUGS: &[&str] = &[
    "sportbet",
    "marathon",
    "baltbet",
    "betboom",
    "olimp",
    "zenit",
    "leon",
    "ligastavok",
    "winline",
    "betcity",
    "melbet",
    "bettery",
    "fonbet",
    "pari",
    "betm",
    "bet24",
];

/// FNS-listed operators whose online betting is currently paused, dormant, or has
/// no confirmed interactive betting line.
pub const INACTIVE_RUSSIAN_BOOKMAKER_SLUGS: &[&str] = &["tennisi", "poolbet", "goldenbet", "abet"];

/// Russian bookmakers with a Rust parser implementation registered in the factory.
/// Three FNS-listed operators currently have no verified public feed integration.
pub const IMPLEMENTED_RU_BOOKMAKER_SLUGS: &[&str] = &[
    "fonbet",
    "winline",
    "betboom",
    "pari",
    "ligastavok",
    "olimp",
    "betcity",
    "melbet",
    "leon",
    "marathon",
    "baltbet",
    "bettery",
    "sportbet",
    "zenit",
    "betm",
    "bet24",
    "tennisi",
];

const BOOKMAKER_REGISTRY: &[BookmakerRegistryEntry] = &[
    BookmakerRegistryEntry {
        slug: "abet",
        name: "ABET",
        aliases: &["abet"],
        parser_type: "unavailable",
        source: "not_implemented",
        execution_supported: false,
        notes: Some("FNS-listed operator (official domain abet.ru), branded as Alfabet by ООО Панорама. The official page publishes company/licence details and lists sports, but no public event/odds feed or machine-readable betting line is documented. Keep disabled until an authorized feed is verified."),
    },
    BookmakerRegistryEntry {
        slug: "goldenbet",
        name: "Golden Bet",
        aliases: &["goldenbet", "golden-bet"],
        parser_type: "unavailable",
        source: "not_implemented",
        execution_supported: false,
        notes: Some("FNS-listed operator (official domain golden-bet.bet). Current public research did not verify an operating interactive line or a documented odds API; the official domain returned HTTP 403 from the development environment. Keep disabled pending operator confirmation."),
    },
    BookmakerRegistryEntry {
        slug: "poolbet",
        name: "Poolbet",
        aliases: &["poolbet"],
        parser_type: "unavailable",
        source: "not_implemented",
        execution_supported: false,
        notes: Some("FNS-listed operator (official domain poolbet.ru). As of 2026-11-27 the planned launch has been postponed to 2026-12-01; no operating public odds feed is verified yet. Keep disabled and recheck after the announced launch date."),
    },
    BookmakerRegistryEntry {
        slug: "betboom",
        name: "BetBoom",
        aliases: &["betboom"],
        parser_type: "browser_interception",
        source: "crates/parsers/src/betboom.rs",
        execution_supported: false,
        notes: Some("Direct site is blocked by QRATOR in this environment. The parser remains a direct-bookmaker implementation only; verify the official application contract through a permitted session or operator-authorized access. Do not substitute third-party feeds or bypass the access challenge."),
    },
    BookmakerRegistryEntry {
        slug: "pari",
        name: "Pari",
        aliases: &["pari"],
        parser_type: "api",
        source: "crates/parsers/src/pari.rs",
        execution_supported: false,
        notes: Some("Rust parser registered for market scanning."),
    },
    BookmakerRegistryEntry {
        slug: "fonbet",
        name: "Fonbet",
        aliases: &["fonbet"],
        parser_type: "api",
        source: "crates/parsers/src/fonbet.rs",
        execution_supported: false,
        notes: Some("Rust parser registered for market scanning."),
    },
    BookmakerRegistryEntry {
        slug: "betm",
        name: "Bet-M",
        aliases: &["betm"],
        parser_type: "headless",
        source: "crates/parsers/src/betm.rs",
        execution_supported: false,
        notes: Some(
            "Rust headless parser probes legacy and current Bet-M public routes, but production scanning stays disabled until public feed coverage is verified.",
        ),
    },
    BookmakerRegistryEntry {
        slug: "bettery",
        name: "Bettery",
        aliases: &["bettery"],
        parser_type: "api",
        source: "crates/parsers/src/bettery.rs",
        execution_supported: false,
        notes: Some("Rust parser registered for market scanning."),
    },
    BookmakerRegistryEntry {
        slug: "marathon",
        name: "Marathon",
        aliases: &["marathon"],
        parser_type: "api",
        source: "crates/parsers/src/marathon.rs",
        execution_supported: false,
        notes: Some("Rust parser registered for market scanning."),
    },
    BookmakerRegistryEntry {
        slug: "mbet",
        name: "мБет",
        aliases: &["mbet"],
        parser_type: "api",
        source: "crates/parsers/src/mbet.rs",
        execution_supported: false,
        notes: Some(
            "мБет API parser with HTML fallback, proxy rotation, and comprehensive market support (1X2, Total, Corners, Cards). Target: 4000+ events.",
        ),
    },
    BookmakerRegistryEntry {
        slug: "bet24",
        name: "24bet",
        aliases: &["_24bet", "bet24"],
        parser_type: "api",
        source: "crates/parsers/src/bet24.rs",
        execution_supported: false,
        notes: Some("Rust parser registered for market scanning."),
    },
    BookmakerRegistryEntry {
        slug: "leon",
        name: "Leon",
        aliases: &["leon"],
        parser_type: "api",
        source: "crates/parsers/src/leon.rs",
        execution_supported: false,
        notes: Some("Rust parser registered for market scanning."),
    },
    BookmakerRegistryEntry {
        slug: "melbet",
        name: "Melbet",
        aliases: &["melbet"],
        parser_type: "browser_interception",
        source: "crates/parsers/src/melbet.rs",
        execution_supported: false,
        notes: Some(
            "Direct site is blocked by a regional HTTP 403 in this environment. Keep this as a direct-bookmaker parser; verify the official RU operator endpoint from a permitted environment or through operator-authorized access. Do not substitute third-party feeds or evade regional restrictions.",
        ),
    },
    BookmakerRegistryEntry {
        slug: "sportbet",
        name: "Sportbet",
        aliases: &["sportbet"],
        parser_type: "api",
        source: "crates/parsers/src/sportbet.rs",
        execution_supported: false,
        notes: Some("Rust parser registered for market scanning."),
    },
    BookmakerRegistryEntry {
        slug: "tennisi",
        name: "Tennisi",
        aliases: &["tennisi"],
        parser_type: "api",
        source: "crates/parsers/src/tennisi.rs",
        execution_supported: false,
        notes: Some(
            "Operator suspended acceptance of bets from 2026-09-01; public reporting says the site/app were left for withdrawals only through 2026-09-30. ODDSCORP still lists Tennisi in its bookmaker catalogue, but that legacy listing does not prove a live feed. Keep disabled until the operator resumes betting and a provider confirms current quotes.",
        ),
    },
    BookmakerRegistryEntry {
        slug: "tennis",
        name: "Tennis (ATP/WTA)",
        aliases: &["tennis"],
        parser_type: "external_feed",
        source: "crates/parsers/src/tennis.rs",
        execution_supported: false,
        notes: Some(
            "Experimental ESPN/Flashscore tennis data feed, not a bookmaker. Excluded from arbitrage ingestion until odds provenance and bookmaker attribution are independently verified.",
        ),
    },
    BookmakerRegistryEntry {
        slug: "winline",
        name: "Winline",
        aliases: &["winline"],
        parser_type: "websocket",
        source: "crates/parsers/src/winline_feed.rs",
        execution_supported: false,
        notes: Some("Direct public binary WebSocket feed; runtime quality and volume gates remain authoritative."),
    },
    BookmakerRegistryEntry {
        slug: "zenit",
        name: "Zenit",
        aliases: &["zenit"],
        parser_type: "api",
        source: "crates/parsers/src/zenit.rs",
        execution_supported: false,
        notes: Some(
            "Pure HTTP parser with imprinthash authentication is registered for scan coverage and runtime diagnostics; readiness now keeps Zenit below production because recent strict nightly runs regressed to zero events after an earlier runtime pass.",
        ),
    },
    BookmakerRegistryEntry {
        slug: "betcity",
        name: "Betcity",
        aliases: &["betcity"],
        parser_type: "api",
        source: "crates/parsers/src/betcity.rs",
        execution_supported: false,
        notes: Some(
            "HTTP parser is registered for scan coverage and runtime diagnostics; the latest synchronized reachable-only sweep and repeated direct-transport retries passed. Production enablement remains off pending the broader release gate.",
        ),
    },
    BookmakerRegistryEntry {
        slug: "baltbet",
        name: "Baltbet",
        aliases: &["baltbet"],
        parser_type: "api",
        source: "crates/parsers/src/baltbet.rs",
        execution_supported: false,
        notes: Some(
            "Pure HTTP parser with live JSON, banner metadata fallback, and legacy prematch groups; strict nightly KPI progress is tracked in readiness diagnostics.",
        ),
    },
    BookmakerRegistryEntry {
        slug: "ligastavok",
        name: "Liga Stavok",
        aliases: &["ligastavok"],
        parser_type: "api",
        source: "crates/parsers/src/ligastavok.rs",
        execution_supported: false,
        notes: Some(
            "Direct site is blocked by QRATOR in this environment. Keep this as a direct-bookmaker parser; verify the official application contract through a permitted session or operator-authorized access. Do not substitute third-party feeds or bypass the access challenge.",
        ),
    },
    BookmakerRegistryEntry {
        slug: "olimp",
        name: "Olimp",
        aliases: &["olimp"],
        parser_type: "api",
        source: "crates/parsers/src/olimp.rs",
        execution_supported: false,
        notes: Some(
            "HTTP parser is re-enabled through the direct Olimp competitions-with-events API path; readiness now locks one bounded 2026-11-26 runtime probe showing non-empty live and prematch event volume while production promotion remains gated.",
        ),
    },
];

pub struct ParserFactory {
    parsers: HashMap<String, Arc<dyn BookmakerParser + Send + Sync>>,
}

impl ParserFactory {
    const STATIC_HEALTH_NOTE: &'static str =
        "Static factory snapshot only; runtime fetch has not been executed yet.";

    fn registry_entry(slug: &str) -> Option<&'static BookmakerRegistryEntry> {
        BOOKMAKER_REGISTRY.iter().find(|entry| entry.slug == slug)
    }

    fn snapshot_health_status(_coverage: &ParserCoverage) -> HealthStatus {
        // A static factory snapshot is not a failed runtime probe.
        HealthStatus::Degraded
    }

    fn snapshot_health_error(coverage: &ParserCoverage) -> Option<String> {
        let mut notes = vec![Self::STATIC_HEALTH_NOTE.to_string()];
        if let Some(note) = coverage.notes.as_ref() {
            notes.push(note.clone());
        }
        Some(notes.join(" "))
    }

    pub fn new(client: Arc<reqwest::Client>) -> Self {
        let mut parsers: HashMap<String, Arc<dyn BookmakerParser + Send + Sync>> = HashMap::new();

        parsers.insert(
            "betboom".to_string(),
            Arc::new(betboom::BetboomParser::new(client.clone())),
        );
        parsers.insert(
            "winline".to_string(),
            Arc::new(winline::WinlineParser::new(client.clone())),
        );

        // HTTP API парсеры
        parsers.insert(
            "pari".to_string(),
            Arc::new(pari::PariParser::new(client.clone())),
        );
        parsers.insert(
            "marathon".to_string(),
            Arc::new(marathon::MarathonParser::new(client.clone())),
        );
        parsers.insert(
            "betm".to_string(),
            Arc::new(betm::BetMParser::new(client.clone())),
        );
        parsers.insert(
            "bettery".to_string(),
            Arc::new(bettery::BetteryParser::new(client.clone())),
        );
        parsers.insert(
            "fonbet".to_string(),
            Arc::new(fonbet::FonbetParser::new(client.clone())),
        );
        parsers.insert(
            "leon".to_string(),
            Arc::new(leon::LeonParser::new(client.clone())),
        );
        parsers.insert(
            "melbet".to_string(),
            Arc::new(melbet::MelbetParser::new(client.clone())),
        );
        parsers.insert(
            "sportbet".to_string(),
            Arc::new(sportbet::SportbetParser::new(client.clone())),
        );
        parsers.insert(
            "tennisi".to_string(),
            Arc::new(tennisi::TennisiParser::new(client.clone())),
        );
        // Active HTTP parsers — Zenit, Betcity, Baltbet and core bookmaker set.
        parsers.insert(
            "zenit".to_string(),
            Arc::new(zenit::ZenitParser::new(client.clone())),
        );
        parsers.insert(
            "betcity".to_string(),
            Arc::new(betcity::BetcityParser::new(client.clone())),
        );
        parsers.insert(
            "baltbet".to_string(),
            Arc::new(baltbet::BaltbetParser::new(client.clone())),
        );
        parsers.insert(
            "ligastavok".to_string(),
            Arc::new(ligastavok::LigaStavokParser::new(client.clone())),
        );

        // 24bet parser
        let bet24_parser: Arc<dyn BookmakerParser + Send + Sync> =
            Arc::new(bet24::_24betParser::new(client.clone()));
        parsers.insert("_24bet".to_string(), bet24_parser.clone());
        parsers.insert("bet24".to_string(), bet24_parser);

        parsers.insert(
            "olimp".to_string(),
            Arc::new(olimp::OlimpParser::new(client.clone())),
        );

        // TODO: New parsers (Tennis, мБет) - need schema fixes
        // These are partially implemented but require updates to match Odd/Event struct definitions
        // parsers.insert(
        //     "tennis".to_string(),
        //     Arc::new(tennis::TennisParser::new(client.clone())),
        // );
        // parsers.insert(
        //     "mbet".to_string(),
        //     Arc::new(mbet::MbetParser::new(client.clone())),
        // );

        ParserFactory { parsers }
    }

    pub fn get(&self, slug: &str) -> Option<Arc<dyn BookmakerParser + Send + Sync>> {
        self.parsers.get(slug).cloned()
    }

    pub fn get_all(&self) -> Vec<Arc<dyn BookmakerParser + Send + Sync>> {
        self.parsers.values().cloned().collect()
    }

    pub fn get_enabled(&self) -> Vec<Arc<dyn BookmakerParser + Send + Sync>> {
        self.parsers
            .values()
            .filter(|p| p.is_enabled())
            .cloned()
            .collect()
    }

    pub fn contains(&self, slug: &str) -> bool {
        self.parsers.contains_key(slug)
    }

    pub fn registered_slugs(&self) -> Vec<String> {
        let mut slugs: Vec<String> = self.parsers.keys().cloned().collect();
        slugs.sort();
        slugs
    }

    pub fn bookmaker_metadata(&self) -> Vec<BookmakerMetadata> {
        let mut items = Vec::new();

        for entry in BOOKMAKER_REGISTRY {
            let parser = entry
                .aliases
                .iter()
                .find_map(|alias| self.parsers.get(*alias));

            let (name, enabled, scan_supported) = if let Some(parser) = parser {
                (
                    parser.name().to_string(),
                    parser.is_enabled(),
                    parser.is_enabled(),
                )
            } else {
                (entry.name.to_string(), false, false)
            };

            items.push(BookmakerMetadata::new(
                entry.slug,
                name,
                enabled,
                scan_supported,
                entry.execution_supported,
                entry.notes.map(str::to_string),
            ));
        }

        for parser in self.parsers.values() {
            let covered_by_registry = BOOKMAKER_REGISTRY
                .iter()
                .any(|entry| entry.aliases.iter().any(|alias| *alias == parser.slug()));

            if covered_by_registry || items.iter().any(|item| item.slug == parser.slug()) {
                continue;
            }

            items.push(parser.metadata());
        }

        items.sort_by(|a, b| a.slug.cmp(&b.slug));
        items
    }

    pub fn parser_coverage(&self) -> Vec<ParserCoverage> {
        let mut items = Vec::new();

        for entry in BOOKMAKER_REGISTRY {
            let parser = entry
                .aliases
                .iter()
                .find_map(|alias| self.parsers.get(*alias));

            let (name, enabled, scan_supported, readiness) = if let Some(parser) = parser {
                (
                    parser.name().to_string(),
                    parser.is_enabled(),
                    parser.is_enabled(),
                    parser.readiness(),
                )
            } else {
                (entry.name.to_string(), false, false, None)
            };

            let metadata = BookmakerMetadata::new(
                entry.slug,
                name.clone(),
                enabled,
                scan_supported,
                entry.execution_supported,
                entry.notes.map(str::to_string),
            );

            items.push(ParserCoverage {
                slug: entry.slug.to_string(),
                name,
                enabled,
                scan_supported,
                execution_supported: entry.execution_supported,
                status: metadata.status,
                parser_type: entry.parser_type.to_string(),
                source: entry.source.to_string(),
                notes: entry.notes.map(str::to_string),
                readiness,
                runtime_health: None,
            });
        }

        items.sort_by(|a, b| a.slug.cmp(&b.slug));
        items
    }

    pub fn parser_health_snapshots(&self) -> Vec<ParserHealth> {
        let mut items = Vec::new();

        for coverage in self.parser_coverage() {
            let diagnostics = coverage
                .readiness
                .as_ref()
                .map(|item| item.checks.clone())
                .unwrap_or_default();
            let status = Self::snapshot_health_status(&coverage);
            let last_error = Self::snapshot_health_error(&coverage);

            items.push(ParserHealth {
                bookmaker: coverage.slug.clone(),
                status,
                last_success: None,
                last_error,
                consecutive_failures: 0,
                avg_response_time_ms: 0.0,
                events_parsed: 0,
                uptime_percent: 0.0,
                readiness: coverage.readiness.clone(),
                diagnostics,
            });
        }

        for health in &mut items {
            if let Some(entry) = Self::registry_entry(&health.bookmaker) {
                if health.last_error.is_none() {
                    health.last_error = entry.notes.map(str::to_string);
                }
            }
        }

        items.sort_by(|a, b| a.bookmaker.cmp(&b.bookmaker));
        items
    }
}

#[cfg(test)]
mod tests {
    use super::ParserFactory;
    use shared::{DiagnosticSeverity, HealthStatus, ParserReadinessStage};
    use std::sync::Arc;

    #[test]
    fn all_implemented_russian_bookmakers_are_registered() {
        let client = Arc::new(reqwest::Client::builder().build().expect("client"));
        let factory = ParserFactory::new(client);

        for slug in super::IMPLEMENTED_RU_BOOKMAKER_SLUGS {
            assert!(
                factory.get(slug).is_some(),
                "missing factory parser: {slug}"
            );
            assert!(
                factory
                    .parser_coverage()
                    .iter()
                    .any(|entry| entry.slug == *slug),
                "missing coverage entry: {slug}"
            );
        }
        assert_eq!(super::IMPLEMENTED_RU_BOOKMAKER_SLUGS.len(), 17);
        assert_eq!(super::RUSSIAN_BOOKMAKER_TARGET_SLUGS.len(), 20);
        for slug in super::ACTIVE_RUSSIAN_BOOKMAKER_SLUGS {
            assert!(
                super::IMPLEMENTED_RU_BOOKMAKER_SLUGS.contains(slug),
                "active operator {slug} must have a parser implementation"
            );
        }
        let coverage = factory.parser_coverage();
        for slug in super::RUSSIAN_BOOKMAKER_TARGET_SLUGS {
            assert!(
                coverage.iter().any(|entry| entry.slug == *slug),
                "missing FNS registry metadata: {slug}"
            );
        }
        for slug in super::IMPLEMENTED_RU_BOOKMAKER_SLUGS {
            assert!(
                super::RUSSIAN_BOOKMAKER_TARGET_SLUGS.contains(slug),
                "implemented slug {slug} must be part of the FNS target list"
            );
        }
    }

    #[test]
    fn active_and_inactive_operator_sets_partition_the_fns_inventory() {
        let active = super::ACTIVE_RUSSIAN_BOOKMAKER_SLUGS;
        let inactive = super::INACTIVE_RUSSIAN_BOOKMAKER_SLUGS;
        for slug in active.iter().chain(inactive.iter()) {
            assert!(
                super::RUSSIAN_BOOKMAKER_TARGET_SLUGS.contains(slug),
                "operator {slug} must be in the FNS inventory"
            );
        }
        for slug in active {
            assert!(
                !inactive.contains(slug),
                "operator {slug} cannot be both active and inactive"
            );
        }
        let mut partition = active
            .iter()
            .chain(inactive.iter())
            .copied()
            .collect::<Vec<_>>();
        partition.sort_unstable();
        partition.dedup();
        assert_eq!(partition.len(), 20);
        assert_eq!(active.len(), 16);
        assert_eq!(inactive.len(), 4);
    }

    #[test]
    fn ligastavok_coverage_includes_readiness() {
        let client = Arc::new(reqwest::Client::builder().build().expect("client"));
        let factory = ParserFactory::new(client);

        let coverage = factory
            .parser_coverage()
            .into_iter()
            .find(|item| item.slug == "ligastavok")
            .expect("coverage");

        assert_eq!(coverage.parser_type, "api");
        assert_eq!(coverage.source, "crates/parsers/src/ligastavok.rs");
        assert!(coverage.readiness.is_some());
        assert!(!coverage.enabled);
    }

    #[test]
    fn ligastavok_health_snapshot_includes_diagnostics() {
        let client = Arc::new(reqwest::Client::builder().build().expect("client"));
        let factory = ParserFactory::new(client);

        let health = factory
            .parser_health_snapshots()
            .into_iter()
            .find(|item| item.bookmaker == "ligastavok")
            .expect("health");

        assert!(matches!(health.status, HealthStatus::Degraded));
        assert!(health.readiness.is_some());
        assert!(health
            .diagnostics
            .iter()
            .any(|check| check.code == "qrator_unattended_bootstrap_unverified"));
    }

    #[test]
    fn betm_coverage_includes_readiness() {
        let client = Arc::new(reqwest::Client::builder().build().expect("client"));
        let factory = ParserFactory::new(client);

        let coverage = factory
            .parser_coverage()
            .into_iter()
            .find(|item| item.slug == "betm")
            .expect("coverage");

        assert_eq!(coverage.parser_type, "headless");
        assert_eq!(coverage.source, "crates/parsers/src/betm.rs");
        assert!(!coverage.enabled);
        let readiness = coverage.readiness.expect("readiness");
        assert_eq!(readiness.stage, ParserReadinessStage::DiagnosticOnly);
        assert!(!readiness.production_enabled);
        assert!(readiness.self_check_available);
        assert!(readiness
            .checks
            .iter()
            .any(|check| check.code == "public_feed_not_confirmed"
                && matches!(check.severity, DiagnosticSeverity::Warn)));
        assert!(readiness
            .checks
            .iter()
            .any(|check| check.code == "product_target_unverified"
                && matches!(check.severity, DiagnosticSeverity::Warn)));
    }

    #[test]
    fn betm_health_snapshot_includes_diagnostics() {
        let client = Arc::new(reqwest::Client::builder().build().expect("client"));
        let factory = ParserFactory::new(client);

        let health = factory
            .parser_health_snapshots()
            .into_iter()
            .find(|item| item.bookmaker == "betm")
            .expect("health");

        assert!(matches!(health.status, HealthStatus::Degraded));
        assert!(health.readiness.is_some());
        assert!(health
            .diagnostics
            .iter()
            .any(|check| check.code == "public_feed_not_confirmed"));
    }

    #[test]
    fn tennisi_coverage_matches_rust_parser_metadata() {
        let client = Arc::new(reqwest::Client::builder().build().expect("client"));
        let factory = ParserFactory::new(client);

        let coverage = factory
            .parser_coverage()
            .into_iter()
            .find(|item| item.slug == "tennisi")
            .expect("coverage");

        assert_eq!(coverage.parser_type, "api");
        assert_eq!(coverage.source, "crates/parsers/src/tennisi.rs");
        assert!(!coverage.enabled);
        assert!(!coverage.scan_supported);
        assert_eq!(
            coverage.notes.as_deref(),
            Some(
                "Operator suspended acceptance of bets from 2026-09-01; public reporting says the site/app were left for withdrawals only through 2026-09-30. ODDSCORP still lists Tennisi in its bookmaker catalogue, but that legacy listing does not prove a live feed. Keep disabled until the operator resumes betting and a provider confirms current quotes."
            )
        );
    }

    #[test]
    fn baltbet_coverage_reflects_post_kpi_readiness() {
        let client = Arc::new(reqwest::Client::builder().build().expect("client"));
        let factory = ParserFactory::new(client);

        let coverage = factory
            .parser_coverage()
            .into_iter()
            .find(|item| item.slug == "baltbet")
            .expect("coverage");

        assert!(coverage.enabled);
        assert!(coverage.scan_supported);
        assert_eq!(coverage.parser_type, "api");
        assert_eq!(coverage.source, "crates/parsers/src/baltbet.rs");
        assert_eq!(
            coverage.notes.as_deref(),
            Some(
                "Pure HTTP parser with live JSON, banner metadata fallback, and legacy prematch groups; strict nightly KPI progress is tracked in readiness diagnostics."
            )
        );

        let readiness = coverage.readiness.expect("readiness");
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
    fn zenit_coverage_reports_rollout_readiness_snapshot() {
        let client = Arc::new(reqwest::Client::builder().build().expect("client"));
        let factory = ParserFactory::new(client);

        let coverage = factory
            .parser_coverage()
            .into_iter()
            .find(|item| item.slug == "zenit")
            .expect("coverage");

        assert!(coverage.enabled);
        assert!(coverage.scan_supported);
        assert_eq!(coverage.parser_type, "api");
        assert_eq!(coverage.source, "crates/parsers/src/zenit.rs");
        let readiness = coverage.readiness.expect("readiness");
        assert_eq!(readiness.stage, ParserReadinessStage::RolloutReady);
        assert!(!readiness.production_enabled);
        assert!(readiness.self_check_available);
        assert!(readiness
            .checks
            .iter()
            .any(|check| check.code == "runtime_kpi_previously_met"
                && matches!(check.severity, DiagnosticSeverity::Pass)));
        assert!(readiness
            .checks
            .iter()
            .any(|check| check.code == "strict_nightly_regressed_to_zero"
                && matches!(check.severity, DiagnosticSeverity::Warn)));
        assert_eq!(
            coverage.notes.as_deref(),
            Some(
                "Pure HTTP parser with imprinthash authentication is registered for scan coverage and runtime diagnostics; readiness now keeps Zenit below production because recent strict nightly runs regressed to zero events after an earlier runtime pass."
            )
        );
    }

    #[test]
    fn betcity_coverage_explains_registration_without_production_promotion() {
        let client = Arc::new(reqwest::Client::builder().build().expect("client"));
        let factory = ParserFactory::new(client);

        let coverage = factory
            .parser_coverage()
            .into_iter()
            .find(|item| item.slug == "betcity")
            .expect("coverage");

        assert!(coverage.enabled);
        assert!(coverage.scan_supported);
        assert_eq!(coverage.parser_type, "api");
        assert_eq!(coverage.source, "crates/parsers/src/betcity.rs");
        let readiness = coverage.readiness.expect("readiness");
        assert_eq!(readiness.stage, ParserReadinessStage::RolloutReady);
        assert!(!readiness.production_enabled);
        assert!(readiness
            .checks
            .iter()
            .any(|check| check.code == "recent_runtime_samples_passed"
                && matches!(check.severity, DiagnosticSeverity::Pass)));
        assert_eq!(
            coverage.notes.as_deref(),
            Some(
                "HTTP parser is registered for scan coverage and runtime diagnostics; the latest synchronized reachable-only sweep and repeated direct-transport retries passed. Production enablement remains off pending the broader release gate."
            )
        );
    }

    #[test]
    fn olimp_metadata_is_not_marked_scan_only() {
        let client = Arc::new(reqwest::Client::builder().build().expect("client"));
        let factory = ParserFactory::new(client);

        let metadata = factory
            .bookmaker_metadata()
            .into_iter()
            .find(|item| item.slug == "olimp")
            .expect("metadata");

        assert!(metadata.enabled);
        assert!(metadata.scan_supported);
        assert_eq!(
            metadata.notes.as_deref(),
            Some(
                "HTTP parser is re-enabled through the direct Olimp competitions-with-events API path; readiness now locks one bounded 2026-11-26 runtime probe showing non-empty live and prematch event volume while production promotion remains gated."
            )
        );
    }

    #[test]
    fn enabled_parser_static_health_snapshot_is_not_healthy() {
        let client = Arc::new(reqwest::Client::builder().build().expect("client"));
        let factory = ParserFactory::new(client);

        let health = factory
            .parser_health_snapshots()
            .into_iter()
            .find(|item| item.bookmaker == "pari")
            .expect("health");

        assert!(matches!(health.status, HealthStatus::Degraded));
        assert!(health
            .last_error
            .as_deref()
            .is_some_and(|message| message.contains(ParserFactory::STATIC_HEALTH_NOTE)));
        assert_eq!(health.uptime_percent, 0.0);
    }
}
