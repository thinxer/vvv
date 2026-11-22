// Modified for vvv from WayShare. SPDX-License-Identifier: Apache-2.0
use anyhow::{Context, Result, bail};
use demand::Input;
use std::path::Path;

pub fn current_user() -> Result<String> {
    std::env::var("USER").context("USER not set; pass --user")
}

/// Reads a password from `path`, trimming the trailing newline, or prompts for it.
pub fn read_password(path: Option<&Path>) -> Result<Vec<u8>> {
    let Some(p) = path else {
        return prompt_password("Screen Sharing asks for this when you connect");
    };
    let raw = std::fs::read(p).with_context(|| format!("reading {}", p.display()))?;
    let end = raw
        .iter()
        .rposition(|&b| b != b'\n' && b != b'\r')
        .map_or(0, |i| i + 1);
    if end == 0 {
        bail!("empty password");
    }
    Ok(raw[..end].to_vec())
}

fn prompt_password(description: &str) -> Result<Vec<u8>> {
    loop {
        let a = Input::new("Password")
            .description(description)
            .password(true)
            .validation(|s| {
                if s.is_empty() {
                    Err("Enter a password")
                } else {
                    Ok(())
                }
            })
            .run()?;
        let b = Input::new("Confirm password").password(true).run()?;
        if a == b {
            return Ok(a.into_bytes());
        }
        eprintln!("Passwords did not match; try again");
    }
}
