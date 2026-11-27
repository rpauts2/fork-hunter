# Fork Hunter Pro — Russian bookmaker parser inventory

Audit date: 2026-11-27 (latest direct-feed market-depth sample: 04:23 UTC).
Regulatory scope: all 20 operators in the FNS list linked below. The active runtime target set contains 16 operators; Tennisi, Poolbet, Golden Bet and ABET are tracked separately because their interactive betting status or public line is paused/unverified.

**Integration policy: direct bookmaker sources only.** Functional ingestion may use only an operator-published API/feed, the operator's own public website endpoints where access is permitted, or a feed/session explicitly authorized by that operator. Third-party odds aggregators (including API-SPORT, ODDSCORP, Odds-API.io and OpticOdds) are excluded from the architecture: no adapters, fallbacks, ingestion or production dependencies. No CAPTCHA, QRATOR, Cloudflare, authentication or regional restriction is bypassed.

A parser being registered, compiling or passing fixture tests is not proof of a working live feed. This inventory distinguishes parser implementation, fixture/CI validation, live source reachability, structural data quality, volume and source-side quote freshness.

## Market depth audit — 2026-11-27 04:23 UTC

- Runtime artifact: [runtime_market_depth_summary_2026-11-27.json](runtime_market_depth_summary_2026-11-27.json).
- The synchronized sample contains **38,979 events** (690 live / 38,289 prematch) and **844,206 valid odds** across the 12 reachable direct feeds. Four blocked active feeds returned no events in this environment.
- Raw distinct market-label counts: Baltbet 317, Bettery 253, Marathon 251, Bet24/Fonbet/PARI 126 each, Winline 84, Betcity 47, Leon 6, Olimp 6, SportBet 5, Zenit 4.
- These are raw labels, not canonical market totals. Parser names, market variants and segment/period suffixes differ, so a low count must be compared with the bookmaker's own rendered market catalogue before treating it as missing data.
- Bet24, Fonbet and PARI still expose sizeable clusters of technical `factor_####` labels. Unknown/ambiguous factor IDs must remain quarantined until the direct bookmaker catalogue proves their market, selection, period/team scope and line semantics. They are not safe to compare as arbitrary markets.
- The latest audit's off-peak event-volume gates failed. The runtime sample is useful for market-depth analysis, but it does not establish peak-hour completeness or bookmaker-side quote freshness.

## Feed access research — 2026-11-27

Detailed first-party access findings are recorded in [RUSSIAN_BK_FEED_ACCESS_RESEARCH_2026-11-27.md](RUSSIAN_BK_FEED_ACCESS_RESEARCH_2026-11-27.md). Current status: 12 active bookmaker feeds were reachable in the development environment; BetBoom, Liga Stavok, Melbet and Bet-M were blocked before parsing by QRATOR or regional HTTP 403 responses. Tennisi is paused, Poolbet's announced launch date is 2026-12-01, and Golden Bet / ABET have no verified public interactive line. These environment-specific blocks do not prove global operator unavailability.

## Runtime validation — 2026-11-27 04:06 UTC (off-peak snapshot)

- Snapshot: [runtime_audit_2026-11-27_full_16.json](runtime_audit_2026-11-27_full_16.json).
- **Structural data quality: 12/12 reachable active feeds passed.** The four feeds blocked before parsing are BetBoom (QRATOR HTTP 401), Liga Stavok (QRATOR warm-up challenge), Melbet (regional HTTP 403) and Bet-M (regional HTTP 403).
- Reachable feed output: **38,829 events** (703 live / 38,126 prematch), **843,631 valid odds**, and 38,697 events with at least one usable quote.
- Structural checks: **0 duplicate event IDs, 0 duplicate odds IDs, 0 invalid odds and 0 stale ingestion timestamps**. Overall odds coverage was 98.1–100%; live coverage 84.7–100%; prematch coverage 98.3–100%.
- The nightly KPI exit was **2 / fail** because the 04:06 UTC sample did not reach the configured event-volume thresholds: 0/12 live volume targets and 11/12 prematch targets passed. This is a volume shortfall in a low-activity time window, not a structural parser-quality failure. Recheck during peak hours before changing parser extraction.
- bookmaker_quote_freshness_verified=false remains in force. The sample timestamps are ingestion times and do not prove when the bookmaker last updated a specific price.

## Readiness definitions

- **Full runtime gate passed** — real feed returns both live and prematch events, valid odds linked to the correct events, no duplicate IDs, ingestion timestamps are current, and the current event-volume gates pass. This does not by itself prove bookmaker-side quote freshness.
- **Quality passed / volume gate not met** — real feeds return correctly linked, valid odds and pass structural checks, but the current event-count threshold is not met. Recheck at peak hours before treating the parser as incomplete.
- **Blocked from this environment** — endpoint returns 401/403, QRATOR, challenge, timeout, or other access refusal. This does not prove the bookmaker is globally unavailable; retest from a Russian network and, where required, a user-authorized session.
- **No usable public feed observed** — official site is under maintenance, parked, informational only, or does not expose a public sports-line endpoint during this audit.
- **Not implemented** — no Rust parser is registered yet.

## FNS target list and current state

The full FNS list is not the same as the active betting market. The active runtime target set contains 16 operators, cross-checked against the current 1Cupis partner and rates pages (https://1cupis.ru/info/partners/ and https://1cupis.ru/info/rates/). Tennisi is paused (official 1Cupis notice: https://1cupis.ru/poleznye-materialy/bukmekerskie-kontory-s-cupis/o-bukmekerskoy-kontore-tennisi-bet/registraciya-v-tennisibet/); Golden Bet is listed as a dormant licence in the SRO register (https://bookmakersunion.ru/reestr-chlenov/). Poolbet and ABET currently have no verified public interactive feed.

| # | Operator | Slug | Rust parser | Corrected 2026-11-26 runtime result |
|---:|---|---|---|---|
| 1 | SportBet | sportbet | Yes | Direct `events.table` parser iterates every active market/outcome in the response; no market-family whitelist found. Numeric/string market IDs are preserved (no `market-0` collisions); event timestamps accept numeric strings. Odds conversion accepts decimal-comma strings and rejects non-finite values. Regression fixture covers string IDs/timestamps and non-finite numbers. The 2026-11-27 sample had 5 raw market labels; whether the endpoint is a main-markets-only view still requires source-vs-frontend validation. |
| 2 | Marathon | marathon | Yes | Post-hardening full sweep previously passed. Shared Line51 extraction now accepts numeric or string event/factor IDs, odds and line values (including decimal commas), rejects non-finite numbers, and preserves parent/child market attachment. Added a regression fixture for string IDs and prices. |
| 3 | Baltbet | baltbet | Yes | Direct feed keeps numeric/string IDs and decimal-comma odds parsing; parser-wide numeric conversion now rejects non-finite values. Regression fixture added for decimal-comma prices and NaN/Infinity rejection. Previous full sweep passed after grouping fallback fix: 192 live / 179 with odds, 3,639 prematch / 3,513 with odds, 18,259 valid odds. |
| 4 | BetBoom | `betboom` | Yes | Blocked from this environment. Official host returns HTTP 401 / QRATOR; browser bootstrap returns no usable feed. |
| 5 | Olimp | olimp | Yes | Direct `/api/v4/0/{live,line/top}/sports-with-competitions-with-events` parser consumes all `event.outcomes` with valid decimal probability and non-empty `groupName`; no market-family whitelist found. Numeric text is now parsed after trimming whitespace and replacing decimal commas; group/selection labels are trimmed before IDs are built. Added regression fixture for string odds/line values. The 2026-11-27 sample exposed 6 raw group labels; confirm whether the endpoint includes expanded event markets. |
| 6 | Zenit | zenit | Yes | Direct HTTP parser maps verified source factor codes 1–10 (1X2, double chance, handicap, totals); valid-priced unknown factor/header pairs are counted and logged, never guessed. The sport-line request now uses the configured frontend `client_v` and no longer forces `popular=1`. For matches whose source response has a positive `ross`, `ross_stats_count`, or `ross_count_with_double_chances` counter, the parser now calls the official frontend's default `printMode=1` endpoint `/ajax/line/ross/data/` (live: `/ajax/live/ross/data/{imprinthash}/`) in batches of 200. It preserves the response's per-match `data.order`/`data.data` tables, `filter`, `f_l`, and `other` bets under `Event.extra.zenit_additional_markets`, while logging table, extra-bet, first-line and filter-group counts. The raw payload is deliberately not converted into actionable cross-book odds until its selection/period/line semantics are mapped. Added regression fixtures for counter detection, request paths/parameters and source-event payload attachment; tests have not yet been run in a build environment. |
| 7 | Leon | leon | Yes | Direct `leon.bet/api-2/betline/events/{inplay,prematch}` parser iterates every returned open market and runner; no market-family whitelist found. Market/selection labels are trimmed; market/runner IDs accept numeric/string sources and null IDs fall back to semantic names; numeric price/line values now reject non-finite values. Regression fixtures cover string/null IDs and decimal-comma/non-finite numbers. The 2026-11-27 sample exposed 6 raw market names; confirm whether the response contains the complete expanded market set. |
| 8 | Liga Stavok | `ligastavok` | Yes | Blocked from this environment by QRATOR before session/API bootstrap. No validated authorized bootstrap available. |
| 9 | Tennisi | `tennisi` | Yes | Inactive / paused. Interactive betting suspended from 2026-09-01; current site serves a maintenance page. Not included in the 16 active-feed runtime sweep. |
| 10 | Winline | winline | Yes | Post-hardening full sweep passed. 182 live / 167 with odds, 2,914 prematch / 2,908 with odds, 82,185 valid odds; coverage 99.3% overall, 91.8% live, 99.8% prematch. |
| 11 | Betcity | betcity | Yes | Readiness remains RolloutReady and production enablement off. Nested event IDs/timestamps accept numeric strings; `lv`/`lvt`, `Tot`, `kf`, and generic fallback odds arrays accept decimal-comma numeric strings. Non-finite/invalid values are rejected, and malformed primary lines fall back to valid secondary lines. Regression fixtures added. |
| 12 | Melbet | `melbet` | Yes | Blocked from this environment. Official sportsbook hosts return HTTP 403 / regional challenge; no usable feed obtained. |
| 13 | Poolbet | `poolbet` | No | Inactive / no verified online line. Official FNS domain currently fails or redirects; no operational public line verified. |
| 14 | Bettery | bettery | Yes | Post-hardening full sweep passed after semantic-quote deduplication. 175 live / 164 with odds, 3,268 prematch / 3,268 with odds, 181,614 valid odds; coverage 99.7% overall, 93.7% live, 100.0% prematch. |
| 15 | Fonbet | fonbet | Yes | Direct `line-lb61-w.bk6bba-resources.com` factor catalog is now loaded for scope 1600 and resolved by source sport ID before legacy fallbacks. Known but ambiguous or unsupported catalog IDs are quarantined, not guessed; numeric-string IDs/prices/lines and event timestamps are supported, non-finite factor values rejected. Synthetic catalog and ambiguous-ID fixtures added. |
| 16 | PARI | pari | Yes | Direct `line-lb51-w.pb06e2-resources.ru` factor catalog is loaded for scope 2300 and resolved by source sport ID. Known but ambiguous or unsupported catalog IDs are quarantined; numeric-string IDs/prices/lines and event timestamps are supported, non-finite factor values rejected. Synthetic catalog and ambiguous-ID fixtures added. |
| 17 | Golden Bet | `goldenbet` | No | Inactive / dormant licence. Official domain is `golden-bet.bet`; no operational public line verified. |
| 18 | БЕТ-М | `betm` | Yes | Blocked from this development region. Official host returns HTTP 403 with an explicit region-unavailable page; retest from a permitted Russian network. |
| 19 | 24bet | bet24 | Yes | Direct `line51.tf39be-resources.com` factor catalog is loaded for scope 3000 and resolved by source sport ID. Ambiguous catalog IDs are quarantined; numeric-string IDs/prices/lines and event timestamps are supported. `_24bet` and `bet24` aliases now enumerate one parser instance only, preventing duplicate feed execution. Synthetic catalog and alias fixtures added. |
| 20 | ABET | `abet` | No | No verified online sports line. Official FNS domain currently serves informational content; no public sports-line feed found. |

**Latest runtime evidence (2026-11-26 14:05 UTC):** The full 16-operator post-hardening sweep passed **12/16**. All 12 reachable operators passed the configured volume and structural-quality gates together; the four remaining operators were blocked before odds parsing: BetBoom (QRATOR HTTP 401), Liga Stavok (QRATOR warm-up block), Melbet (HTTP 403 regional restriction) and Bet-M (HTTP 403 region unavailable). Across reachable feeds there were zero duplicate event IDs, zero duplicate odd IDs and zero invalid odds. The full report is [runtime_audit_2026-11-26_full_16_after_factor_mapping_fix.json](runtime_audit_2026-11-26_full_16_after_factor_mapping_fix.json). Sport-scoped factor mapping now rejects ambiguous reused IDs instead of silently assigning a potentially incorrect market/selection; after this change, the five shared line51 feeds passed a targeted 5/5 run and all 12 reachable feeds passed a synchronized 12/12 run. Betcity remains `RolloutReady` with production enablement off. Baltbet's live grouping fallback recovers active prices when inline coefficients are empty; Bettery deduplicates repeated semantic quotes; PARI resolves sports/leagues through the source hierarchy. Source-side quote freshness remains unverified because timestamps are parser-ingestion times, not bookmaker quote-update times. Four FNS-listed operators (Tennisi, Poolbet, Golden Bet and ABET) remain paused/dormant or without a verified public interactive line and are tracked separately. “Blocked from this environment” is not a claim that a site is unavailable to users in Russia.

## Code changes in this phase

1. Rust registry retains all 20 FNS-listed operators. Seventeen have Rust parser instances; Poolbet, Golden Bet and ABET have explicit `not_implemented` registry records. Default runtime gates cover the 16 active operators; the four inactive entries are tracked separately.
2. Tennisi remains in the full 20-operator regulatory inventory. Its parser exists, but the operator has paused interactive betting and the current official site serves a maintenance page; it is not part of the 16 active runtime gates.
3. Leon parser migrated to the current public API (`leon.bet`): fetches dynamic `ctag`, requests `inplay` and `prematch`, uses `homeAway` for participant ordering, maps sport family, preserves market lines, excludes closed/invalid runners and namespaces event IDs by feed mode.
4. Marathon runtime snapshots deduplicate repeated events and odds and discard orphan odds. Regression test covers repeated events and orphan odds.
5. Zenit runtime diagnostics now expose live parsed-event/odds linkage counts; latest probe confirms live coverage recovered to 90.1% with no duplicate IDs.
6. Runtime volume gates are bookmaker-specific for Winline, SportBet, Leon and Olimp. Quality rules (freshness, event ownership, odds validity, duplicate IDs and live/prematch coverage) are unchanged.
7. The browser runtime in the development box was missing Chrome shared libraries. Chromium and its missing libraries were installed locally in the temporary development environment so browser-based probes could run; this does not change repository dependencies.
8. Olimp now uses direct transport only; HTTP 401/403/429 are surfaced without proxy rotation or retries. Readiness metadata records the dated 2026-11-26 11:04 UTC direct-transport sample as historical evidence, not a live availability guarantee.
11. Shared line51 factor parsing now collapses duplicate source factor IDs that resolve to the same bookmaker/event/market/selection/line key, retaining the best available price and logging the number of collapsed source rows. This prevents duplicate odds from leaking downstream.
12. PARI resolves `sportId` through the source `sports` hierarchy when an event points to a tournament segment rather than a root sport ID. Empty event names now retain the source segment as the league; empty factor lists remain unpriced.
13. Baltbet now requests grouping metadata for every live event, not only events with inline active coefficients. For events with an empty inline `c` array, it reads active prices from the official grouping payload; an explicit suspended inline coefficient is never revived from a grouping value.
14. Runtime diagnostics now include bounded samples of duplicate odds IDs and events without linked odds, allowing source-side empty markets to be distinguished from parser omissions.
15. Fonbet, PARI, Bet24, SportBet, Leon, Olimp and Winline map BTTS and/or Even/Odd outcomes to semantic `OddsType` values rather than generic `Custom`. The shared Even/Odd normalizer checks the negative `Нечёт` form before the positive `Чёт` substring.

**Validation after this phase:** `cargo test -p parsers -- --test-threads=1` passed across all targets (parser library: 424 passed, 0 failed, 2 ignored; parser-factory integration: 12 passed; tennis integration: 2 passed). `cargo check -p parsers --all-targets --quiet`, rustfmt checks for all changed Rust files, and `git diff --check` passed. Existing dead-code warnings remain in legacy/blocked parser paths; they do not fail validation.

## Runtime quality rules

1. A parser must return both live and prematch feeds where the operator offers them.
2. Event IDs are stable and namespaced by feed mode if a source reuses IDs.
3. Odds must point to a returned event from the same bookmaker.
4. Odds must be finite and greater than 1.0. `Odd.timestamp` currently measures parser ingestion time (five-minute age limit; 30-second future tolerance), not the bookmaker's own quote update time. Source-side quote freshness remains unverified for the current 16 feeds unless a documented source timestamp is exposed.
5. Suspended/closed outcomes are excluded.
6. Duplicate event IDs fail quality checks; parsers should deduplicate only genuinely repeated source rows and must not merge different matches.
7. Low event volume is reported separately from data-quality failure. Current event-volume thresholds are provisional and must be checked across peak and off-peak hours; do not use the 150-live default as a universal parser correctness test.

## Known remaining blockers

- **BetBoom, Bet-M, Melbet, Liga Stavok:** access/bootstrap blocked from the current development network. Bet-M explicitly reports that the site is unavailable in this region; Melbet asks to disable VPN; BetBoom and Liga Stavok return QRATOR challenge pages. Retest from a permitted Russian network. Liga Stavok specifically requires a validated QRATOR session bootstrap; do not hardcode or invent cookies.
- **Tennisi:** current official site is a maintenance/return-soon page.
- **Poolbet, Golden Bet:** no operational public line verified; Poolbet official domain is failing, and Golden Bet is listed as a dormant licence.
- **ABET:** the official site is informational and does not expose a public sports-line feed.

The four active blocked operators cannot honestly be marked “working” until fresh public data or an authorized session is available. The four inactive FNS entries should be reactivated only after their betting lines resume and a real feed is verified. No placeholder parser is reported as production-ready.

## Additional correction on 2026-11-26

8. Fonbet, PARI, Marathon, Bettery and 24bet now use the source `place` field (`live`, `line`, `notActive`) to split the combined line51 event catalogue. Previous runtime counts around 3,200 Live events were false classifications and are superseded by the corrected snapshot. Direct `listBase` probes on all five official resource hosts also returned combined catalogues: 207–213 `place=live`, 79 `place=notActive`, and about 10,427–10,511 `place=line` events per response.
9. Betcity now parses valid two-way numeric `T1`/`T2` blocks that omit `D`, preserving them as custom outcomes rather than discarding them or misclassifying them as 1X2.
10. Current corrected runtime report: [runtime_audit_2026-11-26_place_filter.json](runtime_audit_2026-11-26_place_filter.json).


## Historical parser worklog and runtime audit — 2026-11-26 06:55 UTC

The historical runtime snapshot in [runtime_audit_2026-11-26_final.json](runtime_audit_2026-11-26_final.json) was taken after the parser corrections at that time. It is superseded by the 09:42 UTC snapshot linked above. Runtime gate result in this historical sample: 1/16 passed (Leon); 12/16 passed structural odds quality; four active operators were blocked by access restrictions. The volume gate is intentionally separate from structural quality.

Additional parser corrections in this work session:

1. **Baltbet:** the live parser now fetches /api/grouping/event/{id} and joins active coefficient IDs to the bookmaker’s source-declared market and outcome metadata. It preserves market names, map/period context and signed handicap/total lines. Final runtime: 78 live / 64 with odds, 3,326 prematch / 3,193 with odds, 9,407 valid odds; structural quality passed. Continue checking unmatched source coefficient IDs before calling live coverage fully complete.
2. **SportBet:** handicap lines are parsed per outcome, preserving the source sign (e.g. side 1 and side 2 may have opposite lines) and using OddsType::Handicap; previously both sides could inherit one unsigned market-level specifier and be labelled Home/Away.
3. **Winline:** live quotes arriving before their event packet are buffered and attached when the event arrives. Event/line removals clear buffered data, and the temporary buffer is capped at 512 event groups.
4. **Zenit:** sport classification now reads the current dictionary mapping; double-chance factors 4/5/6 (1X/12/X2) are parsed with strict header matching.
5. **Olimp:** sport classification follows source names and current fallback IDs. The confirmed all-sports live and line/top endpoints replace a stale 40-ID sweep and invalid /line/line/all probes; runtime acquisition fell from about 76 seconds to roughly 6–9 seconds without material catalogue loss.
6. **Fonbet and PARI:** recognizable source sport labels now take precedence over legacy numeric IDs, with IDs retained only as fallback.
7. **Line51 bookmaker integrations:** authoritative place field partitioning remains in place for Fonbet, PARI, Marathon, Bettery and 24bet.

## Validation after current corrections

- cargo test -p parsers --tests -- --test-threads=1: **408 unit tests passed, 0 failed, 2 ignored; 4 runtime-diagnostics tests passed; 12 parser-factory integration tests passed; 2 tennis-boundary tests passed.**
- cargo check -p parsers: passed.
- Parser-file rustfmt --check: passed.
- git diff --check: passed.

## Still open before parser sign-off

- Four active feeds cannot be runtime-verified from this development environment: BetBoom (QRATOR HTTP 401), Liga Stavok (QRATOR blocks warm-up before API bootstrap), Melbet (regional HTTP 403) and Bet-M (regional HTTP 403). Need permitted network or authorized session/API material; no bypass work was attempted.
- Poolbet, Golden Bet and ABET have no verified operational public interactive sports line; Tennisi is paused. They remain accounted for in the 20-operator registry.
- Eleven reachable feeds are below their current configured volume targets in this sample. Gather peak-hour source-side counts before changing thresholds.
- bookmaker_quote_freshness_verified=false for all 16 runtime records: parser ingestion timestamps are not bookmaker quote-update timestamps.
- Baltbet source grouping did not resolve every active coefficient/outcome ID in the source payload; although structural quality passed in the final snapshot, keep market-level reconciliation under review.

No scanner or arbitrage-engine work has started. Parser sign-off remains blocked by the open items above.

## Latest full runtime sweep — 2026-11-26 09:42 UTC

- Snapshot: [runtime_audit_2026-11-26_0942_all_russian.json](runtime_audit_2026-11-26_0942_all_russian.json).
- Runtime gate: **5/16 passed** (Betcity, Leon, SportBet, Winline, Zenit).
- Structural odds quality: **12/16 passed**; four active operators remain access-blocked from this environment (BetBoom, Liga Stavok, Melbet, Bet-M).
- Seven reachable operators pass structural quality but miss a volume threshold: Baltbet, Bet24, Bettery, Fonbet, Marathon, Olimp, PARI. Baltbet is one priced live event below its current gate; Olimp is 17 prematch events below its current gate.
- All 12 reachable reports have zero duplicate event IDs, zero duplicate odd IDs and zero invalid odds. Parser-ingestion timestamps still do not prove bookmaker-side quote freshness.
- No parser code changed during this diagnostic sweep.

## Official auxiliary routes checked — 2026-11-26

These routes were checked as legal, first-party alternatives to the blocked sportsbook pages. They are **not** counted as odds feeds:

- **BetBoom:** the official documentation route (`/pages/documentation`) and root page returned an access challenge/403 from the independent browser environment as well. No public feed documentation was exposed. Support contact shown by the official challenge page: `support@betboom.ru`.
- **Liga Stavok:** `top.ligastavok.ru` is publicly readable and lists selected weekly events, but its event links lead back to the protected main sportsbook and the page does not expose bookmaker prices. It may be useful as an auxiliary event-discovery source only. Main line remains QRATOR-blocked. Official contact shown on the challenge page: `info@ligastavok.ru`.
- **Melbet:** `livescore.melbet.ru` is publicly readable and exposes live score/event listings, but no bookmaker odds were found there. The official Russian prematch/live/event pages return an explicit regional denial (`Cloudflare ERROR 1000s box`) from the independent browser environment. The livescore host can be considered an auxiliary score/status source, not a replacement odds feed. Official site lists `support@melbet.ru`.

No third-party odds providers were added or evaluated as fallback sources. No CAPTCHA, anti-bot challenge, login, or regional access control was bypassed. Next legal options are operator-confirmed data access or a user-authorized session in an environment where the operator permits access; neither is currently available to this development box.

## Latest full runtime sweep — 2026-11-26 12:50 UTC

- Snapshot: [runtime_audit_2026-11-26_1250_all_russian.json](runtime_audit_2026-11-26_1250_all_russian.json).
- Runtime gate: **5/16 passed** (Betcity, Leon, SportBet, Winline, Zenit).
- Structural odds quality: **12/16 passed**; four active operators remain access-blocked from this environment (BetBoom, Liga Stavok, Melbet, Bet-M).
- Seven reachable operators pass structural quality but miss a volume threshold: Baltbet, Bet24, Bettery, Fonbet, Marathon, Olimp, PARI.
- All 12 reachable reports have zero duplicate event IDs, zero duplicate odd IDs and zero invalid odds. Parser-ingestion timestamps still do not prove bookmaker-side quote freshness.
- The 09:42 snapshot is retained as a historical comparison; no parser code changed between the two runtime sweeps.

## Latest full runtime sweep — 2026-11-26 17:12 UTC

- Snapshot: [runtime_audit_2026-11-26_1712_full_16.json](runtime_audit_2026-11-26_1712_full_16.json).
- Runtime gate: **12/16 passed**. All 12 reachable feeds passed both configured volume gates and structural quality checks.
- Aggregate reachable-feed catalogue: **41,190 events** (2,216 live / 38,974 prematch). The four blocked operators returned no parsed events and are excluded from those aggregate counts.
- Structural checks across all 16 reports: **0 duplicate event IDs, 0 duplicate odds IDs, 0 invalid odds and 0 stale-ingestion flags**.
- The four pre-parse blockers are unchanged: BetBoom (QRATOR HTTP 401), Liga Stavok (QRATOR blocks warm-up before API bootstrap), Melbet (regional HTTP 403) and Bet-M (regional HTTP 403). No challenge, CAPTCHA, authentication or regional restriction was bypassed.
- Nightly strict status remains **fail / exit 2** because those four active feeds are unavailable to this development environment; this does not indicate a parsing regression in the 12 reachable feeds.
- `bookmaker_quote_freshness_verified=false` remains in force: zero stale-ingestion flags do not prove that source-side bookmaker quote timestamps are fresh. Parser ingestion timestamps are not bookmaker quote-update timestamps.

## Baltbet targeted live recheck — 2026-11-26 17:23 UTC

- Snapshot: [runtime_audit_2026-11-26_1723_baltbet_targeted.json](runtime_audit_2026-11-26_1723_baltbet_targeted.json).
- Result: **4,001 events** (208 live / 3,793 prematch), 20,840 valid odds, 96.5% overall odds coverage and 94.2% live odds coverage. Volume and structural-quality gates passed; duplicate event IDs, duplicate odds IDs, invalid odds and stale-ingestion flags were all zero.
- A live-source fixture for an active Baltbet esports event was added at `crates/parsers/tests/fixtures/baltbet_live_grouping_active_event.json`. Its grouping contains 5 markets and 11 priced outcomes; the new regression test confirms all 11 are normalized into valid odds.
- Several other sampled events returned `isActive=false` or empty market arrays when checked directly. A separate 209-event grouping-endpoint probe at concurrency 8 returned HTTP 200 for all 209 requests. The source changes market availability during live play; no reproducible extraction defect or persistent endpoint throttling was established, so the fetch algorithm was not changed speculatively.
