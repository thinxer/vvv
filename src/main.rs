//! Workspace entry point for the crosvm-backed receivers.
use std::env;
use std::error::Error;
use std::fs::{self, DirBuilder};
use std::os::unix::{fs::DirBuilderExt, process::CommandExt};
use std::path::{Path, PathBuf};
use std::process::Command;

const USAGE: &str =
    "vvv serve --protocol rdp|apple|both --session NAME [--dev-login] [--cuda] [--avg-bitrate BPS] [-- RECEIVER_OPTIONS...]
vvv guest --session NAME [--kernel PATH --disk PATH [--initrd PATH] | --example kde] [--cuda]
vvv set-password --session NAME -- --user USER [--password-file /state/FILE]";

fn workspace() -> Result<PathBuf, Box<dyn Error>> {
    if let Some(path) = env::var_os("VVV_ROOT") {
        return Ok(fs::canonicalize(path)?);
    }
    for start in [env::current_exe()?, env::current_dir()?] {
        for dir in start.ancestors() {
            if dir.join("guest/start-session.sh").is_file()
                && dir.join("crates/apple/run.sh").is_file()
            {
                return Ok(dir.to_owned());
            }
        }
    }
    Err("cannot find workspace; set VVV_ROOT".into())
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = env::args().skip(1);
    let action = args.next().unwrap_or_else(|| "--help".into());
    if matches!(action.as_str(), "-h" | "--help" | "help") {
        println!("{USAGE}");
        return Ok(());
    }
    let mut protocol = None;
    let mut session = None;
    let mut dev = false;
    let mut cuda = false;
    let (mut kernel, mut disk, mut initrd, mut example) = (None, None, None, None);
    let mut options = Vec::new();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--protocol" if action == "serve" => {
                protocol = Some(args.next().ok_or("missing protocol")?)
            }
            "--kernel" if action == "guest" => {
                kernel = Some(PathBuf::from(args.next().ok_or("missing kernel path")?))
            }
            "--disk" if action == "guest" => {
                disk = Some(PathBuf::from(args.next().ok_or("missing disk path")?))
            }
            "--initrd" if action == "guest" => {
                initrd = Some(PathBuf::from(args.next().ok_or("missing initrd path")?))
            }
            "--example" if action == "guest" => {
                example = Some(args.next().ok_or("missing example name")?)
            }
            "--session" => session = Some(args.next().ok_or("missing session name")?),
            "--cuda" if matches!(action.as_str(), "guest" | "serve") => cuda = true,
            "--avg-bitrate" if action == "serve" => {
                let bitrate = args
                    .next()
                    .ok_or("missing average bitrate in bits per second")?
                    .parse::<std::num::NonZeroU32>()?;
                options.extend([arg, bitrate.to_string()]);
            }
            "--dev-login" if action == "serve" => dev = true,
            "--" => {
                options.extend(args);
                break;
            }
            _ => return Err(format!("unexpected option {arg}\n{USAGE}").into()),
        }
    }
    let name = session.ok_or("specify --session NAME")?;
    if name.is_empty()
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
    {
        return Err("session names accept letters, digits, hyphens and underscores".into());
    }
    let root = workspace()?;
    let state = root.join(".local/sessions").join(name);
    let apple = root.join("crates/apple/run.sh");
    let mut cmd = match (action.as_str(), protocol.as_deref()) {
        ("guest", None) if options.is_empty() => match (example.as_deref(), kernel, disk, initrd) {
            (None, Some(kernel), Some(disk), initrd) => vvv::GuestConfig {
                kernel,
                disk,
                initrd,
                cuda_scanout: cuda,
            }
            .command(&root, &state)?,
            (profile @ (None | Some("kde")), None, None, None)
                if profile.is_some() || state.join("kde.img").is_file() =>
            {
                let mut cmd = Command::new(root.join("guest/start-session.sh"));
                cmd.arg(&state)
                    .env_remove("VVV_GUEST_DISK")
                    .env_remove("VVV_GUEST_KERNEL");
                if cuda {
                    cmd.arg("--cuda");
                }
                cmd
            }
            _ => {
                return Err(
                    "supply --kernel and --disk (optional --initrd), or --example kde".into(),
                );
            }
        },
        ("serve", Some("rdp")) if !dev => {
            let receiver = env::current_exe()?.with_file_name("wayland-ironrdp-receiver");
            if !receiver.is_file() {
                return Err("missing sibling RDP receiver; run ./build.sh".into());
            }
            let mut cmd = locked_receiver(&state, &receiver);
            let mut libs = vec![root.join("runtime/lib")];
            if let Some(old) = env::var_os("LD_LIBRARY_PATH") {
                libs.extend(env::split_paths(&old));
            }
            cmd.env("LD_LIBRARY_PATH", env::join_paths(libs)?);
            cmd.arg(state.join("frames.sock"))
                .arg(state.join("captures"))
                .arg("--mouse-sock")
                .arg(state.join("mouse.sock"))
                .arg("--keyboard-sock")
                .arg(state.join("keyboard.sock"))
                .arg("--crosvm-sock")
                .arg(state.join("crosvm.sock"));
            if cuda {
                cmd.arg("--cuda-socket").arg(state.join("cuda.sock"));
            }
            cmd
        }
        ("serve", Some("both")) => {
            let binary = root.join("target/release/vvv-combined");
            if !binary.is_file() {
                return Err("missing combined receiver; run ./build.sh".into());
            }
            let mut cmd = locked_receiver(&state, &binary);
            cmd.arg("--socket").arg(state.join("frames.sock"));
            if dev {
                cmd.arg("--dev-login");
            } else {
                cmd.arg("--srp-file").arg(state.join("verifier"));
            }
            if cuda {
                cmd.arg("--cuda-socket").arg(state.join("cuda.sock"));
            }
            cmd
        }
        ("serve", Some("apple")) => {
            let mut cmd = locked_receiver(&state, &apple);
            cmd.arg(if dev { "serve-dev" } else { "serve" });
            if cuda {
                cmd.arg("--cuda-socket").arg("/state/cuda.sock");
            }
            cmd.env("APPLE_STATE_DIR", &state);
            cmd
        }
        ("set-password", None) => {
            let mut cmd = Command::new(apple);
            cmd.arg("set-password").env("APPLE_STATE_DIR", &state);
            cmd
        }
        _ => return Err(USAGE.into()),
    };
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&state)?;
    let error = cmd.current_dir(root).args(options).exec();
    Err(error.into())
}

fn locked_receiver(state: &Path, receiver: &Path) -> Command {
    let mut cmd = Command::new("flock");
    cmd.args(["--nonblock", "--no-fork"])
        .arg(state.join("receiver.lock"))
        .arg(receiver);
    cmd
}
