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

## Implementation progress — 2026-11-27

The first direct-catalog integration pass is now committed:

| Parser | First-party catalog base URL | Scope | Status |
|---|---|---:|---|
| Fonbet | `https://line-lb61-w.bk6bba-resources.com` | 1600 | Catalog load and sport-scoped resolution wired into live/prematch parsing |
| PARI | `https://line-lb51-w.pb06e2-resources.ru` | 2300 | Catalog load and sport-scoped resolution wired into live/prematch parsing |
| 24bet | `https://line51.tf39be-resources.com` | 3000 | Catalog load and sport-scoped resolution wired into live/prematch parsing |

Implementation details:
- Each parser uses the existing `FactorsCatalog` cache and its bookmaker's own line-platform host; no external provider or aggregator is used.
- Source `sportId` is passed into `get_factor_for_sport` so sport-scoped factor IDs can be resolved safely.
- If an ID exists in the catalog but its meaning is ambiguous for the current sport, it is quarantined. The parser does not fall back to a numeric guess.
- If the catalog does not contain an ID, the existing legacy factor mapping remains available; genuinely unknown IDs remain `factor_<id>`.
- Odds IDs are now generated from semantic market, selection and line keys, and duplicate direct-source quotes retain the best price.
- Event IDs, factor event references, sport IDs and line values accept numeric JSON strings as well as numeric JSON values.
- Regression fixtures exercise catalog-defined mapping and ambiguous-ID quarantine for all three parsers. The fixtures are synthetic; they verify use of catalog semantics, not the real-world meaning of a particular factor ID.

## Validation status

- The source edits and regression tests are committed to `work/core-engine-refactor`.
- A local `cargo test` could not be run in this session because the repository could not be cloned into the build environment (GitHub DNS/network access was unavailable).
- GitHub commit status lookup returned no status checks for the latest parser test commit. Do not interpret that as a passing CI run.
- Before claiming live factor coverage improved, capture real `sportBasicFactors`/`tables` payloads from each direct bookmaker host and compare the catalog-derived mapping with their actual `customFactors` rows.

## Recommended implementation sequence

1. Add a `FactorsCatalog` instance to each parser using its own direct host and scope-market value.
2. Load the catalog through the existing cache/single-flight implementation before parsing the event snapshot; catalog failure must not break otherwise valid event ingestion.
3. Resolve factor IDs with `get_factor_for_sport` using the event's source `sportId`.
4. Reuse the proven market/selection classifier already used by the Marathon parser, or extract it into a shared helper with regression fixtures.
5. Retain conservative hard-coded fallbacks only for IDs whose meaning is already proven. Unknown or ambiguous IDs remain unclassified and must not be normalized into a common market. The Fonbet, PARI and 24bet event parsers now explicitly drop unmapped `factor_<id>` values from comparable odds while preserving the source payload for diagnostics.
6. Add fixtures for catalog definitions, sport-specific ID collisions, line values, child-market context, and unknown IDs before enabling dynamic classification.
7. Compare catalog-derived definitions against raw event factor distributions for each bookmaker separately. Do not assume IDs are interchangeable merely because the APIs share a platform family.

## Important 24bet registry correction

The factory registers the same 24bet parser instance under both `_24bet` and `bet24`. `get_all()` and `get_enabled()` previously enumerated map values without deduplication, so the same bookmaker could be fetched and published twice. Commit `4d3fb02417372ac2eea7a3532a11f64f9d73c9dd` changes these methods to enumerate each parser slug once and adds an alias regression test.

## Validation status

- The alias-deduplication code and regression test are committed on `work/core-engine-refactor`.
- GitHub's commit-workflow lookup returned no workflow runs for that commit; no CI pass is claimed.
- Rust formatting, compilation and parser tests still need to be run in a build environment.
- No factor IDs have been newly mapped based solely on numeric patterns.
- Fonbet/PARI/24bet now quarantine unknown numeric factor IDs instead of emitting synthetic `factor_<id>` markets. Their existing sport-ambiguity fixtures also include an unmapped factor regression case.