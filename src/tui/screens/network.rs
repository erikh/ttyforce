use ratatui::prelude::*;
use ratatui::widgets::*;

use crate::engine::state_machine::InstallerStateMachine;
use crate::manifest::InterfaceKind;

use super::Screen;

pub struct NetworkScreen {
    pub selected_index: usize,
}

impl NetworkScreen {
    pub fn new() -> Self {
        Self { selected_index: 0 }
    }
}

impl Default for NetworkScreen {
    fn default() -> Self {
        Self::new()
    }
}

impl Screen for NetworkScreen {
    fn title(&self) -> &str {
        "Network Configuration"
    }

    fn render(&self, f: &mut Frame, state: &InstallerStateMachine) {
        let area = f.area();

        let outer = Block::default()
            .title(" Town OS Installer — Network Configuration ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan));
        f.render_widget(outer, area);

        let inner = area.inner(Margin { horizontal: 2, vertical: 1 });

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(4),  // intro paragraph
                Constraint::Min(6),     // interface list
                Constraint::Length(3),  // hints
            ])
            .split(inner);

        // --- Intro ---
        let has_connected_eth = state.interfaces.iter().any(|i| {
            i.kind == InterfaceKind::Ethernet && i.has_link && i.has_carrier
        });

        let intro_text = if has_connected_eth {
            "A wired connection is available and ready to use.\n\
             Press Enter to connect automatically, or select an interface below to configure it manually."
        } else {
            "No wired connection was detected.\n\
             Select a network interface below to configure your connection."
        };

        let intro = Paragraph::new(intro_text)
            .style(Style::default().fg(Color::White))
            .wrap(Wrap { trim: true });
        f.render_widget(intro, chunks[0]);

        // --- Interface list ---
        let items: Vec<ListItem> = state
            .interfaces
            .iter()
            .enumerate()
            .map(|(i, iface)| {
                let kind_label = match iface.kind {
                    InterfaceKind::Ethernet => "ETH",
                    InterfaceKind::Wifi => "WIFI",
                };

                let status = if iface.has_link && iface.has_carrier {
                    "link up"
                } else if iface.has_link {
                    "no carrier"
                } else {
                    "no link"
                };

                let ip_part = iface
                    .ip_address
                    .as_deref()
                    .map(|ip| format!("  {}", ip))
                    .unwrap_or_default();

                let label = format!(
                    "[{}] {}  ({}){}",
                    kind_label, iface.name, status, ip_part
                );

                let style = if i == self.selected_index {
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
                } else if iface.has_link && iface.has_carrier {
                    Style::default().fg(Color::Green)
                } else {
                    Style::default().fg(Color::DarkGray)
                };

                ListItem::new(label).style(style)
            })
            .collect();

        let list = List::new(items)
            .block(
                Block::default()
                    .title(" Detected Interfaces ")
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

        // --- Error message if any ---
        if let Some(err) = &state.error_message {
            let err_widget = Paragraph::new(err.as_str())
                .style(Style::default().fg(Color::Red).add_modifier(Modifier::BOLD))
                .wrap(Wrap { trim: true });
            f.render_widget(err_widget, chunks[2]);
        } else {
            let hints = Paragraph::new(
                "Enter: auto-detect  ↑/↓: select interface  s: select highlighted  q: quit",
            )
            .style(Style::default().fg(Color::DarkGray));
            f.render_widget(hints, chunks[2]);
        }
    }
}
