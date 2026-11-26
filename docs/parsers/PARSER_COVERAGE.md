# Parser coverage — current scope

The authoritative operator list and per-bookmaker runtime evidence live in [`PARSER_MASTER_INVENTORY.md`](PARSER_MASTER_INVENTORY.md).

## Current scope

- Full regulatory inventory: all 20 Russian bookmaker operators in the latest FNS official-site list available during the audit.
- Active runtime target set: 16 operators listed in current 1Cupis partner/rates pages; four FNS entries are paused/dormant or have no verified online betting line.
- Rust parser instances: 17.
- Explicitly listed but not yet implemented: Poolbet, Golden Bet and ABET.
- Default runtime diagnostics: all 16 active operators. Inactive/dormant FNS entries remain in the full inventory and are excluded from active-feed failure counts.
- Latest full active-set runtime result (2026-11-26 13:20 UTC): **12/16 passed**. All 12 reachable feeds passed; the remaining four active operators were blocked before feed parsing (BetBoom, Liga Stavok, Melbet, Bet-M). The four failures are access restrictions, not failed odds-quality checks. Report: `runtime_audit_2026-11-26_full_16_active_final.json`.
- Latest synchronized reachable-only run (2026-11-26 13:09 UTC): **12/12 passed together**, with zero duplicate event IDs, zero duplicate odd IDs and zero invalid odds. Report: `runtime_audit_2026-11-26_all_reachable_after_baltbet_grouping_fix.json`.

## Parser acquisition principles

- Prefer direct HTTP/JSON or WebSocket feeds when the provider exposes a stable public contract.
- Where a browser is necessary, use it for authorized page bootstrap and response interception; do not treat fragile DOM scraping as the preferred long-term path.
- Do not bypass anti-bot protections or hardcode session cookies. QRATOR/other session gates must be reported as blocked until a valid, authorized bootstrap exists.
- Keep source event IDs, market identifiers, outcome labels, handicaps/total lines and live/prematch state intact during normalization.
- Runtime readiness requires odds linked to events from the same bookmaker, correct source-declared feed-mode identity, no duplicate event IDs and meaningful coverage in both live and prematch. The current `Odd.timestamp` records ingestion time; bookmaker-side quote freshness is not verified unless a source update timestamp is available.
- Volume thresholds are provisional and must be calibrated against time-of-day and the bookmaker's own source catalogue. A fixed 150-live-event threshold is not a valid universal gate for early-morning snapshots. Do not lower it solely to make reports green.

## Out-of-scope legacy integrations

Pinnacle, Pin-Up, 1xStavka, and the experimental external tennis-results feed are not part of the current Russian FNS bookmaker target. Legacy source files may remain for historical reference, but they must not be counted as active Russian bookmaker parsers or included in the production bookmaker scan.
