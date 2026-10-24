<!-- An example of release notes for a made-up v0.2.0. Copy its shape and its tone, not its content. -->

Borant v0.2.0 adds harness changes for Codex, makes takeovers faster, and fixes two message-delivery bugs. Upgrade the hub and every host together, because the hub protocol changed.

## Upgrade notes

- Upgrade the hub and all host daemons to v0.2.0 at the same time. A v0.1.x host cannot register with a v0.2.0 hub. (#42)
- `host.toml` has a new key, `grace_secs`. The default stays 120 seconds, so no change is necessary.

## New

- Codex sessions can join channels and get messages while idle, through `codex queue`. (#38)
- `borant status` shows the lease time left for each agent. (#40)

## Fixes

- A broadcast that arrived during the last tool call of a turn is no longer lost. (#41)
- `borant attach a` no longer attaches to the session of agent `a1`. (#43)

## Maintenance

- CI runs the Docker scenarios on every pull request. (#39)
