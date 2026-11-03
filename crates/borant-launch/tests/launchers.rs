//! The launcher contract: `focus` opens a window in the person's launcher, or
//! says "not supported" so the caller prints the attach command to copy (mod
//! actions spec, "Go to an agent").

use borant_core::ports::Launcher;
use borant_core::{AgentId, testing};
use borant_launch::{HerdrConfig, HerdrLauncher, OrcaLauncher, ProcessLauncher, ZellijLauncher};

fn agent() -> AgentId {
    "w1".parse().unwrap()
}

/// The launchers with no window of the person's own: each refuses `focus`
/// with the port's exact "not supported", which the CLI turns into the attach
/// command.
#[tokio::test]
async fn launchers_without_a_window_refuse_focus() {
    let dir = tempfile::tempdir().unwrap();
    let launchers: Vec<Box<dyn Launcher>> = vec![
        Box::new(ProcessLauncher::new(dir.path().to_path_buf())),
        Box::new(ZellijLauncher::new(dir.path().to_path_buf())),
        Box::new(OrcaLauncher),
        Box::new(HerdrLauncher::new(HerdrConfig::default())),
    ];
    for launcher in launchers {
        testing::launcher_focus_contract(&*launcher, &agent(), false).await;
    }
}
