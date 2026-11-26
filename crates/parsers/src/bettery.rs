use crate::base::{BookmakerParser, ParserResult};
use crate::factors_catalog::FactorsCatalog;
use async_trait::async_trait;
use reqwest::Client;
use shared::{Event, Odd};
use std::sync::Arc;
use tracing::info;

/// Bettery API — shared platform, scopeMarket=501
/// live: https://line51.at58f5-resources.com/events/list?lang=ru&scopeMarket=501
/// prematch: https://line01.at58f5-resources.com/events/listBase?lang=ru&scopeMarket=501
#[derive(Debug)]
pub struct BetteryParser {
    client: Arc<Client>,
    live_url: String,
    prematch_url: String,
    factors: Arc<FactorsCatalog>,
}

impl BetteryParser {
    pub fn new(client: Arc<Client>) -> Self {
        let live_base_url = "https://line51.at58f5-resources.ru";
        let prematch_base_url = "https://line01.at58f5-resources.ru";
        Self {
            client: client.clone(),
            live_url: format!("{}/events/list?lang=ru&scopeMarket=501", live_base_url),
            prematch_url: format!(
                "{}/events/listBase?lang=ru&scopeMarket=501",
                prematch_base_url
            ),
            factors: Arc::new(FactorsCatalog::new(client, live_base_url, 501)),
        }
    }

    pub async fn load_factors(&self) -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
        self.factors.load().await
    }
}

#[async_trait]
impl BookmakerParser for BetteryParser {
    fn name(&self) -> &str {
        "Bettery"
    }
    fn slug(&self) -> &str {
        "bettery"
    }
    fn is_enabled(&self) -> bool {
        true
    }

    async fn fetch_events(&self) -> Result<Vec<Event>, Box<dyn std::error::Error + Send + Sync>> {
        let mut all_events = Vec::new();
        for (url, is_live) in [(&self.live_url, true), (&self.prematch_url, false)] {
            let results = self.do_fetch(url, is_live).await?;
            all_events.extend(results.into_iter().map(|(event, _)| event));
        }
        info!(count = all_events.len(), "Bettery events parsed");
        Ok(all_events)
    }

    async fn fetch_odds(
        &self,
        _event_id: &str,
    ) -> Result<Vec<Odd>, Box<dyn std::error::Error + Send + Sync>> {
        let mut all_odds = Vec::new();
        for (url, is_live) in [(&self.live_url, true), (&self.prematch_url, false)] {
            let results = self.do_fetch(url, is_live).await?;
            all_odds.extend(results.into_iter().flat_map(|(_, odds)| odds));
        }
        Ok(all_odds)
    }

    async fn fetch_all(&self) -> Result<ParserResult, Box<dyn std::error::Error + Send + Sync>> {
        match self.factors.load().await {
            Ok(count) => info!(factor_count = count, "Bettery factor catalog loaded"),
            Err(error) => {
                tracing::warn!(error = %error, "Bettery factor catalog unavailable; using verified fallback mappings")
            }
        }

        let start = std::time::Instant::now();
        let mut events = Vec::new();
        let mut odds = Vec::new();
        let mut errors = Vec::new();

        for (url, is_live) in [(&self.live_url, true), (&self.prematch_url, false)] {
            match self.do_fetch(url, is_live).await {
                Ok(results) => {
                    for (event, event_odds) in results {
                        events.push(event);
                        odds.extend(event_odds);
                    }
                }
                Err(error) => {
                    errors.push(format!("{url}: {error}"));
                }
            }
        }

        if !errors.is_empty() {
            return Err(std::io::Error::other(errors.join("; ")).into());
        }

        let elapsed = start.elapsed().as_millis() as u64;
        info!(
            events = events.len(),
            odds = odds.len(),
            failed_feeds = errors.len(),
            "Bettery fetch complete"
        );
        Ok(ParserResult::new("bettery", events, odds, elapsed))
    }

    fn base_url(&self) -> &str {
        "https://bettery.ru"
    }
    fn user_agent(&self) -> &str {
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36"
    }
}

impl BetteryParser {
    async fn do_fetch(
        &self,
        url: &str,
        is_live: bool,
    ) -> Result<Vec<(Event, Vec<Odd>)>, Box<dyn std::error::Error + Send + Sync>> {
        let resp = self.client.get(url)
            .header("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36")
            .header("Accept", "application/json, text/plain, */*")
            .header("Accept-Language", "ru-RU,ru;q=0.9")
            .send()
            .await?;

        if !resp.status().is_success() {
            return Err(std::io::Error::other(format!(
                "Bettery API returned HTTP {}",
                resp.status()
            ))
            .into());
        }

        let json: serde_json::Value = resp.json().await?;
        crate::marathon::parse_api_response(&json, is_live, "bettery", &self.factors)
    }
}

#[cfg(test)]
mod parser_fixture_tests {
    use crate::factors_catalog::FactorsCatalog;
    use std::sync::Arc;

    #[test]
    fn shared_platform_fixture_is_normalized_as_bettery() {
        let client = Arc::new(reqwest::Client::builder().build().expect("client"));
        let factors = FactorsCatalog::new(client, "https://line51.at58f5-resources.ru", 501);
        let payload = serde_json::json!({
            "events": [{
                "id": 88,
                "team1": "Team A",
                "team2": "Team B",
                "place": "line",
                "name": "Football: Test League"
            }],
            "customFactors": [{
                "e": 88,
                "factors": [
                    {"f": 921, "v": 2.0},
                    {"f": 924, "v": 1.8, "p": 150},
                    {"f": 924, "v": 2.0, "p": 250}
                ]
            }]
        });

        let parsed = crate::marathon::parse_api_response(&payload, false, "bettery", &factors)
            .expect("valid Bettery feed fixture");
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].0.bookmaker_slug, "bettery");
        assert!(parsed[0]
            .1
            .iter()
            .all(|odd| odd.bookmaker_slug == "bettery"));
        assert!(parsed[0].1.iter().any(|odd| odd.line == Some(1.5)));
        assert!(parsed[0].1.iter().any(|odd| odd.line == Some(2.5)));
        let total_ids = parsed[0]
            .1
            .iter()
            .filter(|odd| odd.market == "Total")
            .map(|odd| odd.id.clone())
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(total_ids.len(), 2);
    }
}
