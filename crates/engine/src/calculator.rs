use chrono::Utc;
use dashmap::DashMap;
use lru::LruCache;
use parking_lot::RwLock;
use shared::odds::{calculate_stakes, calculate_surebet_profit};
use shared::{Event, Odd, Surebet, SurebetLeg};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::num::NonZeroUsize;
use std::sync::Arc;
use tracing::debug;
use uuid::Uuid;

/// Cached combo result for quick lookup
#[derive(Clone, Debug)]
pub struct ComboCache {
    pub profit: Option<f64>,
    pub stakes: Option<Vec<f64>>,
}

impl Hash for ComboCache {
    fn hash<H: Hasher>(&self, state: &mut H) {
        if let Some(p) = self.profit {
            p.to_bits().hash(state);
        }
    }
}

#[derive(Clone)]
pub struct SurebetCalculator {
    pub min_profit: f64,
    pub max_profit: f64,
    pub default_stake: f64,
    pub early_termination_roi: f64,
    seen_surebets: Arc<RwLock<LruCache<String, ()>>>,
    recent_events: Arc<DashMap<String, Vec<Odd>>>,
    /// LRU cache for combo profitability results (up to 1000 entries)
    combo_cache: Arc<RwLock<LruCache<Vec<u64>, ComboCache>>>,
}

impl SurebetCalculator {
    pub fn new(
        min_profit: f64,
        max_profit: f64,
        default_stake: f64,
        capacity: usize,
        error_rate: f64,
    ) -> Self {
        // Default early termination ROI: 3x min_profit
        Self::with_early_termination(
            min_profit,
            max_profit,
            default_stake,
            capacity,
            error_rate,
            min_profit * 3.0,
        )
    }

    /// Create calculator with custom early termination threshold
    pub fn with_early_termination(
        min_profit: f64,
        max_profit: f64,
        default_stake: f64,
        capacity: usize,
        _error_rate: f64,
        early_termination_roi: f64,
    ) -> Self {
        // Initialize LRU cache with max 1000 entries
        let cache_size = NonZeroUsize::new(1000).unwrap();
        Self {
            min_profit,
            max_profit,
            default_stake,
            early_termination_roi,
            seen_surebets: Arc::new(RwLock::new(LruCache::new(
                NonZeroUsize::new(capacity.max(1)).unwrap(),
            ))),
            recent_events: Arc::new(DashMap::new()),
            combo_cache: Arc::new(RwLock::new(LruCache::new(cache_size))),
        }
    }

    pub fn find_surebets(&self, events: &[Event], all_odds: &[Odd]) -> Vec<Surebet> {
        let mut surebets = Vec::new();
        let odds_by_event = self.group_odds_by_event(all_odds);

        for event in events {
            let Some(event_odds) = odds_by_event.get(&event.id) else {
                continue;
            };

            let deduped_odds = self.deduplicate_odds(event_odds);
            for surebet in self.analyze_event(event, &deduped_odds) {
                let key = self.surebet_key(&surebet);
                if self.seen_surebets.read().peek(&key).is_none() {
                    debug!(profit = surebet.profit_percent, "New surebet found");
                    surebets.push(surebet);
                }
            }
        }

        // Do not stop after the first high-ROI result: callers expect a complete
        // scan of the supplied batch. Stable tie-breaking makes output reproducible.
        surebets.sort_by(|left, right| {
            right
                .profit_percent
                .total_cmp(&left.profit_percent)
                .then_with(|| self.surebet_key(left).cmp(&self.surebet_key(right)))
        });
        surebets
    }

    /// Deduplicate odds using HashSet to prevent redundant combo testing
    fn deduplicate_odds(&self, odds: &[Odd]) -> Vec<Odd> {
        let mut seen = HashSet::new();
        let mut deduped = Vec::new();

        for odd in odds {
            let key = (
                odd.bookmaker_slug.to_lowercase(),
                odd.market.trim().to_lowercase(),
                odd.selection.trim().to_lowercase(),
                odd.line.map(f64::to_bits),
                odd.odds.to_bits(),
            );

            if seen.insert(key) {
                deduped.push(odd.clone());
            }
        }

        deduped
    }

    pub fn analyze_two_way(&self, odds_a: f64, odds_b: f64) -> Option<f64> {
        calculate_surebet_profit(&[odds_a, odds_b])
    }

    pub fn analyze_three_way(&self, odds_1: f64, odds_x: f64, odds_2: f64) -> Option<f64> {
        calculate_surebet_profit(&[odds_1, odds_x, odds_2])
    }

    /// Calculate profit with LRU cache for fast repeated lookups
    /// This is critical for performance when testing multiple combo variations
    fn calculate_profit_cached(&self, odds: &[f64]) -> Option<f64> {
        // Create cache key by hashing odds
        let mut cache_key = Vec::new();
        for &o in odds {
            cache_key.push(o.to_bits());
        }

        // Check cache first
        {
            let mut cache = self.combo_cache.write();
            if let Some(result) = cache.get(&cache_key) {
                return result.profit;
            }
        }

        // Calculate if not in cache
        let profit = calculate_surebet_profit(odds);

        // Store in cache
        {
            let mut cache = self.combo_cache.write();
            cache.put(
                cache_key,
                ComboCache {
                    profit,
                    stakes: profit.and_then(|_| Some(calculate_stakes(odds, self.default_stake))),
                },
            );
        }

        profit
    }

    /// Calculate stakes with cache optimization
    fn calculate_stakes_cached(&self, odds: &[f64]) -> Vec<f64> {
        let mut cache_key = Vec::new();
        for &o in odds {
            cache_key.push(o.to_bits());
        }

        {
            let mut cache = self.combo_cache.write();
            if let Some(result) = cache.get(&cache_key) {
                if let Some(stakes) = &result.stakes {
                    return stakes.clone();
                }
            }
        }

        calculate_stakes(odds, self.default_stake)
    }

    pub fn calculate_stakes(&self, odds: &[f64]) -> Vec<f64> {
        calculate_stakes(odds, self.default_stake)
    }

    pub fn mark_seen(&self, surebet: &Surebet) {
        let key = self.surebet_key(surebet);
        self.seen_surebets.write().put(key, ());
    }

    pub fn is_seen(&self, surebet: &Surebet) -> bool {
        let key = self.surebet_key(surebet);
        self.seen_surebets.read().peek(&key).is_some()
    }

    pub fn cache_odds(&self, event_id: &str, odds: Vec<Odd>) {
        self.recent_events.insert(event_id.to_string(), odds);
    }

    /// Clear the combo cache to free memory
    pub fn clear_combo_cache(&self) {
        self.combo_cache.write().clear();
    }

    /// Get current combo cache size
    pub fn combo_cache_size(&self) -> usize {
        self.combo_cache.read().len()
    }

    fn group_odds_by_event(&self, all_odds: &[Odd]) -> HashMap<String, Vec<Odd>> {
        let mut map = HashMap::new();
        for odd in all_odds {
            map.entry(odd.event_id.clone())
                .or_insert_with(Vec::new)
                .push(odd.clone());
        }
        map
    }

    fn analyze_event(&self, event: &Event, odds: &[Odd]) -> Vec<Surebet> {
        let by_market = self.group_by_market(odds);
        let mut market_keys: Vec<_> = by_market.keys().collect();
        market_keys.sort();

        let mut surebets = Vec::new();
        for market_key in market_keys {
            if let Some(market_odds) = by_market.get(market_key) {
                surebets.extend(self.find_market_surebets(event, market_key, market_odds));
            }
        }
        surebets
    }

    /// Group odds by market (market + line) before comparing selections.
    fn group_by_market<'a>(&self, odds: &'a [Odd]) -> HashMap<String, Vec<&'a Odd>> {
        let mut map: HashMap<String, Vec<&'a Odd>> = HashMap::new();
        for odd in odds {
            let normalized_market = self.normalize_market_key(&odd.market, odd.line);
            map.entry(normalized_market).or_default().push(odd);
        }
        map
    }

    /// Normalize market names while keeping different totals/handicap lines apart.
    fn normalize_market_key(&self, market: &str, line: Option<f64>) -> String {
        let m = market.to_lowercase();
        let base = if m.contains("over")
            || m.contains("under")
            || m.contains("тб")
            || m.contains("тм")
            || m.contains("total")
        {
            "total"
        } else if m.contains("handicap") || m.contains("фора") || m.contains("asian") {
            "handicap"
        } else if m.contains("1x2")
            || m.contains("match result")
            || m.contains("full time result")
            || m.contains("match outcome")
        {
            "1x2"
        } else if m.contains("winner") || m.contains("moneyline") {
            "winner"
        } else {
            market
        };

        match line {
            Some(value) if value.is_finite() => {
                let comparable_line = if base == "handicap" {
                    value.abs()
                } else {
                    value
                };
                format!("{}_{:.2}", base, comparable_line)
            }
            Some(_) => format!("{}_invalid_line", base),
            None => base.to_string(),
        }
    }

    /// Find every viable bookmaker combination for a market.
    ///
    /// The old implementation selected the single best price per outcome first.
    /// If those prices came from the same bookmaker it discarded the market,
    /// even when the second-best price from another bookmaker formed an arbitrage.
    /// Retain the best valid price per bookmaker and evaluate cross-bookmaker combinations.
    fn find_market_surebets(
        &self,
        event: &Event,
        _market_key: &str,
        odds: &[&Odd],
    ) -> Vec<Surebet> {
        let mut by_selection: BTreeMap<String, BTreeMap<String, &Odd>> = BTreeMap::new();

        for odd in odds {
            if !odd.odds.is_finite() || odd.odds <= 1.0 || odd.bookmaker_slug.trim().is_empty() {
                continue;
            }

            let selection = Self::normalize_selection(odd);
            if selection.is_empty() {
                continue;
            }

            let by_bookmaker = by_selection.entry(selection).or_default();
            let bookmaker_key = odd.bookmaker_slug.trim().to_lowercase();
            match by_bookmaker.get_mut(&bookmaker_key) {
                Some(existing) if odd.odds > existing.odds => *existing = odd,
                Some(_) => {}
                None => {
                    by_bookmaker.insert(bookmaker_key, odd);
                }
            }
        }

        if !(by_selection.len() == 2 || by_selection.len() == 3)
            || by_selection.values().any(|prices| prices.is_empty())
        {
            return Vec::new();
        }

        // An explicit 1X2 market has three mutually exclusive outcomes. If a
        // parser omitted one selection, treating the remaining two prices as
        // a two-way market would create a false surebet.
        if _market_key.starts_with("1x2")
            && !(by_selection.contains_key("home")
                && by_selection.contains_key("draw")
                && by_selection.contains_key("away"))
        {
            return Vec::new();
        }

        let candidates: Vec<Vec<&Odd>> = by_selection
            .values()
            .map(|prices| prices.values().copied().collect())
            .collect();
        let mut surebets = Vec::new();

        if candidates.len() == 2 {
            for first in &candidates[0] {
                for second in &candidates[1] {
                    if first
                        .bookmaker_slug
                        .trim()
                        .eq_ignore_ascii_case(second.bookmaker_slug.trim())
                    {
                        continue;
                    }
                    if let Some(surebet) = self.build_surebet(event, &[*first, *second]) {
                        surebets.push(surebet);
                    }
                }
            }
        } else {
            for first in &candidates[0] {
                for second in &candidates[1] {
                    for third in &candidates[2] {
                        let bookmakers = [
                            first.bookmaker_slug.as_str(),
                            second.bookmaker_slug.as_str(),
                            third.bookmaker_slug.as_str(),
                        ];
                        if bookmakers
                            .iter()
                            .all(|bookmaker| *bookmaker == bookmakers[0])
                        {
                            continue;
                        }
                        if let Some(surebet) = self.build_surebet(event, &[*first, *second, *third])
                        {
                            surebets.push(surebet);
                        }
                    }
                }
            }
        }

        surebets
    }

    fn normalize_selection(odd: &Odd) -> String {
        let raw = odd.selection.trim().to_lowercase();
        let normalized = match raw.as_str() {
            "1" | "п1" | "p1" | "home" | "home win" | "хозяева" | "победа хозяев" => {
                Some("home")
            }
            "x" | "х" | "draw" | "ничья" => Some("draw"),
            "2" | "п2" | "p2" | "away" | "away win" | "гости" | "победа гостей" => {
                Some("away")
            }
            "over" | "больше" | "тб" | "тотал больше" => Some("over"),
            "under" | "меньше" | "тм" | "тотал меньше" => Some("under"),
            "yes" | "да" | "btts yes" | "обе забьют да" => Some("btts_yes"),
            "no" | "нет" | "btts no" | "обе забьют нет" => Some("btts_no"),
            "even" | "чет" | "чёт" => Some("even"),
            "odd" | "нечет" | "нечёт" => Some("odd"),
            _ => None,
        };
        if let Some(normalized) = normalized {
            return normalized.into();
        }

        match odd.odds_type {
            shared::odds::OddsType::Home => "home".into(),
            shared::odds::OddsType::Draw => "draw".into(),
            shared::odds::OddsType::Away => "away".into(),
            shared::odds::OddsType::Over => "over".into(),
            shared::odds::OddsType::Under => "under".into(),
            shared::odds::OddsType::BothTeamsScoreYes => "btts_yes".into(),
            shared::odds::OddsType::BothTeamsScoreNo => "btts_no".into(),
            shared::odds::OddsType::Even => "even".into(),
            shared::odds::OddsType::Odd => "odd".into(),
            _ => raw,
        }
    }

    fn build_surebet(&self, event: &Event, selected: &[&Odd]) -> Option<Surebet> {
        let odds: Vec<f64> = selected.iter().map(|odd| odd.odds).collect();
        let profit = self.calculate_profit_cached(&odds)?;
        if !profit.is_finite() || profit < self.min_profit || profit > self.max_profit {
            return None;
        }

        let stakes = self.calculate_stakes_cached(&odds);
        if stakes.len() != selected.len()
            || stakes
                .iter()
                .any(|stake| !stake.is_finite() || *stake <= 0.0)
        {
            return None;
        }
        let total_stake: f64 = stakes.iter().sum();
        if !total_stake.is_finite() || total_stake <= 0.0 {
            return None;
        }

        let legs = selected
            .iter()
            .zip(stakes.iter())
            .map(|(odd, stake)| SurebetLeg {
                bookmaker: odd.bookmaker_slug.clone(),
                market: odd.market.clone(),
                selection: odd.selection.clone(),
                odds: odd.odds,
                line: odd.line,
                stake: *stake,
                payout: *stake * odd.odds,
                url: None,
            })
            .collect();

        Some(Surebet {
            id: Uuid::new_v4(),
            sport: event.sport,
            league: event.league.clone(),
            home_team: event.home_team.clone(),
            away_team: event.away_team.clone(),
            start_time: event.start_time,
            is_live: event.is_live,
            profit_percent: profit,
            total_stake,
            legs,
            detected_at: Utc::now(),
            verified: false,
            mirror: false,
        })
    }

    /// Генерирует уникальный ключ для вилки (для bloom filter дедупликации)
    fn surebet_key(&self, surebet: &Surebet) -> String {
        let (first_team, second_team) = {
            let home = surebet.home_team.trim().to_lowercase();
            let away = surebet.away_team.trim().to_lowercase();
            if home <= away {
                (home, away)
            } else {
                (away, home)
            }
        };
        let event_identity = format!(
            "event|{:?}|{}|{}|{}|{}|{}",
            surebet.sport,
            surebet.league.trim().to_lowercase(),
            first_team,
            second_team,
            surebet
                .start_time
                .map(|time| time.timestamp_millis().to_string())
                .unwrap_or_else(|| "unknown-time".into()),
            surebet.is_live
        );
        let mut parts: Vec<String> = vec![event_identity];
        parts.extend(surebet.legs.iter().map(|leg| {
            format!(
                "{}|{}|{}|{:016x}|{:016x}",
                leg.bookmaker.trim().to_lowercase(),
                leg.market.trim().to_lowercase(),
                leg.selection.trim().to_lowercase(),
                leg.line.map(f64::to_bits).unwrap_or(0),
                leg.odds.to_bits()
            )
        }));
        // Sort legs for a stable key independent of outcome order.
        parts[1..].sort();
        parts.join("::")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shared::Sport;
    use shared::odds::OddsType;

    fn event(id: &str) -> Event {
        Event {
            id: id.to_string(),
            sport: Sport::Football,
            league: "Test League".into(),
            home_team: "Home".into(),
            away_team: "Away".into(),
            start_time: None,
            is_live: false,
            bookmaker_slug: "test".into(),
            raw_url: None,
            extra: HashMap::new(),
        }
    }

    fn odd(event_id: &str, bookmaker: &str, selection: &str, odds: f64) -> Odd {
        Odd {
            id: format!("{event_id}-{bookmaker}-{selection}-{odds}"),
            event_id: event_id.into(),
            bookmaker_slug: bookmaker.into(),
            market: "Match Winner".into(),
            selection: selection.into(),
            odds,
            odds_type: match selection {
                "1" => OddsType::Home,
                "X" => OddsType::Draw,
                "2" => OddsType::Away,
                _ => OddsType::Custom,
            },
            line: None,
            timestamp: Utc::now(),
        }
    }

    fn calculator() -> SurebetCalculator {
        SurebetCalculator::new(0.5, 30.0, 1_000.0, 10_000, 0.01)
    }

    #[test]
    fn finds_cross_bookmaker_fork_when_best_prices_are_from_same_bookmaker() {
        let event = event("event-1");
        let odds = vec![
            odd("event-1", "book-a", "1", 2.20),
            odd("event-1", "book-b", "1", 2.05),
            odd("event-1", "book-a", "2", 2.20),
            odd("event-1", "book-c", "2", 2.05),
        ];

        let surebets = calculator().find_surebets(&[event], &odds);

        assert_eq!(surebets.len(), 3);
        assert!(surebets.iter().all(|surebet| {
            surebet.legs.len() == 2
                && surebet.legs[0].bookmaker != surebet.legs[1].bookmaker
                && surebet.total_stake > 0.0
                && (surebet.legs.iter().map(|leg| leg.stake).sum::<f64>() - surebet.total_stake)
                    .abs()
                    < 1e-8
        }));
    }

    #[test]
    fn normalizes_selection_from_odds_type_when_source_label_is_localized() {
        let event = event("event-typed-selections");
        let mut home = odd(
            "event-typed-selections",
            "book-a",
            "Победа первой команды",
            2.20,
        );
        home.odds_type = shared::odds::OddsType::Home;
        let mut away = odd(
            "event-typed-selections",
            "book-b",
            "Победа второй команды",
            2.20,
        );
        away.odds_type = shared::odds::OddsType::Away;

        let surebets = calculator().find_surebets(&[event], &[home, away]);

        assert_eq!(surebets.len(), 1);
    }

    #[test]
    fn bookmaker_slug_comparison_is_case_insensitive() {
        let event = event("event-case-bk");
        let odds = vec![
            odd("event-case-bk", "Pari", "1", 2.20),
            odd("event-case-bk", "pari", "2", 2.20),
        ];

        assert!(calculator().find_surebets(&[event], &odds).is_empty());
    }

    #[test]
    fn scans_remaining_events_after_finding_high_roi_fork() {
        let events = vec![event("event-high"), event("event-normal")];
        let odds = vec![
            odd("event-high", "book-a", "1", 2.50),
            odd("event-high", "book-b", "2", 2.50),
            odd("event-normal", "book-a", "1", 2.20),
            odd("event-normal", "book-b", "2", 2.20),
        ];

        let surebets = calculator().find_surebets(&events, &odds);

        assert_eq!(surebets.len(), 2);
        assert!(surebets[0].profit_percent > surebets[1].profit_percent);
    }

    #[test]
    fn seen_filter_never_marks_an_unseen_surebet_as_seen() {
        let calc = calculator();
        let event = event("event-seen-filter");
        let first_odds = vec![
            odd("event-seen-filter", "book-a", "1", 2.20),
            odd("event-seen-filter", "book-b", "2", 2.20),
        ];
        let second_odds = vec![
            odd("event-seen-filter", "book-a", "1", 2.30),
            odd("event-seen-filter", "book-b", "2", 2.20),
        ];

        let first = calc
            .find_surebets(std::slice::from_ref(&event), &first_odds)
            .into_iter()
            .next()
            .expect("first surebet should be found");
        let second = calc
            .find_surebets(&[event], &second_odds)
            .into_iter()
            .next()
            .expect("second surebet should be found");

        calc.mark_seen(&first);
        assert!(calc.is_seen(&first));
        assert!(!calc.is_seen(&second));
    }

    #[test]
    fn seen_filter_scopes_surebets_to_the_event_identity() {
        let calc = calculator();
        let first_event = event("event-first");
        let mut second_event = event("event-second");
        second_event.home_team = "Another Home".into();
        second_event.away_team = "Another Away".into();

        let first_odds = vec![
            odd("event-first", "book-a", "1", 2.20),
            odd("event-first", "book-b", "2", 2.20),
        ];
        let second_odds = vec![
            odd("event-second", "book-a", "1", 2.20),
            odd("event-second", "book-b", "2", 2.20),
        ];

        let first = calc.find_surebets(&[first_event], &first_odds).remove(0);
        let second = calc.find_surebets(&[second_event], &second_odds).remove(0);
        calc.mark_seen(&first);

        assert!(!calc.is_seen(&second));
    }

    #[test]
    fn rejects_invalid_odds_instead_of_treating_them_as_free_probability() {
        let event = event("event-invalid");
        let odds = vec![
            odd("event-invalid", "book-a", "1", 2.20),
            odd("event-invalid", "book-b", "2", 1.00),
            odd("event-invalid", "book-c", "2", f64::NAN),
        ];

        assert!(calculator().find_surebets(&[event], &odds).is_empty());
    }

    #[test]
    fn does_not_treat_incomplete_football_1x2_prices_as_a_two_way_market() {
        let event = event("event-incomplete-1x2");
        let mut odds = vec![
            odd("event-incomplete-1x2", "book-a", "1", 2.20),
            odd("event-incomplete-1x2", "book-b", "2", 2.20),
        ];
        for odd in &mut odds {
            odd.market = "1X2".into();
        }

        assert!(calculator().find_surebets(&[event], &odds).is_empty());
    }

    #[test]
    fn explicit_1x2_market_requires_home_draw_and_away_outcomes() {
        let event = event("event-invalid-1x2");
        let mut odds = vec![
            odd("event-invalid-1x2", "book-a", "1", 4.00),
            odd("event-invalid-1x2", "book-b", "X", 4.00),
            odd("event-invalid-1x2", "book-c", "Over", 4.00),
        ];
        for odd in &mut odds {
            odd.market = "1X2".into();
        }

        assert!(calculator().find_surebets(&[event], &odds).is_empty());
    }

    #[test]
    fn three_way_fork_can_use_two_outcomes_from_one_bookmaker() {
        let event = event("event-three-way");
        let mut odds = vec![
            odd("event-three-way", "book-a", "1", 4.00),
            odd("event-three-way", "book-a", "X", 4.00),
            odd("event-three-way", "book-b", "2", 4.00),
        ];
        for odd in &mut odds {
            odd.market = "1X2".into();
        }

        let surebets = calculator().find_surebets(&[event], &odds);

        assert_eq!(surebets.len(), 1);
        assert_eq!(surebets[0].legs.len(), 3);
        assert!(surebets[0].profit_percent > 0.0);
    }
}
