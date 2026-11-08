#!/usr/bin/env python3
"""Print AI coding plan usage limits as JSON, using the logins their CLIs already saved.

Every account is a config dir: ~/.claude, ~/.claude_work (CLAUDE_CONFIG_DIR), ~/.codex, ~/.codex-2 (CODEX_HOME)...
Tokens are never refreshed here: that could race the CLI's own token rotation and log you out.
"""
import base64
import glob
import json
import os
import shutil
import sqlite3
import subprocess
import sys
import time
import urllib.parse
import urllib.request
from datetime import datetime, timedelta, timezone

HOME = os.path.expanduser("~")
NAMES = {300: "Session", 10080: "Weekly", 43200: "Monthly"}


def get(url, headers):
    return json.load(urllib.request.urlopen(urllib.request.Request(url, headers=headers), timeout=15))


def jwt(token):
    return json.loads(base64.urlsafe_b64decode(token.split(".")[1] + "=="))


def ts(iso):
    return datetime.fromisoformat(iso.replace("Z", "+00:00")).timestamp() if iso else None


def fresh(expires, cli):
    if expires < time.time():
        raise RuntimeError(f"login expired, open {cli} once to refresh it")


def claude_version():
    # The native install's binary is named after its version; an npm one resolves to cli.js: ask it.
    v = os.path.basename(os.path.realpath(shutil.which("claude") or "x"))
    if not v[:1].isdigit():
        try:
            v = subprocess.run(["claude", "--version"], capture_output=True, text=True, timeout=10).stdout.split()[0]
        except (OSError, IndexError, subprocess.SubprocessError):
            pass
    return v


def claude(d):
    creds = json.load(open(f"{d}/.credentials.json"))["claudeAiOauth"]
    # The default dir keeps its account info next to it, CLAUDE_CONFIG_DIR ones keep it inside.
    info = f"{HOME}/.claude.json" if d == f"{HOME}/.claude" else f"{d}/.claude.json"
    try:
        email = json.load(open(info))["oauthAccount"]["emailAddress"]
    except (OSError, ValueError, KeyError):
        email = ""
    fresh(creds["expiresAt"] / 1000, "claude")
    # cedar_ember=1 adds the free limit resets Anthropic grants (same request T3 Code makes).
    data = get("https://api.anthropic.com/api/oauth/usage?cedar_ember=1", {
        "Authorization": "Bearer " + creds["accessToken"], "anthropic-beta": "oauth-2025-04-20",
        # Generic clients get 429 here, and other surfaces aren't eligible for resets: identify as the CLI.
        "User-Agent": f"claude-cli/{claude_version()} (external, cli)"})
    names = {"five_hour": "Session", "seven_day": "Weekly", "seven_day_opus": "Weekly Opus",
             "seven_day_sonnet": "Weekly Sonnet"}
    grants = data.get("cedar_ember") or {}
    tickets = sum(g.get("resets_left", 0) for g in grants.get("grants") or [] if grants.get("eligible")
                  and g.get("usable_now") and not g.get("paused") and (ts(g.get("ends_at")) or 1e12) > time.time())
    return email, [{"name": n, "used": data[k]["utilization"], "resets": ts(data[k].get("resets_at"))}
                   for k, n in names.items() if data.get(k)], tickets


def codex(d):
    tokens = json.load(open(f"{d}/auth.json"))["tokens"]
    email = jwt(tokens["id_token"]).get("email", "")
    try:
        fresh(jwt(tokens["access_token"])["exp"], "codex")
        data = get("https://chatgpt.com/backend-api/wham/usage", {
            "Authorization": "Bearer " + tokens["access_token"], "ChatGPT-Account-Id": tokens["account_id"],
            "User-Agent": "codex-cli"})
        rl = data["rate_limit"]
        windows = [(w["limit_window_seconds"] // 60, w) for w in (rl.get("primary_window"), rl.get("secondary_window")) if w]
        # Free limit resets ChatGPT hands out, spendable from the Codex app.
        tickets = (data.get("rate_limit_reset_credits") or {}).get("available_count") or 0
        return email, [{"name": NAMES.get(m, f"{m // 60}h"), "used": w["used_percent"], "resets": w.get("reset_at")}
                       for m, w in windows], tickets
    except Exception as e:
        # Expired login or API hiccup: the newest local session log still has the numbers from your last run.
        for path in sorted(glob.glob(f"{d}/sessions/*/*/*/rollout-*.jsonl"), key=os.path.getmtime, reverse=True):
            for line in reversed(open(path).readlines()):
                try:
                    rl = json.loads(line).get("payload", {}).get("rate_limits") if '"rate_limits"' in line else None
                except ValueError:
                    continue  # codex may still be writing the last line
                out = [{"name": NAMES.get(w["window_minutes"], f"{w['window_minutes'] // 60}h"),
                        "used": w["used_percent"], "resets": w.get("resets_at")}
                       for w in (rl or {}).values() if isinstance(w, dict) and w.get("window_minutes")]
                if out:
                    return email, out
        raise e


def grok(d):
    # One entry per login; the most recently refreshed one is the account grok is using.
    auth = max(json.load(open(f"{d}/auth.json")).values(), key=lambda a: a["expires_at"])
    fresh(ts(auth["expires_at"]), "grok")
    data = get("https://cli-chat-proxy.grok.com/v1/billing?format=credits",
               {"Authorization": "Bearer " + auth["key"]})["config"]
    return auth.get("email", ""), grok_limits(data)


def grok_limits(data):
    # A lapsed plan still answers, but with no creditUsagePercent: nothing to show.
    if "creditUsagePercent" not in data:
        return []
    period = data.get("currentPeriod") or {}
    return [{"name": "Weekly" if "WEEKLY" in period.get("type", "") else "Credits",
             "used": data["creditUsagePercent"], "resets": ts(period.get("end"))}]


def antigravity(d):
    # Google never rotates refresh tokens, so minting an access token here can't log Antigravity out.
    creds = json.load(open(f"{d}/acp_token.json"))
    body = urllib.parse.urlencode({"client_id": creds["client_id"], "client_secret": creds["client_secret"],
                                   "refresh_token": creds["refresh_token"], "grant_type": "refresh_token"}).encode()
    try:
        login = json.load(urllib.request.urlopen(creds["token_uri"], body, timeout=15))
    except urllib.error.HTTPError as e:
        raise RuntimeError("login expired, open Antigravity once to refresh it") from e

    def call(method, payload):
        request = urllib.request.Request(f"https://cloudcode-pa.googleapis.com/v1internal:{method}",
                                         json.dumps(payload).encode(), {
            "Authorization": "Bearer " + login["access_token"], "Content-Type": "application/json",
            "User-Agent": "antigravity"})
        return json.load(urllib.request.urlopen(request, timeout=15))

    project = call("loadCodeAssist", {"metadata": {"ideType": "ANTIGRAVITY"}}).get("cloudaicompanionProject")
    models = call("fetchAvailableModels", {"project": project or creds.get("project_id")})["models"]
    return jwt(login["id_token"]).get("email", ""), antigravity_limits(models)


def antigravity_limits(models):
    """Gemini and the other vendors' models (Claude, GPT-OSS) have separate quotas: one bar each, from the
    model with the least left. A model with no remainingFraction has none left."""
    groups = {}
    for key, m in models.items():
        q = m.get("quotaInfo")
        if not q or not m.get("displayName"):
            continue  # tab_*/chat_* are completion internals, not models you pick
        name = "Gemini" if key.startswith("gemini") else "Claude & GPT"
        used = 100 * (1 - q.get("remainingFraction", 0))
        if name not in groups or used > groups[name]["used"]:
            groups[name] = {"name": name, "used": round(used, 1), "resets": ts(q.get("resetTime"))}
    return [groups[n] for n in ("Gemini", "Claude & GPT") if n in groups]


def openrouter_key():
    """opencode keeps the key it uses in its database (its auth.json copy can be an old dead one)."""
    if os.environ.get("OPENROUTER_API_KEY"):
        return os.environ["OPENROUTER_API_KEY"]
    db = f"{HOME}/.local/share/opencode/opencode.db"
    if not os.path.exists(db):
        raise FileNotFoundError(db)
    row = sqlite3.connect(f"file:{db}?mode=ro", uri=True).execute(
        "SELECT value FROM credential WHERE integration_id = 'openrouter' AND active ORDER BY time_updated DESC").fetchone()
    if not row:
        raise FileNotFoundError("no OpenRouter key in opencode")
    return json.loads(row[0])["key"]


def openrouter(_):
    headers = {"Authorization": "Bearer " + openrouter_key()}
    credits = get("https://openrouter.ai/api/v1/credits", headers)["data"]
    key = get("https://openrouter.ai/api/v1/key", headers)["data"]
    return "", openrouter_limits(credits, key, time.time())


def openrouter_limits(credits, key, now):
    """Money, not a plan: "used" still drives the bar, "value" is what the label shows instead of a percent."""
    def bar(name, total, spent, value, resets=None):
        return {"name": name, "used": round(100 * spent / total, 1) if total else 100, "resets": resets, "value": value}
    total, spent = credits["total_credits"], credits["total_usage"]
    out = [bar("Credits", total, spent, f"${max(0, total - spent):.2f} left")]
    reset = key.get("limit_reset")  # daily / weekly / monthly / None
    if key.get("limit"):
        period = {"daily": "day", "weekly": "week", "monthly": "month"}.get(reset)
        used = key["limit"] - key["limit_remaining"]  # what OpenRouter enforces, BYOK included when it counts
        out.append(bar(f"Key this {period}" if period else "Key limit", key["limit"], used,
                       f"${used:.2f}/${key['limit']:g}", next_reset(period, now)))
    return out


def next_reset(period, now):
    """OpenRouter resets key limits at midnight UTC (Mondays for weekly, the 1st for monthly)."""
    if not period:
        return None
    t = datetime.fromtimestamp(now, timezone.utc).replace(hour=0, minute=0, second=0, microsecond=0)
    if period == "day":
        return (t + timedelta(days=1)).timestamp()
    if period == "week":
        return (t + timedelta(days=7 - t.weekday())).timestamp()
    return (t.replace(day=1) + timedelta(days=32)).replace(day=1).timestamp()


def t3(driver, *keys):
    """Account dirs set up as provider instances in T3 Code (Settings > Providers)."""
    try:
        instances = json.load(open(f"{HOME}/.t3/userdata/settings.json"))["providerInstances"].values()
    except (OSError, ValueError, KeyError):
        return []
    return [os.path.expanduser(i["config"][k]) for i in instances if i.get("driver") == driver
            for k in keys if i.get("config", {}).get(k)]


def why(name, e):
    """Short error text for the widget; a bare KeyError would just print the missing key in quotes."""
    return f"{name} changed its usage page (no {str(e).strip(chr(39))})" if isinstance(e, KeyError) else str(e)


def skipped(argv):
    """Provider names after a --skip arg (comma separated); their logins and APIs are never touched."""
    for i, a in enumerate(argv):
        if a == "--skip" and i + 1 < len(argv):
            return {s for s in argv[i + 1].split(",") if s}
    return set()


PROVIDERS = (
    ("Claude", claude, [os.environ.get("CLAUDE_CONFIG_DIR", ""), f"{HOME}/.claude", *glob.glob(f"{HOME}/.claude?*/"),
                        *t3("claudeAgent", "homePath")]),
    ("Codex", codex, [os.environ.get("CODEX_HOME", ""), f"{HOME}/.codex", *glob.glob(f"{HOME}/.codex?*/"),
                      *t3("codex", "homePath", "shadowHomePath"),
                      # ChatGPT accounts added with T3 Code's own login button
                      *glob.glob(f"{HOME}/.t3/userdata/providers/codex/*/shadow/")]),
    ("Grok", grok, [f"{HOME}/.grok"]),
    # T3 Code's Antigravity provider keeps its Google login here.
    ("Antigravity", antigravity, glob.glob(f"{HOME}/.t3/userdata/providers/antigravity/*/antigravity-acp/")),
    ("OpenRouter", openrouter, ["openrouter"]),  # one account, found by openrouter_key()
)


def selfcheck():
    """python3 limits.py --check: the parsing, on canned API answers."""
    models = {"gemini-3-flash": {"displayName": "Gemini 3 Flash", "quotaInfo": {"resetTime": "2026-11-09T22:29:34Z"}},
              "gemini-2.5-pro": {"displayName": "Gemini 2.5 Pro", "quotaInfo": {"remainingFraction": 0.5}},
              "claude-opus-4-6-thinking": {"displayName": "Claude Opus", "quotaInfo": {"remainingFraction": 0.75}},
              "tab_flash_lite_preview": {"quotaInfo": {"remainingFraction": 0}}}
    assert antigravity_limits(models) == [{"name": "Gemini", "used": 100.0, "resets": ts("2026-11-09T22:29:34Z")},
                                          {"name": "Claude & GPT", "used": 25.0, "resets": None}]
    now = datetime(2026, 11, 6, 18, 30, tzinfo=timezone.utc).timestamp()  # a Friday
    assert next_reset("day", now) == datetime(2026, 11, 7, tzinfo=timezone.utc).timestamp()
    assert next_reset("week", now) == datetime(2026, 11, 9, tzinfo=timezone.utc).timestamp()
    assert next_reset("month", datetime(2026, 12, 31, tzinfo=timezone.utc).timestamp()) == \
        datetime(2027, 1, 1, tzinfo=timezone.utc).timestamp()
    credits = {"total_credits": 20, "total_usage": 4.9}
    key = {"limit": 20, "limit_reset": "monthly", "limit_remaining": 18.5, "usage": 4.9, "usage_monthly": 1.5}
    assert openrouter_limits(credits, key, now) == [
        {"name": "Credits", "used": 24.5, "resets": None, "value": "$15.10 left"},
        {"name": "Key this month", "used": 7.5, "resets": next_reset("month", now), "value": "$1.50/$20"}]
    key.update(include_byok_in_limit=True, byok_usage_monthly=1, limit_remaining=17.5)
    assert openrouter_limits(credits, key, now)[1]["value"] == "$2.50/$20"
    assert openrouter_limits({"total_credits": 0, "total_usage": 0}, {"limit": None, "usage": 0}, now) == [
        {"name": "Credits", "used": 100, "resets": None, "value": "$0.00 left"}]
    lapsed = {"currentPeriod": {"type": "USAGE_PERIOD_TYPE_WEEKLY", "end": "2026-11-14T18:59:33.334+00:00"},
              "prepaidBalance": {"val": 0}}  # real shape after the plan ended, 2026-11-08
    assert grok_limits(lapsed) == []
    assert grok_limits({**lapsed, "creditUsagePercent": 40.0}) == [
        {"name": "Weekly", "used": 40.0, "resets": ts("2026-11-14T18:59:33.334+00:00")}]
    assert why("Grok", KeyError("creditUsagePercent")) == "Grok changed its usage page (no creditUsagePercent)"
    assert why("Grok", KeyError()) == "Grok changed its usage page (no )"
    assert why("Codex", RuntimeError("login expired")) == "login expired"
    assert skipped(["limits.py", "--skip", "Grok,Codex"]) == {"Grok", "Codex"}
    assert skipped(["limits.py"]) == set()
    assert skipped(["limits.py", "--skip", ""]) == set()
    print("ok")


if __name__ == "__main__" and "--check" in sys.argv:
    selfcheck()
elif __name__ == "__main__":
    CACHE = f"{HOME}/.cache/leftovers.json"
    try:
        cache = json.load(open(CACHE))
    except (FileNotFoundError, ValueError):
        cache = {}
    result = {"providers": []}
    skip = skipped(sys.argv)
    for name, fetch, dirs in PROVIDERS:
        if name in skip:
            continue  # unticked in the widget's Configure…: not even its login gets read
        for d in dict.fromkeys(os.path.normpath(d) for d in dirs if d):  # dedupe, keep order
            key = f"{name}:{d}"
            try:
                account, limits, *tickets = fetch(d)  # grok has no free resets
                # The Codex session-log fallback has no count: keep the last one rather than hide it.
                cache[key] = {"account": account, "limits": limits,
                              "tickets": tickets[0] if tickets else cache.get(key, {}).get("tickets", 0)}
                # No limits at all (e.g. the plan lapsed): say so instead of an empty card. Cache keeps [] so
                # an old bar never comes back as "last known numbers".
                result["providers"].append({"name": name, **cache[key], **({} if limits else {"error": "no active plan"})})
            except FileNotFoundError:
                pass  # not installed / not an account dir: just don't show it
            except Exception as e:
                # Usage endpoints rate-limit (429); last good numbers still count down correctly.
                last = cache.get(key, {"account": "", "limits": []})
                result["providers"].append({"name": name, **last, "error": why(name, e)})
    os.makedirs(os.path.dirname(CACHE), exist_ok=True)
    json.dump(cache, open(CACHE, "w"))
    print(json.dumps(result))
