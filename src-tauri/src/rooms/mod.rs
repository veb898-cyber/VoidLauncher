//! Room networking ("комнаты друзей") backed by Tailscale Machine Sharing.
//!
//! Module layout (see docs/rooms-integration-plan.md):
//!   * `tailscale`          — TailscaleManager: detect / install / login / status
//!   * `room_state`         — RoomStateManager: create / join / leave, RoomID, persistence
//!   * `peer_discovery`     — parsing of `tailscale status --json` and host resolution
//!   * `void_link`          — launcher↔launcher control bridge over the tailnet
//!   * `minecraft_connector`— LAN-port detection and join-argument building
//!
//! Security principles:
//!   * No Tailscale auth keys are ever stored by the launcher — login is an
//!     interactive device flow (`tailscale up` + browser approval).
//!   * Minecraft traffic never crosses a backend: the tailnet carries it
//!     directly between the guest client and the host's game process.
//!   * The VoidLink bridge only exchanges small control payloads (room id,
//!     Minecraft port). It is guarded by the room code.

pub mod minecraft_connector;
pub mod peer_discovery;
pub mod room_state;
pub mod tailscale;
pub mod void_link;

#[cfg(test)]
mod e2e;

pub use peer_discovery::HostInfo;

/// Data directory for room state files (rooms.json, MSI cache).
pub fn rooms_dir(data_dir: &std::path::Path) -> std::path::PathBuf {
    data_dir.join("rooms")
}