use std::{error::Error, path::PathBuf};
fn main() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args_os().skip(1);
    let socket = PathBuf::from(
        args.next()
            .ok_or("usage: wayland-capture SOCKET OUTPUT_DIR")?,
    );
    let output = PathBuf::from(args.next().ok_or("missing output directory")?);
    wayland_capture::snapshots(socket, output)
}
