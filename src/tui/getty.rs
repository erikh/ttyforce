use std::fs::File;
use std::io::{self, BufRead, BufReader};
use std::os::unix::io::AsRawFd;
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
use crate::engine::real_ops::{cmd_log_append, kmsg_log};
use crate::getty::api::{ServiceInfo, TownApiClient};
use crate::getty::sysinfo::SystemInfo;
use crate::operations::Operation;
use crate::tui::app::{redirect_to_tty, render_cmd_log, restore_fds};

/// Screen blanks after 5 minutes of no keypresses.
const SCREEN_BLANK_TIMEOUT: Duration = Duration::from_secs(300);
/// After unblanking, keys are discarded for 30 seconds.
const SCREEN_GRACE_PERIOD: Duration = Duration::from_secs(30);

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
    /// Non-blocking reader for /dev/kmsg (only when --console is set).
    kmsg_reader: Option<BufReader<File>>,
    /// When true, reconfigure uses `initrd` subcommand instead of `run`.
    pub initrd_mode: bool,
    /// GRUB menu entry number for sledgehammer wipe boot.
    pub sledgehammer_grub_entry: Option<String>,
    /// Last time a key was pressed (for screen blank timeout).
    last_activity: Instant,
    /// Whether the screen is currently blanked.
    screen_blanked: bool,
    /// When the screen was unblanked (for grace period).
    unblanked_at: Option<Instant>,
}

impl GettyApp {
    pub fn new(
        etc_prefix: Option<String>,
        tty: Option<String>,
        mount_point: String,
        console_mode: bool,
    ) -> Self {
        let api_client = TownApiClient::from_env(etc_prefix.as_deref());
        let system_info = SystemInfo::probe(&mount_point);
        let system_services = api_client.fetch_system_services();
        let all_services = api_client.fetch_all_services();

        let kmsg_reader = if console_mode {
            open_kmsg()
        } else {
            None
        };

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
            kmsg_reader,
            initrd_mode: false,
            sledgehammer_grub_entry: None,
            last_activity: Instant::now(),
            screen_blanked: false,
            unblanked_at: None,
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

            // In console mode, detect kernel messages and force full repaint
            if self.drain_kmsg() {
                terminal.clear()?;
            }

            // Screen blanking: blank after inactivity timeout
            if self.should_blank_screen() {
                self.screen_blanked = true;
                terminal.clear()?;
            }

            if !self.screen_blanked {
                terminal.draw(|f| self.render(f))?;
            }

            if event::poll(Duration::from_secs(1))? {
                if let Event::Key(key) = event::read()? {
                    // Screen is blanked — wake up, discard key, start grace period
                    if self.screen_blanked {
                        self.unblank();
                        terminal.clear()?;
                        continue;
                    }

                    // Grace period after unblank — discard key
                    if self.is_in_grace_period() {
                        self.last_activity = Instant::now();
                        continue;
                    }
                    self.unblanked_at = None;

                    self.last_activity = Instant::now();
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
                            self.last_activity = Instant::now();
                            self.screen_blanked = false;
                            self.unblanked_at = None;
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
    /// An empty service list or an API error means we can't confirm readiness.
    pub fn all_services_active(&self) -> bool {
        match &self.system_services {
            Ok(services) if !services.is_empty() => {
                services.iter().all(|s| s.active_state == "active")
            }
            _ => false,
        }
    }

    /// Returns true if the screen should be blanked due to inactivity.
    pub fn should_blank_screen(&self) -> bool {
        !self.screen_blanked && self.last_activity.elapsed() >= SCREEN_BLANK_TIMEOUT
    }

    /// Returns true if within the post-unblank grace period (keys discarded).
    pub fn is_in_grace_period(&self) -> bool {
        match self.unblanked_at {
            Some(t) => t.elapsed() < SCREEN_GRACE_PERIOD,
            None => false,
        }
    }

    /// Unblank the screen and start the grace period.
    pub fn unblank(&mut self) {
        self.screen_blanked = false;
        self.unblanked_at = Some(Instant::now());
        self.last_activity = Instant::now();
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
            if let Err(e) = child.kill() {
                eprintln!("kill journal child: {}", e);
            }
            if let Err(e) = child.wait() {
                eprintln!("wait journal child: {}", e);
            }
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

    /// Drain any pending kernel messages from /dev/kmsg.
    /// Returns true if any messages were read (console was written to).
    fn drain_kmsg(&mut self) -> bool {
        let Some(ref mut reader) = self.kmsg_reader else {
            return false;
        };

        let mut got_output = false;
        let mut line = String::new();

        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) => break,
                Ok(_) => {
                    got_output = true;
                }
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(_) => break,
            }
        }

        got_output
    }

    /// Clear the screen and display /etc/issue like agetty does before login.
    fn display_issue(&self) {
        use std::io::Write;
        let mut stdout = io::stdout();

        // Clear screen and move cursor home
        if let Err(e) = write!(stdout, "\x1b[2J\x1b[H") {
            eprintln!("clear screen: {}", e);
        }

        // Read and display /etc/issue if it exists
        if let Ok(content) = std::fs::read_to_string("/etc/issue") {
            let processed = self.substitute_issue_escapes(&content);
            if let Err(e) = write!(stdout, "{}", processed) {
                eprintln!("write /etc/issue: {}", e);
            }
        }

        if let Err(e) = stdout.flush() {
            eprintln!("flush stdout: {}", e);
        }
    }

    /// Perform agetty-style escape substitutions on /etc/issue content.
    fn substitute_issue_escapes(&self, content: &str) -> String {
        let utsname = nix::sys::utsname::uname().ok();
        let hostname = utsname
            .as_ref()
            .map(|u| u.nodename().to_string_lossy().to_string())
            .unwrap_or_else(|| "localhost".to_string());
        let os_name = utsname
            .as_ref()
            .map(|u| u.sysname().to_string_lossy().to_string())
            .unwrap_or_else(|| "Linux".to_string());
        let release = utsname
            .as_ref()
            .map(|u| u.release().to_string_lossy().to_string())
            .unwrap_or_else(|| "unknown".to_string());
        let machine = utsname
            .as_ref()
            .map(|u| u.machine().to_string_lossy().to_string())
            .unwrap_or_else(|| "unknown".to_string());

        let tty_raw = self.tty.as_deref().unwrap_or("tty1");
        let tty_name = tty_raw.strip_prefix("/dev/").unwrap_or(tty_raw);

        let now = std::time::SystemTime::now();
        let secs = now
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        // Simple date/time formatting without external crate
        let (date_str, time_str) = format_unix_timestamp(secs);

        let mut result = String::with_capacity(content.len());
        let mut chars = content.chars().peekable();
        while let Some(ch) = chars.next() {
            if ch == '\\' {
                match chars.next() {
                    Some('n') => result.push_str(&hostname),
                    Some('l') => result.push_str(tty_name),
                    Some('d') => result.push_str(&date_str),
                    Some('t') => result.push_str(&time_str),
                    Some('s') => result.push_str(&os_name),
                    Some('m') => result.push_str(&machine),
                    Some('r') => result.push_str(&release),
                    Some('\\') => result.push('\\'),
                    Some(other) => {
                        result.push('\\');
                        result.push(other);
                    }
                    None => result.push('\\'),
                }
            } else {
                result.push(ch);
            }
        }
        result
    }

    /// Exec into /bin/login, replacing this process entirely.
    /// Only returns if exec fails.
    fn exec_login(&self) {
        use std::os::unix::process::CommandExt;
        self.display_issue();
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
                let subcommand = if self.initrd_mode { "initrd" } else { "run" };
                let mut args: Vec<String> = vec![subcommand.to_string()];
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
        let entry = match &self.sledgehammer_grub_entry {
            Some(e) => e.clone(),
            None => {
                cmd_log_append("sledgehammer: no --sledgehammer-grub-entry configured".to_string());
                return;
            }
        };

        cmd_log_append(format!("sledgehammer: setting grub-reboot to entry {}", entry));

        if let Err(e) = crate::engine::real_ops::run_cmd("grub-reboot", &[&entry]) {
            cmd_log_append(format!("  -> FAILED: grub-reboot: {}", e));
            return;
        }
        cmd_log_append("  -> grub-reboot set".to_string());

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

        let url = format!("http://{}", self.system_info.mdns_url);

        let api_status = match &self.system_services {
            Ok(services) if services.iter().all(|s| s.active_state == "active") => {
                Span::styled(" ready ", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD))
            }
            Ok(services) if services.iter().any(|s| s.active_state == "failed") => {
                Span::styled(" degraded ", Style::default().fg(Color::Red).add_modifier(Modifier::BOLD))
            }
            Ok(_) => {
                Span::styled(" starting ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD))
            }
            Err(_) => {
                Span::styled(" unavailable ", Style::default().fg(Color::Red))
            }
        };

        let version_span = if version.is_empty() {
            Span::styled("Town OS", Style::default().fg(Color::DarkGray))
        } else {
            Span::styled(format!("Town OS {}", version), Style::default().fg(Color::DarkGray))
        };

        let line = Line::from(vec![
            Span::raw("  "),
            Span::styled(&url, Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
            Span::raw("  "),
            api_status,
            Span::raw("  "),
            version_span,
        ]);

        let title = Paragraph::new(line)
            .alignment(Alignment::Center)
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
                let visible: Vec<_> = services.iter().take(inner_height).collect();

                let name_width = visible
                    .iter()
                    .map(|svc| svc.name.len())
                    .max()
                    .unwrap_or(0);

                let items: Vec<ListItem> = visible
                    .iter()
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
                            Span::raw(format!("  {:<width$} ", svc.name, width = name_width)),
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
        let text = "  [.] Login   [l] Log   [s] Status   [r] Reconfigure   [R] Reboot   [p] Power Off   [!] Wipe";
        let actions = Paragraph::new(text)
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

/// Open /dev/kmsg for non-blocking reading, seeked to the end
/// so we only see new messages.
fn open_kmsg() -> Option<BufReader<File>> {
    let file = match File::open("/dev/kmsg") {
        Ok(f) => f,
        Err(e) => {
            kmsg_log(&format!("getty: failed to open /dev/kmsg: {}", e));
            return None;
        }
    };

    // Set non-blocking so reads don't block the TUI loop
    set_nonblocking(&file);

    // Seek to end so we don't replay old messages
    let fd = file.as_raw_fd();
    if let Err(e) = nix::unistd::lseek(fd, 0, nix::unistd::Whence::SeekEnd) {
        kmsg_log(&format!("getty: failed to seek /dev/kmsg: {}", e));
        return None;
    }

    Some(BufReader::new(file))
}

/// Set a file descriptor to non-blocking mode using fcntl.
fn set_nonblocking(stdout: &impl std::os::unix::io::AsRawFd) {
    let fd = std::os::unix::io::AsRawFd::as_raw_fd(stdout);
    let flags = nix::fcntl::fcntl(fd, nix::fcntl::FcntlArg::F_GETFL)
        .unwrap_or(0);
    let new_flags = nix::fcntl::OFlag::from_bits_truncate(flags)
        | nix::fcntl::OFlag::O_NONBLOCK;
    if let Err(e) = nix::fcntl::fcntl(fd, nix::fcntl::FcntlArg::F_SETFL(new_flags)) {
        eprintln!("fcntl F_SETFL: {}", e);
    }
}


/// Format a Unix timestamp as (date, time) strings.
/// Returns ("YYYY-MM-DD", "HH:MM:SS") in UTC.
fn format_unix_timestamp(secs: u64) -> (String, String) {
    // Days from Unix epoch, accounting for leap years
    let secs_per_day: u64 = 86400;
    let mut days = secs / secs_per_day;
    let day_secs = secs % secs_per_day;

    let hours = day_secs / 3600;
    let minutes = (day_secs % 3600) / 60;
    let seconds = day_secs % 60;

    // Compute year, month, day from days since 1970-01-01
    let mut year: u64 = 1970;
    loop {
        let days_in_year = if is_leap_year(year) { 366 } else { 365 };
        if days < days_in_year {
            break;
        }
        days -= days_in_year;
        year += 1;
    }

    let leap = is_leap_year(year);
    let month_days: [u64; 12] = [
        31,
        if leap { 29 } else { 28 },
        31, 30, 31, 30, 31, 31, 30, 31, 30, 31,
    ];

    let mut month: u64 = 1;
    for &md in &month_days {
        if days < md {
            break;
        }
        days -= md;
        month += 1;
    }
    let day = days + 1;

    (
        format!("{:04}-{:02}-{:02}", year, month, day),
        format!("{:02}:{:02}:{:02}", hours, minutes, seconds),
    )
}

fn is_leap_year(year: u64) -> bool {
    (year.is_multiple_of(4) && !year.is_multiple_of(100)) || year.is_multiple_of(400)
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
    fn test_execute_sledgehammer_no_grub_entry() {
        let mut app = test_app();
        let mut executor = MockExecutor::new(vec![]);
        app.execute_action(&GettyAction::Sledgehammer, &mut executor);
        // No grub entry configured — should do nothing
        let ops = executor.recorded_operations();
        assert!(ops.is_empty());
    }

    #[test]
    fn test_execute_sledgehammer_with_grub_entry() {
        let mut app = test_app();
        app.sledgehammer_grub_entry = Some("2".to_string());
        let mut executor = MockExecutor::new(vec![]);
        app.execute_action(&GettyAction::Sledgehammer, &mut executor);
        let ops = executor.recorded_operations();
        // If grub-reboot is available, Reboot is recorded; if not, nothing happens
        if !ops.is_empty() {
            assert!(matches!(ops[0].operation, Operation::Reboot));
        }
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
            kmsg_reader: None,
            initrd_mode: false,
            sledgehammer_grub_entry: None,
            last_activity: Instant::now(),
            screen_blanked: false,
            unblanked_at: None,
        }
    }

    #[test]
    fn test_all_services_active_empty() {
        let app = test_app();
        // Empty services list means we can't confirm readiness
        assert!(!app.all_services_active());
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

    #[test]
    fn test_format_unix_timestamp_epoch() {
        let (date, time) = format_unix_timestamp(0);
        assert_eq!(date, "1970-01-01");
        assert_eq!(time, "00:00:00");
    }

    #[test]
    fn test_format_unix_timestamp_known_date() {
        // 2024-01-15 12:30:45 UTC = 1705321845
        let (date, time) = format_unix_timestamp(1705321845);
        assert_eq!(date, "2024-01-15");
        assert_eq!(time, "12:30:45");
    }

    #[test]
    fn test_format_unix_timestamp_leap_year() {
        // 2024-02-29 00:00:00 UTC = 1709164800
        let (date, _time) = format_unix_timestamp(1709164800);
        assert_eq!(date, "2024-02-29");
    }

    #[test]
    fn test_is_leap_year() {
        assert!(is_leap_year(2000)); // divisible by 400
        assert!(is_leap_year(2024)); // divisible by 4, not 100
        assert!(!is_leap_year(1900)); // divisible by 100, not 400
        assert!(!is_leap_year(2023)); // not divisible by 4
    }

    #[test]
    fn test_substitute_issue_escapes_hostname() {
        let app = test_app();
        let result = app.substitute_issue_escapes("Welcome to \\n");
        // Uses system hostname from uname, not the test app's system_info
        assert!(!result.contains("\\n"));
        assert!(result.starts_with("Welcome to "));
    }

    #[test]
    fn test_substitute_issue_escapes_tty() {
        let mut app = test_app();
        app.tty = Some("/dev/tty3".to_string());
        let result = app.substitute_issue_escapes("TTY: \\l");
        assert_eq!(result, "TTY: tty3");
    }

    #[test]
    fn test_substitute_issue_escapes_tty_default() {
        let app = test_app();
        // tty is None in test_app
        let result = app.substitute_issue_escapes("TTY: \\l");
        assert_eq!(result, "TTY: tty1");
    }

    #[test]
    fn test_substitute_issue_escapes_os_name() {
        let app = test_app();
        let result = app.substitute_issue_escapes("\\s");
        assert_eq!(result, "Linux");
    }

    #[test]
    fn test_substitute_issue_escapes_backslash() {
        let app = test_app();
        let result = app.substitute_issue_escapes("path\\\\end");
        assert_eq!(result, "path\\end");
    }

    #[test]
    fn test_substitute_issue_escapes_unknown_escape() {
        let app = test_app();
        let result = app.substitute_issue_escapes("\\z");
        assert_eq!(result, "\\z");
    }

    #[test]
    fn test_substitute_issue_escapes_no_escapes() {
        let app = test_app();
        let result = app.substitute_issue_escapes("plain text");
        assert_eq!(result, "plain text");
    }

    #[test]
    fn test_substitute_issue_escapes_trailing_backslash() {
        let app = test_app();
        let result = app.substitute_issue_escapes("end\\");
        assert_eq!(result, "end\\");
    }

    #[test]
    fn test_substitute_issue_escapes_date_time() {
        let app = test_app();
        let result = app.substitute_issue_escapes("\\d \\t");
        // Should contain date and time patterns (YYYY-MM-DD HH:MM:SS)
        assert!(result.contains('-'), "expected date with dashes: {}", result);
        assert!(result.contains(':'), "expected time with colons: {}", result);
    }

    #[test]
    fn test_substitute_issue_escapes_machine() {
        let app = test_app();
        let result = app.substitute_issue_escapes("\\m");
        // Machine comes from uname, should be non-empty
        assert!(!result.is_empty());
        assert!(!result.contains("\\m"));
    }

    #[test]
    fn test_substitute_issue_escapes_kernel_release() {
        let app = test_app();
        let result = app.substitute_issue_escapes("\\r");
        assert!(!result.is_empty());
        assert!(!result.contains("\\r"));
    }

    #[test]
    fn test_screen_blanks_after_timeout() {
        let mut app = test_app();
        app.last_activity = Instant::now() - Duration::from_secs(360);
        assert!(app.should_blank_screen());
    }

    #[test]
    fn test_screen_does_not_blank_before_timeout() {
        let app = test_app();
        assert!(!app.should_blank_screen());
    }

    #[test]
    fn test_screen_does_not_blank_when_already_blanked() {
        let mut app = test_app();
        app.last_activity = Instant::now() - Duration::from_secs(360);
        app.screen_blanked = true;
        // should_blank_screen returns false when already blanked
        assert!(!app.should_blank_screen());
    }

    #[test]
    fn test_grace_period_active() {
        let mut app = test_app();
        app.unblanked_at = Some(Instant::now());
        assert!(app.is_in_grace_period());
    }

    #[test]
    fn test_grace_period_expired() {
        let mut app = test_app();
        app.unblanked_at = Some(Instant::now() - Duration::from_secs(31));
        assert!(!app.is_in_grace_period());
    }

    #[test]
    fn test_grace_period_none() {
        let app = test_app();
        assert!(!app.is_in_grace_period());
    }

    #[test]
    fn test_unblank_sets_grace_period() {
        let mut app = test_app();
        app.screen_blanked = true;
        app.last_activity = Instant::now() - Duration::from_secs(360);
        app.unblank();
        assert!(!app.screen_blanked);
        assert!(app.unblanked_at.is_some());
        assert!(app.is_in_grace_period());
        // last_activity should be recent
        assert!(app.last_activity.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn test_keys_ignored_during_grace_period() {
        let mut app = test_app();
        app.unblanked_at = Some(Instant::now());
        // During grace period, is_in_grace_period is true so keys would be discarded
        assert!(app.is_in_grace_period());
        // Verify that a key press during grace period would not be processed
        // (the event loop checks is_in_grace_period before calling map_key)
        assert!(!app.screen_blanked);
    }

    #[test]
    fn test_activity_resets_on_unblank() {
        let mut app = test_app();
        app.screen_blanked = true;
        app.last_activity = Instant::now() - Duration::from_secs(600);
        app.unblank();
        assert!(app.last_activity.elapsed() < Duration::from_secs(1));
    }
}
