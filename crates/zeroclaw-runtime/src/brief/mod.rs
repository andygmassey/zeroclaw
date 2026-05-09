//! Daily brief composition helpers.
//!
//! Two responsibilities at this layer:
//!
//! 1. **Thinness detection** — for a fresh customer (Day 1) or a
//!    customer with a quiet day, we don't want the brief LLM to
//!    fabricate "things to do" out of nothing. The
//!    [`ThinnessSignals`] struct + [`is_thin`](thin::is_thin)
//!    classifier let the cron job's prompt-template stage decide
//!    whether to compose a normal data-rich brief or a gentle
//!    thin-data brief.
//!
//! 2. **Weather lookup** — when we DO go thin-data, we surface a
//!    one-line observational hook ("the sun is shining, why not
//!    go for a walk") that's grounded in current local weather.
//!    See [`weather`] for the open-meteo client + 6h cache.
//!
//! Both pieces are independent of the cron scheduler and the
//! agent runtime; they're plain library functions that any
//! downstream brief composer can call.
//!
//! # Privacy
//!
//! The weather lookup sends *only* `latitude` + `longitude` (and
//! a fixed parameter list) to `api.open-meteo.com`. No customer
//! identifier, no user-agent personalisation, no PWG content.
//! The lat/lon resolution priority is also privacy-graded:
//!
//! 1. customer-supplied lat/lon in the workspace config
//!    (preferred — fully offline-resolvable).
//! 2. a coarse IP-geo lookup against `ipapi.co`
//!    (fallback only — *one* outbound request, country-level
//!    accuracy is good enough for "is it raining").
//!
//! The macOS Core Location entitlement path mentioned in early
//! design notes is deliberately not wired here — invoking
//! CLLocationManager from a library crate would force the
//! parent process to carry `NSLocationUsageDescription`, which
//! is a customer-visible permission prompt we don't want for a
//! best-effort weather hook. Customers who want exact-block
//! accuracy supply their own lat/lon in the config.
//!
//! # Defaults
//!
//! - Weather cache TTL: 6 hours (matches the granularity of the
//!   open-meteo `current` payload at typical reporting intervals).
//! - Reqwest timeout: 5 seconds (the brief is happy to skip
//!   weather rather than block delivery on a slow ipapi.co).
//!
//! British English throughout.

pub mod thin;
pub mod weather;

pub use thin::{ThinReason, ThinnessSignals, is_thin};
pub use weather::{
    ResolvedLocation, WeatherCategory, WeatherSnapshot, classify_weather_code, render_thin_brief,
    resolve_location,
};
