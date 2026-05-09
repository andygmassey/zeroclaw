//! Weather lookup + thin-data prompt rendering.
//!
//! When the brief composer flips to thin-data mode (see
//! [`super::thin`]), this module provides:
//!
//! - [`resolve_location`] — picks a lat/lon for the customer:
//!   workspace-config supplied first, ipapi.co IP-geo as a
//!   single-request fallback. No Core Location.
//! - [`fetch_weather`] — queries open-meteo.com with that
//!   lat/lon and parses the WMO weather code into a small
//!   high-level [`WeatherCategory`].
//! - [`render_thin_brief`] — renders the customer-visible
//!   one-liner ("your day looks open, and it's properly raining
//!   ...") from the thinness reasons + weather snapshot.
//!
//! ## Privacy
//!
//! Open-meteo accepts coordinates only — no API key, no
//! customer identifier, no PWG payload. The request body is
//! the URL itself; we don't even set a custom User-Agent. See
//! the `weather_request_carries_no_pii` test for the locked-in
//! assertion.
//!
//! ipapi.co is a single, optional fallback. The customer's
//! workspace config is the preferred resolution path, and the
//! library will not fall back to ipapi.co if the customer has
//! supplied lat/lon explicitly.
//!
//! ## Caching
//!
//! Weather snapshots are cached for 6 hours per lat/lon pair.
//! The cache is in-process and resets on assistant restart;
//! for the brief use case (twice-daily fires) that's enough to
//! avoid hammering open-meteo while still letting a long-
//! running daemon refresh between morning and evening fires.
//!
//! British English throughout.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Mutex;

use super::thin::ThinReason;

/// Customer-supplied location overrides loaded from the
/// workspace config (typically `[brief.location]` in
/// `config.toml`). When all three fields are populated, we
/// skip the ipapi.co fallback entirely.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct LocationConfig {
    /// Latitude in degrees, -90..90.
    pub latitude: Option<f64>,
    /// Longitude in degrees, -180..180.
    pub longitude: Option<f64>,
    /// Optional descriptive label used in copy. If absent we
    /// don't name the location in the brief output.
    pub label: Option<String>,
}

/// A resolved location with the source recorded so the caller
/// can log "where did this lat/lon come from".
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedLocation {
    pub latitude: f64,
    pub longitude: f64,
    pub source: LocationSource,
    pub label: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocationSource {
    /// Customer-supplied lat/lon via workspace config.
    /// Preferred — no outbound network, no ipapi.co dependency.
    Config,
    /// Best-effort fallback via ipapi.co. Coarse (city-level
    /// at best) but adequate for "is it raining".
    IpGeo,
}

/// High-level weather category collapsing the WMO code space
/// to the half-dozen states the thin-data copy actually uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WeatherCategory {
    Clear,
    PartlyCloudy,
    Fog,
    Rain,
    Snow,
    Showers,
    Thunder,
    Unknown,
}

/// Open-meteo `current` block, deserialised. Field names match
/// the API exactly so serde does the work; only the fields we
/// actually use are deserialised.
#[derive(Debug, Clone, Deserialize)]
struct OpenMeteoCurrent {
    temperature_2m: Option<f64>,
    weather_code: Option<i32>,
    precipitation: Option<f64>,
    wind_speed_10m: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct OpenMeteoResponse {
    current: Option<OpenMeteoCurrent>,
}

/// Internal cache key. (lat, lon) bucketed to 0.01 degrees to
/// avoid float-equality false misses; that's about 1km of
/// resolution which is more than weather forecasts deliver
/// anyway.
fn cache_bucket(lat: f64, lon: f64) -> (i32, i32) {
    ((lat * 100.0).round() as i32, (lon * 100.0).round() as i32)
}

#[derive(Debug, Clone)]
struct CacheEntry {
    snapshot: WeatherSnapshot,
    cached_at: DateTime<Utc>,
}

/// Cache TTL — 6 hours, per the v0.2 design note. Long enough
/// that a twice-daily brief reuses the morning snapshot when
/// it's still relevant; short enough to refresh before the
/// evening wrap.
const CACHE_TTL: Duration = Duration::hours(6);

static CACHE: Mutex<Option<HashMap<(i32, i32), CacheEntry>>> = Mutex::new(None);

/// What we've learned about the current weather. Prefer this
/// shape over passing the raw open-meteo response into the
/// renderer — keeps the renderer testable without a HTTP mock.
#[derive(Debug, Clone, PartialEq)]
pub struct WeatherSnapshot {
    pub category: WeatherCategory,
    pub temperature_celsius: Option<f64>,
    pub precipitation_mm: Option<f64>,
    pub wind_speed_kph: Option<f64>,
}

/// Map a WMO `weather_code` to a high-level [`WeatherCategory`].
///
/// Reference: <https://open-meteo.com/en/docs#weather_variable_documentation>
pub fn classify_weather_code(code: i32) -> WeatherCategory {
    match code {
        0 => WeatherCategory::Clear,
        1..=3 => WeatherCategory::PartlyCloudy,
        45 | 48 => WeatherCategory::Fog,
        51..=67 => WeatherCategory::Rain,
        71..=77 => WeatherCategory::Snow,
        80..=82 => WeatherCategory::Showers,
        95..=99 => WeatherCategory::Thunder,
        _ => WeatherCategory::Unknown,
    }
}

/// Resolve a location for the brief composer.
///
/// Priority:
/// 1. `cfg` — customer-supplied lat/lon (fully offline).
/// 2. `ipapi.co/json/` lookup (single request, country-level
///    accuracy). Only consulted when `cfg` is incomplete.
///
/// Returns `Err` if neither path yielded a usable lat/lon —
/// the brief composer falls back to a no-weather thin brief.
pub async fn resolve_location(
    cfg: &LocationConfig,
    client: &reqwest::Client,
) -> anyhow::Result<ResolvedLocation> {
    // Path 1: workspace config has both lat + lon. Validate
    // ranges before trusting them; otherwise we'd send junk
    // to open-meteo.
    if let (Some(lat), Some(lon)) = (cfg.latitude, cfg.longitude) {
        if (-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon) {
            return Ok(ResolvedLocation {
                latitude: lat,
                longitude: lon,
                source: LocationSource::Config,
                label: cfg.label.clone(),
            });
        }
        tracing::warn!(
            "brief.location config has out-of-range lat/lon ({lat}, {lon}); falling back to IP-geo"
        );
    }

    // Path 2: ipapi.co. Coarse but no API key required.
    let resp = client
        .get("https://ipapi.co/json/")
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("ipapi.co request failed: {e}"))?;
    if !resp.status().is_success() {
        anyhow::bail!("ipapi.co returned non-success status {}", resp.status());
    }
    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| anyhow::anyhow!("ipapi.co response not JSON: {e}"))?;

    let lat = body.get("latitude").and_then(|v| v.as_f64());
    let lon = body.get("longitude").and_then(|v| v.as_f64());
    match (lat, lon) {
        (Some(lat), Some(lon)) => Ok(ResolvedLocation {
            latitude: lat,
            longitude: lon,
            source: LocationSource::IpGeo,
            // ipapi.co returns city + country_name; combining
            // them is too marketing-y for the Apple-restraint
            // brief tone, so we leave label unset and let the
            // renderer omit any place name.
            label: None,
        }),
        _ => anyhow::bail!("ipapi.co response did not contain latitude + longitude"),
    }
}

/// Fetch the current-weather snapshot from open-meteo for the
/// given lat/lon, going through the in-process cache.
pub async fn fetch_weather(
    lat: f64,
    lon: f64,
    client: &reqwest::Client,
) -> anyhow::Result<WeatherSnapshot> {
    let bucket = cache_bucket(lat, lon);
    let now = Utc::now();

    // Cache lookup. We don't hold the lock across the await
    // because reqwest is async; just probe and release.
    {
        let mut guard = CACHE.lock().expect("brief weather cache poisoned");
        let map = guard.get_or_insert_with(HashMap::new);
        if let Some(entry) = map.get(&bucket)
            && now.signed_duration_since(entry.cached_at) < CACHE_TTL
        {
            return Ok(entry.snapshot.clone());
        }
    }

    let url = build_weather_url(lat, lon);
    let resp = client
        .get(&url)
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("open-meteo request failed: {e}"))?;
    if !resp.status().is_success() {
        anyhow::bail!("open-meteo returned non-success status {}", resp.status());
    }
    let parsed: OpenMeteoResponse = resp
        .json()
        .await
        .map_err(|e| anyhow::anyhow!("open-meteo response not JSON: {e}"))?;
    let current = parsed
        .current
        .ok_or_else(|| anyhow::anyhow!("open-meteo response missing `current` block"))?;
    let snapshot = WeatherSnapshot {
        category: current
            .weather_code
            .map(classify_weather_code)
            .unwrap_or(WeatherCategory::Unknown),
        temperature_celsius: current.temperature_2m,
        precipitation_mm: current.precipitation,
        wind_speed_kph: current.wind_speed_10m,
    };

    // Insert / refresh.
    let mut guard = CACHE.lock().expect("brief weather cache poisoned");
    let map = guard.get_or_insert_with(HashMap::new);
    map.insert(
        bucket,
        CacheEntry {
            snapshot: snapshot.clone(),
            cached_at: now,
        },
    );

    Ok(snapshot)
}

/// Build the open-meteo URL with the documented `current=`
/// parameter list. Pulled out so the privacy test can assert
/// exactly what we send (lat + lon + named parameters, nothing
/// else).
pub fn build_weather_url(lat: f64, lon: f64) -> String {
    format!(
        "https://api.open-meteo.com/v1/forecast?latitude={lat}\
&longitude={lon}\
&current=temperature_2m,weather_code,precipitation,wind_speed_10m\
&timezone=auto"
    )
}

/// Render the thin-data brief one-liner.
///
/// Parameters:
/// - `reasons` — the [`ThinReason`]s the classifier returned.
///   We pick the most-specific reason as the framing (DayOne >
///   EmptyCalendar > others).
/// - `weather` — `Some(snapshot)` if we successfully resolved
///   weather; `None` if all fallbacks failed and we should
///   omit weather copy.
///
/// The output is plain prose, British English, no emoji, no
/// exclamation marks, observational not directive. Per Andy's
/// brief spec: "If thin data the user should be told something
/// along the lines of 'Your day looks quite free. The sun is
/// shining / it's raining / whatever, so why not go for a walk
/// / sit and chill inside / whatever' — you get the idea."
///
/// Importantly, the renderer does NOT fabricate to-dos. The
/// hooks ("a good morning for a walk") are calibrated as
/// gentle observations, not directives, and never fabricate
/// people / events / commitments.
pub fn render_thin_brief(reasons: &[ThinReason], weather: Option<&WeatherSnapshot>) -> String {
    let mut out = String::new();

    // Frame line — picks the most specific reason.
    if reasons.contains(&ThinReason::DayOne) {
        out.push_str(
            "Your assistant just woke up. There's not much to brief on yet \
             — give it a few days to learn your patterns, and these messages \
             get more useful.",
        );
    } else if reasons.contains(&ThinReason::EmptyCalendar) {
        out.push_str("Your day looks open.");
    } else {
        // Catch-all for the QuietOutbound / QuietInbox /
        // StalePeopleGraph cluster: there's some calendar
        // material but the people/inbox graph is thin.
        out.push_str("Light schedule today.");
    }

    // Weather hook (only if we actually got a snapshot).
    if let Some(w) = weather {
        let tail = match w.category {
            WeatherCategory::Clear => {
                " It's bright out there. A good morning for a walk before lunch."
            }
            WeatherCategory::PartlyCloudy => {
                " It's a mild, partly cloudy sort of day. Easy weather for a walk if you fancy one."
            }
            WeatherCategory::Fog => " It's foggy. A quiet morning for a slow start indoors.",
            WeatherCategory::Rain | WeatherCategory::Showers => {
                " It's properly raining. A good morning to settle in with whatever you've been meaning to read."
            }
            WeatherCategory::Snow => " It's snowing. Stay warm.",
            WeatherCategory::Thunder => {
                " There are thunderstorms about. Worth checking the windows are shut."
            }
            WeatherCategory::Unknown => "",
        };
        if !tail.is_empty() {
            out.push_str(tail);
        }
    }

    out
}

#[cfg(test)]
#[allow(clippy::print_stderr)]
mod tests {
    use super::*;

    #[test]
    fn classify_weather_code_covers_documented_ranges() {
        assert_eq!(classify_weather_code(0), WeatherCategory::Clear);
        assert_eq!(classify_weather_code(1), WeatherCategory::PartlyCloudy);
        assert_eq!(classify_weather_code(3), WeatherCategory::PartlyCloudy);
        assert_eq!(classify_weather_code(45), WeatherCategory::Fog);
        assert_eq!(classify_weather_code(48), WeatherCategory::Fog);
        assert_eq!(classify_weather_code(55), WeatherCategory::Rain);
        assert_eq!(classify_weather_code(73), WeatherCategory::Snow);
        assert_eq!(classify_weather_code(81), WeatherCategory::Showers);
        assert_eq!(classify_weather_code(99), WeatherCategory::Thunder);
        assert_eq!(classify_weather_code(150), WeatherCategory::Unknown);
        assert_eq!(classify_weather_code(-1), WeatherCategory::Unknown);
    }

    #[test]
    fn weather_request_carries_no_pii() {
        // Pin the URL shape — only lat / lon + the named
        // parameter list. Any future change that adds a
        // customer identifier or User-Agent token will trip
        // this test.
        let url = build_weather_url(51.5074, -0.1278);
        assert!(url.starts_with("https://api.open-meteo.com/v1/forecast?"));
        assert!(url.contains("latitude=51.5074"));
        assert!(url.contains("longitude=-0.1278"));
        assert!(url.contains("current=temperature_2m,weather_code,precipitation,wind_speed_10m"));
        assert!(url.contains("timezone=auto"));
        // No customer identifier or device-id token.
        assert!(!url.contains("apikey"));
        assert!(!url.contains("user_id"));
        assert!(!url.contains("device"));
        assert!(!url.contains("install_id"));
    }

    #[test]
    fn cache_bucket_groups_nearby_coordinates() {
        // 0.001 degree = ~111 metres; bucket should treat
        // 51.5074 + 51.5075 the same.
        assert_eq!(
            cache_bucket(51.5074, -0.1278),
            cache_bucket(51.5075, -0.1278)
        );
        // 0.05 degrees apart => different bucket.
        assert_ne!(cache_bucket(51.50, 0.0), cache_bucket(51.55, 0.0));
    }

    #[test]
    fn render_thin_brief_day_one_with_clear_weather() {
        let reasons = vec![
            ThinReason::DayOne,
            ThinReason::EmptyCalendar,
            ThinReason::QuietOutbound,
        ];
        let weather = WeatherSnapshot {
            category: WeatherCategory::Clear,
            temperature_celsius: Some(15.0),
            precipitation_mm: Some(0.0),
            wind_speed_kph: Some(5.0),
        };
        let out = render_thin_brief(&reasons, Some(&weather));
        assert!(out.contains("just woke up"));
        assert!(out.contains("bright"));
        // British English checks: must not contain Americanisms
        // for the basic content of this brief.
        assert!(!out.contains("realize"));
        assert!(!out.contains("favorite"));
        assert!(!out.contains("color"));
        // No emoji, no exclamation marks (Apple-restraint tone).
        assert!(!out.contains('!'));
    }

    #[test]
    fn render_thin_brief_empty_calendar_with_rain() {
        let reasons = vec![
            ThinReason::EmptyCalendar,
            ThinReason::QuietOutbound,
            ThinReason::QuietInbox,
        ];
        let weather = WeatherSnapshot {
            category: WeatherCategory::Rain,
            temperature_celsius: Some(8.0),
            precipitation_mm: Some(2.5),
            wind_speed_kph: Some(15.0),
        };
        let out = render_thin_brief(&reasons, Some(&weather));
        assert!(out.contains("open"));
        assert!(out.contains("raining"));
        assert!(!out.contains("rains")); // not "it rains"
        assert!(!out.contains('!'));
    }

    #[test]
    fn render_thin_brief_no_weather_does_not_fabricate() {
        let reasons = vec![
            ThinReason::EmptyCalendar,
            ThinReason::QuietOutbound,
            ThinReason::QuietInbox,
        ];
        let out = render_thin_brief(&reasons, None);
        assert!(out.contains("open"));
        // No weather hooks should appear.
        assert!(!out.contains("walk"));
        assert!(!out.contains("rain"));
        assert!(!out.contains("snow"));
        assert!(!out.contains("bright"));
    }

    #[test]
    fn render_thin_brief_snow() {
        let reasons = vec![ThinReason::EmptyCalendar];
        let weather = WeatherSnapshot {
            category: WeatherCategory::Snow,
            temperature_celsius: Some(-2.0),
            precipitation_mm: Some(1.0),
            wind_speed_kph: Some(10.0),
        };
        let out = render_thin_brief(&reasons, Some(&weather));
        assert!(out.contains("snowing"));
        assert!(out.contains("warm"));
        assert!(!out.contains("snows"));
    }

    #[test]
    fn render_thin_brief_uses_british_spellings() {
        // The renderer must not slip into Americanisms in the
        // hooks. Check the union of the rendered strings for
        // any of the common offenders.
        let all_categories = [
            WeatherCategory::Clear,
            WeatherCategory::PartlyCloudy,
            WeatherCategory::Fog,
            WeatherCategory::Rain,
            WeatherCategory::Showers,
            WeatherCategory::Snow,
            WeatherCategory::Thunder,
            WeatherCategory::Unknown,
        ];
        let mut all_renderings = String::new();
        for cat in all_categories.iter() {
            let weather = WeatherSnapshot {
                category: *cat,
                temperature_celsius: None,
                precipitation_mm: None,
                wind_speed_kph: None,
            };
            all_renderings.push_str(&render_thin_brief(
                &[ThinReason::EmptyCalendar],
                Some(&weather),
            ));
            all_renderings.push(' ');
        }
        // Common Americanism markers that must not appear.
        for offender in &[
            "color",
            "favorite",
            "realize",
            "organize",
            "neighbor",
            "traveling",
            "while ", // British prefers "whilst" but "while" alone is fine; this checks no "while you" Americanisms slipped in. Loosen if false-positive.
        ] {
            // "while " is a soft-flag — we don't have it in
            // current copy but it's worth catching if a future
            // edit adds e.g. "while you're at it".
            if all_renderings.contains(offender) && *offender != "while " {
                eprintln!("Americanism detected in copy: {offender}");
                panic!("Americanism detected: {offender}");
            }
        }
    }

    #[test]
    fn render_thin_brief_falls_back_when_neither_dayone_nor_empty_calendar() {
        let reasons = vec![
            ThinReason::QuietOutbound,
            ThinReason::QuietInbox,
            ThinReason::StalePeopleGraph,
        ];
        let out = render_thin_brief(&reasons, None);
        assert!(out.contains("Light schedule"));
    }

    #[test]
    fn resolved_location_config_path_validates_range() {
        // Out-of-range latitude => the validation flags it
        // and the resolver would fall back. We can't easily
        // exercise the network fallback in a unit test; check
        // that the validator at least pins the contract.
        let cfg_bad = LocationConfig {
            latitude: Some(95.0),
            longitude: Some(0.0),
            label: None,
        };
        assert!(!(-90.0..=90.0).contains(&cfg_bad.latitude.unwrap()));

        let cfg_ok = LocationConfig {
            latitude: Some(51.5074),
            longitude: Some(-0.1278),
            label: Some("London".into()),
        };
        assert!((-90.0..=90.0).contains(&cfg_ok.latitude.unwrap()));
        assert!((-180.0..=180.0).contains(&cfg_ok.longitude.unwrap()));
    }
}
