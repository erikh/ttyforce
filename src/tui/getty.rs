use std::io::{self, BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::prelude::*;
use ratatui::widgets::*;

use crate::engine::executor::OperationExecutor;
use crate::engine::real_ops::{cmd_log_append, kmsg_log, run_cmd};
use crate::getty::api::{ServiceInfo, TownApiClient};
use crate::getty::sysinfo::SystemInfo;
use crate::operations::Operation;
use crate::tui::app::{redirect_to_tty, render_cmd_log, restore_fds};

/// Which panel is shown in the services area.
#[derive(Debug, Clone, PartialEq)]
pub enum PanelView {
    /// Startup: show log, auto-switch to status when all services are green.
    Auto,
    /// User explicitly selected the log panel.
    Log,
    /// User explicitly selected the status panel (or auto-switched).
    Status,
}

/// Actions that can be triggered from the getty screen.
#[derive(Debug, Clone, PartialEq)]
pub enum GettyAction {
    None,
    Login,
    Reconfigure,
    Reboot,
    PowerOff,
    Sledgehammer,
}

/// The getty TUI application — replaces login on a TTY.
pub struct GettyApp {
    pub system_info: SystemInfo,
    /// System services only — used for startup readiness checks.
    pub system_services: Result<Vec<ServiceInfo>, String>,
    /// All services (system + packages) — shown on the status panel.
    pub all_services: Result<Vec<ServiceInfo>, String>,
    pub api_client: TownApiClient,
    pub etc_prefix: Option<String>,
    pub tty: Option<String>,
    pub mount_point: String,
    /// When Some, the user is in sledgehammer confirmation mode.
    pub sledgehammer_input: Option<String>,
    pub should_quit: bool,
    pub panel_view: PanelView,
    last_fast_refresh: Instant,
    last_slow_refresh: Instant,
    /// Live journalctl -f output shown while services are starting.
    journal_lines: Vec<String>,
    journal_child: Option<Child>,
    /// Max lines to keep in the journal buffer.
    journal_max_lines: usize,
}

impl GettyApp {
    pub fn new(
        etc_prefix: Option<String>,
        tty: Option<String>,
        mount_point: String,
    ) -> Self {
        let api_client = TownApiClient::from_env(etc_prefix.as_deref());
        let system_info = SystemInfo::probe(&mount_point);
        let system_services = api_client.fetch_system_services();
        let all_services = api_client.fetch_all_services();

        Self {
            system_info,
            system_services,
            all_services,
            api_client,
            etc_prefix,
            tty,
            mount_point,
            sledgehammer_input: None,
            should_quit: false,
            panel_view: PanelView::Auto,
            last_fast_refresh: Instant::now(),
            last_slow_refresh: Instant::now(),
            journal_lines: Vec::new(),
            journal_child: None,
            journal_max_lines: 200,
        }
    }

    /// Run the getty TUI, optionally redirecting to a TTY device.
    pub fn run(
        &mut self,
        executor: &mut dyn OperationExecutor,
        tty: Option<&str>,
    ) -> io::Result<()> {
        let saved_fds = if let Some(tty_path) = tty {
            kmsg_log(&format!("getty: redirecting to {}", tty_path));
            match redirect_to_tty(tty_path) {
                Ok(fds) => Some(fds),
                Err(e) => {
                    kmsg_log(&format!("getty: failed to open TTY {}: {}", tty_path, e));
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
            kmsg_log(&format!("getty TUI error: {}", e));
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
            // Fast refresh: proc file reads (instant, every 3s)
            if self.last_fast_refresh.elapsed() >= Duration::from_secs(3) {
                self.system_info.refresh_stats(&self.mount_point);
                self.last_fast_refresh = Instant::now();
            }

            // Slow refresh: network check + API (can block briefly, every 15s)
            if self.last_slow_refresh.elapsed() >= Duration::from_secs(15) {
                self.system_info.refresh_network();
                self.system_services = self.api_client.fetch_system_services();
                self.all_services = self.api_client.fetch_all_services();
                self.last_slow_refresh = Instant::now();
            }

            // Manage journal process: start if services starting, drain lines, stop when ready
            self.manage_journal();

            terminal.draw(|f| self.render(f))?;

            if event::poll(Duration::from_secs(1))? {
                if let Event::Key(key) = event::read()? {
                    let action = self.map_key(key);
                    match action {
                        GettyAction::Login => {
                            // exec into /bin/login — cede control entirely.
                            // agetty will respawn us after the shell exits.
                            self.stop_journal();
                            disable_raw_mode()?;
                            execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
                            self.exec_login();
                            // exec_login only returns on failure — recover TUI
                            let mut new_stdout = io::stdout();
                            execute!(new_stdout, EnterAlternateScreen)?;
                            enable_raw_mode()?;
                            terminal = Terminal::new(CrosstermBackend::new(new_stdout))?;
                        }
                        GettyAction::Reconfigure => {
                            // Spawn reconfigure as child, wait, then resume getty
                            self.stop_journal();
                            disable_raw_mode()?;
                            execute!(terminal.backend_mut(), LeaveAlternateScreen)?;

                            self.execute_action(&action, executor);

                            // Re-enter TUI
                            let mut new_stdout = io::stdout();
                            execute!(new_stdout, EnterAlternateScreen)?;
                            enable_raw_mode()?;
                            terminal = Terminal::new(CrosstermBackend::new(new_stdout))?;
                            self.last_fast_refresh = Instant::now() - Duration::from_secs(10);
                            self.last_slow_refresh = Instant::now() - Duration::from_secs(20);
                        }
                        GettyAction::None => {}
                        _ => {
                            self.execute_action(&action, executor);
                        }
                    }
                }
            }
        }

        self.stop_journal();
        disable_raw_mode()?;
        execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
        Ok(())
    }

    /// Map a key event to a GettyAction.
    pub fn map_key(&mut self, key: KeyEvent) -> GettyAction {
        // Ctrl+C always quits
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.should_quit = true;
            return GettyAction::None;
        }

        // Sledgehammer confirmation mode
        if let Some(ref mut input) = self.sledgehammer_input {
            match key.code {
                KeyCode::Esc => {
                    self.sledgehammer_input = None;
                }
                KeyCode::Backspace => {
                    input.pop();
                }
                KeyCode::Enter => {
                    if input == "SLEDGEHAMMER" {
                        self.sledgehammer_input = None;
                        return GettyAction::Sledgehammer;
                    }
                    // Wrong input — clear and stay in confirmation mode
                    input.clear();
                }
                KeyCode::Char(c) => {
                    input.push(c);
                }
                _ => {}
            }
            return GettyAction::None;
        }

        // Normal mode
        match key.code {
            KeyCode::Char('.') => GettyAction::Login,
            KeyCode::Char('l') => {
                if self.panel_view != PanelView::Log {
                    self.panel_view = PanelView::Log;
                    // Restart journal if not running
                    if self.journal_child.is_none() {
                        self.start_journal();
                    }
                }
                GettyAction::None
            }
            KeyCode::Char('s') => {
                self.panel_view = PanelView::Status;
                GettyAction::None
            }
            KeyCode::Char('r') => GettyAction::Reconfigure,
            KeyCode::Char('R') => GettyAction::Reboot,
            KeyCode::Char('p') => GettyAction::PowerOff,
            KeyCode::Char('!') => {
                self.sledgehammer_input = Some(String::new());
                GettyAction::None
            }
            _ => GettyAction::None,
        }
    }

    /// Check whether all system services are active (no activating/failed/inactive).
    /// An empty service list is considered "all active" (nothing to wait for).
    /// An API error is not — we can't confirm services are ready.
    pub fn all_services_active(&self) -> bool {
        match &self.system_services {
            Ok(services) => services.iter().all(|s| s.active_state == "active"),
            Err(_) => false,
        }
    }

    /// Manage journal process and panel view transitions.
    fn manage_journal(&mut self) {
        let all_active = self.all_services_active();

        // Auto mode: switch to Status when all services are green
        if self.panel_view == PanelView::Auto && all_active {
            self.panel_view = PanelView::Status;
        }

        // Journal should run when in Auto (starting) or Log mode
        let need_journal = matches!(self.panel_view, PanelView::Auto | PanelView::Log);

        if need_journal {
            if self.journal_child.is_none() {
                self.start_journal();
            }
            self.drain_journal_lines();
        } else {
            // Status mode with all services active — stop journal
            if all_active {
                self.stop_journal();
            } else {
                // Status mode but services not all active — keep draining
                // so we have data if user switches to log
                if self.journal_child.is_none() {
                    self.start_journal();
                }
                self.drain_journal_lines();
            }
        }
    }

    fn start_journal(&mut self) {
        let child = Command::new("journalctl")
            .args(["-f", "--no-pager", "-n", "50", "-o", "short-iso"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn();

        match child {
            Ok(c) => {
                // Set stdout to non-blocking so we can poll it
                if let Some(ref stdout) = c.stdout {
                    set_nonblocking(stdout);
                }
                self.journal_child = Some(c);
            }
            Err(e) => {
                self.journal_lines.push(format!("Failed to start journalctl: {}", e));
            }
        }
    }

    fn stop_journal(&mut self) {
        if let Some(mut child) = self.journal_child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    fn drain_journal_lines(&mut self) {
        // Take the child temporarily to get a mutable reference to stdout
        let Some(ref mut child) = self.journal_child else {
            return;
        };
        let Some(ref mut stdout) = child.stdout else {
            return;
        };

        let mut reader = BufReader::new(stdout);
        let mut line = String::new();

        // Read all available lines (non-blocking — will get WouldBlock when drained)
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) => break, // EOF
                Ok(_) => {
                    let trimmed = line.trim_end().to_string();
                    if !trimmed.is_empty() {
                        self.journal_lines.push(trimmed);
                    }
                    // Cap buffer size
                    if self.journal_lines.len() > self.journal_max_lines {
                        let excess = self.journal_lines.len() - self.journal_max_lines;
                        self.journal_lines.drain(..excess);
                    }
                }
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(_) => break,
            }
        }
    }

    /// Exec into /bin/login, replacing this process entirely.
    /// Only returns if exec fails.
    fn exec_login(&self) {
        use std::os::unix::process::CommandExt;
        let err = Command::new("/bin/login").exec();
        // only reached on failure — exec replaces the process on success
        cmd_log_append(format!("  -> exec /bin/login failed: {}", err));
    }

    /// Execute a getty action.
    pub fn execute_action(
        &mut self,
        action: &GettyAction,
        executor: &mut dyn OperationExecutor,
    ) {
        match action {
            GettyAction::None | GettyAction::Login => {}
            GettyAction::Reconfigure => {
                let exe = std::env::current_exe()
                    .unwrap_or_else(|_| "ttyforce".into());
                let mut args: Vec<String> = vec!["run".to_string()];
                if let Some(ref prefix) = self.etc_prefix {
                    args.push("--etc-prefix".to_string());
                    args.push(prefix.clone());
                }
                if let Some(ref tty) = self.tty {
                    args.push("--tty".to_string());
                    args.push(tty.clone());
                }
                cmd_log_append(format!("$ {} {}", exe.display(), args.join(" ")));
                let status = std::process::Command::new(&exe).args(&args).status();
                match status {
                    Ok(s) => cmd_log_append(format!("  -> reconfigure exited ({})", s)),
                    Err(e) => cmd_log_append(format!("  -> reconfigure failed: {}", e)),
                }
            }
            GettyAction::Reboot => {
                let op = Operation::Reboot;
                let result = executor.execute(&op);
                cmd_log_append(format!("  -> reboot: {:?}", result));
            }
            GettyAction::PowerOff => {
                let op = Operation::PowerOff;
                let result = executor.execute(&op);
                cmd_log_append(format!("  -> poweroff: {:?}", result));
            }
            GettyAction::Sledgehammer => {
                self.execute_sledgehammer(executor);
            }
        }
    }

    fn execute_sledgehammer(&mut self, executor: &mut dyn OperationExecutor) {
        cmd_log_append("sledgehammer: starting wipe sequence".to_string());

        // Stop all podman containers before unmount
        let stop_op = Operation::StopAllContainers;
        let result = executor.execute(&stop_op);
        cmd_log_append(format!("  -> stop containers: {:?}", result));

        // Unmount /town-os
        let unmount_op = Operation::CleanupUnmount {
            mount_point: self.mount_point.clone(),
        };
        let result = executor.execute(&unmount_op);
        cmd_log_append(format!("  -> unmount: {:?}", result));

        // Discover btrfs member devices
        let devices = discover_btrfs_devices(&self.mount_point);

        // Wipe each device
        for device in &devices {
            let wipe_op = Operation::WipeDisk {
                device: device.clone(),
            };
            let result = executor.execute(&wipe_op);
            cmd_log_append(format!("  -> wipe {}: {:?}", device, result));
        }

        // Reboot
        let reboot_op = Operation::Reboot;
        let result = executor.execute(&reboot_op);
        cmd_log_append(format!("  -> reboot: {:?}", result));
    }

    fn render(&self, f: &mut ratatui::Frame) {
        let area = f.area();

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),  // Title bar
                Constraint::Length(8),  // System info
                Constraint::Min(5),    // Services
                Constraint::Length(10), // Command log
                Constraint::Length(3),  // Action bar
            ])
            .split(area);

        self.render_title(f, chunks[0]);
        self.render_system_info(f, chunks[1]);
        self.render_services(f, chunks[2]);
        render_cmd_log(f, chunks[3]);

        if self.sledgehammer_input.is_some() {
            self.render_sledgehammer_confirm(f, chunks[4]);
        } else {
            self.render_actions(f, chunks[4]);
        }
    }

    fn render_title(&self, f: &mut ratatui::Frame, area: Rect) {
        let version = self
            .system_info
            .town_os_version
            .as_deref()
            .unwrap_or("");
        let title_text = if version.is_empty() {
            format!("{}  —  Town OS", self.system_info.mdns_url)
        } else {
            format!(
                "{}  —  Town OS {}",
                self.system_info.mdns_url, version
            )
        };
        let title = Paragraph::new(title_text)
            .alignment(Alignment::Center)
            .style(
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )
            .block(Block::default().borders(Borders::ALL));
        f.render_widget(title, area);
    }

    fn render_system_info(&self, f: &mut ratatui::Frame, area: Rect) {
        let info = &self.system_info;

        let mem_pct = if info.mem_total_mb > 0 {
            (info.mem_used_mb as f64 / info.mem_total_mb as f64 * 100.0) as u64
        } else {
            0
        };

        let network_line = if info.network_online {
            let ip = info.ip_address.as_deref().unwrap_or("unknown");
            let iface = info.default_interface.as_deref().unwrap_or("unknown");
            format!("Online ({} via {})", ip, iface)
        } else {
            "Offline".to_string()
        };

        let network_style = if info.network_online {
            Style::default().fg(Color::Green)
        } else {
            Style::default().fg(Color::Red)
        };

        let lines = vec![
            Line::from(vec![
                Span::styled("  Kernel: ", Style::default().fg(Color::DarkGray)),
                Span::raw(format!(
                    "{} {}",
                    info.kernel_version, info.architecture
                )),
                Span::styled("     CPU: ", Style::default().fg(Color::DarkGray)),
                Span::raw(format!("{} ({} cores)", info.cpu_model, info.cpu_cores)),
            ]),
            Line::from(vec![
                Span::styled("  Load:   ", Style::default().fg(Color::DarkGray)),
                Span::raw(format!("{:.2}", info.load_average)),
                Span::styled("              Memory: ", Style::default().fg(Color::DarkGray)),
                Span::raw(format!(
                    "{} / {} MB ({}%)",
                    info.mem_used_mb, info.mem_total_mb, mem_pct
                )),
            ]),
            Line::from(vec![
                Span::styled("  Disk:   ", Style::default().fg(Color::DarkGray)),
                Span::raw(format!(
                    "{:.1} / {:.1} GB available on {}",
                    info.disk_available_gb, info.disk_total_gb, self.mount_point
                )),
            ]),
            Line::from(vec![
                Span::styled("  Network: ", Style::default().fg(Color::DarkGray)),
                Span::styled(network_line, network_style),
            ]),
        ];

        let block = Block::default()
            .title(" System ")
            .title_alignment(Alignment::Left)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray));
        let paragraph = Paragraph::new(lines).block(block);
        f.render_widget(paragraph, area);
    }

    fn render_services(&self, f: &mut ratatui::Frame, area: Rect) {
        match self.panel_view {
            PanelView::Auto => {
                // Startup: show journal with "services starting" header
                self.render_journal(f, area);
                return;
            }
            PanelView::Log => {
                self.render_journal(f, area);
                return;
            }
            PanelView::Status => {
                // Fall through to render service list
            }
        }

        let block = Block::default()
            .title(" Services [l: log] ")
            .title_alignment(Alignment::Left)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray));

        match &self.all_services {
            Ok(services) if services.is_empty() => {
                let paragraph = Paragraph::new("  No services found")
                    .style(Style::default().fg(Color::DarkGray))
                    .block(block);
                f.render_widget(paragraph, area);
            }
            Ok(services) => {
                let inner_height = area.height.saturating_sub(2) as usize;
                let visible = services.iter().take(inner_height);

                let items: Vec<ListItem> = visible
                    .map(|svc| {
                        let state_style = match svc.active_state.as_str() {
                            "active" => Style::default().fg(Color::Green),
                            "failed" => Style::default().fg(Color::Red),
                            "activating" | "deactivating" | "reloading" => {
                                Style::default().fg(Color::Yellow)
                            }
                            _ => Style::default().fg(Color::DarkGray),
                        };

                        let line = Line::from(vec![
                            Span::raw(format!("  {:<30} ", svc.name)),
                            Span::styled(
                                format!("{:<12}", svc.active_state),
                                state_style,
                            ),
                            Span::styled(
                                svc.description.clone(),
                                Style::default().fg(Color::DarkGray),
                            ),
                        ]);
                        ListItem::new(line)
                    })
                    .collect();

                let list = List::new(items).block(block);
                f.render_widget(list, area);
            }
            Err(err) => {
                let lines = vec![
                    Line::from(Span::styled(
                        format!("  {}", err),
                        Style::default().fg(Color::Yellow),
                    )),
                    Line::from(""),
                    Line::from(Span::styled(
                        "  Services may still be starting...",
                        Style::default().fg(Color::DarkGray),
                    )),
                ];
                let paragraph = Paragraph::new(lines).block(block);
                f.render_widget(paragraph, area);
            }
        }
    }

    fn render_journal(&self, f: &mut ratatui::Frame, area: Rect) {
        let (title, border_color) = if self.panel_view == PanelView::Auto {
            (" Services starting — live journal [s: status] ", Color::Yellow)
        } else {
            (" Journal [s: status] ", Color::DarkGray)
        };
        let block = Block::default()
            .title(title)
            .title_alignment(Alignment::Left)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(border_color));

        let inner_height = area.height.saturating_sub(2) as usize;
        let start = self.journal_lines.len().saturating_sub(inner_height);
        let visible: Vec<Line> = self.journal_lines[start..]
            .iter()
            .map(|line| {
                let style = if line.contains("error")
                    || line.contains("Error")
                    || line.contains("FAILED")
                    || line.contains("failed")
                {
                    Style::default().fg(Color::Red)
                } else if line.contains("Started") || line.contains("Reached target") {
                    Style::default().fg(Color::Green)
                } else {
                    Style::default().fg(Color::DarkGray)
                };
                Line::from(Span::styled(format!("  {}", line), style))
            })
            .collect();

        let paragraph = Paragraph::new(visible).block(block);
        f.render_widget(paragraph, area);
    }

    fn render_actions(&self, f: &mut ratatui::Frame, area: Rect) {
        let actions = Paragraph::new(
            "  [.] Login   [l] Log   [s] Status   [r] Reconfigure   [R] Reboot   [p] Power Off   [!] Wipe",
        )
        .alignment(Alignment::Center)
        .style(Style::default().fg(Color::White))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::DarkGray)),
        );
        f.render_widget(actions, area);
    }

    fn render_sledgehammer_confirm(&self, f: &mut ratatui::Frame, area: Rect) {
        let input = self
            .sledgehammer_input
            .as_deref()
            .unwrap_or("");
        let text = format!(
            "  Type SLEDGEHAMMER to wipe all data and reboot: {}_    (Esc to cancel)",
            input
        );
        let paragraph = Paragraph::new(text)
            .style(
                Style::default()
                    .fg(Color::Red)
                    .add_modifier(Modifier::BOLD),
            )
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Red)),
            );
        f.render_widget(paragraph, area);
    }
}

/// Set a file descriptor to non-blocking mode using fcntl.
fn set_nonblocking(stdout: &impl std::os::unix::io::AsRawFd) {
    let fd = std::os::unix::io::AsRawFd::as_raw_fd(stdout);
    let flags = nix::fcntl::fcntl(fd, nix::fcntl::FcntlArg::F_GETFL)
        .unwrap_or(0);
    let new_flags = nix::fcntl::OFlag::from_bits_truncate(flags)
        | nix::fcntl::OFlag::O_NONBLOCK;
    let _ = nix::fcntl::fcntl(fd, nix::fcntl::FcntlArg::F_SETFL(new_flags));
}

/// Discover btrfs member devices for a mount point.
fn discover_btrfs_devices(mount_point: &str) -> Vec<String> {
    match run_cmd("btrfs", &["filesystem", "show", mount_point]) {
        Ok(output) => {
            let mut devices = Vec::new();
            for line in output.lines() {
                let trimmed = line.trim();
                if trimmed.contains("path ") {
                    if let Some(path) = trimmed.rsplit("path ").next() {
                        let dev = path.trim().to_string();
                        if dev.starts_with("/dev/") {
                            let disk = strip_partition_suffix(&dev);
                            if !devices.contains(&disk) {
                                devices.push(disk);
                            }
                        }
                    }
                }
            }
            devices
        }
        Err(_) => Vec::new(),
    }
}

/// Strip partition suffix from a device path to get the parent disk.
fn strip_partition_suffix(device: &str) -> String {
    if device.contains("nvme") {
        if let Some(pos) = device.rfind('p') {
            let after_p = &device[pos + 1..];
            let before_p = &device[..pos];
            if !after_p.is_empty()
                && after_p.chars().all(|c| c.is_ascii_digit())
                && before_p.ends_with(|c: char| c.is_ascii_digit())
            {
                return before_p.to_string();
            }
        }
        return device.to_string();
    }
    let trimmed = device.trim_end_matches(|c: char| c.is_ascii_digit());
    if trimmed.len() < device.len()
        && trimmed.len() > "/dev/".len()
        && trimmed.ends_with(|c: char| c.is_ascii_alphabetic())
    {
        return trimmed.to_string();
    }
    device.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::executor::MockExecutor;

    #[test]
    fn test_key_login() {
        let mut app = test_app();
        let key = KeyEvent::from(KeyCode::Char('.'));
        assert_eq!(app.map_key(key), GettyAction::Login);
    }

    #[test]
    fn test_key_log_panel() {
        let mut app = test_app();
        let key = KeyEvent::from(KeyCode::Char('l'));
        assert_eq!(app.map_key(key), GettyAction::None);
        assert_eq!(app.panel_view, PanelView::Log);
    }

    #[test]
    fn test_key_status_panel() {
        let mut app = test_app();
        app.panel_view = PanelView::Log;
        let key = KeyEvent::from(KeyCode::Char('s'));
        assert_eq!(app.map_key(key), GettyAction::None);
        assert_eq!(app.panel_view, PanelView::Status);
    }

    #[test]
    fn test_panel_auto_to_status_when_all_active() {
        let mut app = test_app();
        app.panel_view = PanelView::Auto;
        app.system_services = Ok(vec![
            ServiceInfo { name: "a.service".into(), active_state: "active".into(), description: String::new() },
        ]);
        app.manage_journal();
        assert_eq!(app.panel_view, PanelView::Status);
    }

    #[test]
    fn test_panel_auto_stays_when_not_all_active() {
        let mut app = test_app();
        app.panel_view = PanelView::Auto;
        app.system_services = Ok(vec![
            ServiceInfo { name: "a.service".into(), active_state: "activating".into(), description: String::new() },
        ]);
        app.manage_journal();
        assert_eq!(app.panel_view, PanelView::Auto);
    }

    #[test]
    fn test_key_reconfigure() {
        let mut app = test_app();
        let key = KeyEvent::from(KeyCode::Char('r'));
        assert_eq!(app.map_key(key), GettyAction::Reconfigure);
    }

    #[test]
    fn test_key_reboot() {
        let mut app = test_app();
        let key = KeyEvent::from(KeyCode::Char('R'));
        assert_eq!(app.map_key(key), GettyAction::Reboot);
    }

    #[test]
    fn test_key_poweroff() {
        let mut app = test_app();
        let key = KeyEvent::from(KeyCode::Char('p'));
        assert_eq!(app.map_key(key), GettyAction::PowerOff);
    }

    #[test]
    fn test_key_sledgehammer_enter_mode() {
        let mut app = test_app();
        let key = KeyEvent::from(KeyCode::Char('!'));
        assert_eq!(app.map_key(key), GettyAction::None);
        assert!(app.sledgehammer_input.is_some());
    }

    #[test]
    fn test_key_sledgehammer_cancel() {
        let mut app = test_app();
        app.sledgehammer_input = Some(String::new());
        let key = KeyEvent::from(KeyCode::Esc);
        app.map_key(key);
        assert!(app.sledgehammer_input.is_none());
    }

    #[test]
    fn test_key_sledgehammer_type_and_confirm() {
        let mut app = test_app();
        app.sledgehammer_input = Some(String::new());

        for c in "SLEDGEHAMMER".chars() {
            let key = KeyEvent::from(KeyCode::Char(c));
            assert_eq!(app.map_key(key), GettyAction::None);
        }

        assert_eq!(
            app.sledgehammer_input.as_deref(),
            Some("SLEDGEHAMMER")
        );

        let enter = KeyEvent::from(KeyCode::Enter);
        assert_eq!(app.map_key(enter), GettyAction::Sledgehammer);
        assert!(app.sledgehammer_input.is_none());
    }

    #[test]
    fn test_key_sledgehammer_wrong_text() {
        let mut app = test_app();
        app.sledgehammer_input = Some("WRONG".to_string());

        let enter = KeyEvent::from(KeyCode::Enter);
        assert_eq!(app.map_key(enter), GettyAction::None);
        assert_eq!(app.sledgehammer_input.as_deref(), Some(""));
    }

    #[test]
    fn test_key_sledgehammer_backspace() {
        let mut app = test_app();
        app.sledgehammer_input = Some("SLE".to_string());

        let key = KeyEvent::from(KeyCode::Backspace);
        app.map_key(key);
        assert_eq!(app.sledgehammer_input.as_deref(), Some("SL"));
    }

    #[test]
    fn test_key_unknown() {
        let mut app = test_app();
        let key = KeyEvent::from(KeyCode::F(1));
        assert_eq!(app.map_key(key), GettyAction::None);
    }

    #[test]
    fn test_key_ctrl_c_quits() {
        let mut app = test_app();
        let key = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        app.map_key(key);
        assert!(app.should_quit);
    }

    #[test]
    fn test_execute_reboot() {
        let mut app = test_app();
        let mut executor = MockExecutor::new(vec![]);
        app.execute_action(&GettyAction::Reboot, &mut executor);
        let ops = executor.recorded_operations();
        assert_eq!(ops.len(), 1);
        assert!(matches!(ops[0].operation, Operation::Reboot));
    }

    #[test]
    fn test_execute_poweroff() {
        let mut app = test_app();
        let mut executor = MockExecutor::new(vec![]);
        app.execute_action(&GettyAction::PowerOff, &mut executor);
        let ops = executor.recorded_operations();
        assert_eq!(ops.len(), 1);
        assert!(matches!(ops[0].operation, Operation::PowerOff));
    }

    #[test]
    fn test_execute_sledgehammer_records_stop_unmount_and_reboot() {
        let mut app = test_app();
        let mut executor = MockExecutor::new(vec![]);
        app.execute_action(&GettyAction::Sledgehammer, &mut executor);
        let ops = executor.recorded_operations();
        assert!(ops.len() >= 3);
        assert!(matches!(ops[0].operation, Operation::StopAllContainers));
        assert!(matches!(ops[1].operation, Operation::CleanupUnmount { .. }));
        assert!(matches!(ops[ops.len() - 1].operation, Operation::Reboot));
    }

    #[test]
    fn test_strip_partition_suffix_sda1() {
        assert_eq!(strip_partition_suffix("/dev/sda1"), "/dev/sda");
    }

    #[test]
    fn test_strip_partition_suffix_sda() {
        assert_eq!(strip_partition_suffix("/dev/sda"), "/dev/sda");
    }

    #[test]
    fn test_strip_partition_suffix_nvme() {
        assert_eq!(strip_partition_suffix("/dev/nvme0n1p1"), "/dev/nvme0n1");
    }

    #[test]
    fn test_strip_partition_suffix_nvme_no_partition() {
        assert_eq!(strip_partition_suffix("/dev/nvme0n1"), "/dev/nvme0n1");
    }

    #[test]
    fn test_discover_btrfs_devices_empty() {
        let devices = discover_btrfs_devices("/nonexistent_mount_xyz");
        assert!(devices.is_empty());
    }

    fn test_app() -> GettyApp {
        GettyApp {
            system_info: SystemInfo {
                hostname: "testbox".to_string(),
                kernel_version: "6.1.0".to_string(),
                architecture: "x86_64".to_string(),
                cpu_model: "Test CPU".to_string(),
                cpu_cores: 4,
                load_average: 0.5,
                mem_total_mb: 8192,
                mem_used_mb: 2048,
                disk_total_gb: 500.0,
                disk_used_gb: 100.0,
                disk_available_gb: 400.0,
                network_online: true,
                ip_address: Some("192.168.1.100".to_string()),
                default_interface: Some("eth0".to_string()),
                mdns_url: "testbox.local".to_string(),
                town_os_version: Some("1.0".to_string()),
            },
            system_services: Ok(vec![]),
            all_services: Ok(vec![]),
            api_client: TownApiClient::new(None),
            etc_prefix: None,
            tty: None,
            mount_point: "/town-os".to_string(),
            sledgehammer_input: None,
            should_quit: false,
            panel_view: PanelView::Auto,
            last_fast_refresh: Instant::now(),
            last_slow_refresh: Instant::now(),
            journal_lines: Vec::new(),
            journal_child: None,
            journal_max_lines: 200,
        }
    }

    #[test]
    fn test_all_services_active_empty() {
        let app = test_app();
        // Empty services list = vacuously all active (nothing to wait for)
        assert!(app.all_services_active());
    }

    #[test]
    fn test_all_services_active_all_active() {
        let mut app = test_app();
        app.system_services = Ok(vec![
            ServiceInfo { name: "a.service".into(), active_state: "active".into(), description: String::new() },
            ServiceInfo { name: "b.service".into(), active_state: "active".into(), description: String::new() },
        ]);
        assert!(app.all_services_active());
    }

    #[test]
    fn test_all_services_active_some_activating() {
        let mut app = test_app();
        app.system_services = Ok(vec![
            ServiceInfo { name: "a.service".into(), active_state: "active".into(), description: String::new() },
            ServiceInfo { name: "b.service".into(), active_state: "activating".into(), description: String::new() },
        ]);
        assert!(!app.all_services_active());
    }

    #[test]
    fn test_all_services_active_api_error() {
        let mut app = test_app();
        app.system_services = Err("API unavailable".into());
        assert!(!app.all_services_active());
    }
}
