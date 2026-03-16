use ratatui::prelude::*;
use ratatui::widgets::*;

use crate::engine::state_machine::InstallerStateMachine;

use super::Screen;

pub struct ConfirmScreen {
    pub selected_index: usize,
}

impl ConfirmScreen {
    pub fn new() -> Self {
        Self { selected_index: 0 }
    }
}

impl Default for ConfirmScreen {
    fn default() -> Self {
        Self::new()
    }
}

impl Screen for ConfirmScreen {
    fn title(&self) -> &str {
        "Confirm Installation"
    }

    fn render(&self, f: &mut Frame, state: &InstallerStateMachine) {
        let area = f.area();

        let outer = Block::default()
            .title(" Town OS Installer — Confirm Installation ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan));
        f.render_widget(outer, area);

        let inner = area.inner(Margin { horizontal: 2, vertical: 1 });

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(2),   // warning header
                Constraint::Min(10),     // summary table
                Constraint::Length(3),   // confirm buttons
                Constraint::Length(2),   // hints
            ])
            .split(inner);

        // --- Warning ---
        let warning = Paragraph::new(
            "WARNING: This will ERASE all data on the selected disks. Review your selections carefully.",
        )
        .style(
            Style::default()
                .fg(Color::Red)
                .add_modifier(Modifier::BOLD),
        )
        .wrap(Wrap { trim: true });
        f.render_widget(warning, chunks[0]);

        // --- Summary table ---
        let mut lines: Vec<Line> = vec![Line::from("")];

        // Network
        let iface = state
            .selected_interface
            .as_deref()
            .unwrap_or("<none>");
        let ssid_part = state
            .selected_ssid
            .as_deref()
            .map(|s| format!(" ({})", s))
            .unwrap_or_default();
        let net_status = if state.network_state.is_online() {
            "Online"
        } else {
            "Not online"
        };
        let ip_part = state
            .interfaces
            .iter()
            .find(|i| Some(&i.name) == state.selected_interface.as_ref())
            .and_then(|i| i.ip_address.clone())
            .map(|ip| format!("  IP: {}", ip))
            .unwrap_or_default();

        lines.push(Line::from(vec![
            Span::styled("  Network:     ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("{}{} — {}{}",iface, ssid_part, net_status, ip_part),
                Style::default().fg(Color::White),
            ),
        ]));

        lines.push(Line::from(""));

        // Disk group
        let disk_summary = if let Some(idx) = state.selected_disk_group {
            if let Some(group) = state.disk_groups.get(idx) {
                group.display_name()
            } else {
                "<invalid selection>".to_string()
            }
        } else {
            "<no disk group selected>".to_string()
        };

        lines.push(Line::from(vec![
            Span::styled("  Disk group:  ", Style::default().fg(Color::DarkGray)),
            Span::styled(disk_summary, Style::default().fg(Color::White)),
        ]));

        // Individual disks
        if let Some(idx) = state.selected_disk_group {
            if let Some(group) = state.disk_groups.get(idx) {
                for disk in &group.disks {
                    lines.push(Line::from(vec![
                        Span::styled("               ", Style::default()),
                        Span::styled(
                            format!("{} — {}", disk.device, disk.size_human()),
                            Style::default().fg(Color::DarkGray),
                        ),
                    ]));
                }
            }
        }

        lines.push(Line::from(""));

        // Filesystem
        lines.push(Line::from(vec![
            Span::styled("  Filesystem:  ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                state.selected_filesystem.display_name(),
                Style::default().fg(Color::White),
            ),
        ]));

        // RAID
        let raid_name = state
            .selected_raid
            .as_ref()
            .map(|r| r.display_name().to_string())
            .unwrap_or_else(|| "<none>".to_string());
        let usable_str = if let (Some(raid), Some(idx)) =
            (&state.selected_raid, state.selected_disk_group)
        {
            if let Some(group) = state.disk_groups.get(idx) {
                let usable = raid.usable_capacity(group.total_bytes(), group.disk_count());
                let gb = usable as f64 / 1_073_741_824.0;
                if gb >= 1024.0 {
                    format!("  ({:.1} TB usable)", gb / 1024.0)
                } else {
                    format!("  ({:.1} GB usable)", gb)
                }
            } else {
                String::new()
            }
        } else {
            String::new()
        };

        lines.push(Line::from(vec![
            Span::styled("  RAID:        ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("{}{}", raid_name, usable_str),
                Style::default().fg(Color::White),
            ),
        ]));

        let summary = Paragraph::new(lines)
            .block(
                Block::default()
                    .title(" Installation Summary ")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Blue)),
            )
            .wrap(Wrap { trim: false });
        f.render_widget(summary, chunks[1]);

        // --- Confirm/abort buttons ---
        let button_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(chunks[2]);

        let install_style = if self.selected_index == 0 {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Green)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Green)
        };
        let abort_style = if self.selected_index == 1 {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Red)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Red)
        };

        let install_btn = Paragraph::new("  [ Install Town OS ]  ")
            .style(install_style)
            .alignment(Alignment::Center)
            .block(Block::default().borders(Borders::ALL));
        let abort_btn = Paragraph::new("  [ Abort / Cancel ]  ")
            .style(abort_style)
            .alignment(Alignment::Center)
            .block(Block::default().borders(Borders::ALL));

        f.render_widget(install_btn, button_chunks[0]);
        f.render_widget(abort_btn, button_chunks[1]);

        // --- Hints ---
        if let Some(err) = &state.error_message {
            let err_widget = Paragraph::new(err.as_str())
                .style(Style::default().fg(Color::Red).add_modifier(Modifier::BOLD));
            f.render_widget(err_widget, chunks[3]);
        } else {
            let hints = Paragraph::new(
                "Tab/←/→: switch button  Enter: confirm selection  Esc: back  q: quit",
            )
            .style(Style::default().fg(Color::DarkGray));
            f.render_widget(hints, chunks[3]);
        }
    }
}
