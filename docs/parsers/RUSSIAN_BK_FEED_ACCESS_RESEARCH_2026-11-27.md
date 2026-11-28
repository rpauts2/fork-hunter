# Russian bookmaker feed access research — 2026-11-27

## Integration rule

Fork Hunter connects to Russian bookmakers directly. For every bookmaker, the only acceptable source is:

1. An API/feed published by that bookmaker.
2. The bookmaker's own public website endpoints, where access is permitted.
3. A session or feed explicitly authorized by that bookmaker or supplied by the user for their own account.

**Third-party odds aggregators are excluded from the architecture.** API-SPORT, ODDSCORP, Odds-API.io, OpticOdds and similar services must not be used as live sources, fallback feeds, parser adapters, or cross-book quote providers. Earlier catalogue research is retained below only as an audit trail; it is not an integration proposal. Do not spend engineering time creating aggregator adapters.

No CAPTCHA, QRATOR, Cloudflare, authentication, geo-restriction, or account-control bypasses are part of this plan. Do not reuse credentials from public code or third parties. A 401/403 or anti-bot challenge means the current access path is blocked; it is not permission to evade the control.

## Public-page verification update — 2026-11-28

A separate public-page inspection found evidence that must not be conflated with the runtime API probe:

- **Liga Stavok:** the official homepage exposes a list of event cards with named teams and displayed odds. An official match detail page for CSKA Moscow–Zenit St. Petersburg (event id 23907447) exposes event date/time and quote groups, including 1X2 (3.30 / 3.75 / 2.09 in the inspected snapshot), double chance, draw-no-bet, handicap, total and both-teams-to-score. The page also lists first-half and second-half market groups. This proves that public event/odds data is rendered on the operator's own pages; it does not prove that the current Rust API client can retrieve it from the development runner, where QRATOR still blocks the direct API bootstrap. Next engineering step: compare the official page's server-rendered event/market structure with the parser's expected eventsList model, then validate the same public routes from a permitted runtime/session.
- **Bet-M:** the official public football and line pages expose a JavaScript application shell and advertise prematch/live coverage. The line page also references an iframe on sport.bet-m.ru (RequestHelper, with parent=bet-m.ru). This is a new first-party transport lead, not proof of a feed endpoint or event payload. The Rust parser's live/prematch route probes were updated in commit b4d1f61e3b8a5bb28d12c34db446a53776fcaa1a; current runtime event extraction remains unverified.
- **MelBet:** the official Russian site identifies its operator and publicly describes both prematch and live sports lines. The embedded sportsbook host sport.melbet.ru remains inaccessible from the current inspection environment (HTTP 403); no event/odds payload was extracted.
- **BetBoom:** the official sports site remains inaccessible from this inspection environment (HTTP 401 / QRATOR); no public event/odds payload was extracted.

This update is a public-page observation, not a change to the runtime reachability counts below. Do not mark any of the four blocked feeds as runtime-passed on this evidence alone.
## Direct-bookmaker findings

| Operator | Direct-source finding | Current state / next gate |
|---|---|---|
| Tennisi | Public website relaunched on 2026-11-27 after the 2026-09-01 pause. Industry reports say the redesigned site is accessible and features are still in final testing; personal-account login and the resumption of full betting are not confirmed. The existing parser already contains first-party CGI probes: `!book2_free.LiveBetsLines` and `!rt_home.CategoryInfo`. A normal web inspection of `tennisi.bet/sport/football` received HTTP 403 from this development environment, so no response body or event/odds payload was extracted. | Keep production disabled. Re-test the two existing first-party CGI routes and sport pages from a permitted environment; do not treat site relaunch or marketing banners as proof that the feed is live. |
| Poolbet | Planned opening was announced for 2026-12-01; no operating public odds line was verified by this audit date. | No parser yet. Recheck after the announced launch and verify the official site, operating line, and feed behavior before implementation. |
| Golden Bet | No operating public line or documented direct odds endpoint was verified; the official domain returned HTTP 403 from this environment. | Keep disabled. Do not infer that the bookmaker is globally unavailable from this environment-specific response. |
| ABET / Alfabet | The official abet.ru page identifies ООО «Панорама», the Alfabet brand, sports and licence information, but does not document a public event/odds feed. | Keep disabled until the operator publishes or authorizes a direct feed. |
| BetBoom | The official sports site is protected by QRATOR in this environment (HTTP 401). The Rust parser contains direct-site bootstrap/interception scaffolding, but no live feed was recovered in this audit. | Keep disabled. Continue only through a permitted public route or bookmaker-authorized session/feed. No challenge bypass. |
| Liga Stavok | Direct API routes observed in the parser include the bookmaker-owned `lds-api-sites.ligastavok.ru/rest/events/v8/eventsList`, `/rest/events/v2/filter`, and `/rest/events/v8/tournamentTree`. The current environment is blocked by QRATOR before a stable session/bootstrap can be verified. | Keep disabled. Validate the official browser/session bootstrap only with a permitted public path or user-authorized session. No challenge bypass. |
| MelBet | The official Russian sportsbook is documented as offering prematch and live betting, but direct requests from this environment return a regional HTTP 403. No authorized direct feed was available for runtime verification. | Keep disabled until the official RU endpoint works from a permitted environment or the operator supplies an authorized feed. Do not evade regional restrictions. |
| Bet-M | The official site presents a sports line with prematch and live sections. Direct access from the development environment remains restricted; the parser probes the bookmaker's own routes. | Continue direct-source diagnostics only where access is permitted. Keep production scanning disabled until a real public/authorized feed passes runtime validation. |

## Other direct parser targets

The latest runtime audit distinguishes source availability from parser quality:

- Twelve direct feeds were reachable in the 2026-11-27 sample: Baltbet, Bet24, Betcity, Bettery, Fonbet, Leon, Marathon, Olimp, PARI, SportBet, Winline and Zenit.
- Four active targets could not be validated from this environment: BetBoom (QRATOR HTTP 401), Liga Stavok (QRATOR bootstrap challenge), MelBet (regional HTTP 403) and Bet-M (regional HTTP 403).
- The reachable sample returned 38,829 events and 843,631 valid odds in the 04:06 UTC audit. Structural checks reported no duplicate event IDs, duplicate odds IDs, invalid odds or stale ingestion timestamps.
- The same off-peak run failed event-volume thresholds: no feed met the live-event target and 11/12 met the prematch target. This is a sample-volume result, not evidence that every parser is structurally broken.
- Raw market-label counts are not normalized market counts. Different labels may describe the same market, while period, team and line scope must remain distinct to prevent false arbitrage matches.
- Quote ingestion timestamps do not prove bookmaker-side quote freshness. The scanner must not present them as source price-update timestamps.

See `PARSER_MASTER_INVENTORY.md` and `runtime_market_depth_summary_2026-11-27.json` for the detailed per-operator audit.

## Archived third-party catalogue research — excluded

The earlier research pass found that API-SPORT, ODDSCORP, Odds-API.io and OpticOdds advertise odds for some Russian-facing bookmaker brands. These services are independent third parties, not the bookmakers' own APIs. Their catalogue claims, vendor update-speed claims, prices, trial offers and data rights were not independently verified in this project.

This information is retained only to explain why it was considered during initial research. **It does not change the direct-only rule above:** no trial request, API key, adapter, fallback, feed ingestion, or production dependency should be created for these services.

## Implementation and readiness rules

- Every quote must retain the exact bookmaker slug and direct source provenance.
- Keep the bookmaker's original event, market, factor and selection IDs when supplied.
- Do not infer market semantics from an unknown numeric factor ID. Quarantine unresolved factors until the direct bookmaker catalogue or payload provides unambiguous market, selection and line semantics.
- Do not merge events across books until teams, competition, scheduled start and sport are matched with explicit confidence checks.
- Track source availability, parser health, market coverage, event volume and quote freshness separately.
- A parser being registered, compiling, or passing fixture tests does not prove that a live bookmaker feed is working.
- Keep execution disabled until direct live and prematch feed checks, event/odds linkage, market semantics, freshness and volume gates pass.
- Credentials or session material, if required, must be supplied through the user's own authorized session and stored locally/in the approved secret store. Never commit secrets.

## Remaining direct-source work

1. Improve market/selection normalization using direct bookmaker labels and factor catalogues; unknown markets remain quarantined.
2. Compare raw bookmaker payloads with the website's rendered line for the twelve reachable feeds, especially where runtime market-label counts are unexpectedly low.
3. For BetBoom and Liga Stavok, document the official session/bootstrap contract only when it can be observed through a permitted route or authorized account session.
4. For MelBet and Bet-M, retry the direct official Russian endpoints only from a permitted environment; do not route around regional blocks.
5. Recheck Tennisi only after an official resumption notice, and Poolbet after its announced 2026-12-01 launch.
6. Implement Golden Bet and ABET only after a real official interactive line or authorized direct feed is demonstrated.
