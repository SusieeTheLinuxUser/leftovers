# AGENTS.md

Handoff notes for coding agents. User-facing docs are in README.md.

## Files
- `package/contents/code/limits.py`: reads each CLI's saved login, calls the usage APIs, prints JSON
  (`{"providers": [{name, account, limits: [{name, used, resets, value?}], tickets?, error?}]}`).
  Python stdlib only. Never rotates saved logins (Antigravity only mints a short-lived Google access token).
  `--skip Name,Name` skips providers entirely (main.qml passes the ones hidden in Configure…).
- `package/contents/config/main.xml` + `package/contents/config/config.qml`: the `hiddenProviders`
  setting (KConfigXT) and the Configure… page list.
- `package/contents/ui/configGeneral.qml`: one checkbox per provider; unticked ones are not fetched or shown.
- `package/contents/ui/main.qml`: runs `limits.py` every 5 min, picks the lowest limit for the panel,
  draws the card with one section per provider and ‹ › to flip accounts.
- `package/contents/ui/Bar.qml`: one limit row (label, reset countdown, % or `value`, bar).

## Test
- `HOME=/nonexistent python3 package/contents/code/limits.py --check` is the self-test. It uses canned
  answers, and the fake HOME keeps it from even listing account dirs.
  Add an assert there when you change the parsing.
- A plain `python3 package/contents/code/limits.py` hits the real accounts. Only the main agent may run it,
  never a subagent, and only with the emails stripped:
  `... | python3 -c "import json,sys; d=json.load(sys.stdin); [p.pop('account') for p in d['providers']]; print(d)"`

## Privacy
- The script reads real API keys, OAuth tokens and emails. Subagents only read the repo code: they never
  open `~/.claude*`, `~/.codex*`, `~/.grok`, `~/.t3`, `~/.local/share/opencode` or the cache, and never
  run the script without `--check`. Never print a key or email.

## Cache
- `~/.cache/leftovers.json` keeps the last good numbers per `Provider:dir`. When a fetch fails, the
  widget shows those with `error` set ("last known numbers · …"). Codex keeps the last ticket count.

## Data shapes (checked 2026-11-07)
- Codex `wham/usage`: `rate_limit.primary_window` / `secondary_window` hold `used_percent`,
  `limit_window_seconds` and `reset_at` (epoch seconds). The Go plan has a single 30-day window
  (shown as "Monthly"), and `secondary_window` is null.
- Codex session logs (`sessions/YYYY/MM/DD/rollout-*.jsonl`): `payload.rate_limits.primary` /
  `secondary` with `window_minutes`, `used_percent` and `resets_at` (epoch seconds). Both can be null
  on some lines, so the fallback keeps scanning back.
- T3 `~/.t3/userdata/settings.json`: `providerInstances` is a dict, and some instances have no
  `config` key at all.
- Grok `cli-chat-proxy.grok.com/v1/billing?format=credits` (checked 2026-11-08): `config.creditUsagePercent`
  plus `currentPeriod` {type, end}. After the plan lapsed, `creditUsagePercent` is gone (only
  `currentPeriod`, `prepaidBalance`, billing dates): `grok_limits` returns [] and the widget says "no active plan".

## Errors
- A provider that returns no limits gets `error: "no active plan"` and its cache becomes [] (no stale bar).
- `why()` turns a KeyError into "<Provider> changed its usage page (no <key>)" for every provider.

## Git
- GitHub commits must be by `96988330+SusieeTheLinuxUser@users.noreply.github.com`: check
  `git log --format=%ae origin/main..HEAD` before pushing. The owner merges PRs.
