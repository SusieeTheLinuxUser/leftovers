# Leftovers

A KDE Plasma 6 widget that shows how much of your AI coding plans you have left, with reset timers.

![screenshot](screenshot.png)

| Provider | Reads login from | Limits |
|---|---|---|
| Claude | `~/.claude/.credentials.json` | Session, Weekly, Weekly Opus/Sonnet |
| Codex | `~/.codex/auth.json` (falls back to session logs) | Session, Weekly / Monthly |
| Cursor | `~/.config/cursor/auth.json` (`cursor-agent login`) | Auto, API |
| Kimi Code | `~/.kimi-code/credentials/kimi-code.json` | Session, Weekly, Monthly |

**Multiple accounts:** every config dir is an account. Anything matching `~/.claude*/`, `~/.codex*/` or `~/.kimi*/`
is picked up automatically, so `CLAUDE_CONFIG_DIR=~/.claude_work claude` or `CODEX_HOME=~/.codex-work codex`
just show up as extra sections.

The panel shows the limit closest to running out. Nothing is ever refreshed or written to your CLI logins:
if a token expired, open that CLI once and the widget catches up on its next poll (every 5 minutes).

## Install

```sh
git clone https://github.com/SusieeTheLinuxUser/leftovers
kpackagetool6 -t Plasma/Applet -i leftovers/package   # -u to upgrade
```

Then add **Leftovers** to a panel or the desktop. Needs `python3`, no other dependencies.

Debug the data side with `python3 package/contents/code/limits.py`.

## License

MIT
