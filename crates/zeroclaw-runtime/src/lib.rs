//! Agent runtime — orchestration, security, observability, cron, SOP, skills, hardware, and more.

pub mod cli_input;
pub mod i18n;
pub mod identity;
pub mod migration;
pub mod util;

pub mod agent;
pub mod approval;
pub mod cost;
pub mod cron;
pub mod daemon;
pub mod doctor;
pub mod health;
pub mod heartbeat;
pub mod hooks;
pub mod imessage_tcc;
pub mod integrations;
pub mod nodes;
pub mod observability;
pub mod onboard;
pub mod platform;
pub mod rag;
pub mod routines;
pub mod security;
pub mod service;
pub mod skillforge;
pub mod skills;
pub mod sop;
pub mod tools;
pub mod trust;
pub mod tunnel;
pub mod verifiable_intent;

// PWG-specific product bridge. Off by default so the upstream
// zeroclaw distribution does not pull PWG surface; opted in via
// `--features pwg-wing-resolution` (see this crate's Cargo.toml).
#[cfg(feature = "pwg-wing-resolution")]
pub mod pwg_wing_resolution;
