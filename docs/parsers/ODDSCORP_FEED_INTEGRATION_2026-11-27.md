# ODDSCORP feed integration — 2026-11-27

## Verified provider contract

The public ODDSCORP API reference currently documents:
- GET /bookmakers — the bookmakers actually available under the supplied subscription token.
- GET /events?bk_name={bookmaker} — the bookmaker's live event catalogue.
- GET /events?bk_name={bookmaker}&live=0 — the pre-match event catalogue.
- GET /markets/{bk_event_id} — the markets and prices for one provider event.
- A market contains bet, cf, blocked, meta, updated_at and updated_at_ms.
- An event contains a provider event key (bk_event_id), the bookmaker-native ID (bk_event_native_id), the bookmaker slug, and a provider cross-bookmaker group ID (event_id).

Authentication is documented as a URI token. The public API examples use plain HTTP; request an HTTPS endpoint from the provider before using a real token. The client blocks cleartext HTTP by default and requires an explicit ODDSCORP_ALLOW_INSECURE_HTTP=1 opt-in. The provider's docs explicitly say the available bookmaker list is limited to the bookmakers included in the active subscription. Its metadata format varies by bookmaker and sport.

## Current implementation

The repository now has a guarded transport/data-contract client in crates/parsers/src/oddscorp.rs.

It:
- reads ODDSCORP_API_TOKEN, optional ODDSCORP_BASE_URL, and ODDSCORP_ALLOW_INSECURE_HTTP;
- requires HTTPS by default. The provider's public examples currently show HTTP with a URI token, so cleartext transport is rejected unless ODDSCORP_ALLOW_INSECURE_HTTP=1 is explicitly set after accepting the token-exposure risk;
- does not print the token in Debug output or request errors;
- fetches subscribed bookmakers, live/pre-match event lists, and per-event markets;
- tolerates numeric or string price payloads;
- parses bookmaker-specific metadata without pretending there is one universal schema;
- exposes the provider's market-update timestamp separately from local ingestion time;
- rejects blocked/invalid prices and unsafe event IDs;
- includes fixture tests for event identity, market metadata, source timestamps, and invalid prices.

The client is intentionally not registered in ParserFactory and cannot enable production scanning. Direct bookmaker parsers remain the primary sources. A provider-specific bookmaker adapter is a separate source lane and must preserve provenance (source provider, bookmaker, native event ID, provider event ID, provider group ID, and market metadata).

## Runtime gate before connecting a bookmaker

1. Obtain a trial key from ODDSCORP. The provider advertises a two-week trial via @oddscorp; the account must explicitly include the target bookmakers.
2. Call /bookmakers and save the actual returned subscription list.
3. For BetBoom, Liga Stavok, MelBet and Bet-M, verify exact Russian bookmaker identity and live/prematch availability. A similar brand or a bookmaker with a similar slug is not sufficient.
4. Compare sample event and market data against the operator's own published line where access is permitted.
5. Verify that updated_at_ms advances when a price changes, that blocked outcomes are filtered, and that event/group/native IDs remain stable.
6. Confirm request limits, WebSocket availability, latency, commercial use and redistribution terms in writing.
7. Only then add an adapter and runtime smoke test for the confirmed bookmaker.

## Current blocker

No ODDSCORP trial key or returned live payload is present in this repository. The code is therefore an integration client with fixture-level validation, not a claim that any of the blocked bookmaker feeds is working. API data must not be silently relabelled as a direct operator feed.
