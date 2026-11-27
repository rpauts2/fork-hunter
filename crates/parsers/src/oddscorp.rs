//! Authorized ODDSCORP odds-feed client.
//!
//! The client is deliberately separate from direct bookmaker parsers. It is not
//! registered in ParserFactory and cannot enable scanning by itself. A provider
//! adapter must first prove bookmaker coverage, map source metadata, and preserve
//! quote timestamps using a trial key issued by ODDSCORP.

use chrono::{DateTime, Utc};
use reqwest::{Client, Url};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::Value;
use std::sync::Arc;
use thiserror::Error;

pub const DEFAULT_BASE_URL: &str = "http://api.oddscp.com:8111";
pub const TOKEN_ENV_VAR: &str = "ODDSCORP_API_TOKEN";
pub const BASE_URL_ENV_VAR: &str = "ODDSCORP_BASE_URL";
pub const ALLOW_INSECURE_HTTP_ENV_VAR: &str = "ODDSCORP_ALLOW_INSECURE_HTTP";

#[derive(Debug, Error)]
pub enum OddsCorpError {
    #[error("ODDSCORP_API_TOKEN is not configured")]
    MissingToken,
    #[error("ODDSCORP base URL is invalid")]
    InvalidBaseUrl,
    #[error("ODDSCORP event identifier is empty or contains an invalid path segment")]
    InvalidEventId,
    #[error("ODDSCORP requires HTTPS; set ODDSCORP_ALLOW_INSECURE_HTTP=1 only after accepting the cleartext-token risk")]
    InsecureTransportNotAllowed,
    #[error("ODDSCORP request failed: {0}")]
    Transport(String),
    #[error("ODDSCORP returned HTTP {0}")]
    HttpStatus(u16),
    #[error("ODDSCORP response JSON is invalid")]
    InvalidJson,
    #[error("ODDSCORP response does not contain the expected collection")]
    InvalidCollection,
}

/// Minimal client for the documented ODDSCORP events, bookmaker and markets
/// endpoints. The token is never included in Debug output or error messages.
#[derive(Clone)]
pub struct OddsCorpClient {
    client: Arc<Client>,
    base_url: Url,
    token: String,
}

impl std::fmt::Debug for OddsCorpClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OddsCorpClient")
            .field("base_url", &self.base_url)
            .field("token_configured", &!self.token.is_empty())
            .finish()
    }
}

impl OddsCorpClient {
    pub fn from_env(client: Arc<Client>) -> Result<Self, OddsCorpError> {
        let token = std::env::var(TOKEN_ENV_VAR)
            .ok()
            .filter(|value| !value.trim().is_empty())
            .ok_or(OddsCorpError::MissingToken)?;
        let base_url =
            std::env::var(BASE_URL_ENV_VAR).unwrap_or_else(|_| DEFAULT_BASE_URL.to_string());
        let allow_insecure_http = std::env::var(ALLOW_INSECURE_HTTP_ENV_VAR)
            .map(|value| value.trim() == "1")
            .unwrap_or(false);
        Self::new_with_insecure_http(client, &base_url, token, allow_insecure_http)
    }

    pub fn new(
        client: Arc<Client>,
        base_url: &str,
        token: impl Into<String>,
    ) -> Result<Self, OddsCorpError> {
        Self::new_with_insecure_http(client, base_url, token, false)
    }

    pub fn new_with_insecure_http(
        client: Arc<Client>,
        base_url: &str,
        token: impl Into<String>,
        allow_insecure_http: bool,
    ) -> Result<Self, OddsCorpError> {
        let base_url = Url::parse(base_url).map_err(|_| OddsCorpError::InvalidBaseUrl)?;
        if base_url.scheme() != "https"
            && !(base_url.scheme() == "http" && allow_insecure_http)
        {
            return Err(OddsCorpError::InsecureTransportNotAllowed);
        }
        let token = token.into();
        if token.trim().is_empty() {
            return Err(OddsCorpError::MissingToken);
        }

        Ok(Self {
            client,
            base_url,
            token,
        })
    }

    /// Returns only bookmakers included in the token's current subscription.
    pub async fn fetch_bookmakers(&self) -> Result<Vec<OddsCorpBookmaker>, OddsCorpError> {
        let url = self.endpoint("bookmakers")?;
        let payload = self.get_json(url, &[]).await?;
        parse_collection(payload)
    }

    /// Fetches one bookmaker's LIVE or PREMATCH event catalogue.
    ///
    /// The provider documents LIVE as the default; live=0 selects PREMATCH.
    pub async fn fetch_events(
        &self,
        bookmaker: &str,
        live: bool,
    ) -> Result<Vec<OddsCorpEvent>, OddsCorpError> {
        let url = self.endpoint("events")?;
        let mut query = vec![("bk_name", bookmaker)];
        if !live {
            query.push(("live", "0"));
        }
        let payload = self.get_json(url, &query).await?;
        parse_collection(payload)
    }

    /// Fetches source markets for one provider event ID.
    ///
    /// bk_event_id is the provider's event key, not the bookmaker's native ID
    /// and not the provider's cross-bookmaker group ID.
    pub async fn fetch_markets(
        &self,
        bk_event_id: &str,
    ) -> Result<Vec<OddsCorpMarket>, OddsCorpError> {
        if !is_safe_event_id(bk_event_id) {
            return Err(OddsCorpError::InvalidEventId);
        }
        let mut url = self.endpoint("markets/")?;
        url.path_segments_mut()
            .map_err(|_| OddsCorpError::InvalidBaseUrl)?
            .pop_if_empty()
            .push(bk_event_id);

        let payload = self.get_json(url, &[]).await?;
        parse_collection(payload)
    }

    fn endpoint(&self, path: &str) -> Result<Url, OddsCorpError> {
        self.base_url
            .join(path)
            .map_err(|_| OddsCorpError::InvalidBaseUrl)
    }

    async fn get_json(
        &self,
        url: Url,
        parameters: &[(&str, &str)],
    ) -> Result<Value, OddsCorpError> {
        let mut query = Vec::with_capacity(parameters.len() + 1);
        query.push(("token", self.token.as_str()));
        query.extend_from_slice(parameters);

        let response = self
            .client
            .get(url)
            .query(&query)
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
            .await
            .map_err(|error| OddsCorpError::Transport(error.without_url().to_string()))?;

        let status = response.status();
        if !status.is_success() {
            return Err(OddsCorpError::HttpStatus(status.as_u16()));
        }

        response
            .json::<Value>()
            .await
            .map_err(|_| OddsCorpError::InvalidJson)
    }
}

fn is_safe_event_id(value: &str) -> bool {
    !value.trim().is_empty()
        && !value.contains('/')
        && !value.contains('?')
        && !value.contains('#')
        && !value.contains('\\')
}

fn parse_collection<T: DeserializeOwned>(payload: Value) -> Result<Vec<T>, OddsCorpError> {
    let collection = payload
        .as_array()
        .or_else(|| payload.get("data").and_then(Value::as_array))
        .or_else(|| payload.get("result").and_then(Value::as_array))
        .ok_or(OddsCorpError::InvalidCollection)?;

    collection
        .iter()
        .cloned()
        .map(|item| serde_json::from_value(item).map_err(|_| OddsCorpError::InvalidJson))
        .collect()
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct OddsCorpBookmaker {
    #[serde(default)]
    pub bk_id: String,
    #[serde(default)]
    pub bk_name: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub display_name_en: String,
    #[serde(default)]
    pub production: String,
    #[serde(default)]
    pub private: String,
    #[serde(default)]
    pub active: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct OddsCorpEvent {
    #[serde(default)]
    pub bk_event_id: String,
    #[serde(default)]
    pub bk_event_native_id: String,
    #[serde(default)]
    pub bk_name: String,
    #[serde(default)]
    pub bk_id: String,
    #[serde(default)]
    pub sport_id: String,
    #[serde(default)]
    pub sport: String,
    #[serde(default)]
    pub event_name: String,
    #[serde(default)]
    pub team1: String,
    #[serde(default)]
    pub team2: String,
    #[serde(default)]
    pub league_name: String,
    #[serde(default)]
    pub direct_link: String,
    #[serde(default)]
    pub score: String,
    #[serde(default)]
    pub event_id: String,
    #[serde(default)]
    pub active: String,
    #[serde(default)]
    pub updated_at: String,
    #[serde(default)]
    pub last_cf_at: String,
    #[serde(default)]
    pub swapped: String,
}

impl OddsCorpEvent {
    pub fn is_active(&self) -> bool {
        matches!(self.active.trim(), "1" | "true" | "TRUE")
    }

    /// Shared event-group ID used by ODDSCORP to relate bookmaker event IDs.
    pub fn provider_group_id(&self) -> Option<&str> {
        (!self.event_id.trim().is_empty()).then_some(self.event_id.as_str())
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct OddsCorpMarket {
    #[serde(default)]
    pub bet: String,
    #[serde(default)]
    pub cf: Value,
    #[serde(default)]
    pub direct_link: String,
    #[serde(default)]
    pub blocked: Value,
    #[serde(default)]
    pub meta: Value,
    #[serde(default)]
    pub updated_at: Value,
    #[serde(default)]
    pub updated_at_ms: Value,
}

impl OddsCorpMarket {
    pub fn decimal_odds(&self) -> Option<f64> {
        let value = match &self.cf {
            Value::Number(number) => number.as_f64()?,
            Value::String(text) => text.trim().replace(',', ".").parse::<f64>().ok()?,
            _ => return None,
        };
        (value.is_finite() && value > 1.0).then_some(value)
    }

    pub fn is_blocked(&self) -> bool {
        match &self.blocked {
            Value::Bool(value) => *value,
            Value::Number(value) => value.as_i64().unwrap_or_default() != 0,
            Value::String(value) => {
                matches!(
                    value.trim().to_ascii_lowercase().as_str(),
                    "1" | "true" | "yes"
                )
            }
            _ => false,
        }
    }

    /// Parses bookmaker-specific market metadata without assuming one schema
    /// for every operator. The provider explicitly notes that this shape varies
    /// by bookmaker and sport.
    pub fn parsed_meta(&self) -> Value {
        match &self.meta {
            Value::String(raw) => serde_json::from_str(raw).unwrap_or(Value::Null),
            Value::Object(_) => self.meta.clone(),
            _ => Value::Null,
        }
    }

    pub fn market_label(&self) -> String {
        let meta = self.parsed_meta();
        first_string(&meta, &["market_name", "group_name", "name"])
            .unwrap_or_else(|| self.bet.clone())
    }

    pub fn selection_label(&self) -> Option<String> {
        let meta = self.parsed_meta();
        first_string(&meta, &["outcome_name", "odd", "selection", "hv"])
    }

    pub fn line(&self) -> Option<f64> {
        let meta = self.parsed_meta();
        ["line", "total", "handicap", "param", "P", "p"]
            .iter()
            .find_map(|key| numeric_value(meta.get(*key)?))
    }

    /// Provider price-update time, not local parser ingestion time.
    pub fn source_updated_at(&self) -> Option<DateTime<Utc>> {
        parse_timestamp(&self.updated_at_ms).or_else(|| parse_timestamp(&self.updated_at))
    }
}

fn first_string(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        value.get(*key).and_then(|item| match item {
            Value::String(text) if !text.trim().is_empty() => Some(text.trim().to_string()),
            Value::Number(number) => Some(number.to_string()),
            _ => None,
        })
    })
}

fn numeric_value(value: &Value) -> Option<f64> {
    let parsed = match value {
        Value::Number(number) => number.as_f64()?,
        Value::String(text) => text.trim().replace(',', ".").parse::<f64>().ok()?,
        _ => return None,
    };
    parsed.is_finite().then_some(parsed)
}

fn parse_timestamp(value: &Value) -> Option<DateTime<Utc>> {
    let raw = match value {
        Value::Number(number) => number.as_i64()?,
        Value::String(text) => text.trim().parse::<i64>().ok()?,
        _ => return None,
    };

    if raw <= 0 {
        return None;
    }
    if raw >= 100_000_000_000 {
        DateTime::from_timestamp_millis(raw)
    } else {
        DateTime::from_timestamp(raw, 0)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        is_safe_event_id, parse_collection, OddsCorpClient, OddsCorpMarket, DEFAULT_BASE_URL,
    };
    use reqwest::Client;
    use serde_json::json;
    use std::sync::Arc;

    #[test]
    fn client_debug_never_exposes_api_token() {
        let client = OddsCorpClient::new(
            Arc::new(Client::new()),
            "https://provider.example",
            "secret-test-token",
        )
        .expect("client");
        let debug = format!("{client:?}");
        assert!(debug.contains("token_configured"));
        assert!(!debug.contains("secret-test-token"));
    }


    #[test]
    fn rejects_cleartext_http_unless_explicitly_enabled() {
        let client = Arc::new(Client::new());
        assert!(matches!(
            OddsCorpClient::new(client.clone(), DEFAULT_BASE_URL, "token"),
            Err(super::OddsCorpError::InsecureTransportNotAllowed)
        ));
        assert!(OddsCorpClient::new_with_insecure_http(
            client,
            DEFAULT_BASE_URL,
            "token",
            true
        )
        .is_ok());
    }

    #[test]
    fn parses_provider_event_collections_and_preserves_group_ids() {
        let payload = json!([
            {
                "bk_event_id": "BB-EVENT-1",
                "bk_event_native_id": "12345",
                "bk_name": "betboom",
                "bk_id": "BB",
                "sport": "football",
                "team1": "Команда А",
                "team2": "Команда Б",
                "league_name": "Премьер-лига",
                "event_id": "PROVIDER-GROUP-9",
                "active": "1",
                "updated_at": "1795770000"
            }
        ]);

        let events = parse_collection::<super::OddsCorpEvent>(payload).expect("events");
        assert_eq!(events.len(), 1);
        assert!(events[0].is_active());
        assert_eq!(events[0].provider_group_id(), Some("PROVIDER-GROUP-9"));
        assert_eq!(events[0].bk_event_native_id, "12345");
    }

    #[test]
    fn parses_market_odds_metadata_and_source_timestamp() {
        let market: OddsCorpMarket = serde_json::from_value(json!({
            "bet": "WIN__P1",
            "cf": "1.85",
            "blocked": "0",
            "meta": {"group_name": "Основные", "market_name": "Исходы", "outcome_name": "1", "factor_id": 921},
            "updated_at": "1795770000",
            "updated_at_ms": "1795770000123"
        }))
        .expect("market");

        assert_eq!(market.decimal_odds(), Some(1.85));
        assert!(!market.is_blocked());
        assert_eq!(market.market_label(), "Исходы");
        assert_eq!(market.selection_label().as_deref(), Some("1"));
        assert_eq!(
            market.source_updated_at().unwrap().timestamp_millis(),
            1_795_770_000_123
        );
    }

    #[test]
    fn rejects_invalid_market_quotes_and_blocked_outcomes() {
        let market: OddsCorpMarket = serde_json::from_value(json!({
            "bet": "TOTAL__OVER",
            "cf": "1,00",
            "blocked": true,
            "meta": {"market_name": "Тотал", "outcome_name": "Больше", "line": "2.5"}
        }))
        .expect("market");

        assert_eq!(market.decimal_odds(), None);
        assert!(market.is_blocked());
        assert_eq!(market.line(), Some(2.5));
    }

    #[test]
    fn supports_wrapped_collections_and_rejects_malformed_payloads() {
        let wrapped = json!({"data": [{"bk_name": "melbet"}]});
        let items = parse_collection::<super::OddsCorpBookmaker>(wrapped).expect("wrapped");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].bk_name, "melbet");
        assert!(
            parse_collection::<super::OddsCorpBookmaker>(json!({"error": "unauthorized"})).is_err()
        );
    }

    #[test]
    fn rejects_path_injection_in_provider_event_ids() {
        assert!(is_safe_event_id("BB-EVENT-1"));
        assert!(!is_safe_event_id(""));
        assert!(!is_safe_event_id("event/../markets"));
        assert!(!is_safe_event_id("event?token=bad"));
    }
}
