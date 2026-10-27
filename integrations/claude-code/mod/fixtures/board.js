// The fixture board: the approved mockup of 2026-10-27, in the BoardView shape
// of the sagas spec. `BORANT_COS_BOARD=fixture` draws this, so a chief session
// without a hub still shows a board. It is a code module and not board.json
// because a mod and its tests import only .js, .ts and their variants.
//
// Do not edit by hand: it is the output of `borant_core::board::build` for the
// records in crates/borant-core/tests/board_fixture.rs, so it is exactly what
// `borant board --json` returns. Regenerate it with
// `BORANT_BOARD_FIXTURE=write cargo test -p borant-core --test board_fixture`.

export const board = {
  "at": 1793025120000,
  "collections": [
    {
      "collection": "personal",
      "projects": [
        {
          "project": "boopmark",
          "mirror_issues": false,
          "orchestrators": [
            {
              "agent": "orch-boopmark-2",
              "online": true,
              "busy": false,
              "host": null
            }
          ],
          "sagas": [],
          "epics": [
            {
              "channel": "personal-boopmark",
              "task": "01M5ZG00000000000000000034",
              "title": "Move production to Fly.io",
              "status": "closed",
              "issue": null,
              "branch": null,
              "pr_url": null,
              "counts": {
                "open": 0,
                "ready": 0,
                "running": 0,
                "blocked": 0,
                "stuck": 0,
                "needs_human": 0,
                "done": 1
              }
            }
          ],
          "tasks": {
            "open": 0,
            "ready": 0,
            "running": 0,
            "blocked": 0,
            "stuck": 0,
            "needs_human": 0,
            "done": 1
          }
        },
        {
          "project": "borant",
          "mirror_issues": true,
          "orchestrators": [
            {
              "agent": "orchestrator",
              "online": true,
              "busy": false,
              "host": "macbook"
            }
          ],
          "sagas": [
            {
              "channel": "personal-borant",
              "title": "Elastic compute",
              "issue": null,
              "state": "approved",
              "gate": "01M5ZG00000000000000000001",
              "epics": [
                {
                  "channel": "personal-borant",
                  "task": "01M5ZG00000000000000000004",
                  "title": "Hub backup to object storage",
                  "status": "in_progress",
                  "issue": null,
                  "branch": null,
                  "pr_url": null,
                  "counts": {
                    "open": 3,
                    "ready": 2,
                    "running": 0,
                    "blocked": 1,
                    "stuck": 0,
                    "needs_human": 2,
                    "done": 0
                  }
                },
                {
                  "channel": "personal-borant",
                  "task": "01M5ZG00000000000000000005",
                  "title": "Delete borant-2",
                  "status": "open",
                  "issue": null,
                  "branch": null,
                  "pr_url": null,
                  "counts": {
                    "open": 1,
                    "ready": 0,
                    "running": 0,
                    "blocked": 1,
                    "stuck": 0,
                    "needs_human": 0,
                    "done": 0
                  }
                },
                {
                  "channel": "personal-borant",
                  "task": "01M5ZG00000000000000000006",
                  "title": "Hub move to borant-hub",
                  "status": "open",
                  "issue": null,
                  "branch": null,
                  "pr_url": null,
                  "counts": {
                    "open": 1,
                    "ready": 1,
                    "running": 0,
                    "blocked": 0,
                    "stuck": 0,
                    "needs_human": 0,
                    "done": 0
                  }
                },
                {
                  "channel": "personal-borant",
                  "task": "01M5ZG00000000000000000002",
                  "title": "Golden image",
                  "status": "closed",
                  "issue": null,
                  "branch": null,
                  "pr_url": null,
                  "counts": {
                    "open": 0,
                    "ready": 0,
                    "running": 0,
                    "blocked": 0,
                    "stuck": 0,
                    "needs_human": 0,
                    "done": 3
                  }
                },
                {
                  "channel": "personal-borant",
                  "task": "01M5ZG00000000000000000003",
                  "title": "Hub join for pool boxes",
                  "status": "closed",
                  "issue": null,
                  "branch": null,
                  "pr_url": null,
                  "counts": {
                    "open": 0,
                    "ready": 0,
                    "running": 0,
                    "blocked": 0,
                    "stuck": 0,
                    "needs_human": 0,
                    "done": 2
                  }
                }
              ],
              "done": 2,
              "total": 5
            }
          ],
          "epics": [],
          "tasks": {
            "open": 3,
            "ready": 1,
            "running": 0,
            "blocked": 2,
            "stuck": 0,
            "needs_human": 0,
            "done": 12
          }
        }
      ],
      "needs_you": [
        {
          "project": "borant",
          "channel": "personal-borant",
          "task": "01M5ZG00000000000000000015",
          "title": "Create Hetzner bucket borant-hub-backup + 1Password item",
          "status": "needs_human",
          "attention_msg": "blocks 2 epics",
          "since": 1793022600000,
          "gate": false
        },
        {
          "project": "borant",
          "channel": "personal-borant",
          "task": "01M5ZG00000000000000000016",
          "title": "Revoke old willisrocks gh token?",
          "status": "needs_human",
          "attention_msg": "optional",
          "since": 1793024400000,
          "gate": false
        }
      ]
    }
  ]
}

export default board
