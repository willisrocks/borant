// The fixture board: the approved mockup of 2026-10-27, in the BoardView shape
// of the sagas spec. `BORANT_COS_BOARD=fixture` draws this, so a chief session
// without a hub still shows a board. It is a code module and not board.json
// because a mod and its tests import only .js, .ts and their variants.

export const board = {
  "at": "2026-10-26T14:32:00Z",
  "collections": [
    {
      "collection": "personal",
      "projects": [
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
              "done": 2,
              "total": 5,
              "epics": [
                {
                  "channel": "personal-borant-golden-image",
                  "task": null,
                  "title": "Golden image",
                  "status": "closed",
                  "close_reason": "done",
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
                  "channel": "personal-borant-hub-join",
                  "task": null,
                  "title": "Hub join for pool boxes",
                  "status": "closed",
                  "close_reason": "done",
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
                },
                {
                  "channel": "personal-borant-hub-backup",
                  "task": null,
                  "title": "Hub backup to object storage",
                  "status": "needs_human",
                  "issue": null,
                  "branch": null,
                  "pr_url": null,
                  "counts": {
                    "open": 3,
                    "ready": 1,
                    "running": 0,
                    "blocked": 1,
                    "stuck": 0,
                    "needs_human": 1,
                    "done": 0
                  }
                },
                {
                  "channel": "personal-borant-delete-borant-2",
                  "task": null,
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
                  "channel": "personal-borant-hub-move",
                  "task": null,
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
                }
              ]
            }
          ],
          "epics": [],
          "tasks": {
            "open": 3,
            "ready": 0,
            "running": 0,
            "blocked": 2,
            "stuck": 0,
            "needs_human": 1,
            "done": 12
          }
        },
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
              "task": "01M5ZCS35Z2RWKFF0BCYQWJYV0",
              "title": "Move production to Fly.io",
              "status": "closed",
              "close_reason": "done",
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
        }
      ],
      "needs_you": [
        {
          "project": "borant",
          "channel": "personal-borant-hub-backup",
          "task": "01M5ZCS35Z2RWKFF0BCYQWJYW1",
          "title": "Create Hetzner bucket borant-hub-backup + 1Password item",
          "status": "needs_human",
          "attention_msg": "blocks 2 epics",
          "since": "2026-10-26T13:50:00Z",
          "gate": true
        },
        {
          "project": "borant",
          "channel": "personal-borant",
          "task": "01M5ZCS35Z2RWKFF0BCYQWJYW2",
          "title": "Revoke old willisrocks gh token?",
          "status": "needs_human",
          "attention_msg": "optional",
          "since": "2026-10-26T14:20:00Z",
          "gate": false
        }
      ]
    }
  ]
}

export default board
