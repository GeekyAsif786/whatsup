//! The interactive TUI (DESIGN.md section 13): `whatsup` with no arguments
//! on a terminal.
//!
//! - [`app`]: the pure state machine (`on_key` returns an `Effect`).
//! - [`view`]: pure rendering.

pub mod app;
#[cfg(test)]
mod fixtures;
pub mod view;
