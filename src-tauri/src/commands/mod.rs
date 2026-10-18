//! Tauri command layer: thin wrappers over services/domain modules.
//!
//! Each command validates its inputs (instance names, subfolders, URLs,
//! filenames), then delegates to the corresponding domain module.
//! No file-system traversal, downloading, or state mutation happens here
//! beyond what a command necessarily orchestrates.

pub mod auth;
pub mod content;
pub mod diagnostics;
pub mod heads;
pub mod instances;
pub mod launcher;
pub mod misc;
pub mod modpacks;
pub mod mods;
pub mod packlock;
pub mod rooms;
pub mod servers;
pub mod verify;
pub mod versions;
