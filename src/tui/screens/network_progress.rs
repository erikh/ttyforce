use ratatui::prelude::*;
use ratatui::widgets::*;

use crate::engine::state_machine::InstallerStateMachine;
use crate::network::state::NetworkState;

use super::Screen;

pub struct NetworkProgressScreen {
    pub selected_index: usize,
}

impl NetworkProgressScreen {
    pub fn new() -> Self {
        Self { selected_index: 0 }
    }
}

impl Default for NetworkProgressScreen {
    fn default() -> Self {
        Self::new()
    }
}

fn ethernet_steps() -> Vec<(NetworkState, &'static str)> {
    vec![
        (NetworkState::DeviceEnabled, "Device enabled"),
        (NetworkState::DhcpConfiguring, "Configuring DHCP"),
        (NetworkState::IpAssigned, "IP address assigned"),
        (NetworkState::CheckingRouter, "Checking upstream router"),
        (NetworkState::CheckingInternet, "Checking internet routability"),
        (NetworkState::CheckingDns, "Checking DNS resolution"),
        (NetworkState::Online, "Online"),
    ]
}

fn wifi_steps() -> Vec<(NetworkState, &'static str)> {
    vec![
        (NetworkState::DeviceEnabled, "Device enabled"),
        (NetworkState::Scanning, "Scanning for networks"),
        (NetworkState::NetworkSelected, "Network selected"),
        (NetworkState::Authenticating, "Authenticating"),
        (NetworkState::Connected, "Connected to access point"),
        (NetworkState::DhcpConfiguring, "Configuring DHCP"),
        (NetworkState::IpAssigned, "IP address assigned"),
        (NetworkState::CheckingRouter, "Checking upstream router"),
        (NetworkState::CheckingInternet, "Checking internet routability"),
        (NetworkState::CheckingDns, "Checking DNS resolution"),
        (NetworkState::Online, "Online"),
    ]
}

fn state_rank(current: &NetworkState, steps: &[(NetworkState, &str)]) -> Option<usize> {
    steps.iter().position(|(st, _)| st == current)
}

impl Screen for NetworkProgressScreen {
    fn title(&self) -> &str {
        "Network Progress"
    }

    fn render(&self, f: &mut Frame, state: &InstallerStateMachine) {
        let area = f.area();

        let outer = Block::default()
            .title(" Town OS Installer — Network Progress ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan));
        f.render_widget(outer, area);

        let inner = area.inner(Margin { horizontal: 2, vertical: 1 });

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(2),  // interface / ssid summary
                Constraint::Min(8),     // step list
                Constraint::Length(4),  // status block
                Constraint::Length(3),  // hints
            ])
            .split(inner);

        // --- Interface / SSID summary ---
        let iface_label = state
            .selected_interface
            .as_deref()
            .unwrap_or("<none>");
        let ssid_part = state
            .selected_ssid
            .as_deref()
            .map(|s| format!("  SSID: {}", s))
            .unwrap_or_default();
        let iface_text = format!("Interface: {}{}", iface_label, ssid_part);
        let iface_para = Paragraph::new(iface_text)
            .style(Style::default().fg(Color::Yellow));
        f.render_widget(iface_para, chunks[0]);

        // --- Step list ---
        let is_wifi = state.selected_ssid.is_some();
        let steps = if is_wifi { wifi_steps() } else { ethernet_steps() };
        let is_error = matches!(&state.network_state, NetworkState::Error(_));
        let current_rank = state_rank(&state.network_state, &steps);

        let items: Vec<ListItem> = steps
            .iter()
            .enumerate()
            .map(|(i, (_, label))| {
                let (symbol, style) = match current_rank {
                    Some(rank) if i < rank => (
                        "✓",
                        Style::default().fg(Color::Green),
                    ),
                    Some(rank) if i == rank && !is_error => (
                        "▶",
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Some(rank) if i == rank && is_error => (
                        "✗",
                        Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                    ),
                    _ => (
                        "○",
                        Style::default().fg(Color::DarkGray),
                    ),
                };
                ListItem::new(format!("  {}  {}", symbol, label)).style(style)
            })
            .collect();

        let list = List::new(items).block(
            Block::default()
                .title(" Connection Steps ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Blue)),
        );
        f.render_widget(list, chunks[1]);

        // --- Status block ---
        let (status_text, status_style) = if state.network_state.is_online() {
            let ip = state
                .interfaces
                .iter()
                .find(|i| Some(&i.name) == state.selected_interface.as_ref())
                .and_then(|i| i.ip_address.clone())
                .unwrap_or_else(|| "unknown".to_string());
            (
                format!("Status: Online   IP: {}", ip),
                Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
            )
        } else if let NetworkState::Error(msg) = &state.network_state {
            (
                format!("Error: {}", msg),
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            )
        } else {
            (
                format!("Status: {}", state.network_state),
                Style::default().fg(Color::Yellow),
            )
        };

        let status_para = Paragraph::new(status_text)
            .style(status_style)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::DarkGray)),
            )
            .wrap(Wrap { trim: true });
        f.render_widget(status_para, chunks[2]);

        // --- Hints ---
        let hint_text = if state.network_state.is_online() {
            "Enter: continue to disk setup  Esc: back to network config  q: quit"
        } else if is_error {
            "Esc: back to network config  q: quit"
        } else {
            "Waiting for network...  Esc: cancel  q: quit"
        };

        let hints = Paragraph::new(hint_text)
            .style(Style::default().fg(Color::DarkGray));
        f.render_widget(hints, chunks[3]);
    }
}
