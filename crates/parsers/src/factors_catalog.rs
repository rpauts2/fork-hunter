use parking_lot::RwLock;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};

/// Полное определение фактора из каталога
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FactorDefinition {
    pub id: u64,
    pub name: String,
    pub market_type: String,
    pub selection_name: String,
    pub sport: String,
}

/// Каталог факторов для shared platform (*-resources.com)
#[derive(Clone)]
pub struct FactorsCatalog {
    client: Arc<Client>,
    base_url: String,
    scope_market: u64,
    /// factor_id -> FactorDefinition
    cache: Arc<RwLock<HashMap<u64, FactorDefinition>>>,
    /// factor_id -> sport-specific definitions from sportBasicFactors.
    sport_cache: Arc<RwLock<HashMap<u64, Vec<FactorDefinition>>>>,
    /// market_name -> Vec<factor_id>
    market_index: Arc<RwLock<HashMap<String, Vec<u64>>>>,
    loaded_at: Arc<RwLock<Option<Instant>>>,
    load_lock: Arc<tokio::sync::Mutex<()>>,
}

impl FactorsCatalog {
    pub fn new(client: Arc<Client>, base_url: &str, scope_market: u64) -> Self {
        Self {
            client,
            base_url: base_url.trim_end_matches('/').to_string(),
            scope_market,
            cache: Arc::new(RwLock::new(HashMap::new())),
            sport_cache: Arc::new(RwLock::new(HashMap::new())),
            market_index: Arc::new(RwLock::new(HashMap::new())),
            loaded_at: Arc::new(RwLock::new(None)),
            load_lock: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    /// Загрузить полный каталог факторов из API.
    /// Cached definitions are reused for 15 minutes; refresh builds a separate
    /// snapshot and swaps it atomically only after at least one valid endpoint
    /// returned usable data, so a temporary network outage never clears a good
    /// catalog.
    pub async fn load(&self) -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
        const CACHE_TTL: Duration = Duration::from_secs(15 * 60);
        const PARTIAL_RETRY_TTL: Duration = Duration::from_secs(30);

        let now = Instant::now();
        let cached = self.cache.read().len();
        let is_fresh = self
            .loaded_at
            .read()
            .as_ref()
            .is_some_and(|loaded_at| now.duration_since(*loaded_at) < CACHE_TTL);
        if cached > 0 && is_fresh {
            return Ok(cached);
        }

        // Single-flight refresh: concurrent live/prematch fetches share one
        // catalog request instead of issuing six duplicate HTTP requests.
        let _load_guard = self.load_lock.lock().await;
        let now = Instant::now();
        let cached = self.cache.read().len();
        let is_fresh = self
            .loaded_at
            .read()
            .as_ref()
            .is_some_and(|loaded_at| now.duration_since(*loaded_at) < CACHE_TTL);
        if cached > 0 && is_fresh {
            return Ok(cached);
        }

        let refreshed = FactorsCatalog::new(self.client.clone(), &self.base_url, self.scope_market);
        let endpoints = vec![
            format!("{}/line/factorsCatalog/independentFactors?version=0&lang=ru&sysId=21&scopeMarket={}", self.base_url, self.scope_market),
            format!("{}/line/factorsCatalog/sportBasicFactors?version=0&lang=ru&sysId=21", self.base_url),
            format!("{}/line/factorsCatalog/tables?version=0&lang=ru&sysId=21", self.base_url),
        ];

        let mut total = 0;
        let mut successful_endpoints = 0usize;

        for url in endpoints {
            debug!(url, "Fetching factors catalog");
            match self
                .client
                .get(&url)
                .header(
                    "User-Agent",
                    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36",
                )
                .header("Accept", "application/json")
                .send()
                .await
            {
                Ok(resp) if resp.status().is_success() => {
                    match resp.json::<serde_json::Value>().await {
                        Ok(json) => {
                            let count = refreshed.parse_catalog(&json);
                            total += count;
                            successful_endpoints += 1;
                            debug!(url, count, "Factors parsed");
                        }
                        Err(error) => {
                            warn!(url, error = %error, "Failed to parse JSON from factors catalog");
                        }
                    }
                }
                Ok(resp) => {
                    warn!(url, status = %resp.status(), "Factors catalog endpoint returned HTTP error");
                }
                Err(error) => {
                    warn!(url, error = %error, "Failed to fetch factors catalog");
                }
            }
        }

        if total == 0 {
            if cached > 0 {
                warn!(
                    cached,
                    "Factor catalog refresh failed; retaining last known-good snapshot"
                );
                return Ok(cached);
            }
            return Err(std::io::Error::other(
                "factor catalog returned no usable factor definitions from any endpoint",
            )
            .into());
        }

        refreshed.rebuild_market_index();
        let refreshed_cache = refreshed.cache.read().clone();
        let refreshed_sport_cache = refreshed.sport_cache.read().clone();
        let refreshed_market_index = refreshed.market_index.read().clone();

        *self.cache.write() = refreshed_cache;
        *self.sport_cache.write() = refreshed_sport_cache;
        *self.market_index.write() = refreshed_market_index;
        let cache_timestamp = if successful_endpoints == 3 {
            now
        } else {
            now - (CACHE_TTL - PARTIAL_RETRY_TTL)
        };
        *self.loaded_at.write() = Some(cache_timestamp);

        let final_count = self.cache.read().len();
        info!(
            total_factors = final_count,
            parsed_definitions = total,
            successful_endpoints,
            markets = self.market_index.read().len(),
            "Factors catalog loaded"
        );
        Ok(final_count)
    }

    /// Распарсить JSON каталог
    pub(crate) fn parse_catalog(&self, json: &serde_json::Value) -> usize {
        let mut count = 0;
        let mut cache = self.cache.write();
        let mut sport_cache = self.sport_cache.write();

        // Формат 1: { factors: [{id, name, type, outcomes: [{id, name}]}] }
        if let Some(factors) = json.get("factors").and_then(|f| f.as_array()) {
            for factor in factors {
                if let (Some(fid), Some(name)) = (
                    factor.get("id").and_then(|v| v.as_u64()),
                    factor.get("name").and_then(|v| v.as_str()),
                ) {
                    let market_type = factor
                        .get("type")
                        .and_then(|t| t.as_str())
                        .unwrap_or("")
                        .to_string();
                    let outcomes = factor.get("outcomes").and_then(|o| o.as_array());

                    if let Some(outcomes_arr) = &outcomes {
                        for outcome in outcomes_arr.iter() {
                            if let (Some(oid), Some(oname)) = (
                                outcome.get("id").and_then(|v| v.as_u64()),
                                outcome.get("name").and_then(|v| v.as_str()),
                            ) {
                                cache.insert(
                                    oid,
                                    FactorDefinition {
                                        id: oid,
                                        name: oname.to_string(),
                                        market_type: market_type.clone(),
                                        selection_name: oname.to_string(),
                                        sport: factor
                                            .get("sport")
                                            .and_then(|s| s.as_str())
                                            .unwrap_or("")
                                            .to_string(),
                                    },
                                );
                                count += 1;
                            }
                        }
                    }

                    // Сам фактор тоже может быть outcome
                    if outcomes.as_ref().map_or(true, |o| o.is_empty()) {
                        cache.insert(
                            fid,
                            FactorDefinition {
                                id: fid,
                                name: name.to_string(),
                                market_type: market_type.clone(),
                                selection_name: name.to_string(),
                                sport: factor
                                    .get("sport")
                                    .and_then(|s| s.as_str())
                                    .unwrap_or("")
                                    .to_string(),
                            },
                        );
                        count += 1;
                    }
                }
            }
        }

        // Формат 2: { data: [{factorId, factorName, marketType, outcomes: [...]}] }
        if let Some(data) = json.get("data").and_then(|d| d.as_array()) {
            for item in data {
                if let (Some(fid), Some(fname)) = (
                    item.get("factorId").and_then(|v| v.as_u64()),
                    item.get("factorName").and_then(|v| v.as_str()),
                ) {
                    let market_type = item
                        .get("marketType")
                        .and_then(|t| t.as_str())
                        .unwrap_or("")
                        .to_string();
                    let sport = item
                        .get("sport")
                        .and_then(|s| s.as_str())
                        .unwrap_or("")
                        .to_string();

                    match item.get("outcomes").and_then(|value| value.as_array()) {
                        Some(outcomes) if !outcomes.is_empty() => {
                            for outcome in outcomes {
                                if let (Some(oid), Some(oname)) = (
                                    outcome.get("id").and_then(|value| value.as_u64()),
                                    outcome.get("name").and_then(|value| value.as_str()),
                                ) {
                                    cache.insert(
                                        oid,
                                        FactorDefinition {
                                            id: oid,
                                            name: oname.to_string(),
                                            market_type: market_type.clone(),
                                            selection_name: oname.to_string(),
                                            sport: sport.clone(),
                                        },
                                    );
                                    count += 1;
                                } else {
                                    warn!(
                                        factor_id = fid,
                                        "Skipping catalog outcome without a numeric ID or name"
                                    );
                                }
                            }
                        }
                        _ => {
                            cache.insert(
                                fid,
                                FactorDefinition {
                                    id: fid,
                                    name: fname.to_string(),
                                    market_type: market_type.clone(),
                                    selection_name: fname.to_string(),
                                    sport: sport.clone(),
                                },
                            );
                            count += 1;
                        }
                    }
                }
            }
        }

        // Текущий формат платформы: sportBasicFactors -> configs -> markets ->
        // marketEventKindsSets -> factorSets -> columns. Это основной источник
        // соответствия factor ID конкретному рынку и стороне/исходу.
        count += Self::parse_sport_basic_factors(json, &mut cache, &mut sport_cache);

        // Расширенный каталог платформы: таблицы содержат дополнительные рынки
        // и явное соответствие factorId колонке таблицы. Добавляем только IDs,
        // которых нет в более точном sportBasicFactors.
        count += Self::parse_tables(json, &mut cache);

        // Формат 3: Прямой маппинг { "921": "П1", "922": "Х", ... }
        if let Some(obj) = json.as_object() {
            for (key, val) in obj {
                if let Ok(fid) = key.parse::<u64>() {
                    if let Some(name) = val.as_str() {
                        cache.entry(fid).or_insert_with(|| FactorDefinition {
                            id: fid,
                            name: name.to_string(),
                            market_type: Self::guess_market_type(name),
                            selection_name: name.to_string(),
                            sport: String::new(),
                        });
                        count += 1;
                    }
                }
            }
        }

        count
    }

    fn parse_sport_basic_factors(
        json: &serde_json::Value,
        cache: &mut HashMap<u64, FactorDefinition>,
        sport_cache: &mut HashMap<u64, Vec<FactorDefinition>>,
    ) -> usize {
        let Some(configs) = json.get("configs").and_then(serde_json::Value::as_array) else {
            return 0;
        };
        let mut inserted = 0;

        for config in configs {
            let sport = config
                .get("sports")
                .and_then(serde_json::Value::as_array)
                .map(|sports| {
                    sports
                        .iter()
                        .filter_map(|value| {
                            value
                                .as_str()
                                .map(str::to_owned)
                                .or_else(|| value.as_u64().map(|id| id.to_string()))
                        })
                        .collect::<Vec<_>>()
                        .join(",")
                })
                .unwrap_or_default();

            let Some(markets) = config.get("markets").and_then(serde_json::Value::as_array) else {
                continue;
            };

            for market in markets {
                let market_name = market
                    .get("caption")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default();
                let market_tag = market
                    .get("tag")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default();
                let market_type = Self::normalize_market_type(market_name, market_tag);

                let Some(event_kind_sets) = market
                    .get("marketEventKindsSets")
                    .and_then(serde_json::Value::as_array)
                else {
                    continue;
                };

                for event_kind_set in event_kind_sets {
                    let Some(factor_sets) = event_kind_set
                        .get("factorSets")
                        .and_then(serde_json::Value::as_array)
                    else {
                        continue;
                    };

                    for factor_set in factor_sets {
                        let Some(columns) = factor_set
                            .get("columns")
                            .and_then(serde_json::Value::as_array)
                        else {
                            continue;
                        };

                        for column in columns {
                            // columnType=3 is a parameter/input column, not an
                            // outcome. Its IDs also appear in the Б/М outcome
                            // columns, so mapping it would create false clashes.
                            if column.get("columnType").and_then(serde_json::Value::as_u64)
                                == Some(3)
                            {
                                continue;
                            }

                            let selection_caption = column
                                .get("caption")
                                .and_then(serde_json::Value::as_str)
                                .unwrap_or_default();
                            let selection =
                                Self::normalize_selection(&market_type, selection_caption);
                            let Some(factor_ids) =
                                column.get("factors").and_then(serde_json::Value::as_array)
                            else {
                                continue;
                            };

                            for factor_id in factor_ids.iter().filter_map(Self::value_as_u64) {
                                let definition = FactorDefinition {
                                    id: factor_id,
                                    name: format!("{} / {}", market_name, selection_caption),
                                    market_type: market_type.clone(),
                                    selection_name: selection.clone(),
                                    sport: sport.clone(),
                                };
                                let definitions = sport_cache.entry(factor_id).or_default();
                                let is_new = !definitions.iter().any(|existing| {
                                    existing.market_type == definition.market_type
                                        && existing.selection_name == definition.selection_name
                                        && existing.sport == definition.sport
                                });
                                if is_new {
                                    definitions.push(definition.clone());
                                    inserted += 1;
                                }
                                cache.entry(factor_id).or_insert(definition);
                            }
                        }
                    }
                }
            }
        }

        inserted
    }

    fn parse_tables(json: &serde_json::Value, cache: &mut HashMap<u64, FactorDefinition>) -> usize {
        let Some(groups) = json.get("groups").and_then(serde_json::Value::as_array) else {
            return 0;
        };
        let mut inserted = 0;

        for group in groups {
            let group_name = group
                .get("name")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            let Some(tables) = group.get("tables").and_then(serde_json::Value::as_array) else {
                continue;
            };

            for table in tables {
                let table_name = table
                    .get("name")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default();
                let market_type = Self::normalize_market_type(table_name, group_name);
                let Some(rows) = table.get("rows").and_then(serde_json::Value::as_array) else {
                    continue;
                };
                let Some(headers) = rows.first().and_then(serde_json::Value::as_array) else {
                    continue;
                };

                for row in rows.iter().skip(1).filter_map(serde_json::Value::as_array) {
                    for (column_index, cell) in row.iter().enumerate() {
                        if cell.get("kind").and_then(serde_json::Value::as_str) != Some("value") {
                            continue;
                        }
                        let Some(factor_id) = cell.get("factorId").and_then(Self::value_as_u64)
                        else {
                            continue;
                        };
                        let selection_caption = headers
                            .get(column_index)
                            .and_then(|header| header.get("name"))
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or_default();
                        let selection = Self::normalize_selection(&market_type, selection_caption);
                        let row_label = row
                            .first()
                            .and_then(|value| value.get("name"))
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or_default();
                        let name = if row_label.is_empty() {
                            format!("{} / {}", table_name, selection_caption)
                        } else {
                            format!("{} / {} / {}", group_name, table_name, row_label)
                        };
                        let definition = FactorDefinition {
                            id: factor_id,
                            name,
                            market_type: market_type.clone(),
                            selection_name: selection,
                            sport: String::new(),
                        };
                        if let std::collections::hash_map::Entry::Vacant(entry) =
                            cache.entry(factor_id)
                        {
                            entry.insert(definition);
                            inserted += 1;
                        }
                    }
                }
            }
        }

        inserted
    }

    fn value_as_u64(value: &serde_json::Value) -> Option<u64> {
        value
            .as_u64()
            .or_else(|| value.as_str().and_then(|text| text.parse::<u64>().ok()))
    }

    fn normalize_market_type(market_name: &str, tag: &str) -> String {
        let combined = format!("{} {}", market_name, tag).to_lowercase();
        // Keep first/second-half scope in the canonical market type. Without
        // this, "1-й тайм Тотал" and the full-match total normalize identically
        // and can be compared as if they were the same market.
        let period_prefix = if [
            "1-й тайм",
            "1-го тайма",
            "1-м тайме",
            "1 тайм",
            "1й тайм",
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
        ]
        .iter()
        .any(|marker| combined.contains(marker))
        {
            "first_half_"
        } else if [
            "2-й тайм",
            "2-го тайма",
            "2-м тайме",
            "2 тайм",
            "2й тайм",
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
        ]
        .iter()
        .any(|marker| combined.contains(marker))
        {
            "second_half_"
        } else {
            ""
        };

        if combined.contains("фора по сет") || combined.contains("sets_handicap") {
            "sets_handicap".to_string()
        } else if combined.contains("фора по карт") || combined.contains("maps_handicap")
        {
            "maps_handicap".to_string()
        } else if combined.contains("инд. тотал 1")
            || combined.contains("индивидуальный тотал команды 1")
            || combined.contains("тотал команды 1")
            || combined.contains("тотал первой команды")
            || combined.contains("тотал 1-й команды")
            || combined.contains("team1_total")
            || combined.contains("team 1 total")
        {
            format!("{period_prefix}team1_total")
        } else if combined.contains("инд. тотал 2")
            || combined.contains("индивидуальный тотал команды 2")
            || combined.contains("тотал команды 2")
            || combined.contains("тотал второй команды")
            || combined.contains("тотал 2-й команды")
            || combined.contains("team2_total")
            || combined.contains("team 2 total")
        {
            format!("{period_prefix}team2_total")
        } else if combined.contains("тотал сет") || combined.contains("sets_total") {
            "sets_total".to_string()
        } else if combined.contains("тотал карт") || combined.contains("maps_total") {
            "maps_total".to_string()
        } else if combined.contains("тотал серии") || combined.contains("series_total") {
            "series_total".to_string()
        } else if combined.contains("фор") || combined.contains("handicap") {
            format!("{period_prefix}handicap")
        } else if combined.contains("тотал") || combined.contains("total") {
            format!("{period_prefix}total")
        } else if combined.contains("двойн")
            || combined.contains("double_chance")
            || combined.contains("double chance")
        {
            format!("{period_prefix}double_chance")
        } else if combined.contains("обе забьют")
            || combined.contains("обе команды забьют")
            || combined.contains("both_to_score")
            || combined.contains("both_teams_score")
            || combined.contains("both teams to score")
        {
            format!("{period_prefix}both_teams_score")
        } else if combined.contains("чёт")
            || combined.contains("чет")
            || combined.contains("even_odd")
        {
            format!("{period_prefix}even_odd")
        } else if combined.contains("итоговая победа") || combined.contains("final_win")
        {
            format!("{period_prefix}final_win")
        } else if combined.contains("побед") || combined.contains("to_win") {
            format!("{period_prefix}to_win")
        } else if combined.contains("исход") || combined.contains("result") {
            format!("{period_prefix}1x2")
        } else if combined.contains("кто выше") || combined.contains("head_to_head") {
            "head_to_head".to_string()
        } else if combined.contains("проход") || combined.contains("to_qualify") {
            "to_qualify".to_string()
        } else {
            market_name.to_string()
        }
    }

    fn normalize_selection(market_type: &str, caption: &str) -> String {
        let value = caption.trim().to_lowercase();
        let base_market_type = market_type
            .strip_prefix("first_half_")
            .or_else(|| market_type.strip_prefix("second_half_"))
            .unwrap_or(market_type);
        match base_market_type {
            "handicap" | "sets_handicap" | "maps_handicap" => {
                if value == "1"
                    || value.starts_with("ф1")
                    || value.contains("фора 1")
                    || value.contains("фора1")
                {
                    "1".to_string()
                } else if value == "2"
                    || value.starts_with("ф2")
                    || value.contains("фора 2")
                    || value.contains("фора2")
                {
                    "2".to_string()
                } else {
                    caption.trim().to_string()
                }
            }
            "total" | "team1_total" | "team2_total" | "sets_total" | "maps_total"
            | "series_total" => {
                if value == "б"
                    || value.starts_with("тб")
                    || value.starts_with("итб")
                    || value.contains("больше")
                    || value.contains("over")
                {
                    "over".to_string()
                } else if value == "м"
                    || value.starts_with("тм")
                    || value.starts_with("итм")
                    || value.contains("меньше")
                    || value.contains("under")
                {
                    "under".to_string()
                } else {
                    caption.trim().to_string()
                }
            }
            "1x2" | "to_win" | "final_win" => {
                if value == "х" || value == "x" || value.contains("ничья") {
                    "x".to_string()
                } else if value == "1" || value.contains("п1") {
                    "1".to_string()
                } else if value == "2" || value.contains("п2") {
                    "2".to_string()
                } else {
                    caption.trim().to_string()
                }
            }
            "both_teams_score" => {
                if value == "да" || value == "yes" {
                    "yes".to_string()
                } else if value == "нет" || value == "no" {
                    "no".to_string()
                } else {
                    caption.trim().to_string()
                }
            }
            "even_odd" => {
                // Check the negative form first: "нечёт" also contains "чёт".
                if value.contains("нечёт") || value.contains("нечет") || value == "odd" {
                    "odd".to_string()
                } else if value.contains("чёт") || value.contains("чет") || value == "even" {
                    "even".to_string()
                } else {
                    caption.trim().to_string()
                }
            }
            "double_chance" => {
                // Russian feeds commonly use Cyrillic Х where Latin X is
                // expected. Normalize both alphabets and verbose captions.
                let compact = value
                    .replace('х', "x")
                    .replace(' ', "")
                    .replace('-', "")
                    .replace('/', "")
                    .replace('(', "")
                    .replace(')', "");
                if compact == "1x"
                    || compact.contains("1илиx")
                    || compact.contains("1илиничья")
                    || compact.contains("xили1")
                    || compact.contains("ничьяили1")
                {
                    "1X".to_string()
                } else if compact == "x2"
                    || compact.contains("xили2")
                    || compact.contains("ничьяили2")
                    || compact.contains("2илиx")
                    || compact.contains("2илиничья")
                {
                    "X2".to_string()
                } else if compact == "12" || compact.contains("1или2") || compact.contains("2или1")
                {
                    "12".to_string()
                } else {
                    caption.trim().to_string()
                }
            }
            _ => caption.trim().to_string(),
        }
    }

    /// Угадать тип рынка по названию
    fn guess_market_type(name: &str) -> String {
        let n = name.to_lowercase();
        if n.contains("итб") || n.contains("итм") || n.contains("individual") {
            "IndividualTotal".into()
        } else if n.contains("тб") || n.contains("тм") || n.contains("total") {
            "Total".into()
        } else if n.contains("фора") || n.contains("handicap") || n.contains("ф ") {
            "Handicap".into()
        } else if n.contains("оз") || n.contains("обе") || n.contains("btts") || n.contains("both")
        {
            "BothTeamsScore".into()
        } else if n.contains("чёт")
            || n.contains("нечет")
            || n.contains("even")
            || n.contains("odd")
        {
            "EvenOdd".into()
        } else if n.contains("двойн")
            || n.contains("double")
            || n.contains("1x")
            || n.contains("x2")
            || n.contains("12")
        {
            "DoubleChance".into()
        } else if n.contains("точн") || n.contains("correct") || n.contains("score") {
            "CorrectScore".into()
        } else if n.contains("1-й тайм") || n.contains("1h") {
            "FirstHalf".into()
        } else if n.contains("2-й тайм") || n.contains("2h") {
            "SecondHalf".into()
        } else if n.contains("п1") || n.contains("п2") || n.contains("х") || n.contains("1x2") {
            "1X2".into()
        } else {
            n
        }
    }

    /// Перестроить индекс market -> factor_ids
    fn rebuild_market_index(&self) {
        let cache = self.cache.read();
        let mut market_index: HashMap<String, Vec<u64>> = HashMap::new();

        for (&fid, def) in cache.iter() {
            market_index
                .entry(def.market_type.clone())
                .or_default()
                .push(fid);
        }
        drop(cache);

        let sport_cache = self.sport_cache.read();
        for (&fid, definitions) in sport_cache.iter() {
            for definition in definitions {
                market_index
                    .entry(definition.market_type.clone())
                    .or_default()
                    .push(fid);
            }
        }

        for ids in market_index.values_mut() {
            ids.sort_unstable();
            ids.dedup();
        }

        drop(sport_cache);
        *self.market_index.write() = market_index;
    }

    /// Whether the direct source catalog contains this ID, even when its
    /// sport-scoped definitions conflict and therefore cannot be resolved safely.
    pub fn contains_factor_id(&self, factor_id: u64) -> bool {
        if self.cache.read().contains_key(&factor_id) {
            return true;
        }
        self.sport_cache.read().contains_key(&factor_id)
    }

    /// Получить определение фактора по ID
    pub fn get_factor(&self, factor_id: u64) -> Option<FactorDefinition> {
        self.cache.read().get(&factor_id).cloned()
    }

    /// Resolve an ID using the source sport ID when the bookmaker reuses factor
    /// IDs for different market families in different sports.
    pub fn get_factor_for_sport(
        &self,
        factor_id: u64,
        source_sport_id: Option<&str>,
    ) -> Option<FactorDefinition> {
        let sport_cache = self.sport_cache.read();
        if let Some(definitions) = sport_cache.get(&factor_id) {
            if let Some(source_sport_id) = source_sport_id {
                let mut matching = definitions.iter().filter(|definition| {
                    definition
                        .sport
                        .split(',')
                        .any(|sport| sport.trim() == source_sport_id.trim())
                });
                if let Some(first_match) = matching.next() {
                    // A factor can be reused inside one sport as well. A sport
                    // match is not enough if the same ID maps to conflicting
                    // market/selection semantics in that sport.
                    let is_unambiguous = matching.all(|definition| {
                        definition.market_type == first_match.market_type
                            && definition.selection_name == first_match.selection_name
                    });
                    return is_unambiguous.then(|| first_match.clone());
                }
            }

            let first = definitions.first()?;
            // Some event sport IDs are child/tournament IDs that are not
            // present in the public factor catalogue. A fallback is safe only
            // when every sport-scoped definition agrees on market and outcome;
            // otherwise fail closed instead of borrowing an arbitrary sport.
            let is_unambiguous = definitions.iter().all(|definition| {
                definition.market_type == first.market_type
                    && definition.selection_name == first.selection_name
            });
            return is_unambiguous.then(|| first.clone());
        }
        drop(sport_cache);
        self.get_factor(factor_id)
    }

    /// Получить все factor IDs для типа рынка
    pub fn get_factor_ids(&self, market_type: &str) -> Vec<u64> {
        self.market_index
            .read()
            .get(market_type)
            .cloned()
            .unwrap_or_default()
    }

    /// Получить все известные рынки
    pub fn get_all_markets(&self) -> Vec<String> {
        let mut markets: Vec<String> = self.market_index.read().keys().cloned().collect();
        markets.sort();
        markets
    }

    /// Получить все известные факторы
    pub fn get_all_factors(&self) -> Vec<FactorDefinition> {
        let mut all = self.cache.read().values().cloned().collect::<Vec<_>>();
        let sport_cache = self.sport_cache.read();
        for definitions in sport_cache.values() {
            all.extend(definitions.iter().cloned());
        }
        all
    }

    /// Печатная сводка
    pub fn print_summary(&self) {
        let cache = self.cache.read();
        let market_index = self.market_index.read();

        info!("=== Factors Catalog Summary ===");
        info!("Total unique factors: {}", cache.len());
        info!("Total markets: {}", market_index.len());

        for (market, ids) in market_index.iter() {
            info!(
                "  {}: {} factors (IDs: {:?})",
                market,
                ids.len(),
                ids.iter().take(5).collect::<Vec<_>>()
            );
        }
    }

    /// Найти факторы по ключевому слову
    pub fn find_factors(&self, keyword: &str) -> Vec<FactorDefinition> {
        let kw = keyword.to_lowercase();
        self.get_all_factors()
            .into_iter()
            .filter(|factor| {
                factor.name.to_lowercase().contains(&kw)
                    || factor.market_type.to_lowercase().contains(&kw)
                    || factor.selection_name.to_lowercase().contains(&kw)
            })
            .collect()
    }
}

impl std::fmt::Debug for FactorsCatalog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FactorsCatalog")
            .field("base_url", &self.base_url)
            .field("scope_market", &self.scope_market)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::FactorsCatalog;
    use std::sync::Arc;

    fn catalog() -> FactorsCatalog {
        let client = Arc::new(reqwest::Client::builder().build().expect("client"));
        FactorsCatalog::new(client, "https://example.invalid", 3000)
    }

    #[test]
    fn even_odd_normalization_checks_odd_before_even_substring() {
        assert_eq!(
            FactorsCatalog::normalize_selection("even_odd", "Чёт"),
            "even"
        );
        assert_eq!(
            FactorsCatalog::normalize_selection("even_odd", "Нечёт"),
            "odd"
        );
        assert_eq!(
            FactorsCatalog::normalize_selection("even_odd", "Нечет"),
            "odd"
        );
        assert_eq!(
            FactorsCatalog::normalize_selection("even_odd", "Odd"),
            "odd"
        );
    }

    #[test]
    fn format_two_keeps_unique_outcome_ids() {
        let catalog = catalog();
        let count = catalog.parse_catalog(&serde_json::json!({
            "data": [{
                "factorId": 930,
                "factorName": "Total",
                "marketType": "Total",
                "sport": "Football",
                "outcomes": [
                    {"id": 9301, "name": "Over"},
                    {"id": 9302, "name": "Under"}
                ]
            }]
        }));

        assert_eq!(count, 2);
        assert_eq!(catalog.get_factor(9301).unwrap().selection_name, "Over");
        assert_eq!(catalog.get_factor(9302).unwrap().selection_name, "Under");
        assert!(catalog.get_factor(930).is_none());
    }

    #[test]
    fn format_two_does_not_collapse_missing_outcome_ids_to_factor_id() {
        let catalog = catalog();
        let count = catalog.parse_catalog(&serde_json::json!({
            "data": [{
                "factorId": 930,
                "factorName": "Total",
                "marketType": "Total",
                "sport": "Football",
                "outcomes": [
                    {"name": "Over"},
                    {"name": "Under"}
                ]
            }]
        }));

        assert_eq!(count, 0);
        assert!(catalog.get_factor(930).is_none());
    }

    #[test]
    fn format_two_without_outcomes_uses_factor_id() {
        let catalog = catalog();
        let count = catalog.parse_catalog(&serde_json::json!({
            "data": [{
                "factorId": 921,
                "factorName": "Home win",
                "marketType": "1X2",
                "sport": "Football"
            }]
        }));

        assert_eq!(count, 1);
        assert_eq!(catalog.get_factor(921).unwrap().selection_name, "Home win");
    }

    #[test]
    fn current_sport_basic_factors_schema_preserves_handicap_sides_and_totals() {
        let catalog = catalog();
        let count = catalog.parse_catalog(&serde_json::json!({
            "configs": [{
                "sports": ["1"],
                "markets": [
                    {
                        "caption": "Форы",
                        "tag": "handicaps",
                        "marketEventKindsSets": [{
                            "factorSets": [{
                                "columns": [
                                    {"caption": "ФОРА 1", "columnType": 2, "factors": ["910", "927"]},
                                    {"caption": "ФОРА 2", "columnType": 2, "factors": ["912", "928"]}
                                ]
                            }]
                        }]
                    },
                    {
                        "caption": "Тоталы",
                        "tag": "totals",
                        "marketEventKindsSets": [{
                            "factorSets": [{
                                "columns": [
                                    {"caption": "Тотал", "columnType": 3, "factors": ["930"]},
                                    {"caption": "Б", "columnType": 1, "factors": ["930"]},
                                    {"caption": "М", "columnType": 1, "factors": ["931"]}
                                ]
                            }]
                        }]
                    }
                ]
            }]
        }));

        assert_eq!(count, 6);
        let home_handicap = catalog.get_factor(910).unwrap();
        let away_handicap = catalog.get_factor(912).unwrap();
        assert_eq!(home_handicap.market_type, "handicap");
        assert_eq!(home_handicap.selection_name, "1");
        assert_eq!(away_handicap.market_type, "handicap");
        assert_eq!(away_handicap.selection_name, "2");
        assert_eq!(catalog.get_factor(930).unwrap().selection_name, "over");
        assert_eq!(catalog.get_factor(931).unwrap().selection_name, "under");
    }

    #[test]
    fn reused_factor_id_does_not_fall_back_to_another_sports_market() {
        let catalog = catalog();
        catalog.parse_catalog(&serde_json::json!({
            "configs": [
                {
                    "sports": ["1"],
                    "markets": [{
                        "caption": "Исходы",
                        "tag": "result",
                        "marketEventKindsSets": [{"factorSets": [{"columns": [
                            {"caption": "1", "columnType": 1, "factors": ["921"]}
                        ]}]}]
                    }]
                },
                {
                    "sports": ["11634"],
                    "markets": [{
                        "caption": "Победы",
                        "tag": "to_win",
                        "marketEventKindsSets": [{"factorSets": [{"columns": [
                            {"caption": "1", "columnType": 1, "factors": ["921"]}
                        ]}]}]
                    }]
                }
            ]
        }));

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
        // Unknown sports must not inherit an arbitrary market definition.
        assert!(catalog.get_factor_for_sport(921, Some("999999")).is_none());
    }

    #[test]
    fn reused_factor_id_with_conflicting_definitions_in_same_sport_is_rejected() {
        let catalog = catalog();
        catalog.parse_catalog(&serde_json::json!({
            "configs": [{
                "sports": ["1"],
                "markets": [
                    {
                        "caption": "Исходы",
                        "tag": "result",
                        "marketEventKindsSets": [{"factorSets": [{"columns": [
                            {"caption": "1", "columnType": 1, "factors": ["921"]}
                        ]}]}]
                    },
                    {
                        "caption": "Форы",
                        "tag": "handicaps",
                        "marketEventKindsSets": [{"factorSets": [{"columns": [
                            {"caption": "ФОРА 1", "columnType": 1, "factors": ["921"]}
                        ]}]}]
                    }
                ]
            }]
        }));

        assert!(
            catalog.get_factor_for_sport(921, Some("1")).is_none(),
            "a matching sport must not hide conflicting definitions for the same factor ID"
        );
        assert!(catalog.contains_factor_id(921));
    }

    #[test]
    fn sport_catalog_accepts_numeric_sport_ids() {
        let catalog = catalog();
        catalog.parse_catalog(&serde_json::json!({
            "configs": [{
                "sports": [1],
                "markets": [{
                    "caption": "Исходы",
                    "tag": "result",
                    "marketEventKindsSets": [{"factorSets": [{"columns": [
                        {"caption": "1", "columnType": 1, "factors": ["921"]}
                    ]}]}]
                }]
            }]
        }));

        assert_eq!(
            catalog
                .get_factor_for_sport(921, Some("1"))
                .unwrap()
                .market_type,
            "1x2"
        );
    }

    #[test]
    fn reused_factor_id_without_sport_is_rejected_when_definitions_are_ambiguous() {
        let catalog = catalog();
        catalog.parse_catalog(&serde_json::json!({
            "configs": [
                {
                    "sports": ["1"],
                    "markets": [{
                        "caption": "Исходы",
                        "tag": "result",
                        "marketEventKindsSets": [{"factorSets": [{"columns": [
                            {"caption": "1", "columnType": 1, "factors": ["921"]}
                        ]}]}]
                    }]
                },
                {
                    "sports": ["11634"],
                    "markets": [{
                        "caption": "Победы",
                        "tag": "to_win",
                        "marketEventKindsSets": [{"factorSets": [{"columns": [
                            {"caption": "1", "columnType": 1, "factors": ["921"]}
                        ]}]}]
                    }]
                }
            ]
        }));

        assert!(catalog.get_factor_for_sport(921, None).is_none());
        assert!(
            catalog.contains_factor_id(921),
            "ambiguous source IDs must be distinguishable from IDs absent in the catalog"
        );
        assert!(!catalog.contains_factor_id(999_999));
    }

    #[test]
    fn sport_scoped_factor_with_one_definition_can_resolve_without_sport() {
        let catalog = catalog();
        catalog.parse_catalog(&serde_json::json!({
            "configs": [{
                "sports": ["1"],
                "markets": [{
                    "caption": "Исходы",
                    "tag": "result",
                    "marketEventKindsSets": [{"factorSets": [{"columns": [
                        {"caption": "1", "columnType": 1, "factors": ["921"]}
                    ]}]}]
                }]
            }]
        }));

        assert_eq!(
            catalog.get_factor_for_sport(921, None).unwrap().market_type,
            "1x2"
        );
    }

    #[test]
    fn current_tables_schema_maps_value_cells_without_overwriting_basic_factors() {
        let catalog = catalog();
        let count = catalog.parse_catalog(&serde_json::json!({
            "groups": [{
                "name": "Основные ставки",
                "tables": [{
                    "name": "Фора",
                    "rows": [
                        [{"name": "Фора"}, {"name": "1"}, {"name": "Фора"}, {"name": "2"}],
                        [{"kind": "param", "factorId": 910}, {"kind": "value", "factorId": 910},
                         {"kind": "param", "factorId": 912}, {"kind": "value", "factorId": 912}]
                    ]
                }]
            }]
        }));

        assert_eq!(count, 2);
        assert_eq!(catalog.get_factor(910).unwrap().selection_name, "1");
        assert_eq!(catalog.get_factor(912).unwrap().selection_name, "2");
    }
    #[test]
    fn market_normalization_preserves_first_and_second_half_scope() {
        assert_eq!(
            FactorsCatalog::normalize_market_type("Тотал первого тайма", ""),
            "first_half_total"
        );
        assert_eq!(
            FactorsCatalog::normalize_market_type("Тотал второго тайма", ""),
            "second_half_total"
        );
        assert_eq!(
            FactorsCatalog::normalize_market_type("Фора 1-го тайма", ""),
            "first_half_handicap"
        );
        assert_eq!(
            FactorsCatalog::normalize_market_type("Исход 2-го тайма", ""),
            "second_half_1x2"
        );
        assert_eq!(
            FactorsCatalog::normalize_market_type("Тотал матча", ""),
            "total"
        );
    }

    #[test]
    fn period_scoped_selection_normalization_uses_base_market_family() {
        assert_eq!(
            FactorsCatalog::normalize_selection("first_half_total", "Б"),
            "over"
        );
        assert_eq!(
            FactorsCatalog::normalize_selection("second_half_handicap", "ФОРА 2"),
            "2"
        );
        assert_eq!(
            FactorsCatalog::normalize_selection("first_half_1x2", "Х"),
            "x"
        );
    }

    #[test]
    fn market_normalization_recognizes_period_aliases_and_extended_market_labels() {
        assert_eq!(
            FactorsCatalog::normalize_market_type("Both teams to score", "1st_half"),
            "first_half_both_teams_score"
        );
        assert_eq!(
            FactorsCatalog::normalize_market_type("Индивидуальный тотал команды 1", "first-half"),
            "first_half_team1_total"
        );
        assert_eq!(
            FactorsCatalog::normalize_market_type("Тотал команды 2", "2nd_half"),
            "second_half_team2_total"
        );
        assert_eq!(
            FactorsCatalog::normalize_market_type("Double Chance", "2nd_half"),
            "second_half_double_chance"
        );
        assert_eq!(
            FactorsCatalog::normalize_market_type("Обе команды забьют", ""),
            "both_teams_score"
        );
    }

    #[test]
    fn selection_normalization_accepts_common_bookmaker_short_labels() {
        assert_eq!(
            FactorsCatalog::normalize_selection("total", "ТБ 2,5"),
            "over"
        );
        assert_eq!(
            FactorsCatalog::normalize_selection("total", "ТМ 2.5"),
            "under"
        );
        assert_eq!(
            FactorsCatalog::normalize_selection("handicap", "Ф1 (+1.5)"),
            "1"
        );
        assert_eq!(
            FactorsCatalog::normalize_selection("second_half_handicap", "Ф2 (-1.5)"),
            "2"
        );
    }

    #[test]
    fn double_chance_selection_normalizes_cyrillic_and_verbose_labels() {
        assert_eq!(
            FactorsCatalog::normalize_selection("double_chance", "1Х"),
            "1X"
        );
        assert_eq!(
            FactorsCatalog::normalize_selection("double_chance", "Х2"),
            "X2"
        );
        assert_eq!(
            FactorsCatalog::normalize_selection("double_chance", "1 или 2"),
            "12"
        );
        assert_eq!(
            FactorsCatalog::normalize_selection("first_half_double_chance", "1Х"),
            "1X"
        );
    }
}
