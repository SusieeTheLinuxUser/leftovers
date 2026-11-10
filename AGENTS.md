# AGENTS.md

Handoff notes for coding agents. User-facing docs are in README.md.

## Files
- `src/main.rs` (Rust, one file; `Cargo.toml` at the root): the helper binary `leftovers`. Reads each CLI's
  saved login, calls the usage APIs, prints JSON
  (`{"providers": [{name, account, limits: [{name, used, resets, value?, extra?}], tickets?, error?}]}`).
  Never rotates saved logins (Antigravity only mints a short-lived Google access token).
  `--skip Name,Name` skips providers entirely (main.qml passes the ones hidden in Configure…).
  Ported from `limits.py` on 2026-11-08 (same JSON, same cache; compared provider by provider on real accounts).
- `package/contents/bin/leftovers`: where main.qml runs the helper from. Not in git: CI or `cargo build` puts it there.
- `.github/workflows/build.yml`: clippy, tests, static musl build, then `leftovers-<version>.plasmoid` as the
  `plasmoid` artifact of every run (and a GitHub release asset on `v*` tags). That file is what goes to the KDE Store.
- `package/contents/config/main.xml` + `package/contents/config/config.qml`: the `hiddenProviders`
  setting (KConfigXT) and the Configure… page list.
- `package/contents/ui/configGeneral.qml`: one checkbox per provider; unticked ones are not fetched or shown.
- `package/contents/ui/main.qml`: runs `bin/leftovers` every 5 min (shows a clear message if it can't run), picks the lowest limit for the panel,
  draws the card with one section per provider and ‹ › to flip accounts.
- `package/contents/ui/Bar.qml`: one limit row (label, reset countdown, % or `value`, bar).

## Test
- `CARGO_BUILD_JOBS=2 nice -n 19 cargo clippy --all-targets -- -D warnings && cargo test`: the parsers on
  synthetic fixtures (no real accounts). Add a test when you change the parsing.
- `env -u CLAUDE_CONFIG_DIR -u CODEX_HOME HOME=/nonexistent target/release/leftovers` must print `{"providers":[]}`.
  Unset those two: T3 sets `CLAUDE_CONFIG_DIR` in agent shells, and `HOME=/nonexistent` alone still reads that account.
- A plain `target/release/leftovers` hits the real accounts. Only the main agent may run it,
  never a subagent, and only with the emails stripped:
  `... | jq -c '.providers[] |= del(.account)'`

## Privacy
- The helper reads real API keys, OAuth tokens and emails. Subagents only read the repo code: they never
  open `~/.claude*`, `~/.codex*`, `~/.grok`, `~/.t3`, `~/.local/share/opencode` or the cache, and never
  run the helper without the `env -u … HOME=/nonexistent` from **Test**. Never print a key or email.

## Packaging (decided 2026-11-08)
- A `.plasmoid` can't compile anything, and store users are on every distro. Options weighed:
  (a) a static x86_64 musl binary inside the package, built by CI: installs like before, no Python, no
  compiler, any x86_64 distro; other CPUs get a clear message in the card. **Picked.**
  (b) binary on GitHub releases fetched by an install.sh: store installs never run install.sh, so broken for them.
  (c) binary + limits.py fallback: two implementations to keep in sync, and still needs Python.
- The executable bit survives `kpackagetool6`/store installs (tested). main.qml appends ` || exit` so the
  command goes through a shell: run directly, a wrong-CPU binary fails without any newData (tested, 0/5).
- Add an aarch64 build to CI when someone asks for it.
- Locally there's no musl target (and agents don't add toolchain targets): build with plain `cargo build --release`.

## Cache
- `~/.cache/leftovers.json` (0600, written via temp file + rename) keeps the last good numbers per `Provider:dir`. When a fetch fails, the
  widget shows those with `error` set ("last known numbers · …"). Codex keeps the last ticket count.

## Data shapes (checked 2026-11-07)
- Claude `api/oauth/usage` (checked 2026-11-10): the fixed keys `five_hour`, `seven_day`, `seven_day_opus`,
  `seven_day_sonnet` hold `utilization` + `resets_at`; ~20 more codenamed keys (`omelette_promotional`,
  `promo_credits`, …) were all null. A `limits` list carries every window with a display name:
  `{kind, group, percent, resets_at, scope: {model: {display_name}, surface} | null}`. During the Fable launch it
  had `session`, `weekly_all` and a `weekly_scoped` one for model "Fable". `claude_parse` shows every entry that
  isn't `session`/`weekly_all` (or a name already shown) as "<Group> <display name>" (e.g. "Weekly Fable"), or the
  kind in words when there's no scope, with `extra: true`; main.qml leaves `extra` limits out of the panel's pick
  (one model's bucket running out leaves the others usable). Promo gone from `limits` = line gone.
  The launch's free reset sits in `cedar_ember.grants` (label "New model launch: one Fable reset…") and is
  already counted as 🎟.
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
  `currentPeriod`, `prepaidBalance`, billing dates): `grok_limits()` returns [] and the widget says "no active plan".

## Errors
- A provider that returns no limits gets `error: "no active plan"` and its cache becomes [] (no stale bar).
- `why()` turns a missing JSON key (`Err::Missing`) into "<Provider> changed its usage page (no <key>)" for every provider. `Err::NotFound` (no login file) hides that account.

## Git
- GitHub commits must be by `96988330+SusieeTheLinuxUser@users.noreply.github.com`: check
  `git log --format=%ae origin/main..HEAD` before pushing. The owner merges PRs.
