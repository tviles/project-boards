//! The terminal UI: a pure state machine (`app`) plus rendering and the runtime loop.

pub mod board;
pub mod keymap;
pub mod table;
pub mod text;
pub mod theme;

#[cfg(test)]
pub mod fixtures;
