use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NoctaliaSyncMode {
    #[default]
    Auto,
    Always,
    Never,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct BrightnessConfig {
    /// Backlight sysfs directory name under /sys/class/backlight, or "auto".
    pub device: String,
    /// Minimum perceptual brightness percentage (clamped >= 0.5% by default to prevent black screen).
    pub min_percent: f64,
    /// Perceptual gamma exponent (2.2 matches human luminance perception).
    pub gamma: f64,
}

impl Default for BrightnessConfig {
    fn default() -> Self {
        Self {
            device: "auto".to_string(),
            min_percent: 1.0,
            gamma: 2.2,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct VolumeConfig {
    /// Maximum speaker volume percentage allowed via swipe (e.g. 100.0).
    pub max_percent: f64,
    /// Automatically mute sink when swiped down to 0%.
    pub auto_mute_at_zero: bool,
    /// Automatically unmute sink when swiping upward.
    pub auto_unmute_on_up: bool,
}

impl Default for VolumeConfig {
    fn default() -> Self {
        Self {
            max_percent: 100.0,
            auto_mute_at_zero: true,
            auto_unmute_on_up: true,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct OsdConfig {
    /// Whether to notify Noctalia's IPC socket for OSD feedback ("auto" | "always" | "never").
    pub noctalia_sync: NoctaliaSyncMode,
    /// Max rate (Hz) at which OSD IPC messages are emitted during continuous drags.
    pub throttle_hz: u32,
}

impl Default for OsdConfig {
    fn default() -> Self {
        Self {
            noctalia_sync: NoctaliaSyncMode::Auto,
            throttle_hz: 30,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Target Wayland output connector name (e.g. "eDP-1"), or "auto" for internal/primary output.
    pub output: String,
    /// Enable edge strips only when folded into tablet mode (SW_TABLET_MODE == 1).
    pub tablet_mode_only: bool,
    /// Preferred evdev path for SW_TABLET_MODE switch; falls back to scanning /dev/input/event*.
    pub tablet_switch_path: String,
    /// Width of left and right touch strips in Wayland logical pixels.
    pub edge_width_px: u32,
    /// Fraction of screen height left free at BOTH top and bottom (0.12 = middle 76% active).
    pub vertical_margin_ratio: f64,
    /// Fraction of screen height corresponding to a full 0% -> 100% scrub.
    pub sweep_height_ratio: f64,
    /// Vertical finger movement (logical px) required before scrubbing locks in.
    pub deadzone_px: f64,
    /// Maximum hardware update rate (Hz) during continuous finger motion.
    pub throttle_hz: u32,
    pub brightness: BrightnessConfig,
    pub volume: VolumeConfig,
    pub osd: OsdConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            output: "auto".to_string(),
            tablet_mode_only: true,
            tablet_switch_path: "/dev/input/by-path/platform-gpio-keys.0.auto-event".to_string(),
            edge_width_px: 22,
            vertical_margin_ratio: 0.12,
            sweep_height_ratio: 0.60,
            deadzone_px: 10.0,
            throttle_hz: 60,
            brightness: BrightnessConfig::default(),
            volume: VolumeConfig::default(),
            osd: OsdConfig::default(),
        }
    }
}

impl Config {
    pub fn default_path() -> Option<PathBuf> {
        if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
            if !xdg.is_empty() {
                return Some(PathBuf::from(xdg).join("swipe-adjust/config.toml"));
            }
        }
        std::env::var("HOME")
            .ok()
            .map(|h| PathBuf::from(h).join(".config/swipe-adjust/config.toml"))
    }

    pub fn load(explicit_path: Option<&Path>) -> Result<Self, String> {
        let path = match explicit_path {
            Some(p) => Some(p.to_path_buf()),
            None => Self::default_path().filter(|p| p.exists()),
        };

        let mut cfg = match path {
            Some(p) => {
                let raw = fs::read_to_string(&p)
                    .map_err(|e| format!("failed to read config {}: {e}", p.display()))?;
                toml::from_str::<Config>(&raw)
                    .map_err(|e| format!("failed to parse TOML in {}: {e}", p.display()))?
            }
            None => Config::default(),
        };

        cfg.sanitize();
        Ok(cfg)
    }

    pub fn sanitize(&mut self) {
        self.edge_width_px = self.edge_width_px.clamp(4, 120);
        self.vertical_margin_ratio = self.vertical_margin_ratio.clamp(0.0, 0.45);
        self.sweep_height_ratio = self.sweep_height_ratio.clamp(0.10, 2.0);
        self.deadzone_px = self.deadzone_px.clamp(0.0, 100.0);
        self.throttle_hz = self.throttle_hz.clamp(10, 240);

        self.brightness.min_percent = self.brightness.min_percent.clamp(0.0, 50.0);
        self.brightness.gamma = self.brightness.gamma.clamp(0.5, 5.0);

        self.volume.max_percent = self.volume.max_percent.clamp(10.0, 150.0);
        self.osd.throttle_hz = self.osd.throttle_hz.clamp(5, 120);
    }
}
