pub mod confirm;
pub mod disk_select;
pub mod filesystem;
pub mod network;
pub mod network_progress;
pub mod raid_config;
pub mod reboot;
pub mod wifi_password;
pub mod wifi_select;


use ratatui::Frame;

use crate::engine::state_machine::InstallerStateMachine;

pub trait Screen {
    fn render(&self, f: &mut Frame, state: &InstallerStateMachine);
    fn title(&self) -> &str;
}
