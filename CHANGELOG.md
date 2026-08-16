# Changelog

Short, user-facing notes for every Zeron release.

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
