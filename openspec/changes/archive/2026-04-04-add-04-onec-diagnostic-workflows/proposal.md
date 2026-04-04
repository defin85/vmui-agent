# Change: Add 1C diagnostic workflows

## Why

The core daemon, Windows observer, and semantic actions are still generic. The repository exists for 1C workflows, so it needs first-class operating modes, locator profiles, and post-failure diagnostics tailored to 1C application UI and Configurator.

## What Changes

- Add explicit `enterprise_ui` and `configurator` operating modes with process/window filtering.
- Define the post-failure workflow for standard 1C automated test crashes: open the relevant context, collect state, diff against baseline, and emit diagnostic artifacts.
- Add 1C-aware locator profiles and fallback annotations for ordinary forms and Configurator surfaces.
- Define how the daemon cooperates with standard 1C automated testing rather than replacing it.

## Impact

- Depends on: `add-01-daemon-session-foundation`, `add-02-windows-ui-observer`, `add-03-semantic-actions-and-artifacts`
- Affected specs: `onec-diagnostic-workflows`
- Affected code: `crates/vmui-agent`, `crates/vmui-platform-windows`, `crates/vmui-core`, 1C-specific configuration and profile assets
