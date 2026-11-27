# Direct bookmaker parser policy and work queue — 2026-11-27

## Binding project decision

Fork Hunter Pro uses **direct integrations with Russian bookmakers only**. Third-party odds aggregators and commercial feed providers are out of scope and must not be proposed, implemented, configured, or used as fallback sources.

Permitted source hierarchy:
1. The bookmaker's own documented API.
2. The bookmaker's own public web application endpoints, where access is permitted.
3. A bookmaker-authorized direct feed or session, if the operator explicitly provides one.

Do not bypass CAPTCHA, QRATOR, Cloudflare, login, geo-restrictions, or other access controls. An HTTP 401/403 or challenge means the parser is blocked from the current environment, not that the bookmaker has no feed.

## Current eight priority operators

| Operator | Existing state | Direct-only next action |
|---|---|---|
| BetBoom | Rust parser exists; current environment reports QRATOR HTTP 401 | Inspect official app/network contract from a permitted session or operator-provided access; no challenge bypass |
| Liga Stavok | Rust parser exists; QRATOR blocks bootstrap | Same: direct official source/session only |
| MelBet | Rust parser exists; regional HTTP 403 | Verify official RU-domain/region endpoint from a permitted environment; no geo-evasion |
| Bet-M | Rust parser exists; regional HTTP 403 | Verify official public line and permitted direct endpoint |
| Tennisi | Parser exists; operator's interactive betting is paused | Keep disabled until operator officially resumes |
| Poolbet | No parser; no verified operating line as of 2026-11-27; announced launch 2026-12-01 | Recheck after launch; confirm official interactive line before implementation |
| Golden Bet | No parser; no verified operating public line | Keep disabled until a live official line is demonstrated |
| ABET / Alfabet | No parser; official information page but no verified public sports line | Keep disabled until a live official line is demonstrated |

## Active direct parsers: next technical priorities

1. Resolve raw market labels and unknown factor_<id> values for Fonbet, PARI and 24bet only from source metadata, official schemas, or observed direct bookmaker responses. Never infer a factor's meaning from its numeric ID alone.
2. Keep unknown factors quarantined from cross-book comparisons until market, selection, period and line semantics are proven.
3. Compare low market-label counts at Leon, Olimp, SportBet and Zenit against the bookmaker's own raw response before treating them as genuine source limitations.
4. Verify each parser independently for live and prematch event coverage, event/odds linkage, duplicate IDs, market depth, line values, and failure handling.
5. Distinguish parser ingestion time from bookmaker quote-update time. Do not mark quote freshness verified unless the bookmaker source supplies a trustworthy update timestamp.
6. Re-run volume gates during active hours; low-activity snapshots are not sufficient evidence of missing markets.

## Production acceptance gate

A bookmaker parser is not complete merely because it is registered or fixture tests pass. Require:
- direct bookmaker source and exact RU operator identity documented;
- real source response captured through permitted access;
- live and prematch parsing verified where those modes exist;
- correct event, sport, league, period, market, selection and line semantics;
- no duplicate event/odd IDs or invalid prices;
- coverage and market depth compared with the raw source;
- source timestamp semantics documented;
- regression fixtures and parser tests passing;
- runtime health and source errors observable.

## Explicit exclusions

- No ODDSCORP, API-SPORT, Odds-API.io, OpticOdds, or any other aggregator.
- No aggregator adapter, provider fallback, or silent source substitution.
- No credentials from public repositories or third parties.
- No CAPTCHA, anti-bot, login, or geographic access-control bypasses.

This document supersedes earlier research notes that listed commercial aggregators as candidate implementation routes. Those notes may remain as historical research only; they are not an approved architecture or work plan.