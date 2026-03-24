use std::fs::File;
use std::io;
use std::os::unix::io::AsRawFd;

use crossterm::event::{self, Event, KeyCode, KeyEvent};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::prelude::*;
use ratatui::widgets::*;

use crate::engine::executor::OperationExecutor;
use crate::engine::state_machine::{InstallerStateMachine, ScreenId, UserInput};
use crate::engine::real_ops::kmsg_log;
use crate::tui::input::map_key_event;

/// Redirect stdin and stdout to the given TTY device.
/// Returns the saved file descriptors for stdin and stdout so they can be restored.
fn redirect_to_tty(tty_path: &str) -> io::Result<(i32, i32)> {
    let tty_file = File::options().read(true).write(true).open(tty_path)?;
    let tty_fd = tty_file.as_raw_fd();

    // Save current stdin/stdout fds
    let saved_stdin = nix::unistd::dup(0)
        .map_err(io::Error::other)?;
    let saved_stdout = nix::unistd::dup(1)
        .map_err(io::Error::other)?;

    // Redirect stdin and stdout to the TTY
    nix::unistd::dup2(tty_fd, 0)
        .map_err(io::Error::other)?;
    nix::unistd::dup2(tty_fd, 1)
        .map_err(io::Error::other)?;

    // tty_file is consumed here but the fd remains via dup2
    std::mem::forget(tty_file);

    Ok((saved_stdin, saved_stdout))
}

/// Restore stdin and stdout from saved file descriptors.
fn restore_fds(saved_stdin: i32, saved_stdout: i32) {
    let _ = nix::unistd::dup2(saved_stdin, 0);
    let _ = nix::unistd::dup2(saved_stdout, 1);
    let _ = nix::unistd::close(saved_stdin);
    let _ = nix::unistd::close(saved_stdout);
}

pub struct App {
    pub state_machine: InstallerStateMachine,
    pub selected_index: usize,
    pub password_input: String,
    pub should_quit: bool,
}

/// Return a centered sub-rect within `area`.
/// `width_pct` and `height_pct` are 0–100 percentages of the available space.
fn centered_rect(width_pct: u16, height_pct: u16, area: Rect) -> Rect {
    let v_pad = area.height.saturating_sub(area.height * height_pct / 100) / 2;
    let h_pad = area.width.saturating_sub(area.width * width_pct / 100) / 2;
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(v_pad),
            Constraint::Min(0),
            Constraint::Length(v_pad),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(h_pad),
            Constraint::Min(0),
            Constraint::Length(h_pad),
        ])
        .split(vertical[1])[1]
}

impl App {
    pub fn new(state_machine: InstallerStateMachine) -> Self {
        Self {
            state_machine,
            selected_index: 0,
            password_input: String::new(),
            should_quit: false,
        }
    }

    pub fn run(&mut self, executor: &mut dyn OperationExecutor, tty: Option<&str>) -> io::Result<()> {
        let saved_fds = if let Some(tty_path) = tty {
            kmsg_log(&format!("redirecting TUI to {}", tty_path));
            match redirect_to_tty(tty_path) {
                Ok(fds) => Some(fds),
                Err(e) => {
                    kmsg_log(&format!("failed to open TTY {}: {}", tty_path, e));
                    return Err(e);
                }
            }
        } else {
            None
        };

        let result = self.run_tui_loop(executor);

        if let Some((saved_stdin, saved_stdout)) = saved_fds {
            restore_fds(saved_stdin, saved_stdout);
        }

        if let Err(ref e) = result {
            kmsg_log(&format!("TUI error: {}", e));
        }

        result
    }

    fn run_tui_loop(&mut self, executor: &mut dyn OperationExecutor) -> io::Result<()> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen)?;
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend)?;

        while !self.should_quit {
            terminal.draw(|f| self.render(f))?;

            // On NetworkProgress, use a poll timeout so we can advance
            // connectivity checks between renders (showing progress).
            let on_progress = matches!(
                self.state_machine.current_screen,
                ScreenId::NetworkProgress
            );
            let timeout = if on_progress {
                std::time::Duration::from_millis(500)
            } else {
                std::time::Duration::from_secs(60)
            };

            if event::poll(timeout)? {
                if let Event::Key(key) = event::read()? {
                    self.handle_key(key, executor);
                }
            }

            // Advance connectivity checks one step at a time so the
            // progress screen updates between each check.
            if on_progress {
                self.state_machine.advance_connectivity(executor);
            }
        }

        disable_raw_mode()?;
        execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
        Ok(())
    }

    fn render(&self, f: &mut ratatui::Frame) {
        let area = f.area();

        // Outer layout: title bar, content area, command log, status bar
        let outer = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Min(12),
                Constraint::Length(10),
                Constraint::Length(3),
            ])
            .split(area);

        // Title
        let title = Paragraph::new("Town OS Installer")
            .alignment(Alignment::Center)
            .style(
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )
            .block(Block::default().borders(Borders::ALL));
        f.render_widget(title, outer[0]);

        // Main content
        self.render_screen(f, outer[1]);

        // Command log pane
        self.render_cmd_log(f, outer[2]);

        // Status bar
        let status = match &self.state_machine.error_message {
            Some(err) => Paragraph::new(err.as_str()).style(Style::default().fg(Color::Red)),
            None => Paragraph::new(format!(
                "Screen: {:?} | Network: {}",
                self.state_machine.current_screen, self.state_machine.network_state
            ))
            .style(Style::default().fg(Color::DarkGray)),
        };
        let status = status
            .alignment(Alignment::Center)
            .block(Block::default().borders(Borders::ALL));
        f.render_widget(status, outer[3]);
    }

    fn render_cmd_log(&self, f: &mut ratatui::Frame, area: Rect) {
        let log = crate::engine::real_ops::cmd_log();
        let block = Block::default()
            .title(" Command Output ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray));

        let inner_height = area.height.saturating_sub(2) as usize; // border top+bottom
        let start = log.len().saturating_sub(inner_height);
        let visible: Vec<Line> = log[start..]
            .iter()
            .map(|line| {
                let style = if line.starts_with('$') {
                    Style::default().fg(Color::Yellow)
                } else if line.contains("FAILED") || line.contains("error:") || line.contains("err:") {
                    Style::default().fg(Color::Red)
                } else if line.contains("-> ok") {
                    Style::default().fg(Color::Green)
                } else {
                    Style::default().fg(Color::DarkGray)
                };
                Line::from(Span::styled(line.as_str(), style))
            })
            .collect();

        let paragraph = Paragraph::new(visible).block(block);
        f.render_widget(paragraph, area);
    }

    fn render_screen(&self, f: &mut ratatui::Frame, area: Rect) {
        match &self.state_machine.current_screen {
            ScreenId::NetworkConfig => self.render_network_config(f, area),
            ScreenId::WifiSelect => self.render_wifi_select(f, area),
            ScreenId::WifiPassword => self.render_wifi_password(f, area),
            ScreenId::NetworkProgress => self.render_network_progress(f, area),
            ScreenId::DiskGroupSelect => self.render_disk_select(f, area),
            ScreenId::RaidConfig => self.render_raid_config(f, area),
            ScreenId::Confirm => self.render_confirm(f, area),
            ScreenId::InstallProgress => self.render_install_progress(f, area),
            ScreenId::Reboot => self.render_reboot(f, area),
        }
    }

    fn render_network_config(&self, f: &mut ratatui::Frame, area: Rect) {
        let items: Vec<ListItem> = self
            .state_machine
            .interfaces
            .iter()
            .enumerate()
            .map(|(i, iface)| {
                let link_status = if iface.has_link && iface.has_carrier {
                    " [connected]"
                } else if iface.has_link {
                    " [link]"
                } else {
                    " [no link]"
                };
                let style = if i == self.selected_index {
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };
                ListItem::new(format!(
                    "  {} ({:?}){}",
                    iface.name, iface.kind, link_status
                ))
                .style(style)
            })
            .collect();

        let content_height = items.len() as u16 + 2; // +2 for border
        let height_pct = (content_height * 100 / area.height.max(1)).clamp(30, 80);
        let center = centered_rect(60, height_pct, area);

        let list = List::new(items)
            .block(
                Block::default()
                    .title(" Network — ↑↓: navigate, Enter: select ")
                    .title_alignment(Alignment::Center)
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Cyan)),
            )
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
        f.render_widget(list, center);
    }

    fn render_wifi_select(&self, f: &mut ratatui::Frame, area: Rect) {
        let items: Vec<ListItem> = self
            .state_machine
            .wifi_networks
            .iter()
            .enumerate()
            .map(|(i, net)| {
                let style = if i == self.selected_index {
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD)
                } else if !net.reachable {
                    Style::default().fg(Color::DarkGray)
                } else {
                    Style::default()
                };
                ListItem::new(format!(
                    "  {} {:<24} {:>4}  {}",
                    net.signal_display(),
                    net.ssid,
                    net.security_display(),
                    if net.reachable { "" } else { "[unreachable]" }
                ))
                .style(style)
            })
            .collect();

        let content_height = items.len() as u16 + 2;
        let height_pct = (content_height * 100 / area.height.max(1)).clamp(30, 80);
        let center = centered_rect(70, height_pct, area);

        let list = List::new(items).block(
            Block::default()
                .title(" WiFi Networks — r: refresh, Esc: back ")
                .title_alignment(Alignment::Center)
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        );
        f.render_widget(list, center);
    }

    fn render_wifi_password(&self, f: &mut ratatui::Frame, area: Rect) {
        let ssid = self
            .state_machine
            .selected_ssid
            .as_deref()
            .unwrap_or("Unknown");
        let masked: String = "*".repeat(self.password_input.len());
        let display = format!("  Network: {}\n\n  Password: {}_", ssid, masked);

        let center = centered_rect(50, 30, area);

        let paragraph = Paragraph::new(display).block(
            Block::default()
                .title(" Enter WiFi Password ")
                .title_alignment(Alignment::Center)
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        );
        f.render_widget(paragraph, center);
    }

    fn render_network_progress(&self, f: &mut ratatui::Frame, area: Rect) {
        let status_icon = if self.state_machine.network_state.is_online() {
            "\n  Press Enter to continue to disk setup"
        } else if self.state_machine.network_state.is_terminal() {
            "\n  Press Esc to go back and try again"
        } else {
            "\n  Connecting..."
        };

        let status = format!(
            "  Status:    {}\n  Interface: {}{}",
            self.state_machine.network_state,
            self.state_machine
                .selected_interface
                .as_deref()
                .unwrap_or("None"),
            status_icon,
        );

        let center = centered_rect(50, 30, area);

        let paragraph = Paragraph::new(status).block(
            Block::default()
                .title(" Network Progress ")
                .title_alignment(Alignment::Center)
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        );
        f.render_widget(paragraph, center);
    }

    fn render_disk_select(&self, f: &mut ratatui::Frame, area: Rect) {
        let single_mode = self.state_machine.is_single_disk_mode();
        let mut items: Vec<ListItem> = Vec::new();

        if single_mode {
            // Single disk mode: show individual disks
            for (i, disk) in self.state_machine.all_disks.iter().enumerate() {
                let style = if i == self.selected_index {
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };
                let serial_str = disk
                    .serial
                    .as_deref()
                    .map(|s| format!("  SN:{}", s))
                    .unwrap_or_default();
                items.push(
                    ListItem::new(format!(
                        "  {} — {} {} — {}{}",
                        disk.device,
                        disk.make,
                        disk.model,
                        disk.size_human(),
                        serial_str
                    ))
                    .style(style),
                );
            }
        } else {
            // Group mode: show disk groups
            let compatible = self.state_machine.compatible_disk_groups();
            for (list_idx, &group_idx) in compatible.iter().enumerate() {
                let group = &self.state_machine.disk_groups[group_idx];
                let style = if list_idx == self.selected_index {
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };

                // Group header
                let mut lines = vec![Line::from(format!(
                    "  {} {} — {}x, {} total",
                    group.make,
                    group.model,
                    group.disk_count(),
                    group.total_human()
                ))];

                // Individual disks
                for disk in &group.disks {
                    let serial_str = disk
                        .serial
                        .as_deref()
                        .map(|s| format!("  SN:{}", s))
                        .unwrap_or_default();
                    lines.push(Line::from(format!(
                        "    {} — {}{}",
                        disk.device,
                        disk.size_human(),
                        serial_str
                    )));
                }

                items.push(ListItem::new(lines).style(style));
            }
        }

        if items.is_empty() {
            items.push(
                ListItem::new("  No compatible disks for selected RAID level")
                    .style(Style::default().fg(Color::Red)),
            );
        }

        let content_height = items
            .iter()
            .map(|i| i.height() as u16)
            .sum::<u16>()
            + 2;
        let height_pct = (content_height * 100 / area.height.max(1)).clamp(30, 80);
        let center = centered_rect(65, height_pct, area);

        let raid_label = self
            .state_machine
            .selected_raid
            .as_ref()
            .map(|r| r.display_name())
            .unwrap_or("None");

        let title = if single_mode {
            format!(" Select Disk — {} ", raid_label)
        } else {
            format!(" Select Disk Group — RAID: {} ", raid_label)
        };

        let list = List::new(items).block(
            Block::default()
                .title(title)
                .title_alignment(Alignment::Center)
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        );
        f.render_widget(list, center);
    }

    fn render_raid_config(&self, f: &mut ratatui::Frame, area: Rect) {
        use crate::disk::RaidConfig;

        let disk_count = self.state_machine.max_disk_count();
        let options = RaidConfig::for_disk_count(disk_count);

        let items: Vec<ListItem> = options
            .iter()
            .enumerate()
            .map(|(i, opt)| {
                let style = if i == self.selected_index {
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };
                ListItem::new(format!("  {}\n    {}", opt.display_name(), opt.description()))
                    .style(style)
            })
            .collect();

        let content_height = (items.len() as u16 * 2) + 2;
        let height_pct = (content_height * 100 / area.height.max(1)).clamp(30, 70);
        let center = centered_rect(70, height_pct, area);

        let list = List::new(items).block(
            Block::default()
                .title(" RAID Configuration ")
                .title_alignment(Alignment::Center)
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        );
        f.render_widget(list, center);
    }

    fn render_confirm(&self, f: &mut ratatui::Frame, area: Rect) {
        let raid_name = self
            .state_machine
            .selected_raid
            .as_ref()
            .map(|r| r.display_name().to_string())
            .unwrap_or_else(|| "None".to_string());

        let single_mode = self.state_machine.is_single_disk_mode();

        let disk_summary = if single_mode {
            if let Some(disk_idx) = self.state_machine.selected_disk {
                let disk = &self.state_machine.all_disks[disk_idx];
                format!("{} — {} {} ({})", disk.device, disk.make, disk.model, disk.size_human())
            } else {
                "None".to_string()
            }
        } else {
            let group_idx = self.state_machine.selected_disk_group.unwrap_or(0);
            self.state_machine.disk_groups.get(group_idx)
                .map(|g| g.display_name())
                .unwrap_or_else(|| "None".to_string())
        };

        let mut summary = format!(
            "  Network:    {} ({})\
             \n  Filesystem: {}\
             \n  RAID:       {}\
             \n  {}:  {}",
            self.state_machine
                .selected_interface
                .as_deref()
                .unwrap_or("None"),
            self.state_machine.network_state,
            self.state_machine.selected_filesystem,
            raid_name,
            if single_mode { "Disk      " } else { "Disk Group" },
            disk_summary,
        );

        if !single_mode {
            if let Some(group_idx) = self.state_machine.selected_disk_group {
                if let Some(g) = self.state_machine.disk_groups.get(group_idx) {
                    for disk in &g.disks {
                        summary.push_str(&format!("\n    {} — {}", disk.device, disk.size_human()));
                    }
                }
            }
        }

        summary.push_str("\n\n  Enter: install  |  Esc: back  |  a: abort");

        let line_count = summary.lines().count() as u16 + 2;
        let height_pct = (line_count * 100 / area.height.max(1)).clamp(30, 60);
        let center = centered_rect(60, height_pct, area);

        let paragraph = Paragraph::new(summary).block(
            Block::default()
                .title(" Confirm Installation ")
                .title_alignment(Alignment::Center)
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Green)),
        );
        f.render_widget(paragraph, center);
    }

    fn render_install_progress(&self, f: &mut ratatui::Frame, area: Rect) {
        let status = format!(
            "  Status:     {:?}\
             \n  Operations: {}\
             \n\
             \n  Press Enter to continue",
            self.state_machine.action_manifest.final_state,
            self.state_machine.action_manifest.operations.len()
        );

        let center = centered_rect(50, 30, area);

        let paragraph = Paragraph::new(status).block(
            Block::default()
                .title(" Installation Progress ")
                .title_alignment(Alignment::Center)
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        );
        f.render_widget(paragraph, center);
    }

    fn render_reboot(&self, f: &mut ratatui::Frame, area: Rect) {
        let options = ["Reboot", "Exit", "Power Off"];
        let items: Vec<ListItem> = options
            .iter()
            .enumerate()
            .map(|(i, label)| {
                let style = if i == self.selected_index {
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };
                ListItem::new(format!("  {}", label)).style(style)
            })
            .collect();

        let center = centered_rect(50, 30, area);

        let list = List::new(items).block(
            Block::default()
                .title(" Installation Complete — ↑↓: navigate, Enter: select ")
                .title_alignment(Alignment::Center)
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Green)),
        );
        f.render_widget(list, center);
    }

    fn handle_key(&mut self, key: KeyEvent, executor: &mut dyn OperationExecutor) {
        // Handle password input specially
        if self.state_machine.current_screen == ScreenId::WifiPassword {
            match key.code {
                KeyCode::Enter => {
                    let password = self.password_input.clone();
                    self.password_input.clear();
                    let input = UserInput::EnterWifiPassword(password);
                    if self
                        .state_machine
                        .process_input(input, executor)
                        .is_some()
                    {
                        self.selected_index = 0;
                    }
                    return;
                }
                KeyCode::Backspace => {
                    self.password_input.pop();
                    return;
                }
                KeyCode::Char(c) => {
                    self.password_input.push(c);
                    return;
                }
                KeyCode::Esc => {
                    self.password_input.clear();
                    self.state_machine.process_input(UserInput::Back, executor);
                    self.selected_index = 0;
                    return;
                }
                _ => return,
            }
        }

        // Handle up/down for list navigation
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                if self.selected_index > 0 {
                    self.selected_index -= 1;
                }
                return;
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.selected_index += 1;
                return;
            }
            _ => {}
        }

        if let Some(input) =
            map_key_event(key, &self.state_machine.current_screen, self.selected_index)
        {
            if matches!(input, UserInput::Quit) {
                self.should_quit = true;
                return;
            }
            let is_exit = matches!(input, UserInput::ExitInstaller);
            let is_terminal = matches!(
                input,
                UserInput::ConfirmInstall | UserInput::AbortInstall
            ) && self.state_machine.current_screen == ScreenId::Confirm;
            if self
                .state_machine
                .process_input(input, executor)
                .is_some()
            {
                self.selected_index = 0;
            }
            if is_exit || is_terminal {
                self.should_quit = true;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_redirect_to_tty_nonexistent_device() {
        let result = redirect_to_tty("/dev/nonexistent_tty_device_xyz");
        assert!(result.is_err());
    }

    #[test]
    fn test_redirect_and_restore_with_devnull() {
        // Use /dev/null as a stand-in — it's always available and read+write.
        // This tests the dup/dup2/restore mechanics without needing a real TTY.
        let saved_stdin = nix::unistd::dup(0).unwrap();
        let saved_stdout = nix::unistd::dup(1).unwrap();

        let result = redirect_to_tty("/dev/null");
        assert!(result.is_ok(), "redirect_to_tty(/dev/null) failed: {:?}", result);

        let (inner_stdin, inner_stdout) = result.unwrap();
        // After redirect, fd 0 and 1 should point to /dev/null.
        // Restore original fds.
        restore_fds(inner_stdin, inner_stdout);

        // Verify stdout still works after restore by writing to it
        use std::io::Write;
        let _ = writeln!(std::io::stdout(), "stdout still works after restore");

        let _ = nix::unistd::close(saved_stdin);
        let _ = nix::unistd::close(saved_stdout);
    }
}
