use anyhow::{Context, Result, ensure};
use std::{
    collections::BTreeSet,
    path::PathBuf,
    process::Command,
    sync::{
        Arc, Condvar, Mutex,
        mpsc::{self, RecvTimeoutError, Sender},
    },
    thread,
    time::Duration,
};

type Size = (u32, u32);
#[derive(Default)]
struct State {
    active: Option<(u32, Size)>,
    pending: Option<(BTreeSet<u32>, Size)>,
}
#[derive(Clone, Default)]
pub struct Displays(Arc<(Mutex<State>, Condvar)>);
impl Displays {
    pub fn accept(&self, id: u32, size: Size) -> bool {
        let (lock, changed) = &*self.0;
        let mut state = lock.lock().unwrap();
        if let Some((active, _)) = state.active {
            if active != id {
                if !state
                    .pending
                    .as_ref()
                    .is_some_and(|(before, target)| !before.contains(&id) && *target == size)
                {
                    return false;
                }
                state.pending = None;
            }
        }
        if state.active != Some((id, size)) {
            state.active = Some((id, size));
            changed.notify_all();
        }
        true
    }
    pub fn worker(&self, bin: PathBuf, socket: PathBuf) -> (Sender<Size>, thread::JoinHandle<()>) {
        let (tx, rx) = mpsc::channel();
        let displays = self.clone();
        let join = thread::spawn(move || {
            let gpu = Gpu { bin, socket };
            while let Ok(mut size) = rx.recv() {
                loop {
                    match rx.recv_timeout(Duration::from_millis(200)) {
                        Ok(new) => size = new,
                        Err(RecvTimeoutError::Timeout) => break,
                        Err(RecvTimeoutError::Disconnected) => return,
                    }
                }
                if let Err(e) = displays.resize(&gpu, size) {
                    eprintln!("guest resize: {e:#}");
                }
            }
        });
        (tx, join)
    }
    fn resize(&self, gpu: &Gpu, size: Size) -> Result<()> {
        let (lock, changed) = &*self.0;
        let active = changed
            .wait_timeout_while(lock.lock().unwrap(), Duration::from_secs(5), |s| {
                s.active.is_none()
            })
            .unwrap()
            .0
            .active;
        let (old, current) = active.context("guest has not presented a display")?;
        if current == size {
            return Ok(());
        }
        let before = gpu.ids()?;
        ensure!(before.contains(&old), "active display {old} missing");
        lock.lock().unwrap().pending = Some((before.clone(), size));
        let mode = format!("mode=windowed[{},{}],dpi=[96,96]", size.0, size.1);
        if let Err(e) = gpu.command("add-displays", &["--gpu-display", &mode]) {
            lock.lock().unwrap().pending = None;
            return Err(e);
        }
        let mut state = changed
            .wait_timeout_while(lock.lock().unwrap(), Duration::from_secs(5), |s| {
                s.active.map(|(id, _)| id) == Some(old)
            })
            .unwrap()
            .0;
        let switched = state.active.map(|(id, _)| id) != Some(old);
        state.pending = None;
        drop(state);
        let remove = if switched {
            old
        } else {
            let after = gpu.ids()?;
            let new: Vec<_> = after.difference(&before).copied().collect();
            let [id] = new.as_slice() else {
                anyhow::bail!("no unambiguous replacement to roll back");
            };
            *id
        };
        gpu.command("remove-displays", &["--display-id", &remove.to_string()])?;
        ensure!(switched, "guest did not present {}x{}", size.0, size.1);
        Ok(())
    }
}
struct Gpu {
    bin: PathBuf,
    socket: PathBuf,
}
impl Gpu {
    fn command(&self, op: &str, args: &[&str]) -> Result<Vec<u8>> {
        let out = Command::new(&self.bin)
            .args(["gpu", op])
            .arg(&self.socket)
            .args(args)
            .output()?;
        ensure!(
            out.status.success(),
            "crosvm {op}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        Ok(out.stdout)
    }
    fn ids(&self) -> Result<BTreeSet<u32>> {
        let data: serde_json::Value = serde_json::from_slice(&self.command("list-displays", &[])?)?;
        data["displays"]
            .as_object()
            .context("missing displays")?
            .keys()
            .map(|key| Ok(key.parse()?))
            .collect()
    }
}
