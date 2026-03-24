use ratatui::prelude::*;
use ratatui::widgets::*;

use crate::engine::state_machine::InstallerStateMachine;

use super::Screen;

pub struct WifiSelectScreen {
    pub selected_index: usize,
}

impl WifiSelectScreen {
    pub fn new() -> Self {
        Self { selected_index: 0 }
    }
}

impl Default for WifiSelectScreen {
    fn default() -> Self {
        Self::new()
    }
}

impl Screen for WifiSelectScreen {
    fn title(&self) -> &str {
        "Select Wi-Fi Network"
    }

    fn render(&self, f: &mut Frame, state: &InstallerStateMachine) {
        let area = f.area();

        let outer = Block::default()
            .title(" Town OS Installer — Wi-Fi Network Selection ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan));
        f.render_widget(outer, area);

        let inner = area.inner(Margin { horizontal: 2, vertical: 1 });

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(2),  // interface info
                Constraint::Min(5),     // network list
                Constraint::Length(3),  // hints / error
            ])
            .split(inner);

        // --- Interface info ---
        let iface_text = state
            .selected_interface
            .as_deref()
            .map(|name| format!("Scanning on interface: {}", name))
            .unwrap_or_else(|| "No interface selected".to_string());

        let iface_para = Paragraph::new(iface_text)
            .style(Style::default().fg(Color::Yellow));
        f.render_widget(iface_para, chunks[0]);

        // --- Network list ---
        let items: Vec<ListItem> = if state.wifi_networks.is_empty() {
            vec![ListItem::new("  No networks found. Press r to refresh.")
                .style(Style::default().fg(Color::DarkGray))]
        } else {
            state
                .wifi_networks
                .iter()
                .enumerate()
                .map(|(i, net)| {
                    let signal = net.signal_display();
                    let security = net.security_display();
                    let freq_band = if net.frequency_mhz >= 5000 { "5 GHz" } else { "2.4 GHz" };
                    let reachable = if net.reachable { "" } else { " [unreachable]" };

                    let label = format!(
                        " {} {:.<30} {:>4} dBm  {}  {}{}",
                        signal,
                        format!("{} ", net.ssid),
                        net.signal_strength,
                        freq_band,
                        security,
                        reachable,
                    );

                    let style = if i == self.selected_index {
                        Style::default()
                            .fg(Color::Black)
                            .bg(Color::Cyan)
                            .add_modifier(Modifier::BOLD)
                    } else if !net.reachable {
                        Style::default().fg(Color::DarkGray)
                    } else {
                        Style::default().fg(Color::White)
                    };

                    ListItem::new(label).style(style)
                })
                .collect()
        };

        let header = format!(
            " Signal  SSID{:.<26}  dBm   Band   Security",
            ""
        );

        let list = List::new(items)
            .block(
                Block::default()
                    .title(header)
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

        // --- Hints / error ---
        if let Some(err) = &state.error_message {
            let err_widget = Paragraph::new(err.as_str())
                .style(Style::default().fg(Color::Red).add_modifier(Modifier::BOLD))
                .wrap(Wrap { trim: true });
            f.render_widget(err_widget, chunks[2]);
        } else {
            let hint = Paragraph::new(
                "Enter: connect  ↑/↓: move  r: refresh  Esc: back  q: quit",
            )
            .style(Style::default().fg(Color::DarkGray));
            f.render_widget(hint, chunks[2]);
        }
    }
}
