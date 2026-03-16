use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::engine::state_machine::{ScreenId, UserInput};

pub fn map_key_event(key: KeyEvent, screen: &ScreenId, selected_index: usize) -> Option<UserInput> {
    // Global keys
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        return Some(UserInput::Quit);
    }

    match key.code {
        KeyCode::Esc => Some(UserInput::Back),
        KeyCode::Enter => map_enter(screen, selected_index),
        KeyCode::Char('q') => Some(UserInput::Quit),
        KeyCode::Char('r') if matches!(screen, ScreenId::WifiSelect) => {
            Some(UserInput::RefreshWifiScan)
        }
        KeyCode::Char('a') => Some(UserInput::AbortInstall),
        _ => None,
    }
}

fn map_enter(screen: &ScreenId, selected_index: usize) -> Option<UserInput> {
    match screen {
        ScreenId::NetworkConfig => Some(UserInput::Select(selected_index)),
        ScreenId::WifiSelect => Some(UserInput::SelectWifiNetwork(selected_index)),
        ScreenId::WifiPassword => None, // handled by text input widget
        ScreenId::NetworkProgress => Some(UserInput::Confirm),
        ScreenId::DiskGroupSelect => Some(UserInput::SelectDiskGroup(selected_index)),
        ScreenId::FilesystemSelect => Some(UserInput::SelectFilesystem(selected_index)),
        ScreenId::RaidConfig => Some(UserInput::SelectRaidOption(selected_index)),
        ScreenId::Confirm => Some(UserInput::ConfirmInstall),
        ScreenId::InstallProgress => Some(UserInput::Confirm),
        ScreenId::Reboot => Some(UserInput::RebootSystem),
    }
}
