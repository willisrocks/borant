//! The chief mod's board fixture is the JSON that `borant board --json` writes
//! for the mockup's records, so the mod and its tests read the shape that the
//! hub really serves. The default run fails when the file has drifted from
//! `borant_core::board::build`; `BORANT_BOARD_FIXTURE=write` rewrites it.

use std::collections::BTreeMap;

use borant_core::board::{BoardInput, build};
use borant_core::model::{
    AgentStatus, Channel, ChannelKind, CloseReason, LeaseState, Project, Task, TaskKind, TaskStatus,
};
use borant_core::rules::roles::CollectionConfig;
use borant_core::rules::saga;
use borant_core::{
    AgentId, ChannelName, CollectionName, HostName, ProjectName, SessionId, TaskId, Timestamp,
};

/// `2026-10-26T14:32:00Z`, the time the mockup shows.
const AT: Timestamp = Timestamp(1_793_025_120_000);

const HEADER: &str = "\
// The fixture board: the approved mockup of 2026-10-27, in the BoardView shape
// of the sagas spec. `BORANT_COS_BOARD=fixture` draws this, so a chief session
// without a hub still shows a board. It is a code module and not board.json
// because a mod and its tests import only .js, .ts and their variants.
//
// Do not edit by hand: it is the output of `borant_core::board::build` for the
// records in crates/borant-core/tests/board_fixture.rs, so it is exactly what
// `borant board --json` returns. Regenerate it with
// `BORANT_BOARD_FIXTURE=write cargo test -p borant-core --test board_fixture`.
";

fn col(name: &str) -> CollectionName {
    name.parse().expect("a collection name")
}

fn proj(name: &str) -> ProjectName {
    name.parse().expect("a project name")
}

fn chan(name: &str) -> ChannelName {
    name.parse().expect("a channel name")
}

fn agent(name: &str) -> AgentId {
    name.parse().expect("an agent name")
}

fn host(name: &str) -> HostName {
    name.parse().expect("a host name")
}

/// A task id that rises with the place of the task in the fixture.
fn id(n: u32) -> TaskId {
    format!("01M5ZG{n:020}").parse().expect("a task id")
}

fn channel(name: &str, kind: ChannelKind, project: &str, owner: Option<&str>) -> Channel {
    Channel {
        name: chan(name),
        owner: owner.map(agent),
        schema: serde_json::json!({}),
        created: Timestamp(1_793_000_000_000),
        closed: None,
        project: Some(proj(project)),
        branch: None,
        collection: Some(col("personal")),
        paused: false,
        kind,
        title: None,
        issue: None,
        gate: None,
        root: None,
    }
}

fn task(n: u32, title: &str, status: TaskStatus, created: i64) -> Task {
    Task {
        id: id(n),
        title: title.into(),
        body: String::new(),
        status,
        close_reason: None,
        close_message: None,
        attention_msg: None,
        claimed_by: None,
        deadline: None,
        attempts: 0,
        max_attempts: None,
        blocked_by: Vec::new(),
        parent: None,
        kind: TaskKind::Task,
        issue: None,
        properties: Default::default(),
        created: Timestamp(created),
        updated: Timestamp(created),
        create_key: None,
    }
}

fn epic(n: u32, title: &str, status: TaskStatus, created: i64) -> Task {
    Task {
        kind: TaskKind::Epic,
        ..task(n, title, status, created)
    }
}

fn child(n: u32, title: &str, parent: TaskId, status: TaskStatus) -> Task {
    Task {
        parent: Some(parent),
        ..task(n, title, status, 1_793_100_000_000)
    }
}

fn closed(mut task: Task) -> Task {
    task.status = TaskStatus::Closed;
    task.close_reason = Some(CloseReason::Done);
    task
}

fn needs_human(mut task: Task, msg: &str, since: i64) -> Task {
    task.status = TaskStatus::NeedsHuman;
    task.attention_msg = Some(msg.into());
    task.updated = Timestamp(since);
    task
}

fn agent_line(name: &str, on: Option<&str>) -> AgentStatus {
    let lease = LeaseState::Held {
        session: SessionId::new(),
        host: on.map(host).unwrap_or_else(|| host("h1")),
        expires: AT,
    };
    AgentStatus {
        agent: agent(name),
        harness: None,
        host: on.map(host),
        lease: Some(lease),
        session: None,
        busy: false,
        claim: None,
        role: None,
        collection: Some(col("personal")),
        pinned: false,
        retired: false,
        model: None,
    }
}

fn project(name: &str, mirror_issues: Option<bool>) -> Project {
    Project {
        name: proj(name),
        remote_url: format!("git@github.com:willisrocks/{name}.git"),
        default_branch: "main".into(),
        checkpoint_remote: None,
        setup: None,
        test: None,
        collection: Some(col("personal")),
        mirror_issues,
    }
}

/// The records of the approved mockup of 2026-10-27, as the hub would hold
/// them.
struct Records {
    projects: Vec<Project>,
    channels: Vec<Channel>,
    tasks: Vec<(ChannelName, Task)>,
    agents: Vec<AgentStatus>,
}

fn mockup() -> Records {
    let projects = vec![project("borant", None), project("boopmark", Some(false))];

    let mut saga = channel(
        "personal-borant",
        ChannelKind::Saga,
        "borant",
        Some("orchestrator"),
    );
    saga.title = Some("Elastic compute".into());
    saga.gate = Some(id(1));
    let mut borant_tasks = channel(
        "personal-borant-tasks",
        ChannelKind::Project,
        "borant",
        Some("orchestrator"),
    );
    borant_tasks.title = Some("Borant tasks".into());
    let mut boopmark_epic = channel(
        "personal-boopmark",
        ChannelKind::Epic,
        "boopmark",
        Some("orch-boopmark-2"),
    );
    boopmark_epic.root = Some(id(34));
    let boopmark_tasks = channel(
        "personal-boopmark-tasks",
        ChannelKind::Project,
        "boopmark",
        Some("orch-boopmark-2"),
    );
    let channels = vec![saga, borant_tasks, boopmark_epic, boopmark_tasks];

    let elastic = chan("personal-borant");
    let mut tasks: Vec<(ChannelName, Task)> = Vec::new();
    let mut put = |task: Task| tasks.push((elastic.clone(), task));

    // The gate, closed as done: the saga is approved.
    put(closed(task(
        1,
        saga::GATE_TITLE,
        TaskStatus::Open,
        1_793_010_000_000,
    )));
    // Two epics closed as done, then three that are still open, in the order
    // that the board draws them: open first, then by created.
    put(closed(epic(
        2,
        "Golden image",
        TaskStatus::Open,
        1_793_010_400_000,
    )));
    put(closed(epic(
        3,
        "Hub join for pool boxes",
        TaskStatus::Open,
        1_793_010_500_000,
    )));
    put(epic(
        4,
        "Hub backup to object storage",
        TaskStatus::Open,
        1_793_010_100_000,
    ));
    put(epic(
        5,
        "Delete borant-2",
        TaskStatus::Open,
        1_793_010_200_000,
    ));
    put(epic(
        6,
        "Hub move to borant-hub",
        TaskStatus::Open,
        1_793_010_300_000,
    ));

    for (n, title) in [
        (7, "The image"),
        (8, "The host boot"),
        (9, "The smoke test"),
    ] {
        put(closed(child(n, title, id(2), TaskStatus::Open)));
    }
    for (n, title) in [(10, "The join token"), (11, "The bootstrap")] {
        put(closed(child(n, title, id(3), TaskStatus::Open)));
    }
    put(child(12, "The bucket", id(4), TaskStatus::Open));
    put(child(13, "The restore", id(4), TaskStatus::Open));
    put(Task {
        blocked_by: vec![id(12)],
        ..child(14, "The backup job", id(4), TaskStatus::Open)
    });
    put(needs_human(
        child(
            15,
            "Create Hetzner bucket borant-hub-backup + 1Password item",
            id(4),
            TaskStatus::Open,
        ),
        "blocks 2 epics",
        1_793_022_600_000,
    ));
    put(needs_human(
        child(
            16,
            "Revoke old willisrocks gh token?",
            id(4),
            TaskStatus::Open,
        ),
        "optional",
        1_793_024_400_000,
    ));
    put(child(17, "The move plan", id(6), TaskStatus::Open));
    put(Task {
        blocked_by: vec![id(17)],
        ..child(18, "The retire of borant-2", id(5), TaskStatus::Open)
    });

    let plain = chan("personal-borant-tasks");
    tasks.push((
        plain.clone(),
        task(19, "The guide", TaskStatus::Open, 1_793_020_000_000),
    ));
    tasks.push((
        plain.clone(),
        Task {
            blocked_by: vec![id(19)],
            ..task(20, "The skills", TaskStatus::Open, 1_793_020_000_000)
        },
    ));
    tasks.push((
        plain.clone(),
        Task {
            blocked_by: vec![id(20)],
            ..task(21, "The release notes", TaskStatus::Open, 1_793_020_000_000)
        },
    ));
    for n in 22..=33 {
        tasks.push((
            plain.clone(),
            closed(task(
                n,
                &format!("The old task {n}"),
                TaskStatus::Open,
                1_792_900_000_000,
            )),
        ));
    }

    let boopmark = chan("personal-boopmark");
    tasks.push((
        boopmark.clone(),
        closed(epic(
            34,
            "Move production to Fly.io",
            TaskStatus::Open,
            1_793_010_000_000,
        )),
    ));
    tasks.push((
        boopmark.clone(),
        closed(child(35, "The fly.toml", id(34), TaskStatus::Open)),
    ));
    let boopmark_plain = chan("personal-boopmark-tasks");
    tasks.push((
        boopmark_plain,
        closed(task(
            36,
            "The old deploy",
            TaskStatus::Open,
            1_792_900_000_000,
        )),
    ));

    let agents = vec![
        agent_line("orchestrator", Some("macbook")),
        agent_line("orch-boopmark-2", None),
    ];
    Records {
        projects,
        channels,
        tasks,
        agents,
    }
}

/// The fixture file that the mod imports.
fn render() -> String {
    let collections = BTreeMap::from([(
        col("personal"),
        CollectionConfig {
            mirror_issues: true,
            ..CollectionConfig::default()
        },
    )]);
    let records = mockup();
    let view = build(
        &BoardInput {
            at: AT,
            projects: &records.projects,
            channels: &records.channels,
            tasks: &records.tasks,
            agents: &records.agents,
            collections: &collections,
        },
        &[Some(col("personal"))],
    );
    let json = serde_json::to_string_pretty(&view).expect("the board as JSON");
    format!("{HEADER}\nexport const board = {json}\n\nexport default board\n")
}

#[test]
fn fixture_is_the_board_of_the_mockup() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../integrations/claude-code/mod/fixtures/board.js");
    let want = render();
    if std::env::var("BORANT_BOARD_FIXTURE").as_deref() == Ok("write") {
        std::fs::write(&path, &want).expect("write the fixture");
        return;
    }
    let have = std::fs::read_to_string(&path).expect("read the fixture");
    assert_eq!(
        have, want,
        "the board fixture has drifted from `build`: regenerate it with \
         BORANT_BOARD_FIXTURE=write cargo test -p borant-core --test board_fixture"
    );
}
