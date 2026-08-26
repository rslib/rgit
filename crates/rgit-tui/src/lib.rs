//! The rgit terminal UI.
//!
//! Structured as The Elm Architecture over an async event loop: an [`events`]
//! task produces input, tick, and render events; [`app::update`] turns messages
//! into state changes and [`app::Effect`]s; [`runtime`] performs the effects and
//! draws. Every view is a [`buffer::Buffer`] over a content tree.

mod ai;
mod app;
mod buffer;
mod config;
mod events;
mod forge;
mod keymap;
mod runtime;
pub mod session_log;
mod theme;
mod ui;

pub use config::{Config, load as load_config};
pub use runtime::run;
