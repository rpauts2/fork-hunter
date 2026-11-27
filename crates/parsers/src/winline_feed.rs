//! Direct Winline public WebSocket feed decoder.
//!
//! Winline's current line page sends a binary protocol over `data_ng`. The
//! transport and packet parser are kept separate from the older DOM discovery
//! code in `winline.rs` so line data does not depend on rendered card markup.

use chrono::{DateTime, Utc};
use flate2::read::GzDecoder;
use futures::{SinkExt, StreamExt};
use shared::odds::OddsType;
use shared::{Event, Odd, Sport};
use std::collections::HashMap;
use std::io::Read;
use std::time::{Duration, Instant};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::{ACCEPT_LANGUAGE, ORIGIN, REFERER, USER_AGENT};
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, info, warn};

const WS_URL: &str = "wss://wss.winline.ru/data_ng?client=newsite&nb=true";
const WS_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";
const MAX_CONNECT_TIME: Duration = Duration::from_secs(18);
const READ_IDLE_TIMEOUT: Duration = Duration::from_millis(2200);
const RUSSIA_UTC_OFFSET_SECONDS: i64 = 10_800;
const MAX_PENDING_ODDS_EVENT_GROUPS: usize = 512;

#[derive(Debug, Clone)]
struct MarketDefinition {
    source_type: i32,
    name: String,
    outcomes: Vec<String>,
}

#[derive(Debug, Clone)]
struct Championship {
    id: i32,
    sport_id: i32,
    name: String,
}

#[derive(Debug, Clone)]
struct SportDefinition {
    name: String,
}

#[derive(Default)]
struct FeedState {
    markets: HashMap<u16, MarketDefinition>,
    sports: HashMap<i32, SportDefinition>,
    championships: HashMap<i32, Championship>,
    events: HashMap<String, Event>,
    odds: HashMap<String, Odd>,
    // Some live WebSocket batches announce line packets before the matching
    // event packet. Keep those quotes until the event arrives instead of
    // silently losing them or emitting orphan odds.
    pending_odds: HashMap<String, HashMap<String, Odd>>,
    prematch_seen: bool,
    live_seen: bool,
    menu_seen: bool,
    incomplete_tail_records: usize,
}

struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.offset)
    }

    fn seek(&mut self, offset: usize) -> Result<(), String> {
        if offset > self.bytes.len() {
            return Err(format!("Winline packet seek out of range: {offset}"));
        }
        self.offset = offset;
        Ok(())
    }

    fn take(&mut self, count: usize) -> Result<&'a [u8], String> {
        let end = self
            .offset
            .checked_add(count)
            .ok_or_else(|| "Winline packet offset overflow".to_string())?;
        if end > self.bytes.len() {
            return Err(format!(
                "truncated Winline packet at {}: need {}, have {}",
                self.offset,
                count,
                self.remaining()
            ));
        }
        let result = &self.bytes[self.offset..end];
        self.offset = end;
        Ok(result)
    }

    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, String> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }

    fn i32(&mut self) -> Result<i32, String> {
        Ok(i32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn utf(&mut self) -> Result<String, String> {
        let len = self.u16()? as usize;
        let bytes = self.take(len)?;
        let end = bytes
            .iter()
            .position(|byte| *byte == 27)
            .unwrap_or(bytes.len());
        Ok(String::from_utf8_lossy(&bytes[..end]).into_owned())
    }
}

/// Connects to Winline's public line socket and returns a normalized snapshot.
pub async fn fetch() -> Result<(Vec<Event>, Vec<Odd>), Box<dyn std::error::Error + Send + Sync>> {
    let mut request = WS_URL.into_client_request()?;
    request
        .headers_mut()
        .insert(USER_AGENT, HeaderValue::from_static(WS_USER_AGENT));
    request
        .headers_mut()
        .insert(ORIGIN, HeaderValue::from_static("https://winline.ru"));
    request
        .headers_mut()
        .insert(REFERER, HeaderValue::from_static("https://winline.ru/line"));
    request.headers_mut().insert(
        ACCEPT_LANGUAGE,
        HeaderValue::from_static("ru-RU,ru;q=0.9,en;q=0.8"),
    );

    info!(
        ws_url = WS_URL,
        "Winline: connecting to public line WebSocket"
    );
    let (mut socket, response) = tokio::time::timeout(
        Duration::from_secs(12),
        tokio_tungstenite::connect_async(request),
    )
    .await
    .map_err(|_| "Winline WebSocket connection timed out after 12 seconds")??;
    if response.status() != tokio_tungstenite::tungstenite::http::StatusCode::SWITCHING_PROTOCOLS {
        return Err(format!(
            "Winline WebSocket handshake returned unexpected status: {}",
            response.status()
        )
        .into());
    }

    for command in ["lang", "AA==", "data", "WINLINE", "getdate"] {
        tokio::time::timeout(
            Duration::from_secs(3),
            socket.send(Message::Text(command.to_string().into())),
        )
        .await
        .map_err(|_| format!("Winline WebSocket send timed out for {command}"))??;
        info!(command, "Winline: initialization command sent");
    }

    let started = Instant::now();
    let mut state = FeedState::default();
    let mut last_message = Instant::now();
    let mut frames = 0usize;
    let mut frame_decode_errors = Vec::new();

    while started.elapsed() < MAX_CONNECT_TIME {
        let idle = last_message.elapsed();
        if state.menu_seen && state.prematch_seen && state.live_seen && idle >= READ_IDLE_TIMEOUT {
            break;
        }

        let remaining = MAX_CONNECT_TIME.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            break;
        }
        let wait_for = remaining.min(READ_IDLE_TIMEOUT);

        match tokio::time::timeout(wait_for, socket.next()).await {
            Ok(Some(Ok(Message::Binary(payload)))) => {
                last_message = Instant::now();
                frames += 1;
                if let Err(error) = state.process_frame(&payload) {
                    let dump_path =
                        format!("/tmp/winline-failed-frame-{frames}-{}.bin", payload.len());
                    let _ = std::fs::write(&dump_path, &payload);
                    warn!(frame = frames, payload_bytes = payload.len(), dump_path = %dump_path, %error, "Winline frame could not be decoded");
                    frame_decode_errors
                        .push(format!("frame {frames} ({} bytes): {error}", payload.len()));
                } else {
                    debug!(
                        frame = frames,
                        menu = state.menu_seen,
                        prematch = state.prematch_seen,
                        live = state.live_seen,
                        events = state.events.len(),
                        odds = state.odds.len(),
                        "Winline frame decoded"
                    );
                }
            }
            Ok(Some(Ok(Message::Text(_)))) => {
                last_message = Instant::now();
            }
            Ok(Some(Ok(Message::Ping(payload)))) => {
                last_message = Instant::now();
                socket.send(Message::Pong(payload)).await?;
            }
            Ok(Some(Ok(Message::Close(_)))) | Ok(None) => break,
            Ok(Some(Ok(_))) => {
                last_message = Instant::now();
            }
            Ok(Some(Err(error))) => return Err(error.into()),
            Err(_) => {
                if state.menu_seen && state.prematch_seen && state.live_seen {
                    break;
                }
            }
        }
    }

    let _ = socket.close(None).await;
    if state.incomplete_tail_records > 0 {
        warn!(
            skipped_records = state.incomplete_tail_records,
            "Winline: ignored only incomplete one-byte tail records after preserving complete records"
        );
    }
    let mut events = state.events.into_values().collect::<Vec<_>>();
    let mut odds = state.odds.into_values().collect::<Vec<_>>();
    events.sort_by(|left, right| left.id.cmp(&right.id));
    odds.sort_by(|left, right| left.id.cmp(&right.id));

    if !frame_decode_errors.is_empty() {
        return Err(format!(
            "Winline feed contained {} undecodable protocol frames: {}",
            frame_decode_errors.len(),
            frame_decode_errors.join("; ")
        )
        .into());
    }
    if !state.menu_seen {
        return Err("Winline did not send its market catalogue (GET_MENU)".into());
    }
    if !state.prematch_seen {
        return Err("Winline did not send its prematch feed".into());
    }
    if !state.live_seen {
        return Err("Winline did not send its live feed".into());
    }
    let live_events = events.iter().filter(|event| event.is_live).count();
    let prematch_events = events.len().saturating_sub(live_events);
    if live_events == 0 || prematch_events == 0 || odds.is_empty() {
        return Err(format!(
            "Winline feed returned incomplete data: {} live events, {} prematch events, {} odds, {} frames",
            live_events,
            prematch_events,
            odds.len(),
            frames
        )
        .into());
    }

    debug!(
        frames,
        events = events.len(),
        odds = odds.len(),
        "Winline WebSocket snapshot parsed"
    );
    Ok((events, odds))
}

fn is_single_byte_trailing_truncation(error: &str, remaining: usize) -> bool {
    if !error.contains("truncated Winline packet") {
        return false;
    }
    let Some(need_start) = error.rfind("need ") else {
        return false;
    };
    let need = error[need_start + 5..]
        .split(|character: char| !character.is_ascii_digit())
        .next()
        .and_then(|value| value.parse::<usize>().ok());
    let Some(have_start) = error.rfind("have ") else {
        return false;
    };
    let have = error[have_start + 5..]
        .split(|character: char| !character.is_ascii_digit())
        .next()
        .and_then(|value| value.parse::<usize>().ok());
    // The failed read must reach the exact end of this frame and be short by
    // precisely one byte. `need` is bounded to prevent treating absurd lengths
    // as a valid provider tail.
    matches!((need, have), (Some(need), Some(have))
        if need <= 256 && have == remaining && need == have.saturating_add(1))
}

impl FeedState {
    fn insert_event(&mut self, event: Event) {
        let event_id = event.id.clone();
        self.events.insert(event_id.clone(), event);
        if let Some(pending) = self.pending_odds.remove(&event_id) {
            self.odds.extend(pending);
        }
    }

    fn enforce_pending_odds_limit(&mut self) {
        while self.pending_odds.len() > MAX_PENDING_ODDS_EVENT_GROUPS {
            let oldest_event_id = self
                .pending_odds
                .iter()
                .map(|(event_id, quotes)| {
                    let oldest_timestamp = quotes
                        .values()
                        .map(|odd| odd.timestamp.timestamp_millis())
                        .min()
                        .unwrap_or(i64::MAX);
                    (event_id.clone(), oldest_timestamp)
                })
                .min_by_key(|(_, timestamp)| *timestamp)
                .map(|(event_id, _)| event_id);

            let Some(oldest_event_id) = oldest_event_id else {
                break;
            };
            self.pending_odds.remove(&oldest_event_id);
        }
    }

    fn remove_event(&mut self, mode: &str, source_id: u32) {
        let event_id = format!("winline-{mode}-{source_id}");
        self.events.remove(&event_id);
        self.odds.retain(|_, odd| odd.event_id != event_id);
        self.pending_odds.remove(&event_id);
    }

    fn remove_line(&mut self, mode: &str, line_id: u32) {
        let prefix = format!("winline-{mode}-");
        let marker = format!("-line-{line_id}-");
        let is_target_line = |id: &str| id.starts_with(&prefix) && id.contains(&marker);
        self.odds.retain(|id, _| !is_target_line(id));
        for pending in self.pending_odds.values_mut() {
            pending.retain(|id, _| !is_target_line(id));
        }
        self.pending_odds.retain(|_, pending| !pending.is_empty());
    }

    fn process_frame(&mut self, payload: &[u8]) -> Result<(), String> {
        let decoded = if payload.starts_with(&[0x1f, 0x8b]) {
            let mut decoder = GzDecoder::new(payload);
            let mut output = Vec::new();
            decoder
                .read_to_end(&mut output)
                .map_err(|error| format!("Winline gzip decompression failed: {error}"))?;
            output
        } else {
            payload.to_vec()
        };

        if decoded.len() < 2 {
            return Ok(());
        }
        let step = u16::from_le_bytes([decoded[0], decoded[1]]);
        let body = &decoded[2..];
        let result: Result<(), String> = match step {
            16 => {
                self.parse_menu(body)?;
                self.menu_seen = true;
                Ok(())
            }
            3 => {
                self.parse_prematch(body)
                    .map_err(|error| format!("Winline step {step}: {error}"))?;
                self.prematch_seen = true;
                Ok(())
            }
            4 if body.len() == 12 && body.iter().all(|byte| *byte == 0) => Ok(()),
            4 => {
                self.parse_live(body)
                    .map_err(|error| format!("Winline step {step}: {error}"))?;
                self.live_seen = true;
                Ok(())
            }
            _ => Ok(()),
        };
        result.map_err(|error| format!("Winline step {step}: {error}"))
    }

    fn parse_menu(&mut self, bytes: &[u8]) -> Result<(), String> {
        let mut reader = Reader::new(bytes);
        let sport_count = reader.u32()? as usize;
        if sport_count > 2_000 {
            return Err(format!("invalid Winline sport count: {sport_count}"));
        }
        for _ in 0..sport_count {
            let id = reader.i32()?;
            let _sort = reader.i32()?;
            let name = reader.utf()?;
            for _ in 0..9 {
                let _ = reader.utf()?;
            }
            self.sports.insert(id, SportDefinition { name });
        }

        let market_count = reader.u32()? as usize;
        if market_count > 20_000 {
            return Err(format!("invalid Winline market count: {market_count}"));
        }
        for _ in 0..market_count {
            let id = u16::try_from(reader.u32()?)
                .map_err(|_| "Winline market ID exceeds supported range".to_string())?;
            let _sports = reader.utf()?;
            let _favorite = reader.i32()?;
            let _count_v = reader.i32()?;
            let source_type = reader.i32()?;
            let name = reader.utf()?;
            let mut outcomes = Vec::with_capacity(30);
            for _ in 0..30 {
                outcomes.push(reader.utf()?);
            }
            self.markets.insert(
                id,
                MarketDefinition {
                    source_type,
                    name,
                    outcomes,
                },
            );
        }

        let tv_count = reader.u32()? as usize;
        if tv_count > 2_000 {
            return Err(format!("invalid Winline TV count: {tv_count}"));
        }
        for _ in 0..tv_count {
            let _id = reader.u32()?;
            let _name = reader.utf()?;
        }

        let variable_count = reader.u32()? as usize;
        if variable_count > 256 {
            return Err(format!("invalid Winline variable count: {variable_count}"));
        }
        for _ in 0..variable_count {
            let _id = reader.u8()?;
            let _value = reader.u32()?;
        }
        Ok(())
    }

    fn parse_prematch(&mut self, bytes: &[u8]) -> Result<(), String> {
        // The first 16 bytes contain version/position/margin metadata. The last
        // byte is the protocol trailer and is not part of the step sequence.
        if bytes.len() < 17 {
            return Err("Winline prematch frame is shorter than its header".to_string());
        }
        let payload = &bytes[..bytes.len() - 1];
        let mut reader = Reader::new(payload);
        reader.seek(16)?;
        let limit = payload.len();
        let mut current_championship = None;
        let mut current_event = None;

        while reader.offset < limit {
            if reader.remaining() < 2 {
                // A one-byte final trailer is not a step code.
                break;
            }
            let step_offset = reader.offset;
            let step = reader.u8()?;
            let step_result = (|| -> Result<(), String> {
                match step {
                    1 => {
                        let _id = reader.i32()?;
                        let _is_region = reader.i32()?;
                        let _name = reader.utf()?;
                        let _sort = reader.i32()?;
                        let _continent = reader.u8()?;
                        let _x = reader.u16()?;
                        let _y = reader.u16()?;
                        let _group = reader.u8()?;
                    }
                    2 => {
                        let championship = Self::read_prematch_championship(&mut reader)?;
                        current_championship = Some(championship.id);
                        self.championships.insert(championship.id, championship);
                    }
                    3 => {
                        if let Some(event) =
                            self.read_prematch_event(&mut reader, current_championship, false)?
                        {
                            current_event = Some(event.id.clone());
                            self.insert_event(event);
                        }
                    }
                    4 => {
                        if let Some(event_id) = current_event.as_deref() {
                            self.read_prematch_line(&mut reader, event_id, false)?;
                        } else {
                            return Err("Winline prematch line arrived before an event".to_string());
                        }
                    }
                    31 => {
                        let _event_id = reader.u32()?;
                        let _date = reader.u32()?;
                    }
                    32 => {
                        let event_id = reader.u32()?;
                        self.remove_event("prematch", event_id);
                    }
                    33 => {
                        let _event_id = reader.u32()?;
                        let _count_add_line = reader.u8()?;
                        let _is_od = reader.u8()?;
                    }
                    34 => {
                        if let Some(event) =
                            self.read_prematch_event(&mut reader, current_championship, true)?
                        {
                            current_event = Some(event.id.clone());
                            self.insert_event(event);
                        }
                    }
                    42 => {
                        let line_id = reader.u32()?;
                        self.remove_line("prematch", line_id);
                    }
                    43 => self.read_prematch_line(&mut reader, "", true)?,
                    _ if reader.remaining() == 0 => return Ok(()),
                    _ => {
                        return Err(format!(
                            "unknown Winline prematch step {step} at byte {}",
                            reader.offset.saturating_sub(1)
                        ));
                    }
                }
                Ok(())
            })();

            if let Err(error) = step_result {
                if is_single_byte_trailing_truncation(&error, reader.remaining()) {
                    self.incomplete_tail_records += 1;
                    warn!(
                        step,
                        offset = step_offset,
                        remaining = reader.remaining(),
                        %error,
                        "Winline: skipped one incomplete final prematch record"
                    );
                    break;
                }
                return Err(format!(
                    "prematch record step {step} at byte {step_offset}: {error}"
                ));
            }
        }
        Ok(())
    }

    fn read_prematch_championship(reader: &mut Reader<'_>) -> Result<Championship, String> {
        let id = reader.i32()?;
        let sport_id = reader.i32()?;
        let _country_id = reader.i32()?;
        let name = reader.utf()?;
        let _sort = reader.i32()?;
        let _code = reader.i32()?;
        let _count_special = reader.i32()?;
        let _level_on_site = reader.u8()?;
        let _sort_level = reader.i32()?;
        let _sort_new_level = reader.i32()?;
        Ok(Championship { id, sport_id, name })
    }

    fn read_prematch_event(
        &mut self,
        reader: &mut Reader<'_>,
        current_championship: Option<i32>,
        is_update: bool,
    ) -> Result<Option<Event>, String> {
        let source_id = reader.i32()?;
        let _radar_id = reader.u32()?;
        let native_id = reader.u32()?;
        let _provider = reader.u8()?;
        let _category = reader.u8()?;
        let _id_live = reader.u8()?;
        let _lines_availability = reader.u8()?;
        let _unused_a = reader.u8()?;
        let _unused_b = reader.u8()?;
        let _metadata = Self::read_metadata_block(reader)?;
        let championship_id = if is_update {
            reader.i32()?
        } else {
            current_championship.unwrap_or_default()
        };
        let timestamp = reader.u32()? as i64;
        let timestamp = timestamp + 29 - RUSSIA_UTC_OFFSET_SECONDS;
        let timestamp = timestamp.div_euclid(60) * 60;
        let _count_add_line = reader.u8()?;
        let _is_od = reader.u8()?;
        let home = reader.utf()?;
        let away = reader.utf()?;

        if source_id <= 0 || home.trim().is_empty() || away.trim().is_empty() {
            return Ok(None);
        }
        let Some(start_time) = DateTime::<Utc>::from_timestamp(timestamp, 0) else {
            return Ok(None);
        };
        Ok(Some(self.build_event(
            source_id,
            championship_id,
            home,
            away,
            Some(start_time),
            false,
            native_id,
            None,
        )))
    }

    fn read_prematch_line(
        &mut self,
        reader: &mut Reader<'_>,
        current_event_id: &str,
        is_update: bool,
    ) -> Result<(), String> {
        let line_id = reader.u32()?;
        let event_id = if is_update {
            reader.u32()?.to_string()
        } else {
            if !current_event_id.starts_with("winline-prematch-") {
                return Err("invalid Winline prematch event ID".to_string());
            }
            current_event_id.to_string()
        };
        let market_id = reader.u16()?;
        let _margin_a = reader.u16()?;
        let _margin_b = reader.u16()?;
        let Some(market) = self.markets.get(&market_id).cloned() else {
            return Err(format!("Winline market catalogue lacks ID {market_id}"));
        };

        let (koef, favorite) = Self::read_market_prefix(reader, market.source_type)?;
        // The menu carries the actual outcome labels (up to 30 slots). The
        // numeric source_type controls the line/favorite prefix, but it does
        // not reliably encode the number of prices for every market family.
        // Count declared outcomes first, keeping the legacy count only when a
        // menu record has no labels at all.
        let declared_outcomes = market
            .outcomes
            .iter()
            .filter(|outcome| !outcome.trim().is_empty())
            .count();
        let outcome_count = if declared_outcomes > 0 {
            declared_outcomes
        } else {
            match market.source_type {
                2 | 5 | 51 => 3,
                9 => 4,
                _ => 2,
            }
        };
        if outcome_count > 30 {
            return Err(format!(
                "invalid Winline market outcome count: {outcome_count}"
            ));
        }
        let mut values = Vec::with_capacity(outcome_count);
        for _ in 0..outcome_count {
            values.push(reader.u16()? as f64 / 100.0);
        }

        self.add_market_odds(
            "prematch", line_id, &event_id, market, &koef, favorite, values,
        );
        Ok(())
    }

    fn read_market_prefix(
        reader: &mut Reader<'_>,
        source_type: i32,
    ) -> Result<(String, u8), String> {
        let mut favorite = 0;
        let koef = match source_type {
            3 | 6 => {
                favorite = reader.u8()?;
                format!("{}", reader.u16()? as f64 / 100.0)
            }
            4 | 7 => format!("{}", reader.u16()? as f64 / 100.0),
            51 | 151 => "1".to_string(),
            61 => {
                favorite = reader.u8()?;
                format!("1/{}", reader.u16()? as f64 / 100.0)
            }
            71 => format!("1/{}", reader.u16()? as f64 / 100.0),
            _ => String::new(),
        };
        Ok((koef, favorite))
    }

    fn parse_live(&mut self, bytes: &[u8]) -> Result<(), String> {
        // Live packets have a 12-byte protocol header before the step stream.
        if bytes.len() < 13 {
            return Err("Winline live frame is shorter than its header".to_string());
        }
        let mut reader = Reader::new(bytes);
        reader.seek(12)?;
        while reader.remaining() > 0 {
            let step_offset = reader.offset;
            let step = reader.u8()?;
            match step {
                2 => {
                    let championship = Championship {
                        id: reader.i32()?,
                        sport_id: reader.i32()?,
                        // sorting, country, code, display metadata
                        name: String::new(),
                    };
                    let championship = Championship {
                        name: {
                            let _sort = reader.i32()?;
                            let _country = reader.i32()?;
                            let _code = reader.u8()?;
                            let name = reader.utf()?;
                            let _level = reader.u8()?;
                            let _sort_level = reader.i32()?;
                            let _sort_new_level = reader.i32()?;
                            name
                        },
                        ..championship
                    };
                    self.championships.insert(championship.id, championship);
                }
                3 => {
                    if let Some(event) = self
                        .read_live_event(&mut reader)
                        .map_err(|error| {
                        let preview = reader.bytes[step_offset..]
                            .iter().take(48)
                            .map(|byte| format!("{byte:02x}"))
                            .collect::<Vec<_>>().join(" ");
                        format!("live record step 3 at {step_offset} (remaining {}): {error}; bytes={preview}", reader.remaining())
                    })?
                    {
                        self.insert_event(event);
                    }
                }
                4 => self
                    .read_live_event_update(&mut reader)
                    .map_err(|error| {
                        let preview = reader.bytes[step_offset..]
                            .iter().take(48)
                            .map(|byte| format!("{byte:02x}"))
                            .collect::<Vec<_>>().join(" ");
                        format!("live record step 4 at {step_offset} (remaining {}): {error}; bytes={preview}", reader.remaining())
                    })?,
                5 => self
                    .read_live_line(&mut reader)
                    .map_err(|error| {
                        let preview = reader.bytes[step_offset..]
                            .iter().take(48)
                            .map(|byte| format!("{byte:02x}"))
                            .collect::<Vec<_>>().join(" ");
                        format!("live record step 5 at {step_offset} (remaining {}): {error}; bytes={preview}", reader.remaining())
                    })?,
                6 => {
                    let _event_id = reader.u32()?;
                    let _count = reader.u8()?;
                }
                _ => {
                    return Err(format!(
                        "unknown Winline live step {step} at byte {}",
                        reader.offset.saturating_sub(1)
                    ));
                }
            }
        }
        Ok(())
    }

    fn read_live_event(&mut self, reader: &mut Reader<'_>) -> Result<Option<Event>, String> {
        let source_id = reader.i32()?;
        let _radar_id = reader.u32()?;
        let native_id = reader.u32()?;
        let _provider = reader.u8()?;
        let _category = reader.u8()?;
        let _metadata = Self::read_metadata_block(reader)?;
        let _is_od = reader.u8()?;
        let _unused = reader.u8()?;
        let _duration = reader.u8()?;
        let state = reader.i32()?;
        if state > 3 {
            self.remove_event("live", source_id as u32);
            return Ok(None);
        }
        let championship_id = reader.u32()? as i32;
        let home = reader.utf()?;
        let away = reader.utf()?;
        let timestamp = reader.u32()? as i64 - RUSSIA_UTC_OFFSET_SECONDS;
        let clock = reader.utf()?;
        let cards = [reader.u8()?, reader.u8()?, reader.u8()?, reader.u8()?];
        let _feed = reader.u8()?;
        let score = reader.utf()?;
        let set_score = reader.utf()?;
        let add_info = reader.utf()?;
        let line_count = reader.u8()?;

        if source_id <= 0 || home.trim().is_empty() || away.trim().is_empty() {
            return Ok(None);
        }
        let Some(start_time) = DateTime::<Utc>::from_timestamp(timestamp, 0) else {
            return Ok(None);
        };
        Ok(Some(self.build_event(
            source_id,
            championship_id,
            home,
            away,
            Some(start_time),
            true,
            native_id,
            Some(serde_json::json!({
                "clock": clock,
                "score": score,
                "set_score": set_score,
                "additional_score": add_info,
                "cards": cards,
                "line_count": line_count,
            })),
        )))
    }

    fn read_live_event_update(&mut self, reader: &mut Reader<'_>) -> Result<(), String> {
        let source_id = reader.i32()?;
        let _metadata = Self::read_metadata_block(reader)?;
        let _is_od = reader.u8()?;
        let _unused = reader.u8()?;
        let _duration = reader.u8()?;
        let state = reader.i32()?;
        if source_id <= 0 {
            return Err(format!("invalid Winline live event update ID: {source_id}"));
        }
        // A state above 3 is a removal/terminal update. Such packets end
        // immediately after the state field and do not carry the score block.
        if state > 3 {
            self.remove_event("live", source_id as u32);
            return Ok(());
        }

        let clock = reader.utf()?;
        let cards = [reader.u8()?, reader.u8()?, reader.u8()?, reader.u8()?];
        let feed = reader.u8()?;
        let score = reader.utf()?;
        let set_score = reader.utf()?;
        let additional = reader.utf()?;
        let line_count = reader.u8()?;

        let event_id = format!("winline-live-{source_id}");
        if let Some(event) = self.events.get_mut(&event_id) {
            event
                .extra
                .insert("live_state".to_string(), serde_json::json!(state));
            event
                .extra
                .insert("clock".to_string(), serde_json::json!(clock));
            event
                .extra
                .insert("cards".to_string(), serde_json::json!(cards));
            event
                .extra
                .insert("feed".to_string(), serde_json::json!(feed));
            event
                .extra
                .insert("score".to_string(), serde_json::json!(score));
            event
                .extra
                .insert("set_score".to_string(), serde_json::json!(set_score));
            event.extra.insert(
                "additional_score".to_string(),
                serde_json::json!(additional),
            );
            event
                .extra
                .insert("line_count".to_string(), serde_json::json!(line_count));
        }
        Ok(())
    }

    fn read_live_line(&mut self, reader: &mut Reader<'_>) -> Result<(), String> {
        let line_id = reader.u32()?;
        let state = reader.u8()?;
        if state == 5 {
            self.remove_line("live", line_id);
            return Ok(());
        }
        let event_id = reader.u32()?;
        let count = reader.u8()? as usize;
        if count > 31 {
            return Err(format!("invalid Winline live line outcome count: {count}"));
        }
        let mut values = Vec::with_capacity(count);
        for _ in 0..count {
            values.push(reader.u16()? as f64 / 100.0);
        }
        let market_id = reader.u16()?;
        let koef = reader.utf()?;
        let favorite = reader.u8()?;
        let Some(market) = self.markets.get(&market_id).cloned() else {
            return Err(format!(
                "Winline market catalogue lacks live ID {market_id}"
            ));
        };
        let event_key = format!("winline-live-{event_id}");
        self.add_market_odds("live", line_id, &event_key, market, &koef, favorite, values);
        Ok(())
    }

    fn read_metadata_block(reader: &mut Reader<'_>) -> Result<HashMap<String, String>, String> {
        let size = reader.u16()? as usize;
        let end = reader
            .offset
            .checked_add(size)
            .ok_or_else(|| "Winline metadata block overflow".to_string())?;
        if end > reader.bytes.len() {
            return Err("truncated Winline metadata block".to_string());
        }
        let mut flags = HashMap::new();
        while reader.offset < end {
            let kind = reader.u8()?;
            let length = reader.u8()? as usize;
            let block_end = reader.offset + length;
            if block_end > end {
                return Err("Winline metadata sub-block exceeds parent".to_string());
            }
            match kind {
                1 => {
                    let mut channels = Vec::new();
                    while reader.offset < block_end {
                        channels.push(reader.u16()?.to_string());
                    }
                    flags.insert("tv".to_string(), channels.join(","));
                }
                2 if length >= 2 => {
                    let video = reader.u8()?;
                    let stats = reader.u8()?;
                    flags.insert("video".to_string(), (video > 0).to_string());
                    flags.insert("stats".to_string(), (stats > 0).to_string());
                }
                3 if length >= 1 => {
                    flags.insert("xds_version".to_string(), reader.u8()?.to_string());
                }
                _ => {
                    let _ = reader.take(length)?;
                }
            }
            if reader.offset != block_end {
                reader.seek(block_end)?;
            }
        }
        Ok(flags)
    }

    fn build_event(
        &self,
        source_id: i32,
        championship_id: i32,
        home: String,
        away: String,
        start_time: Option<DateTime<Utc>>,
        is_live: bool,
        native_id: u32,
        extra: Option<serde_json::Value>,
    ) -> Event {
        let championship = self.championships.get(&championship_id);
        let sport_id = championship.map(|item| item.sport_id).unwrap_or_default();
        let sport_name = self
            .sports
            .get(&sport_id)
            .map(|item| item.name.as_str())
            .unwrap_or_default();
        let mut metadata = serde_json::Map::new();
        metadata.insert("source_event_id".to_string(), serde_json::json!(source_id));
        metadata.insert(
            "championship_id".to_string(),
            serde_json::json!(championship_id),
        );
        metadata.insert("native_id".to_string(), serde_json::json!(native_id));
        if let Some(extra) = extra {
            if let Some(extra) = extra.as_object() {
                metadata.extend(extra.clone());
            }
        }
        let mode = if is_live { "live" } else { "prematch" };
        Event {
            id: format!("winline-{mode}-{source_id}"),
            sport: Sport::from_str(sport_name),
            league: championship
                .map(|item| item.name.clone())
                .unwrap_or_else(|| "Unknown championship".to_string()),
            home_team: home,
            away_team: away,
            start_time,
            is_live,
            bookmaker_slug: "winline".to_string(),
            raw_url: None,
            extra: metadata
                .into_iter()
                .map(|(key, value)| (key, value))
                .collect(),
        }
    }

    fn add_market_odds(
        &mut self,
        _mode: &str,
        line_id: u32,
        event_key: &str,
        market: MarketDefinition,
        koef: &str,
        _favorite: u8,
        values: Vec<f64>,
    ) {
        let event_is_known = self.events.contains_key(event_key);
        let (market_name, line) = Self::normalize_market(&market, koef, 0);
        if Self::market_requires_line(&market, &market_name) && line.is_none() {
            warn!(
                event_id = event_key,
                market = %market_name,
                source_type = market.source_type,
                "Winline total/handicap omitted because its line is missing"
            );
            return;
        }
        let timestamp = Utc::now();
        for (index, price) in values.into_iter().enumerate() {
            if !price.is_finite() || price <= 1.0 {
                continue;
            }
            let Some(source_selection) = market.outcomes.get(index) else {
                continue;
            };
            if source_selection.trim().is_empty() {
                continue;
            }
            let (selection, odds_type) = Self::normalize_selection(source_selection, &market_name);
            if selection.is_empty() {
                continue;
            }
            let id = format!("{event_key}-line-{line_id}-outcome-{index}");
            let odd = Odd {
                id: id.clone(),
                event_id: event_key.to_string(),
                bookmaker_slug: "winline".to_string(),
                market: market_name.clone(),
                selection,
                odds: price,
                odds_type,
                line,
                timestamp,
            };
            if event_is_known {
                self.odds.insert(id, odd);
            } else {
                self.pending_odds
                    .entry(event_key.to_string())
                    .or_default()
                    .insert(id, odd);
                self.enforce_pending_odds_limit();
            }
        }
    }

    fn normalize_market(
        market: &MarketDefinition,
        koef: &str,
        _favorite: u8,
    ) -> (String, Option<f64>) {
        let raw = market.name.trim();
        let lower = raw.to_lowercase().replace('ё', "е");

        let period_prefix = if [
            "1-й тайм",
            "1-го тайма",
            "1-м тайме",
            "1 тайм",
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
            "1h",
            "1 h",
        ]
        .iter()
        .any(|marker| lower.contains(marker))
        {
            Some("1H_")
        } else if [
            "2-й тайм",
            "2-го тайма",
            "2-м тайме",
            "2 тайм",
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
            "2h",
            "2 h",
        ]
        .iter()
        .any(|marker| lower.contains(marker))
        {
            Some("2H_")
        } else {
            None
        };

        let team_scope = if [
            "команды 1",
            "команда 1",
            "команды №1",
            "1-й команды",
            "первой команды",
            "хозяев",
            "home team",
            "team 1",
            "team1",
            "1к",
            "1 к",
            "ит1",
            "ит 1",
            "итб1",
            "итм1",
            "итб 1",
            "итм 1",
        ]
        .iter()
        .any(|marker| lower.contains(marker))
        {
            Some("Team1")
        } else if [
            "команды 2",
            "команда 2",
            "команды №2",
            "2-й команды",
            "второй команды",
            "гостей",
            "away team",
            "team 2",
            "team2",
            "2к",
            "2 к",
            "ит2",
            "ит 2",
            "итб2",
            "итм2",
            "итб 2",
            "итм 2",
        ]
        .iter()
        .any(|marker| lower.contains(marker))
        {
            Some("Team2")
        } else {
            None
        };

        let is_individual_total = lower.contains("индивидуальн")
            || lower.contains("инд. тотал")
            || lower.contains("инд тотал")
            || lower.contains("итб")
            || lower.contains("итм")
            || lower.contains("ит 1")
            || lower.contains("ит 2");
        let is_double_chance = lower.contains("двойн") || lower.contains("double chance");
        let is_btts = lower.contains("обе забьют")
            || lower.contains("обе команды забьют")
            || lower.contains("both teams score")
            || lower.contains("btts");
        let is_even_odd = lower.contains("чёт/нечёт")
            || lower.contains("чет/нечет")
            || lower.contains("even/odd")
            || lower.contains("четность")
            || lower.contains("чётность");

        let mut category = if lower.contains("исход 1x2") || market.source_type == 2 {
            "1X2".to_string()
        } else if lower.contains("исход 12") || market.source_type == 1 {
            "MatchWinner12".to_string()
        } else if is_double_chance {
            "DoubleChance".to_string()
        } else if is_btts {
            "BothTeamsScore".to_string()
        } else if is_even_odd {
            "EvenOdd".to_string()
        } else if lower.contains("тотал") || matches!(market.source_type, 4 | 7 | 71) {
            match team_scope {
                Some(team) => format!("{team}Total"),
                None if is_individual_total => format!("Custom:{}", raw),
                None => "Total".to_string(),
            }
        } else if lower.contains("фора")
            || lower.contains("гандикап")
            || matches!(market.source_type, 3 | 6 | 61)
        {
            match team_scope {
                Some(team) => format!("{team}Handicap"),
                None => "Handicap".to_string(),
            }
        } else if lower.contains("победител") || lower.contains("winner") {
            if lower.contains("карты") || lower.contains("map") {
                "MapWinner".to_string()
            } else {
                "Winner".to_string()
            }
        } else {
            raw.to_string()
        };

        if let Some(prefix) = period_prefix {
            category = format!("{prefix}{category}");
        } else if lower.contains("период") || lower.contains("period") {
            // Period captions need an explicit period number (carried in the
            // protocol's coefficient prefix) before they can be comparable.
            if !koef
                .split('/')
                .next()
                .is_some_and(|value| value.trim().parse::<u8>().is_ok())
            {
                category = format!("Custom:{}", raw);
            }
        }

        let parts = koef.split('/').collect::<Vec<_>>();
        let line_value = parts
            .last()
            .and_then(|value| value.trim().parse::<f64>().ok());
        if parts.len() > 1 {
            if let Ok(period) = parts[0].trim().parse::<u8>() {
                category = format!("{category}:period-{period}");
            }
        }
        let line = line_value.filter(|value| value.is_finite());
        (category, line)
    }

    fn market_requires_line(market: &MarketDefinition, category: &str) -> bool {
        if matches!(market.source_type, 3 | 4 | 6 | 7 | 61 | 71) {
            return true;
        }

        let base = category
            .strip_prefix("1H_")
            .or_else(|| category.strip_prefix("2H_"))
            .unwrap_or(category)
            .split(":period-")
            .next()
            .unwrap_or(category);
        matches!(
            base,
            "Total"
                | "Handicap"
                | "Team1Total"
                | "Team2Total"
                | "Team1Handicap"
                | "Team2Handicap"
        )
    }

    fn normalize_selection(selection: &str, market: &str) -> (String, OddsType) {
        let trimmed = selection.trim();
        let lower = trimmed.to_lowercase().replace('ё', "е");
        let base = market
            .strip_prefix("1H_")
            .or_else(|| market.strip_prefix("2H_"))
            .unwrap_or(market)
            .split(":period-")
            .next()
            .unwrap_or(market);

        if base.starts_with("Handicap") || base.ends_with("Handicap") {
            let side = if lower == "1"
                || lower == "п1"
                || lower.starts_with("ф1")
                || lower == "home"
                || lower.contains("команда 1")
            {
                "1"
            } else if lower == "2"
                || lower == "п2"
                || lower.starts_with("ф2")
                || lower == "away"
                || lower.contains("команда 2")
            {
                "2"
            } else {
                return (String::new(), OddsType::Custom);
            };
            return (side.to_string(), OddsType::Handicap);
        }

        if base == "DoubleChance" {
            let compact = lower
                .replace('х', "x")
                .replace(' ', "")
                .replace('-', "")
                .replace('/', "");
            return match compact.as_str() {
                "1x" => ("1X".to_string(), OddsType::Custom),
                "x2" => ("X2".to_string(), OddsType::Custom),
                "12" => ("12".to_string(), OddsType::Custom),
                _ => (String::new(), OddsType::Custom),
            };
        }

        if base == "BothTeamsScore" {
            return match lower.as_str() {
                "да" | "yes" | "y" => {
                    ("Yes".to_string(), OddsType::BothTeamsScoreYes)
                }
                "нет" | "no" | "n" => {
                    ("No".to_string(), OddsType::BothTeamsScoreNo)
                }
                _ => (String::new(), OddsType::Custom),
            };
        }

        if base == "EvenOdd" {
            return match lower.as_str() {
                "чёт" | "чет" | "even" => ("Even".to_string(), OddsType::Even),
                "нечёт" | "нечет" | "odd" => ("Odd".to_string(), OddsType::Odd),
                _ => (String::new(), OddsType::Custom),
            };
        }

        if base == "Total" || base.ends_with("Total") {
            return match lower.as_str() {
                "больше" | "тб" | "итб" | "over" => ("Over".to_string(), OddsType::Over),
                "меньше" | "тм" | "итм" | "under" => ("Under".to_string(), OddsType::Under),
                _ => (String::new(), OddsType::Custom),
            };
        }

        match lower.as_str() {
            "1" | "п1" | "home" => ("1".to_string(), OddsType::Home),
            "x" | "н" | "ничья" | "draw" => ("X".to_string(), OddsType::Draw),
            "2" | "п2" | "away" => ("2".to_string(), OddsType::Away),
            _ => (trimmed.to_string(), OddsType::Custom),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        is_single_byte_trailing_truncation, FeedState, MarketDefinition, Reader,
        MAX_PENDING_ODDS_EVENT_GROUPS,
    };
    use shared::odds::OddsType;
    use shared::{Event, Sport};
    use std::collections::HashMap;

    #[test]
    fn reader_decodes_little_endian_and_utf8_strings() {
        let mut bytes = vec![0x34, 0x12, 0x78, 0x56, 0x34, 0x12, 0x08, 0x00];
        bytes.extend_from_slice("тест".as_bytes());
        let mut reader = Reader::new(&bytes);
        assert_eq!(reader.u16().unwrap(), 0x1234);
        assert_eq!(reader.u32().unwrap(), 0x12345678);
        assert_eq!(reader.utf().unwrap(), "тест");
        assert_eq!(reader.remaining(), 0);
    }

    #[test]
    fn normalizes_common_winline_markets_and_periods() {
        let market = MarketDefinition {
            source_type: 2,
            name: "Исход 1X2 (@NP@)".to_string(),
            outcomes: vec!["1".into(), "X".into(), "2".into()],
        };
        let (name, line) = FeedState::normalize_market(&market, "", 0);
        assert_eq!(name, "1X2");
        assert_eq!(line, None);

        let total = MarketDefinition {
            source_type: 4,
            name: "Тотал [a] (@NP@)".to_string(),
            outcomes: vec!["Больше".into(), "Меньше".into()],
        };
        let (name, line) = FeedState::normalize_market(&total, "2.5", 0);
        assert_eq!(name, "Total");
        assert_eq!(line, Some(2.5));

        let period = MarketDefinition {
            source_type: 61,
            name: "@[a]P@ фора [b]".to_string(),
            outcomes: vec!["1".into(), "2".into()],
        };
        let (name, line) = FeedState::normalize_market(&period, "3/-2.5", 1);
        assert_eq!(name, "Handicap:period-3");
        assert_eq!(line, Some(-2.5));
    }

    #[test]
    fn normalizes_team_totals_and_half_scoped_winline_markets() {
        let market = |name: &str, source_type| MarketDefinition {
            source_type,
            name: name.to_string(),
            outcomes: vec!["Больше".into(), "Меньше".into()],
        };

        let (team1, _) =
            FeedState::normalize_market(&market("Индивидуальный тотал команды 1", 4), "2.5", 0);
        let (team2, _) =
            FeedState::normalize_market(&market("Индивидуальный тотал команды 2", 4), "2.5", 0);
        assert_eq!(team1, "Team1Total");
        assert_eq!(team2, "Team2Total");

        let (first_half, _) =
            FeedState::normalize_market(&market("Тотал первого тайма", 4), "2.5", 0);
        let (second_half, _) =
            FeedState::normalize_market(&market("Total second half", 4), "2.5", 0);
        assert_eq!(first_half, "1H_Total");
        assert_eq!(second_half, "2H_Total");
    }

    #[test]
    fn requires_explicit_lines_for_winline_totals_and_handicaps() {
        let total = MarketDefinition {
            source_type: 4,
            name: "Тотал".into(),
            outcomes: vec!["Больше".into(), "Меньше".into()],
        };
        let handicap = MarketDefinition {
            source_type: 3,
            name: "Фора".into(),
            outcomes: vec!["1".into(), "2".into()],
        };
        let (_, total_line) = FeedState::normalize_market(&total, "", 0);
        let (_, handicap_line) = FeedState::normalize_market(&handicap, "", 0);
        assert!(FeedState::market_requires_line(&total, "Total"));
        assert!(FeedState::market_requires_line(&handicap, "Handicap"));
        assert!(total_line.is_none());
        assert!(handicap_line.is_none());

        let mut state = FeedState::default();
        state.add_market_odds(
            "prematch",
            1,
            "winline-prematch-41",
            total,
            "",
            0,
            vec![1.8, 2.0],
        );
        assert!(state.odds.is_empty());
        assert!(state.pending_odds.is_empty());

        let mut state = FeedState::default();
        let handicap = MarketDefinition {
            source_type: 3,
            name: "Фора".into(),
            outcomes: vec!["1".into(), "2".into()],
        };
        state.add_market_odds(
            "prematch",
            2,
            "winline-prematch-42",
            handicap,
            "",
            0,
            vec![1.9, 1.9],
        );
        assert!(state.odds.is_empty());
        assert!(state.pending_odds.is_empty());
    }

    #[test]
    fn normalizes_double_chance_and_segment_outcomes() {
        assert_eq!(
            FeedState::normalize_selection("1Х", "DoubleChance"),
            ("1X".to_string(), OddsType::Custom)
        );
        assert_eq!(
            FeedState::normalize_selection("Х2", "DoubleChance"),
            ("X2".to_string(), OddsType::Custom)
        );
        assert_eq!(
            FeedState::normalize_selection("12", "DoubleChance"),
            ("12".to_string(), OddsType::Custom)
        );
        assert_eq!(
            FeedState::normalize_selection("Да", "1H_BothTeamsScore"),
            ("Yes".to_string(), OddsType::BothTeamsScoreYes)
        );
        assert_eq!(
            FeedState::normalize_selection("Нечёт", "2H_EvenOdd"),
            ("Odd".to_string(), OddsType::Odd)
        );
        assert_eq!(
            FeedState::normalize_selection("Ф1 (+1.5)", "1H_Handicap"),
            ("1".to_string(), OddsType::Handicap)
        );
    }

    #[test]
    fn prematch_line_reads_the_declared_number_of_market_outcomes() {
        let mut state = FeedState::default();
        let event_id = "winline-prematch-42".to_string();
        state.events.insert(
            event_id.clone(),
            Event {
                id: event_id.clone(),
                sport: Sport::Football,
                league: "Test league".into(),
                home_team: "Home".into(),
                away_team: "Away".into(),
                start_time: None,
                is_live: false,
                bookmaker_slug: "winline".into(),
                raw_url: None,
                extra: HashMap::new(),
            },
        );
        state.markets.insert(
            931,
            MarketDefinition {
                source_type: 4,
                name: "Total special one-way".into(),
                outcomes: vec!["Over".into()],
            },
        );

        let mut bytes = Vec::new();
        bytes.extend_from_slice(&123_u32.to_le_bytes());
        bytes.extend_from_slice(&931_u16.to_le_bytes());
        bytes.extend_from_slice(&10_000_u16.to_le_bytes());
        bytes.extend_from_slice(&10_000_u16.to_le_bytes());
        bytes.extend_from_slice(&250_u16.to_le_bytes()); // line 2.50
        bytes.extend_from_slice(&235_u16.to_le_bytes()); // one price 2.35
        let mut reader = Reader::new(&bytes);

        state
            .read_prematch_line(&mut reader, &event_id, false)
            .expect("one-outcome prematch line");
        assert_eq!(reader.remaining(), 0);
        assert_eq!(state.odds.len(), 1);
        assert_eq!(state.odds.values().next().unwrap().odds, 2.35);
    }

    #[test]
    fn incomplete_one_byte_tail_is_detected_but_larger_corruption_is_not() {
        assert!(is_single_byte_trailing_truncation(
            "truncated Winline packet at 99: need 2; have 1",
            1
        ));
        assert!(is_single_byte_trailing_truncation(
            "truncated Winline packet at 99: need 1; have 0",
            0
        ));
        assert!(!is_single_byte_trailing_truncation(
            "truncated Winline packet at 99: need 4; have 2",
            2
        ));
        assert!(!is_single_byte_trailing_truncation(
            "truncated Winline packet at 99: need 2; have 1",
            5
        ));
        assert!(is_single_byte_trailing_truncation(
            "prematch record step 34: truncated Winline packet at 5763: need 20; have 19",
            19
        ));
        assert!(!is_single_byte_trailing_truncation(
            "unknown Winline market ID 99",
            1
        ));
    }

    #[test]
    fn empty_live_control_frame_is_ignored_without_losing_live_feed_state() {
        let mut state = FeedState::default();
        let mut decoded = Vec::new();
        decoded.extend_from_slice(&4_u16.to_le_bytes());
        decoded.extend_from_slice(&[0_u8; 12]);
        state
            .process_frame(&decoded)
            .expect("empty live control frame");
        assert!(!state.live_seen);
        assert!(state.events.is_empty());
        assert!(state.odds.is_empty());
    }

    #[test]
    fn terminal_live_event_update_does_not_read_absent_score_fields() {
        let mut state = FeedState::default();
        let event_id = "winline-live-42".to_string();
        state.events.insert(
            event_id.clone(),
            Event {
                id: event_id,
                sport: Sport::Football,
                league: "Test league".into(),
                home_team: "Home".into(),
                away_team: "Away".into(),
                start_time: None,
                is_live: true,
                bookmaker_slug: "winline".into(),
                raw_url: None,
                extra: HashMap::new(),
            },
        );

        // source_id, empty metadata, is_od, unused, duration, terminal state.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&42_i32.to_le_bytes());
        bytes.extend_from_slice(&0_u16.to_le_bytes());
        bytes.extend_from_slice(&[0, 0, 0]);
        bytes.extend_from_slice(&5_i32.to_le_bytes());
        let mut reader = Reader::new(&bytes);

        state
            .read_live_event_update(&mut reader)
            .expect("terminal update");
        assert!(state.events.is_empty());
        assert_eq!(reader.remaining(), 0);
    }

    #[test]
    fn live_line_arriving_before_event_is_buffered_then_attached() {
        let mut state = FeedState::default();
        let event_id = "winline-live-42";
        let market = MarketDefinition {
            source_type: 2,
            name: "Исход 1X2".into(),
            outcomes: vec!["1".into(), "X".into(), "2".into()],
        };

        state.add_market_odds("live", 123, event_id, market, "", 0, vec![2.1, 3.2, 3.4]);

        assert!(state.odds.is_empty());
        assert_eq!(state.pending_odds.get(event_id).map(HashMap::len), Some(3));

        state.insert_event(Event {
            id: event_id.into(),
            sport: Sport::Football,
            league: "Test league".into(),
            home_team: "Home".into(),
            away_team: "Away".into(),
            start_time: None,
            is_live: true,
            bookmaker_slug: "winline".into(),
            raw_url: None,
            extra: HashMap::new(),
        });

        assert_eq!(state.odds.len(), 3);
        assert!(state.pending_odds.is_empty());
        assert!(state.odds.values().all(|odd| odd.event_id == event_id));
    }

    #[test]
    fn pending_live_quote_buffer_is_bounded() {
        let mut state = FeedState::default();
        let market = MarketDefinition {
            source_type: 2,
            name: "Исход 1X2".into(),
            outcomes: vec!["1".into()],
        };

        for event_id in 0..(MAX_PENDING_ODDS_EVENT_GROUPS + 37) {
            state.add_market_odds(
                "live",
                event_id as u32,
                &format!("winline-live-{event_id}"),
                market.clone(),
                "",
                0,
                vec![2.1],
            );
        }

        assert_eq!(state.pending_odds.len(), MAX_PENDING_ODDS_EVENT_GROUPS);
        assert!(state.pending_odds.contains_key(&format!(
            "winline-live-{}",
            MAX_PENDING_ODDS_EVENT_GROUPS + 36
        )));
        assert!(state.odds.is_empty());
    }

    #[test]
    fn removing_event_or_line_also_clears_buffered_live_quotes() {
        let mut state = FeedState::default();
        let market = MarketDefinition {
            source_type: 2,
            name: "Исход 1X2".into(),
            outcomes: vec!["1".into(), "X".into(), "2".into()],
        };

        state.add_market_odds(
            "live",
            123,
            "winline-live-42",
            market.clone(),
            "",
            0,
            vec![2.1, 3.2, 3.4],
        );
        state.add_market_odds(
            "live",
            124,
            "winline-live-43",
            market,
            "",
            0,
            vec![2.2, 3.1, 3.5],
        );
        assert_eq!(state.pending_odds.len(), 2);

        state.remove_line("live", 123);
        assert!(!state.pending_odds.contains_key("winline-live-42"));
        assert!(state.pending_odds.contains_key("winline-live-43"));

        state.remove_event("live", 43);
        assert!(state.pending_odds.is_empty());
    }

    #[test]
    fn removing_line_and_event_also_removes_their_odds() {
        let mut state = FeedState::default();
        let event_id = "winline-live-42".to_string();
        state.events.insert(
            event_id.clone(),
            Event {
                id: event_id.clone(),
                sport: Sport::Football,
                league: "Test league".into(),
                home_team: "Home".into(),
                away_team: "Away".into(),
                start_time: None,
                is_live: true,
                bookmaker_slug: "winline".into(),
                raw_url: None,
                extra: HashMap::new(),
            },
        );
        state.markets.insert(
            490,
            MarketDefinition {
                source_type: 2,
                name: "Исход 1X2".into(),
                outcomes: vec!["1".into(), "X".into(), "2".into()],
            },
        );
        state.add_market_odds(
            "live",
            123,
            &event_id,
            state.markets.get(&490).unwrap().clone(),
            "",
            0,
            vec![2.1, 3.2, 3.4],
        );
        state.add_market_odds(
            "live",
            124,
            &event_id,
            state.markets.get(&490).unwrap().clone(),
            "",
            0,
            vec![2.2, 3.1, 3.5],
        );
        assert_eq!(state.odds.len(), 6);

        state.remove_line("live", 123);
        assert_eq!(state.odds.len(), 3);
        assert!(state
            .odds
            .keys()
            .all(|id| id.starts_with("winline-live-42-line-124-")));

        state.remove_event("live", 42);
        assert!(state.events.is_empty());
        assert!(state.odds.is_empty());
    }

    #[test]
    fn line_ids_are_scoped_by_event_and_line_updates_are_not_discarded() {
        fn append_utf(bytes: &mut Vec<u8>, value: &str) {
            bytes.extend_from_slice(&(value.len() as u16).to_le_bytes());
            bytes.extend_from_slice(value.as_bytes());
        }

        let mut state = FeedState::default();
        for source_id in [42, 43] {
            let event_id = format!("winline-live-{source_id}");
            state.events.insert(
                event_id.clone(),
                Event {
                    id: event_id,
                    sport: Sport::Football,
                    league: "Test league".into(),
                    home_team: format!("Home {source_id}"),
                    away_team: format!("Away {source_id}"),
                    start_time: None,
                    is_live: true,
                    bookmaker_slug: "winline".into(),
                    raw_url: None,
                    extra: HashMap::new(),
                },
            );
        }
        let market = MarketDefinition {
            source_type: 2,
            name: "Исход 1X2".into(),
            outcomes: vec!["1".into(), "X".into(), "2".into()],
        };
        for event_id in ["winline-live-42", "winline-live-43"] {
            state.add_market_odds(
                "live",
                123,
                event_id,
                market.clone(),
                "",
                0,
                vec![2.1, 3.2, 3.4],
            );
        }
        assert_eq!(state.odds.len(), 6);
        state.remove_line("live", 123);
        assert!(state.odds.is_empty());

        let event_id = "winline-live-42";
        state
            .events
            .get_mut(event_id)
            .unwrap()
            .extra
            .insert("clock".into(), serde_json::json!("00:12"));
        // A state update to a finished/cancelled match removes its event and every quote.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&42_i32.to_le_bytes());
        bytes.extend_from_slice(&0_u16.to_le_bytes()); // empty metadata block
        bytes.extend_from_slice(&[0, 0, 0]);
        bytes.extend_from_slice(&4_i32.to_le_bytes()); // finished state
        append_utf(&mut bytes, "90:00");
        bytes.extend_from_slice(&[0, 0, 0, 0, 0]); // cards and feed
        append_utf(&mut bytes, "2:1");
        append_utf(&mut bytes, "");
        append_utf(&mut bytes, "");
        bytes.push(0);
        let mut reader = Reader::new(&bytes);
        state.read_live_event_update(&mut reader).unwrap();
        assert!(!state.events.contains_key(event_id));
    }

    #[test]
    fn selection_mapping_is_market_aware() {
        let (home, home_type) = FeedState::normalize_selection("1", "1X2");
        let (draw, draw_type) = FeedState::normalize_selection("X", "1X2");
        let (away, away_type) = FeedState::normalize_selection("2", "1X2");
        let (over, over_type) = FeedState::normalize_selection("Больше", "Total");
        let (yes, yes_type) = FeedState::normalize_selection("Да", "BothTeamsScore");
        assert_eq!((home.as_str(), home_type), ("1", OddsType::Home));
        assert_eq!((draw.as_str(), draw_type), ("X", OddsType::Draw));
        assert_eq!((away.as_str(), away_type), ("2", OddsType::Away));
        assert_eq!((over.as_str(), over_type), ("Over", OddsType::Over));
        assert_eq!(
            (yes.as_str(), yes_type),
            ("Yes", OddsType::BothTeamsScoreYes)
        );
        let (even, even_type) = FeedState::normalize_selection("Чёт", "EvenOdd");
        let (odd, odd_type) = FeedState::normalize_selection("Нечёт", "EvenOdd");
        assert_eq!((even.as_str(), even_type), ("Even", OddsType::Even));
        assert_eq!((odd.as_str(), odd_type), ("Odd", OddsType::Odd));
    }
}
