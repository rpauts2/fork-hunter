# Direct bookmaker factor-catalog audit — 2026-11-27

## Findings

Fonbet, PARI and 24bet currently parse their direct `events/list` and `events/listBase` responses, but their factor mapping is hard-coded in each parser. Unknown IDs are emitted as `factor_<id>` with the numeric ID as the selection.

The repository already contains a direct-source catalog implementation in `crates/parsers/src/factors_catalog.rs`, used by the Marathon parser. It requests the bookmaker platform's own `line/factorsCatalog` endpoints and parses `independentFactors`, `sportBasicFactors`, and `tables`. It supports sport-scoped definitions and refuses ambiguous factor definitions when sport-specific variants disagree.

## Direct endpoints already configured in the parsers

| Bookmaker | Event API host | Scope market |
|---|---|---:|
| Fonbet | `line-lb61-w.bk6bba-resources.com` (event path includes `/ma/`) | 1600 |
| PARI | `line-lb51-w.pb06e2-resources.ru` | 2300 |
| 24bet | `line51.tf39be-resources.com` | 3000 |

These are first-party bookmaker/platform endpoints already present in the parser source. No aggregator is involved. The catalog endpoint's availability and schema for each exact host/scope still need runtime verification.

## Recommended implementation sequence

1. Add a `FactorsCatalog` instance to each parser using its own direct host and scope-market value.
2. Load the catalog through the existing cache/single-flight implementation before parsing the event snapshot; catalog failure must not break otherwise valid event ingestion.
3. Resolve factor IDs with `get_factor_for_sport` using the event's source `sportId`.
4. Reuse the proven market/selection classifier already used by the Marathon parser, or extract it into a shared helper with regression fixtures.
5. Retain conservative hard-coded fallbacks only for IDs whose meaning is already proven. Unknown or ambiguous IDs remain unclassified and must not be normalized into a common market.
6. Add fixtures for catalog definitions, sport-specific ID collisions, line values, child-market context, and unknown IDs before enabling dynamic classification.
7. Compare catalog-derived definitions against raw event factor distributions for each bookmaker separately. Do not assume IDs are interchangeable merely because the APIs share a platform family.

## Important 24bet registry correction

The factory registers the same 24bet parser instance under both `_24bet` and `bet24`. `get_all()` and `get_enabled()` previously enumerated map values without deduplication, so the same bookmaker could be fetched and published twice. Commit `4d3fb02417372ac2eea7a3532a11f64f9d73c9dd` changes these methods to enumerate each parser slug once and adds an alias regression test.

## Validation status

- The alias-deduplication code and regression test are committed on `work/core-engine-refactor`.
- GitHub's commit-workflow lookup returned no workflow runs for that commit; no CI pass is claimed.
- Rust formatting, compilation and parser tests still need to be run in a build environment.
- No factor IDs have been newly mapped based solely on numeric patterns.