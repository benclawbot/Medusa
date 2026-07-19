#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use std::{error::Error, path::PathBuf};

use medusa_daemon::{DaemonPaths, serve};

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args_os().skip(1);
    if args.next().as_deref() == Some(std::ffi::OsStr::new("__daemon-serve")) {
        let flag = args.next().ok_or("missing --repo for daemon host")?;
        if flag != "--repo" {
            return Err(format!("expected --repo for daemon host, got {flag:?}").into());
        }
        let repo = PathBuf::from(args.next().ok_or("missing repository path for daemon host")?);
        if args.next().is_some() {
            return Err("unexpected extra daemon host arguments".into());
        }
        let repo = repo.canonicalize().unwrap_or(repo);
        serve(DaemonPaths::for_repo(&repo))?;
        return Ok(());
    }

    medusa_desktop_lib::run();
    Ok(())
}
