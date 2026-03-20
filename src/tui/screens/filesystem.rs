use ratatui::prelude::*;
use ratatui::widgets::*;

use crate::disk::FilesystemType;
use crate::engine::state_machine::InstallerStateMachine;

use super::Screen;

pub struct FilesystemScreen {
    pub selected_index: usize,
}

impl FilesystemScreen {
    pub fn new() -> Self {
        Self { selected_index: 0 }
    }
}

impl Default for FilesystemScreen {
    fn default() -> Self {
        Self::new()
    }
}

struct FsOption {
    fs: FilesystemType,
    label: &'static str,
    description: &'static str,
}

fn filesystem_options() -> Vec<FsOption> {
    vec![
        FsOption {
            fs: FilesystemType::Btrfs,
            label: "Btrfs  [Recommended]",
            description: "Modern copy-on-write filesystem built into the Linux kernel.\n\
                          Supports snapshots, compression, and transparent RAID.\n\
                          Excellent tooling support. Recommended for most installations.",
        },
    ]
}

impl Screen for FilesystemScreen {
    fn title(&self) -> &str {
        "Choose Filesystem"
    }

    fn render(&self, f: &mut Frame, state: &InstallerStateMachine) {
        let area = f.area();

        let outer = Block::default()
            .title(" Town OS Installer — Filesystem Selection ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan));
        f.render_widget(outer, area);

        let inner = area.inner(Margin { horizontal: 2, vertical: 1 });

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(2),  // intro
                Constraint::Min(4),     // option list
                Constraint::Length(6),  // description panel
                Constraint::Length(3),  // hints
            ])
            .split(inner);

        // --- Intro ---
        let intro = Paragraph::new("Choose the filesystem for your installation.")
            .style(Style::default().fg(Color::White));
        f.render_widget(intro, chunks[0]);

        // --- Option list ---
        let options = filesystem_options();
        let items: Vec<ListItem> = options
            .iter()
            .enumerate()
            .map(|(i, opt)| {
                let is_current = opt.fs == state.selected_filesystem;
                let marker = if is_current { "●" } else { "○" };
                let text = format!("  {}  {}", marker, opt.label);

                let style = if i == self.selected_index {
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
                } else if is_current {
                    Style::default().fg(Color::Green)
                } else {
                    Style::default().fg(Color::White)
                };

                ListItem::new(text).style(style)
            })
            .collect();

        let list = List::new(items)
            .block(
                Block::default()
                    .title(" Filesystem Options ")
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

        // --- Description panel for highlighted option ---
        let desc_text = options
            .get(self.selected_index)
            .map(|opt| opt.description)
            .unwrap_or("");

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
