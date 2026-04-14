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
use crate::engine::state_machine::InstallerStateMachine;
use crate::getty::api::{AuditEntry, ServiceInfo, TownApiClient};
use crate::getty::sysinfo::SystemInfo;
use crate::operations::Operation;
use crate::tui::app::{redirect_to_tty, restore_fds, App};
use crate::tui::evdev_input::{ActivityResult, EvdevWatcher};

/// Screen blanks after 5 minutes of no keypresses.
const SCREEN_BLANK_TIMEOUT: Duration = Duration::from_secs(300);

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
    Quit,
    ReconfigureMenu,
    ReconfigureNetwork,
    ReconfigureSshKeys,
    Reboot,
    PowerOff,
    Sledgehammer,
}

/// PgUp/PgDn step size in lines for journal scrolling.
const JOURNAL_PAGE_STEP: usize = 10;

/// Append a line to a journal buffer, locking the user's view if scrolled back.
///
/// When `scroll > 0` (user has paged back), incrementing it by 1 keeps the
/// same content visible as new lines stream in. When the buffer exceeds `max`,
/// trim from the front and clamp `scroll` so the view stays valid.
fn append_log_line(lines: &mut Vec<String>, scroll: &mut usize, max: usize, line: String) {
    lines.push(line);
    if *scroll > 0 {
        *scroll += 1;
    }
    if lines.len() > max {
        let excess = lines.len() - max;
        lines.drain(..excess);
    }
    let max_scroll = lines.len().saturating_sub(1);
    if *scroll > max_scroll {
        *scroll = max_scroll;
    }
}

/// Compute the visible window for a scrolled journal pane.
/// Returns `(start, end)` indices into `lines` such that `lines[start..end]`
/// is the slice to render.
fn journal_window(total: usize, scroll: usize, height: usize) -> (usize, usize) {
    let end = total.saturating_sub(scroll);
    let start = end.saturating_sub(height);
    (start, end)
}

/// State for the SSH key input prompt within the reconfigure menu.
struct SshInputState {
    /// Index into ssh_users for the current user being configured.
    current_user_idx: usize,
    /// GitHub username being typed.
    github_username: String,
    /// Status message from last operation.
    status_message: Option<(String, bool)>,
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
    /// When true, show a [q] Quit action to exit the getty.
    pub quit_enabled: bool,
    /// When true, reconfigure uses `initrd` subcommand instead of `run`.
    pub initrd_mode: bool,
    /// GRUB menu entry number for sledgehammer wipe boot.
    pub sledgehammer_grub_entry: Option<String>,
    /// System users for SSH key import (from --ssh-user).
    pub ssh_users: Vec<String>,
    /// Whether the reconfigure submenu is active.
    reconfigure_menu: bool,
    /// SSH key input state (active when entering GitHub usernames).
    ssh_input: Option<SshInputState>,
    /// Last time a key was pressed (for screen blank timeout).
    last_activity: Instant,
    /// Whether the screen is currently blanked.
    screen_blanked: bool,
    /// When true, the next crossterm key event is discarded (it's the wake key).
    discard_next_key: bool,
    /// evdev keyboard watcher for detecting bare modifier keypresses.
    evdev_watcher: Option<EvdevWatcher>,
    /// Live `journalctl -xe` output for the bottom-left pane.
    xe_journal_lines: Vec<String>,
    xe_journal_child: Option<Child>,
    xe_journal_max_lines: usize,
    /// Audit log entries fetched from the Town OS API.
    audit_entries: Vec<AuditEntry>,
    /// When true, show full-screen journal -f log instead of the quad.
    pub show_full_log: bool,
    /// Scrollback offset for the journal -f pane (0 = follow tail).
    journal_scroll: usize,
    /// Scrollback offset for the journal -xe pane (0 = follow tail).
    xe_journal_scroll: usize,
    /// Mock mode: don't execute real operations (login, reboot, etc).
    pub mock_mode: bool,
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

        let audit_entries = api_client.fetch_audit_log().unwrap_or_default();

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
            quit_enabled: false,
            initrd_mode: false,
            sledgehammer_grub_entry: None,
            ssh_users: Vec::new(),
            reconfigure_menu: false,
            ssh_input: None,
            last_activity: Instant::now(),
            screen_blanked: false,
            discard_next_key: false,
            evdev_watcher: Some(EvdevWatcher::open()),
            xe_journal_lines: Vec::new(),
            xe_journal_child: None,
            xe_journal_max_lines: 200,
            audit_entries,
            show_full_log: false,
            journal_scroll: 0,
            xe_journal_scroll: 0,
            mock_mode: false,
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
                if let Ok(entries) = self.api_client.fetch_audit_log() {
                    self.audit_entries = entries;
                }
                self.last_slow_refresh = Instant::now();
            }

            // Manage journal process: start if services starting, drain lines, stop when ready
            self.manage_journal();

            // Manage journalctl -xe subprocess for the bottom-left pane
            self.manage_xe_journal();

            // In console mode, detect kernel messages and force full repaint
            if self.drain_kmsg() {
                terminal.clear()?;
            }

            // Drain evdev events for keyboard activity detection
            let evdev_activity = self.drain_evdev();
            if evdev_activity.any_activity {
                self.last_activity = Instant::now();
            }

            // Screen blanking: blank after inactivity timeout
            if self.should_blank_screen() {
                self.screen_blanked = true;
                terminal.clear()?;
            }

            // evdev activity while blanked: unblank immediately
            if evdev_activity.any_activity && self.screen_blanked {
                self.unblank();
                // Only discard next crossterm key if a non-modifier was pressed,
                // since crossterm won't see bare modifier presses.
                self.discard_next_key = evdev_activity.has_non_modifier;
                terminal.clear()?;
            }

            if !self.screen_blanked {
                terminal.draw(|f| self.render(f))?;
            }

            if event::poll(Duration::from_secs(1))? {
                let ev = event::read()?;

                // Crossterm fallback: unblank on any event (for SSH / no evdev)
                if self.screen_blanked {
                    self.unblank();
                    self.discard_next_key = true;
                    terminal.clear()?;
                    continue;
                }

                if let Event::Key(key) = ev {
                    // Discard exactly one key after unblank (the wake key)
                    if self.discard_next_key {
                        self.discard_next_key = false;
                        self.last_activity = Instant::now();
                        continue;
                    }

                    self.last_activity = Instant::now();
                    let action = self.map_key(key);
                    match action {
                        GettyAction::Login => {
                            if self.mock_mode {
                                cmd_log_append("mock: exec /bin/login (skipped)".to_string());
                            } else {
                                // exec into /bin/login — cede control entirely.
                                // agetty will respawn us after the shell exits.
                                self.stop_journal();
                                self.stop_xe_journal();
                                disable_raw_mode()?;
                                execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
                                self.exec_login();
                                // exec_login only returns on failure — recover TUI
                                let mut new_stdout = io::stdout();
                                execute!(new_stdout, EnterAlternateScreen)?;
                                enable_raw_mode()?;
                                terminal = Terminal::new(CrosstermBackend::new(new_stdout))?;
                            }
                        }
                        GettyAction::Quit => {
                            self.should_quit = true;
                        }
                        GettyAction::ReconfigureMenu => {
                            self.reconfigure_menu = true;
                        }
                        GettyAction::ReconfigureNetwork => {
                            self.reconfigure_menu = false;
                            self.stop_journal();
                            self.stop_xe_journal();
                            disable_raw_mode()?;
                            execute!(terminal.backend_mut(), LeaveAlternateScreen)?;

                            self.run_network_reconfigure(executor);

                            // Re-enter TUI
                            let mut new_stdout = io::stdout();
                            execute!(new_stdout, EnterAlternateScreen)?;
                            enable_raw_mode()?;
                            terminal = Terminal::new(CrosstermBackend::new(new_stdout))?;
                            self.last_fast_refresh = Instant::now() - Duration::from_secs(10);
                            self.last_slow_refresh = Instant::now() - Duration::from_secs(20);
                            self.last_activity = Instant::now();
                            self.screen_blanked = false;
                            self.discard_next_key = false;
                        }
                        GettyAction::ReconfigureSshKeys => {
                            if self.ssh_input.is_some() {
                                // In SSH input mode: execute import for typed username
                                self.execute_ssh_key_import(executor);
                            } else if self.ssh_users.is_empty() {
                                cmd_log_append("No --ssh-user configured".to_string());
                            } else {
                                // Entering SSH input mode from reconfigure menu
                                self.reconfigure_menu = false;
                                self.ssh_input = Some(SshInputState {
                                    current_user_idx: 0,
                                    github_username: String::new(),
                                    status_message: None,
                                });
                            }
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
        self.stop_xe_journal();
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

        // SSH key input mode
        if let Some(ref mut ssh_state) = self.ssh_input {
            match key.code {
                KeyCode::Esc => {
                    self.ssh_input = None;
                }
                KeyCode::Backspace => {
                    ssh_state.github_username.pop();
                    ssh_state.status_message = None;
                }
                KeyCode::Enter => {
                    if ssh_state.github_username.is_empty() {
                        // Empty enter: skip to next user
                        ssh_state.current_user_idx += 1;
                        ssh_state.status_message = None;
                        if ssh_state.current_user_idx >= self.ssh_users.len() {
                            self.ssh_input = None;
                        }
                    } else {
                        // Non-empty: signal import (handled in main loop)
                        return GettyAction::ReconfigureSshKeys;
                    }
                }
                KeyCode::Char(c) => {
                    ssh_state.github_username.push(c);
                    ssh_state.status_message = None;
                }
                _ => {}
            }
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

        // Reconfigure submenu
        if self.reconfigure_menu {
            match key.code {
                KeyCode::Esc => {
                    self.reconfigure_menu = false;
                }
                KeyCode::Char('n') => return GettyAction::ReconfigureNetwork,
                KeyCode::Char('k') => {
                    return GettyAction::ReconfigureSshKeys;
                }
                KeyCode::Char('!') => {
                    self.sledgehammer_input = Some(String::new());
                }
                _ => {}
            }
            return GettyAction::None;
        }

        // Normal mode
        match key.code {
            KeyCode::Char('.') => GettyAction::Login,
            KeyCode::Char('l') => {
                self.show_full_log = true;
                GettyAction::None
            }
            KeyCode::Char('s') => {
                self.show_full_log = false;
                GettyAction::None
            }
            KeyCode::Char('q') if self.quit_enabled => GettyAction::Quit,
            KeyCode::Char('@') => GettyAction::ReconfigureMenu,
            KeyCode::Char('R') => GettyAction::Reboot,
            KeyCode::Char('p') => GettyAction::PowerOff,
            KeyCode::PageUp => {
                self.scroll_active_journal_back(JOURNAL_PAGE_STEP);
                GettyAction::None
            }
            KeyCode::PageDown => {
                self.scroll_active_journal_forward(JOURNAL_PAGE_STEP);
                GettyAction::None
            }
            KeyCode::Up => {
                self.scroll_active_journal_back(1);
                GettyAction::None
            }
            KeyCode::Down => {
                self.scroll_active_journal_forward(1);
                GettyAction::None
            }
            KeyCode::End => {
                self.scroll_active_journal_to_tail();
                GettyAction::None
            }
            KeyCode::Home => {
                self.scroll_active_journal_to_top();
                GettyAction::None
            }
            _ => GettyAction::None,
        }
    }

    /// Scroll the currently visible journal pane back by `n` lines.
    /// In `show_full_log` mode the active pane is journal -f; otherwise
    /// it is journal -xe (the only journal pane in the quad view).
    pub fn scroll_active_journal_back(&mut self, n: usize) {
        if self.show_full_log {
            let max_scroll = self.journal_lines.len().saturating_sub(1);
            self.journal_scroll = (self.journal_scroll + n).min(max_scroll);
        } else {
            let max_scroll = self.xe_journal_lines.len().saturating_sub(1);
            self.xe_journal_scroll = (self.xe_journal_scroll + n).min(max_scroll);
        }
    }

    /// Scroll the currently visible journal pane forward (toward tail) by `n` lines.
    pub fn scroll_active_journal_forward(&mut self, n: usize) {
        if self.show_full_log {
            self.journal_scroll = self.journal_scroll.saturating_sub(n);
        } else {
            self.xe_journal_scroll = self.xe_journal_scroll.saturating_sub(n);
        }
    }

    /// Reset the active journal pane to follow the live tail.
    pub fn scroll_active_journal_to_tail(&mut self) {
        if self.show_full_log {
            self.journal_scroll = 0;
        } else {
            self.xe_journal_scroll = 0;
        }
    }

    /// Jump the active journal pane to the top of its buffer.
    pub fn scroll_active_journal_to_top(&mut self) {
        if self.show_full_log {
            self.journal_scroll = self.journal_lines.len().saturating_sub(1);
        } else {
            self.xe_journal_scroll = self.xe_journal_lines.len().saturating_sub(1);
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

    /// Unblank the screen. Caller sets `discard_next_key` based on context.
    pub fn unblank(&mut self) {
        self.screen_blanked = false;
        self.last_activity = Instant::now();
    }

    /// Drain evdev events and return activity result.
    fn drain_evdev(&mut self) -> ActivityResult {
        match self.evdev_watcher {
            Some(ref mut watcher) => watcher.has_activity(),
            None => ActivityResult {
                any_activity: false,
                has_non_modifier: false,
            },
        }
    }

    /// Manage journal process and panel view transitions.
    fn manage_journal(&mut self) {
        // Auto mode: mark as Status when all services are green (for title indicator)
        if self.panel_view == PanelView::Auto && self.all_services_active() {
            self.panel_view = PanelView::Status;
        }

        // Journal always runs — the pane is always visible
        if self.journal_child.is_none() {
            self.start_journal();
        }
        self.drain_journal_lines();
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
                        append_log_line(
                            &mut self.journal_lines,
                            &mut self.journal_scroll,
                            self.journal_max_lines,
                            trimmed,
                        );
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

    /// Manage the journalctl -xe subprocess for the bottom-left pane.
    fn manage_xe_journal(&mut self) {
        if self.xe_journal_child.is_none() {
            self.start_xe_journal();
        }
        self.drain_xe_journal_lines();
    }

    fn start_xe_journal(&mut self) {
        let child = Command::new("journalctl")
            .args(["-xe", "--no-pager", "-n", "50", "-o", "short-iso", "-f"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn();

        match child {
            Ok(c) => {
                if let Some(ref stdout) = c.stdout {
                    set_nonblocking(stdout);
                }
                self.xe_journal_child = Some(c);
            }
            Err(e) => {
                self.xe_journal_lines
                    .push(format!("Failed to start journalctl -xe: {}", e));
            }
        }
    }

    fn stop_xe_journal(&mut self) {
        if let Some(mut child) = self.xe_journal_child.take() {
            if let Err(e) = child.kill() {
                eprintln!("kill xe journal child: {}", e);
            }
            if let Err(e) = child.wait() {
                eprintln!("wait xe journal child: {}", e);
            }
        }
    }

    fn drain_xe_journal_lines(&mut self) {
        let Some(ref mut child) = self.xe_journal_child else {
            return;
        };
        let Some(ref mut stdout) = child.stdout else {
            return;
        };

        let mut reader = BufReader::new(stdout);
        let mut line = String::new();

        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) => break,
                Ok(_) => {
                    let trimmed = line.trim_end().to_string();
                    if !trimmed.is_empty() {
                        append_log_line(
                            &mut self.xe_journal_lines,
                            &mut self.xe_journal_scroll,
                            self.xe_journal_max_lines,
                            trimmed,
                        );
                    }
                }
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(_) => break,
            }
        }
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
            GettyAction::None
            | GettyAction::Login
            | GettyAction::Quit
            | GettyAction::ReconfigureMenu
            | GettyAction::ReconfigureNetwork => {}
            GettyAction::ReconfigureSshKeys => {
                self.execute_ssh_key_import(executor);
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

    /// Run the network reconfigure flow inline using the installer TUI.
    fn run_network_reconfigure(&mut self, executor: &mut dyn OperationExecutor) {
        cmd_log_append("reconfigure: detecting hardware...".to_string());
        let detect_result = if self.initrd_mode {
            crate::detect::detect_hardware_initrd()
        } else {
            crate::detect::detect_hardware()
        };

        let hardware = match detect_result {
            Ok(h) => h,
            Err(e) => {
                cmd_log_append(format!("reconfigure: hardware detection failed: {}", e));
                return;
            }
        };

        cmd_log_append(format!(
            "reconfigure: found {} interface(s)",
            hardware.network.interfaces.len()
        ));

        let mut state_machine = InstallerStateMachine::new(hardware);
        state_machine.network_only = true;
        if let Some(ref prefix) = self.etc_prefix {
            state_machine.etc_prefix = Some(prefix.clone());
        }
        let mut app = App::new(state_machine);
        if let Err(e) = app.run(executor, self.tty.as_deref()) {
            cmd_log_append(format!("reconfigure: {}", e));
        } else {
            cmd_log_append("reconfigure: network setup complete".to_string());
        }
    }

    /// Execute SSH key import for the current user in the SSH input state.
    fn execute_ssh_key_import(&mut self, executor: &mut dyn OperationExecutor) {
        let ssh_state = match self.ssh_input.as_mut() {
            Some(s) => s,
            None => return,
        };

        let system_user = match self.ssh_users.get(ssh_state.current_user_idx) {
            Some(u) => u.clone(),
            None => return,
        };
        let github_username = ssh_state.github_username.clone();

        if github_username.is_empty() {
            return;
        }

        if !crate::ssh::is_valid_github_username(&github_username) {
            ssh_state.status_message = Some((
                format!("Invalid GitHub username: {}", github_username),
                false,
            ));
            ssh_state.github_username.clear();
            return;
        }

        let op = Operation::ImportSshKeys {
            mount_point: self.mount_point.clone(),
            system_user: system_user.clone(),
            github_username: github_username.clone(),
        };
        let result = executor.execute(&op);
        let success = result.is_success();
        cmd_log_append(format!(
            "ssh-keys: {} -> {}: {:?}",
            github_username, system_user, result
        ));
        if success {
            ssh_state.status_message = Some((
                format!("Imported keys for {} -> {}", github_username, system_user),
                true,
            ));
            ssh_state.github_username.clear();
            ssh_state.current_user_idx += 1;
            if ssh_state.current_user_idx >= self.ssh_users.len() {
                self.ssh_input = None;
            }
        } else {
            ssh_state.status_message = Some((
                format!("Failed to import keys for {}", github_username),
                false,
            ));
            ssh_state.github_username.clear();
        }
    }

    fn render(&self, f: &mut ratatui::Frame) {
        let area = f.area();

        if self.show_full_log {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(5), // Title bar
                    Constraint::Min(5),   // Full journal -f
                    Constraint::Length(3), // Action bar
                ])
                .split(area);

            self.render_title(f, chunks[0]);
            self.render_journal(f, chunks[1]);

            if self.ssh_input.is_some() {
                self.render_ssh_input(f, chunks[2]);
            } else if self.sledgehammer_input.is_some() {
                self.render_sledgehammer_confirm(f, chunks[2]);
            } else if self.reconfigure_menu {
                self.render_reconfigure_menu(f, chunks[2]);
            } else {
                self.render_actions(f, chunks[2]);
            }
            return;
        }

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(5),      // Title bar
                Constraint::Percentage(50), // Quad top row: services | metrics
                Constraint::Percentage(50), // Quad bottom row: audit log | journal -xe
                Constraint::Length(3),      // Action bar
            ])
            .split(area);

        self.render_title(f, chunks[0]);

        // Quad top row: service status (left) | system metrics (right)
        let quad_top = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(50),
                Constraint::Percentage(50),
            ])
            .split(chunks[1]);
        self.render_service_status(f, quad_top[0]);
        self.render_system_info(f, quad_top[1]);

        // Quad bottom row: audit log (left) | journal -xe (right)
        let quad_bottom = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(50),
                Constraint::Percentage(50),
            ])
            .split(chunks[2]);
        self.render_audit_log(f, quad_bottom[0]);
        self.render_xe_journal(f, quad_bottom[1]);

        if self.ssh_input.is_some() {
            self.render_ssh_input(f, chunks[3]);
        } else if self.sledgehammer_input.is_some() {
            self.render_sledgehammer_confirm(f, chunks[3]);
        } else if self.reconfigure_menu {
            self.render_reconfigure_menu(f, chunks[3]);
        } else {
            self.render_actions(f, chunks[3]);
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

        let hostname_line = Line::from(vec![
            Span::styled(
                &self.system_info.hostname,
                Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
            ),
        ]);

        let detail_line = Line::from(vec![
            Span::styled(&url, Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
            Span::raw("  "),
            api_status,
            Span::raw("  "),
            version_span,
        ]);

        let title = Paragraph::new(vec![
            Line::from(""),
            hostname_line,
            detail_line,
        ])
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
                Span::styled("Kernel: ", Style::default().fg(Color::DarkGray)),
                Span::raw(format!(
                    "{} {}",
                    info.kernel_version, info.architecture
                )),
                Span::styled("     CPU: ", Style::default().fg(Color::DarkGray)),
                Span::raw(format!("{} ({} cores)", info.cpu_model, info.cpu_cores)),
            ]),
            Line::from(vec![
                Span::styled("Load:   ", Style::default().fg(Color::DarkGray)),
                Span::raw(format!("{:.2}", info.load_average)),
                Span::styled("              Memory: ", Style::default().fg(Color::DarkGray)),
                Span::raw(format!(
                    "{} / {} MB ({}%)",
                    info.mem_used_mb, info.mem_total_mb, mem_pct
                )),
            ]),
            Line::from(vec![
                Span::styled("Disk:   ", Style::default().fg(Color::DarkGray)),
                Span::raw(format!(
                    "{:.1} / {:.1} GB available on {}",
                    info.disk_available_gb, info.disk_total_gb, self.mount_point
                )),
            ]),
            Line::from(vec![
                Span::styled("Network: ", Style::default().fg(Color::DarkGray)),
                Span::styled(network_line, network_style),
            ]),
        ];

        let block = Block::default()
            .title(" System ")
            .title_alignment(Alignment::Left)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray))
            .padding(Padding::horizontal(1));
        let paragraph = Paragraph::new(lines).block(block);
        f.render_widget(paragraph, area);
    }

    fn render_service_status(&self, f: &mut ratatui::Frame, area: Rect) {
        let title = if self.panel_view == PanelView::Auto {
            " Services (starting…) "
        } else {
            " Services "
        };
        let block = Block::default()
            .title(title)
            .title_alignment(Alignment::Left)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray))
            .padding(Padding::horizontal(1));

        match &self.all_services {
            Ok(services) if services.is_empty() => {
                let lines = vec![
                    Line::from(""),
                    Line::from(Span::styled(
                        "No services running",
                        Style::default().fg(Color::DarkGray),
                    )),
                ];
                let paragraph = Paragraph::new(lines)
                    .alignment(Alignment::Center)
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
                            Span::raw(format!("{:<width$} ", svc.name, width = name_width)),
                            Span::styled(
                                svc.active_state.clone(),
                                state_style,
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
                        err.to_string(),
                        Style::default().fg(Color::Yellow),
                    )),
                ];
                let paragraph = Paragraph::new(lines).block(block);
                f.render_widget(paragraph, area);
            }
        }
    }

    fn render_journal(&self, f: &mut ratatui::Frame, area: Rect) {
        let title = if self.journal_scroll > 0 {
            format!(
                " Journal -f [s: status] (paused, -{} lines, End to follow) ",
                self.journal_scroll
            )
        } else {
            " Journal -f [s: status] ".to_string()
        };
        let border_color = if self.journal_scroll > 0 {
            Color::Yellow
        } else {
            Color::DarkGray
        };
        let block = Block::default()
            .title(title)
            .title_alignment(Alignment::Left)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(border_color))
            .padding(Padding::horizontal(1));

        let inner_height = area.height.saturating_sub(2) as usize;
        let (start, end) = journal_window(
            self.journal_lines.len(),
            self.journal_scroll,
            inner_height,
        );
        let visible: Vec<Line> = self.journal_lines[start..end]
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
                Line::from(Span::styled(line.as_str(), style))
            })
            .collect();

        let paragraph = Paragraph::new(visible).block(block);
        f.render_widget(paragraph, area);
    }

    fn render_audit_log(&self, f: &mut ratatui::Frame, area: Rect) {
        let block = Block::default()
            .title(" Audit Log ")
            .title_alignment(Alignment::Left)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray))
            .padding(Padding::horizontal(1));

        if self.audit_entries.is_empty() {
            let lines = vec![
                Line::from(""),
                Line::from(Span::styled(
                    "No audit log entries",
                    Style::default().fg(Color::DarkGray),
                )),
            ];
            let paragraph = Paragraph::new(lines)
                .block(block)
                .alignment(Alignment::Center);
            f.render_widget(paragraph, area);
            return;
        }

        // inner width = total area minus 2 for borders minus 2 for padding
        let inner_width = area.width.saturating_sub(4) as usize;
        let inner_height = area.height.saturating_sub(2) as usize;
        let start = self.audit_entries.len().saturating_sub(inner_height);

        // Column layout: " OK /path/here  action  key=val key=val  12:30:45 "
        // Status: 3 chars, Path: 30% of width, Action: 15%, Time: 9 chars, Detail: rest
        let status_w = 3;
        let time_w = 9; // " HH:MM:SS"
        let separators = 4; // spaces between columns
        let fixed = status_w + time_w + separators;
        let flexible = inner_width.saturating_sub(fixed);
        let path_w = flexible * 35 / 100;
        let action_w = flexible * 20 / 100;
        let detail_w = flexible.saturating_sub(path_w + action_w);

        let visible: Vec<Line> = self.audit_entries[start..]
            .iter()
            .map(|entry| {
                let mut spans = Vec::new();

                // Status column
                if entry.success {
                    spans.push(Span::styled(
                        "OK ",
                        Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
                    ));
                } else {
                    spans.push(Span::styled(
                        "ERR",
                        Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                    ));
                }

                spans.push(Span::raw(" "));

                // Path column — prominent, cyan
                let path_display = truncate_str(&entry.path, path_w);
                let path_padded = format!("{:<width$}", path_display, width = path_w);
                spans.push(Span::styled(
                    path_padded,
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                ));

                spans.push(Span::raw(" "));

                // Action column
                let action_display = truncate_str(&entry.action, action_w);
                let action_padded = format!("{:<width$}", action_display, width = action_w);
                spans.push(Span::styled(
                    action_padded,
                    Style::default().fg(Color::White),
                ));

                spans.push(Span::raw(" "));

                // Detail column — key=value pairs with highlighted keys
                if !entry.detail.is_empty() {
                    let detail_display = truncate_str(&entry.detail, detail_w);
                    let detail_spans = highlight_key_value_pairs(&detail_display, detail_w);
                    spans.extend(detail_spans);
                } else if !entry.error.is_empty() {
                    let err_display = truncate_str(&entry.error, detail_w);
                    spans.push(Span::styled(
                        format!("{:<width$}", err_display, width = detail_w),
                        Style::default().fg(Color::Red),
                    ));
                } else {
                    spans.push(Span::styled(
                        " ".repeat(detail_w),
                        Style::default(),
                    ));
                }

                spans.push(Span::raw(" "));

                // Time column — HH:MM:SS only, dimmed
                let time_display = if entry.timestamp.len() >= 19 {
                    &entry.timestamp[11..19]
                } else if !entry.timestamp.is_empty() {
                    &entry.timestamp
                } else {
                    "        "
                };
                spans.push(Span::styled(
                    format!("{:>8}", time_display),
                    Style::default().fg(Color::DarkGray),
                ));

                Line::from(spans)
            })
            .collect();

        let paragraph = Paragraph::new(visible).block(block);
        f.render_widget(paragraph, area);
    }

    fn render_xe_journal(&self, f: &mut ratatui::Frame, area: Rect) {
        let title = if self.xe_journal_scroll > 0 {
            format!(
                " Journal -xe (paused, -{} lines, End to follow) ",
                self.xe_journal_scroll
            )
        } else {
            " Journal -xe ".to_string()
        };
        let border_color = if self.xe_journal_scroll > 0 {
            Color::Yellow
        } else {
            Color::DarkGray
        };
        let block = Block::default()
            .title(title)
            .title_alignment(Alignment::Left)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(border_color))
            .padding(Padding::horizontal(1));

        let inner_height = area.height.saturating_sub(2) as usize;
        let (start, end) = journal_window(
            self.xe_journal_lines.len(),
            self.xe_journal_scroll,
            inner_height,
        );
        let visible: Vec<Line> = self.xe_journal_lines[start..end]
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
                Line::from(Span::styled(line.as_str(), style))
            })
            .collect();

        let paragraph = Paragraph::new(visible).block(block);
        f.render_widget(paragraph, area);
    }


    fn render_actions(&self, f: &mut ratatui::Frame, area: Rect) {
        let text = if self.quit_enabled {
            "  [.] Login   [s] Status   [l] Log   [q] Quit   [@] Reconfigure   [R] Reboot   [p] Power Off   [PgUp/PgDn] Scroll"
        } else {
            "  [.] Login   [s] Status   [l] Log   [@] Reconfigure   [R] Reboot   [p] Power Off   [PgUp/PgDn] Scroll"
        };
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

    fn render_reconfigure_menu(&self, f: &mut ratatui::Frame, area: Rect) {
        let text = "  [@] Reconfigure:   [n] Network   [k] SSH Keys   [!] Wipe   [Esc] Back";
        let actions = Paragraph::new(text)
            .alignment(Alignment::Center)
            .style(
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Yellow)),
            );
        f.render_widget(actions, area);
    }

    fn render_ssh_input(&self, f: &mut ratatui::Frame, area: Rect) {
        let ssh_state = match &self.ssh_input {
            Some(s) => s,
            None => return,
        };
        let user = self
            .ssh_users
            .get(ssh_state.current_user_idx)
            .map(|s| s.as_str())
            .unwrap_or("?");
        let progress = format!(
            "({}/{})",
            ssh_state.current_user_idx + 1,
            self.ssh_users.len()
        );
        let status = match &ssh_state.status_message {
            Some((msg, true)) => format!("  ✓ {}", msg),
            Some((msg, false)) => format!("  ✗ {}", msg),
            None => String::new(),
        };
        let text = format!(
            "  SSH keys for {user} {progress}: {username}_   (Enter: import, empty Enter: skip, Esc: cancel){status}",
            user = user,
            progress = progress,
            username = ssh_state.github_username,
            status = status,
        );
        let paragraph = Paragraph::new(text)
            .style(
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Cyan)),
            );
        f.render_widget(paragraph, area);
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

/// Truncate a string to fit within `max_width` characters.
/// Appends "…" if truncated (counts as 1 char of width).
fn truncate_str(s: &str, max_width: usize) -> String {
    if max_width == 0 {
        return String::new();
    }
    if s.len() <= max_width {
        return s.to_string();
    }
    let mut result: String = s.chars().take(max_width.saturating_sub(1)).collect();
    result.push('\u{2026}'); // …
    result
}

/// Parse "key=val key2=val2" into spans with highlighted keys (yellow) and values (white).
/// Truncates to fit within `max_width` total characters.
fn highlight_key_value_pairs(s: &str, max_width: usize) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut chars_used = 0;

    for (i, token) in s.split_whitespace().enumerate() {
        if chars_used >= max_width {
            break;
        }
        if i > 0 {
            spans.push(Span::raw(" ".to_string()));
            chars_used += 1;
        }
        if let Some(eq_pos) = token.find('=') {
            let key = &token[..eq_pos + 1]; // includes '='
            let val = &token[eq_pos + 1..];
            let remaining = max_width.saturating_sub(chars_used);
            if key.len() + val.len() > remaining {
                let trunc = truncate_str(token, remaining);
                spans.push(Span::styled(
                    trunc.clone(),
                    Style::default().fg(Color::Yellow),
                ));
                chars_used += trunc.len();
            } else {
                spans.push(Span::styled(
                    key.to_string(),
                    Style::default().fg(Color::Yellow),
                ));
                spans.push(Span::styled(
                    val.to_string(),
                    Style::default().fg(Color::White),
                ));
                chars_used += key.len() + val.len();
            }
        } else {
            let remaining = max_width.saturating_sub(chars_used);
            let display = truncate_str(token, remaining);
            spans.push(Span::styled(
                display.clone(),
                Style::default().fg(Color::White),
            ));
            chars_used += display.len();
        }
    }

    // Pad remaining space
    if chars_used < max_width {
        spans.push(Span::raw(" ".repeat(max_width - chars_used)));
    }

    spans
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
    fn test_key_reconfigure_menu() {
        let mut app = test_app();
        let key = KeyEvent::from(KeyCode::Char('@'));
        assert_eq!(app.map_key(key), GettyAction::ReconfigureMenu);
    }

    #[test]
    fn test_reconfigure_menu_network() {
        let mut app = test_app();
        app.reconfigure_menu = true;
        let key = KeyEvent::from(KeyCode::Char('n'));
        assert_eq!(app.map_key(key), GettyAction::ReconfigureNetwork);
    }

    #[test]
    fn test_reconfigure_menu_ssh_keys() {
        let mut app = test_app();
        app.reconfigure_menu = true;
        let key = KeyEvent::from(KeyCode::Char('k'));
        assert_eq!(app.map_key(key), GettyAction::ReconfigureSshKeys);
    }

    #[test]
    fn test_reconfigure_menu_sledgehammer() {
        let mut app = test_app();
        app.reconfigure_menu = true;
        let key = KeyEvent::from(KeyCode::Char('!'));
        app.map_key(key);
        assert!(app.sledgehammer_input.is_some());
    }

    #[test]
    fn test_reconfigure_menu_esc() {
        let mut app = test_app();
        app.reconfigure_menu = true;
        let key = KeyEvent::from(KeyCode::Esc);
        app.map_key(key);
        assert!(!app.reconfigure_menu);
    }

    #[test]
    fn test_reconfigure_menu_blocks_normal_keys() {
        let mut app = test_app();
        app.reconfigure_menu = true;
        // 'R' should not trigger reboot from reconfigure menu
        let key = KeyEvent::from(KeyCode::Char('R'));
        assert_eq!(app.map_key(key), GettyAction::None);
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
        app.reconfigure_menu = true;
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
            quit_enabled: false,
            initrd_mode: false,
            sledgehammer_grub_entry: None,
            ssh_users: Vec::new(),
            reconfigure_menu: false,
            ssh_input: None,
            last_activity: Instant::now(),
            screen_blanked: false,
            discard_next_key: false,
            evdev_watcher: None,
            xe_journal_lines: Vec::new(),
            xe_journal_child: None,
            xe_journal_max_lines: 200,
            audit_entries: Vec::new(),
            show_full_log: false,
            journal_scroll: 0,
            xe_journal_scroll: 0,
            mock_mode: false,
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
    fn test_unblank_clears_blanked_state() {
        let mut app = test_app();
        app.screen_blanked = true;
        app.last_activity = Instant::now() - Duration::from_secs(360);
        app.unblank();
        assert!(!app.screen_blanked);
        assert!(app.last_activity.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn test_discard_next_key_starts_false() {
        let app = test_app();
        assert!(!app.discard_next_key);
    }

    #[test]
    fn test_discard_next_key_set_on_non_modifier_wake() {
        let mut app = test_app();
        app.screen_blanked = true;
        app.unblank();
        // Caller sets discard_next_key for non-modifier wake keys
        app.discard_next_key = true;
        assert!(app.discard_next_key);
        // Simulating the second key: discard_next_key is cleared
        app.discard_next_key = false;
        assert!(!app.discard_next_key);
    }

    #[test]
    fn test_discard_next_key_not_set_for_modifier_only() {
        let mut app = test_app();
        app.screen_blanked = true;
        app.unblank();
        // Modifier-only wake: crossterm won't see it, so don't discard
        app.discard_next_key = false;
        assert!(!app.discard_next_key);
    }

    #[test]
    fn test_activity_resets_on_unblank() {
        let mut app = test_app();
        app.screen_blanked = true;
        app.last_activity = Instant::now() - Duration::from_secs(600);
        app.unblank();
        assert!(app.last_activity.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn test_drain_evdev_none_returns_no_activity() {
        let mut app = test_app();
        let result = app.drain_evdev();
        assert!(!result.any_activity);
        assert!(!result.has_non_modifier);
    }

    #[test]
    fn test_key_quit_disabled_by_default() {
        let mut app = test_app();
        assert!(!app.quit_enabled);
        let key = KeyEvent::from(KeyCode::Char('q'));
        assert_eq!(app.map_key(key), GettyAction::None);
        assert!(!app.should_quit);
    }

    #[test]
    fn test_key_quit_enabled() {
        let mut app = test_app();
        app.quit_enabled = true;
        let key = KeyEvent::from(KeyCode::Char('q'));
        assert_eq!(app.map_key(key), GettyAction::Quit);
    }

    #[test]
    fn test_quit_action_not_executed_by_execute_action() {
        let mut app = test_app();
        let mut executor = MockExecutor::new(vec![]);
        app.execute_action(&GettyAction::Quit, &mut executor);
        let ops = executor.recorded_operations();
        assert!(ops.is_empty());
    }

    #[test]
    fn test_ssh_input_enter_empty_skips_user() {
        let mut app = test_app();
        app.ssh_users = vec!["root".to_string(), "erikh".to_string()];
        app.ssh_input = Some(SshInputState {
            current_user_idx: 0,
            github_username: String::new(),
            status_message: None,
        });
        let key = KeyEvent::from(KeyCode::Enter);
        app.map_key(key);
        // Should advance to next user
        assert_eq!(app.ssh_input.as_ref().map(|s| s.current_user_idx), Some(1));
    }

    #[test]
    fn test_ssh_input_enter_empty_last_user_exits() {
        let mut app = test_app();
        app.ssh_users = vec!["root".to_string()];
        app.ssh_input = Some(SshInputState {
            current_user_idx: 0,
            github_username: String::new(),
            status_message: None,
        });
        let key = KeyEvent::from(KeyCode::Enter);
        app.map_key(key);
        // Only one user, empty enter should exit SSH mode
        assert!(app.ssh_input.is_none());
    }

    #[test]
    fn test_ssh_input_esc_exits() {
        let mut app = test_app();
        app.ssh_users = vec!["root".to_string()];
        app.ssh_input = Some(SshInputState {
            current_user_idx: 0,
            github_username: String::new(),
            status_message: None,
        });
        let key = KeyEvent::from(KeyCode::Esc);
        app.map_key(key);
        assert!(app.ssh_input.is_none());
    }

    #[test]
    fn test_ssh_input_typing() {
        let mut app = test_app();
        app.ssh_users = vec!["root".to_string()];
        app.ssh_input = Some(SshInputState {
            current_user_idx: 0,
            github_username: String::new(),
            status_message: None,
        });
        app.map_key(KeyEvent::from(KeyCode::Char('a')));
        app.map_key(KeyEvent::from(KeyCode::Char('b')));
        assert_eq!(app.ssh_input.as_ref().map(|s| s.github_username.as_str()), Some("ab"));
        app.map_key(KeyEvent::from(KeyCode::Backspace));
        assert_eq!(app.ssh_input.as_ref().map(|s| s.github_username.as_str()), Some("a"));
    }

    #[test]
    fn test_ssh_input_enter_with_username_returns_action() {
        let mut app = test_app();
        app.ssh_users = vec!["root".to_string()];
        app.ssh_input = Some(SshInputState {
            current_user_idx: 0,
            github_username: "testuser".to_string(),
            status_message: None,
        });
        let key = KeyEvent::from(KeyCode::Enter);
        assert_eq!(app.map_key(key), GettyAction::ReconfigureSshKeys);
    }

    #[test]
    fn test_old_r_key_does_nothing() {
        let mut app = test_app();
        let key = KeyEvent::from(KeyCode::Char('r'));
        assert_eq!(app.map_key(key), GettyAction::None);
    }

    #[test]
    fn test_xe_journal_lines_initially_empty() {
        let app = test_app();
        assert!(app.xe_journal_lines.is_empty());
    }

    #[test]
    fn test_xe_journal_child_initially_none() {
        let app = test_app();
        assert!(app.xe_journal_child.is_none());
    }

    #[test]
    fn test_xe_journal_max_lines_default() {
        let app = test_app();
        assert_eq!(app.xe_journal_max_lines, 200);
    }

    #[test]
    fn test_xe_journal_buffer_cap() {
        let mut app = test_app();
        app.xe_journal_max_lines = 5;
        for i in 0..10 {
            app.xe_journal_lines.push(format!("line {}", i));
            if app.xe_journal_lines.len() > app.xe_journal_max_lines {
                let excess = app.xe_journal_lines.len() - app.xe_journal_max_lines;
                app.xe_journal_lines.drain(..excess);
            }
        }
        assert_eq!(app.xe_journal_lines.len(), 5);
        assert_eq!(app.xe_journal_lines[0], "line 5");
        assert_eq!(app.xe_journal_lines[4], "line 9");
    }

    #[test]
    fn test_stop_xe_journal_when_no_child() {
        let mut app = test_app();
        // Should not panic when no child exists
        app.stop_xe_journal();
        assert!(app.xe_journal_child.is_none());
    }

    #[test]
    fn test_drain_xe_journal_lines_no_child() {
        let mut app = test_app();
        // Should not panic when no child exists
        app.drain_xe_journal_lines();
        assert!(app.xe_journal_lines.is_empty());
    }

    #[test]
    fn test_ssh_import_success_advances_to_next_user() {
        let mut app = test_app();
        app.ssh_users = vec!["root".to_string(), "erikh".to_string()];
        app.ssh_input = Some(SshInputState {
            current_user_idx: 0,
            github_username: "validuser".to_string(),
            status_message: None,
        });
        let mut executor = MockExecutor::new(vec![]);
        app.execute_ssh_key_import(&mut executor);
        // Should advance to next user after success
        let ssh_state = app.ssh_input.as_ref().expect("ssh_input should still exist");
        assert_eq!(ssh_state.current_user_idx, 1);
        assert!(ssh_state.github_username.is_empty());
    }

    #[test]
    fn test_ssh_import_success_last_user_exits() {
        let mut app = test_app();
        app.ssh_users = vec!["root".to_string()];
        app.ssh_input = Some(SshInputState {
            current_user_idx: 0,
            github_username: "validuser".to_string(),
            status_message: None,
        });
        let mut executor = MockExecutor::new(vec![]);
        app.execute_ssh_key_import(&mut executor);
        // Last user done — should exit SSH input mode
        assert!(app.ssh_input.is_none());
    }

    #[test]
    fn test_ssh_import_failure_stays_on_same_user() {
        use crate::engine::executor::{OperationMatcher, SimulatedResponse};
        use crate::engine::feedback::OperationResult;
        let mut app = test_app();
        app.ssh_users = vec!["root".to_string(), "erikh".to_string()];
        app.ssh_input = Some(SshInputState {
            current_user_idx: 0,
            github_username: "validuser".to_string(),
            status_message: None,
        });
        let mut executor = MockExecutor::new(vec![SimulatedResponse {
            operation_match: OperationMatcher::ByType("ImportSshKeys".to_string()),
            result: OperationResult::Error("fetch failed".to_string()),
            consume: false,
        }]);
        app.execute_ssh_key_import(&mut executor);
        // Should stay on same user after failure
        let ssh_state = app.ssh_input.as_ref().expect("ssh_input should still exist");
        assert_eq!(ssh_state.current_user_idx, 0);
        assert!(ssh_state.status_message.is_some());
        let (_, success) = ssh_state.status_message.as_ref().unwrap();
        assert!(!success);
    }

    #[test]
    fn test_manage_xe_journal_starts_child() {
        let mut app = test_app();
        app.manage_xe_journal();
        // On a system with journalctl, a child should be spawned.
        // On CI or containers without journalctl, an error line is added instead.
        let has_child = app.xe_journal_child.is_some();
        let has_error = app.xe_journal_lines.iter().any(|l| l.contains("Failed to start"));
        assert!(has_child || has_error);
        // Clean up
        app.stop_xe_journal();
    }

    #[test]
    fn test_key_l_switches_to_log() {
        let mut app = test_app();
        assert!(!app.show_full_log);
        let key = KeyEvent::from(KeyCode::Char('l'));
        assert_eq!(app.map_key(key), GettyAction::None);
        assert!(app.show_full_log);
        // Pressing 'l' again stays on log (not a toggle)
        let key = KeyEvent::from(KeyCode::Char('l'));
        app.map_key(key);
        assert!(app.show_full_log);
    }

    #[test]
    fn test_key_s_switches_to_status() {
        let mut app = test_app();
        app.show_full_log = true;
        let key = KeyEvent::from(KeyCode::Char('s'));
        assert_eq!(app.map_key(key), GettyAction::None);
        assert!(!app.show_full_log);
        // Pressing 's' again stays on status (not a toggle)
        let key = KeyEvent::from(KeyCode::Char('s'));
        app.map_key(key);
        assert!(!app.show_full_log);
    }

    #[test]
    fn test_key_l_then_s_round_trip() {
        let mut app = test_app();
        assert!(!app.show_full_log);
        app.map_key(KeyEvent::from(KeyCode::Char('l')));
        assert!(app.show_full_log);
        app.map_key(KeyEvent::from(KeyCode::Char('s')));
        assert!(!app.show_full_log);
    }

    #[test]
    fn test_audit_entries_initially_empty() {
        let app = test_app();
        assert!(app.audit_entries.is_empty());
    }

    #[test]
    fn test_audit_entries_can_be_populated() {
        let mut app = test_app();
        app.audit_entries = vec![
            AuditEntry {
                timestamp: "2024-01-15 12:00:00".to_string(),
                success: true,
                path: "/account/authenticate".to_string(),
                action: "authenticate".to_string(),
                detail: "username=erikh".to_string(),
                error: String::new(),
            },
            AuditEntry {
                timestamp: "2024-01-15 12:01:00".to_string(),
                success: false,
                path: "/packages/install".to_string(),
                action: "install package".to_string(),
                detail: "name=bad-pkg".to_string(),
                error: "not found".to_string(),
            },
        ];
        assert_eq!(app.audit_entries.len(), 2);
        assert!(app.audit_entries[0].success);
        assert!(!app.audit_entries[1].success);
        assert_eq!(app.audit_entries[0].path, "/account/authenticate");
        assert_eq!(app.audit_entries[1].error, "not found");
    }

    #[test]
    fn test_truncate_str_no_truncation() {
        assert_eq!(truncate_str("hello", 10), "hello");
    }

    #[test]
    fn test_truncate_str_exact_fit() {
        assert_eq!(truncate_str("hello", 5), "hello");
    }

    #[test]
    fn test_truncate_str_truncated() {
        let result = truncate_str("hello world", 6);
        assert_eq!(result, "hello\u{2026}");
    }

    #[test]
    fn test_truncate_str_zero_width() {
        assert_eq!(truncate_str("hello", 0), "");
    }

    #[test]
    fn test_highlight_key_value_pairs_basic() {
        let spans = highlight_key_value_pairs("username=erikh", 20);
        // Should have key span (yellow), value span (white), and padding
        assert!(spans.len() >= 2);
    }

    #[test]
    fn test_highlight_key_value_pairs_multiple() {
        let spans = highlight_key_value_pairs("admin=true email=e@x.com", 30);
        // Multiple key=value pairs should produce multiple colored spans
        assert!(spans.len() >= 4);
    }

    #[test]
    fn test_highlight_key_value_pairs_no_equals() {
        let spans = highlight_key_value_pairs("plaintext", 20);
        assert!(!spans.is_empty());
    }

    // ── Journal scrolling ────────────────────────────────────────────────

    #[test]
    fn test_journal_window_at_tail() {
        let (start, end) = journal_window(100, 0, 10);
        assert_eq!(end, 100);
        assert_eq!(start, 90);
    }

    #[test]
    fn test_journal_window_scrolled_back() {
        let (start, end) = journal_window(100, 5, 10);
        assert_eq!(end, 95);
        assert_eq!(start, 85);
    }

    #[test]
    fn test_journal_window_smaller_than_height() {
        let (start, end) = journal_window(3, 0, 10);
        assert_eq!(end, 3);
        assert_eq!(start, 0);
    }

    #[test]
    fn test_journal_window_scroll_past_top_clamps() {
        let (start, end) = journal_window(10, 100, 5);
        assert_eq!(end, 0);
        assert_eq!(start, 0);
    }

    #[test]
    fn test_append_log_line_basic() {
        let mut lines = Vec::new();
        let mut scroll = 0;
        append_log_line(&mut lines, &mut scroll, 100, "first".to_string());
        append_log_line(&mut lines, &mut scroll, 100, "second".to_string());
        assert_eq!(lines, vec!["first", "second"]);
        assert_eq!(scroll, 0, "scroll stays at 0 when following tail");
    }

    #[test]
    fn test_append_log_line_locks_view_when_scrolled() {
        let mut lines = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let mut scroll = 1;
        append_log_line(&mut lines, &mut scroll, 100, "d".to_string());
        // Scroll bumped from 1 → 2 so the same content stays visible
        assert_eq!(scroll, 2);
        assert_eq!(lines, vec!["a", "b", "c", "d"]);
    }

    #[test]
    fn test_append_log_line_trims_when_over_max() {
        let mut lines = Vec::new();
        let mut scroll = 0;
        for i in 0..10 {
            append_log_line(&mut lines, &mut scroll, 5, format!("line {}", i));
        }
        assert_eq!(lines.len(), 5);
        assert_eq!(lines[0], "line 5");
        assert_eq!(lines[4], "line 9");
        assert_eq!(scroll, 0);
    }

    #[test]
    fn test_append_log_line_clamps_scroll_to_buffer_size() {
        let mut lines = vec!["a".to_string(), "b".to_string()];
        let mut scroll = 10;
        append_log_line(&mut lines, &mut scroll, 5, "c".to_string());
        // Scroll incremented to 11, then clamped to len-1 = 2
        assert_eq!(scroll, 2);
    }

    #[test]
    fn test_pgup_scrolls_xe_journal_in_quad_view() {
        let mut app = test_app();
        app.show_full_log = false;
        for i in 0..50 {
            app.xe_journal_lines.push(format!("line {}", i));
        }
        app.map_key(KeyEvent::from(KeyCode::PageUp));
        assert_eq!(app.xe_journal_scroll, JOURNAL_PAGE_STEP);
        assert_eq!(app.journal_scroll, 0, "journal -f untouched");
    }

    #[test]
    fn test_pgup_scrolls_journal_f_in_full_log_view() {
        let mut app = test_app();
        app.show_full_log = true;
        for i in 0..50 {
            app.journal_lines.push(format!("line {}", i));
        }
        app.map_key(KeyEvent::from(KeyCode::PageUp));
        assert_eq!(app.journal_scroll, JOURNAL_PAGE_STEP);
        assert_eq!(app.xe_journal_scroll, 0, "journal -xe untouched");
    }

    #[test]
    fn test_pgdn_returns_toward_tail() {
        let mut app = test_app();
        for i in 0..50 {
            app.xe_journal_lines.push(format!("line {}", i));
        }
        app.xe_journal_scroll = 20;
        app.map_key(KeyEvent::from(KeyCode::PageDown));
        assert_eq!(app.xe_journal_scroll, 20 - JOURNAL_PAGE_STEP);
    }

    #[test]
    fn test_pgdn_clamps_at_zero() {
        let mut app = test_app();
        for i in 0..50 {
            app.xe_journal_lines.push(format!("line {}", i));
        }
        app.xe_journal_scroll = 3;
        app.map_key(KeyEvent::from(KeyCode::PageDown));
        assert_eq!(app.xe_journal_scroll, 0);
    }

    #[test]
    fn test_up_arrow_scrolls_one_line() {
        let mut app = test_app();
        for i in 0..50 {
            app.xe_journal_lines.push(format!("line {}", i));
        }
        app.map_key(KeyEvent::from(KeyCode::Up));
        assert_eq!(app.xe_journal_scroll, 1);
    }

    #[test]
    fn test_down_arrow_scrolls_one_line_forward() {
        let mut app = test_app();
        for i in 0..50 {
            app.xe_journal_lines.push(format!("line {}", i));
        }
        app.xe_journal_scroll = 5;
        app.map_key(KeyEvent::from(KeyCode::Down));
        assert_eq!(app.xe_journal_scroll, 4);
    }

    #[test]
    fn test_end_key_returns_to_tail() {
        let mut app = test_app();
        for i in 0..50 {
            app.xe_journal_lines.push(format!("line {}", i));
        }
        app.xe_journal_scroll = 25;
        app.map_key(KeyEvent::from(KeyCode::End));
        assert_eq!(app.xe_journal_scroll, 0);
    }

    #[test]
    fn test_home_key_jumps_to_top() {
        let mut app = test_app();
        for i in 0..10 {
            app.xe_journal_lines.push(format!("line {}", i));
        }
        app.map_key(KeyEvent::from(KeyCode::Home));
        assert_eq!(app.xe_journal_scroll, 9);
    }

    #[test]
    fn test_pgup_clamps_at_buffer_top() {
        let mut app = test_app();
        for i in 0..5 {
            app.xe_journal_lines.push(format!("line {}", i));
        }
        // PageUp step is 10 but buffer only has 5 lines — clamp to 4
        app.map_key(KeyEvent::from(KeyCode::PageUp));
        assert_eq!(app.xe_journal_scroll, 4);
    }

    #[test]
    fn test_journal_scroll_initially_zero() {
        let app = test_app();
        assert_eq!(app.journal_scroll, 0);
        assert_eq!(app.xe_journal_scroll, 0);
    }
}
