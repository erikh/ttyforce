use ratatui::prelude::*;
use ratatui::widgets::*;

use crate::engine::state_machine::InstallerStateMachine;

use super::Screen;

pub struct WifiPasswordScreen {
    pub selected_index: usize,
    pub input_buffer: String,
    pub cursor_visible: bool,
}

impl WifiPasswordScreen {
    pub fn new() -> Self {
        Self {
            selected_index: 0,
            input_buffer: String::new(),
            cursor_visible: true,
        }
    }
}

impl Default for WifiPasswordScreen {
    fn default() -> Self {
        Self::new()
    }
}

impl Screen for WifiPasswordScreen {
    fn title(&self) -> &str {
        "Wi-Fi Password"
    }

    fn render(&self, f: &mut Frame, state: &InstallerStateMachine) {
        let area = f.area();

        let outer = Block::default()
            .title(" Town OS Installer — Wi-Fi Authentication ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan));
        f.render_widget(outer, area);

        let inner = area.inner(Margin { horizontal: 2, vertical: 1 });

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),  // SSID banner
                Constraint::Length(1),  // spacer
                Constraint::Length(3),  // security info
                Constraint::Length(1),  // spacer
                Constraint::Length(3),  // password field
                Constraint::Min(1),     // spacer / error
                Constraint::Length(3),  // hints
            ])
            .split(inner);

        // --- SSID banner ---
        let ssid = state
            .selected_ssid
            .as_deref()
            .unwrap_or("<unknown network>");

        let ssid_text = vec![
            Line::from(vec![
                Span::styled("Network: ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    ssid,
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
            ]),
        ];

        let ssid_para = Paragraph::new(ssid_text)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Blue)),
            );
        f.render_widget(ssid_para, chunks[0]);

        // --- Security info ---
        let security_text = state
            .wifi_networks
            .iter()
            .find(|n| Some(&n.ssid) == state.selected_ssid.as_ref())
            .map(|n| {
                format!(
                    "Security: {}   Signal: {} ({} dBm)   Band: {}",
                    n.security_display(),
                    n.signal_display(),
                    n.signal_strength,
                    if n.frequency_mhz >= 5000 { "5 GHz" } else { "2.4 GHz" }
                )
            })
            .unwrap_or_else(|| "Security: unknown".to_string());

        let security_para = Paragraph::new(security_text)
            .style(Style::default().fg(Color::Yellow));
        f.render_widget(security_para, chunks[2]);

        // --- Password field ---
        let masked: String = "*".repeat(self.input_buffer.len());
        let cursor = if self.cursor_visible { "▌" } else { " " };
        let display = format!("{}{}", masked, cursor);

        let password_field = Paragraph::new(display)
            .block(
                Block::default()
                    .title(" Password ")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Green)),
            )
            .style(Style::default().fg(Color::White));
        f.render_widget(password_field, chunks[4]);

        // --- Error message if any ---
        if let Some(err) = &state.error_message {
            let err_widget = Paragraph::new(err.as_str())
                .style(Style::default().fg(Color::Red).add_modifier(Modifier::BOLD))
                .wrap(Wrap { trim: true });
            f.render_widget(err_widget, chunks[5]);
        }

        // --- Hints ---
        let hint = Paragraph::new("Enter: connect  Esc: back to network list  q: quit")
            .style(Style::default().fg(Color::DarkGray));
        f.render_widget(hint, chunks[6]);
    }
}
