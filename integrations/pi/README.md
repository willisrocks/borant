# Borant extension for Pi and omp

`borant.ts` connects a Pi or omp session to the Borant host daemon. It
registers the session, reports turn events, wakes the agent when a message
arrives, and adds the Borant tools (`send`, `task_*`, `channel_*`, `board`,
`spawn`, and others).

It needs a running Borant host daemon. The extension finds it at
`$BORANT_HOME/host.sock` (default `~/.borant/host.sock`). If there is no
socket, the extension does nothing and adds no tools.

## Install

Copy or symlink the file into the extensions directory of each harness:

```sh
ln -s "$PWD/borant.ts" ~/.pi/agent/extensions/borant.ts
ln -s "$PWD/borant.ts" ~/.omp/agent/extensions/borant.ts
```

Or load it for one run:

```sh
pi -e path/to/borant.ts
```

Set `BORANT_AGENT` and `BORANT_CHANNEL` to choose the agent ID and the first
channel at registration.

## Develop

```sh
bun install
bun run check   # tsc --noEmit
bun test
```

At runtime the file imports only node built-ins and `typebox`. Pi and omp
each supply `typebox` to extensions. The Pi package is a type-only import.
