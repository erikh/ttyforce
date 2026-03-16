use ratatui::prelude::*;
use ratatui::widgets::*;

use crate::engine::state_machine::InstallerStateMachine;

use super::Screen;

pub struct DiskSelectScreen {
    pub selected_index: usize,
}

impl DiskSelectScreen {
    pub fn new() -> Self {
        Self { selected_index: 0 }
    }
}

impl Default for DiskSelectScreen {
    fn default() -> Self {
        Self::new()
    }
}

impl Screen for DiskSelectScreen {
    fn title(&self) -> &str {
        "Select Disk Group"
    }

    fn render(&self, f: &mut Frame, state: &InstallerStateMachine) {
        let area = f.area();

        let outer = Block::default()
            .title(" Town OS Installer — Disk Group Selection ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan));
        f.render_widget(outer, area);

        let inner = area.inner(Margin { horizontal: 2, vertical: 1 });

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),  // intro
                Constraint::Min(5),     // group list
                Constraint::Length(5),  // detail panel
                Constraint::Length(3),  // hints
            ])
            .split(inner);

        // --- Intro ---
        let intro = Paragraph::new(
            "Disks have been grouped by make and model. Select the group you want to install Town OS onto.\n\
             All disks in the selected group will be used.",
        )
        .style(Style::default().fg(Color::White))
        .wrap(Wrap { trim: true });
        f.render_widget(intro, chunks[0]);

        // --- Group list ---
        let items: Vec<ListItem> = if state.disk_groups.is_empty() {
            vec![ListItem::new("  No disks found on this system.")
                .style(Style::default().fg(Color::Red))]
        } else {
            state
                .disk_groups
                .iter()
                .enumerate()
                .map(|(i, group)| {
                    let count = group.disk_count();
                    let total = group.total_human();
                    let label = format!(
                        "  {} {}   ×{}   {}",
                        group.make, group.model, count, total
                    );

                    let style = if i == self.selected_index {
                        Style::default()
                            .fg(Color::Black)
                            .bg(Color::Cyan)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(Color::White)
                    };

                    ListItem::new(label).style(style)
                })
                .collect()
        };

        let list = List::new(items)
            .block(
                Block::default()
                    .title(" Available Disk Groups ")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Blue)),
            )
            .highlight_style(
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            );
        f.render_widget(list, chunks[1]);

        // --- Detail panel for selected group ---
        let detail_text = if let Some(group) = state.disk_groups.get(self.selected_index) {
            let disk_lines: Vec<String> = group
                .disks
                .iter()
                .map(|d| {
                    let serial = d
                        .serial
                        .as_deref()
                        .map(|s| format!(" (s/n: {})", s))
                        .unwrap_or_default();
                    format!("  {}  {}{}", d.device, d.size_human(), serial)
                })
                .collect();
            format!(
                "{} {}  ×{}  {}\n{}",
                group.make,
                group.model,
                group.disk_count(),
                group.total_human(),
                disk_lines.join("\n")
            )
        } else {
            "No group selected.".to_string()
        };

        let detail = Paragraph::new(detail_text)
            .block(
                Block::default()
                    .title(" Selected Group Details ")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::DarkGray)),
            )
            .style(Style::default().fg(Color::White))
            .wrap(Wrap { trim: true });
        f.render_widget(detail, chunks[2]);

        // --- Error or hints ---
        if let Some(err) = &state.error_message {
            let err_widget = Paragraph::new(err.as_str())
                .style(Style::default().fg(Color::Red).add_modifier(Modifier::BOLD))
                .wrap(Wrap { trim: true });
            f.render_widget(err_widget, chunks[3]);
        } else {
            let hints = Paragraph::new(
                "Enter/Space: select group  ↑/↓: move  q: quit",
            )
            .style(Style::default().fg(Color::DarkGray));
            f.render_widget(hints, chunks[3]);
        }
    }
}
