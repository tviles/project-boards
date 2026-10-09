//! The terminal UI: a pure state machine (`app`) plus rendering and the runtime loop.

pub mod app;
pub mod board;
pub mod detail;
pub mod keymap;
pub mod markdown;
pub mod picker;
pub mod search;
pub mod table;
pub mod text;
pub mod theme;

#[cfg(test)]
pub mod fixtures;
