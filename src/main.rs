mod brightness;
mod config;
mod daemon;
mod gesture;
mod ipc;
mod noctalia;
mod tablet_mode;
mod volume;
mod wayland;

use crate::config::Config;
use crate::ipc::{default_socket_path, send_command, IpcCommand};
use std::path::PathBuf;
use std::process::ExitCode;

const USAGE: &str = "\
swipe-adjust — Touchscreen edge-swipe brightness & volume daemon for Wayland

USAGE:
  swipe-adjust [daemon] [--config <path>]   Run the Wayland edge-swipe daemon
  swipe-adjust toggle                       Toggle edge strips on/off
  swipe-adjust enable                       Force-enable edge strips (even in laptop mode)
  swipe-adjust disable                      Force-disable edge strips
  swipe-adjust auto                         Follow hardware SW_TABLET_MODE switch automatically
  swipe-adjust status                       Print daemon status as JSON
  swipe-adjust --help | -h                  Show this help message
  swipe-adjust --version | -V               Show version
";

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let mut subcmd: Option<String> = None;
    let mut config_path: Option<PathBuf> = None;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" | "help" => {
                print!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            "-V" | "--version" | "version" => {
                println!("swipe-adjust {}", env!("CARGO_PKG_VERSION"));
                return ExitCode::SUCCESS;
            }
            "-c" | "--config" => match args.next() {
                Some(p) => config_path = Some(PathBuf::from(p)),
                None => {
                    eprintln!("error: --config requires a file path");
                    return ExitCode::FAILURE;
                }
            },
            other if other.starts_with("--config=") => {
                let p = &other["--config=".len()..];
                config_path = Some(PathBuf::from(p));
            }
            other if other.starts_with('-') => {
                eprintln!("error: unknown option '{other}'\n\n{USAGE}");
                return ExitCode::FAILURE;
            }
            other => {
                if subcmd.is_none() {
                    subcmd = Some(other.to_string());
                } else {
                    eprintln!("error: unexpected argument '{other}'\n\n{USAGE}");
                    return ExitCode::FAILURE;
                }
            }
        }
    }

    match subcmd.as_deref().unwrap_or("daemon") {
        "daemon" => {
            let cfg = match Config::load(config_path.as_deref()) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("swipe-adjust: {e}");
                    return ExitCode::FAILURE;
                }
            };
            if let Err(e) = daemon::run_daemon(cfg) {
                eprintln!("swipe-adjust: {e}");
                return ExitCode::FAILURE;
            }
            ExitCode::SUCCESS
        }
        other => match IpcCommand::parse(other) {
            Some(cmd) => {
                let sock = default_socket_path();
                match send_command(&sock, cmd) {
                    Ok(resp) => {
                        print!("{resp}");
                        ExitCode::SUCCESS
                    }
                    Err(e) => {
                        eprintln!("swipe-adjust: {e}");
                        ExitCode::FAILURE
                    }
                }
            }
            None => {
                eprintln!("error: unknown command '{other}'\n\n{USAGE}");
                ExitCode::FAILURE
            }
        },
    }
}
