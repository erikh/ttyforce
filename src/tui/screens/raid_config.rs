use ratatui::prelude::*;
use ratatui::widgets::*;

use crate::disk::RaidConfig;
use crate::engine::state_machine::InstallerStateMachine;

use super::Screen;

pub struct RaidConfigScreen {
    pub selected_index: usize,
}

impl RaidConfigScreen {
    pub fn new() -> Self {
        Self { selected_index: 0 }
    }
}

impl Default for RaidConfigScreen {
    fn default() -> Self {
        Self::new()
    }
}

impl Screen for RaidConfigScreen {
    fn title(&self) -> &str {
        "RAID Configuration"
    }

    fn render(&self, f: &mut Frame, state: &InstallerStateMachine) {
        let area = f.area();

        let outer = Block::default()
            .title(" Town OS Installer — RAID Configuration ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan));
        f.render_widget(outer, area);

        let inner = area.inner(Margin { horizontal: 2, vertical: 1 });

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),  // context: disk group + fs
                Constraint::Min(4),     // option list
                Constraint::Length(6),  // description panel
                Constraint::Length(3),  // hints
            ])
            .split(inner);

        // --- Context header ---
        let (group_summary, disk_count, total_bytes) =
            if let Some(idx) = state.selected_disk_group {
                if let Some(group) = state.disk_groups.get(idx) {
                    (
                        format!(
                            "{} {}  ×{}  {}",
                            group.make,
                            group.model,
                            group.disk_count(),
                            group.total_human()
                        ),
                        group.disk_count(),
                        group.total_bytes(),
                    )
                } else {
                    ("No group selected".to_string(), 0, 0)
                }
            } else {
                ("No group selected".to_string(), 0, 0)
            };

        let context_text = format!(
            "Disks: {}   Filesystem: {}",
            group_summary,
            state.selected_filesystem.display_name()
        );
        let context_para = Paragraph::new(context_text)
            .style(Style::default().fg(Color::Yellow))
            .wrap(Wrap { trim: true });
        f.render_widget(context_para, chunks[0]);

        // --- Option list ---
        let options = RaidConfig::for_disk_count(disk_count, &state.selected_filesystem);
        let recommended = RaidConfig::recommended_for_count(disk_count, &state.selected_filesystem);

        let items: Vec<ListItem> = options
            .iter()
            .enumerate()
            .map(|(i, opt)| {
                let is_recommended = *opt == recommended;
                let rec_tag = if is_recommended { " [Recommended]" } else { "" };
                let usable = opt.usable_capacity(total_bytes, disk_count);
                let usable_gb = usable as f64 / 1_073_741_824.0;
                let usable_str = if usable_gb >= 1024.0 {
                    format!("{:.1} TB usable", usable_gb / 1024.0)
                } else {
                    format!("{:.1} GB usable", usable_gb)
                };
                let label = format!("  {}{}   {}", opt.display_name(), rec_tag, usable_str);

                let style = if i == self.selected_index {
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
                } else if is_recommended {
                    Style::default().fg(Color::Green)
                } else {
                    Style::default().fg(Color::White)
                };

                ListItem::new(label).style(style)
            })
            .collect();

        let list = if items.is_empty() {
            let empty =
                vec![ListItem::new("  No RAID options available (no disks selected).")
                    .style(Style::default().fg(Color::DarkGray))];
            List::new(empty)
        } else {
            List::new(items)
        };

        let list = list.block(
            Block::default()
                .title(" RAID Options ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Blue)),
        );
        f.render_widget(list, chunks[1]);

        // --- Description panel ---
        let desc_text = options
            .get(self.selected_index)
            .map(|opt| opt.description())
            .unwrap_or("Select a disk group first.");

        let desc = Paragraph::new(desc_text)
            .block(
                Block::default()
                    .title(" Description ")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::DarkGray)),
            )
            .style(Style::default().fg(Color::White))
            .wrap(Wrap { trim: true });
        f.render_widget(desc, chunks[2]);

        // --- Hints ---
        if let Some(err) = &state.error_message {
            let err_widget = Paragraph::new(err.as_str())
                .style(Style::default().fg(Color::Red).add_modifier(Modifier::BOLD))
                .wrap(Wrap { trim: true });
            f.render_widget(err_widget, chunks[3]);
        } else {
            let hints = Paragraph::new(
                "Enter/Space: select  ↑/↓: move  Esc: back  q: quit",
            )
            .style(Style::default().fg(Color::DarkGray));
            f.render_widget(hints, chunks[3]);
        }
    }
}
