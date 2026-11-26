# Source quote timestamp audit — 2026-11-26

## Purpose

This audit checks whether the public provider payloads inspected by the parser project expose a timestamp for the **price itself**. It explicitly separates price-update time from event start time, match clock, packet/catalogue version and local parser ingestion time.

## Observed public payload fields

| Provider / feed family | Observed price payload | Time-like fields found | Interpretation |
|---|---|---|---|
| Fonbet, PARI, Marathon, Bettery, 24bet (shared line51 API) | customFactors[].factors[] includes factor code/value (f, v); event object includes place and startTime | eventMiscs.timerUpdateTimestampMsec, liveEventInfos.timerTimestampMsec; root has packetVersion, factorsVersion and catalogue version counters | Event startTime is scheduled start. timerUpdateTimestampMsec / timerTimestampMsec anchor match clock/score state, not an individual odds quote. Root versions are catalogue/delta versions, not per-outcome timestamps. Sample factor records did not expose a price update time. |
| Olimp | Outcome contains probability, groupName, shortName, IDs, categories and market identifiers | startDateTime is on event, not outcome | No price-update timestamp in sampled outcome objects. |
| SportBet | Market contains id, status, name, outcomes; outcome contains odd, active, specifiers, participant IDs and names | Event scheduled is used as start time | No price-update timestamp in sampled market/outcome objects. |
| Zenit | Live factor contains h (price), id, oddKey, o, t, st and display/state fields | Match/event clock is separate from factor | No price-update timestamp in sampled factor objects. |
| Winline | Live line packet contains line ID, state, event ID, outcome count, prices, market ID, coefficient/selection metadata | Event packet contains a timestamp parsed into Event.start_time; live score/clock metadata is separate | The timestamp in the event packet is event start time. The line/odds packet parser does not receive a price-update timestamp. |
| Leon | Market runners contain price / priceStr, market and runner IDs, open state | kickoff is mapped to event start time | No price-update timestamp was found in the sampled market/runner schema. |
| Betcity | API parsing assigns the local capture time to constructed Odd values | Event start timestamp is parsed separately | No provider quote-update timestamp is currently mapped by this adapter. |
| BetBoom, Liga Stavok, Melbet, Bet-M | Runtime feed blocked in this environment | Not available for schema inspection | Quote-time capability remains unknown until permitted access or authorized feed data is available. |

## Conclusion

The public payloads inspected do not currently provide a documented per-quote update timestamp that can safely populate Odd.timestamp. Several fields that look time-related are not quote timestamps:

- Event startTime, startDateTime, scheduled and Winline event-record timestamps describe the scheduled match.
- timerUpdateTimestampMsec, timerTimestampMsec, score clocks and timers describe live match state.
- packetVersion, factorsVersion and similar counters indicate a catalogue/feed version, not the update time of a specific outcome price.
- The time at which this parser receives a response is an ingestion timestamp, not proof that the bookmaker has just refreshed the displayed price.

The parser must not substitute any of those fields for a quote-update timestamp. Odd.timestamp therefore remains the local parser ingestion time, and runtime diagnostics correctly keep bookmaker_quote_freshness_verified=false.

## Evidence limits and next condition for closure

This is a schema audit of the accessible public endpoints and sample payloads used by the current adapters, not proof that no provider has an undocumented or private price-history endpoint. The four blocked operators could not be inspected. Reopen this task only when an official API contract, permitted feed response, or authorized session exposes a quote-level update timestamp or a documented monotonic price-change sequence that can be mapped without conflating it with event or match time.
