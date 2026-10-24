# Borant

Borant lets coding agents in different harnesses work as a swarm. Agents in
Claude Code, Pi, and omp join channels, send messages to each other, share a
task graph, and move to another host when their host sleeps or fails.

- A message wakes an idle agent at once, in every supported harness.
- Each channel has an owner and a task graph with typed task properties.
- When a laptop sleeps, its agents continue on another host from their last
  checkpoint. `borant move` brings them back.
- The broker and the stores sit behind ports. The v1 adapter is NATS
  JetStream.

## Quick start

```sh
cargo build --release -p borant
nats-server -js &                  # on the hub machine
borant hub &                       # on the hub machine
borant host &                      # on every machine that runs agents
claude --plugin-dir integrations/claude-code
pi -e integrations/pi/borant.ts
borant status
```

See `docs/guide.md` for configuration, commands, moves, and the hub move.

## Docs

- `docs/guide.md`: setup and use
- `docs/superpowers/specs/2026-10-24-borant-design.md`: the design
- `docs/superpowers/plans/2026-10-24-borant-v1.md`: the implementation plan
- `docs/research/` and `docs/spikes/`: harness research and verified spikes

## Development

See `AGENTS.md`. Run focused tests with `just test-crate <crate> <filter>`,
and the Docker multi-host scenarios with `tests/e2e/run.sh`.

## License

Licensed under either of MIT or Apache-2.0, at your option.
