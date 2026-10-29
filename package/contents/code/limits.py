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
import time
import urllib.parse
import urllib.request
from datetime import datetime

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


def claude(d):
    creds = json.load(open(f"{d}/.credentials.json"))["claudeAiOauth"]
    # The default dir keeps its account info next to it, CLAUDE_CONFIG_DIR ones keep it inside.
    info = f"{HOME}/.claude.json" if d == f"{HOME}/.claude" else f"{d}/.claude.json"
    try:
        email = json.load(open(info))["oauthAccount"]["emailAddress"]
    except (OSError, ValueError, KeyError):
        email = ""
    fresh(creds["expiresAt"] / 1000, "claude")
    data = get("https://api.anthropic.com/api/oauth/usage", {
        "Authorization": "Bearer " + creds["accessToken"], "anthropic-beta": "oauth-2025-04-20",
        # Generic clients get 429 here; identify as the Claude Code whose login this is.
        "User-Agent": "claude-code/" + os.path.basename(os.path.realpath(shutil.which("claude") or "x"))})
    names = {"five_hour": "Session", "seven_day": "Weekly", "seven_day_opus": "Weekly Opus",
             "seven_day_sonnet": "Weekly Sonnet"}
    return email, [{"name": n, "used": data[k]["utilization"], "resets": ts(data[k].get("resets_at"))}
                   for k, n in names.items() if data.get(k)]


def codex(d):
    tokens = json.load(open(f"{d}/auth.json"))["tokens"]
    email = jwt(tokens["id_token"]).get("email", "")
    try:
        fresh(jwt(tokens["access_token"])["exp"], "codex")
        rl = get("https://chatgpt.com/backend-api/wham/usage", {
            "Authorization": "Bearer " + tokens["access_token"], "ChatGPT-Account-Id": tokens["account_id"],
            "User-Agent": "codex-cli"})["rate_limit"]
        windows = [(w["limit_window_seconds"] // 60, w) for w in (rl.get("primary_window"), rl.get("secondary_window")) if w]
        return email, [{"name": NAMES.get(m, f"{m // 60}h"), "used": w["used_percent"], "resets": w.get("reset_at")}
                       for m, w in windows]
    except Exception as e:
        # Expired login or API hiccup: the newest local session log still has the numbers from your last run.
        for path in sorted(glob.glob(f"{d}/sessions/*/*/*/rollout-*.jsonl"), key=os.path.getmtime, reverse=True):
            for line in reversed(open(path).readlines()):
                rl = json.loads(line).get("payload", {}).get("rate_limits") if '"rate_limits"' in line else None
                out = [{"name": NAMES.get(w["window_minutes"], f"{w['window_minutes'] // 60}h"),
                        "used": w["used_percent"], "resets": w.get("resets_at")}
                       for w in (rl or {}).values() if isinstance(w, dict) and w.get("window_minutes")]
                if out:
                    return email, out
        raise e


def cursor(d):
    token = json.load(open(f"{d}/auth.json"))["accessToken"]
    try:
        email = json.load(open(f"{HOME}/.cursor/cli-config.json"))["authInfo"]["email"]
    except (OSError, ValueError, KeyError):
        email = ""
    claims = jwt(token)
    fresh(claims["exp"], "cursor-agent")
    cookie = urllib.parse.quote(claims["sub"].split("|")[-1] + "::" + token)
    data = get("https://cursor.com/api/usage-summary", {"Cookie": "WorkosCursorSessionToken=" + cookie})
    plan, resets = data["individualUsage"]["plan"], ts(data.get("billingCycleEnd"))
    return email, [{"name": "Auto", "used": plan["autoPercentUsed"], "resets": resets},
                   {"name": "API", "used": plan["apiPercentUsed"], "resets": resets}]


def kimi(d):
    creds = json.load(open(f"{d}/credentials/kimi-code.json"))
    fresh(creds["expires_at"], "kimi")
    usages = get("https://api.kimi.com/coding/v1/usages", {"Authorization": "Bearer " + creds["access_token"]})["usages"]
    names = {"limit_5h": "Session", "limit_7d": "Weekly", "limit_month_total": "Monthly",
             "limit_month_code": "Monthly Code"}
    return "", [{"name": n, "used": float(usages[k]["used_ratio"]) * 100, "resets": ts(usages[k].get("reset_time"))}
                for k, n in names.items() if usages.get(k)]


def t3(driver, *keys):
    """Account dirs set up as provider instances in T3 Code (Settings > Providers)."""
    try:
        instances = json.load(open(f"{HOME}/.t3/userdata/settings.json"))["providerInstances"].values()
    except (OSError, ValueError, KeyError):
        return []
    return [os.path.expanduser(i["config"][k]) for i in instances if i.get("driver") == driver
            for k in keys if i.get("config", {}).get(k)]


PROVIDERS = (
    ("Claude", claude, [os.environ.get("CLAUDE_CONFIG_DIR", ""), f"{HOME}/.claude", *glob.glob(f"{HOME}/.claude?*/"),
                        *t3("claudeAgent", "homePath")]),
    ("Codex", codex, [os.environ.get("CODEX_HOME", ""), f"{HOME}/.codex", *glob.glob(f"{HOME}/.codex?*/"),
                      *t3("codex", "homePath", "shadowHomePath"),
                      # ChatGPT accounts added with T3 Code's own login button
                      *glob.glob(f"{HOME}/.t3/userdata/providers/codex/*/shadow/")]),
    ("Cursor", cursor, [f"{HOME}/.config/cursor"]),
    ("Kimi", kimi, [f"{HOME}/.kimi-code", *glob.glob(f"{HOME}/.kimi?*/")]),
)

if __name__ == "__main__":
    CACHE = f"{HOME}/.cache/leftovers.json"
    try:
        cache = json.load(open(CACHE))
    except (FileNotFoundError, ValueError):
        cache = {}
    result = {"providers": []}
    for name, fetch, dirs in PROVIDERS:
        for d in dict.fromkeys(os.path.normpath(d) for d in dirs if d):  # dedupe, keep order
            key = f"{name}:{d}"
            try:
                account, limits = fetch(d)
                cache[key] = {"account": account, "limits": limits}
                result["providers"].append({"name": name, **cache[key]})
            except FileNotFoundError:
                pass  # not installed / not an account dir: just don't show it
            except Exception as e:
                # Usage endpoints rate-limit (429); last good numbers still count down correctly.
                last = cache.get(key, {"account": "", "limits": []})
                result["providers"].append({"name": name, **last, "error": str(e)})
    os.makedirs(os.path.dirname(CACHE), exist_ok=True)
    json.dump(cache, open(CACHE, "w"))
    print(json.dumps(result))
