use super::{Screen, virtio_input::VirtioInput};
use anyhow::Result;
use std::sync::Arc;
use std::time::Instant;
use wayshare::input::{
    InputSink, ScrollEvent,
    keyboard::{KbAction, KeyboardEngine},
    pointer::PointerEngine,
};

pub struct GuestInput {
    screen: Arc<Screen>,
    keys: Arc<VirtioInput>,
    keyboard: KeyboardEngine,
    pointer: PointerEngine,
    tablet: Arc<VirtioInput>,
}

impl GuestInput {
    pub fn new(
        screen: Arc<Screen>,
        keys: Arc<VirtioInput>,
        tablet: Arc<VirtioInput>,
        w: u32,
        h: u32,
    ) -> Self {
        Self {
            screen,
            keys,
            tablet,
            keyboard: KeyboardEngine::new(Default::default()),
            pointer: PointerEngine::new(Default::default(), w, h),
        }
    }
}

fn keys(device: &VirtioInput, actions: Vec<KbAction>) -> Result<()> {
    for action in actions {
        if let KbAction::Key { code, down } = action {
            device.key(code, down)?;
        }
    }
    Ok(())
}

impl InputSink for GuestInput {
    fn key(&mut self, down: bool, sym: u32, code: Option<u16>) -> Result<()> {
        *self.screen.last_input.lock().unwrap() = Some(Instant::now());
        keys(&self.keys, self.keyboard.key(down, sym, code))
    }
    fn pointer(&mut self, x: i32, y: i32, buttons: u8) -> Result<()> {
        *self.screen.last_input.lock().unwrap() = Some(Instant::now());
        self.screen.set_pointer((x, y));
        self.tablet.pointer(self.pointer.pointer(x, y, buttons))
    }
    fn scroll(&mut self, event: &ScrollEvent) -> Result<()> {
        *self.screen.last_input.lock().unwrap() = Some(Instant::now());
        self.tablet.pointer(self.pointer.scroll(event))
    }
    fn set_extent(&mut self, w: u32, h: u32) {
        self.pointer.set_extent(w, h);
    }
    fn set_input_source(&mut self, _: &str) -> Result<()> {
        Ok(())
    }
    fn release_all(&mut self) -> Result<()> {
        keys(&self.keys, self.keyboard.release_all())?;
        self.tablet.pointer(self.pointer.release_all())
    }
}
