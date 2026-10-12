//! The terminal UI: a pure state machine (`app`) plus rendering and the runtime loop.

pub mod app;
pub mod board;
pub mod chrome;
pub mod controller;
pub mod detail;
pub mod keymap;
pub mod labels;
pub mod markdown;
pub mod picker;
pub mod pills;
pub mod runtime;
pub mod search;
pub mod table;
pub mod text;
pub mod theme;

#[cfg(test)]
pub mod fixtures;
