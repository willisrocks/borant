// The fixture channels: what `borant channel list` prints for the channels of
// mod/fixtures/tasks.js, in the shape of borant_core::model::Channel (only the
// fields the mod reads). `BORANT_COS_BOARD=fixture` reads this, so the mod
// knows which project each channel belongs to, as it does from a live hub.

export const channels = [
  { name: 'personal-borant', kind: 'saga', project: 'borant', collection: 'personal', owner: 'orchestrator', closed: null },
  { name: 'personal-borant-tasks', kind: 'project', project: 'borant', collection: 'personal', owner: 'orchestrator', closed: null },
  { name: 'personal-boopmark', kind: 'epic', project: 'boopmark', collection: 'personal', owner: 'orch-boopmark-2', closed: null },
  { name: 'personal-boopmark-tasks', kind: 'project', project: 'boopmark', collection: 'personal', owner: 'orch-boopmark-2', closed: null },
  { name: 'personal-notes-tasks', kind: 'project', project: 'notes', collection: 'personal', owner: 'orch-notes', closed: null },
]

export default channels
