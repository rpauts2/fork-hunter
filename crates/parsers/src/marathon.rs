use crate::base::{BookmakerParser, ParserResult};
use crate::factors_catalog::FactorsCatalog;
use async_trait::async_trait;
use chrono::Utc;
use reqwest::Client;
use shared::odds::OddsType;
use shared::{Event, Odd, Sport};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tracing::{debug, info, warn};

/// Marathon API — shared platform (*-resources.com, scopeMarket=3000)
/// Загружает факторы из каталога для динамического обнаружения рынков
#[derive(Debug)]
pub struct MarathonParser {
    http_client: Arc<Client>,
    live_url: String,
    prematch_url: String,
    factors: Arc<FactorsCatalog>,
}

impl MarathonParser {
    pub fn new(client: Arc<Client>) -> Self {
        Self {
            http_client: client.clone(),
            live_url: "https://line51.tf39be-resources.com/events/list?lang=ru&scopeMarket=3000"
                .to_string(),
            prematch_url:
                "https://line51.tf39be-resources.com/events/listBase?lang=ru&scopeMarket=3000"
                    .to_string(),
            factors: Arc::new(FactorsCatalog::new(
                client.clone(),
                "https://line51.tf39be-resources.com",
                3000,
            )),
        }
    }

    fn deduplicate_snapshot(events: Vec<Event>, odds: Vec<Odd>) -> (Vec<Event>, Vec<Odd>) {
        let mut seen_events = std::collections::HashSet::new();
        let unique_events = events
            .into_iter()
            .filter(|event| seen_events.insert(event.id.clone()))
            .collect::<Vec<_>>();

        let event_ids = unique_events
            .iter()
            .map(|event| event.id.as_str())
            .collect::<std::collections::HashSet<_>>();
        let mut seen_odds = std::collections::HashSet::new();
        let unique_odds = odds
            .into_iter()
            .filter(|odd| {
                event_ids.contains(odd.event_id.as_str()) && seen_odds.insert(odd.id.clone())
            })
            .collect::<Vec<_>>();

        (unique_events, unique_odds)
    }

    pub async fn load_factors(&self) -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
        self.factors.load().await
    }

    /// Fetch events and odds from the bookmaker's JSON feed.
    async fn fetch_via_api(
        &self,
        url: &str,
        is_live: bool,
    ) -> Result<Vec<(Event, Vec<Odd>)>, Box<dyn std::error::Error + Send + Sync>> {
        let response = self
            .http_client
            .get(url)
            .timeout(std::time::Duration::from_secs(30))
            .header(
                "User-Agent",
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36",
            )
            .header("Accept", "application/json, text/plain, */*")
            .header("Accept-Language", "ru-RU,ru;q=0.9")
            .header("Origin", "https://www.marathonbet.ru")
            .header("Referer", "https://www.marathonbet.ru/")
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let body_preview = response
                .text()
                .await
                .unwrap_or_default()
                .chars()
                .take(240)
                .collect::<String>();
            return Err(std::io::Error::other(format!(
                "Marathon API returned HTTP {status}: {body_preview}"
            ))
            .into());
        }

        let payload: serde_json::Value = response.json().await?;
        parse_api_response(&payload, is_live, "marathon", &self.factors)
    }
}

#[async_trait]
impl BookmakerParser for MarathonParser {
    fn name(&self) -> &str {
        "Marathon"
    }
    fn slug(&self) -> &str {
        "marathon"
    }
    fn is_enabled(&self) -> bool {
        true
    }

    async fn fetch_events(&self) -> Result<Vec<Event>, Box<dyn std::error::Error + Send + Sync>> {
        let mut all_events = Vec::new();
        let mut errors = Vec::new();

        for (url, is_live) in [(&self.live_url, true), (&self.prematch_url, false)] {
            match self.fetch_via_api(url, is_live).await {
                Ok(results) => all_events.extend(results.into_iter().map(|(event, _)| event)),
                Err(error) => {
                    warn!(error = %error, "Marathon fetch events failed");
                    errors.push(format!("{url}: {error}"));
                }
            }
        }

        if !errors.is_empty() {
            return Err(std::io::Error::other(errors.join("; ")).into());
        }
        info!(count = all_events.len(), "Marathon events parsed");
        Ok(all_events)
    }

    async fn fetch_odds(
        &self,
        _event_id: &str,
    ) -> Result<Vec<Odd>, Box<dyn std::error::Error + Send + Sync>> {
        let mut all_odds = Vec::new();
        let mut errors = Vec::new();

        for (url, is_live) in [(&self.live_url, true), (&self.prematch_url, false)] {
            match self.fetch_via_api(url, is_live).await {
                Ok(results) => all_odds.extend(results.into_iter().flat_map(|(_, odds)| odds)),
                Err(error) => {
                    warn!(error = %error, "Marathon fetch odds failed");
                    errors.push(format!("{url}: {error}"));
                }
            }
        }

        if !errors.is_empty() {
            return Err(std::io::Error::other(errors.join("; ")).into());
        }
        Ok(all_odds)
    }

    async fn fetch_all(&self) -> Result<ParserResult, Box<dyn std::error::Error + Send + Sync>> {
        // Загружаем каталог факторов
        match self.factors.load().await {
            Ok(count) => debug!(factor_count = count, "Factors catalog loaded for Marathon"),
            Err(e) => warn!(error = %e, "Failed to load factors catalog"),
        }

        let start = std::time::Instant::now();
        let mut all_events = Vec::new();
        let mut all_odds = Vec::new();
        let mut errors = Vec::new();

        for (url, is_live) in [(&self.live_url, true), (&self.prematch_url, false)] {
            match self.fetch_via_api(url, is_live).await {
                Ok(results) => {
                    for (event, odds) in results {
                        all_events.push(event);
                        all_odds.extend(odds);
                    }
                }
                Err(error) => {
                    warn!(error = %error, "Marathon fetch failed");
                    errors.push(format!("{url}: {error}"));
                }
            }
        }

        if !errors.is_empty() {
            return Err(std::io::Error::other(errors.join("; ")).into());
        }

        let (all_events, all_odds) = Self::deduplicate_snapshot(all_events, all_odds);
        let elapsed = start.elapsed().as_millis() as u64;
        debug!(
            events = all_events.len(),
            odds = all_odds.len(),
            time_ms = elapsed,
            "Marathon fetch complete"
        );
        Ok(ParserResult::new("marathon", all_events, all_odds, elapsed))
    }

    fn base_url(&self) -> &str {
        "https://www.marathonbet.ru"
    }
    fn user_agent(&self) -> &str {
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36"
    }
}

/// HTTP fallback — если headless Chrome недоступен
pub async fn fetch_platform_http(
    client: &Client,
    base_url: &str,
    is_live: bool,
    slug: &str,
    factors: &FactorsCatalog,
) -> Result<Vec<(Event, Vec<Odd>)>, Box<dyn std::error::Error + Send + Sync>> {
    let suffix = if is_live {
        "events/list"
    } else {
        "events/listBase"
    };
    let scope = match slug {
        "pari" => "2300",
        "marathon" => "3000",
        "bettery" => "501",
        "zenit" => "1300",
        _ => "3000",
    };
    let url = format!("{}/{}?lang=ru&scopeMarket={}", base_url, suffix, scope);

    debug!(url, "HTTP fetch");
    let resp = client.get(&url)
        .header("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36")
        .header("Accept", "application/json, text/plain, */*")
        .header("Accept-Language", "ru-RU,ru;q=0.9")
        .header("Accept-Encoding", "gzip, deflate, br")
        .header("Origin", format!("https://{}.ru", slug))
        .header("Referer", format!("https://{}.ru/", slug))
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await?;

    if !resp.status().is_success() {
        warn!(status = %resp.status(), slug, "HTTP fetch failed");
        return Ok(Vec::new());
    }

    // Get raw bytes and handle decompression manually
    let bytes = resp.bytes().await?;
    debug!(slug, bytes = bytes.len(), "HTTP response size");

    // Decompress if needed
    let json_bytes = if bytes.len() > 2 && bytes[0] == 0x1f && bytes[1] == 0x8b {
        // Gzip compressed
        use flate2::read::GzDecoder;
        use std::io::Read;
        let mut decoder = GzDecoder::new(&bytes[..]);
        let mut decompressed = Vec::new();
        if decoder.read_to_end(&mut decompressed).is_ok() {
            debug!(slug, decompressed = decompressed.len(), "Gzip decompressed");
            decompressed
        } else {
            bytes.to_vec()
        }
    } else {
        bytes.to_vec()
    };

    let json: serde_json::Value = match serde_json::from_slice(&json_bytes) {
        Ok(j) => j,
        Err(e) => {
            warn!(error = %e, slug, "JSON parse failed, first 200 bytes: {}", String::from_utf8_lossy(&json_bytes[..json_bytes.len().min(200)]));
            return Ok(Vec::new());
        }
    };

    parse_api_response(&json, is_live, slug, factors)
}

/// Парсит JSON ответ API shared platform
/// The shared line51 feed returns a combined event catalogue from both list and listBase.
/// `place` is the authoritative partition marker; the requested URL alone is not.
pub fn event_place_matches_feed(event_data: &serde_json::Value, is_live: bool) -> bool {
    match event_data
        .get("place")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("live") => is_live,
        Some("line") => !is_live,
        // `notActive`, unknown values, and missing markers are deliberately excluded.
        _ => false,
    }
}

pub fn parse_api_response(
    json: &serde_json::Value,
    is_live: bool,
    slug: &str,
    factors: &FactorsCatalog,
) -> Result<Vec<(Event, Vec<Odd>)>, Box<dyn std::error::Error + Send + Sync>> {
    let events = json
        .get("events")
        .and_then(|value| value.as_array())
        .ok_or_else(|| {
            std::io::Error::other("Marathon API response is missing the events array")
        })?;

    let custom_factors = json
        .get("customFactors")
        .and_then(|value| value.as_array())
        .ok_or_else(|| {
            std::io::Error::other("Marathon API response is missing the customFactors array")
        })?;

    // Shared Line51 payloads can omit event.name entirely. Resolve the source
    // sport and tournament from the bookmaker's own taxonomy instead of
    // defaulting otherwise valid events to Sport::Other / an empty league.
    let sports_by_id: HashMap<u64, &serde_json::Value> = json
        .get("sports")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|node| {
            node.get("id")
                .and_then(parse_catalog_id)
                .map(|id| (id, node))
        })
        .collect();
    let tournament_infos_by_id: HashMap<u64, &serde_json::Value> = json
        .get("tournamentInfos")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|info| {
            info.get("id")
                .and_then(parse_catalog_id)
                .map(|id| (id, info))
        })
        .collect();

    // Child catalogue rows (halves, corners, cards, etc.) carry their own
    // factor IDs but intentionally omit team names. Attach their factors to
    // the parent match instead of dropping them or creating synthetic events.
    let valid_parent_ids: HashSet<u64> = events
        .iter()
        .filter(|event| event_place_matches_feed(event, is_live))
        .filter_map(|event| {
            let id = event.get("id").and_then(parse_catalog_id)?;
            let team1 = event.get("team1").and_then(serde_json::Value::as_str)?;
            let team2 = event.get("team2").and_then(serde_json::Value::as_str)?;
            (!team1.trim().is_empty() && !team2.trim().is_empty()).then_some(id)
        })
        .collect();
    let child_to_parent: HashMap<u64, (u64, String)> = events
        .iter()
        .filter(|event| event_place_matches_feed(event, is_live))
        .filter_map(|event| {
            let child_id = event.get("id").and_then(parse_catalog_id)?;
            let has_own_participants = event
                .get("team1")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|team| !team.trim().is_empty())
                && event
                    .get("team2")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|team| !team.trim().is_empty());
            if has_own_participants {
                return None;
            }
            let parent_id = event.get("parentId").and_then(parse_catalog_id)?;
            if !valid_parent_ids.contains(&parent_id) {
                return None;
            }
            let context = event
                .get("name")
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(str::to_owned)
                .or_else(|| {
                    event
                        .get("kind")
                        .and_then(parse_catalog_id)
                        .map(|kind| format!("market_{kind}"))
                })?;
            Some((child_id, (parent_id, context)))
        })
        .collect();

    // Build factor lookup. For child rows, use the parent match as the key so
    // every market is emitted against the same canonical Event ID.
    let mut factor_map: HashMap<u64, Vec<(u64, f64, Option<f64>, Option<String>)>> = HashMap::new();
    for factor_entry in custom_factors {
        if let (Some(event_id), Some(factors_arr)) = (
            factor_entry.get("e").and_then(parse_catalog_id),
            factor_entry.get("factors").and_then(|f| f.as_array()),
        ) {
            let (canonical_event_id, market_context) = child_to_parent
                .get(&event_id)
                .map(|(parent_id, context)| (*parent_id, Some(context.clone())))
                .unwrap_or((event_id, None));
            for f in factors_arr {
                if let (Some(fid), Some(fval)) = (
                    f.get("f").and_then(parse_catalog_id),
                    f.get("v").and_then(parse_catalog_float),
                ) {
                    // Shared-platform payload expresses line values in hundredths:
                    // p=150 represents 1.5, p=-75 represents -0.75.
                    let line = f
                        .get("p")
                        .and_then(parse_catalog_float)
                        .or_else(|| f.get("pt").and_then(parse_catalog_float))
                        .map(|value| value / 100.0);
                    factor_map.entry(canonical_event_id).or_default().push((
                        fid,
                        fval,
                        line,
                        market_context.clone(),
                    ));
                }
            }
        }
    }

    let mut results = Vec::new();

    for event_data in events {
        if !event_place_matches_feed(event_data, is_live) {
            continue;
        }

        let (Some(event_id), Some(team1), Some(team2)) = (
            event_data.get("id").and_then(parse_catalog_id),
            event_data.get("team1").and_then(|t| t.as_str()),
            event_data.get("team2").and_then(|t| t.as_str()),
        ) else {
            continue;
        };

        if team1.is_empty() || team2.is_empty() {
            continue;
        }

        let event_name = event_data
            .get("name")
            .and_then(|t| t.as_str())
            .unwrap_or("");
        let source_sport_id = event_data.get("sportId").and_then(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .or_else(|| value.as_u64().map(|id| id.to_string()))
        });
        let source_sport_numeric_id = event_data.get("sportId").and_then(parse_catalog_id);
        let (catalog_sport, catalog_league) = resolve_source_taxonomy(
            source_sport_numeric_id,
            &sports_by_id,
            &tournament_infos_by_id,
        );
        let name_sport = detect_sport(event_name);
        let sport = if name_sport != Sport::Other {
            name_sport
        } else {
            catalog_sport
        };
        let name_league = extract_league(event_name);
        let league = if event_name.contains(':') && !name_league.trim().is_empty() {
            name_league
        } else {
            catalog_league.unwrap_or(name_league)
        };
        let mut extra = HashMap::new();
        if let Some(source_sport_id) = &source_sport_id {
            extra.insert(
                "source_sport_id".to_string(),
                serde_json::Value::String(source_sport_id.clone()),
            );
        }

        let event = Event {
            id: format!(
                "{}-{}-{}",
                slug,
                if is_live { "live" } else { "prematch" },
                event_id
            ),
            sport,
            league,
            home_team: team1.to_string(),
            away_team: team2.to_string(),
            start_time: None,
            is_live,
            bookmaker_slug: slug.to_string(),
            raw_url: None,
            extra,
        };

        let mut odds = Vec::new();
        if let Some(factors_list) = factor_map.get(&event_id) {
            let now = Utc::now();
            for (fid, val, line, market_context) in factors_list.iter().cloned() {
                if val <= 1.0 {
                    continue;
                }

                let odds_before = odds.len();
                let catalog_knows_factor = factors.contains_factor_id(fid);
                let mut classified = false;

                if catalog_knows_factor {
                    if let Some(factor_def) =
                        factors.get_factor_for_sport(fid, source_sport_id.as_deref())
                    {
                        let mt = factor_def.market_type.to_lowercase();
                        let sel = factor_def.selection_name.to_lowercase();
                        classified = classify_and_add_odd(
                            &mut odds, slug, &event, fid, val, line, &mt, &sel, now,
                        );
                    }

                    // A catalog-known ID may be ambiguous for this sport or may
                    // describe a market/outcome this classifier cannot safely
                    // represent. Never reinterpret it using a numeric-ID guess.
                    if !classified {
                        debug!(
                            bookmaker = slug,
                            event_id,
                            factor_id = fid,
                            source_sport_id = ?source_sport_id,
                            "Quarantined catalog-known factor without a safe classification"
                        );
                    }
                } else {
                    // Legacy mappings are allowed only for IDs absent from the
                    // direct catalog. This prevents a catalog definition that
                    // conflicts with a historical numeric mapping from becoming
                    // a false cross-book market.
                    match fid {
                        921 => odds.push(make_odd(
                            slug,
                            &event,
                            "1X2",
                            "1",
                            val,
                            None,
                            OddsType::Home,
                            now,
                        )),
                        922 => odds.push(make_odd(
                            slug,
                            &event,
                            "1X2",
                            "X",
                            val,
                            None,
                            OddsType::Draw,
                            now,
                        )),
                        923 => odds.push(make_odd(
                            slug,
                            &event,
                            "1X2",
                            "2",
                            val,
                            None,
                            OddsType::Away,
                            now,
                        )),
                        924 | 930 | 1002 | 1010 | 1054 => {
                            if let Some(l) = line {
                                odds.push(make_odd(
                                    slug,
                                    &event,
                                    "Total",
                                    "Over",
                                    val,
                                    Some(l),
                                    OddsType::Over,
                                    now,
                                ));
                            }
                        }
                        925 | 931 | 1003 | 1011 | 1055 => {
                            if let Some(l) = line {
                                odds.push(make_odd(
                                    slug,
                                    &event,
                                    "Total",
                                    "Under",
                                    val,
                                    Some(l),
                                    OddsType::Under,
                                    now,
                                ));
                            }
                        }
                        910 | 937 | 989 | 1004 | 1006 | 1012 | 1569 | 1672 | 1677 | 1680 | 1683
                        | 1686 | 1689 | 1692 | 1723 => {
                            if let Some(l) = line {
                                odds.push(make_odd(
                                    slug,
                                    &event,
                                    "Handicap",
                                    "1",
                                    val,
                                    Some(l),
                                    OddsType::Handicap,
                                    now,
                                ));
                            }
                        }
                        912 | 938 | 991 | 1005 | 1013 | 1572 | 1675 | 1678 | 1681 | 1684 | 1687
                        | 1690 | 1718 | 1724 => {
                            if let Some(l) = line {
                                odds.push(make_odd(
                                    slug,
                                    &event,
                                    "Handicap",
                                    "2",
                                    val,
                                    Some(l),
                                    OddsType::Handicap,
                                    now,
                                ));
                            }
                        }
                        926 => odds.push(make_odd(
                            slug,
                            &event,
                            "BothTeamsScore",
                            "Yes",
                            val,
                            None,
                            OddsType::BothTeamsScoreYes,
                            now,
                        )),
                        // 927/928 are source handicap factor IDs (team 1/team 2),
                        // not generic BothTeamsScore/EvenOdd outcomes.
                        929 => odds.push(make_odd(
                            slug,
                            &event,
                            "EvenOdd",
                            "Odd",
                            val,
                            None,
                            OddsType::Odd,
                            now,
                        )),
                        _ => {}
                    }
                }

                // A child row represents a distinct market family on the same
                // match. Namespace the market before constructing its stable ID
                // so e.g. match 1X2 and corners 1X2 cannot collide.
                if let Some(context) = market_context {
                    for odd in &mut odds[odds_before..] {
                        let market = format!("{context}: {}", odd.market);
                        *odd = make_odd(
                            slug,
                            &event,
                            &market,
                            &odd.selection,
                            odd.odds,
                            odd.line,
                            odd.odds_type,
                            odd.timestamp,
                        );
                    }
                }
            }
        }

        let (odds, collapsed_duplicates) = deduplicate_same_market_quotes(odds);
        if collapsed_duplicates > 0 {
            tracing::warn!(
                bookmaker = slug,
                event_id = %event.id,
                collapsed_duplicates,
                "Collapsed duplicate source quotes for identical market/selection/line keys; retained the best price"
            );
        }
        results.push((event, odds));
    }

    Ok(results)
}

fn canonical_total_market(market_type: &str) -> &'static str {
    match market_type {
        "first_half_total" => "1H_Total",
        "second_half_total" => "2H_Total",
        "first_half_team1_total" => "1H_Team1Total",
        "second_half_team1_total" => "2H_Team1Total",
        "first_half_team2_total" => "1H_Team2Total",
        "second_half_team2_total" => "2H_Team2Total",
        "team1_total" => "Team1Total",
        "team2_total" => "Team2Total",
        "sets_total" => "SetsTotal",
        "maps_total" => "MapsTotal",
        "series_total" => "SeriesTotal",
        _ => "Total",
    }
}

fn canonical_handicap_market(market_type: &str) -> &'static str {
    match market_type {
        "first_half_handicap" => "1H_Handicap",
        "second_half_handicap" => "2H_Handicap",
        "sets_handicap" => "SetsHandicap",
        "maps_handicap" => "MapsHandicap",
        _ => "Handicap",
    }
}

fn canonical_result_market(market_type: &str) -> &'static str {
    match market_type {
        "first_half_1x2" => "1H_Result",
        "second_half_1x2" => "2H_Result",
        "first_half_to_win" => "1H_Winner",
        "second_half_to_win" => "2H_Winner",
        "first_half_final_win" => "1H_FinalWin",
        "second_half_final_win" => "2H_FinalWin",
        "final_win" => "FinalWin",
        "to_win" => "Winner",
        _ => "1X2",
    }
}

fn canonical_btts_market(market_type: &str) -> &'static str {
    match market_type {
        "first_half_both_teams_score" => "1H_BothTeamsScore",
        "second_half_both_teams_score" => "2H_BothTeamsScore",
        _ => "BothTeamsScore",
    }
}

fn canonical_even_odd_market(market_type: &str) -> &'static str {
    match market_type {
        "first_half_even_odd" => "1H_EvenOdd",
        "second_half_even_odd" => "2H_EvenOdd",
        _ => "EvenOdd",
    }
}

fn canonical_double_chance_market(market_type: &str) -> &'static str {
    match market_type {
        "first_half_double_chance" => "1H_DoubleChance",
        "second_half_double_chance" => "2H_DoubleChance",
        _ => "DoubleChance",
    }
}

/// Reuse the source-catalog classifier from other direct parsers that share
/// the bookmaker line platform. Returns no quote for unknown/ambiguous semantics.
pub(crate) fn classify_catalog_factor(
    slug: &str,
    event: &Event,
    factor_id: u64,
    odds_value: f64,
    line: Option<f64>,
    market_type: &str,
    selection: &str,
    now: chrono::DateTime<Utc>,
) -> Option<Odd> {
    let mut classified = Vec::with_capacity(1);
    classify_and_add_odd(
        &mut classified,
        slug,
        event,
        factor_id,
        odds_value,
        line,
        market_type,
        selection,
        now,
    )
    .then(|| classified.into_iter().next())
    .flatten()
}

fn normalized_participant_key(value: &str) -> String {
    value
        .to_lowercase()
        .chars()
        .filter(|character| character.is_alphanumeric())
        .collect()
}

fn classify_and_add_odd(
    odds: &mut Vec<Odd>,
    slug: &str,
    event: &Event,
    _fid: u64,
    val: f64,
    line: Option<f64>,
    market_type: &str,
    selection: &str,
    now: chrono::DateTime<Utc>,
) -> bool {
    if market_type.contains("1x2")
        || market_type.contains("исход")
        || market_type.contains("winner")
        || market_type.contains("to_win")
        || market_type.contains("final_win")
    {
        // A broad Cyrillic substring check such as contains("х") matches
        // "хозяева" and can turn a home-win quote into a draw. Accept only
        // unambiguous canonical outcomes or explicit bookmaker synonyms.
        let selection_lower = selection.trim().to_lowercase();
        let selection_key = normalized_participant_key(selection);
        let home_key = normalized_participant_key(&event.home_team);
        let away_key = normalized_participant_key(&event.away_team);
        let canonical_selection = if !selection_key.is_empty()
            && !home_key.is_empty()
            && home_key != away_key
            && selection_key == home_key
        {
            "1"
        } else if !selection_key.is_empty()
            && !away_key.is_empty()
            && home_key != away_key
            && selection_key == away_key
        {
            "2"
        } else {
            match selection_lower.as_str() {
                "1"
                | "п1"
                | "home"
                | "home win"
                | "хозяева"
                | "победа хозяев"
                | "победа 1"
                | "команда 1"
                | "первая команда" => "1",
                "x" | "х" | "draw" | "tie" | "ничья" => "X",
                "2"
                | "п2"
                | "away"
                | "away win"
                | "гости"
                | "победа гостей"
                | "победа 2"
                | "команда 2"
                | "вторая команда" => "2",
                _ => return false,
            }
        };
        let (selection, odds_type) = match canonical_selection {
            "1" => ("1", OddsType::Home),
            "X" => ("X", OddsType::Draw),
            "2" => ("2", OddsType::Away),
            _ => return false,
        };
        odds.push(make_odd(
            slug,
            event,
            canonical_result_market(market_type),
            selection,
            val,
            None,
            odds_type,
            now,
        ));
        return true;
    } else if market_type.contains("total") || market_type.contains("тотал") {
        if let Some(l) = line {
            if selection == "б"
                || selection.contains("больше")
                || selection.contains("over")
                || selection.contains("тб")
            {
                let market = canonical_total_market(market_type);
                odds.push(make_odd(
                    slug,
                    event,
                    market,
                    "Over",
                    val,
                    Some(l),
                    OddsType::Over,
                    now,
                ));
                return true;
            } else if selection == "м"
                || selection.contains("меньше")
                || selection.contains("under")
                || selection.contains("тм")
            {
                let market = canonical_total_market(market_type);
                odds.push(make_odd(
                    slug,
                    event,
                    market,
                    "Under",
                    val,
                    Some(l),
                    OddsType::Under,
                    now,
                ));
                return true;
            }
        }
    } else if market_type.contains("handicap") || market_type.contains("фора") {
        if let Some(l) = line {
            // Handicap sign is not the selected side. The source outcome label
            // determines whether the quote belongs to team 1 or team 2.
            let selection_lower = selection.to_lowercase();
            let sel = if selection_lower == "1"
                || selection_lower.contains("п1")
                || selection_lower.contains("фора 1")
                || selection_lower.contains("фора1")
                || selection_lower.contains("home")
                || selection_lower.contains("team 1")
                || selection_lower.contains("команда 1")
            {
                "1"
            } else if selection_lower == "2"
                || selection_lower.contains("п2")
                || selection_lower.contains("фора 2")
                || selection_lower.contains("фора2")
                || selection_lower.contains("away")
                || selection_lower.contains("team 2")
                || selection_lower.contains("команда 2")
            {
                "2"
            } else {
                return false;
            };
            odds.push(make_odd(
                slug,
                event,
                canonical_handicap_market(market_type),
                sel,
                val,
                Some(l),
                OddsType::Handicap,
                now,
            ));
            return true;
        }
    } else if market_type.contains("head_to_head") {
        odds.push(make_odd(
            slug,
            event,
            "HeadToHead",
            selection,
            val,
            line,
            OddsType::Custom,
            now,
        ));
        return true;
    } else if market_type.contains("to_qualify") {
        odds.push(make_odd(
            slug,
            event,
            "ToQualify",
            selection,
            val,
            line,
            OddsType::Custom,
            now,
        ));
        return true;
    } else if market_type.contains("both")
        || market_type.contains("обе")
        || market_type.contains("oz")
        || market_type.contains("btts")
    {
        // Keep a strict two-way mapping; substring matches such as "да" in
        // unrelated captions (for example "команда") are not outcome evidence.
        match selection.trim().to_lowercase().as_str() {
            "да" | "yes" | "y" => odds.push(make_odd(
                slug,
                event,
                canonical_btts_market(market_type),
                "Yes",
                val,
                None,
                OddsType::BothTeamsScoreYes,
                now,
            )),
            "нет" | "no" | "n" => odds.push(make_odd(
                slug,
                event,
                canonical_btts_market(market_type),
                "No",
                val,
                None,
                OddsType::BothTeamsScoreNo,
                now,
            )),
            _ => return false,
        }
        return true;
    } else if market_type.contains("even")
        || market_type.contains("odd")
        || market_type.contains("чёт")
        || market_type.contains("чет")
    {
        // Check the negative form first: "нечёт" contains "чёт".
        let selection_lower = selection.trim().to_lowercase();
        if selection_lower.contains("нечёт")
            || selection_lower.contains("нечет")
            || selection_lower == "odd"
        {
            odds.push(make_odd(
                slug,
                event,
                canonical_even_odd_market(market_type),
                "Odd",
                val,
                None,
                OddsType::Odd,
                now,
            ));
            return true;
        } else if selection_lower.contains("чёт")
            || selection_lower.contains("чет")
            || selection_lower == "even"
        {
            odds.push(make_odd(
                slug,
                event,
                canonical_even_odd_market(market_type),
                "Even",
                val,
                None,
                OddsType::Even,
                now,
            ));
            return true;
        }
    } else if market_type.contains("double") || market_type.contains("двойн") {
        // Only canonical double-chance outcomes are actionable. Unknown
        // captions must not leak into cross-book market comparison.
        let normalized_selection = selection
            .trim()
            .to_lowercase()
            .replace('х', "x")
            .replace(' ', "")
            .replace('-', "");
        let canonical_selection = match normalized_selection.as_str() {
            "1x" => "1X",
            "x2" => "X2",
            "12" => "12",
            _ => return false,
        };
        odds.push(make_odd(
            slug,
            event,
            canonical_double_chance_market(market_type),
            canonical_selection,
            val,
            None,
            OddsType::Custom,
            now,
        ));
        return true;
    } else if market_type.contains("correct")
        || market_type.contains("точн")
        || market_type.contains("score")
    {
        odds.push(make_odd(
            slug,
            event,
            "CorrectScore",
            selection,
            val,
            None,
            OddsType::Custom,
            now,
        ));
        return true;
    }
    false
}

fn parse_catalog_id(value: &serde_json::Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_str().and_then(|text| text.trim().parse().ok()))
}

fn parse_catalog_float(value: &serde_json::Value) -> Option<f64> {
    let parsed = value.as_f64().or_else(|| {
        value
            .as_str()
            .and_then(|text| text.trim().replace(',', ".").parse::<f64>().ok())
    })?;
    parsed.is_finite().then_some(parsed)
}

fn resolve_source_taxonomy(
    source_sport_id: Option<u64>,
    sports_by_id: &HashMap<u64, &serde_json::Value>,
    tournament_infos_by_id: &HashMap<u64, &serde_json::Value>,
) -> (Sport, Option<String>) {
    let Some(mut node_id) = source_sport_id else {
        return (Sport::Other, None);
    };

    let mut sport = Sport::Other;
    let mut league = None;
    let mut segment_name = None;
    let mut visited = HashSet::new();

    while visited.insert(node_id) {
        let Some(node) = sports_by_id.get(&node_id).copied() else {
            break;
        };

        if league.is_none() {
            league = node
                .get("tournamentInfoId")
                .and_then(parse_catalog_id)
                .and_then(|id| tournament_infos_by_id.get(&id).copied())
                .and_then(|info| info.get("caption"))
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|caption| !caption.is_empty())
                .map(str::to_owned);
        }

        let node_kind = node
            .get("kind")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        if segment_name.is_none() && !node_kind.eq_ignore_ascii_case("sport") {
            segment_name = node
                .get("name")
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(str::to_owned);
        }

        if node_kind.eq_ignore_ascii_case("sport") {
            sport = node
                .get("name")
                .and_then(serde_json::Value::as_str)
                .map(Sport::from_str)
                .unwrap_or(Sport::Other);
            break;
        }

        let Some(parent_id) = node.get("parentId").and_then(parse_catalog_id) else {
            break;
        };
        node_id = parent_id;
    }

    (sport, league.or(segment_name))
}

fn detect_sport(event_name: &str) -> Sport {
    let sport_label = event_name
        .split_once(':')
        .map(|(sport, _)| sport.trim())
        .unwrap_or(event_name.trim());
    let detected = Sport::from_str(sport_label);
    if detected != Sport::Other {
        return detected;
    }

    let name = event_name.to_lowercase();
    if name.contains("настольн") || name.contains("table tennis") {
        Sport::TableTennis
    } else if name.contains("футзал") || name.contains("futsal") || name.contains("мини-футбол")
    {
        Sport::Futsal
    } else if name.contains("киберспорт") || name.contains("esport") {
        Sport::Esports
    } else {
        Sport::Other
    }
}

pub(crate) fn extract_league(event_name: &str) -> String {
    event_name
        .split_once(':')
        .map(|(_, league)| league.trim())
        .filter(|league| !league.is_empty())
        .unwrap_or_else(|| event_name.trim())
        .to_string()
}

fn id_component(value: &str) -> String {
    let mut result = String::new();
    let mut separator_pending = false;

    for ch in value.chars() {
        if ch.is_alphanumeric() {
            if separator_pending && !result.is_empty() {
                result.push('_');
            }
            separator_pending = false;
            result.extend(ch.to_lowercase());
        } else {
            separator_pending = true;
        }
    }

    if result.is_empty() {
        "unknown".to_string()
    } else {
        result
    }
}

/// A bookmaker feed can publish multiple factor IDs for the same semantic
/// market/selection/line (e.g. duplicated parameter rows). Those are one
/// actionable quote key, not separate opportunities. Keep the best price for
/// the bettor and preserve a stable output order.
pub(crate) fn deduplicate_same_market_quotes(odds: Vec<Odd>) -> (Vec<Odd>, usize) {
    let mut positions: HashMap<String, usize> = HashMap::with_capacity(odds.len());
    let mut unique: Vec<Odd> = Vec::with_capacity(odds.len());
    let mut duplicates = 0usize;

    for odd in odds {
        if let Some(existing_index) = positions.get(&odd.id).copied() {
            duplicates += 1;
            if odd.odds > unique[existing_index].odds {
                unique[existing_index] = odd;
            }
        } else {
            positions.insert(odd.id.clone(), unique.len());
            unique.push(odd);
        }
    }

    (unique, duplicates)
}

pub(crate) fn make_market_odd(
    slug: &str,
    event: &Event,
    market: &str,
    selection: &str,
    odds: f64,
    line: Option<f64>,
    odds_type: OddsType,
    timestamp: chrono::DateTime<Utc>,
) -> Odd {
    make_odd(
        slug, event, market, selection, odds, line, odds_type, timestamp,
    )
}

fn make_odd(
    slug: &str,
    event: &Event,
    market: &str,
    selection: &str,
    odds: f64,
    line: Option<f64>,
    odds_type: OddsType,
    timestamp: chrono::DateTime<Utc>,
) -> Odd {
    let line_key = line
        .map(|value| format!("{value:.6}").replace('-', "m").replace('.', "p"))
        .unwrap_or_else(|| "no-line".to_string());

    Odd {
        // Selection alone is not unique: "1" can exist in 1X2 and handicap,
        // while Over/Under repeat across total lines and periods.
        id: format!(
            "{}-{}-{}-{}-{}",
            slug,
            event.id,
            id_component(market),
            id_component(selection),
            line_key
        ),
        event_id: event.id.clone(),
        bookmaker_slug: slug.to_string(),
        market: market.into(),
        selection: selection.into(),
        odds,
        odds_type,
        line,
        timestamp,
    }
}

#[cfg(test)]
mod parser_identity_tests {
    use super::{
        classify_and_add_odd, classify_catalog_factor, deduplicate_same_market_quotes,
        detect_sport, extract_league, make_odd, parse_api_response,
    };
    use crate::factors_catalog::FactorsCatalog;
    use shared::Sport;
    use std::sync::Arc;

    #[test]
    fn shared_platform_resolves_missing_event_name_from_source_taxonomy() {
        let client = Arc::new(reqwest::Client::builder().build().expect("client"));
        let factors = FactorsCatalog::new(client, "https://line51.tf39be-resources.com", 3000);
        let payload = serde_json::json!({
            "sports": [
                {
                    "id": 29086,
                    "kind": "sport",
                    "name": "Киберспорт"
                },
                {
                    "id": 117354,
                    "parentId": 29086,
                    "kind": "segment",
                    "name": "Dota 2. BLAST Slam. Bo3. Сравнение по киллам",
                    "tournamentInfoId": 1425
                },
                {
                    "id": 11634,
                    "kind": "sport",
                    "name": "Крикет"
                },
                {
                    "id": 148686,
                    "parentId": 11634,
                    "kind": "segment",
                    "name": "T20. Индия. Лига Ньюлендс"
                }
            ],
            "tournamentInfos": [
                {"id": 1425, "caption": "Dota 2. BLAST Slam"}
            ],
            "events": [
                {
                    "id": 70279601,
                    "team1": "watson (Team Yandex)",
                    "team2": "Yuma (GamerLegion)",
                    "place": "live",
                    "sportId": 117354,
                    "name": ""
                },
                {
                    "id": 70282490,
                    "team1": "XI Хайваан",
                    "team2": "Гьярах Шайтан",
                    "place": "live",
                    "sportId": 148686,
                    "name": ""
                }
            ],
            "customFactors": []
        });

        let parsed = parse_api_response(&payload, true, "marathon", &factors)
            .expect("valid source-taxonomy fixture");
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].0.sport, Sport::Esports);
        assert_eq!(parsed[0].0.league, "Dota 2. BLAST Slam");
        assert_eq!(parsed[1].0.sport, Sport::Cricket);
        assert_eq!(parsed[1].0.league, "T20. Индия. Лига Ньюлендс");
        assert_eq!(
            parsed[0]
                .0
                .extra
                .get("source_sport_id")
                .and_then(serde_json::Value::as_str),
            Some("117354")
        );
    }

    #[test]
    fn shared_platform_normalizes_lines_sport_and_competition_name() {
        let client = Arc::new(reqwest::Client::builder().build().expect("client"));
        let factors = FactorsCatalog::new(client, "https://line51.tf39be-resources.com", 3000);
        let payload = serde_json::json!({
            "events": [{
                "id": 12345,
                "team1": "Home FC",
                "team2": "Away FC",
                "place": "line",
                "name": "Football: Premier League"
            }],
            "customFactors": [{
                "e": 12345,
                "factors": [
                    {"f": 924, "v": 1.80, "p": 150},
                    {"f": 925, "v": 2.00, "p": 250},
                    {"f": 1004, "v": 1.91, "p": -75}
                ]
            }]
        });

        let parsed = parse_api_response(&payload, false, "marathon", &factors)
            .expect("valid shared-platform fixture");
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].0.sport, Sport::Football);
        assert_eq!(parsed[0].0.league, "Premier League");
        assert!(parsed[0].1.iter().any(|odd| odd.line == Some(1.5)));
        assert!(parsed[0].1.iter().any(|odd| odd.line == Some(2.5)));
        assert!(parsed[0].1.iter().any(|odd| odd.line == Some(-0.75)));
        assert!(parsed[0].1.iter().any(|odd| {
            odd.market == "Handicap" && odd.selection == "1" && odd.line == Some(-0.75)
        }));
        let mut catalog_handicap_odds = Vec::new();
        assert!(classify_and_add_odd(
            &mut catalog_handicap_odds,
            "marathon",
            &parsed[0].0,
            1004,
            1.91,
            Some(-0.75),
            "handicap",
            "1",
            chrono::Utc::now(),
        ));
        assert_eq!(catalog_handicap_odds[0].selection, "1");
        assert_eq!(catalog_handicap_odds[0].line, Some(-0.75));
        assert_eq!(detect_sport("Tennis: ATP"), Sport::Tennis);
        assert_eq!(detect_sport("Unknown sport: event"), Sport::Other);
        assert_eq!(extract_league("Football: Premier League"), "Premier League");
        assert_eq!(extract_league("Standalone Cup"), "Standalone Cup");
    }

    #[test]
    fn shared_platform_accepts_string_factor_ids_prices_and_line_values() {
        let client = Arc::new(reqwest::Client::builder().build().expect("client"));
        let factors = FactorsCatalog::new(client, "https://line51.tf39be-resources.com", 3000);
        let payload = serde_json::json!({
            "events": [{
                "id": "12345",
                "team1": "Home FC",
                "team2": "Away FC",
                "place": "line",
                "sportId": "1",
                "name": "Football: Premier League"
            }],
            "customFactors": [{
                "e": "12345",
                "factors": [
                    {"f": "924", "v": " 1,80 ", "p": "150"},
                    {"f": "925", "v": "2,00", "pt": "250"}
                ]
            }]
        });

        let parsed = parse_api_response(&payload, false, "marathon", &factors)
            .expect("numeric-string factor payload should parse");
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].0.id, "marathon-prematch-12345");
        assert_eq!(parsed[0].1.len(), 2);
        assert!(parsed[0].1.iter().any(|odd| {
            odd.market == "Total"
                && odd.selection == "Over"
                && odd.odds == 1.8
                && odd.line == Some(1.5)
        }));
        assert!(parsed[0].1.iter().any(|odd| {
            odd.market == "Total"
                && odd.selection == "Under"
                && odd.odds == 2.0
                && odd.line == Some(2.5)
        }));
    }

    #[test]
    fn shared_platform_uses_explicit_result_labels_and_rejects_unknowns() {
        use chrono::Utc;
        use shared::{Event, OddsType};
        use std::collections::HashMap;

        let event = Event {
            id: "marathon-pre-result-fixture".into(),
            sport: Sport::Football,
            league: "League".into(),
            home_team: "Home".into(),
            away_team: "Away".into(),
            start_time: None,
            is_live: false,
            bookmaker_slug: "marathon".into(),
            raw_url: None,
            extra: HashMap::new(),
        };
        let mut odds = Vec::new();
        assert!(classify_and_add_odd(
            &mut odds,
            "marathon",
            &event,
            921,
            1.80,
            None,
            "1x2",
            "Хозяева",
            Utc::now(),
        ));
        assert_eq!(odds[0].selection, "1");
        assert_eq!(odds[0].odds_type, OddsType::Home);

        assert!(!classify_and_add_odd(
            &mut odds,
            "marathon",
            &event,
            921,
            1.80,
            None,
            "1x2",
            "неизвестный исход",
            Utc::now(),
        ));
        assert_eq!(odds.len(), 1);
    }

    #[test]
    fn shared_platform_resolves_named_winner_outcomes_to_event_participants() {
        use chrono::Utc;
        use shared::{Event, OddsType, Sport};
        use std::collections::HashMap;

        let event = Event {
            id: "marathon-pre-tennis-winner-fixture".into(),
            sport: Sport::Tennis,
            league: "ATP".into(),
            home_team: "Player One".into(),
            away_team: "Player Two".into(),
            start_time: None,
            is_live: false,
            bookmaker_slug: "marathon".into(),
            raw_url: None,
            extra: HashMap::new(),
        };
        let mut odds = Vec::new();
        assert!(classify_and_add_odd(
            &mut odds,
            "marathon",
            &event,
            5001,
            1.75,
            None,
            "to_win",
            "PLAYER-ONE",
            Utc::now(),
        ));
        assert_eq!(odds[0].market, "Winner");
        assert_eq!(odds[0].selection, "1");
        assert_eq!(odds[0].odds_type, OddsType::Home);

        assert!(classify_and_add_odd(
            &mut odds,
            "marathon",
            &event,
            5002,
            2.10,
            None,
            "to_win",
            "Player Two",
            Utc::now(),
        ));
        assert_eq!(odds[1].market, "Winner");
        assert_eq!(odds[1].selection, "2");
        assert_eq!(odds[1].odds_type, OddsType::Away);
    }

    #[test]
    fn shared_platform_rejects_ambiguous_btts_captions() {
        use chrono::Utc;
        use shared::Event;
        use std::collections::HashMap;

        let event = Event {
            id: "marathon-pre-btts-fixture".into(),
            sport: Sport::Football,
            league: "League".into(),
            home_team: "Home".into(),
            away_team: "Away".into(),
            start_time: None,
            is_live: false,
            bookmaker_slug: "marathon".into(),
            raw_url: None,
            extra: HashMap::new(),
        };
        let mut odds = Vec::new();
        assert!(!classify_and_add_odd(
            &mut odds,
            "marathon",
            &event,
            926,
            1.90,
            None,
            "both_teams_score",
            "команда",
            Utc::now(),
        ));
        assert!(odds.is_empty());
    }

    #[test]
    fn shared_platform_classifies_even_odd_without_prefix_collision() {
        use chrono::Utc;
        use shared::{Event, OddsType};
        use std::collections::HashMap;

        let event = Event {
            id: "marathon-pre-odd-fixture".into(),
            sport: Sport::Football,
            league: "League".into(),
            home_team: "Home".into(),
            away_team: "Away".into(),
            start_time: None,
            is_live: false,
            bookmaker_slug: "marathon".into(),
            raw_url: None,
            extra: HashMap::new(),
        };
        let mut odds = Vec::new();
        assert!(classify_and_add_odd(
            &mut odds,
            "marathon",
            &event,
            929,
            1.91,
            None,
            "even_odd",
            "odd",
            Utc::now(),
        ));
        assert_eq!(odds.len(), 1);
        assert_eq!(odds[0].market, "EvenOdd");
        assert_eq!(odds[0].selection, "Odd");
        assert_eq!(odds[0].odds_type, OddsType::Odd);
    }

    #[test]
    fn shared_platform_classifies_cyrillic_odd_before_even_substring() {
        use chrono::Utc;
        use shared::{Event, OddsType};
        use std::collections::HashMap;

        let event = Event {
            id: "marathon-pre-odd-cyrillic-fixture".into(),
            sport: Sport::Football,
            league: "League".into(),
            home_team: "Home".into(),
            away_team: "Away".into(),
            start_time: None,
            is_live: false,
            bookmaker_slug: "marathon".into(),
            raw_url: None,
            extra: HashMap::new(),
        };
        let mut odds = Vec::new();
        assert!(classify_and_add_odd(
            &mut odds,
            "marathon",
            &event,
            929,
            1.91,
            None,
            "even_odd",
            "Нечёт",
            Utc::now(),
        ));
        assert_eq!(odds.len(), 1);
        assert_eq!(odds[0].selection, "Odd");
        assert_eq!(odds[0].odds_type, OddsType::Odd);
    }

    #[test]
    fn shared_platform_rejects_unknown_double_chance_outcomes() {
        use chrono::Utc;
        use shared::{Event, Sport};
        use std::collections::HashMap;

        let event = Event {
            id: "marathon-pre-double-chance-fixture".into(),
            sport: Sport::Football,
            league: "League".into(),
            home_team: "Home".into(),
            away_team: "Away".into(),
            start_time: None,
            is_live: false,
            bookmaker_slug: "marathon".into(),
            raw_url: None,
            extra: HashMap::new(),
        };
        let mut odds = Vec::new();
        assert!(!classify_and_add_odd(
            &mut odds,
            "marathon",
            &event,
            1014,
            2.10,
            None,
            "double_chance",
            "home wins or over 2.5",
            Utc::now(),
        ));
        assert!(odds.is_empty());
    }

    #[test]
    fn shared_platform_normalizes_cyrillic_double_chance_outcomes() {
        use chrono::Utc;
        use shared::{Event, OddsType, Sport};
        use std::collections::HashMap;

        let event = Event {
            id: "marathon-pre-double-chance-cyrillic-fixture".into(),
            sport: Sport::Football,
            league: "League".into(),
            home_team: "Home".into(),
            away_team: "Away".into(),
            start_time: None,
            is_live: false,
            bookmaker_slug: "marathon".into(),
            raw_url: None,
            extra: HashMap::new(),
        };
        let mut odds = Vec::new();
        assert!(classify_and_add_odd(
            &mut odds,
            "marathon",
            &event,
            1014,
            1.62,
            None,
            "double_chance",
            "1Х",
            Utc::now(),
        ));
        assert_eq!(odds.len(), 1);
        assert_eq!(odds[0].selection, "1X");
        assert_eq!(odds[0].odds_type, OddsType::Custom);
    }

    #[test]
    fn duplicate_semantic_quotes_keep_the_best_price_and_one_stable_id() {
        use chrono::Utc;
        use shared::{Event, OddsType, Sport};
        use std::collections::HashMap;

        let event = Event {
            id: "bettery-prematch-69681909".into(),
            sport: Sport::Football,
            league: "League".into(),
            home_team: "Home".into(),
            away_team: "Away".into(),
            start_time: None,
            is_live: false,
            bookmaker_slug: "bettery".into(),
            raw_url: None,
            extra: HashMap::new(),
        };
        let now = Utc::now();
        let odds = vec![
            make_odd(
                "bettery",
                &event,
                "Team1Total",
                "Over",
                2.15,
                Some(3.0),
                OddsType::Over,
                now,
            ),
            make_odd(
                "bettery",
                &event,
                "Team1Total",
                "Under",
                1.65,
                Some(3.0),
                OddsType::Under,
                now,
            ),
            make_odd(
                "bettery",
                &event,
                "Team1Total",
                "Over",
                2.30,
                Some(3.0),
                OddsType::Over,
                now,
            ),
            make_odd(
                "bettery",
                &event,
                "Team1Total",
                "Under",
                1.58,
                Some(3.0),
                OddsType::Under,
                now,
            ),
        ];

        let (deduplicated, collapsed) = deduplicate_same_market_quotes(odds);
        assert_eq!(collapsed, 2);
        assert_eq!(deduplicated.len(), 2);
        assert_eq!(deduplicated[0].selection, "Over");
        assert_eq!(deduplicated[0].odds, 2.30);
        assert_eq!(deduplicated[1].selection, "Under");
        assert_eq!(deduplicated[1].odds, 1.65);
        assert_eq!(
            deduplicated[0].id,
            deduplicated[1]
                .id
                .replace("team1total-under", "team1total-over")
        );
    }

    #[test]
    fn odd_ids_distinguish_market_and_line_for_same_event_and_selection() {
        use chrono::Utc;
        use shared::{Event, OddsType, Sport};
        use std::collections::HashMap;

        let event = Event {
            id: "marathon-live-100".into(),
            sport: Sport::Football,
            league: "Test League".into(),
            home_team: "Home".into(),
            away_team: "Away".into(),
            start_time: None,
            is_live: true,
            bookmaker_slug: "marathon".into(),
            raw_url: None,
            extra: HashMap::new(),
        };
        let now = Utc::now();
        let one_x_two = make_odd(
            "marathon",
            &event,
            "1X2",
            "1",
            2.1,
            None,
            OddsType::Home,
            now,
        );
        let handicap = make_odd(
            "marathon",
            &event,
            "Handicap",
            "1",
            1.9,
            Some(-0.5),
            OddsType::Handicap,
            now,
        );
        let total_15 = make_odd(
            "marathon",
            &event,
            "Total",
            "Over",
            1.8,
            Some(1.5),
            OddsType::Over,
            now,
        );
        let total_25 = make_odd(
            "marathon",
            &event,
            "Total",
            "Over",
            2.0,
            Some(2.5),
            OddsType::Over,
            now,
        );

        let ids = [one_x_two.id, handicap.id, total_15.id, total_25.id];
        let unique = ids.iter().collect::<std::collections::HashSet<_>>();
        assert_eq!(unique.len(), ids.len());
    }

    #[test]
    fn runtime_snapshot_deduplicates_repeated_live_events_and_orphan_odds() {
        use super::MarathonParser;
        use chrono::Utc;
        use shared::{Event, Odd, Sport};
        use std::collections::HashMap;

        let event = Event {
            id: "marathon-live-12345".into(),
            sport: Sport::Football,
            league: "Test League".into(),
            home_team: "Home FC".into(),
            away_team: "Away FC".into(),
            start_time: None,
            is_live: true,
            bookmaker_slug: "marathon".into(),
            raw_url: None,
            extra: HashMap::new(),
        };
        let odd = Odd {
            id: "marathon-live-12345-1".into(),
            event_id: event.id.clone(),
            bookmaker_slug: "marathon".into(),
            market: "1X2".into(),
            selection: "1".into(),
            odds: 2.1,
            odds_type: shared::OddsType::Home,
            line: None,
            timestamp: Utc::now(),
        };
        let orphan = Odd {
            id: "orphan-odd".into(),
            event_id: "missing-event".into(),
            ..odd.clone()
        };

        let (events, odds) = MarathonParser::deduplicate_snapshot(
            vec![event.clone(), event.clone()],
            vec![odd.clone(), odd.clone(), orphan],
        );

        assert_eq!(events.len(), 1);
        assert_eq!(odds.len(), 1);
        assert_eq!(odds[0].event_id, events[0].id);
    }

    #[test]
    fn rejects_shared_api_payloads_with_missing_required_arrays() {
        use super::parse_api_response;
        use crate::factors_catalog::FactorsCatalog;
        use std::sync::Arc;

        let client = Arc::new(reqwest::Client::builder().build().expect("client"));
        let factors = FactorsCatalog::new(client, "https://line51.tf39be-resources.com", 3000);

        let missing_events = parse_api_response(
            &serde_json::json!({"customFactors": []}),
            true,
            "marathon",
            &factors,
        );
        assert!(missing_events
            .expect_err("missing events must fail")
            .to_string()
            .contains("events array"));

        let missing_factors = parse_api_response(
            &serde_json::json!({"events": []}),
            true,
            "marathon",
            &factors,
        );
        assert!(missing_factors
            .expect_err("missing customFactors must fail")
            .to_string()
            .contains("customFactors array"));
    }

    #[test]
    fn shared_platform_attaches_child_market_factors_to_parent_match() {
        use super::parse_api_response;
        use crate::factors_catalog::FactorsCatalog;
        use std::sync::Arc;

        let client = Arc::new(reqwest::Client::builder().build().expect("client"));
        let factors = FactorsCatalog::new(client, "https://line51.tf39be-resources.com", 3000);
        let payload = serde_json::json!({
            "events": [
                {
                    "id": 100,
                    "team1": "Home FC",
                    "team2": "Away FC",
                    "place": "live",
                    "sportId": 1,
                    "name": "Football: Test League"
                },
                {
                    "id": 101,
                    "parentId": 100,
                    "name": "угловые",
                    "place": "live",
                    "sportId": 1
                }
            ],
            "customFactors": [
                {"e": 100, "factors": [{"f": 921, "v": 2.1}]},
                {"e": 101, "factors": [{"f": 930, "v": 1.8, "p": 950}]}
            ]
        });

        let parsed = parse_api_response(&payload, true, "marathon", &factors)
            .expect("valid parent/child catalogue fixture");
        assert_eq!(
            parsed.len(),
            1,
            "child rows must not become synthetic events"
        );
        assert_eq!(parsed[0].0.id, "marathon-live-100");
        assert!(parsed[0].1.iter().any(|odd| odd.market == "1X2"));
        assert!(parsed[0]
            .1
            .iter()
            .any(|odd| odd.market == "угловые: Total" && odd.line == Some(9.5)));
        assert!(parsed[0]
            .1
            .iter()
            .all(|odd| odd.event_id == "marathon-live-100"));
        let odd_ids = parsed[0]
            .1
            .iter()
            .map(|odd| odd.id.as_str())
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(odd_ids.len(), parsed[0].1.len());
    }

    #[test]
    fn shared_platform_uses_place_marker_not_requested_endpoint_for_live_state() {
        use super::event_place_matches_feed;

        assert!(event_place_matches_feed(
            &serde_json::json!({"place": "live"}),
            true
        ));
        assert!(!event_place_matches_feed(
            &serde_json::json!({"place": "live"}),
            false
        ));
        assert!(event_place_matches_feed(
            &serde_json::json!({"place": "line"}),
            false
        ));
        assert!(!event_place_matches_feed(
            &serde_json::json!({"place": "line"}),
            true
        ));
        assert!(!event_place_matches_feed(
            &serde_json::json!({"place": "notActive"}),
            true
        ));
        assert!(!event_place_matches_feed(&serde_json::json!({}), false));
    }

    #[test]
    fn live_and_prematch_reused_source_ids_are_namespaced() {
        let client = Arc::new(reqwest::Client::builder().build().expect("client"));
        let factors = FactorsCatalog::new(client, "https://line51.tf39be-resources.com", 3000);
        let live_payload = serde_json::json!({
            "events": [{
                "id": 12345,
                "team1": "Home FC",
                "team2": "Away FC",
                "place": "live",
                "name": "Football: Test League"
            }],
            "customFactors": [{
                "e": 12345,
                "factors": [
                    {"f": 921, "v": 2.10},
                    {"f": 922, "v": 3.20},
                    {"f": 923, "v": 3.50}
                ]
            }]
        });
        let prematch_payload = serde_json::json!({
            "events": [{
                "id": 12345,
                "team1": "Home FC",
                "team2": "Away FC",
                "place": "line",
                "name": "Football: Test League"
            }],
            "customFactors": [{
                "e": 12345,
                "factors": [
                    {"f": 921, "v": 2.10},
                    {"f": 922, "v": 3.20},
                    {"f": 923, "v": 3.50}
                ]
            }]
        });

        let live = parse_api_response(&live_payload, true, "marathon", &factors)
            .expect("parse live payload");
        let prematch = parse_api_response(&prematch_payload, false, "marathon", &factors)
            .expect("parse prematch payload");

        assert_eq!(live.len(), 1);
        assert_eq!(prematch.len(), 1);
        assert_eq!(live[0].0.id, "marathon-live-12345");
        assert_eq!(prematch[0].0.id, "marathon-prematch-12345");
        assert_ne!(live[0].0.id, prematch[0].0.id);

        assert_eq!(live[0].1.len(), 3);
        assert_eq!(prematch[0].1.len(), 3);
        assert!(live[0].1.iter().all(|odd| odd.event_id == live[0].0.id));
        assert!(prematch[0]
            .1
            .iter()
            .all(|odd| odd.event_id == prematch[0].0.id));
        assert!(live[0].1.iter().all(|odd| odd.bookmaker_slug == "marathon"));
        assert!(prematch[0]
            .1
            .iter()
            .all(|odd| odd.bookmaker_slug == "marathon"));
    }
    #[test]
    fn current_shared_factor_catalog_does_not_collapse_opposite_handicap_sides() {
        let client = Arc::new(reqwest::Client::builder().build().expect("client"));
        let factors = FactorsCatalog::new(client, "https://line51.at58f5-resources.ru", 501);
        let catalog_payload = serde_json::json!({
            "configs": [{
                "sports": ["1"],
                "markets": [{
                    "caption": "Форы",
                    "tag": "handicaps",
                    "marketEventKindsSets": [{
                        "factorSets": [{
                            "columns": [
                                {"caption": "ФОРА 1", "columnType": 2, "factors": ["910"]},
                                {"caption": "ФОРА 2", "columnType": 2, "factors": ["912"]}
                            ]
                        }]
                    }]
                }]
            }]
        });
        assert_eq!(factors.parse_catalog(&catalog_payload), 2);

        let payload = serde_json::json!({
            "events": [{
                "id": 70171121,
                "team1": "Болуспор",
                "team2": "Эрокспор",
                "place": "line",
                "name": "Football: Turkey"
            }],
            "customFactors": [{
                "e": 70171121,
                "factors": [
                    {"f": 910, "v": 6.60, "p": 0, "pt": "0"},
                    {"f": 912, "v": 1.06, "p": 0, "pt": "0"}
                ]
            }]
        });

        let parsed = parse_api_response(&payload, false, "bettery", &factors)
            .expect("parse Bettery handicap pair");
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].1.len(), 2);
        let home = parsed[0].1.iter().find(|odd| odd.selection == "1").unwrap();
        let away = parsed[0].1.iter().find(|odd| odd.selection == "2").unwrap();
        assert_eq!(home.odds, 6.60);
        assert_eq!(away.odds, 1.06);
        assert_eq!(home.market, "Handicap");
        assert_eq!(away.market, "Handicap");
        assert_ne!(home.id, away.id);
    }

    #[test]
    fn current_catalog_resolves_reused_factor_ids_by_source_sport() {
        let catalog = FactorsCatalog::new(
            Arc::new(reqwest::Client::builder().build().expect("client")),
            "https://line51.at58f5-resources.ru",
            501,
        );
        let payload = serde_json::json!({
            "configs": [
                {
                    "sports": ["1"],
                    "markets": [{
                        "caption": "Исходы",
                        "tag": "result",
                        "marketEventKindsSets": [{"factorSets": [{"columns": [
                            {"caption": "1", "columnType": 1, "factors": ["921"]},
                            {"caption": "Х", "columnType": 1, "factors": ["922"]},
                            {"caption": "2", "columnType": 1, "factors": ["923"]}
                        ]}]}]
                    }]
                },
                {
                    "sports": ["11634"],
                    "markets": [{
                        "caption": "Победы",
                        "tag": "to_win",
                        "marketEventKindsSets": [{"factorSets": [{"columns": [
                            {"caption": "1", "columnType": 1, "factors": ["921"]},
                            {"caption": "2", "columnType": 1, "factors": ["923"]}
                        ]}]}]
                    }]
                }
            ]
        });
        assert_eq!(catalog.parse_catalog(&payload), 5);

        assert_eq!(
            catalog
                .get_factor_for_sport(921, Some("1"))
                .unwrap()
                .market_type,
            "1x2"
        );
        assert_eq!(
            catalog
                .get_factor_for_sport(921, Some("11634"))
                .unwrap()
                .market_type,
            "to_win"
        );
        assert_eq!(
            catalog
                .get_factor_for_sport(923, Some("11634"))
                .unwrap()
                .market_type,
            "to_win"
        );
    }
    #[test]
    fn catalog_known_factor_is_not_reclassified_by_legacy_numeric_fallback() {
        let client = Arc::new(reqwest::Client::builder().build().expect("client"));
        let factors = FactorsCatalog::new(client, "https://example.invalid", 3000);
        let parsed = factors.parse_catalog(&serde_json::json!({
            "configs": [{
                "sports": ["1"],
                "markets": [{
                    "caption": "Тотал",
                    "tag": "total",
                    "marketEventKindsSets": [{
                        "factorSets": [{
                            "columns": [{
                                "caption": "Больше",
                                "columnType": 1,
                                "factors": ["921"]
                            }]
                        }]
                    }]
                }]
            }]
        }));
        assert_eq!(parsed, 1);

        // 921 historically maps to Home (1X2), but the direct catalog says
        // this ID is a total outcome. With no line value the total cannot be
        // emitted safely; it must not fall through to the legacy Home mapping.
        let payload = serde_json::json!({
            "events": [{
                "id": 12345,
                "team1": "Home FC",
                "team2": "Away FC",
                "place": "line",
                "sportId": "1",
                "name": "Football: Premier League"
            }],
            "customFactors": [{
                "e": 12345,
                "factors": [{"f": 921, "v": 1.85}]
            }]
        });

        let parsed = parse_api_response(&payload, false, "marathon", &factors)
            .expect("valid direct-source fixture");
        assert_eq!(parsed.len(), 1);
        assert!(
            parsed[0].1.is_empty(),
            "a catalog-known but unclassifiable ID must not be reinterpreted as 1X2 Home"
        );
    }

    #[test]
    fn catalog_classifier_quarantines_unscoped_individual_totals() {
        let event = shared::Event {
            id: "marathon-prematch-individual-total".to_string(),
            sport: Sport::Football,
            league: "Test League".to_string(),
            home_team: "Home FC".to_string(),
            away_team: "Away FC".to_string(),
            start_time: None,
            is_live: false,
            bookmaker_slug: "marathon".to_string(),
            raw_url: None,
            extra: std::collections::HashMap::new(),
        };
        let quote = classify_catalog_factor(
            "marathon",
            &event,
            921,
            1.85,
            Some(2.5),
            "unresolved_individual",
            "over",
            chrono::Utc::now(),
        );
        assert!(
            quote.is_none(),
            "an individual total without a proven team side must not become a match total"
        );
    }

    #[test]
    fn catalog_classifier_keeps_half_time_markets_separate_from_full_match() {
        let event = shared::Event {
            id: "marathon-prematch-1".to_string(),
            sport: Sport::Football,
            league: "Test League".to_string(),
            home_team: "Home FC".to_string(),
            away_team: "Away FC".to_string(),
            start_time: None,
            is_live: false,
            bookmaker_slug: "marathon".to_string(),
            raw_url: None,
            extra: std::collections::HashMap::new(),
        };
        let now = chrono::Utc::now();
        let mut odds = Vec::new();

        assert!(classify_and_add_odd(
            &mut odds,
            "marathon",
            &event,
            1,
            1.90,
            Some(1.5),
            "total",
            "over",
            now
        ));
        assert!(classify_and_add_odd(
            &mut odds,
            "marathon",
            &event,
            2,
            1.95,
            Some(1.5),
            "first_half_total",
            "over",
            now
        ));
        assert!(classify_and_add_odd(
            &mut odds,
            "marathon",
            &event,
            3,
            1.95,
            Some(1.5),
            "second_half_total",
            "over",
            now
        ));
        assert!(classify_and_add_odd(
            &mut odds,
            "marathon",
            &event,
            4,
            1.90,
            Some(-0.5),
            "first_half_handicap",
            "1",
            now
        ));
        assert!(classify_and_add_odd(
            &mut odds,
            "marathon",
            &event,
            5,
            1.90,
            None,
            "first_half_1x2",
            "1",
            now
        ));

        assert!(odds.iter().any(|odd| odd.market == "Total"));
        assert!(odds.iter().any(|odd| odd.market == "1H_Total"));
        assert!(odds.iter().any(|odd| odd.market == "2H_Total"));
        assert!(odds.iter().any(|odd| odd.market == "1H_Handicap"));
        assert!(odds.iter().any(|odd| odd.market == "1H_Result"));
    }
}
