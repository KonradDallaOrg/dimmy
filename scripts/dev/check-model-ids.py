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

# Which env var(s) carry each provider's key — both the conventional
# `*_API_KEY` names and the repo `.env` convention (`*_KEY`). First non-empty
# wins. The repo `.env` (if present) is auto-loaded so the script just works.
ENV_KEYS = {
    "openai": ["OPENAI_API_KEY", "OPENAI_KEY"],
    "anthropic": ["ANTHROPIC_API_KEY", "ANTHROPIC_KEY"],
    "gemini": ["GEMINI_API_KEY", "GOOGLE_API_KEY", "GEMINI_KEY"],
    "groq": ["GROQ_API_KEY", "GROQ_KEY"],
    "deepgram": ["DEEPGRAM_API_KEY", "DEEPGRAM_KEY"],
    "openrouter": ["OPENROUTER_API_KEY", "OPENROUTER_KEY"],
    "together": ["TOGETHER_API_KEY", "TOGETHER_KEY"],
    "fireworks": ["FIREWORKS_API_KEY", "FIREWORKS_KEY"],
}


def load_dotenv():
    """Best-effort load of the repo-root .env into os.environ (does NOT
    override already-set vars). Single-line `KEY=value` / `export KEY=value`,
    optional surrounding quotes, `#` comments. Multi-line / malformed lines
    are skipped — we only need the single-line provider keys."""
    env_path = Path(__file__).resolve().parents[2] / ".env"
    if not env_path.exists():
        return
    for raw in env_path.read_text(encoding="utf-8", errors="ignore").splitlines():
        line = raw.strip()
        if not line or line.startswith("#") or "=" not in line:
            continue
        if line.startswith("export "):
            line = line[len("export "):]
        name, _, value = line.partition("=")
        name = name.strip()
        value = value.strip().strip('"').strip("'")
        if name and value:
            os.environ.setdefault(name, value)

# Notable-new patterns: which live ids are worth suggesting as additions
# (current-generation TEXT models only, so the report isn't flooded with
# legacy / fine-tuned / non-text variants).
NOTABLE = {
    "openai": re.compile(r"^gpt-5(\.|-|$)"),
    "anthropic": re.compile(r"^claude-(opus|sonnet|haiku)-(4-[6-9]|[5-9])"),
    "gemini": re.compile(r"^gemini-3(\.|-|$)"),
    "groq": re.compile(r"^(llama|qwen|deepseek|gpt-oss|moonshot|kimi)"),
    "deepgram": re.compile(r"x^"),     # too granular; suggestions off
    "openrouter": re.compile(r"x^"),
    "together": re.compile(r"x^"),
    "fireworks": re.compile(r"x^"),
}

# Substrings marking a non-text modality / niche variant we never surface as
# a chat / recap candidate.
NON_TEXT = ("image", "-tts", "tts-", "audio", "computer-use", "embedding",
            "embed", "-search", "moderation", "realtime", "-vision", "dall-e")

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
    # A browser-ish User-Agent is REQUIRED: Groq + Together sit behind
    # Cloudflare, which 403s the default python-urllib UA ("error code: 1010").
    # Without this the checker silently can't validate those two providers.
    headers = dict(headers)
    headers.setdefault(
        "User-Agent",
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 "
        "(KHTML, like Gecko) Chrome/124.0 Safari/537.36")
    headers.setdefault("Accept", "application/json")
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
    # OpenAI-compatible: bearer token. Most return { "data": [ {id} ] };
    # Together returns a bare top-level list.
    data = http_json(endpoint, {"Authorization": f"Bearer {key}"})
    arr = data if isinstance(data, list) else data.get("data", [])
    return {m["id"] for m in arr if isinstance(m, dict) and "id" in m}


def is_live(cid, live):
    """A catalog id counts as live if it's listed verbatim OR it's the short
    alias of a granular live id (e.g. Deepgram `nova-3` -> `nova-3-general`,
    where the API accepts the short form even though /models lists variants)."""
    if cid in live:
        return True
    return any(lid.startswith(cid + "-") for lid in live)


def suggest(catalog_id, live):
    """Best-effort 'did you mean' for a stale id — live ids that share a stem."""
    stem = catalog_id.split("/")[-1][:8].lower()
    hits = sorted(l for l in live if stem and stem in l.lower())
    return hits[:3]


def main():
    load_dotenv()
    catalog = json.loads(CATALOG.read_text(encoding="utf-8"))
    any_stale = False
    checked = 0

    for prov in catalog["providers"]:
        pid = prov["id"]
        endpoint = prov.get("models_endpoint")
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
        models = prov["models"]
        # Hard-check LLM / recap ids (these 404 at runtime when wrong — the
        # bugs we keep hitting). Soft-check STT-only ids: some providers accept
        # short aliases not in /models (Deepgram) or serve STT on a different
        # endpoint (Fireworks/Together audio), so a miss there is informational.
        def is_hard(m):
            return "llm" in m["tasks"] or "recap" in m["tasks"]

        hard_stale = [m["id"] for m in models if is_hard(m) and not is_live(m["id"], live)]
        soft_miss = [m["id"] for m in models if not is_hard(m) and not is_live(m["id"], live)]
        ok_n = len(models) - len(hard_stale) - len(soft_miss)

        notable = NOTABLE.get(pid, re.compile(r"x^"))
        cat_ids = {m["id"] for m in models}
        new = sorted(l for l in live
                     if notable.search(l) and l not in cat_ids
                     and not any(x in l for x in NON_TEXT))

        print(f"  {GREEN}OK: {ok_n}/{len(models)}{RST}  "
              f"{DIM}(live models: {len(live)}){RST}")
        for cid in hard_stale:
            any_stale = True
            hint = suggest(cid, live)
            tail = f"  {DIM}did you mean: {', '.join(hint)}{RST}" if hint else ""
            print(f"  {RED}STALE  {cid}{RST}{tail}  {DIM}(llm/recap — remove){RST}")
        for cid in soft_miss:
            print(f"  {YELL}stt?   {cid}{RST}  {DIM}not in /models — verify the STT alias/endpoint{RST}")
        for n in new[:12]:
            print(f"  {YELL}NEW?   {n}{RST}")
        if len(new) > 12:
            print(f"  {DIM}...and {len(new) - 12} more{RST}")

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
