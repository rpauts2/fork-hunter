#!/usr/bin/env python3
"""Check HTTP reachability of the 20 FNS-listed Russian bookmaker domains.

This is deliberately only a homepage/network check. A reachable homepage does
not prove that a sports feed, event parser, or odds mapping is operational.
No proxy, CAPTCHA, cookie, or anti-bot workaround is attempted.
"""

import asyncio
import json
from datetime import datetime, timezone
from typing import Any

import aiohttp

BOOKMAKERS = {
    "sportbet": "https://sportbet.ru/",
    "marathon": "https://marathonbet.ru/",
    "baltbet": "https://baltbet.ru/",
    "betboom": "https://betboom.ru/",
    "olimp": "https://olimp.bet/",
    "zenit": "https://zenit.win/",
    "leon": "https://leon.ru/",
    "ligastavok": "https://ligastavok.ru/",
    "tennisi": "https://tennisi.bet/",
    "winline": "https://winline.ru/",
    "betcity": "https://betcity.ru/",
    "melbet": "https://melbet.ru/",
    "poolbet": "https://poolbet.ru/",
    "bettery": "https://bettery.ru/",
    "fonbet": "https://fon.bet/",
    "pari": "https://pari.ru/",
    "goldenbet": "https://golden-bet.bet/",
    "betm": "https://bet-m.ru/",
    "bet24": "https://24bet.ru/",
    "abet": "https://abet.ru/",
}

USER_AGENT = (
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) "
    "AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36"
)


def detect_access_gate(status: int, headers: aiohttp.typedefs.LooseHeaders, body: str) -> str | None:
    server = str(headers.get("server", "")).lower()
    body_lower = body.lower()
    if status in (401, 403, 429) and ("qrator" in server or "__qrator" in body_lower):
        return "qrator_or_access_challenge"
    if status in (401, 403, 429) and ("cloudflare" in server or "1000s_box" in body_lower):
        return "cloudflare_or_access_challenge"
    if status in (401, 403, 429):
        return "http_access_restricted"
    return None


async def check_bookmaker(
    session: aiohttp.ClientSession, name: str, url: str
) -> dict[str, Any]:
    try:
        async with session.get(url, allow_redirects=True) as response:
            body = await response.text(errors="ignore")
            status = response.status
            return {
                "name": name,
                "official_fns_url": url,
                "final_url": str(response.url),
                "http_status": status,
                "http_responded": True,
                "page_reachable": 200 <= status < 400,
                "access_restricted": detect_access_gate(status, response.headers, body),
                "content_type": response.headers.get("content-type", ""),
                "html_size": len(body),
                "sports_feed_verified": False,
                "scope": "homepage_http_reachability_only_not_sports_feed_health",
            }
    except (aiohttp.ClientError, asyncio.TimeoutError) as error:
        return {
            "name": name,
            "official_fns_url": url,
            "final_url": None,
            "http_status": None,
            "http_responded": False,
            "page_reachable": False,
            "access_restricted": None,
            "error": f"{type(error).__name__}: {error}",
            "sports_feed_verified": False,
            "scope": "homepage_http_reachability_only_not_sports_feed_health",
        }


async def main() -> None:
    timeout = aiohttp.ClientTimeout(total=12, connect=6, sock_read=8)
    connector = aiohttp.TCPConnector(limit=10, ssl=True)
    headers = {"User-Agent": USER_AGENT, "Accept": "text/html,application/xhtml+xml,*/*"}

    async with aiohttp.ClientSession(
        timeout=timeout, connector=connector, headers=headers
    ) as session:
        results = await asyncio.gather(
            *(check_bookmaker(session, name, url) for name, url in BOOKMAKERS.items())
        )

    reachable = [item for item in results if item["page_reachable"]]
    restricted = [item for item in results if item["access_restricted"]]
    failed = [item for item in results if not item["http_responded"]]

    print("=" * 76)
    print("FNS RUSSIAN BOOKMAKER HOMEPAGE HTTP CHECK")
    print(f"Checked at: {datetime.now(timezone.utc).isoformat()}")
    print("Important: HTTP reachability is not proof of a working sports odds feed.")
    print("=" * 76)

    for item in results:
        status = item["http_status"] if item["http_status"] is not None else "ERROR"
        label = "REACHABLE" if item["page_reachable"] else "RESTRICTED" if item["access_restricted"] else "NOT REACHABLE"
        extra = item.get("access_restricted") or item.get("error") or ""
        print(f"{item['name']:12} | {status!s:>5} | {label:14} | {extra}")

    print("-" * 76)
    print(
        f"FNS domains: {len(results)} | HTTP reachable: {len(reachable)} | "
        f"Access restricted: {len(restricted)} | No HTTP response: {len(failed)}"
    )
    print("Verified sports feeds by this check: 0 (use the Rust runtime parser audit).")

    snapshot = {
        "generated_at": datetime.now(timezone.utc).isoformat(),
        "source_scope": "20 FNS-listed Russian bookmaker domains",
        "check_scope": "homepage HTTP reachability only; not sports feed health",
        "sports_feed_verified_by_this_check": False,
        "counts": {
            "fns_domains": len(results),
            "http_reachable": len(reachable),
            "access_restricted": len(restricted),
            "no_http_response": len(failed),
        },
        "results": results,
    }
    with open("bookmakers_status.json", "w", encoding="utf-8") as file:
        json.dump(snapshot, file, indent=2, ensure_ascii=False)
    print("Snapshot saved to bookmakers_status.json")


if __name__ == "__main__":
    asyncio.run(main())
