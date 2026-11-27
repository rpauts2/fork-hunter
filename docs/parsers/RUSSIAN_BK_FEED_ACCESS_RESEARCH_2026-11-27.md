# Russian bookmaker feed access research — 2026-11-27

## Objective and safety boundary

Identify a working, supportable source for all 20 FNS-listed Russian bookmaker targets. Preferred order:
1. Operator-published API or written authorized feed.
2. Licensed commercial odds-data provider with explicit bookmaker-level coverage and rights to redistribute/use the data.
3. Public operator feed only where its use is permitted and does not require bypassing access controls.

No CAPTCHA, QRATOR, Cloudflare, authentication, geo-restriction, or account-control bypasses are part of this plan. Do not reuse credentials from public code or third parties.

## Findings

| Operator | Current finding | Candidate route | Status / next gate |
|---|---|---|---|
| Tennisi | Betting suspended from 2026-09-01; withdrawals reportedly remained available through 2026-09-30. | ODDSCORP still lists Tennisi, but the catalogue appears older than the suspension and is not proof of live quotes. | Keep disabled. Recheck after operator announces resumption; require live sample before reactivation. |
| Poolbet | Planned opening moved to 2026-12-01; not operating as of this research date. | No verified odds provider or current line. | Keep disabled. Recheck after 2026-12-01; confirm the operator/domain and legality before integration. |
| Golden Bet | No operational public line or documented feed found; official domain returned HTTP 403 from this environment. | No confirmed provider listing. | Keep disabled; request operator confirmation only if an active product is demonstrated. |
| ABET / Alfabet | Official abet.ru page identifies ООО «Панорама» and the Alfabet brand, lists sports and licence information, but does not document a public event/odds feed. | No confirmed provider listing. | Keep disabled until a live line or authorized feed is verified. |
| BetBoom | Direct environment access blocked by QRATOR. | API-SPORT documents BetBoom odds; ODDSCORP lists BetBoom. | Strongest provider candidates. Need trial/API key, actual RU coverage, update cadence and redistribution terms. |
| Liga Stavok | Direct environment access blocked by QRATOR. | ODDSCORP lists Liga Stavok in its commercial odds feed catalogue. | Candidate. Need bookmaker-specific live/prematch sample, freshness and terms. |
| MelBet | Direct environment access returns regional HTTP 403. | API-SPORT documents MelBet odds; ODDSCORP and Odds-API.io list MelBet. | Strongest provider candidates. Need region-specific coverage, actual source timestamps and terms. |
| Bet-M | Direct site is an active JavaScript sports line with prematch and live sections; direct access from the development environment remains restricted. | No confirmed bookmaker-specific feed in the provider catalogues reviewed. | Continue direct public-feed diagnostics only where access is permitted; otherwise request a licensed feed from provider/operator. |

## Provider evaluation

### API-SPORT.ru
- Public developer documentation exposes REST + WebSocket for seven sports.
- Documented bookmaker keys include betboom, melbet, pari, marathon, and pinnacle.
- Odds require the with_bk_odds option; the documented model has updatedAt / updatedAtByStage, market state, stage, and optional per-price dateUpdateMs.
- updatedAt means last board retrieval from the source, not necessarily last price change. For BetBoom, the docs say per-outcome update timestamps are not provided and the board timestamp is used; MelBet may provide dateUpdateMs, with nulls on some prematch prices.
- Authentication requires an API key in the Authorization header. /v2/account/usage exposes plan and remaining daily quota without consuming quota.
- This is a documented commercial/third-party data source, not an official BetBoom or MelBet API. No API key or pricing/redistribution terms were available in the project environment, so no live feed can yet be asserted.

### ODDSCORP
- The current ODDSCORP API page states 43 connected bookmakers and explicitly lists BetBoom, Liga Stavok, MelBet and Tennisi among available operators.
- It advertises HTTP API and WebSocket odds updates, a public live demo, and a two-week trial on request via Telegram @oddscorp; the page says a manager issues trial access within 24 hours.
- The pricing page separates live/prematch and HTTP/WebSocket by bookmaker. Visible price figures include 450 for live HTTP, 300 for prematch HTTP, and 675 for a combined HTTP option per bookmaker, but the page does not make the currency/unit explicit in the readable content. Confirm the payable amount in writing.
- The provider advertises approximately 0.4-second average updates and claims 99% of data is under one second. These are vendor claims, not independently verified measurements.
- No trial key, payload, or written usage rights are available in the repository. This is now the clearest trial path for BetBoom and Liga Stavok, and a second candidate for MelBet. Do not enable production until the trial proves the Russian bookmaker variant, event/market depth, source timestamps, and permitted use.

### Odds-API.io
- Its current catalogue lists MelBet and provides live/prematch odds via REST/WebSocket.
- Its public bookmaker list did not show BetBoom, Liga Stavok, Tennisi, Poolbet, Golden Bet, ABET or the Russian Bet-M operator in the inspected results.
- OpticOdds also exposes a separate sportsbook identifier named betM, but the public coverage material describes a multi-region international catalogue and does not establish that this is the Russian ООО «БЕТ-М». Do not treat it as a confirmed Bet-M feed without written confirmation.
- It is an independent commercial aggregator, not an official operator API. Treat MelBet as a potential fallback only after a trial verifies the Russian market variant, market depth, quote timestamps and redistribution rights.

## Implementation decision

Do not silently replace an operator parser with a generic aggregator feed. Implement provider adapters as separately identified sources with:
- source_provider and source_bookmaker provenance;
- original bookmaker event/market IDs retained where supplied;
- strict source timestamp and board freshness handling;
- no merging of provider events until team/competition/time matching is proven;
- provider health and per-bookmaker availability exposed independently;
- feature flag and API key from environment/secret store only;
- a provider contract test fixture and live smoke test before production enablement.

## Remaining work

1. Request the free two-week ODDSCORP trial for BetBoom, Liga Stavok and MelBet; in parallel request an API-SPORT trial for BetBoom and MelBet. Compare the same sample events.
2. Confirm commercial use and redistribution rights, region variant, markets, rate limits, SLA, latency and cancellation terms.
3. Evaluate Bet-M against licensed providers and permitted public feed behavior; do not evade regional restrictions.
4. Recheck Poolbet after 2026-12-01 and Tennisi only after official resumption.
5. Continue market-depth audit for the 12 currently reachable direct parsers.
