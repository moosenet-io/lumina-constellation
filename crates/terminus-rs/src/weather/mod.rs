//! Weather tool — current conditions and forecasts via OpenWeatherMap.
//!
//! One LLM-callable tool:
//!   weather  — current / tomorrow / this-week weather for a location.
//!
//! Location resolution (BUG 1): when `location` is omitted or empty the tool
//! defaults to the operator's home address from the COMMUTE_HOME env var (the
//! same source of truth the commute tools use). This is what stops the model
//! from asking "which city?" — the description advertises the default and the
//! code injects it deterministically. If COMMUTE_HOME is also unset and no
//! location was given, a clear NotConfigured error is returned rather than a
//! silent failure.
//!
//! FUTURE ENHANCEMENT: an Engram "where does {user} live" lookup could be a
//! further fallback when COMMUTE_HOME is unset. That is intentionally OUT OF
//! SCOPE here — COMMUTE_HOME is the single env-based source of truth, per the
//! repo's inference de-bloat rules (env/Python over LLM).
//!
//! Forecast extraction (BUG 2): the OpenWeatherMap free tier exposes current
//! conditions at /data/2.5/weather and a 5-day / 3-hour forecast at
//! /data/2.5/forecast. The forecast endpoint returns a `list` of 3-hour data
//! points each stamped with `dt` (unix UTC) and `dt_txt` ("YYYY-MM-DD HH:MM:SS").
//!   - `tomorrow` filters the list to the points whose date == today+1 (UTC),
//!     then reduces them to a min/max temp and the most common condition.
//!   - `week` groups every point by its date and summarises each day the same
//!     way, giving the full ~5-day outlook.
//! All parsing is done in Rust with serde — no LLM.
//!
//! Required env:
//!   OPENWEATHER_API_KEY  — OpenWeatherMap API key (free tier works)
//! Optional env:
//!   OPENWEATHER_API_URL  — base URL (default https://api.openweathermap.org)
//!   OPENWEATHER_UNITS    — metric (default) | imperial | standard
//!   COMMUTE_HOME         — default location when none is supplied

use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::BTreeMap;

use crate::error::ToolError;
use crate::registry::ToolRegistry;
use crate::tool::RustTool;

const DEFAULT_BASE_URL: &str = "https://api.openweathermap.org";
const DEFAULT_UNITS: &str = "metric";

// ── Config ──────────────────────────────────────────────────────────────────

#[derive(Clone)]
struct WeatherConfig {
    api_key: String,
    base_url: String,
    units: String,
    /// Operator home address, reused from the commute tools' COMMUTE_HOME so a
    /// bare `weather` call resolves to "where I live" without re-prompting.
    home: Option<String>,
}

impl WeatherConfig {
    fn from_env() -> Result<Self, ToolError> {
        let api_key = std::env::var("OPENWEATHER_API_KEY")
            .ok()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| ToolError::NotConfigured("OPENWEATHER_API_KEY not set".into()))?;
        let base_url = std::env::var("OPENWEATHER_API_URL")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| DEFAULT_BASE_URL.to_string());
        let units = std::env::var("OPENWEATHER_UNITS")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| DEFAULT_UNITS.to_string());
        Ok(Self {
            api_key,
            base_url: base_url.trim_end_matches('/').to_string(),
            units,
            home: std::env::var("COMMUTE_HOME").ok().filter(|s| !s.is_empty()),
        })
    }

    fn client() -> Result<reqwest::Client, ToolError> {
        reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .user_agent("MooseNet-MCP/1.0")
            .build()
            .map_err(|e| ToolError::Http(e.to_string()))
    }

    /// Resolve the caller-supplied location into something the OWM API accepts.
    /// An absent/empty location falls back to COMMUTE_HOME (BUG 1); if that is
    /// also unset we return a clear, actionable NotConfigured error rather than
    /// silently guessing.
    fn resolve_location(&self, input: Option<&str>) -> Result<String, ToolError> {
        let trimmed = input.map(str::trim).filter(|s| !s.is_empty());
        match trimmed {
            Some(loc) => Ok(loc.to_string()),
            None => self.home.clone().ok_or_else(|| {
                ToolError::NotConfigured(
                    "No location given and COMMUTE_HOME is not configured. \
                     Set COMMUTE_HOME to a default home address or pass a 'location'."
                        .into(),
                )
            }),
        }
    }

    /// Temperature unit suffix for human-readable output.
    fn temp_unit(&self) -> &str {
        match self.units.as_str() {
            "imperial" => "°F",
            "standard" => "K",
            _ => "°C",
        }
    }
}

// ── Geocoding ───────────────────────────────────────────────────────────────

/// Resolve a location string to (lat, lon). Accepts a literal "lat,lon" pair
/// as-is; otherwise queries the OWM geocoding API.
async fn geocode(
    client: &reqwest::Client,
    cfg: &WeatherConfig,
    location: &str,
) -> Result<(f64, f64), ToolError> {
    if let Some(pair) = parse_coord_pair(location) {
        return Ok(pair);
    }

    let url = format!("{}/geo/1.0/direct", cfg.base_url);
    let resp = client
        .get(&url)
        .query(&[
            ("q", location),
            ("limit", "1"),
            ("appid", cfg.api_key.as_str()),
        ])
        .send()
        .await
        .map_err(|e| ToolError::Http(e.to_string()))?;

    if !resp.status().is_success() {
        return Err(ToolError::Http(format!(
            "Geocode HTTP {} for '{location}'",
            resp.status()
        )));
    }

    let body: Value = resp.json().await.map_err(|e| ToolError::Http(e.to_string()))?;
    let first = body
        .as_array()
        .and_then(|a| a.first())
        .ok_or_else(|| ToolError::NotFound(format!("Could not geocode '{location}'")))?;
    let lat = first
        .get("lat")
        .and_then(Value::as_f64)
        .ok_or_else(|| ToolError::NotFound(format!("No latitude for '{location}'")))?;
    let lon = first
        .get("lon")
        .and_then(Value::as_f64)
        .ok_or_else(|| ToolError::NotFound(format!("No longitude for '{location}'")))?;
    Ok((lat, lon))
}

/// Parse "lat,lon" → (f64, f64). Returns None if not a coordinate pair.
fn parse_coord_pair(s: &str) -> Option<(f64, f64)> {
    let parts: Vec<&str> = s.split(',').collect();
    if parts.len() != 2 {
        return None;
    }
    let lat = parts[0].trim().parse::<f64>().ok()?;
    let lon = parts[1].trim().parse::<f64>().ok()?;
    Some((lat, lon))
}

// ── API calls ───────────────────────────────────────────────────────────────

async fn fetch_current(
    client: &reqwest::Client,
    cfg: &WeatherConfig,
    lat: f64,
    lon: f64,
) -> Result<Value, ToolError> {
    let url = format!("{}/data/2.5/weather", cfg.base_url);
    let resp = client
        .get(&url)
        .query(&[
            ("lat", lat.to_string()),
            ("lon", lon.to_string()),
            ("units", cfg.units.clone()),
            ("appid", cfg.api_key.clone()),
        ])
        .send()
        .await
        .map_err(|e| ToolError::Http(e.to_string()))?;

    if !resp.status().is_success() {
        return Err(ToolError::Http(format!(
            "Weather HTTP {} (current)",
            resp.status()
        )));
    }
    resp.json().await.map_err(|e| ToolError::Http(e.to_string()))
}

async fn fetch_forecast(
    client: &reqwest::Client,
    cfg: &WeatherConfig,
    lat: f64,
    lon: f64,
) -> Result<Value, ToolError> {
    let url = format!("{}/data/2.5/forecast", cfg.base_url);
    let resp = client
        .get(&url)
        .query(&[
            ("lat", lat.to_string()),
            ("lon", lon.to_string()),
            ("units", cfg.units.clone()),
            ("appid", cfg.api_key.clone()),
        ])
        .send()
        .await
        .map_err(|e| ToolError::Http(e.to_string()))?;

    if !resp.status().is_success() {
        return Err(ToolError::Http(format!(
            "Weather HTTP {} (forecast)",
            resp.status()
        )));
    }
    resp.json().await.map_err(|e| ToolError::Http(e.to_string()))
}

// ── Parsing / summarising ───────────────────────────────────────────────────

/// A reduced per-day summary of forecast data points.
struct DaySummary {
    date: String,
    temp_min: f64,
    temp_max: f64,
    condition: String,
}

/// Reduce a slice of OWM forecast `list` entries (all for one day) into a
/// min/max temperature and the most frequent textual condition.
fn summarise_points(date: &str, points: &[&Value]) -> Option<DaySummary> {
    if points.is_empty() {
        return None;
    }
    let mut temp_min = f64::INFINITY;
    let mut temp_max = f64::NEG_INFINITY;
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();

    for p in points {
        if let Some(main) = p.get("main") {
            if let Some(t) = main.get("temp_min").and_then(Value::as_f64) {
                temp_min = temp_min.min(t);
            }
            if let Some(t) = main.get("temp_max").and_then(Value::as_f64) {
                temp_max = temp_max.max(t);
            }
            // Fall back to the instantaneous temp if min/max are absent.
            if let Some(t) = main.get("temp").and_then(Value::as_f64) {
                temp_min = temp_min.min(t);
                temp_max = temp_max.max(t);
            }
        }
        if let Some(desc) = p
            .get("weather")
            .and_then(Value::as_array)
            .and_then(|a| a.first())
            .and_then(|w| w.get("description"))
            .and_then(Value::as_str)
        {
            *counts.entry(desc.to_string()).or_insert(0) += 1;
        }
    }

    if !temp_min.is_finite() || !temp_max.is_finite() {
        return None;
    }

    let condition = counts
        .into_iter()
        .max_by_key(|(_, n)| *n)
        .map(|(d, _)| d)
        .unwrap_or_else(|| "unknown".to_string());

    Some(DaySummary {
        date: date.to_string(),
        temp_min,
        temp_max,
        condition,
    })
}

/// The `YYYY-MM-DD` date portion of an OWM `dt_txt` field.
fn date_of(point: &Value) -> Option<String> {
    point
        .get("dt_txt")
        .and_then(Value::as_str)
        .and_then(|s| s.split_whitespace().next())
        .map(str::to_string)
}

/// Group a forecast `list` by calendar date (preserving chronological order).
fn group_by_date(list: &[Value]) -> Vec<(String, Vec<&Value>)> {
    let mut order: Vec<String> = Vec::new();
    let mut groups: BTreeMap<String, Vec<&Value>> = BTreeMap::new();
    for p in list {
        if let Some(d) = date_of(p) {
            if !groups.contains_key(&d) {
                order.push(d.clone());
            }
            groups.entry(d).or_default().push(p);
        }
    }
    order
        .into_iter()
        .map(|d| {
            let pts = groups.remove(&d).unwrap_or_default();
            (d, pts)
        })
        .collect()
}

fn format_current(cfg: &WeatherConfig, label: &str, body: &Value) -> String {
    let unit = cfg.temp_unit();
    let temp = body
        .get("main")
        .and_then(|m| m.get("temp"))
        .and_then(Value::as_f64);
    let feels = body
        .get("main")
        .and_then(|m| m.get("feels_like"))
        .and_then(Value::as_f64);
    let desc = body
        .get("weather")
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .and_then(|w| w.get("description"))
        .and_then(Value::as_str)
        .unwrap_or("unknown conditions");
    let humidity = body
        .get("main")
        .and_then(|m| m.get("humidity"))
        .and_then(Value::as_f64);
    let wind = body
        .get("wind")
        .and_then(|w| w.get("speed"))
        .and_then(Value::as_f64);

    let mut out = format!("Current weather for {label}: {desc}");
    if let Some(t) = temp {
        out.push_str(&format!(", {t:.0}{unit}"));
    }
    if let Some(f) = feels {
        out.push_str(&format!(" (feels like {f:.0}{unit})"));
    }
    if let Some(h) = humidity {
        out.push_str(&format!(", humidity {h:.0}%"));
    }
    if let Some(w) = wind {
        out.push_str(&format!(", wind {w:.0}"));
    }
    out.push('.');
    out
}

fn format_day(cfg: &WeatherConfig, d: &DaySummary) -> String {
    let unit = cfg.temp_unit();
    format!(
        "{}: {}, {:.0}–{:.0}{unit}",
        d.date, d.condition, d.temp_min, d.temp_max
    )
}

// ── Tool ────────────────────────────────────────────────────────────────────

struct Weather {
    cfg: WeatherConfig,
}

#[async_trait]
impl RustTool for Weather {
    fn name(&self) -> &str {
        "weather"
    }

    fn description(&self) -> &str {
        "Get the weather for a place. 'location' is OPTIONAL — when omitted it \
defaults to the user's home (the configured home address), so you do NOT need to \
ask which city. Pass a city name, an address, or 'lat,lon' to override. \
'when' selects the timeframe: 'current' (default, conditions right now), \
'tomorrow' (tomorrow's high/low and outlook), or 'week' (the 5-day outlook). \
Returns a short human-readable summary."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "location": {
                    "type": "string",
                    "description": "City, address, or 'lat,lon'. Optional — defaults to the user's home."
                },
                "when": {
                    "type": "string",
                    "enum": ["current", "tomorrow", "week"],
                    "description": "current (default), tomorrow, or week (5-day outlook)."
                }
            }
        })
    }

    async fn execute(&self, args: Value) -> Result<String, ToolError> {
        let location = self.cfg.resolve_location(args["location"].as_str())?;
        let when = args["when"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("current");

        let client = WeatherConfig::client()?;
        let (lat, lon) = geocode(&client, &self.cfg, &location).await?;

        match when {
            "current" => {
                let body = fetch_current(&client, &self.cfg, lat, lon).await?;
                Ok(format_current(&self.cfg, &location, &body))
            }
            "tomorrow" => {
                let body = fetch_forecast(&client, &self.cfg, lat, lon).await?;
                let list = body
                    .get("list")
                    .and_then(Value::as_array)
                    .ok_or_else(|| ToolError::NotFound("No forecast data returned".into()))?;
                let grouped = group_by_date(list);
                // Tomorrow is the second distinct date in the forecast (the
                // first is today). If only one day is present, there is no
                // tomorrow to report.
                let day = grouped
                    .get(1)
                    .and_then(|(date, pts)| summarise_points(date, pts))
                    .ok_or_else(|| {
                        ToolError::NotFound("No forecast available for tomorrow".into())
                    })?;
                Ok(format!(
                    "Tomorrow's weather for {location} — {}",
                    format_day(&self.cfg, &day)
                ))
            }
            "week" => {
                let body = fetch_forecast(&client, &self.cfg, lat, lon).await?;
                let list = body
                    .get("list")
                    .and_then(Value::as_array)
                    .ok_or_else(|| ToolError::NotFound("No forecast data returned".into()))?;
                let grouped = group_by_date(list);
                let days: Vec<DaySummary> = grouped
                    .iter()
                    .filter_map(|(date, pts)| summarise_points(date, pts))
                    .collect();
                if days.is_empty() {
                    return Err(ToolError::NotFound("No forecast data available".into()));
                }
                let mut out = format!("{}-day outlook for {location}:\n", days.len());
                for d in &days {
                    out.push_str(&format!("- {}\n", format_day(&self.cfg, d)));
                }
                Ok(out)
            }
            other => Err(ToolError::InvalidArgument(format!(
                "'when' must be current, tomorrow, or week (got '{other}')"
            ))),
        }
    }
}

// ── Registration ────────────────────────────────────────────────────────────

pub fn register(registry: &mut ToolRegistry) {
    match WeatherConfig::from_env() {
        Ok(cfg) => {
            registry.register_or_replace(Box::new(Weather { cfg }));
        }
        Err(e) => {
            tracing::warn!("Weather tool not configured: {e}. Registering stub.");
            registry.register_or_replace(Box::new(NotConfiguredStub));
        }
    }
}

struct NotConfiguredStub;

#[async_trait]
impl RustTool for NotConfiguredStub {
    fn name(&self) -> &str {
        "weather"
    }
    fn description(&self) -> &str {
        "Weather tool (OPENWEATHER_API_KEY not configured)"
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {}})
    }
    async fn execute(&self, _args: Value) -> Result<String, ToolError> {
        Err(ToolError::NotConfigured("OPENWEATHER_API_KEY not set".into()))
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use httpmock::prelude::*;

    fn cfg_for(server: &MockServer, home: Option<&str>) -> WeatherConfig {
        WeatherConfig {
            api_key: "testkey".into(),
            base_url: server.base_url(),
            units: "metric".into(),
            home: home.map(str::to_string),
        }
    }

    fn geo_body() -> Value {
        json!([{ "name": "San Francisco", "lat": 37.7749, "lon": -122.4194, "country": "US" }])
    }

    fn current_body() -> Value {
        json!({
            "weather": [{ "description": "clear sky" }],
            "main": { "temp": 18.0, "feels_like": 17.0, "humidity": 60, "temp_min": 15.0, "temp_max": 20.0 },
            "wind": { "speed": 3.0 }
        })
    }

    /// Forecast spanning today (2 points) + tomorrow (2) + day-after (1).
    fn forecast_body() -> Value {
        json!({
            "list": [
                { "dt_txt": "2026-06-09 12:00:00", "main": { "temp": 19.0, "temp_min": 17.0, "temp_max": 21.0 }, "weather": [{ "description": "clear sky" }] },
                { "dt_txt": "2026-06-09 15:00:00", "main": { "temp": 20.0, "temp_min": 18.0, "temp_max": 22.0 }, "weather": [{ "description": "clear sky" }] },
                { "dt_txt": "2026-06-10 09:00:00", "main": { "temp": 14.0, "temp_min": 12.0, "temp_max": 16.0 }, "weather": [{ "description": "light rain" }] },
                { "dt_txt": "2026-06-10 18:00:00", "main": { "temp": 16.0, "temp_min": 13.0, "temp_max": 19.0 }, "weather": [{ "description": "light rain" }] },
                { "dt_txt": "2026-06-11 12:00:00", "main": { "temp": 22.0, "temp_min": 19.0, "temp_max": 25.0 }, "weather": [{ "description": "few clouds" }] }
            ]
        })
    }

    // ── location resolution (BUG 1) ──────────────────────────────────────────

    #[test]
    fn resolve_explicit_location_passthrough() {
        let c = WeatherConfig {
            api_key: "k".into(),
            base_url: "http://x".into(),
            units: "metric".into(),
            home: Some("Reno NV".into()),
        };
        assert_eq!(c.resolve_location(Some("Paris")).unwrap(), "Paris");
    }

    #[test]
    fn resolve_omitted_location_falls_back_to_home() {
        let c = WeatherConfig {
            api_key: "k".into(),
            base_url: "http://x".into(),
            units: "metric".into(),
            home: Some("123 Home St".into()),
        };
        assert_eq!(c.resolve_location(None).unwrap(), "123 Home St");
        // empty string is treated as omitted
        assert_eq!(c.resolve_location(Some("  ")).unwrap(), "123 Home St");
    }

    #[test]
    fn resolve_missing_location_and_home_errors() {
        let c = WeatherConfig {
            api_key: "k".into(),
            base_url: "http://x".into(),
            units: "metric".into(),
            home: None,
        };
        match c.resolve_location(None) {
            Err(ToolError::NotConfigured(msg)) => {
                assert!(msg.contains("COMMUTE_HOME"));
                assert!(msg.contains("location"));
            }
            other => panic!("expected NotConfigured, got {other:?}"),
        }
    }

    #[test]
    fn parse_coord_pair_works() {
        assert_eq!(parse_coord_pair("37.75,-122.41"), Some((37.75, -122.41)));
        assert_eq!(parse_coord_pair(" 1.0 , 2.0 "), Some((1.0, 2.0)));
        assert_eq!(parse_coord_pair("San Jose, CA"), None);
        assert_eq!(parse_coord_pair("37.75"), None);
    }

    // ── missing key → NotConfigured ──────────────────────────────────────────

    #[tokio::test]
    async fn stub_returns_not_configured() {
        let r = NotConfiguredStub.execute(json!({})).await;
        assert!(matches!(r, Err(ToolError::NotConfigured(_))));
    }

    // ── current → /data/2.5/weather ──────────────────────────────────────────

    #[tokio::test]
    async fn current_hits_weather_endpoint() {
        let server = MockServer::start();
        let geo = server.mock(|when, then| {
            when.method(GET).path("/geo/1.0/direct").query_param("q", "San Francisco");
            then.status(200).json_body(geo_body());
        });
        let wx = server.mock(|when, then| {
            when.method(GET).path("/data/2.5/weather");
            then.status(200).json_body(current_body());
        });

        let tool = Weather { cfg: cfg_for(&server, None) };
        let out = tool.execute(json!({"location": "San Francisco", "when": "current"})).await.unwrap();
        geo.assert();
        wx.assert();
        assert!(out.contains("clear sky"));
        assert!(out.contains("18°C"));
    }

    #[tokio::test]
    async fn current_is_default_when_when_omitted() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET).path("/geo/1.0/direct");
            then.status(200).json_body(geo_body());
        });
        let wx = server.mock(|when, then| {
            when.method(GET).path("/data/2.5/weather");
            then.status(200).json_body(current_body());
        });
        let tool = Weather { cfg: cfg_for(&server, None) };
        let out = tool.execute(json!({"location": "San Francisco"})).await.unwrap();
        wx.assert();
        assert!(out.starts_with("Current weather"));
    }

    // ── omitted location uses COMMUTE_HOME (BUG 1, end-to-end) ────────────────

    #[tokio::test]
    async fn omitted_location_geocodes_home() {
        let server = MockServer::start();
        let geo = server.mock(|when, then| {
            when.method(GET).path("/geo/1.0/direct").query_param("q", "1 Home Rd");
            then.status(200).json_body(geo_body());
        });
        server.mock(|when, then| {
            when.method(GET).path("/data/2.5/weather");
            then.status(200).json_body(current_body());
        });
        let tool = Weather { cfg: cfg_for(&server, Some("1 Home Rd")) };
        // No "location" key at all.
        let out = tool.execute(json!({"when": "current"})).await.unwrap();
        geo.assert();
        assert!(out.contains("1 Home Rd"));
    }

    #[tokio::test]
    async fn omitted_location_no_home_errors() {
        let server = MockServer::start();
        let tool = Weather { cfg: cfg_for(&server, None) };
        let r = tool.execute(json!({"when": "current"})).await;
        assert!(matches!(r, Err(ToolError::NotConfigured(_))));
    }

    // ── tomorrow → /data/2.5/forecast, tomorrow extraction ───────────────────

    #[tokio::test]
    async fn tomorrow_hits_forecast_and_extracts_second_day() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET).path("/geo/1.0/direct");
            then.status(200).json_body(geo_body());
        });
        let fc = server.mock(|when, then| {
            when.method(GET).path("/data/2.5/forecast");
            then.status(200).json_body(forecast_body());
        });
        let tool = Weather { cfg: cfg_for(&server, None) };
        let out = tool.execute(json!({"location": "SF", "when": "tomorrow"})).await.unwrap();
        fc.assert();
        // Second distinct date is 2026-06-10 with "light rain", 12–19.
        assert!(out.contains("2026-06-10"));
        assert!(out.contains("light rain"));
        assert!(out.contains("12") && out.contains("19"));
        // must NOT report today's clear sky as tomorrow
        assert!(!out.contains("2026-06-09"));
    }

    // ── week → /data/2.5/forecast, full outlook ──────────────────────────────

    #[tokio::test]
    async fn week_summarises_all_days() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET).path("/geo/1.0/direct");
            then.status(200).json_body(geo_body());
        });
        let fc = server.mock(|when, then| {
            when.method(GET).path("/data/2.5/forecast");
            then.status(200).json_body(forecast_body());
        });
        let tool = Weather { cfg: cfg_for(&server, None) };
        let out = tool.execute(json!({"location": "SF", "when": "week"})).await.unwrap();
        fc.assert();
        // Three distinct days present.
        assert!(out.contains("3-day outlook"));
        assert!(out.contains("2026-06-09"));
        assert!(out.contains("2026-06-10"));
        assert!(out.contains("2026-06-11"));
        assert!(out.contains("few clouds"));
    }

    // ── coord pair skips geocoding ───────────────────────────────────────────

    #[tokio::test]
    async fn coord_pair_skips_geocode() {
        let server = MockServer::start();
        let geo = server.mock(|when, then| {
            when.method(GET).path("/geo/1.0/direct");
            then.status(200).json_body(geo_body());
        });
        let wx = server.mock(|when, then| {
            when.method(GET).path("/data/2.5/weather");
            then.status(200).json_body(current_body());
        });
        let tool = Weather { cfg: cfg_for(&server, None) };
        let out = tool.execute(json!({"location": "37.77,-122.41"})).await.unwrap();
        // geocode endpoint should NOT have been called
        assert_eq!(geo.hits(), 0);
        wx.assert();
        assert!(out.contains("clear sky"));
    }

    // ── invalid `when` ───────────────────────────────────────────────────────

    #[tokio::test]
    async fn invalid_when_errors() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET).path("/geo/1.0/direct");
            then.status(200).json_body(geo_body());
        });
        let tool = Weather { cfg: cfg_for(&server, None) };
        let r = tool.execute(json!({"location": "SF", "when": "yesterday"})).await;
        assert!(matches!(r, Err(ToolError::InvalidArgument(_))));
    }

    // ── forecast parsing helpers ─────────────────────────────────────────────

    #[test]
    fn group_by_date_preserves_order_and_groups() {
        let body = forecast_body();
        let list = body.get("list").and_then(Value::as_array).unwrap();
        let grouped = group_by_date(list);
        assert_eq!(grouped.len(), 3);
        assert_eq!(grouped[0].0, "2026-06-09");
        assert_eq!(grouped[0].1.len(), 2);
        assert_eq!(grouped[1].0, "2026-06-10");
        assert_eq!(grouped[1].1.len(), 2);
        assert_eq!(grouped[2].1.len(), 1);
    }

    #[test]
    fn summarise_points_min_max_and_condition() {
        let body = forecast_body();
        let list = body.get("list").and_then(Value::as_array).unwrap();
        let grouped = group_by_date(list);
        let (date, pts) = &grouped[1]; // tomorrow
        let s = summarise_points(date, pts).unwrap();
        assert_eq!(s.condition, "light rain");
        assert_eq!(s.temp_min, 12.0);
        assert_eq!(s.temp_max, 19.0);
    }

    #[test]
    fn temp_unit_by_units() {
        let mk = |u: &str| WeatherConfig {
            api_key: "k".into(),
            base_url: "http://x".into(),
            units: u.into(),
            home: None,
        };
        assert_eq!(mk("metric").temp_unit(), "°C");
        assert_eq!(mk("imperial").temp_unit(), "°F");
        assert_eq!(mk("standard").temp_unit(), "K");
    }

    // ── registration ─────────────────────────────────────────────────────────

    #[test]
    #[serial]
    fn register_adds_weather_stub_without_key() {
        let mut reg = ToolRegistry::new();
        let key = std::env::var("OPENWEATHER_API_KEY").ok();
        std::env::remove_var("OPENWEATHER_API_KEY");
        register(&mut reg);
        if let Some(k) = key { std::env::set_var("OPENWEATHER_API_KEY", k); }
        assert!(reg.contains("weather"));
    }

    #[test]
    fn tool_name_and_schema_stable() {
        let cfg = WeatherConfig {
            api_key: "k".into(),
            base_url: "http://x".into(),
            units: "metric".into(),
            home: None,
        };
        let t = Weather { cfg };
        assert_eq!(t.name(), "weather");
        let p = t.parameters();
        assert_eq!(p["type"], "object");
        assert!(p["properties"]["location"].is_object());
        assert!(p["properties"]["when"]["enum"].is_array());
        // description advertises the home default so the model won't re-prompt
        assert!(t.description().to_lowercase().contains("home"));
        assert!(t.description().to_lowercase().contains("optional"));
    }
}
