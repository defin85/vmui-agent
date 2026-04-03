# Change: Add Windows UI observer backend

## Why

After the daemon contract exists, the next blocking gap is the lack of live Windows UI observation. Without a Windows observer backend, the daemon cannot produce real snapshots, diffs, or locator-backed state for the VM desktop.

## What Changes

- Implement the first Windows backend observer that runs inside the interactive VM session.
- Use UI Automation as the primary source of tree and property data, with WinEvent and MSAA/IAccessible as fallback inputs.
- Normalize backend events into targeted refreshes and diff batches for the daemon state cache.
- Add session-stable window/element identity plus reusable locators instead of raw ordinal-only paths.
- Record backend provenance and confidence so later action and 1C diagnostic layers know when fallback behavior is in play.

## Impact

- Depends on: `add-01-daemon-session-foundation`
- Affected specs: `windows-ui-observer`
- Affected code: `crates/vmui-platform-windows`, `crates/vmui-platform`, `crates/vmui-core`, `crates/vmui-agent`
