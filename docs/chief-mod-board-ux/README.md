# Chief mod board UX captures

The before/after renders for the column, selection, and pane-focus changes.
Each image is a real Claude Code v2.1.310 session with the plugin from this
branch (`after-*`) or from `main` (`before-*`), in tmux at 200x50 and 120x40,
driven with `/cos`, `ctrl+x tab`, `tab`, `down`, `enter`, and `j`.

| State | What it shows |
|---|---|
| `-band` | The band with no focus: the project and saga columns. |
| `-band-focused` | `ctrl+x tab`, `tab`, `down`: the whole selected row on its background. |
| `-pane-opened` | `enter` on the row: the project pane, with the keyboard in it. |
| `-pane-moved` | `j`: the selection moved through the pane's epics. |

The names are long on purpose (`roger-docs`, `ROG-4199: LiteLLM proxy
rollout`), because the fixture's short names hid the cut. The board came from
a fake `borant board --json` on `PATH` built from
`integrations/claude-code/mod/fixtures/board.js` with those two names changed;
the session ran with `BORANT_COS_BOARD=cli`.

Regenerate both sets with:

```sh
BORANT_COS_BOARD=cli PATH=/tmp/cos/fake:$PATH \
  nix shell nixpkgs#charm-freeze --command \
  scripts/cos-visual.sh integrations/claude-code docs/chief-mod-board-ux/captures after
```

and the same command with the `main` worktree's plugin directory, prefix
`before`.
