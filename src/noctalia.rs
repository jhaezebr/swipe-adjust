use crate::config::{NoctaliaSyncMode, OsdConfig};
use std::io::Write;
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Zero-fork IPC client for Noctalia Shell (`$XDG_RUNTIME_DIR/noctalia-$WAYLAND_DISPLAY.sock`).
pub struct NoctaliaClient {
    mode: NoctaliaSyncMode,
    socket_path: Option<PathBuf>,
    min_interval: Duration,
    last_brightness_pct: Option<u32>,
    last_brightness_sent: Option<Instant>,
    last_volume_pct: Option<u32>,
    last_volume_sent: Option<Instant>,
}

impl NoctaliaClient {
    pub fn new(cfg: &OsdConfig) -> Self {
        let socket_path = discover_noctalia_socket();
        let min_interval = Duration::from_secs_f64(1.0 / (cfg.throttle_hz.max(1) as f64));
        Self {
            mode: cfg.noctalia_sync,
            socket_path,
            min_interval,
            last_brightness_pct: None,
            last_brightness_sent: None,
            last_volume_pct: None,
            last_volume_sent: None,
        }
    }

    fn should_send(&self) -> bool {
        match self.mode {
            NoctaliaSyncMode::Never => false,
            NoctaliaSyncMode::Always => self.socket_path.is_some(),
            NoctaliaSyncMode::Auto => self.socket_path.as_ref().is_some_and(|p| p.exists()),
        }
    }

    pub fn notify_brightness(&mut self, norm: f64, force: bool) {
        if !self.should_send() {
            return;
        }
        let pct = (norm * 100.0).round().clamp(0.0, 100.0) as u32;
        if !force {
            if self.last_brightness_pct == Some(pct) {
                return;
            }
            if let Some(prev) = self.last_brightness_sent {
                if prev.elapsed() < self.min_interval {
                    return;
                }
            }
        }
        if self.send_msg(&format!("brightness-osd {pct}")) {
            self.last_brightness_pct = Some(pct);
            self.last_brightness_sent = Some(Instant::now());
        }
    }

    pub fn notify_volume(&mut self, norm: f64, muted: bool, force: bool) {
        if !self.should_send() {
            return;
        }
        let pct = if muted {
            0
        } else {
            (norm * 100.0).round().clamp(0.0, 150.0) as u32
        };
        if !force {
            if self.last_volume_pct == Some(pct) {
                return;
            }
            if let Some(prev) = self.last_volume_sent {
                if prev.elapsed() < self.min_interval {
                    return;
                }
            }
        }
        // `volume-osd` without args reads current sink mute/volume directly inside Noctalia;
        // passing `<pct>` overrides displayed percentage. When muted, `volume-osd` shows mute icon.
        let cmd = if muted {
            "volume-osd".to_string()
        } else {
            format!("volume-osd {pct}")
        };
        if self.send_msg(&cmd) {
            self.last_volume_pct = Some(pct);
            self.last_volume_sent = Some(Instant::now());
        }
    }

    pub fn reset_gesture_cache(&mut self) {
        self.last_brightness_pct = None;
        self.last_volume_pct = None;
    }

    /// Sends `<cwd>\x1e<command>` to Noctalia's Unix domain socket without spawning a process.
    pub fn send_msg(&self, command: &str) -> bool {
        let Some(path) = self.socket_path.as_ref() else {
            return false;
        };
        let Ok(mut stream) = UnixStream::connect(path) else {
            return false;
        };
        let _ = stream.set_write_timeout(Some(Duration::from_millis(20)));
        let payload = format!("/\x1e{command}");
        let ok = stream.write_all(payload.as_bytes()).is_ok();
        let _ = stream.shutdown(Shutdown::Write);
        ok
    }
}

fn discover_noctalia_socket() -> Option<PathBuf> {
    let runtime_dir = PathBuf::from(std::env::var("XDG_RUNTIME_DIR").ok()?);
    if let Ok(wl_display) = std::env::var("WAYLAND_DISPLAY") {
        if !wl_display.is_empty() {
            return Some(runtime_dir.join(format!("noctalia-{wl_display}.sock")));
        }
    }
    Some(runtime_dir.join("noctalia-wayland-0.sock"))
}
