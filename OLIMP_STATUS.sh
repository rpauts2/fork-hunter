#!/usr/bin/env bash
# Current Olimp parser status (historical proxy-rotation implementation retired).
set -euo pipefail

cat <<'STATUS'
Olimp parser
============

Current transport policy:
- Uses the configured HTTP client and official public API routes only.
- Does not rotate proxies or change network identity after HTTP 401/403.
- Does not retry HTTP 401/403/429; these are reported as access/rate restrictions.
- Retries only transient transport/server failures with bounded backoff.
- Uses a circuit breaker to limit repeated failures.

Current verification:
- Parser unit tests: run `cargo test -p parsers --lib olimp::tests`.
- Runtime volume and source quality: run `cargo run -p parsers --bin runtime_parser_diagnostics -- olimp`.
- Readiness remains non-production until repeated live and prematch snapshots confirm coverage.

The previous proxy-rotation implementation and its claims are obsolete and must not be used.
STATUS
