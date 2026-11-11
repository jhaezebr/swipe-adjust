use crate::brightness::BrightnessBackend;
use crate::config::Config;
use crate::gesture::{EdgeScrubber, ScrubUpdate};
use crate::ipc::{default_socket_path, IpcCommand, IpcServer, OverrideMode};
use crate::noctalia::NoctaliaClient;
use crate::tablet_mode::TabletModeWatcher;
use crate::volume::VolumeBackend;
use crate::wayland::{Side, TouchEvent, WaylandRuntime};
use std::os::fd::AsRawFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

static STOP_REQUESTED: AtomicBool = AtomicBool::new(false);

extern "C" fn handle_signal(_sig: libc::c_int) {
    STOP_REQUESTED.store(true, Ordering::Relaxed);
}

fn install_signal_handlers() {
    let handler = handle_signal as *const () as libc::sighandler_t;
    unsafe {
        libc::signal(libc::SIGINT, handler);
        libc::signal(libc::SIGTERM, handler);
    }
}

struct ThrottleSlot {
    min_interval: Duration,
    last_applied: Option<Instant>,
    pending: Option<ScrubUpdate>,
}

impl ThrottleSlot {
    fn new(hz: u32) -> Self {
        Self {
            min_interval: Duration::from_secs_f64(1.0 / (hz.max(1) as f64)),
            last_applied: None,
            pending: None,
        }
    }

    /// Returns `Some(update)` if it should be applied immediately, or stores it in `pending`
    /// so `next_timeout_ms()` will flush it within `min_interval`.
    fn push(&mut self, update: ScrubUpdate) -> Option<ScrubUpdate> {
        let now = Instant::now();
        let ready = self
            .last_applied
            .is_none_or(|t| now.saturating_duration_since(t) >= self.min_interval);
        if ready {
            self.last_applied = Some(now);
            self.pending = None;
            Some(update)
        } else {
            self.pending = Some(update);
            None
        }
    }

    fn take_if_due(&mut self) -> Option<ScrubUpdate> {
        let pending = self.pending?;
        let now = Instant::now();
        if self
            .last_applied
            .is_none_or(|t| now.saturating_duration_since(t) >= self.min_interval)
        {
            self.last_applied = Some(now);
            self.pending = None;
            Some(pending)
        } else {
            None
        }
    }

    fn flush(&mut self) -> Option<ScrubUpdate> {
        let pending = self.pending.take()?;
        self.last_applied = Some(Instant::now());
        Some(pending)
    }

    fn remaining_ms(&self) -> Option<i32> {
        self.pending?;
        let last = self.last_applied?;
        let elapsed = Instant::now().saturating_duration_since(last);
        if elapsed >= self.min_interval {
            Some(0)
        } else {
            let rem = self.min_interval - elapsed;
            Some((rem.as_millis() as i32).max(1))
        }
    }
}

pub fn run_daemon(cfg: Config) -> Result<(), String> {
    install_signal_handlers();

    let sock_path = default_socket_path();
    let ipc = IpcServer::bind(&sock_path)?;

    let mut wl = WaylandRuntime::connect(&cfg)?;
    let mut tablet = TabletModeWatcher::open(&cfg.tablet_switch_path);
    let mut brightness = BrightnessBackend::new(&cfg.brightness);
    let mut volume = VolumeBackend::new(&cfg.volume);
    let mut noctalia = NoctaliaClient::new(&cfg.osd);

    let min_b = brightness
        .as_ref()
        .map_or(0.01, BrightnessBackend::min_norm);
    let mut left_scrubber = EdgeScrubber::new(cfg.deadzone_px, min_b, 1.0);
    let mut right_scrubber = EdgeScrubber::new(cfg.deadzone_px, 0.0, volume.max_norm());

    let mut left_throttle = ThrottleSlot::new(cfg.throttle_hz);
    let mut right_throttle = ThrottleSlot::new(cfg.throttle_hz);
    let mut last_brightness_val: Option<f64> = None;
    let mut last_volume_val: Option<f64> = None;

    let mut override_mode = OverrideMode::Auto;
    let compute_active = |mode: OverrideMode, hw_tablet: bool| -> bool {
        match mode {
            OverrideMode::ForceEnabled => true,
            OverrideMode::ForceDisabled => false,
            OverrideMode::Auto => {
                if cfg.tablet_mode_only {
                    hw_tablet
                } else {
                    true
                }
            }
        }
    };

    let mut strips_active = compute_active(override_mode, tablet.is_tablet_mode());
    wl.set_mapped(strips_active);
    eprintln!(
        "swipe-adjust: daemon running (strips_active={strips_active}, mode={}, output={})",
        override_mode.as_str(),
        wl.active_output_name()
    );

    while !STOP_REQUESTED.load(Ordering::Relaxed) && !wl.state.closed_by_compositor {
        // 1. Dispatch any buffered Wayland events and prepare read guard for poll().
        wl.event_queue
            .dispatch_pending(&mut wl.state)
            .map_err(|e| format!("Wayland dispatch error: {e}"))?;
        let _ = wl.conn.flush();

        let read_guard = loop {
            if let Some(g) = wl.conn.prepare_read() {
                break g;
            }
            wl.event_queue
                .dispatch_pending(&mut wl.state)
                .map_err(|e| format!("Wayland dispatch error: {e}"))?;
        };

        // 2. Compute poll timeout: -1 (infinite kernel sleep, 0 wakeups/sec) unless a drag update is throttled.
        let timeout_ms = match (left_throttle.remaining_ms(), right_throttle.remaining_ms()) {
            (Some(a), Some(b)) => a.min(b),
            (Some(a), None) => a,
            (None, Some(b)) => b,
            (None, None) => -1,
        };

        let wl_fd = read_guard.connection_fd().as_raw_fd();
        let ipc_fd = ipc.as_raw_fd();
        let mut fds = Vec::with_capacity(3);
        fds.push(libc::pollfd {
            fd: wl_fd,
            events: libc::POLLIN,
            revents: 0,
        });
        fds.push(libc::pollfd {
            fd: ipc_fd,
            events: libc::POLLIN,
            revents: 0,
        });
        let tablet_idx = tablet.as_raw_fd().map(|tfd| {
            let idx = fds.len();
            fds.push(libc::pollfd {
                fd: tfd,
                events: libc::POLLIN,
                revents: 0,
            });
            idx
        });

        let poll_rc =
            unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, timeout_ms) };

        if poll_rc < 0 {
            drop(read_guard);
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return Err(format!("poll() failed: {err}"));
        }

        // 3. Handle Wayland fd readiness (or cancel read guard cleanly).
        if (fds[0].revents & (libc::POLLIN | libc::POLLERR | libc::POLLHUP)) != 0 {
            if let Err(wayland_client::backend::WaylandError::Io(io_err)) = read_guard.read() {
                if io_err.kind() != std::io::ErrorKind::WouldBlock {
                    return Err(format!("Wayland connection closed: {io_err}"));
                }
            }
            wl.event_queue
                .dispatch_pending(&mut wl.state)
                .map_err(|e| format!("Wayland dispatch error: {e}"))?;
        } else {
            drop(read_guard);
        }

        // 4. Process any touch events produced by Wayland dispatch.
        if !wl.state.pending_events.is_empty() {
            let sweep_px = (wl.active_logical_height() as f64) * cfg.sweep_height_ratio;
            let events = std::mem::take(&mut wl.state.pending_events);

            for ev in events {
                match ev {
                    TouchEvent::Down { side, id, y } => match side {
                        Side::Left => left_scrubber.on_touch_down(id, y),
                        Side::Right => right_scrubber.on_touch_down(id, y),
                    },
                    TouchEvent::Motion { side, id, y } => match side {
                        Side::Left => {
                            if let Some(b_backend) = brightness.as_mut() {
                                if let Some(upd) =
                                    left_scrubber.on_touch_motion(id, y, sweep_px, || {
                                        b_backend.read_perceptual()
                                    })
                                {
                                    last_brightness_val = Some(upd.value);
                                    if let Some(imm) = left_throttle.push(upd) {
                                        if b_backend.set_perceptual(imm.value) {
                                            noctalia.notify_brightness(imm.value, false);
                                        }
                                    }
                                }
                            }
                        }
                        Side::Right => {
                            if let Some(upd) =
                                right_scrubber
                                    .on_touch_motion(id, y, sweep_px, || volume.read_norm())
                            {
                                last_volume_val = Some(upd.value);
                                if let Some(imm) = right_throttle.push(upd) {
                                    if volume.set_norm(imm.value, imm.swiping_up) {
                                        noctalia.notify_volume(imm.value, volume.is_muted(), false);
                                    }
                                }
                            }
                        }
                    },
                    TouchEvent::Up { side, id } => match side {
                        Side::Left => {
                            if left_scrubber.on_touch_up(id) {
                                if let Some(final_upd) = left_throttle.flush() {
                                    if let Some(b_backend) = brightness.as_mut() {
                                        let _ = b_backend.set_perceptual(final_upd.value);
                                    }
                                }
                                if let Some(v) = last_brightness_val.take() {
                                    noctalia.notify_brightness(v, true);
                                }
                                noctalia.reset_gesture_cache();
                            }
                        }
                        Side::Right => {
                            if right_scrubber.on_touch_up(id) {
                                if let Some(final_upd) = right_throttle.flush() {
                                    let _ = volume.set_norm(final_upd.value, final_upd.swiping_up);
                                }
                                if let Some(v) = last_volume_val.take() {
                                    noctalia.notify_volume(v, volume.is_muted(), true);
                                }
                                noctalia.reset_gesture_cache();
                            }
                        }
                    },
                    TouchEvent::Cancel => {
                        left_scrubber.reset();
                        right_scrubber.reset();
                        let _ = left_throttle.flush();
                        let _ = right_throttle.flush();
                        last_brightness_val = None;
                        last_volume_val = None;
                        noctalia.reset_gesture_cache();
                    }
                }
            }
        }

        // 5. Flush any due throttled updates when timer expires.
        if let Some(upd) = left_throttle.take_if_due() {
            if let Some(b_backend) = brightness.as_mut() {
                if b_backend.set_perceptual(upd.value) {
                    noctalia.notify_brightness(upd.value, false);
                }
            }
        }
        if let Some(upd) = right_throttle.take_if_due() {
            if volume.set_norm(upd.value, upd.swiping_up) {
                noctalia.notify_volume(upd.value, volume.is_muted(), false);
            }
        }

        // 6. Handle hardware SW_TABLET_MODE switch transitions.
        if let Some(idx) = tablet_idx {
            if (fds[idx].revents & (libc::POLLIN | libc::POLLERR | libc::POLLHUP)) != 0 {
                if let Some(hw_state) = tablet.drain_events() {
                    eprintln!("swipe-adjust: SW_TABLET_MODE changed -> {hw_state}");
                    let next_active = compute_active(override_mode, hw_state);
                    if next_active != strips_active {
                        strips_active = next_active;
                        wl.set_mapped(strips_active);
                        if !strips_active {
                            left_scrubber.reset();
                            right_scrubber.reset();
                        }
                    }
                }
            }
        }

        // 7. Handle runtime CLI / IPC control commands.
        if (fds[1].revents & libc::POLLIN) != 0 {
            let hw_state = tablet.is_tablet_mode();
            let hw_avail = tablet.is_available();
            let switch_desc = tablet.device_description().to_string();
            let out_name = wl.active_output_name();

            let mut mode_changed = false;
            ipc.accept_commands(|cmd| match cmd {
                IpcCommand::Toggle => {
                    let currently_active = compute_active(override_mode, hw_state);
                    override_mode = if currently_active {
                        OverrideMode::ForceDisabled
                    } else {
                        OverrideMode::ForceEnabled
                    };
                    mode_changed = true;
                    let now_active = compute_active(override_mode, hw_state);
                    format!("active={now_active} mode={}\n", override_mode.as_str())
                }
                IpcCommand::Enable => {
                    override_mode = OverrideMode::ForceEnabled;
                    mode_changed = true;
                    "active=true mode=enabled\n".to_string()
                }
                IpcCommand::Disable => {
                    override_mode = OverrideMode::ForceDisabled;
                    mode_changed = true;
                    "active=false mode=disabled\n".to_string()
                }
                IpcCommand::Auto => {
                    override_mode = OverrideMode::Auto;
                    mode_changed = true;
                    let now_active = compute_active(override_mode, hw_state);
                    format!("active={now_active} mode=auto\n")
                }
                IpcCommand::Status => {
                    let now_active = compute_active(override_mode, hw_state);
                    format!(
                        "{{\n  \"active\": {now_active},\n  \"mode\": \"{}\",\n  \"tabletModeHw\": {hw_state},\n  \"tabletSwitchAvailable\": {hw_avail},\n  \"tabletSwitchDevice\": \"{}\",\n  \"output\": \"{}\"\n}}\n",
                        override_mode.as_str(),
                        escape_json(&switch_desc),
                        escape_json(&out_name),
                    )
                }
            });

            if mode_changed {
                let next_active = compute_active(override_mode, tablet.is_tablet_mode());
                if next_active != strips_active {
                    strips_active = next_active;
                    wl.set_mapped(strips_active);
                    if !strips_active {
                        left_scrubber.reset();
                        right_scrubber.reset();
                    }
                    eprintln!(
                        "swipe-adjust: IPC updated strips_active={strips_active} (mode={})",
                        override_mode.as_str()
                    );
                }
            }
        }
    }

    wl.set_mapped(false);
    eprintln!("swipe-adjust: shutting down cleanly");
    Ok(())
}

fn escape_json(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}
