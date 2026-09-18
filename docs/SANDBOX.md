# Sandbox boundary

Reactor validates workspace scope and passes a policy to the selected agent. The agent is responsible for enforcing file, network, and process isolation.

## Reactor responsibilities

- Store canonical workspace and additional writable paths.
- Reject overlapping or invalid workspace paths.
- Detect policy changes before execution.
- Check that the selected agent supports required capabilities.
- Start the agent in non-interactive mode with the configured sandbox options.
- Record the invocation, exit state, logs, and workspace diff.

## Agent responsibilities

- Enforce read and write path restrictions.
- Apply network, MCP, hook, and command policies supported by that agent.
- Use existing Git, SSH, API, and CLI credentials without exposing them to Reactor.
- Reject operations outside the granted policy.

## Outside Reactor

Reactor does not store, refresh, or broker credentials. It does not implement an OS-level sandbox or attempt to contain a compromised agent. For stronger isolation, run Reactor in a container or restricted operating-system account and mount only the required workspaces and credentials.

Additional paths should be granted only when a plan needs them. Work that requires unavailable capabilities should remain blocked or be moved to a later schedule after the workspace policy is updated and prepared again.
