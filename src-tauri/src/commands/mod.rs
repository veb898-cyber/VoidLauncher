//! Tauri command layer: thin wrappers over services/domain modules.
//!
//! Each command validates its inputs (instance names, subfolders, URLs,
//! filenames), then delegates to the corresponding domain module.
//! No file-system traversal, downloading, or state mutation happens here
//! beyond what a command necessarily orchestrates.

pub mod auth;
pub mod instances;
pub mod launcher;
pub mod misc;
pub mod mods;
pub mod versions;
// pub mod misc;
// pub mod mods;
// pub mod versions;
