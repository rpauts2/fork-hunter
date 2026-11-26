# Runtime parser quality audit — 2026-11-25

## Scope and limits

This is a one-session runtime probe from the development box, not a multi-day uptime study and not proof that a bookmaker feed is universally available. Geographic restrictions, DNS, temporary provider incidents, and request fingerprints can affect results. No bookmaker accounts were authenticated and no bets were placed.

The probe now records both event volume and odds quality:
- returned events and live/prematch split;
- total and valid odds;
- number and percentage of events with at least one fresh, valid mapped odd;
- stale odds;
- separate quality and volume gates.

An odds item is counted as valid only when its event ID belongs to the returned event set, its bookmaker slug matches after known alias normalization, its decimal odds are finite and greater than 1.0, and its timestamp is within the last five minutes (with 30 seconds of future clock skew).

## Runtime results

| Bookmaker | Events (live / prematch) | Odds | Valid odds | Event coverage | Quality | Volume gate |
|---|---:|---:|---:|---:|---|---|
| Fonbet | 6,534 (3,267 / 3,267) | 169,742 | 169,742 | 100.0% | Pass | Pass |
| Pari | 0 | 0 | 0 | 0% | Fail | Fail |
| Bettery | 0 | 0 | 0 | 0% | Fail | Fail |
| Leon | 0 | 0 | 0 | 0% | Fail | Fail |
| Baltbet | 3,083 (109 / 2,974) | 8,246 | 8,246 | 96.2% | Pass | Fail: 41 live and 26 prematch short |
| Betcity | 3,424 (99 / 3,325) | 26,808 | 26,808 | 99.6% | Pass | Fail: 51 live short |
| Olimp | 1,611 (148 / 1,463) | 29,332 | 29,332 | 89.1% | Pass | Fail: 2 live and 1,537 prematch short |
| Zenit | 3,947 (144 / 3,803) | 20,715 | 20,715 | 96.3% | Pass | Fail: 6 live short |
| Tennisi | 0 | 0 | 0 | 0% | Fail | Fail |
| 24bet | 6,533 (3,267 / 3,266) | 169,058 | 169,058 | 100.0% | Pass | Pass |

### Transport / empty-feed observations

- Pari: both configured resource endpoints failed to connect from this environment.
- Bettery: both configured resource endpoints failed to connect from this environment.
- Leon: both API routes failed while following redirects from this environment.
- Tennisi: the parser completed with zero parsed events/odds in this run; this needs a dedicated endpoint/response-shape investigation before any status promotion.
- These are observations from this box and this run only; do not label them permanently down based on this evidence.

### 24bet slug defect found and fixed

The 24bet parser emits _24bet as its event/odds bookmaker slug, while the factory and runtime diagnostics use bet24. Diagnostics canonicalize _24bet, 24bet, and bet24 to the same key. A second issue then became visible: live and prematch endpoints can reuse the same raw event IDs, so the old parser output collided across the two feeds and falsely assigned live odds to prematch events. Event and odd IDs now include the feed mode (live or pre). The latest runtime run returned 6,533 events and 169,058 fresh, correctly linked odds with 100% live and prematch event coverage and zero duplicate IDs.

### Cross-feed event ID collision found and fixed

The quality gate exposed a real parser defect that aggregate event/odds counts concealed. Fonbet and 24bet could reuse a source event ID in both the live and prematch endpoints. Their event records therefore collided in the scanner's ID-based association, and the diagnostic initially reported only 50% coverage with all odds appearing to belong to prematch events. The parsers now namespace event and odd IDs by feed mode. The diagnostics also fail explicitly when duplicate event IDs remain, rather than silently overwriting one feed's event metadata. After the fix, Fonbet and 24bet both passed a fresh live/prematch quality run with 100% event coverage.

Pari's endpoints were unreachable from this development environment during the latest run; its IDs were also namespaced by feed mode in code, but runtime validation remains blocked by transport.

## Code changes made in this phase

1. Parser health checks now require events and usable odds, verify odds-to-event linkage, bookmaker ownership, finite decimal prices, and freshness. Events without odds no longer make a parser Healthy.
2. Health checks run with bounded concurrency (3) rather than serially; results are sorted for deterministic output.
3. Runtime diagnostics now report odds count, valid odds, event coverage, stale odds, and a separate quality gate. A volume pass alone is no longer enough.
4. Runtime diagnostics run with bounded concurrency (3) rather than sequentially.
5. Pari, Fonbet, Leon, and Bettery no longer turn a fully failed fetch into a successful empty result. Their shared HTTP client is reused; Bettery now fetches each feed once rather than fetching events and odds in two duplicate passes.
6. Tennisi's prematch probe concurrency was reduced from 32 to 4, and the parser now returns an error if every feed request fails and no data is collected.
7. 24bet legacy slug normalization is applied in runtime quality checks and both factory slugs resolve to the same parser instance.
8. Melbet is now visible in ParserFactory coverage as diagnostics-only and disabled for scanning, matching its explicit DiagnosticOnly readiness state.
9. Static parser health snapshots are always Degraded until an actual runtime probe runs; an unprobed parser is no longer mislabeled Unhealthy.

## Promotion decision

- Fonbet and 24bet passed the latest single-run volume and quality gate with 100% live and prematch event coverage and zero duplicate event IDs. They still need repeated scheduled runs and cross-checks against source pages before production promotion.
- Baltbet, Betcity, Olimp, and Zenit returned valid, well-linked odds in this run but did not meet both current volume thresholds. Keep their existing readiness gates; do not promote them from this single snapshot.
- Pari, Bettery, Leon, and Tennisi remain unverified in this environment. Marathon's earlier run is historical evidence only; it was not included in the latest two-bookmaker recheck. Diagnose transport and response contracts separately.
- Do not enable real-money execution based on parser health. Parser availability is only one prerequisite in the end-to-end chain.

## Follow-up runtime check — 2026-11-26 03:53 UTC

A fresh isolated run of `runtime_parser_diagnostics --json-stdout olimp zenit` was executed from the current development box. Full machine-readable output is archived in `runtime_audit_2026-11-26_olimp_zenit.json`.

| Bookmaker | Events (live / prematch) | Valid odds | Events with odds | Odds coverage | Quality | Volume gate |
|---|---:|---:|---:|---:|---|---|
| Olimp | 1,431 (77 / 1,354) | 28,217 | 1,431 | 100.0% | Pass | Fail: live 77/100, prematch 1,354/1,450 |
| Zenit | 4,017 (83 / 3,934) | 22,219 | 4,005 | 99.7% | Pass | Fail: live 72/125; prematch 3,933/3,000 passes |

Both feeds returned valid, fresh, correctly linked prices with no duplicate event IDs and no stale odds. The current run failed only volume thresholds: Olimp missed both of its provider-specific gates; Zenit missed its live gate while prematch passed. This differs from the previous 2026-11-25 snapshot and demonstrates why a single successful run must not be treated as a permanent production guarantee. The lower current live volume may reflect the current event schedule or source response; no quality gate was relaxed.

The 2026-11-26 parser hardening also added regression coverage for Olimp's numeric sport IDs, malformed event identities, whitespace-only participant names, and case/whitespace-insensitive selection mapping. These fixture tests do not substitute for the live runtime gates.

## Full Russian operator runtime pass — 2026-11-26 04:11 UTC

Executed the default 16-operator runtime diagnostic sweep. Raw machine-readable output is archived in `runtime_audit_2026-11-26_all_russian.json`.

| Operator | Live events / with odds | Prematch events / with odds | Valid odds | Result |
|---|---:|---:|---:|---|
| Baltbet | 96 / 82 | 3,167 / 3,046 | 8,844 | Quality pass; volume KPI fail |
| Bet24 | 3,233 / 3,233 | 3,231 / 3,231 | 168,984 | Pass |
| BetBoom | 0 / 0 | 0 / 0 | 0 | Blocked: QRATOR HTTP 401 |
| Betcity | 92 / 86 | 3,529 / 3,529 | 28,656 | Quality pass; volume KPI fail |
| Bet-M | 0 / 0 | 0 / 0 | 0 | Blocked: regional HTTP 403 |
| Bettery | 3,242 / 3,234 | 3,242 / 3,233 | 167,572 | Pass |
| Fonbet | 3,234 / 3,233 | 3,232 / 3,232 | 169,650 | Pass |
| Leon | 101 / 101 | 2,916 / 2,916 | 7,969 | Pass under operator-specific KPI |
| Liga Stavok | 0 / 0 | 0 / 0 | 0 | Blocked: QRATOR warm-up navigation |
| Marathon | 3,236 / 3,228 | 3,237 / 3,229 | 167,394 | Pass |
| Melbet | 0 / 0 | 0 / 0 | 0 | Blocked: regional HTTP 403 |
| Olimp | 76 / 76 | 1,399 / 1,399 | 29,129 | Quality pass; volume KPI fail |
| PARI | 3,237 / 3,236 | 3,236 / 3,236 | 169,488 | Pass |
| Sportbet | 28 / 28 | 1,523 / 1,523 | 25,918 | Pass under operator-specific KPI |
| Winline | 79 / 74 | 2,810 / 2,794 | 69,406 | Quality pass; volume KPI fail |
| Zenit | 97 / 87 | 3,945 / 3,944 | 22,413 | Quality pass; volume KPI fail |

Seven of 16 operators passed the current KPI thresholds. Nine failed: five because of live/prematch volume thresholds, and four because their official sites could not be reached through this environment. No duplicate event/odd IDs, invalid odds, or stale ingestion timestamps were reported for the five volume-limited parsers. The feed data-quality flag is not equivalent to bookmaker quote freshness: all 16 reports explicitly state `quote_timestamp_basis=parser_ingestion_timestamp_only` and `bookmaker_quote_freshness_verified=false`.

The runtime sweep was performed at 04:11 UTC (07:11 Moscow time), when many sports schedules naturally have few live events. The default live target of 150 is not a universal proof of parser failure; it must be calibrated using repeated time-of-day observations and source-specific catalogue expectations. Do not lower thresholds merely to turn red results green. Betcity also emitted six live events without prices (86/92 live events had odds); these are retained as events but excluded from the events-with-odds KPI.

## Parser crate hardening and test gate — 2026-11-26

The active parser crate test gate now completes deterministically:

- Library tests: **391 passed, 0 failed, 2 ignored** (the two ignored tests are explicitly runtime/network-only Zenit probes).
- Runtime diagnostics binary tests: **4 passed**.
- Parser factory integration tests: **12 passed**.
- Tennis-feed boundary tests: **2 passed**.
- `cargo check -p parsers`: passed with **0 compiler warnings**.
- `rustfmt --check` over changed Rust files under `crates/parsers`: passed.
- `git diff --check`: passed.

The previous clean compile had 74 warnings. They were removed by deleting unused imports, marking test-only helpers/constants as test-only, and isolating Winline's retired headless extraction implementation behind `cfg(test)` while preserving the active production WebSocket adapter and all its regression tests. No blanket `allow(dead_code)` was added. The full workspace `cargo fmt --all -- --check` still reports formatting differences in unrelated non-parser crates; those files were not reformatted or changed as part of this parser sprint.

This is a code/test quality milestone, not a claim that all 16 runtime feeds are production-ready. The four region/protection-blocked operators and five volume-limited runtime results remain open; the ingestion timestamp still does not verify the bookmaker's own quote update time.

### Repeat volume/coverage sample — 2026-11-26 04:21 UTC

A second live run of the five volume-limited parsers was completed 10 minutes after the full sweep. Raw results are archived in `runtime_audit_2026-11-26_volume_repeat.json`.

| Operator | Live events / with odds | Prematch events / with odds | Valid odds | Quality |
|---|---:|---:|---:|---|
| Baltbet | 88 / 71 | 3,197 / 3,074 | 8,876 | Pass |
| Betcity | 91 / 76 | 3,525 / 3,525 | 28,225 | Pass |
| Olimp | 36 / 36 | 1,400 / 1,400 | 28,518 | Pass |
| Winline | 77 / 68 | 2,816 / 2,805 | 69,720 | Pass |
| Zenit | 88 / 74 | 3,962 / 3,960 | 22,457 | Pass |

All five feeds again passed the structural/odds quality checks but failed the current 150-live-event threshold. Live events-with-odds coverage was 80.7% Baltbet, 83.5% Betcity, 100% Olimp, 88.3% Winline, and 84.1% Zenit. The repeated low live catalogue counts at 07:11–07:21 Moscow time indicate the generic live KPI is unsuitable as a single time-independent gate; the next check must compare peak sports hours and expected event catalogue totals. This does not justify relaxing the threshold without evidence.


### Betcity two-way numeric market recovery — 2026-11-26T04:24:00.270543604Z

Runtime payload inspection exposed a real parser gap: some live `TMSNum` blocks contain `T1` and `T2` prices but omit `D`. The previous parser only recognized this family when all three keys were present, so it discarded the valid two-outcome market. The parser now preserves `T1`/`T2` as custom two-way numeric outcomes (not incorrectly classifying them as 1X2), carrying the source `Num` value as the line. A regression test covers this shape.

After rebuilding the runtime diagnostics binary and re-fetching Betcity, the live snapshot contained 87 events, 76 with odds, and 29188 valid odds overall; the previously observed tennis event `betcity-25122494` was no longer reported as having no live odds. This confirms the recovery path. The full runtime still does not meet the generic live-volume threshold. Raw output is in `runtime_audit_2026-11-26_betcity_two_way_fix.json`.


## Critical correction: authoritative live/prematch partition on the shared line51 API — 2026-11-26T04:32:22.045513902Z

The full runtime sweep exposed and corrected a systemic classification defect affecting Fonbet, PARI, Marathon, Bettery and 24bet. Their `events/list` and `events/listBase` endpoints return a combined catalogue containing `place=live`, `place=line` and `place=notActive`. The previous adapters trusted which endpoint they had called and assigned that boolean to every parsed event. That duplicated prematch events into the Live partition and produced implausible counts around 3,200 Live events.

The parser now uses the source event's `place` field as the authoritative partition marker and fails closed for missing/unknown states. This is shared by the five adapters and covered by mixed-catalogue regression tests. Direct `events/listBase` probes on all five hosts also returned the combined catalogue (about 10.7k events, 207–213 marked `live`, 79 `notActive`, and the remainder `line`), confirming that the endpoint name itself cannot be trusted as the feed-mode marker. Raw endpoint inspection across all five hosts confirmed the same `place` vocabulary.

Corrected 16-operator runtime snapshot:

| Operator | Live events / with odds | Prematch events / with odds | Valid odds | Feed quality | Runtime gate |
|---|---:|---:|---:|---|---|
| baltbet | 94 / 82 | 3196 / 3073 | 8907 | Pass | Volume KPI fail |
| bet24 | 67 / 66 | 3153 / 3153 | 84479 | Pass | Volume KPI fail |
| betboom | 0 / 0 | 0 / 0 | 0 | Blocked | Blocked |
| betcity | 95 / 82 | 3530 / 3530 | 29489 | Pass | Volume KPI fail |
| betm | 0 / 0 | 0 / 0 | 0 | Blocked | Blocked |
| bettery | 71 / 67 | 3157 / 3157 | 83808 | Pass | Volume KPI fail |
| fonbet | 66 / 65 | 3153 / 3153 | 84767 | Pass | Volume KPI fail |
| leon | 106 / 106 | 2914 / 2914 | 8006 | Pass | Pass |
| ligastavok | 0 / 0 | 0 / 0 | 0 | Blocked | Blocked |
| marathon | 71 / 67 | 3157 / 3157 | 83834 | Pass | Volume KPI fail |
| melbet | 0 / 0 | 0 / 0 | 0 | Blocked | Blocked |
| olimp | 84 / 84 | 1399 / 1399 | 29100 | Pass | Volume KPI fail |
| pari | 65 / 64 | 3158 / 3158 | 84697 | Pass | Volume KPI fail |
| sportbet | 19 / 19 | 1510 / 1510 | 25671 | Pass | Volume KPI fail |
| winline | 75 / 71 | 2804 / 2793 | 69713 | Pass | Volume KPI fail |
| zenit | 91 / 78 | 3944 / 3942 | 22412 | Pass | Volume KPI fail |

The corrected sweep reports **1/16 fully passed** the existing volume gate. Twelve reachable feeds passed structural/odds quality; four remain blocked by external access (BetBoom, Liga Stavok, Melbet, Bet-M). Eleven reachable feeds are below the current volume thresholds at 07:32 Moscow time. Their corrected live counts are roughly 19–106, not 3,000+. This is a material correction: earlier 04:11/04:21 snapshots for the five shared-platform brands used the old classification and must not be used as evidence of real live volume. The 150-live-event default is now explicitly under review; it must be calibrated against peak-hour catalogues and actual source-side `place=live` events, not relaxed just to make the gate pass.


## Final parser verification after line51 and Betcity corrections — 2026-11-26

- `cargo test -p parsers --tests -- --test-threads=1`: **391 unit tests passed, 0 failed, 2 ignored**, 4 runtime-diagnostics binary tests passed, 12 parser-factory integration tests passed, and 2 tennis-boundary integration tests passed.
- `cargo check -p parsers`: passed with 0 library compiler warnings.
- `git diff --check`: passed.
- Parser-only `rustfmt --check`: passed.

The full runtime sweep is archived in `runtime_audit_2026-11-26_place_filter.json`; it supersedes earlier 2026-11-26 live counts for the shared line51 operators.


## Latest complete runtime sweep after parser corrections — 2026-11-26 06:55 UTC

Raw report: [runtime_audit_2026-11-26_final.json](runtime_audit_2026-11-26_final.json). The report is authoritative for this snapshot and supersedes earlier same-day counts.

| Operator | Live / with odds | Prematch / with odds | Valid odds | Structural quality | Volume gate |
|---|---:|---:|---:|---|---|
| Baltbet | 78 / 64 | 3,326 / 3,193 | 9,407 | Pass | Fail: live volume |
| 24bet | 75 / 75 | 3,235 / 3,235 | 86,897 | Pass | Fail: live volume |
| BetBoom | 0 / 0 | 0 / 0 | 0 | Blocked: QRATOR 401 | Blocked |
| Betcity | 85 / 71 | 3,546 / 3,546 | 28,854 | Pass | Fail: live volume |
| Bet-M | 0 / 0 | 0 / 0 | 0 | Blocked: regional 403 | Blocked |
| Bettery | 88 / 77 | 3,234 / 3,234 | 86,217 | Pass | Fail: live volume |
| Fonbet | 76 / 76 | 3,235 / 3,235 | 87,249 | Pass | Fail: live volume |
| Leon | 122 / 122 | 2,920 / 2,920 | 8,119 | Pass | Pass |
| Liga Stavok | 0 / 0 | 0 / 0 | 0 | Blocked: QRATOR bootstrap | Blocked |
| Marathon | 86 / 75 | 3,233 / 3,233 | 86,211 | Pass | Fail: live volume |
| Melbet | 0 / 0 | 0 / 0 | 0 | Blocked: regional 403 | Blocked |
| Olimp | 78 / 78 | 1,434 / 1,434 | 30,393 | Pass | Fail: live and prematch volume |
| PARI | 75 / 74 | 3,238 / 3,238 | 87,155 | Pass | Fail: live volume |
| SportBet | 15 / 15 | 1,557 / 1,557 | 27,011 | Pass | Fail: live volume |
| Winline | 97 / 86 | 2,795 / 2,787 | 70,620 | Pass | Fail: live volume |
| Zenit | 106 / 95 | 3,947 / 3,947 | 28,361 | Pass | Fail: live volume |

**Result:** 12/16 active feeds were reachable and all 12 passed structural/odds quality; four remained blocked by access restrictions. Only Leon passed the current full event-volume gate. There were no duplicate event IDs, duplicate odd IDs or invalid odds in the 12 reachable reports. Prematch odds coverage ranged from 96.0% to 100%; live coverage ranged from 82.1% to 100%. These counts are time-specific and should be compared with bookmaker-side source catalogues and repeated at peak hours.

All 16 reports use quote_timestamp_basis=parser_ingestion_timestamp_only and bookmaker_quote_freshness_verified=false. Therefore, valid numeric prices and current parser-ingestion timestamps do not prove that the bookmaker quote itself has not changed or gone stale.

The runtime gate failed for 15/16, but that count includes four access-blocked operators and 11 reachable providers below provisional event-volume thresholds; it does not mean 15 parsers failed schema validation.


### Child-market aggregation correction — 2026-11-26 10:16 UTC

A live-feed inspection found that the shared `*-resources` catalogue returns child rows for markets such as corners, cards and periods. These rows carry `parentId` and odds but intentionally omit participant names; the previous parser dropped them. The shared-platform parser now maps child rows to a validated parent match and namespaces the child market in both the market label and stable odd ID, preventing collisions with the parent match's 1X2/totals. A regression fixture verifies that a child-market price is attached to the parent event and does not create a synthetic event.

The seven reachable feeds were rechecked after this change: 6/7 passed the runtime volume gate; the full 16-operator sweep now passes 11/16. All 12 reachable operators passed structural quality with zero duplicate event IDs, zero duplicate odd IDs and zero invalid odds. The full sweep snapshot left Olimp 3 short of its 1,450 prematch threshold (1,447). A targeted repeat at 10:19 UTC returned 1,467 prematch events with odds and passed the provider-specific gate. Therefore all 12 reachable operators have recorded at least one passing volume sample across same-day runs, although not all passed in one simultaneous sweep. Four operators remain blocked by access restrictions. Full raw report: `runtime_audit_2026-11-26_child_market_fix_full.json`; Olimp repeat: `runtime_audit_2026-11-26_olimp_repeat.json`.

This is a real parser coverage correction, not a threshold relaxation. Repeated peak/off-peak samples are still required, and source-side quote freshness remains unverified.


### Direct-transport policy and synchronized reachable-feed verification — 2026-11-26 11:04 UTC

The Olimp parser previously attempted proxy rotation after HTTP 403, and its readiness metadata incorrectly described this as a supported feature. This was removed. Olimp now uses only the configured HTTP client; 401, 403 and 429 are treated as access/rate restrictions and stop retries. Transient transport/server failures retain bounded retries and the circuit breaker. Regression coverage verifies that 401/403/429 are not retryable while transient status codes remain retryable. The historical `OLIMP_STATUS.sh` was rewritten to remove obsolete proxy-bypass claims.

After the policy change, Olimp passed a direct-transport runtime check: 164 live and 1,509 prematch events with odds, 32,881 valid odds, zero duplicates and zero invalid odds. A synchronized reachable-only sweep then ran all 12 accessible active operators in one invocation: **12/12 passed** configured volume and structural-quality gates, all with zero duplicate event IDs, zero duplicate odd IDs and zero invalid odds. The four blocked active operators remain outside that denominator. The preceding full 16-operator sweep had 11/16 because Betcity transiently returned no usable feed; a targeted retry and the synchronized reachable-only sweep both passed Betcity.

The report still cannot establish bookmaker-side quote freshness: `Odd.timestamp` remains parser-ingestion time where source quote timestamps are absent. See `SOURCE_QUOTE_TIMESTAMP_AUDIT_2026-11-26.md`. Raw reports: `runtime_audit_2026-11-26_all_reachable_after_policy.json`, `runtime_audit_2026-11-26_after_olimp_access_policy.json`, and `runtime_audit_2026-11-26_olimp_direct_transport.json`.

Validation after the direct-transport change: `cargo test -p parsers --lib -- --test-threads=1` — 411 passed, 0 failed, 2 ignored; `cargo check -p parsers --lib --quiet`, `rustfmt --edition 2021 --check crates/parsers/src/olimp.rs`, and `git diff --check` all passed.

## Sport-scoped factor resolution hardening — 2026-11-26 14:01 UTC

A deeper market-mapping audit found a correctness risk in the shared line51 factor catalogue: when a factor ID had multiple sport-specific definitions, an event whose `sportId` did not match any catalogue entry could silently inherit the first global definition. This could assign a quote to the wrong market or selection. Resolution now follows a fail-closed rule: use an exact source-sport match; if there is no exact match, allow fallback only when all sport-scoped definitions agree on both market type and selection; otherwise omit the ambiguous quote rather than misclassify it. When the event has no sport ID, the same semantic-unambiguity rule applies.

Regression coverage now verifies exact sport matches, rejection of ambiguous reused IDs for unknown/missing sports, and safe resolution where only one definition exists.

Post-change runtime verification:
- Shared line51 group (Marathon, Bettery, Fonbet, PARI, 24bet): **5/5 passed** in the targeted run; 0 duplicate event IDs, 0 duplicate odd IDs, 0 invalid odds.
- All reachable active Russian feeds: **12/12 passed together** in the synchronized run archived as `runtime_audit_2026-11-26_reachable_after_factor_mapping_fix.json` (2026-11-26 14:01 UTC). The run returned 39,619 events (2,051 live / 37,568 prematch), with 0 duplicate event IDs, 0 duplicate odd IDs and 0 invalid odds across all 12 providers.
- Per-feed event-with-odds coverage ranged from 95.9% to 100.0% overall; live coverage ranged from 84.2% to 100.0%; prematch coverage ranged from 96.5% to 100.0%.

The 12/12 result is a single synchronized snapshot, not proof of continuous availability or bookmaker-side quote freshness. The latter remains unverified because the inspected public feeds do not expose a quote-level update timestamp.

## Full active-set verification after sport-scoped factor hardening — 2026-11-26 14:05 UTC

A fresh full 16-operator sweep after the factor-resolution change passed **12/16**. All 12 reachable operators passed their configured live/prematch volume and structural-quality gates in the same run. The four failures remained access/bootstrap blockers before odds parsing: BetBoom (QRATOR HTTP 401), Liga Stavok (QRATOR warm-up block), Melbet (HTTP 403 regional restriction) and Bet-M (HTTP 403 region unavailable).

Across the 12 reachable reports: 39,618 events total (2,022 live / 37,596 prematch), zero duplicate event IDs, zero duplicate odd IDs, zero invalid odds. Overall event-with-odds coverage ranged from 96.4% to 100.0%; live coverage ranged from 85.8% to 100.0%; prematch coverage ranged from 96.5% to 100.0%. Raw report: `runtime_audit_2026-11-26_full_16_after_factor_mapping_fix.json`.

This is a single full-set snapshot. The previous synchronized 12/12 reachable-only run at 14:01 UTC independently confirmed the post-change shared factor mapping. Neither run establishes continuous availability or bookmaker-side quote freshness.
