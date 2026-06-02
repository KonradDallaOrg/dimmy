#!/usr/bin/env python3
"""check-model-ids.py — validate assets/model-catalog.json against each
provider's LIVE /models endpoint.

Local dev tool, NOT CI: it needs network access + your API keys (read from
environment variables) so it never runs on the build machine. Use it to keep
the catalog honest — it flags ids we list that the provider no longer serves
(remove them; they 404 at runtime, e.g. the bare `gpt-5.4` / `gemini-3.1-pro`)
and notable new models we don't list yet (consider adding).

Usage (set only the keys you have — missing providers are skipped):

    export OPENAI_API_KEY=...
    export ANTHROPIC_API_KEY=...
    export GEMINI_API_KEY=...        # or GOOGLE_API_KEY
    export GROQ_API_KEY=...
    export DEEPGRAM_API_KEY=...
    export OPENROUTER_API_KEY=... TOGETHER_API_KEY=... FIREWORKS_API_KEY=...
    python3 scripts/dev/check-model-ids.py

Exit code: 1 if any catalog id is stale (missing from the live list), else 0.
The key is only ever sent to its own provider over HTTPS; it is never printed.
"""

import json
import os
import re
import sys
import urllib.error
import urllib.request
from pathlib import Path

CATALOG = Path(__file__).resolve().parents[2] / "assets" / "model-catalog.json"

# Which env var(s) carry each provider's key. First non-empty wins.
ENV_KEYS = {
    "openai": ["OPENAI_API_KEY"],
    "anthropic": ["ANTHROPIC_API_KEY"],
    "gemini": ["GEMINI_API_KEY", "GOOGLE_API_KEY"],
    "groq": ["GROQ_API_KEY"],
    "deepgram": ["DEEPGRAM_API_KEY"],
    "openrouter": ["OPENROUTER_API_KEY"],
    "together": ["TOGETHER_API_KEY"],
    "fireworks": ["FIREWORKS_API_KEY"],
}

# Notable-new patterns: which live ids are worth suggesting as additions
# (so the report isn't flooded with every legacy / fine-tuned model).
NOTABLE = {
    "openai": re.compile(r"^(gpt-[0-9]|o[0-9])"),
    "anthropic": re.compile(r"^claude-(opus|sonnet|haiku)"),
    "gemini": re.compile(r"^gemini-[0-9]"),
    "groq": re.compile(r"."),
    "deepgram": re.compile(r"^nova"),
    "openrouter": re.compile(r"x^"),   # too many; suggestions off
    "together": re.compile(r"x^"),
    "fireworks": re.compile(r"x^"),
}

if sys.stdout.isatty() and os.environ.get("NO_COLOR") is None:
    GREEN, RED, YELL, DIM, RST = (
        "\033[32m", "\033[31m", "\033[33m", "\033[2m", "\033[0m")
else:
    GREEN = RED = YELL = DIM = RST = ""


def env_key(provider):
    for name in ENV_KEYS.get(provider, []):
        v = os.environ.get(name)
        if v:
            return v
    return None


def http_json(url, headers):
    req = urllib.request.Request(url, headers=headers)
    with urllib.request.urlopen(req, timeout=25) as resp:
        return json.loads(resp.read().decode("utf-8"))


def live_ids(provider, endpoint, auth, key):
    """Return the set of live model ids for a provider, or raise on error."""
    if auth == "gemini":
        sep = "&" if "?" in endpoint else "?"
        data = http_json(f"{endpoint}{sep}key={key}&pageSize=1000", {})
        return {m["name"].split("/", 1)[-1] for m in data.get("models", [])}
    if auth == "anthropic":
        sep = "&" if "?" in endpoint else "?"
        data = http_json(f"{endpoint}{sep}limit=1000",
                         {"x-api-key": key, "anthropic-version": "2023-06-01"})
        return {m["id"] for m in data.get("data", [])}
    if auth == "deepgram":
        data = http_json(endpoint, {"Authorization": f"Token {key}"})
        ids = set()
        for group in data.values():
            if isinstance(group, list):
                for m in group:
                    if isinstance(m, dict):
                        ids.add(m.get("canonical_name") or m.get("name"))
        return {i for i in ids if i}
    # OpenAI-compatible: bearer token, { "data": [ { "id": ... } ] }
    data = http_json(endpoint, {"Authorization": f"Bearer {key}"})
    arr = data.get("data", data if isinstance(data, list) else [])
    return {m["id"] for m in arr if isinstance(m, dict) and "id" in m}


def suggest(catalog_id, live):
    """Best-effort 'did you mean' for a stale id — live ids that share a stem."""
    stem = catalog_id.split("/")[-1][:8].lower()
    hits = sorted(l for l in live if stem and stem in l.lower())
    return hits[:3]


def main():
    catalog = json.loads(CATALOG.read_text(encoding="utf-8"))
    any_stale = False
    checked = 0

    for prov in catalog["providers"]:
        pid = prov["id"]
        endpoint = prov.get("models_endpoint")
        catalog_ids = [m["id"] for m in prov["models"]]
        key = env_key(pid)

        print(f"\n{prov['name']} ({pid})")
        if not endpoint:
            print(f"  {DIM}no /models endpoint — skipped{RST}")
            continue
        if not key:
            names = " / ".join(ENV_KEYS.get(pid, []))
            print(f"  {DIM}no key in env ({names}) — skipped{RST}")
            continue

        try:
            live = live_ids(pid, endpoint, prov["auth"], key)
        except urllib.error.HTTPError as e:
            print(f"  {RED}HTTP {e.code} fetching /models — check the key{RST}")
            continue
        except Exception as e:  # noqa: BLE001 — dev tool, surface anything
            print(f"  {RED}error: {type(e).__name__}: {e}{RST}")
            continue

        checked += 1
        ok = [c for c in catalog_ids if c in live]
        stale = [c for c in catalog_ids if c not in live]
        notable = NOTABLE.get(pid, re.compile(r"x^"))
        new = sorted(l for l in live
                     if notable.search(l) and l not in catalog_ids)

        print(f"  {GREEN}OK: {len(ok)}/{len(catalog_ids)}{RST}  "
              f"{DIM}(live models: {len(live)}){RST}")
        for c in stale:
            any_stale = True
            hint = suggest(c, live)
            tail = f"  {DIM}did you mean: {', '.join(hint)}{RST}" if hint else ""
            print(f"  {RED}STALE  {c}{RST}{tail}")
        for n in new[:15]:
            print(f"  {YELL}NEW?   {n}{RST}")
        if len(new) > 15:
            print(f"  {DIM}...and {len(new) - 15} more new candidates{RST}")

    print()
    if checked == 0:
        print(f"{YELL}No providers checked — set at least one *_API_KEY env var.{RST}")
        return 0
    if any_stale:
        print(f"{RED}STALE ids found — remove them from the catalog "
              f"(they 404 at runtime).{RST}")
        return 1
    print(f"{GREEN}All catalog ids are live.{RST}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
