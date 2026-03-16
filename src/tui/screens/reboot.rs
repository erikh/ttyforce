use ratatui::prelude::*;
use ratatui::widgets::*;

use crate::engine::state_machine::InstallerStateMachine;
use crate::manifest::InstallerFinalState;

use super::Screen;

pub struct RebootScreen {
    pub selected_index: usize,
}

impl RebootScreen {
    pub fn new() -> Self {
        Self { selected_index: 0 }
    }
}

impl Default for RebootScreen {
    fn default() -> Self {
        Self::new()
    }
}

impl Screen for RebootScreen {
    fn title(&self) -> &str {
        "Reboot"
    }

    fn render(&self, f: &mut Frame, state: &InstallerStateMachine) {
        let area = f.area();

        let outer = Block::default()
            .title(" Town OS Installer — Complete ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan));
        f.render_widget(outer, area);

        let inner = area.inner(Margin { horizontal: 2, vertical: 1 });

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(6),    // status message
                Constraint::Length(5), // buttons
                Constraint::Length(2), // hints
            ])
            .split(inner);

        // --- Status message ---
        let (header, body, header_style) = match &state.action_manifest.final_state {
            InstallerFinalState::Installed => (
                "Installation Complete",
                "Town OS has been successfully installed.\n\n\
                 You may now reboot the machine to start your new system,\n\
                 or stay in the installer to review what was done.",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
            InstallerFinalState::Aborted => (
                "Installation Aborted",
                "The installation was aborted before completion.\n\n\
                 No permanent changes have been made to your system.\n\
                 You can reboot to start over or power off the machine.",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            InstallerFinalState::Error(_) => (
                "Installation Failed",
                // body is built below — we need to own it
                "",
                Style::default()
                    .fg(Color::Red)
                    .add_modifier(Modifier::BOLD),
            ),
            InstallerFinalState::Rebooted => (
                "Rebooting...",
                "The system is rebooting now.",
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            ),
            InstallerFinalState::Exited => (
                "Exiting...",
                "Returning to the shell.",
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            ),
        };

        // Handle the Error variant body separately to avoid borrow issues
        let error_body_owned: String;
        let body_str: &str = match &state.action_manifest.final_state {
            InstallerFinalState::Error(msg) => {
                error_body_owned = format!(
                    "The installation encountered an error and could not complete.\n\n\
                     Error: {}\n\n\
                     You may reboot to try again or abort to exit.",
                    msg
                );
                &error_body_owned
            }
            _ => body,
        };

        let status_lines = vec![
            Line::from(Span::styled(header, header_style)),
            Line::from(""),
            Line::from(body_str),
        ];

        let status = Paragraph::new(status_lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::DarkGray)),
            )
            .wrap(Wrap { trim: true });
        f.render_widget(status, chunks[0]);

        // --- Buttons ---
        let button_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Ratio(1, 3),
                Constraint::Ratio(1, 3),
                Constraint::Ratio(1, 3),
            ])
            .split(chunks[1]);

        let btn_style = |idx: usize, fg: Color| {
            if self.selected_index == idx {
                Style::default()
                    .fg(Color::Black)
                    .bg(fg)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(fg)
            }
        };

        let reboot_label = match &state.action_manifest.final_state {
            InstallerFinalState::Installed => "  [ Reboot Now ]  ",
            InstallerFinalState::Aborted | InstallerFinalState::Error(_) => "  [ Reboot / Retry ]  ",
            InstallerFinalState::Rebooted | InstallerFinalState::Exited => "  [ Reboot ]  ",
        };

        let reboot_btn = Paragraph::new(reboot_label)
            .style(btn_style(0, Color::Green))
            .alignment(Alignment::Center)
            .block(Block::default().borders(Borders::ALL));
        let exit_btn = Paragraph::new("  [ Exit ]  ")
            .style(btn_style(1, Color::Cyan))
            .alignment(Alignment::Center)
            .block(Block::default().borders(Borders::ALL));
        let abort_btn = Paragraph::new("  [ Power Off ]  ")
            .style(btn_style(2, Color::Yellow))
            .alignment(Alignment::Center)
            .block(Block::default().borders(Borders::ALL));

        f.render_widget(reboot_btn, button_chunks[0]);
        f.render_widget(exit_btn, button_chunks[1]);
        f.render_widget(abort_btn, button_chunks[2]);

        // --- Hints ---
        let hints = Paragraph::new(
            "Tab/←/→: switch button  Enter: confirm  q: quit",
        )
        .style(Style::default().fg(Color::DarkGray));
        f.render_widget(hints, chunks[2]);
    }
}
