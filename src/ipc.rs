use std::fs;
use std::io::{Read, Write};
use std::net::Shutdown;
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverrideMode {
    Auto,
    ForceEnabled,
    ForceDisabled,
}

impl OverrideMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::ForceEnabled => "enabled",
            Self::ForceDisabled => "disabled",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IpcCommand {
    Toggle,
    Enable,
    Disable,
    Auto,
    Status,
}

impl IpcCommand {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim() {
            "toggle" => Some(Self::Toggle),
            "enable" | "on" => Some(Self::Enable),
            "disable" | "off" => Some(Self::Disable),
            "auto" => Some(Self::Auto),
            "status" => Some(Self::Status),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Toggle => "toggle",
            Self::Enable => "enable",
            Self::Disable => "disable",
            Self::Auto => "auto",
            Self::Status => "status",
        }
    }
}

pub fn default_socket_path() -> PathBuf {
    if let Ok(dir) = std::env::var("XDG_RUNTIME_DIR") {
        if !dir.is_empty() {
            return PathBuf::from(dir).join("swipe-adjust.sock");
        }
    }
    let uid = unsafe { libc::getuid() };
    PathBuf::from(format!("/tmp/swipe-adjust-{uid}.sock"))
}

pub struct IpcServer {
    listener: UnixListener,
    path: PathBuf,
}

impl IpcServer {
    pub fn bind(path: &Path) -> Result<Self, String> {
        if path.exists() {
            // Check whether another instance is alive on this socket.
            if UnixStream::connect(path).is_ok() {
                return Err(format!(
                    "another swipe-adjust daemon is already listening on {}",
                    path.display()
                ));
            }
            let _ = fs::remove_file(path);
        }

        let listener = UnixListener::bind(path)
            .map_err(|e| format!("failed to bind IPC socket {}: {e}", path.display()))?;
        listener
            .set_nonblocking(true)
            .map_err(|e| format!("failed to set IPC socket nonblocking: {e}"))?;

        Ok(Self {
            listener,
            path: path.to_path_buf(),
        })
    }

    pub fn as_raw_fd(&self) -> RawFd {
        self.listener.as_raw_fd()
    }

    pub fn accept_commands<F>(&self, mut handler: F)
    where
        F: FnMut(IpcCommand) -> String,
    {
        loop {
            match self.listener.accept() {
                Ok((mut stream, _)) => {
                    let _ = stream.set_read_timeout(Some(Duration::from_millis(100)));
                    let _ = stream.set_write_timeout(Some(Duration::from_millis(100)));
                    let mut req = String::new();
                    if stream.read_to_string(&mut req).is_ok() {
                        let resp = match IpcCommand::parse(&req) {
                            Some(cmd) => handler(cmd),
                            None => format!("error: unknown command '{}'\n", req.trim()),
                        };
                        let _ = stream.write_all(resp.as_bytes());
                        let _ = stream.shutdown(Shutdown::Both);
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(_) => break,
            }
        }
    }
}

impl Drop for IpcServer {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

pub fn send_command(path: &Path, cmd: IpcCommand) -> Result<String, String> {
    let mut stream = UnixStream::connect(path).map_err(|e| {
        format!(
            "could not connect to swipe-adjust daemon at {}: {e}",
            path.display()
        )
    })?;
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));

    stream
        .write_all(cmd.as_str().as_bytes())
        .map_err(|e| format!("failed writing to IPC socket: {e}"))?;
    let _ = stream.shutdown(Shutdown::Write);

    let mut resp = String::new();
    stream
        .read_to_string(&mut resp)
        .map_err(|e| format!("failed reading IPC response: {e}"))?;
    Ok(resp)
}
