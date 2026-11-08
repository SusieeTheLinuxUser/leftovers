//! Prints AI coding plan usage limits as JSON, using the logins their CLIs already saved.
//! Ported from the old limits.py (same JSON on stdout, same ~/.cache/leftovers.json).
//!
//! Every account is a config dir: ~/.claude, ~/.claude_work (CLAUDE_CONFIG_DIR), ~/.codex, ~/.codex-2 (CODEX_HOME)...
//! Tokens are never refreshed here: that could race the CLI's own token rotation and log you out.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use chrono::{DateTime, Datelike, NaiveDate, NaiveDateTime};
use serde_json::{json, Map, Value};
use std::cmp::Ordering;
use std::collections::HashSet;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use std::{env, fs, io};

/// Python's FileNotFoundError / KeyError / any other exception.
#[derive(Debug, PartialEq)]
enum Err {
    NotFound,
    Missing(String),
    Other(String),
}

impl From<io::Error> for Err {
    fn from(e: io::Error) -> Self {
        if e.kind() == io::ErrorKind::NotFound {
            Err::NotFound
        } else {
            Err::Other(e.to_string())
        }
    }
}
impl From<serde_json::Error> for Err {
    fn from(e: serde_json::Error) -> Self {
        Err::Other(e.to_string())
    }
}
impl From<rusqlite::Error> for Err {
    fn from(e: rusqlite::Error) -> Self {
        Err::Other(e.to_string())
    }
}

fn other<T>(msg: impl Into<String>) -> Result<T, Err> {
    Err(Err::Other(msg.into()))
}

/// What a provider returns: account, limits, and the free-reset count (None = unknown, keep the cached one).
type Fetch = Result<(String, Vec<Value>, Option<i64>), Err>;

// ---------- small helpers ----------

fn home() -> &'static str {
    static H: OnceLock<String> = OnceLock::new();
    H.get_or_init(|| {
        env::var("HOME").ok().filter(|h| !h.is_empty()).unwrap_or_else(|| {
            #[allow(deprecated)]
            env::home_dir().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default()
        })
    })
}

fn now() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64())
}

/// `data[k]`: a missing key is Python's KeyError.
fn key<'a>(v: &'a Value, k: &str) -> Result<&'a Value, Err> {
    v.get(k).ok_or_else(|| Err::Missing(k.to_string()))
}

fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64() != Some(0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

fn num(v: &Value) -> Result<f64, Err> {
    v.as_f64().map_or_else(|| other(format!("expected a number, got {v}")), Ok)
}

fn text(v: &Value) -> Result<&str, Err> {
    v.as_str().map_or_else(|| other(format!("expected a string, got {v}")), Ok)
}

/// A string field or "" (Python's `.get("x", "")`).
fn str_of(v: Option<&Value>) -> String {
    v.and_then(Value::as_str).unwrap_or_default().to_string()
}

fn to_i64(v: &Value) -> i64 {
    v.as_i64().or_else(|| v.as_f64().map(|f| f as i64)).unwrap_or(0)
}

fn read_json(path: &str) -> Result<Value, Err> {
    Ok(serde_json::from_str(&fs::read_to_string(path)?)?)
}

/// Python's round(x, 1): round the exact decimal value, ties to even.
fn round1(x: f64) -> f64 {
    format!("{x:.1}").parse().unwrap_or(x)
}

/// Python's format(x, "g").
fn fmt_g(x: f64) -> String {
    let sci = format!("{x:.5e}");
    let (mantissa, exp) = sci.split_once('e').unwrap_or((&sci, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let strip = |s: &str| if s.contains('.') { s.trim_end_matches('0').trim_end_matches('.').to_string() } else { s.to_string() };
    if !(-4..6).contains(&exp) {
        format!("{}e{}{:02}", strip(mantissa), if exp < 0 { '-' } else { '+' }, exp.abs())
    } else {
        strip(&format!("{:.*}", (5 - exp) as usize, x))
    }
}

/// Python's os.path.normpath (lexical only).
fn normpath(p: &str) -> String {
    if p.is_empty() {
        return ".".into();
    }
    let init = if p.starts_with("//") && !p.starts_with("///") {
        2
    } else {
        usize::from(p.starts_with('/'))
    };
    let mut out: Vec<&str> = vec![];
    for c in p.split('/') {
        if c.is_empty() || c == "." {
            continue;
        }
        if c != ".." || (init == 0 && out.is_empty()) || out.last() == Some(&"..") {
            out.push(c);
        } else {
            out.pop();
        }
    }
    let path = format!("{}{}", "/".repeat(init), out.join("/"));
    if path.is_empty() {
        ".".into()
    } else {
        path
    }
}

fn expanduser(p: &str) -> String {
    match p.strip_prefix("~/") {
        Some(rest) => format!("{}/{rest}", home()),
        None if p == "~" => home().to_string(),
        None => p.to_string(),
    }
}

// ---------- time ----------

/// ISO string -> epoch seconds. Empty/null -> None.
fn ts(v: Option<&Value>) -> Result<Option<f64>, Err> {
    match v {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if s.is_empty() => Ok(None),
        Some(Value::String(s)) => ts_str(s).map(Some),
        Some(other_v) => other(format!("'{other_v}' is not an ISO string")),
    }
}

fn ts_str(s: &str) -> Result<f64, Err> {
    let dt = DateTime::parse_from_rfc3339(s)
        .map(|d| d.naive_utc())
        // ponytail: Python reads an offset-less time as local time, this reads it as UTC; the APIs always send an offset.
        .or_else(|_| NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%.f"))
        .map_err(|_| Err::Other(format!("Invalid isoformat string: '{s}'")))?;
    // Same maths as Python's datetime.timestamp(): whole microseconds / 1e6.
    let micros = dt.and_utc().timestamp() * 1_000_000 + i64::from(dt.and_utc().timestamp_subsec_micros());
    Ok(micros as f64 / 1e6)
}

/// OpenRouter resets key limits at midnight UTC (Mondays for weekly, the 1st for monthly).
fn next_reset(period: Option<&str>, now: f64) -> Option<f64> {
    let period = period?;
    let today = DateTime::from_timestamp(now.floor() as i64, 0)?.date_naive();
    let date = match period {
        "day" => today.succ_opt()?,
        "week" => today + chrono::Days::new(u64::from(7 - today.weekday().num_days_from_monday())),
        _ => {
            let (y, m) = if today.month() == 12 { (today.year() + 1, 1) } else { (today.year(), today.month() + 1) };
            NaiveDate::from_ymd_opt(y, m, 1)?
        }
    };
    Some(date.and_hms_opt(0, 0, 0)?.and_utc().timestamp() as f64)
}

fn fresh(expires: f64, cli: &str) -> Result<(), Err> {
    if expires < now() {
        return other(format!("login expired, open {cli} once to refresh it"));
    }
    Ok(())
}

// ---------- HTTP ----------

fn agent() -> &'static ureq::Agent {
    static A: OnceLock<ureq::Agent> = OnceLock::new();
    A.get_or_init(|| ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(15))).build().into())
}

/// GET (or POST with a body) and return the response text.
fn request(url: &str, headers: &[(&str, &str)], body: Option<&str>) -> Result<String, ureq::Error> {
    let a = agent();
    let mut res = match body {
        None => {
            let mut r = a.get(url);
            for (k, v) in headers {
                r = r.header(*k, *v);
            }
            r.call()?
        }
        Some(b) => {
            let mut r = a.post(url);
            for (k, v) in headers {
                r = r.header(*k, *v);
            }
            r.send(b)?
        }
    };
    res.body_mut().read_to_string()
}

/// urllib style: `HTTP Error 429: Too Many Requests`.
fn http_err(e: ureq::Error) -> Err {
    match e {
        ureq::Error::StatusCode(c) => {
            let reason = ureq::http::StatusCode::from_u16(c).ok().and_then(|s| s.canonical_reason()).unwrap_or("");
            Err::Other(format!("HTTP Error {c}: {reason}"))
        }
        e => Err::Other(e.to_string()),
    }
}

fn get(url: &str, headers: &[(&str, &str)]) -> Result<Value, Err> {
    Ok(serde_json::from_str(&request(url, headers, None).map_err(http_err)?)?)
}

fn post_json(url: &str, headers: &[(&str, &str)], payload: &Value) -> Result<Value, Err> {
    let body = payload.to_string();
    Ok(serde_json::from_str(&request(url, headers, Some(&body)).map_err(http_err)?)?)
}

/// Python's urllib.parse.quote_plus.
fn quote_plus(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'0'..=b'9' | b'a'..=b'z' | b'A'..=b'Z' | b'_' | b'.' | b'-' | b'~' => (b as char).to_string(),
            b' ' => "+".into(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn jwt(token: &str) -> Result<Value, Err> {
    let Some(part) = token.split('.').nth(1) else {
        return other("list index out of range");
    };
    let bytes = URL_SAFE_NO_PAD.decode(part.trim_end_matches('=')).map_err(|e| Err::Other(e.to_string()))?;
    Ok(serde_json::from_slice(&bytes)?)
}

// ---------- Claude ----------

/// The native install's binary is named after its version; an npm one resolves to cli.js: ask it.
fn claude_version() -> &'static str {
    static V: OnceLock<String> = OnceLock::new();
    V.get_or_init(|| {
        let found = env::var_os("PATH").and_then(|paths| {
            env::split_paths(&paths).map(|d| d.join("claude")).find(|p| {
                use std::os::unix::fs::PermissionsExt;
                fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            })
        });
        let real = found.map_or_else(|| PathBuf::from("x"), |p| fs::canonicalize(&p).unwrap_or(p));
        let mut v = real.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if !v.starts_with(|c: char| c.is_ascii_digit()) {
            if let Some(out) = claude_version_cmd() {
                v = out;
            }
        }
        v
    })
}

/// `claude --version`, first word, 10 s at most.
fn claude_version_cmd() -> Option<String> {
    let mut child = Command::new("claude")
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let start = Instant::now();
    while child.try_wait().ok()?.is_none() {
        if start.elapsed() > Duration::from_secs(10) {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let mut out = String::new();
    child.stdout.take()?.read_to_string(&mut out).ok()?;
    out.split_whitespace().next().map(String::from)
}

fn claude(d: &str) -> Fetch {
    let file = read_json(&format!("{d}/.credentials.json"))?;
    let creds = key(&file, "claudeAiOauth")?;
    // The default dir keeps its account info next to it, CLAUDE_CONFIG_DIR ones keep it inside.
    let info = if d == format!("{}/.claude", home()) { format!("{}/.claude.json", home()) } else { format!("{d}/.claude.json") };
    let email = read_json(&info)
        .ok()
        .and_then(|j| j.get("oauthAccount")?.get("emailAddress")?.as_str().map(String::from))
        .unwrap_or_default();
    fresh(num(key(creds, "expiresAt")?)? / 1000.0, "claude")?;
    let bearer = format!("Bearer {}", text(key(creds, "accessToken")?)?);
    // cedar_ember=1 adds the free limit resets Anthropic grants (same request T3 Code makes).
    let data = get(
        "https://api.anthropic.com/api/oauth/usage?cedar_ember=1",
        // Generic clients get 429 here, and other surfaces aren't eligible for resets: identify as the CLI.
        &[
            ("Authorization", &bearer),
            ("anthropic-beta", "oauth-2025-04-20"),
            ("User-Agent", &format!("claude-cli/{} (external, cli)", claude_version())),
        ],
    )?;
    let (limits, tickets) = claude_parse(&data, now())?;
    Ok((email, limits, Some(tickets)))
}

fn claude_parse(data: &Value, now: f64) -> Result<(Vec<Value>, i64), Err> {
    let grants = data.get("cedar_ember").filter(|v| truthy(v));
    let eligible = grants.and_then(|g| g.get("eligible")).is_some_and(truthy);
    let mut tickets = 0;
    for g in grants.and_then(|g| g.get("grants")).and_then(Value::as_array).into_iter().flatten() {
        if eligible
            && g.get("usable_now").is_some_and(truthy)
            && !g.get("paused").is_some_and(truthy)
            && ts(g.get("ends_at"))?.filter(|t| *t != 0.0).unwrap_or(1e12) > now
        {
            tickets += g.get("resets_left").map_or(0, to_i64);
        }
    }
    let mut limits = vec![];
    for (k, name) in [
        ("five_hour", "Session"),
        ("seven_day", "Weekly"),
        ("seven_day_opus", "Weekly Opus"),
        ("seven_day_sonnet", "Weekly Sonnet"),
    ] {
        if let Some(v) = data.get(k).filter(|v| truthy(v)) {
            limits.push(json!({"name": name, "used": key(v, "utilization")?, "resets": ts(v.get("resets_at"))?}));
        }
    }
    Ok((limits, tickets))
}

// ---------- Codex ----------

fn window_name(minutes: i64) -> String {
    match minutes {
        300 => "Session".into(),
        10080 => "Weekly".into(),
        43200 => "Monthly".into(),
        m => format!("{}h", m.div_euclid(60)),
    }
}

fn codex(d: &str) -> Fetch {
    let auth = read_json(&format!("{d}/auth.json"))?;
    // API key login: no ChatGPT plan to show
    let Some(tokens) = auth.get("tokens").filter(|t| truthy(t)) else {
        return Err(Err::NotFound);
    };
    let email = str_of(jwt(text(key(tokens, "id_token")?)?)?.get("email"));
    match codex_live(tokens) {
        Ok((limits, tickets)) => Ok((email, limits, Some(tickets))),
        // Expired login or API hiccup: the newest local session log still has the numbers from your last run.
        Err(e) => match codex_logs(d)? {
            Some(limits) => Ok((email, limits, None)),
            None => Err(e),
        },
    }
}

fn codex_live(tokens: &Value) -> Result<(Vec<Value>, i64), Err> {
    let access = text(key(tokens, "access_token")?)?;
    fresh(num(key(&jwt(access)?, "exp")?)?, "codex")?;
    let data = get(
        "https://chatgpt.com/backend-api/wham/usage",
        &[
            ("Authorization", &format!("Bearer {access}")),
            ("ChatGPT-Account-Id", text(key(tokens, "account_id")?)?),
            ("User-Agent", "codex-cli"),
        ],
    )?;
    codex_wham(&data)
}

fn codex_wham(data: &Value) -> Result<(Vec<Value>, i64), Err> {
    let rl = key(data, "rate_limit")?;
    let mut limits = vec![];
    for w in [rl.get("primary_window"), rl.get("secondary_window")].into_iter().flatten().filter(|w| truthy(w)) {
        let minutes = (num(key(w, "limit_window_seconds")?)? / 60.0).floor() as i64;
        limits.push(json!({
            "name": window_name(minutes),
            "used": key(w, "used_percent")?,
            "resets": w.get("reset_at").unwrap_or(&Value::Null),
        }));
    }
    // Free limit resets ChatGPT hands out, spendable from the Codex app.
    let tickets = data
        .get("rate_limit_reset_credits")
        .filter(|c| truthy(c))
        .and_then(|c| c.get("available_count"))
        .map_or(0, to_i64);
    Ok((limits, tickets))
}

/// Non-hidden sub directories of `dir`, sorted by name.
fn subdirs(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| !e.file_name().to_string_lossy().starts_with('.') && e.path().is_dir())
        .map(|e| e.path())
        .collect();
    v.sort();
    v
}

/// The newest session log with rate limits in it (sessions/Y/M/D/rollout-*.jsonl), newest line first.
fn codex_logs(d: &str) -> Result<Option<Vec<Value>>, Err> {
    let mut files: Vec<(SystemTime, PathBuf)> = vec![];
    for day in subdirs(&Path::new(d).join("sessions")).iter().flat_map(|y| subdirs(y)).flat_map(|m| subdirs(&m)) {
        for e in fs::read_dir(day).into_iter().flatten().flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with("rollout-") && name.ends_with(".jsonl") {
                if let Ok(m) = e.metadata() {
                    if m.is_file() {
                        files.push((m.modified().unwrap_or(UNIX_EPOCH), e.path()));
                    }
                }
            }
        }
    }
    files.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    for (_, path) in files {
        let Ok(bytes) = fs::read(&path) else { continue };
        for line in String::from_utf8_lossy(&bytes).lines().rev() {
            if let Some(out) = codex_log_line(line)? {
                return Ok(Some(out));
            }
        }
    }
    Ok(None)
}

fn codex_log_line(line: &str) -> Result<Option<Vec<Value>>, Err> {
    if !line.contains("\"rate_limits\"") {
        return Ok(None);
    }
    // codex may still be writing the last line
    let Ok(v) = serde_json::from_str::<Value>(line) else { return Ok(None) };
    let Some(rl) = v.get("payload").and_then(|p| p.get("rate_limits")).and_then(Value::as_object) else {
        return Ok(None);
    };
    let mut out = vec![];
    for w in rl.values() {
        let Some(minutes) = w.as_object().and_then(|w| w.get("window_minutes")).filter(|m| truthy(m)) else { continue };
        out.push(json!({
            "name": window_name(to_i64(minutes)),
            "used": key(w, "used_percent")?,
            "resets": w.get("resets_at").unwrap_or(&Value::Null),
        }));
    }
    Ok(Some(out).filter(|o| !o.is_empty()))
}

// ---------- Grok ----------

fn grok(d: &str) -> Fetch {
    // One entry per login; the most recently refreshed one is the account grok is using.
    let file = read_json(&format!("{d}/auth.json"))?;
    let mut best: Option<&Value> = None;
    for a in file.as_object().into_iter().flat_map(Map::values) {
        let expires = key(a, "expires_at")?;
        if best.is_none_or(|b| cmp_json(expires, &b["expires_at"]) == Ordering::Greater) {
            best = Some(a);
        }
    }
    let Some(auth) = best else { return other("max() arg is an empty sequence") };
    let Some(expires) = ts(Some(&auth["expires_at"]))? else {
        return other("'<' not supported between instances of 'NoneType' and 'float'");
    };
    fresh(expires, "grok")?;
    let bearer = format!("Bearer {}", text(key(auth, "key")?)?);
    let data = get("https://cli-chat-proxy.grok.com/v1/billing?format=credits", &[("Authorization", &bearer)])?;
    Ok((str_of(auth.get("email")), grok_limits(key(&data, "config")?)?, None))
}

fn cmp_json(a: &Value, b: &Value) -> Ordering {
    match (a, b) {
        (Value::String(a), Value::String(b)) => a.cmp(b),
        _ => a.as_f64().partial_cmp(&b.as_f64()).unwrap_or(Ordering::Equal),
    }
}

fn grok_limits(data: &Value) -> Result<Vec<Value>, Err> {
    // A lapsed plan still answers, but with no creditUsagePercent: nothing to show.
    let Some(used) = data.get("creditUsagePercent") else { return Ok(vec![]) };
    let period = data.get("currentPeriod").filter(|p| truthy(p));
    let weekly = period.and_then(|p| p.get("type")).and_then(Value::as_str).is_some_and(|t| t.contains("WEEKLY"));
    Ok(vec![json!({
        "name": if weekly { "Weekly" } else { "Credits" },
        "used": used,
        "resets": ts(period.and_then(|p| p.get("end")))?,
    })])
}

// ---------- Antigravity ----------

fn antigravity(d: &str) -> Fetch {
    // Google never rotates refresh tokens, so minting an access token here can't log Antigravity out.
    let creds = read_json(&format!("{d}/acp_token.json"))?;
    let field = |k: &str| -> Result<String, Err> { Ok(quote_plus(text(key(&creds, k)?)?)) };
    let body = format!(
        "client_id={}&client_secret={}&refresh_token={}&grant_type=refresh_token",
        field("client_id")?,
        field("client_secret")?,
        field("refresh_token")?
    );
    let login = match request(
        text(key(&creds, "token_uri")?)?,
        &[("Content-Type", "application/x-www-form-urlencoded")],
        Some(&body),
    ) {
        Err(ureq::Error::StatusCode(_)) => return other("login expired, open Antigravity once to refresh it"),
        r => serde_json::from_str::<Value>(&r.map_err(http_err)?)?,
    };
    let bearer = format!("Bearer {}", text(key(&login, "access_token")?)?);
    let call = |method: &str, payload: Value| {
        post_json(
            &format!("https://cloudcode-pa.googleapis.com/v1internal:{method}"),
            &[("Authorization", &bearer), ("Content-Type", "application/json"), ("User-Agent", "antigravity")],
            &payload,
        )
    };
    let loaded = call("loadCodeAssist", json!({"metadata": {"ideType": "ANTIGRAVITY"}}))?;
    let project = loaded.get("cloudaicompanionProject").filter(|p| truthy(p)).or(creds.get("project_id"));
    let models = call("fetchAvailableModels", json!({"project": project}))?;
    let email = str_of(jwt(text(key(&login, "id_token")?)?)?.get("email"));
    Ok((email, antigravity_limits(key(&models, "models")?)?, None))
}

/// Gemini and the other vendors' models (Claude, GPT-OSS) have separate quotas: one bar each, from the
/// model with the least left. A model with no remainingFraction has none left.
fn antigravity_limits(models: &Value) -> Result<Vec<Value>, Err> {
    let mut groups: Vec<(&str, Value)> = vec![];
    for (k, m) in models.as_object().into_iter().flatten() {
        let Some(q) = m.get("quotaInfo").filter(|q| truthy(q)) else { continue };
        if !m.get("displayName").is_some_and(truthy) {
            continue; // tab_*/chat_* are completion internals, not models you pick
        }
        let name = if k.starts_with("gemini") { "Gemini" } else { "Claude & GPT" };
        // Like Python: an int fraction (or none) gives an int, a float one a rounded float.
        let (used, shown) = match q.get("remainingFraction") {
            None => (100.0, json!(100)),
            Some(r) if r.is_i64() || r.is_u64() => {
                let u = 100 * (1 - to_i64(r));
                (u as f64, json!(u))
            }
            Some(r) => {
                let u = 100.0 * (1.0 - num(r)?);
                (u, json!(round1(u)))
            }
        };
        let slot = groups.iter().position(|g| g.0 == name);
        // The comparison is against the stored (rounded) value, as in the Python.
        if slot.is_none_or(|i| used > groups[i].1["used"].as_f64().unwrap_or(0.0)) {
            let group = (name, json!({"name": name, "used": shown, "resets": ts(q.get("resetTime"))?}));
            match slot {
                Some(i) => groups[i] = group,
                None => groups.push(group),
            }
        }
    }
    Ok(["Gemini", "Claude & GPT"]
        .iter()
        .filter_map(|n| groups.iter().find(|g| g.0 == *n).map(|g| g.1.clone()))
        .collect())
}

// ---------- OpenRouter ----------

/// opencode keeps the key it uses in its database (its auth.json copy can be an old dead one).
fn openrouter_key() -> Result<String, Err> {
    if let Some(k) = env::var("OPENROUTER_API_KEY").ok().filter(|k| !k.is_empty()) {
        return Ok(k);
    }
    let db = format!("{}/.local/share/opencode/opencode.db", home());
    if !Path::new(&db).exists() {
        return Err(Err::NotFound);
    }
    use rusqlite::OptionalExtension;
    let conn = rusqlite::Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let row: Option<String> = conn
        .query_row(
            "SELECT value FROM credential WHERE integration_id = 'openrouter' AND active ORDER BY time_updated DESC",
            [],
            |r| {
                r.get::<_, String>(0)
                    .or_else(|_| r.get::<_, Vec<u8>>(0).map(|b| String::from_utf8_lossy(&b).into_owned()))
            },
        )
        .optional()?;
    let Some(row) = row else { return Err(Err::NotFound) }; // no OpenRouter key in opencode
    Ok(text(key(&serde_json::from_str(&row)?, "key")?)?.to_string())
}

fn openrouter(_: &str) -> Fetch {
    let bearer = format!("Bearer {}", openrouter_key()?);
    let headers = [("Authorization", bearer.as_str())];
    let credits = get("https://openrouter.ai/api/v1/credits", &headers)?;
    let info = get("https://openrouter.ai/api/v1/key", &headers)?;
    Ok((String::new(), openrouter_limits(key(&credits, "data")?, key(&info, "data")?, now())?, None))
}

/// Money, not a plan: "used" still drives the bar, "value" is what the label shows instead of a percent.
fn openrouter_limits(credits: &Value, info: &Value, now: f64) -> Result<Vec<Value>, Err> {
    let bar = |name: &str, total: f64, spent: f64, value: String, resets: Option<f64>| {
        let used = if total != 0.0 { json!(round1(100.0 * spent / total)) } else { json!(100) };
        json!({"name": name, "used": used, "resets": resets, "value": value})
    };
    let total = num(key(credits, "total_credits")?)?;
    let spent = num(key(credits, "total_usage")?)?;
    let left = if total - spent > 0.0 { total - spent } else { 0.0 };
    let mut out = vec![bar("Credits", total, spent, format!("${left:.2} left"), None)];
    // daily / weekly / monthly / None
    let period = match info.get("limit_reset").and_then(Value::as_str) {
        Some("daily") => Some("day"),
        Some("weekly") => Some("week"),
        Some("monthly") => Some("month"),
        _ => None,
    };
    if info.get("limit").is_some_and(truthy) {
        let limit = num(&info["limit"])?;
        let used = limit - num(key(info, "limit_remaining")?)?; // what OpenRouter enforces, BYOK included when it counts
        let name = period.map_or("Key limit".to_string(), |p| format!("Key this {p}"));
        out.push(bar(&name, limit, used, format!("${used:.2}/${}", fmt_g(limit)), next_reset(period, now)));
    }
    Ok(out)
}

// ---------- accounts ----------

/// Account dirs set up as provider instances in T3 Code (Settings > Providers).
fn t3(driver: &str, keys: &[&str]) -> Vec<String> {
    let Ok(settings) = read_json(&format!("{}/.t3/userdata/settings.json", home())) else { return vec![] };
    let Some(instances) = settings.get("providerInstances").and_then(Value::as_object) else { return vec![] };
    instances
        .values()
        .filter(|i| i.get("driver").and_then(Value::as_str) == Some(driver))
        .flat_map(|i| {
            keys.iter().filter_map(move |k| {
                i.get("config")?.get(*k)?.as_str().filter(|s| !s.is_empty()).map(expanduser)
            })
        })
        .collect()
}

/// Python glob `<parent>/<prefix>?*/`: directories named <prefix> plus at least one more character.
fn dirs_with_prefix(parent: &str, prefix: &str) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(parent)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(prefix) && n.len() > prefix.len() && Path::new(parent).join(n).is_dir())
        .collect();
    v.sort();
    v.into_iter().map(|n| format!("{parent}/{n}")).collect()
}

/// Python glob `<parent>/*/<sub>/`.
fn star_sub(parent: &str, sub: &str) -> Vec<String> {
    subdirs(Path::new(parent))
        .into_iter()
        .map(|p| p.join(sub))
        .filter(|p| p.is_dir())
        .map(|p| p.to_string_lossy().into_owned())
        .collect()
}

fn env_dir(name: &str) -> String {
    env::var(name).unwrap_or_default()
}

fn claude_dirs() -> Vec<String> {
    let h = home();
    let mut v = vec![env_dir("CLAUDE_CONFIG_DIR"), format!("{h}/.claude")];
    v.extend(dirs_with_prefix(h, ".claude"));
    v.extend(t3("claudeAgent", &["homePath"]));
    v
}

fn codex_dirs() -> Vec<String> {
    let h = home();
    let mut v = vec![env_dir("CODEX_HOME"), format!("{h}/.codex")];
    v.extend(dirs_with_prefix(h, ".codex"));
    v.extend(t3("codex", &["homePath", "shadowHomePath"]));
    // ChatGPT accounts added with T3 Code's own login button
    v.extend(star_sub(&format!("{h}/.t3/userdata/providers/codex"), "shadow"));
    v
}

fn grok_dirs() -> Vec<String> {
    vec![format!("{}/.grok", home())]
}

/// T3 Code's Antigravity provider keeps its Google login here.
fn antigravity_dirs() -> Vec<String> {
    star_sub(&format!("{}/.t3/userdata/providers/antigravity", home()), "antigravity-acp")
}

fn openrouter_dirs() -> Vec<String> {
    vec!["openrouter".into()] // one account, found by openrouter_key()
}

/// Short error text for the widget; a bare missing key would just print the key.
fn why(name: &str, e: &Err) -> String {
    match e {
        Err::Missing(k) => format!("{name} changed its usage page (no {k})"),
        Err::Other(m) => m.clone(),
        Err::NotFound => "No such file or directory".into(),
    }
}

/// Provider names after a --skip arg (comma separated); their logins and APIs are never touched.
fn skipped(argv: &[String]) -> HashSet<String> {
    for (i, a) in argv.iter().enumerate() {
        if a == "--skip" && i + 1 < argv.len() {
            return argv[i + 1].split(',').filter(|s| !s.is_empty()).map(String::from).collect();
        }
    }
    HashSet::new()
}

fn main() {
    type Provider = (&'static str, fn(&str) -> Fetch, fn() -> Vec<String>);
    let providers: [Provider; 5] = [
        ("Claude", claude, claude_dirs),
        ("Codex", codex, codex_dirs),
        ("Grok", grok, grok_dirs),
        ("Antigravity", antigravity, antigravity_dirs),
        ("OpenRouter", openrouter, openrouter_dirs),
    ];
    let cache_path = format!("{}/.cache/leftovers.json", home());
    let mut cache: Map<String, Value> =
        fs::read_to_string(&cache_path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
    let skip = skipped(&env::args().collect::<Vec<_>>());
    let mut result = vec![];
    for (name, fetch, dirs) in providers {
        if skip.contains(name) {
            continue; // unticked in the widget's Configure…: not even its login gets read
        }
        let mut accounts: Vec<String> = vec![];
        for d in dirs().iter().filter(|d| !d.is_empty()).map(|d| normpath(d)) {
            if !accounts.contains(&d) {
                accounts.push(d); // dedupe, keep order
            }
        }
        for d in accounts {
            let cache_key = format!("{name}:{d}");
            let mut provider = Map::new();
            provider.insert("name".into(), name.into());
            match fetch(&d) {
                Ok((account, limits, tickets)) => {
                    // The Codex session-log fallback has no count: keep the last one rather than hide it.
                    let tickets = tickets.map_or_else(
                        || cache.get(&cache_key).and_then(|c| c.get("tickets")).cloned().unwrap_or(json!(0)),
                        Value::from,
                    );
                    let no_plan = limits.is_empty();
                    let entry = json!({"account": account, "limits": limits, "tickets": tickets});
                    provider.extend(entry.as_object().cloned().unwrap_or_default());
                    // No limits at all (e.g. the plan lapsed): say so instead of an empty card. Cache keeps [] so
                    // an old bar never comes back as "last known numbers".
                    if no_plan {
                        provider.insert("error".into(), "no active plan".into());
                    }
                    cache.insert(cache_key, entry);
                }
                Err(Err::NotFound) => continue, // not installed / not an account dir: just don't show it
                Err(e) => {
                    // Usage endpoints rate-limit (429); last good numbers still count down correctly.
                    let last = cache.get(&cache_key).and_then(Value::as_object).cloned();
                    provider.extend(last.unwrap_or_else(|| {
                        json!({"account": "", "limits": []}).as_object().cloned().unwrap_or_default()
                    }));
                    provider.insert("error".into(), why(name, &e).into());
                }
            }
            result.push(Value::Object(provider));
        }
    }
    // Write then rename: a second widget reading mid-write would see half a file and drop the whole cache.
    // Best effort (the Python crashed here when the cache dir couldn't be made).
    let _ = (|| -> io::Result<()> {
        if let Some(dir) = Path::new(&cache_path).parent() {
            fs::create_dir_all(dir)?;
        }
        let tmp = format!("{cache_path}.{}", std::process::id());
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        // 0600: it holds account emails.
        fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)?
            .write_all(serde_json::to_string(&cache)?.as_bytes())?;
        fs::rename(tmp, &cache_path)
    })();
    println!("{}", json!({"providers": result}));
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    fn t(s: &str) -> f64 {
        ts_str(s).unwrap()
    }
    fn utc(y: i32, m: u32, d: u32, h: u32, min: u32) -> f64 {
        Utc.with_ymd_and_hms(y, m, d, h, min, 0).unwrap().timestamp() as f64
    }
    fn tmp(tag: &str) -> PathBuf {
        let p = env::temp_dir().join(format!("leftovers-test-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    // ---- ports of selfcheck() ----

    #[test]
    fn antigravity() {
        let models = json!({
            "gemini-3-flash": {"displayName": "Gemini 3 Flash", "quotaInfo": {"resetTime": "2026-11-09T22:29:34Z"}},
            "gemini-2.5-pro": {"displayName": "Gemini 2.5 Pro", "quotaInfo": {"remainingFraction": 0.5}},
            "claude-opus-4-6-thinking": {"displayName": "Claude Opus", "quotaInfo": {"remainingFraction": 0.75}},
            "tab_flash_lite_preview": {"quotaInfo": {"remainingFraction": 0}}
        });
        // Python compares 100 == 100.0; here a missing fraction stays an int like in Python's JSON output.
        assert_eq!(
            antigravity_limits(&models).unwrap(),
            vec![
                json!({"name": "Gemini", "used": 100, "resets": t("2026-11-09T22:29:34Z")}),
                json!({"name": "Claude & GPT", "used": 25.0, "resets": null}),
            ]
        );
    }

    #[test]
    fn resets() {
        let now = utc(2026, 11, 6, 18, 30); // a Friday
        assert_eq!(next_reset(Some("day"), now), Some(utc(2026, 11, 7, 0, 0)));
        assert_eq!(next_reset(Some("week"), now), Some(utc(2026, 11, 9, 0, 0)));
        assert_eq!(next_reset(Some("month"), utc(2026, 12, 31, 0, 0)), Some(utc(2027, 1, 1, 0, 0)));
        assert_eq!(next_reset(None, now), None);
        assert_eq!(next_reset(Some("week"), utc(2026, 11, 9, 5, 0)), Some(utc(2026, 11, 16, 0, 0))); // a Monday
    }

    #[test]
    fn openrouter() {
        let now = utc(2026, 11, 6, 18, 30);
        let credits = json!({"total_credits": 20, "total_usage": 4.9});
        let mut info = json!({"limit": 20, "limit_reset": "monthly", "limit_remaining": 18.5, "usage": 4.9, "usage_monthly": 1.5});
        assert_eq!(
            openrouter_limits(&credits, &info, now).unwrap(),
            vec![
                json!({"name": "Credits", "used": 24.5, "resets": null, "value": "$15.10 left"}),
                json!({"name": "Key this month", "used": 7.5, "resets": next_reset(Some("month"), now), "value": "$1.50/$20"}),
            ]
        );
        info["include_byok_in_limit"] = json!(true);
        info["byok_usage_monthly"] = json!(1);
        info["limit_remaining"] = json!(17.5);
        assert_eq!(openrouter_limits(&credits, &info, now).unwrap()[1]["value"], "$2.50/$20");
        assert_eq!(
            openrouter_limits(&json!({"total_credits": 0, "total_usage": 0}), &json!({"limit": null, "usage": 0}), now).unwrap(),
            vec![json!({"name": "Credits", "used": 100, "resets": null, "value": "$0.00 left"})]
        );
        // a limit with no limit_remaining is a changed API, not a crash
        assert_eq!(
            openrouter_limits(&credits, &json!({"limit": 5}), now),
            Err(Err::Missing("limit_remaining".into()))
        );
    }

    #[test]
    fn grok() {
        let lapsed = json!({"currentPeriod": {"type": "USAGE_PERIOD_TYPE_WEEKLY", "end": "2026-11-14T18:59:33.334+00:00"},
                            "prepaidBalance": {"val": 0}}); // real shape after the plan ended, 2026-11-08
        assert_eq!(grok_limits(&lapsed).unwrap(), Vec::<Value>::new());
        let mut live = lapsed.clone();
        live["creditUsagePercent"] = json!(40.0);
        assert_eq!(
            grok_limits(&live).unwrap(),
            vec![json!({"name": "Weekly", "used": 40.0, "resets": t("2026-11-14T18:59:33.334+00:00")})]
        );
        live["currentPeriod"]["type"] = json!("USAGE_PERIOD_TYPE_MONTHLY");
        assert_eq!(grok_limits(&live).unwrap()[0]["name"], "Credits");
    }

    #[test]
    fn errors_and_args() {
        assert_eq!(why("Grok", &Err::Missing("creditUsagePercent".into())), "Grok changed its usage page (no creditUsagePercent)");
        assert_eq!(why("Grok", &Err::Missing(String::new())), "Grok changed its usage page (no )");
        assert_eq!(why("Codex", &Err::Other("login expired".into())), "login expired");
        let a = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let set = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<HashSet<_>>();
        assert_eq!(skipped(&a(&["limits.py", "--skip", "Grok,Codex"])), set(&["Grok", "Codex"]));
        assert_eq!(skipped(&a(&["limits.py"])), set(&[]));
        assert_eq!(skipped(&a(&["limits.py", "--skip", ""])), set(&[]));
        assert_eq!(skipped(&a(&["limits.py", "--skip"])), set(&[]));
    }

    #[test]
    fn codex_api_key_login_is_hidden() {
        let d = tmp("apikey");
        let p = d.to_str().unwrap();
        fs::write(d.join("auth.json"), r#"{"OPENAI_API_KEY": "sk-test", "tokens": null}"#).unwrap();
        assert_eq!(codex(p), Err(Err::NotFound));
        fs::write(d.join("auth.json"), r#"{"OPENAI_API_KEY": "sk-test"}"#).unwrap();
        assert_eq!(codex(p), Err(Err::NotFound));
        fs::remove_file(d.join("auth.json")).unwrap();
        assert_eq!(codex(p), Err(Err::NotFound)); // no login file at all
        let _ = fs::remove_dir_all(&d);
    }

    // ---- synthetic fixtures ----

    #[test]
    fn claude_usage() {
        let now = utc(2026, 11, 8, 12, 0);
        let data = json!({
            "five_hour": {"utilization": 12.0, "resets_at": "2026-11-08T17:00:00+00:00"},
            "seven_day": {"utilization": 40, "resets_at": null},
            "seven_day_opus": null,
            "cedar_ember": {"eligible": true, "grants": [
                {"usable_now": true, "paused": false, "resets_left": 2, "ends_at": "2026-12-01T00:00:00Z"},
                {"usable_now": true, "paused": true, "resets_left": 5, "ends_at": "2026-12-01T00:00:00Z"},
                {"usable_now": true, "paused": false, "resets_left": 7, "ends_at": "2026-01-01T00:00:00Z"},
                {"usable_now": false, "paused": false, "resets_left": 9, "ends_at": "2026-12-01T00:00:00Z"},
                {"usable_now": true, "resets_left": 1}
            ]}
        });
        let (limits, tickets) = claude_parse(&data, now).unwrap();
        assert_eq!(tickets, 3); // active + no end date; paused, expired and not-usable ones don't count
        assert_eq!(
            limits,
            vec![
                json!({"name": "Session", "used": 12.0, "resets": t("2026-11-08T17:00:00+00:00")}),
                json!({"name": "Weekly", "used": 40, "resets": null}),
            ]
        );
        let mut off = data.clone();
        off["cedar_ember"]["eligible"] = json!(false);
        assert_eq!(claude_parse(&off, now).unwrap().1, 0);
        // no cedar_ember at all, and the other weekly limits present
        let (limits, tickets) = claude_parse(
            &json!({"seven_day_opus": {"utilization": 1, "resets_at": null}, "seven_day_sonnet": {"utilization": 2}}),
            now,
        )
        .unwrap();
        assert_eq!((limits.len(), tickets), (2, 0));
        assert_eq!(limits[0]["name"], "Weekly Opus");
        assert_eq!(limits[1]["name"], "Weekly Sonnet");
        // a limit without "utilization" is a changed API
        assert_eq!(claude_parse(&json!({"five_hour": {"resets_at": null}}), now), Err(Err::Missing("utilization".into())));
    }

    #[test]
    fn codex_go_plan() {
        let data = json!({
            "rate_limit": {
                "primary_window": {"limit_window_seconds": 2592000, "used_percent": 12.5, "reset_at": 1794000000},
                "secondary_window": null
            },
            "rate_limit_reset_credits": {"available_count": 2}
        });
        assert_eq!(
            codex_wham(&data).unwrap(),
            (vec![json!({"name": "Monthly", "used": 12.5, "resets": 1794000000})], 2)
        );
        // Plus-style: 5 h and 7 d windows, no credits block
        let data = json!({"rate_limit": {
            "primary_window": {"limit_window_seconds": 18000, "used_percent": 3, "reset_at": 10},
            "secondary_window": {"limit_window_seconds": 604800, "used_percent": 4},
        }});
        let (limits, tickets) = codex_wham(&data).unwrap();
        assert_eq!(tickets, 0);
        assert_eq!(limits[0], json!({"name": "Session", "used": 3, "resets": 10}));
        assert_eq!(limits[1], json!({"name": "Weekly", "used": 4, "resets": null}));
        assert_eq!(codex_wham(&json!({})), Err(Err::Missing("rate_limit".into())));
        assert_eq!(window_name(120), "2h");
    }

    #[test]
    fn codex_session_log() {
        let d = tmp("log");
        let day = d.join("sessions/2026/11/08");
        fs::create_dir_all(&day).unwrap();
        let line = |used: f64| {
            format!(r#"{{"payload":{{"rate_limits":{{"primary":{{"window_minutes":43200,"used_percent":{used},"resets_at":1794000000}},"secondary":null}}}}}}"#)
        };
        // oldest line first; a line without limits, one with only null windows, and a half-written last line
        let content = format!(
            "{}\n{{\"payload\":{{\"type\":\"other\"}}}}\n{}\n{}\n{{\"payload\":{{\"rate_limits\":{{\"prim",
            line(10.0),
            line(20.0),
            r#"{"payload":{"rate_limits":{"primary":null,"secondary":null}}}"#
        );
        fs::write(day.join("rollout-a.jsonl"), content).unwrap();
        fs::write(day.join("notes.txt"), "ignored").unwrap();
        let got = codex_logs(d.to_str().unwrap()).unwrap().unwrap();
        assert_eq!(got, vec![json!({"name": "Monthly", "used": 20, "resets": 1794000000})]);
        assert_eq!(codex_log_line(r#"{"payload":{"rate_limits":{"primary":{"window_minutes":300}}}}"#), Err(Err::Missing("used_percent".into())));
        assert_eq!(codex_log_line("not json with \"rate_limits\""), Ok(None));
        assert_eq!(codex_logs("/nonexistent-leftovers-test").unwrap(), None);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn helpers() {
        assert_eq!(normpath("/a//b/../c/"), "/a/c");
        assert_eq!(normpath("/home/u/.claude-2/"), "/home/u/.claude-2");
        assert_eq!(normpath("openrouter"), "openrouter");
        assert_eq!(normpath("//x"), "//x");
        assert_eq!(normpath("///x/./y"), "/x/y");
        assert_eq!(normpath("a/../.."), "..");
        assert_eq!(normpath("/.."), "/");
        assert_eq!(normpath("./"), ".");
        assert_eq!(fmt_g(20.0), "20");
        assert_eq!(fmt_g(20.5), "20.5");
        assert_eq!(fmt_g(1e6), "1e+06");
        assert_eq!(fmt_g(0.0001), "0.0001");
        assert_eq!(fmt_g(0.00001), "1e-05");
        assert_eq!(fmt_g(123456.7), "123457");
        assert_eq!(round1(0.25), 0.2);
        assert_eq!(round1(24.500000000000004), 24.5);
        assert_eq!(t("2026-11-14T18:59:33.334+00:00"), t("2026-11-14T20:59:33.334+02:00"));
        assert_eq!(t("2026-11-09T00:00:00Z"), utc(2026, 11, 9, 0, 0));
        assert_eq!(ts(Some(&json!(""))).unwrap(), None);
        assert!(ts_str("nope").is_err());
        assert_eq!(quote_plus("a b&c~"), "a+b%26c~");
        assert_eq!(jwt("x.eyJlbWFpbCI6InVzZXJAZXhhbXBsZS5jb20ifQ.y").unwrap()["email"], "user@example.com");
        assert_eq!(jwt("nodots"), Err(Err::Other("list index out of range".into())));
        assert!(fresh(1.0, "claude").is_err());
        assert_eq!(fresh(1.0, "claude"), Err(Err::Other("login expired, open claude once to refresh it".into())));
        assert!(fresh(now() + 100.0, "claude").is_ok());
        assert_eq!(
            Err::from(io::Error::from(io::ErrorKind::NotFound)),
            Err::NotFound
        );
    }
}
