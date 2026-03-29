use std::os::unix::io::AsRawFd;

use evdev::Key;

use crate::engine::real_ops::kmsg_log;

/// Result of polling evdev devices for keyboard activity.
pub struct ActivityResult {
    /// Whether any keyboard event was detected.
    pub any_activity: bool,
    /// Whether any non-modifier key was pressed (crossterm will also see these).
    pub has_non_modifier: bool,
}

/// Watches keyboard input devices via evdev for activity detection.
/// Used only for screen unblank — crossterm handles actual key processing.
pub struct EvdevWatcher {
    devices: Vec<evdev::Device>,
}

impl EvdevWatcher {
    /// Open all keyboard devices in /dev/input/ with non-blocking I/O.
    /// Returns a watcher with an empty device list if none are found or accessible.
    pub fn open() -> Self {
        let mut devices = Vec::new();

        for (_path, device) in evdev::enumerate() {
            let is_keyboard = device
                .supported_keys()
                .is_some_and(|keys| keys.contains(Key::KEY_A));

            if !is_keyboard {
                continue;
            }

            let fd = device.as_raw_fd();
            let flags = nix::fcntl::fcntl(fd, nix::fcntl::FcntlArg::F_GETFL).unwrap_or(0);
            let new_flags = nix::fcntl::OFlag::from_bits_truncate(flags)
                | nix::fcntl::OFlag::O_NONBLOCK;
            if let Err(e) = nix::fcntl::fcntl(fd, nix::fcntl::FcntlArg::F_SETFL(new_flags)) {
                kmsg_log(&format!("evdev: failed to set non-blocking on {:?}: {}", device.name(), e));
                continue;
            }

            devices.push(device);
        }

        if devices.is_empty() {
            kmsg_log("evdev: no keyboard devices found, falling back to crossterm-only");
        } else {
            kmsg_log(&format!("evdev: watching {} keyboard device(s)", devices.len()));
        }

        Self { devices }
    }

    /// Drain all pending events and report activity.
    pub fn has_activity(&mut self) -> ActivityResult {
        let mut any_activity = false;
        let mut has_non_modifier = false;

        for device in &mut self.devices {
            match device.fetch_events() {
                Ok(events) => {
                    for event in events {
                        any_activity = true;
                        if let evdev::InputEventKind::Key(key) = event.kind() {
                            if !is_modifier_key(key) {
                                has_non_modifier = true;
                            }
                        }
                    }
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(ref e) => {
                    kmsg_log(&format!("evdev: fetch_events error: {}", e));
                }
            }
        }

        ActivityResult {
            any_activity,
            has_non_modifier,
        }
    }
}

/// Returns true if the key is a modifier (Ctrl, Shift, Alt, Meta).
pub fn is_modifier_key(key: Key) -> bool {
    matches!(
        key,
        Key::KEY_LEFTCTRL
            | Key::KEY_RIGHTCTRL
            | Key::KEY_LEFTSHIFT
            | Key::KEY_RIGHTSHIFT
            | Key::KEY_LEFTALT
            | Key::KEY_RIGHTALT
            | Key::KEY_LEFTMETA
            | Key::KEY_RIGHTMETA
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_modifier_key_classification() {
        assert!(is_modifier_key(Key::KEY_LEFTCTRL));
        assert!(is_modifier_key(Key::KEY_RIGHTCTRL));
        assert!(is_modifier_key(Key::KEY_LEFTSHIFT));
        assert!(is_modifier_key(Key::KEY_RIGHTSHIFT));
        assert!(is_modifier_key(Key::KEY_LEFTALT));
        assert!(is_modifier_key(Key::KEY_RIGHTALT));
        assert!(is_modifier_key(Key::KEY_LEFTMETA));
        assert!(is_modifier_key(Key::KEY_RIGHTMETA));
    }

    #[test]
    fn test_non_modifier_keys() {
        assert!(!is_modifier_key(Key::KEY_A));
        assert!(!is_modifier_key(Key::KEY_ENTER));
        assert!(!is_modifier_key(Key::KEY_ESC));
        assert!(!is_modifier_key(Key::KEY_SPACE));
        assert!(!is_modifier_key(Key::KEY_Q));
    }

    #[test]
    fn test_empty_watcher_no_activity() {
        let mut watcher = EvdevWatcher {
            devices: Vec::new(),
        };
        let result = watcher.has_activity();
        assert!(!result.any_activity);
        assert!(!result.has_non_modifier);
    }
}
