use crate::base::{BookmakerParser, ParserResult};
use crate::headless_helper::{is_valid_team_name, HeadlessChromeHelper};
use async_trait::async_trait;
use chrono::Utc;
use reqwest::Client;
use shared::odds::OddsType;
use shared::{
    DiagnosticSeverity, Event, Odd, ParserDiagnosticCheck, ParserReadiness, ParserReadinessStage,
    Sport,
};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, info, warn};

const BOOKMAKER_SLUG: &str = "betm";
const HEADLESS_WAIT_MS: u64 = 6_000;
const HEADLESS_RETRY_DELAY_MS: u64 = 1_000;
const HEADLESS_EVAL_ATTEMPTS: usize = 3;
const HEADLESS_SCROLL_PASSES: usize = 3;
const HEADLESS_SCROLL_WAIT: Duration = Duration::from_millis(450);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RouteKind {
    Live,
    Prematch,
    Unknown,
}

impl RouteKind {
    fn from_source_url(source_url: &str) -> Self {
        let normalized = source_url.trim().to_ascii_lowercase();
        if normalized.is_empty() {
            return Self::Unknown;
        }

        if normalized.contains("/live") {
            Self::Live
        } else if normalized.contains("/line") || normalized.contains("/ru/sport/sports/") {
            Self::Prematch
        } else {
            Self::Unknown
        }
    }

    fn is_live(self, fallback_live: bool) -> bool {
        match self {
            Self::Live => true,
            Self::Prematch => false,
            Self::Unknown => fallback_live,
        }
    }

    fn league_label(self, fallback_live: bool) -> String {
        if self.is_live(fallback_live) {
            "Live".to_string()
        } else {
            "Pre-match".to_string()
        }
    }
}

#[derive(Debug, Default)]
struct RuntimeProbeStats {
    probes_attempted: usize,
    probes_with_payload: usize,
    navigation_failures: usize,
    unavailable_pages: usize,
    empty_payloads: usize,
    payload_items: usize,
    parsed_events: usize,
    parsed_odds: usize,
}

#[derive(Clone, Copy, Debug)]
struct Probe {
    url: &'static str,
    is_live: bool,
}

const PROBES: &[Probe] = &[
    Probe { url: "https://bet-m.ru/ru/sport/live", is_live: true },
    Probe { url: "https://bet-m.ru/ru/sport/sports/football", is_live: false },
    Probe { url: "https://bet-m.ru/ru/sport/sports/basketball", is_live: false },
    Probe { url: "https://bet-m.ru/ru/sport/sports/tennis", is_live: false },
    Probe { url: "https://bet-m.ru/ru/sport/sports/ice-hockey", is_live: false },
    Probe { url: "https://bet-m.ru/ru/sport/sports/table-tennis", is_live: false },
    Probe { url: "https://bet-m.ru/ru/sport/sports/handball", is_live: false },
    Probe { url: "https://bet-m.ru/ru/sport/sports/volleyball", is_live: false },
    Probe { url: "https://bet-m.ru/ru/sport/sports/boxing", is_live: false },
    Probe { url: "https://bet-m.ru/ru/sport/sports/martial-arts", is_live: false },
    Probe { url: "https://bet-m.ru/ru/sport/sports/cricket", is_live: false },
    Probe { url: "https://bet-m.ru/ru/sport/sports/baseball", is_live: false },
    Probe { url: "https://bet-m.ru/ru/sport/sports/badminton", is_live: false },
    Probe { url: "https://bet-m.ru/ru/sport/sports/american-football", is_live: false },
    Probe { url: "https://bet-m.ru/ru/sport/sports/formula/formula-1", is_live: false },
    Probe { url: "https://bet-m.ru/ru/sport/sports/esports", is_live: false },
];

const HEADLESS_EXTRACT_JS: &str = r#"(() => {
    const normalizeText = (value) => String(value || '').replace(/[ \t\r\f\v]+/g, ' ').trim();
    const parseOdds = (value) => {
        const normalized = normalizeText(value).replace(',', '.');
        if (!normalized) return null;
        const parsed = Number.parseFloat(normalized);
        return Number.isFinite(parsed) && parsed >= 1.01 && parsed <= 100 ? parsed : null;
    };
    const isName = (value) => {
        const normalized = normalizeText(value);
        if (!normalized || normalized.length < 2 || normalized.length > 80) return false;
        if (/^(live|match|event|game|line|football|basketball|tennis|события|линия|лайв)$/i.test(normalized)) return false;
        if (/^\d+[.,]?\d*$/.test(normalized)) return false;
        if (/^(today|tomorrow|сегодня|завтра)$/i.test(normalized)) return false;
        return true;
    };
    const splitPair = (value) => {
        const normalized = normalizeText(value).replace(/\s+/g, ' ');
        for (const separator of [' - ', ' – ', ' — ', ' vs ', ' VS ', ' v ', ' против ']) {
            const index = normalized.indexOf(separator);
            if (index <= 0) continue;
            const home = normalizeText(normalized.slice(0, index));
            const away = normalizeText(normalized.slice(index + separator.length));
            if (isName(home) && isName(away) && home !== away) return [home, away];
        }
        return null;
    };
    const firstAttribute = (node, names) => {
        for (const name of names) {
            const value = node.getAttribute(name);
            if (value && value.trim()) return value.trim();
        }
        return '';
    };
    const sportFromPath = (pathname) => {
        const path = String(pathname || '').toLowerCase();
        if (path.includes('/sports/basketball')) return 'basketball';
        if (path.includes('/sports/tennis')) return 'tennis';
        if (path.includes('/sports/ice-hockey')) return 'hockey';
        if (path.includes('/sports/table-tennis')) return 'table_tennis';
        if (path.includes('/sports/handball')) return 'handball';
        if (path.includes('/sports/volleyball')) return 'volleyball';
        if (path.includes('/sports/boxing')) return 'boxing';
        if (path.includes('/sports/martial-arts')) return 'mma';
        if (path.includes('/sports/cricket')) return 'cricket';
        if (path.includes('/sports/baseball')) return 'baseball';
        if (path.includes('/sports/badminton')) return 'badminton';
        if (path.includes('/sports/american-football')) return 'other';
        if (path.includes('/sports/formula/')) return 'motorsport';
        if (path.includes('/sports/esports')) return 'esports';
        if (path.includes('/sports/football')) return 'football';
        return 'other';
    };

    const sportFromLabel = (label) => {
        const value = String(label || '').toLowerCase();
        if (/настольн|table tennis|ping.?pong/.test(value)) return 'table_tennis';
        if (/футбол|football|soccer/.test(value)) return 'football';
        if (/баскет|basketball/.test(value)) return 'basketball';
        if (/теннис|tennis/.test(value)) return 'tennis';
        if (/хоккей|hockey/.test(value)) return 'hockey';
        if (/волейбол|volleyball/.test(value)) return 'volleyball';
        if (/гандбол|handball/.test(value)) return 'handball';
        if (/киберспорт|esports|e-sports/.test(value)) return 'esports';
        if (/бейсбол|baseball/.test(value)) return 'baseball';
        if (/бокс|boxing/.test(value)) return 'boxing';
        if (/единоборств|martial arts|mma|ufc/.test(value)) return 'mma';
        if (/крикет|cricket/.test(value)) return 'cricket';
        if (/регби|rugby/.test(value)) return 'rugby';
        if (/футзал|futsal/.test(value)) return 'futsal';
        if (/бадминтон|badminton/.test(value)) return 'badminton';
        if (/дартс|darts/.test(value)) return 'darts';
        if (/снукер|snooker|бильярд/.test(value)) return 'snooker';
        if (/водное поло|water polo/.test(value)) return 'water_polo';
        if (/гольф|golf/.test(value)) return 'golf';
        if (/биатлон|лыжи|winter sports/.test(value)) return 'winter_sports';
        if (/формула|автоспорт|motorsport|formula 1|f1/.test(value)) return 'motorsport';
        return 'other';
    };

    const sportFromNode = (el) => {
        let node = el;
        for (let depth = 0; node && depth < 6; depth += 1, node = node.parentElement) {
            const label = firstAttribute(node, [
                'data-sport',
                'data-sport-name',
                'data-sport-title',
                'aria-label'
            ]);
            const mapped = sportFromLabel(label);
            if (mapped !== 'other') return mapped;
        }
        return 'other';
    };

    const seen = new Set();
    const results = [];
    const selectors = [
        '[data-event-id]', '[data-eventid]', '.event-item', '.match-item', '.game-item',
        '.sport-event', '.event-line', '[class*="event-card"]', '[class*="match-card"]',
        '[class*="event"]', '[class*="match"]', '[class*="game"]'
    ];
    const containers = Array.from(new Set(selectors.flatMap((selector) => Array.from(document.querySelectorAll(selector)))));
    const sourceUrl = window.location.href;
    const sport = sportFromPath(window.location.pathname);

    for (const el of containers) {
        try {
            const rawText = String(el.innerText || el.textContent || '');
            if (normalizeText(rawText).length < 8) continue;

            let home = '';
            let away = '';
            const participantSelectors = [
                '[data-team-name]', '[data-participant-name]', '[class*="participant"]',
                '[class*="competitor"]', '[class*="team-name"]', '[class*="team_name"]'
            ];
            const participants = Array.from(new Set(participantSelectors.flatMap((selector) => {
                try { return Array.from(el.querySelectorAll(selector)); } catch (_) { return []; }
            })))
                .map((node) => normalizeText(firstAttribute(node, ['data-team-name', 'data-participant-name']) || node.innerText || node.textContent))
                .filter(isName)
                .filter((name, index, list) => list.indexOf(name) === index);

            const homeNode = el.querySelector('[data-home-team], [class*="home-team"], [class*="team-home"]');
            const awayNode = el.querySelector('[data-away-team], [class*="away-team"], [class*="team-away"]');
            const explicitHome = firstAttribute(el, ['data-home-team']) || (homeNode ? normalizeText(firstAttribute(homeNode, ['data-home-team']) || homeNode.innerText || homeNode.textContent) : '');
            const explicitAway = firstAttribute(el, ['data-away-team']) || (awayNode ? normalizeText(firstAttribute(awayNode, ['data-away-team']) || awayNode.innerText || awayNode.textContent) : '');
            if (isName(explicitHome) && isName(explicitAway) && explicitHome !== explicitAway) {
                home = explicitHome;
                away = explicitAway;
            } else if (participants.length >= 2) {
                [home, away] = participants.slice(0, 2);
            } else {
                const titleCandidate = firstAttribute(el, ['data-event-name', 'data-match-name', 'aria-label', 'title']);
                const titlePair = splitPair(titleCandidate);
                if (titlePair) {
                    [home, away] = titlePair;
                } else {
                    for (const line of rawText.split(/\\n+/).map(normalizeText).filter(Boolean)) {
                        const pair = splitPair(line);
                        if (pair) { [home, away] = pair; break; }
                    }
                }
            }

            if (!home || !away || home === away || !isName(home) || !isName(away)) continue;
            const idNode = (el.matches && el.matches('[data-event-id], [data-eventid], [data-id]'))
                ? el
                : el.querySelector('[data-event-id], [data-eventid], [data-id]');
            const sourceEventId = idNode ? firstAttribute(idNode, ['data-event-id', 'data-eventid', 'data-id']) : '';
            const eventSport = sport !== 'other' ? sport : sportFromNode(el);

            // Only keep prices whose source DOM explicitly supplies a market and selection label.
            const labelledOdds = [];
            const priceNodes = el.querySelectorAll(
                '[data-odd][data-selection], [data-odd][data-outcome], ' +
                '[data-coefficient][data-selection], [data-price][data-selection]'
            );
            priceNodes.forEach((node) => {
                const selectionNode = node.closest('[data-selection], [data-outcome], [data-outcome-name]') || node;
                const marketNode = node.closest('[data-market], [data-market-name]') || node;
                const market = firstAttribute(marketNode, ['data-market-name', 'data-market']);
                const selection = firstAttribute(selectionNode, ['data-selection', 'data-outcome-name', 'data-outcome']);
                const rawPrice = firstAttribute(node, ['data-odd', 'data-coefficient', 'data-price']) || node.textContent;
                const price = parseOdds(rawPrice);
                const lineNode = node.closest('[data-line]');
                const line = firstAttribute(node, ['data-line']) || (lineNode ? lineNode.getAttribute('data-line') : '');
                if (market && selection && price !== null) labelledOdds.push({ market, selection, odds: price, line: line || null });
            });

            const key = sourceEventId
                ? [eventSport, sourceEventId, sourceUrl].join('|')
                : [eventSport, home, away, sourceUrl].join('|');
            if (seen.has(key)) continue;
            seen.add(key);
            results.push({ home, away, sourceEventId: sourceEventId || null, sport: eventSport, labelledOdds, sourceUrl });
        } catch (_) {}
    }
    return results;
})()"#;

#[derive(Debug)]
pub struct BetMParser {
    client: Arc<Client>,
}

impl BetMParser {
    pub fn new(client: Arc<Client>) -> Self {
        Self { client }
    }

    fn readiness_snapshot() -> ParserReadiness {
        ParserReadiness {
            stage: ParserReadinessStage::DiagnosticOnly,
            production_enabled: false,
            self_check_available: true,
            checks: vec![
                ParserDiagnosticCheck {
                    code: "legacy_dom_path_ported".to_string(),
                    severity: DiagnosticSeverity::Pass,
                    message: "Legacy Playwright DOM extraction is ported to the Rust headless helper.".to_string(),
                },
                ParserDiagnosticCheck {
                    code: "legacy_and_current_domains_probed".to_string(),
                    severity: DiagnosticSeverity::Pass,
                    message: "The Rust path targets the official bet-m.ru domain listed for the Russian operator.".to_string(),
                },
                ParserDiagnosticCheck {
                    code: "legacy_net_routes_return_placeholder_404".to_string(),
                    severity: DiagnosticSeverity::Pass,
                    message: "The previous bet-m.net and betm.ru aliases were not authoritative for the Russian operator; runtime now targets the official bet-m.ru domain.".to_string(),
                },
                ParserDiagnosticCheck {
                    code: "betm_official_domain_access_denied".to_string(),
                    severity: DiagnosticSeverity::Warn,
                    message: "The official bet-m.ru sports page currently displays a regional-access restriction (site unavailable in this region); no public feed payload has been verified from this environment.".to_string(),
                },
                ParserDiagnosticCheck {
                    code: "public_feed_not_confirmed".to_string(),
                    severity: DiagnosticSeverity::Warn,
                    message: "The official bet-m.ru site is reachable only as an access-denied response from this development environment. Feed endpoints and live/prematch data remain unverified, so scan enablement stays off.".to_string(),
                },
                ParserDiagnosticCheck {
                    code: "product_target_unverified".to_string(),
                    severity: DiagnosticSeverity::Warn,
                    message: "Live 150+ and prematch 3000+ coverage is not yet verifiable from the available public surface.".to_string(),
                },
            ],
        }
    }

    fn is_unavailable_page_text(text: &str) -> bool {
        let normalized = text.to_lowercase();
        normalized.contains("404 not found")
            || normalized.contains("сайт недоступен в вашем регионе")
            || normalized.contains("недоступен в вашем регионе")
            || normalized.contains("недоступен в вашей стране")
            || normalized.contains("доступ к сайту ограничен")
    }

    fn is_plausible_team(value: &str) -> bool {
        if !is_valid_team_name(value) {
            return false;
        }

        let normalized = value.to_lowercase();
        !normalized.contains("404")
            && !normalized.contains("not found")
            && !normalized.contains("bet-m")
    }

    fn split_match_title(value: &str) -> Option<(String, String)> {
        for separator in [" - ", " -", "- ", " – ", " — ", " vs ", " VS ", " v "] {
            let Some(position) = value.find(separator) else {
                continue;
            };

            let home = value[..position].trim();
            let away = value[position + separator.len()..].trim();
            if Self::is_plausible_team(home) && Self::is_plausible_team(away) && home != away {
                return Some((home.to_string(), away.to_string()));
            }
        }

        None
    }

    fn normalize_identifier(value: &str) -> String {
        value
            .chars()
            .map(|ch| {
                if ch.is_ascii_alphanumeric() {
                    ch.to_ascii_lowercase()
                } else if ch.is_alphanumeric() {
                    ch
                } else {
                    '-'
                }
            })
            .collect::<String>()
            .split('-')
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join("-")
    }

    fn sport_from_source_url(source_url: &str) -> Sport {
        let normalized = source_url.trim().to_ascii_lowercase();
        if normalized.contains("/sports/basketball") {
            Sport::Basketball
        } else if normalized.contains("/sports/tennis") {
            Sport::Tennis
        } else if normalized.contains("/sports/ice-hockey") {
            Sport::Hockey
        } else if normalized.contains("/sports/table-tennis") {
            Sport::TableTennis
        } else if normalized.contains("/sports/handball") {
            Sport::Handball
        } else if normalized.contains("/sports/volleyball") {
            Sport::Volleyball
        } else if normalized.contains("/sports/boxing") {
            Sport::Boxing
        } else if normalized.contains("/sports/martial-arts") {
            Sport::Mma
        } else if normalized.contains("/sports/cricket") {
            Sport::Cricket
        } else if normalized.contains("/sports/baseball") {
            Sport::Baseball
        } else if normalized.contains("/sports/badminton") {
            Sport::Badminton
        } else if normalized.contains("/sports/formula/") {
            Sport::Motorsport
        } else if normalized.contains("/sports/esports") {
            Sport::Esports
        } else if normalized.contains("/sports/football") {
            Sport::Football
        } else {
            Sport::Other
        }
    }

    fn build_event_id(
        sport: Sport,
        home: &str,
        away: &str,
        is_live: bool,
        source_event_id: Option<&str>,
    ) -> String {
        let sport_key = sport.to_string().to_ascii_lowercase().replace('_', "-");
        let mode = if is_live { "live" } else { "prematch" };
        if let Some(source_id) = source_event_id
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            return format!(
                "betm-{sport_key}-{mode}-{}",
                Self::normalize_identifier(source_id)
            );
        }

        format!(
            "betm-{sport_key}-{mode}-{}-{}",
            Self::normalize_identifier(home),
            Self::normalize_identifier(away)
        )
    }

    fn parse_numeric_value(value: &serde_json::Value) -> Option<f64> {
        let parsed = value.as_f64().or_else(|| {
            value
                .as_str()
                .and_then(|raw| raw.trim().replace(',', ".").parse::<f64>().ok())
        })?;
        (parsed.is_finite() && (1.01..=100.0).contains(&parsed)).then_some(parsed)
    }

    fn odds_type_from_selection(selection: &str) -> OddsType {
        let normalized = selection.trim().to_lowercase().replace('ё', "е");
        match normalized.as_str() {
            "1" | "п1" | "home" | "home win" | "хозяева" => OddsType::Home,
            "x" | "х" | "ничья" | "draw" => OddsType::Draw,
            "2" | "п2" | "away" | "away win" | "гости" => OddsType::Away,
            "больше" | "б" | "тб" | "over" => OddsType::Over,
            "меньше" | "м" | "тм" | "under" => OddsType::Under,
            "да" | "yes" | "обе забьют да" => OddsType::BothTeamsScoreYes,
            "нет" | "no" | "обе забьют нет" => OddsType::BothTeamsScoreNo,
            "чет" | "even" | "четное" => OddsType::Even,
            "нечет" | "odd" | "нечетное" => OddsType::Odd,
            _ => OddsType::Custom,
        }
    }

    #[cfg(test)]
    fn detect_live_state(source_url: &str, fallback_live: bool) -> bool {
        RouteKind::from_source_url(source_url).is_live(fallback_live)
    }

    fn parse_headless_item(
        item: &serde_json::Value,
        fallback_live: bool,
    ) -> Option<(Event, Vec<Odd>)> {
        let home_team = item
            .get("home")
            .and_then(|value| value.as_str())
            .map(str::trim);
        let away_team = item
            .get("away")
            .and_then(|value| value.as_str())
            .map(str::trim);
        let (home_team, away_team) = match (home_team, away_team) {
            (Some(home_team), Some(away_team))
                if Self::is_plausible_team(home_team)
                    && Self::is_plausible_team(away_team)
                    && home_team != away_team =>
            {
                (home_team.to_string(), away_team.to_string())
            }
            (Some(home_team), _) | (_, Some(home_team)) => Self::split_match_title(home_team)?,
            _ => return None,
        };

        let source_url = item
            .get("sourceUrl")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        let route_kind = RouteKind::from_source_url(source_url);
        let is_live = route_kind.is_live(fallback_live);
        let sport = item
            .get("sport")
            .and_then(|value| value.as_str())
            .map(|value| {
                let parsed = Sport::from_str(value);
                if parsed != Sport::Other {
                    parsed
                } else {
                    let normalized = value.replace('_', " ").replace('-', " ");
                    Sport::from_str(&normalized)
                }
            })
            .filter(|sport| *sport != Sport::Other)
            .unwrap_or_else(|| Self::sport_from_source_url(source_url));
        let source_event_id = item
            .get("sourceEventId")
            .and_then(|value| value.as_str().map(str::to_string))
            .or_else(|| {
                item.get("sourceEventId")
                    .filter(|value| value.is_number())
                    .map(ToString::to_string)
            });
        let event_id = Self::build_event_id(
            sport,
            &home_team,
            &away_team,
            is_live,
            source_event_id.as_deref(),
        );
        let league = route_kind.league_label(fallback_live);

        let mut extra = HashMap::new();
        if let Some(source_id) = source_event_id.as_ref() {
            extra.insert(
                "source_event_id".to_string(),
                serde_json::Value::String(source_id.clone()),
            );
        }

        let event = Event {
            id: event_id.clone(),
            sport,
            league,
            home_team: home_team.clone(),
            away_team: away_team.clone(),
            start_time: None,
            is_live,
            bookmaker_slug: BOOKMAKER_SLUG.to_string(),
            raw_url: (!source_url.is_empty()).then(|| source_url.to_string()),
            extra,
        };

        // Do not infer a 1X2 or Moneyline market from an unlabelled list of
        // numbers. Only emit prices when the source DOM explicitly supplies
        // both the market and selection labels.
        let now = Utc::now();
        let mut odds = Vec::new();
        if let Some(labelled_odds) = item.get("labelledOdds").and_then(|value| value.as_array()) {
            for quote in labelled_odds {
                let Some(market) = quote
                    .get("market")
                    .and_then(|value| value.as_str())
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                else {
                    continue;
                };
                let Some(selection) = quote
                    .get("selection")
                    .and_then(|value| value.as_str())
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                else {
                    continue;
                };
                let Some(price) = quote.get("odds").and_then(Self::parse_numeric_value) else {
                    continue;
                };
                let line = quote
                    .get("line")
                    .and_then(|value| {
                        value.as_f64().or_else(|| {
                            value
                                .as_str()
                                .and_then(|raw| raw.trim().replace(',', ".").parse::<f64>().ok())
                        })
                    })
                    .filter(|value| value.is_finite());
                let market_key = Self::normalize_identifier(market);
                let selection_key = Self::normalize_identifier(selection);
                let line_key = line
                    .map(|value| format!("{value:.4}"))
                    .unwrap_or_else(|| "none".into());
                odds.push(Odd {
                    id: format!("{event_id}-{market_key}-{selection_key}-{line_key}"),
                    event_id: event_id.clone(),
                    bookmaker_slug: BOOKMAKER_SLUG.to_string(),
                    market: market.to_string(),
                    selection: selection.to_string(),
                    odds: price,
                    odds_type: Self::odds_type_from_selection(selection),
                    line,
                    timestamp: now,
                });
            }
        }

        Some((event, odds))
    }

    fn merge_unique_payload_items(
        payload: &mut Vec<serde_json::Value>,
        seen: &mut HashSet<String>,
        incoming: Vec<serde_json::Value>,
    ) {
        for item in incoming {
            let fingerprint = item.to_string();
            if seen.insert(fingerprint) {
                payload.push(item);
            }
        }
    }

    fn parse_headless_payload(
        payload: &[serde_json::Value],
        fallback_live: bool,
    ) -> (Vec<Event>, Vec<Odd>) {
        let mut events = Vec::new();
        let mut odds = Vec::new();
        let mut seen = HashSet::new();

        for item in payload {
            if let Some((event, mut event_odds)) = Self::parse_headless_item(item, fallback_live) {
                if seen.insert(event.id.clone()) {
                    events.push(event);
                    odds.append(&mut event_odds);
                }
            }
        }

        (events, odds)
    }

    fn fetch_runtime_data_blocking(
    ) -> Result<(Vec<Event>, Vec<Odd>), Box<dyn std::error::Error + Send + Sync>> {
        let helper = HeadlessChromeHelper::new()?;
        let mut all_events = Vec::new();
        let mut all_odds = Vec::new();
        let mut seen_event_ids = HashSet::new();
        let mut seen_odd_ids = HashSet::new();
        let mut stats = RuntimeProbeStats::default();

        for probe in PROBES {
            stats.probes_attempted += 1;

            let tab = match helper.navigate_and_wait(probe.url, HEADLESS_WAIT_MS) {
                Ok(tab) => tab,
                Err(error) => {
                    stats.navigation_failures += 1;
                    debug!(url = probe.url, error = %error, "BetM: navigation failed");
                    continue;
                }
            };

            let page_text = HeadlessChromeHelper::get_page_text(&tab).unwrap_or_default();
            if Self::is_unavailable_page_text(&page_text) {
                stats.unavailable_pages += 1;
                debug!(
                    url = probe.url,
                    "BetM: route returned unavailable placeholder page"
                );
                continue;
            }

            let mut payload = Vec::new();
            let mut payload_fingerprints = HashSet::new();
            let initial_payload = HeadlessChromeHelper::evaluate_json_with_retry(
                &tab,
                HEADLESS_EXTRACT_JS,
                HEADLESS_EVAL_ATTEMPTS,
                HEADLESS_RETRY_DELAY_MS,
            )
            .and_then(|value| value.as_array().cloned())
            .unwrap_or_default();
            Self::merge_unique_payload_items(
                &mut payload,
                &mut payload_fingerprints,
                initial_payload,
            );

            // Sportsbooks often lazy-load event cards below the first viewport.
            // Collect each scroll batch and merge by full source-item fingerprint;
            // event-level deduplication still happens after semantic parsing.
            let mut empty_scroll_streak = 0usize;
            for _ in 0..HEADLESS_SCROLL_PASSES {
                if HeadlessChromeHelper::scroll_page(&tab).is_err() {
                    break;
                }
                std::thread::sleep(HEADLESS_SCROLL_WAIT);
                let next_payload = HeadlessChromeHelper::evaluate_json_with_retry(
                    &tab,
                    HEADLESS_EXTRACT_JS,
                    1,
                    HEADLESS_RETRY_DELAY_MS,
                )
                .and_then(|value| value.as_array().cloned())
                .unwrap_or_default();
                let previous_len = payload.len();
                Self::merge_unique_payload_items(
                    &mut payload,
                    &mut payload_fingerprints,
                    next_payload,
                );
                if payload.len() == previous_len {
                    empty_scroll_streak += 1;
                    if empty_scroll_streak >= 2 {
                        break;
                    }
                } else {
                    empty_scroll_streak = 0;
                }
            }

            if payload.is_empty() {
                stats.empty_payloads += 1;
                debug!(url = probe.url, "BetM: probe returned empty payload");
                continue;
            }

            let (events, odds) = Self::parse_headless_payload(&payload, probe.is_live);
            stats.probes_with_payload += 1;
            stats.payload_items += payload.len();
            stats.parsed_events += events.len();
            stats.parsed_odds += odds.len();
            let new_events = events
                .iter()
                .filter(|event| !seen_event_ids.contains(&event.id))
                .count();
            let new_odds = odds
                .iter()
                .filter(|odd| !seen_odd_ids.contains(&odd.id))
                .count();
            debug!(
                url = probe.url,
                items = payload.len(),
                events = events.len(),
                odds = odds.len(),
                new_events,
                new_odds,
                "BetM: headless payload parsed"
            );

            for event in events {
                if seen_event_ids.insert(event.id.clone()) {
                    all_events.push(event);
                }
            }
            for odd in odds {
                if seen_odd_ids.insert(odd.id.clone()) {
                    all_odds.push(odd);
                }
            }
        }

        info!(
            probes_attempted = stats.probes_attempted,
            probes_with_payload = stats.probes_with_payload,
            navigation_failures = stats.navigation_failures,
            unavailable_pages = stats.unavailable_pages,
            empty_payloads = stats.empty_payloads,
            payload_items = stats.payload_items,
            parsed_events = stats.parsed_events,
            parsed_odds = stats.parsed_odds,
            unique_events = all_events.len(),
            unique_odds = all_odds.len(),
            "BetM: runtime probe summary"
        );

        Ok((all_events, all_odds))
    }

    async fn fetch_runtime_data(
        &self,
    ) -> Result<(Vec<Event>, Vec<Odd>), Box<dyn std::error::Error + Send + Sync>> {
        // Check the official public page before starting a headless browser.
        // This is a normal page request, not an attempt to evade the regional
        // access restriction; it turns a known 403 into a fast, explicit result.
        match self
            .client
            .get("https://bet-m.ru/ru/sport/sports/football")
            .header("User-Agent", "Mozilla/5.0")
            .timeout(Duration::from_secs(8))
            .send()
            .await
        {
            Ok(response) => {
                let status = response.status();
                let page = response.text().await.unwrap_or_default();
                if Self::is_unavailable_page_text(&page) || status.as_u16() == 403 {
                    return Err(format!(
                        "Bet-M official sports page is unavailable from this region (HTTP {status}); headless feed parsing was not started"
                    )
                    .into());
                }
            }
            Err(error) => {
                debug!(error = %error, "Bet-M official page preflight failed; continuing with the bounded headless probe");
            }
        }

        tokio::task::spawn_blocking(Self::fetch_runtime_data_blocking)
            .await
            .map_err(|error| -> Box<dyn std::error::Error + Send + Sync> { Box::new(error) })?
    }
}

#[async_trait]
impl BookmakerParser for BetMParser {
    fn name(&self) -> &str {
        "Bet-M"
    }

    fn slug(&self) -> &str {
        BOOKMAKER_SLUG
    }

    fn is_enabled(&self) -> bool {
        false
    }

    fn readiness(&self) -> Option<ParserReadiness> {
        Some(Self::readiness_snapshot())
    }

    async fn fetch_events(&self) -> Result<Vec<Event>, Box<dyn std::error::Error + Send + Sync>> {
        let (events, _) = self.fetch_runtime_data().await?;
        info!(count = events.len(), "BetM: events fetched");
        Ok(events)
    }

    async fn fetch_odds(
        &self,
        _event_id: &str,
    ) -> Result<Vec<Odd>, Box<dyn std::error::Error + Send + Sync>> {
        let (_, odds) = self.fetch_runtime_data().await?;
        info!(count = odds.len(), "BetM: odds fetched");
        Ok(odds)
    }

    async fn fetch_all(&self) -> Result<ParserResult, Box<dyn std::error::Error + Send + Sync>> {
        let started = std::time::Instant::now();
        let (events, odds) = self.fetch_runtime_data().await?;
        let elapsed = started.elapsed().as_millis() as u64;

        if events.is_empty() {
            warn!("BetM: no public events detected on probed routes");
        }

        Ok(ParserResult::new(BOOKMAKER_SLUG, events, odds, elapsed))
    }

    fn base_url(&self) -> &str {
        "https://bet-m.ru"
    }

    fn user_agent(&self) -> &str {
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36"
    }
}

#[cfg(test)]
mod tests {
    use super::{BetMParser, RouteKind};
    use shared::{OddsType, Sport};

    #[test]
    fn payload_batches_are_merged_without_duplicate_source_items() {
        let mut merged = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let first = serde_json::json!({"id": "1", "home": "A", "away": "B"});
        let second = serde_json::json!({"id": "2", "home": "C", "away": "D"});

        BetMParser::merge_unique_payload_items(
            &mut merged,
            &mut seen,
            vec![first.clone(), second.clone()],
        );
        BetMParser::merge_unique_payload_items(&mut merged, &mut seen, vec![first, second.clone()]);

        assert_eq!(merged.len(), 2);
        assert_eq!(merged[1], second);
    }

    #[test]
    fn runtime_probes_cover_current_public_live_and_multisport_routes() {
        assert_eq!(super::PROBES.len(), 16);
        assert!(super::PROBES
            .iter()
            .any(|probe| { probe.is_live && probe.url == "https://bet-m.ru/ru/sport/live" }));
        let prematch = super::PROBES
            .iter()
            .filter(|probe| !probe.is_live)
            .map(|probe| probe.url)
            .collect::<std::collections::HashSet<_>>();
        for route in [
            "football",
            "basketball",
            "tennis",
            "ice-hockey",
            "table-tennis",
            "handball",
            "volleyball",
            "boxing",
            "martial-arts",
            "cricket",
            "baseball",
            "badminton",
            "american-football",
            "formula/formula-1",
            "esports",
        ] {
            assert!(
                prematch.contains(format!("https://bet-m.ru/ru/sport/sports/{route}").as_str()),
                "missing official prematch route: {route}"
            );
        }
    }

    #[test]
    fn parses_events_without_guessing_unlabelled_prices() {
        let payload: Vec<serde_json::Value> =
            serde_json::from_str(include_str!("../tests/fixtures/betm_headless_payload.json"))
                .expect("fixture should parse");

        let (events, odds) = BetMParser::parse_headless_payload(&payload, true);

        assert_eq!(events.len(), 3);
        assert_eq!(odds.len(), 5);

        let football = events
            .iter()
            .find(|event| event.home_team == "Спартак Москва")
            .expect("football event exists");
        assert_eq!(football.sport, Sport::Football);
        assert!(football.is_live);
        assert_eq!(football.league, "Live");
        assert!(football.id.contains("betm-football-live"));

        assert!(odds.iter().any(|odd| {
            odd.event_id == football.id
                && odd.selection == "X"
                && odd.market == "1X2"
                && odd.odds_type == OddsType::Draw
                && (odd.odds - 3.55).abs() < f64::EPSILON
        }));

        let basketball = events
            .iter()
            .find(|event| event.sport == Sport::Basketball)
            .expect("basketball event exists");
        assert_eq!(basketball.home_team, "Локомотив Кубань");
        assert_eq!(basketball.away_team, "УНИКС");
        assert!(!basketball.is_live);
        assert!(basketball.id.starts_with("betm-basketball-prematch-"));

        assert!(odds.iter().any(|odd| {
            odd.event_id == basketball.id
                && odd.market == "Moneyline"
                && odd.selection == "2"
                && odd.odds_type == OddsType::Away
                && (odd.odds - 2.08).abs() < f64::EPSILON
        }));

        let real_madrid = events
            .iter()
            .find(|event| event.home_team == "Реал Мадрид")
            .expect("event without labelled prices still exists");
        assert_eq!(real_madrid.sport, Sport::Football);
        assert!(!odds.iter().any(|odd| odd.event_id == real_madrid.id));

        assert_eq!(
            events
                .iter()
                .filter(|event| event.home_team == "Спартак Москва" && event.away_team == "Зенит")
                .count(),
            1
        );
    }

    #[test]
    fn same_source_event_id_is_namespaced_by_sport_and_feed_mode() {
        let football =
            BetMParser::build_event_id(Sport::Football, "Team A", "Team B", false, Some("123"));
        let basketball =
            BetMParser::build_event_id(Sport::Basketball, "Team A", "Team B", false, Some("123"));
        let live_football =
            BetMParser::build_event_id(Sport::Football, "Team A", "Team B", true, Some("123"));

        assert_ne!(football, basketball);
        assert_ne!(football, live_football);
    }

    #[test]
    fn maps_official_public_routes_to_their_sports() {
        let cases = [
            ("/ru/sport/sports/football", Sport::Football),
            ("/ru/sport/sports/basketball", Sport::Basketball),
            ("/ru/sport/sports/tennis", Sport::Tennis),
            ("/ru/sport/sports/ice-hockey", Sport::Hockey),
            ("/ru/sport/sports/table-tennis", Sport::TableTennis),
            ("/ru/sport/sports/handball", Sport::Handball),
            ("/ru/sport/sports/volleyball", Sport::Volleyball),
            ("/ru/sport/sports/boxing", Sport::Boxing),
            ("/ru/sport/sports/martial-arts", Sport::Mma),
            ("/ru/sport/sports/cricket", Sport::Cricket),
            ("/ru/sport/sports/baseball", Sport::Baseball),
            ("/ru/sport/sports/badminton", Sport::Badminton),
            ("/ru/sport/sports/formula/formula-1", Sport::Motorsport),
            ("/ru/sport/sports/esports", Sport::Esports),
            ("/ru/sport/sports/american-football", Sport::Other),
        ];

        for (route, expected) in cases {
            let url = format!("https://bet-m.ru{route}");
            assert_eq!(BetMParser::sport_from_source_url(&url), expected, "{route}");
        }
        assert_eq!(
            BetMParser::sport_from_source_url("https://bet-m.ru/ru/sport/live"),
            Sport::Other
        );
    }

    #[test]
    fn parses_snake_case_sport_slug_from_live_event_metadata() {
        let item = serde_json::json!({
            "home": "Спартак Москва",
            "away": "Зенит",
            "sport": "table_tennis",
            "sourceEventId": "live-table-tennis-77",
            "sourceUrl": "https://bet-m.ru/ru/sport/live",
            "labelledOdds": []
        });

        let (events, odds) = BetMParser::parse_headless_payload(&[item], false);

        assert_eq!(events.len(), 1);
        assert!(odds.is_empty());
        assert_eq!(events[0].sport, Sport::TableTennis);
        assert!(events[0].is_live);
    }

    #[test]
    fn generic_live_route_uses_explicit_event_sport_metadata() {
        let item = serde_json::json!({
            "home": "Локомотив Кубань",
            "away": "УНИКС",
            "sport": "basketball",
            "sourceEventId": "live-basketball-42",
            "sourceUrl": "https://bet-m.ru/ru/sport/live",
            "labelledOdds": []
        });

        let (events, odds) = BetMParser::parse_headless_payload(&[item], false);

        assert_eq!(events.len(), 1);
        assert!(odds.is_empty());
        assert_eq!(events[0].sport, Sport::Basketball);
        assert!(events[0].is_live);
        assert!(events[0].id.starts_with("betm-basketball-live-"));
    }

    #[test]
    fn parses_prices_only_when_market_and_selection_are_explicit() {
        let explicit = serde_json::json!({
            "home": "Спартак Москва",
            "away": "Зенит",
            "sourceEventId": "456",
            "sourceUrl": "https://bet-m.ru/ru/sport/sports/football",
            "odds": [2.0, 3.0, 4.0],
            "labelledOdds": [
                {"market": "1X2", "selection": "П1", "odds": "2,10"},
                {"market": "1X2", "selection": "X", "odds": "3.20"},
                {"market": "1X2", "selection": "П2", "odds": "4.00"}
            ]
        });
        let unlabelled = serde_json::json!({
            "home": "Локомотив Кубань",
            "away": "УНИКС",
            "sourceUrl": "https://bet-m.ru/ru/sport/sports/football",
            "odds": [1.5, 2.5, 3.5]
        });

        let (_, explicit_odds) = BetMParser::parse_headless_payload(&[explicit], false);
        let (_, unlabelled_odds) = BetMParser::parse_headless_payload(&[unlabelled], false);

        assert_eq!(explicit_odds.len(), 3);
        assert_eq!(explicit_odds[0].odds_type, OddsType::Home);
        assert!((explicit_odds[0].odds - 2.1).abs() < f64::EPSILON);
        assert!(unlabelled_odds.is_empty());
    }

    #[test]
    fn detects_live_state_from_source_url_with_fallback_for_unknown_routes() {
        assert!(BetMParser::detect_live_state(
            "https://bet-m.ru/ru/sport/live",
            false
        ));
        assert!(!BetMParser::detect_live_state(
            "https://bet-m.ru/ru/sport/sports/football",
            true
        ));
        assert!(BetMParser::detect_live_state(
            "https://bet-m.ru/sports",
            true
        ));
        assert!(!BetMParser::detect_live_state("", false));
    }

    #[test]
    fn classifies_route_kind_explicitly() {
        assert_eq!(
            RouteKind::from_source_url("https://bet-m.ru/ru/sport/live"),
            RouteKind::Live
        );
        assert_eq!(
            RouteKind::from_source_url("https://bet-m.ru/ru/sport/sports/football"),
            RouteKind::Prematch
        );
        assert_eq!(
            RouteKind::from_source_url("https://bet-m.ru/sports"),
            RouteKind::Unknown
        );
        assert_eq!(RouteKind::from_source_url(""), RouteKind::Unknown);
    }

    #[test]
    fn treats_only_explicit_unavailable_markers_as_blocked() {
        assert!(BetMParser::is_unavailable_page_text(
            "404 Not Found\nHome\nBet-M"
        ));
        assert!(BetMParser::is_unavailable_page_text(
            "Сайт недоступен в вашем регионе"
        ));
        assert!(BetMParser::is_unavailable_page_text(
            "Данный сайт недоступен в вашей стране"
        ));
        assert!(BetMParser::is_unavailable_page_text(
            "Доступ к сайту ограничен"
        ));
        assert!(!BetMParser::is_unavailable_page_text(
            "Бет-М — букмекерская контора. Официальный сайт. Линия и Live."
        ));
        assert!(!BetMParser::is_unavailable_page_text(
            "Лига чемпионов\nСпартак Москва\nЗенит"
        ));
    }

    #[test]
    fn readiness_snapshot_keeps_betm_diagnostic_only() {
        let readiness = BetMParser::readiness_snapshot();

        assert_eq!(
            readiness.stage,
            shared::ParserReadinessStage::DiagnosticOnly
        );
        assert!(!readiness.production_enabled);
        assert!(readiness.self_check_available);
        assert!(readiness
            .checks
            .iter()
            .any(|check| check.code == "legacy_dom_path_ported"));
        assert!(readiness
            .checks
            .iter()
            .any(|check| check.code == "legacy_and_current_domains_probed"));
        assert!(readiness
            .checks
            .iter()
            .any(|check| check.code == "legacy_net_routes_return_placeholder_404"));
        assert!(readiness
            .checks
            .iter()
            .any(|check| check.code == "betm_official_domain_access_denied"));
        assert!(readiness
            .checks
            .iter()
            .any(|check| check.code == "public_feed_not_confirmed"));
        assert!(readiness
            .checks
            .iter()
            .any(|check| check.code == "product_target_unverified"));
    }
}
