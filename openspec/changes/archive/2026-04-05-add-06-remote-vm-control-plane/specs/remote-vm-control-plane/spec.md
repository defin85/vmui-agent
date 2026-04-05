## ADDED Requirements

### Requirement: SSH-first remote administration

The system SHALL define SSH as the default remote administration path from the Linux/WSL host to the dedicated Windows VM.

#### Scenario: Host reaches the current Windows VM target

- **WHEN** an operator or Codex session needs to connect to the Windows VM for deploy or test work
- **THEN** the repository documents the current target endpoint and the SSH-based access pattern
- **AND** the workflow supports non-interactive key-based access from Linux/WSL

### Requirement: Separate service-plane and interactive-plane execution

The system SHALL keep remote administration and GUI-capable execution as separate planes.

#### Scenario: Operator restarts the daemon for desktop automation

- **WHEN** a remote operator triggers daemon restart or GUI smoke execution
- **THEN** the action is bridged into a logged-on interactive Windows user session
- **AND** the workflow does not depend on Session 0 or a plain SSH service session to access the desktop

### Requirement: Loopback-only daemon exposure by default

The system SHALL keep the daemon on a VM-local endpoint by default and let the host attach through a tunnel.

#### Scenario: Host attaches local MCP tooling to the daemon

- **WHEN** the Linux/WSL host needs to talk to `vmui-agent`
- **THEN** the default path uses an SSH local forward to the daemon loopback endpoint
- **AND** the daemon does not need to listen on a LAN-reachable address by default

### Requirement: Repeatable deploy and test workflow

The system SHALL define a repeatable remote workflow for source sync, build, daemon restart, smoke execution, and artifact retrieval.

#### Scenario: Operator deploys the current workspace to the VM

- **WHEN** an operator or Codex session needs to validate the current workspace on Windows
- **THEN** the repository defines the expected sequence for sync, build, restart, smoke, and artifact pull
- **AND** that sequence is suitable for future repo-tracked helper scripts

### Requirement: Repo-visible operator context

The system SHALL keep the current VM context and remote workflow in agent-visible repository docs.

#### Scenario: New Codex session starts

- **WHEN** a new session begins and needs remote deploy/test context
- **THEN** root-level agent instructions or start-here docs point to the current VM target and remote workflow document
- **AND** the documentation makes clear which parts are only planned and which parts are already automated
