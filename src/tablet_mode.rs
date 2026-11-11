use std::ffi::CStr;
use std::fs::{self, File};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

const EV_SW: u16 = 0x05;
const SW_TABLET_MODE: u16 = 0x01;
const SW_MAX: usize = 0x10;
const SW_BYTES: usize = SW_MAX.div_ceil(8);

// Linux _IOC(_IOC_READ, 'E', nr, size) on x86_64 / aarch64:
// dir = 2 (_IOC_READ), type = 0x45 ('E')
const fn ioc_read_e(nr: libc::c_ulong, size: usize) -> libc::c_ulong {
    (2 << 30) | ((size as libc::c_ulong) << 16) | (0x45 << 8) | nr
}

const EVIOCGNAME_256: libc::c_ulong = ioc_read_e(0x06, 256);
const EVIOCGSW_BYTES: libc::c_ulong = ioc_read_e(0x1b, SW_BYTES);
const EVIOCGBIT_EV_SW: libc::c_ulong = ioc_read_e(0x20 + (EV_SW as libc::c_ulong), SW_BYTES);

pub struct TabletModeWatcher {
    file: Option<File>,
    device_desc: String,
    state: bool,
}

impl TabletModeWatcher {
    pub fn open(preferred_path: &str) -> Self {
        if let Some((file, desc, initial)) = Self::try_open_switch(Path::new(preferred_path)) {
            eprintln!(
                "swipe-adjust: watching SW_TABLET_MODE on {desc} (initial tablet_mode={initial})"
            );
            return Self {
                file: Some(file),
                device_desc: desc,
                state: initial,
            };
        }

        if let Some((file, desc, initial)) = Self::scan_input_devices() {
            eprintln!(
                "swipe-adjust: watching SW_TABLET_MODE on {desc} (initial tablet_mode={initial})"
            );
            return Self {
                file: Some(file),
                device_desc: desc,
                state: initial,
            };
        }

        eprintln!(
            "swipe-adjust: warning: could not open an input device with SW_TABLET_MODE (checked '{}' and /dev/input/event*). \
             Enable `hardware.swipe-adjust.enable = true` in NixOS for udev seat access, or run `swipe-adjust enable` to force-activate.",
            preferred_path
        );
        Self {
            file: None,
            device_desc: "unavailable".to_string(),
            state: false,
        }
    }

    pub fn is_available(&self) -> bool {
        self.file.is_some()
    }

    pub fn is_tablet_mode(&self) -> bool {
        self.state
    }

    pub fn device_description(&self) -> &str {
        &self.device_desc
    }

    pub fn as_raw_fd(&self) -> Option<RawFd> {
        self.file.as_ref().map(AsRawFd::as_raw_fd)
    }

    /// Drains all queued input events from the switch fd.
    /// Returns `Some(new_state)` if `SW_TABLET_MODE` changed.
    pub fn drain_events(&mut self) -> Option<bool> {
        let fd = self.file.as_ref()?.as_raw_fd();
        let mut changed = None;
        let event_size = std::mem::size_of::<libc::input_event>();
        let mut buf = [std::mem::MaybeUninit::<libc::input_event>::uninit(); 16];

        loop {
            let byte_len = buf.len() * event_size;
            let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), byte_len) };
            if n < 0 {
                let err = std::io::Error::last_os_error();
                if err.raw_os_error() == Some(libc::EAGAIN)
                    || err.raw_os_error() == Some(libc::EINTR)
                {
                    break;
                }
                eprintln!("swipe-adjust: tablet switch device disconnected ({err})");
                self.file = None;
                break;
            }
            if n == 0 {
                break;
            }

            let count = (n as usize) / event_size;
            for item in buf.iter().take(count) {
                let ev = unsafe { item.assume_init() };
                if ev.type_ == EV_SW && ev.code == SW_TABLET_MODE {
                    let new_state = ev.value != 0;
                    if new_state != self.state {
                        self.state = new_state;
                        changed = Some(new_state);
                    }
                }
            }
        }

        changed
    }

    fn scan_input_devices() -> Option<(File, String, bool)> {
        let mut entries: Vec<PathBuf> = fs::read_dir("/dev/input")
            .ok()?
            .filter_map(|e| e.ok().map(|d| d.path()))
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("event"))
            })
            .collect();
        entries.sort();

        for path in entries {
            if let Some(found) = Self::try_open_switch(&path) {
                return Some(found);
            }
        }
        None
    }

    fn try_open_switch(path: &Path) -> Option<(File, String, bool)> {
        let file = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(path)
            .ok()?;

        let fd = file.as_raw_fd();

        // Check EV_SW capability bitmask for SW_TABLET_MODE.
        let mut sw_caps = [0u8; SW_BYTES];
        let rc = unsafe { libc::ioctl(fd, EVIOCGBIT_EV_SW as _, sw_caps.as_mut_ptr()) };
        if rc < 0 || !bit_is_set(&sw_caps, SW_TABLET_MODE as usize) {
            return None;
        }

        // Read initial switch state via EVIOCGSW.
        let mut sw_state = [0u8; SW_BYTES];
        let initial = if unsafe { libc::ioctl(fd, EVIOCGSW_BYTES as _, sw_state.as_mut_ptr()) } >= 0
        {
            bit_is_set(&sw_state, SW_TABLET_MODE as usize)
        } else {
            false
        };

        let mut name_buf = [0u8; 256];
        let name = if unsafe { libc::ioctl(fd, EVIOCGNAME_256 as _, name_buf.as_mut_ptr()) } > 0 {
            CStr::from_bytes_until_nul(&name_buf)
                .ok()
                .and_then(|c| c.to_str().ok())
                .unwrap_or("unknown")
                .to_string()
        } else {
            "unknown".to_string()
        };

        let desc = format!("{} ({name})", path.display());
        Some((file, desc, initial))
    }
}

fn bit_is_set(bytes: &[u8], bit: usize) -> bool {
    let idx = bit / 8;
    let mask = 1u8 << (bit % 8);
    bytes.get(idx).is_some_and(|b| (b & mask) != 0)
}
