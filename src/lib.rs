//! Host-side integration with a vvv source installation and crosvm session.
use std::{
    io,
    path::{Path, PathBuf},
    process::Command,
};

/// Boot assets supplied by the embedding application. The disk is opened writable.
pub struct GuestConfig {
    pub kernel: PathBuf,
    pub disk: PathBuf,
    pub initrd: Option<PathBuf>,
    pub cuda_scanout: bool,
}

impl GuestConfig {
    /// Construct the launch command for the receiver's session directory.
    /// Start the receiver first. The caller owns the resulting child process.
    pub fn command(&self, workspace: &Path, session_dir: &Path) -> io::Result<Command> {
        let mut cmd = Command::new(workspace.canonicalize()?.join("guest/start-session.sh"));
        cmd.arg(std::path::absolute(session_dir)?)
            .env("VVV_GUEST_KERNEL", self.kernel.canonicalize()?)
            .env("VVV_GUEST_DISK", self.disk.canonicalize()?)
            .env_remove("VVV_GUEST_INITRD");
        if let Some(path) = &self.initrd {
            cmd.env("VVV_GUEST_INITRD", path.canonicalize()?);
        }
        if self.cuda_scanout {
            cmd.arg("--cuda");
        }
        Ok(cmd)
    }
}
