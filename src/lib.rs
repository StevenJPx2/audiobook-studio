//! Audiobook Studio library crate.
//!
//! All pipeline logic lives here so it can be shared by both binaries:
//! the egui GUI (`src/main.rs`) and the headless CLI (`src/bin/abs.rs`).
//! Intra-crate references use `crate::…`, which resolves the same for every
//! module declared below regardless of which binary links the library.
//!
//! `app` is the egui front end; it's part of the library only so the GUI
//! binary stays a thin shell, and is never used by the CLI.

pub mod agent;
pub mod app;
pub mod bundle;
pub mod cover;
pub mod error;
pub mod g2p;
pub mod kokoro;
pub mod model;
pub mod ocr;
pub mod pdf;
pub mod pipeline;
pub mod sidecar;
pub mod split;
pub mod tts;
