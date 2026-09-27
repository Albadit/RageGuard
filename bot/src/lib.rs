//! RageGuard: a Discord bot that listens to one selected member in voice, classifies the
//! emotion of their speech with an external AI service, and applies a timeout after repeated,
//! high-confidence angry speech.
//!
//! Everything except process startup lives in this library so it can be tested.

pub mod ai;
pub mod app;
pub mod commands;
pub mod config;
pub mod detection;
pub mod discord;
pub mod logging;
pub mod moderation;
pub mod monitor;
pub mod server_log;
pub mod state;
pub mod voice;
