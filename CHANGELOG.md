# Changelog

Short, user-facing notes for every Zeron release.

## [0.2.10] - 2026-08-17

### Changed

- Each Zeron app now owns its local runtime while controlling other devices through the private relay; Devices shows one shared app version.

### Fixed

- macOS updates restart the bundled background runtime before reopening the GUI, preventing GUI/runtime version drift.
- Superseded device identities can merge into the current runtime without losing projects or chats.
- The PC tunnel no longer replaces the Mac runtime's local port, keeping local and remote runtimes independently available.

## [0.2.9] - 2026-08-17

### Added

- Codex commentary and genuine adapter warnings now render as distinct transcript activity instead of ordinary assistant prose.

### Fixed

- The private local edge is supervised by macOS and recovers automatically after crashes, while the reverse SSH tunnel reconnects the PC runtime without manual Wrangler startup.
- The Mac runtime stays usable through its local edge when the PC runtime is unavailable.
- Markdown callouts now round and clip all four corners, including the accented left edge.

## [0.2.8] - 2026-08-16

### Fixed

- Remote runtime connections now rebuild automatically after an engine or SSH tunnel restart instead of leaving the app on stale loading and retry loops.
- Localhost WebSocket probes fail faster while a remote tunnel is still starting.

### Changed

- Offline device choices now say “Runtime offline” so engine status cannot be mistaken for the Mac’s Wi-Fi status.

## [0.2.7] - 2026-08-16

### Fixed

- Every build now checks Zeron’s GitHub release channel instead of falling back to the upstream private sync server.
- The Devices page marks the connected runtime online without requiring cloud presence and displays its live runtime version.

### Changed

- Device rows now say “Connected runtime” and label versions explicitly as runtime versions.

## [0.2.6] - 2026-08-16

### Added

- Completion notifications can include a concise final-response preview for local and remote runs, with a privacy toggle in Settings.

### Changed

- Zeron now checks for updates immediately after launch instead of waiting 20 seconds.
- The local-only account label now reads “Private workspace” instead of “Local development runtime.”
