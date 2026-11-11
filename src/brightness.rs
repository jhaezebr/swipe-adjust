use crate::config::BrightnessConfig;
use crate::gesture::{perceptual_to_raw, raw_to_perceptual};
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use zbus::blocking::Connection;
use zbus::zvariant::{OwnedObjectPath, Value};

pub struct BrightnessBackend {
    cfg: BrightnessConfig,
    device_name: String,
    brightness_path: PathBuf,
    max_raw: u32,
    direct_file: Option<File>,
    dbus: Option<LogindBrightness>,
    last_written_raw: Option<u32>,
}

struct LogindBrightness {
    conn: Connection,
    session_path: OwnedObjectPath,
}

impl BrightnessBackend {
    pub fn new(cfg: &BrightnessConfig) -> Option<Self> {
        let (device_name, dir) = discover_backlight_device(&cfg.device)?;
        let max_raw = read_u32(&dir.join("max_brightness"))?;
        if max_raw == 0 {
            return None;
        }

        let brightness_path = dir.join("brightness");
        let direct_file = fs::OpenOptions::new()
            .write(true)
            .open(&brightness_path)
            .ok();
        let dbus = if direct_file.is_none() {
            match LogindBrightness::connect() {
                Ok(d) => Some(d),
                Err(e) => {
                    eprintln!("swipe-adjust: warning: failed to connect to systemd-logind for brightness: {e}");
                    None
                }
            }
        } else {
            None
        };

        eprintln!(
            "swipe-adjust: brightness backend ready on '{}' (max_brightness={max_raw}, via={})",
            device_name,
            if direct_file.is_some() {
                "sysfs-direct"
            } else if dbus.is_some() {
                "systemd-logind"
            } else {
                "read-only"
            }
        );

        Some(Self {
            cfg: cfg.clone(),
            device_name,
            brightness_path,
            max_raw,
            direct_file,
            dbus,
            last_written_raw: None,
        })
    }

    pub fn min_norm(&self) -> f64 {
        (self.cfg.min_percent / 100.0).clamp(0.0, 0.5)
    }

    /// Reads the current raw brightness from sysfs and returns perceptual `[min_norm, 1.0]`.
    pub fn read_perceptual(&mut self) -> f64 {
        self.last_written_raw = None;
        let raw = read_u32(&self.brightness_path).unwrap_or(self.max_raw / 2);
        raw_to_perceptual(raw, self.max_raw, self.cfg.min_percent, self.cfg.gamma)
    }

    /// Converts perceptual `[min_norm, 1.0]` to raw units and writes if changed.
    /// Returns `true` if a hardware update occurred.
    pub fn set_perceptual(&mut self, norm: f64) -> bool {
        let raw = perceptual_to_raw(norm, self.max_raw, self.cfg.min_percent, self.cfg.gamma);
        if self.last_written_raw == Some(raw) {
            return false;
        }

        if let Some(file) = self.direct_file.as_mut() {
            let buf = format!("{raw}\n");
            if file.write_all(buf.as_bytes()).is_ok() {
                self.last_written_raw = Some(raw);
                return true;
            }
        }

        if let Some(dbus) = self.dbus.as_ref() {
            if dbus.set_brightness(&self.device_name, raw).is_ok() {
                self.last_written_raw = Some(raw);
                return true;
            }
        }

        false
    }
}

impl LogindBrightness {
    fn connect() -> Result<Self, String> {
        let conn =
            Connection::system().map_err(|e| format!("system D-Bus connection failed: {e}"))?;

        let auto_path = OwnedObjectPath::try_from("/org/freedesktop/login1/session/auto")
            .expect("valid object path");

        // Probe whether /org/freedesktop/login1/session/auto resolves for our process
        // (when started under systemd --user, older/strict setups resolve via /user/self Display).
        let auto_ok = conn
            .call_method(
                Some("org.freedesktop.login1"),
                &auto_path,
                Some("org.freedesktop.DBus.Properties"),
                "Get",
                &("org.freedesktop.login1.Session", "Id"),
            )
            .is_ok();

        let session_path = if auto_ok {
            auto_path
        } else {
            resolve_user_display_session(&conn)?
        };

        Ok(Self { conn, session_path })
    }

    fn set_brightness(&self, device: &str, raw: u32) -> Result<(), zbus::Error> {
        self.conn.call_method(
            Some("org.freedesktop.login1"),
            &self.session_path,
            Some("org.freedesktop.login1.Session"),
            "SetBrightness",
            &("backlight", device, raw),
        )?;
        Ok(())
    }
}

fn resolve_user_display_session(conn: &Connection) -> Result<OwnedObjectPath, String> {
    let reply = conn
        .call_method(
            Some("org.freedesktop.login1"),
            "/org/freedesktop/login1/user/self",
            Some("org.freedesktop.DBus.Properties"),
            "Get",
            &("org.freedesktop.login1.User", "Display"),
        )
        .map_err(|e| format!("failed querying login1 user Display property: {e}"))?;

    let body = reply.body();
    let val: Value<'_> = body
        .deserialize()
        .map_err(|e| format!("failed decoding login1 Display variant: {e}"))?;

    if let Value::Structure(s) = val {
        let fields = s.fields();
        if let Some(Value::ObjectPath(p)) = fields.get(1) {
            if !p.as_str().is_empty() && p.as_str() != "/" {
                return Ok(p.to_owned().into());
            }
        }
    }

    Err("no active graphical login1 session found for user".to_string())
}

fn discover_backlight_device(requested: &str) -> Option<(String, PathBuf)> {
    let base = Path::new("/sys/class/backlight");
    if requested != "auto" && !requested.is_empty() {
        let dir = base.join(requested);
        if dir.exists() {
            return Some((requested.to_string(), dir));
        }
    }

    let entries = fs::read_dir(base).ok()?;
    let mut best: Option<(u32, String, PathBuf)> = None;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = entry.path();
        let max_b = read_u32(&path.join("max_brightness")).unwrap_or(0);
        if max_b > 0 && best.as_ref().is_none_or(|(b, _, _)| max_b > *b) {
            best = Some((max_b, name, path));
        }
    }
    best.map(|(_, name, path)| (name, path))
}

fn read_u32(path: &Path) -> Option<u32> {
    fs::read_to_string(path).ok()?.trim().parse::<u32>().ok()
}
