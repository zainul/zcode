//! zcode — the lean Rust coding agent.
//!
//! This crate is the composition root: it wires concrete infrastructure
//! adapters into the `app::App` engine and exposes both interfaces — the
//! headless CLI and the interactive TUI (FR-IFACE-03: one engine, two faces).
#![forbid(unsafe_code)]

pub mod cli;
