use std::path::PathBuf;

use anyhow::{Context, Result};

const APP_NAME: &str = "MeetliteCapture.app";

pub fn installed_agent_path() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").context("could not determine home directory")?;
    Ok(PathBuf::from(home)
        .join("Library/Application Support/Meetlite")
        .join(APP_NAME))
}
