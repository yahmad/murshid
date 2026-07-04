//! The `murshid` subcommand implementations (`setup`/`goal`/`review`/
//! `progress`). `main.rs` parses argv and dispatches here; the `watch` arm
//! lives in the sibling `watch` module.

pub mod goal;
pub mod progress;
pub mod review;
pub mod setup;
