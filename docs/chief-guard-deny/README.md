# Chief guard: deny, not ask

A real chief session (Claude Code 2.1.310, Sonnet 5.5, `Bash(*)` pre-allowed,
default permission mode) with the branch plugin and the fixture board:

- `printf hi > /tmp/guard-ok.txt` runs: a redirect into a temporary folder is
  no project file.
- `npm --version` is refused by the plugin, with no dialog for the person; the
  model reads "Chief guard: refused, …" and does not reword the command.

![deny and /tmp](02-deny-and-tmp.png)
