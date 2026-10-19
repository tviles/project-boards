//! project-boards: a herdr plugin for GitHub Projects and Linear boards.

pub mod backend;
pub mod board_choice;
pub mod cli;
pub mod commands;
pub mod config;
pub mod error;
pub mod fsutil;
pub mod github;
pub mod herdr;
pub mod linear;
pub mod logging;
pub mod model;
pub mod ops;
pub mod state;
pub mod store;
pub mod sync;
pub mod transport;
pub mod ui;
