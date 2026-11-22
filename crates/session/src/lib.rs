//! All joined viewers receive frames. Only the primary may affect the guest.
use cuda_scanout::display::DisplayLayout;
use std::sync::{Arc, Mutex};

type Action = Box<dyn Fn() + Send + Sync>;
type Resize = Box<dyn Fn(DisplayLayout) + Send + Sync>;

struct Member {
    id: u64,
    protocol: &'static str,
    release: Action,
    resize: Resize,
    preferred: Option<DisplayLayout>,
    held: u8,
    suppressed: u8,
}

#[derive(Default)]
struct State {
    next_id: u64,
    // The primary is first. Disconnect promotes the next connected viewer.
    members: Vec<Member>,
}

#[derive(Default)]
pub struct ViewerSeat {
    state: Mutex<State>,
    views: tokio::sync::watch::Sender<Views>,
    pub observer_cursor: Mutex<Option<cuda_scanout::overlay::ObserverCursor>>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Views {
    members: Vec<(u64, &'static str, Option<DisplayLayout>)>,
    promoting: bool,
}
impl Views {
    pub fn status(&self) -> Option<(&'static str, usize)> {
        (self.members.len() > 1).then(|| (self.members[0].1, self.members.len()))
    }
    fn canvas(&self, index: usize, guest: (u16, u16)) -> Option<(u16, u16)> {
        (index != 0 || self.promoting)
            .then(|| self.members[index].2.map_or(guest, |layout| layout.backing))
    }
    pub fn apple_observing(&self) -> bool {
        self.members.iter().skip(1).any(|v| v.1 == "Apple")
    }
    pub fn rdp_observing(&self) -> bool {
        self.members.iter().skip(1).any(|v| v.1 == "RDP")
    }
    pub fn apple_canvas(&self, guest: (u16, u16)) -> Option<(u16, u16)> {
        self.members
            .iter()
            .position(|v| v.1 == "Apple")
            .and_then(|i| self.canvas(i, guest))
    }
}

pub struct Viewer {
    seat: Arc<ViewerSeat>,
    id: u64,
}

impl ViewerSeat {
    pub fn subscribe(&self) -> tokio::sync::watch::Receiver<Views> {
        self.views.subscribe()
    }
    fn publish(&self, state: &State, promoting: bool) {
        self.views.send_replace(Views {
            promoting,
            members: state
                .members
                .iter()
                .map(|m| (m.id, m.protocol, m.preferred))
                .collect(),
        });
    }
    pub fn join(
        self: &Arc<Self>,
        protocol: &'static str,
        release: impl Fn() + Send + Sync + 'static,
        resize: impl Fn(DisplayLayout) + Send + Sync + 'static,
    ) -> Viewer {
        let mut state = self.state.lock().unwrap();
        state.next_id += 1;
        let id = state.next_id;
        eprintln!(
            "{protocol} viewer {id} joined (primary={})",
            state.members.is_empty()
        );
        state.members.push(Member {
            id,
            protocol,
            release: Box::new(release),
            resize: Box::new(resize),
            preferred: None,
            held: 0,
            suppressed: 0,
        });
        self.publish(&state, false);
        Viewer {
            seat: self.clone(),
            id,
        }
    }
}

impl Member {
    fn promote(&self) {
        eprintln!("{} viewer {} took control", self.protocol, self.id);
        if let Some(layout) = self.preferred {
            (self.resize)(layout);
        }
    }
}

impl Viewer {
    pub fn observing(&self) -> bool {
        self.seat
            .views
            .borrow()
            .members
            .iter()
            .skip(1)
            .any(|v| v.0 == self.id)
    }
    pub fn canvas(&self, guest: (u16, u16)) -> Option<(u16, u16)> {
        let views = self.seat.views.borrow();
        views
            .members
            .iter()
            .position(|v| v.0 == self.id)
            .and_then(|i| views.canvas(i, guest))
    }
    pub fn input<T>(&self, send: impl FnOnce() -> T) -> Option<T> {
        let state = self.seat.state.lock().unwrap();
        state
            .members
            .first()
            .filter(|m| m.id == self.id)
            .map(|_| send())
    }

    /// Update the indicated physical button bits. Suppress the full takeover gesture,
    /// including repeats and its release. Wheel bits never participate in ownership.
    pub fn pointer<T>(
        &self,
        changed_mask: u8,
        down: u8,
        position: (i32, i32),
        send: impl FnOnce() -> T,
    ) -> Option<T> {
        let mut state = self.seat.state.lock().unwrap();
        let index = state.members.iter().position(|m| m.id == self.id)?;
        let member = &mut state.members[index];
        let pressed = down & changed_mask & !member.held;
        member.held = (member.held & !changed_mask) | (down & changed_mask);
        let suppressed = member.suppressed & changed_mask != 0;
        member.suppressed &= member.held;
        if index != 0 {
            if pressed & 1 != 0 && cuda_scanout::overlay::Stamp::hit(position.0, position.1) {
                let old = &mut state.members[0];
                (old.release)();
                old.suppressed = old.held;
                let mut new = state.members.remove(index);
                new.suppressed = new.held;
                state.members.insert(0, new);
                // Keep both outputs at their preferred canvas until the guest presents
                // the new primary size. This avoids an intermediate HP renegotiation.
                self.seat.publish(&state, true);
                state.members[0].promote();
                self.seat.publish(&state, false);
            }
            return None;
        }
        (!suppressed).then(send)
    }

    pub fn layout(&self, layout: DisplayLayout) -> bool {
        let (w, h) = layout.backing;
        let mut state = self.seat.state.lock().unwrap();
        let Some(index) = state.members.iter().position(|m| m.id == self.id) else {
            return false;
        };
        let member = &mut state.members[index];
        if !(320..=8192).contains(&w) || !(200..=8192).contains(&h) {
            return false;
        }
        let changed = member.preferred != Some(layout);
        member.preferred = Some(layout);
        if index == 0 {
            (member.resize)(layout);
        }
        if changed {
            self.seat.publish(&state, false);
        }
        index == 0
    }

    /// Release this viewer now. Dropping it also releases ownership.
    pub fn leave(&self) {
        let mut state = self.seat.state.lock().unwrap();
        if let Some(index) = state.members.iter().position(|m| m.id == self.id) {
            let old = state.members.remove(index);
            if index == 0 {
                (old.release)();
                self.seat.publish(&state, true);
                if let Some(next) = state.members.first() {
                    next.promote();
                }
            }
            self.seat.publish(&state, false);
        }
    }
}
impl Drop for Viewer {
    fn drop(&mut self) {
        self.leave();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn clicks_transfer_control_and_replay_only_the_new_primary_layout() {
        let seat = Arc::new(ViewerSeat::default());
        let log = Arc::new(Mutex::new(Vec::new()));
        let join = |name| {
            let reset = log.clone();
            let resize = log.clone();
            seat.join(
                name,
                move || reset.lock().unwrap().push((name, 0, 0)),
                move |layout| {
                    resize
                        .lock()
                        .unwrap()
                        .push((name, layout.backing.0, layout.backing.1))
                },
            )
        };
        let apple = join("Apple");
        let rdp = join("RDP");
        assert!(!apple.observing() && rdp.observing());
        assert!(seat.subscribe().borrow().rdp_observing());
        assert!(apple.layout(DisplayLayout::from_rdp((1280, 720), None)));
        assert!(!rdp.layout(DisplayLayout::from_rdp((900, 600), Some(150))));
        assert!(rdp.input(|| ()).is_none());
        assert!(rdp.pointer(1, 0, (20, 20), || ()).is_none());
        assert!(rdp.pointer(1, 1, (200, 200), || ()).is_none());
        assert!(rdp.pointer(1, 0, (200, 200), || ()).is_none());
        assert!(apple.input(|| ()).is_some());
        assert!(apple.pointer(7, 2, (20, 20), || ()).is_some());
        assert!(rdp.pointer(1, 1, (20, 20), || ()).is_none());
        assert!(apple.observing() && !rdp.observing());
        assert!(seat.subscribe().borrow().apple_observing());
        // The old primary is still dragging. Repeated button masks cannot reclaim the seat.
        assert!(apple.pointer(7, 2, (20, 20), || ()).is_none());
        assert!(rdp.pointer(1, 1, (20, 20), || ()).is_none());
        assert!(rdp.pointer(1, 0, (20, 20), || ()).is_none());
        assert!(rdp.pointer(1, 1, (20, 20), || ()).is_some());
        assert!(apple.pointer(7, 0, (20, 20), || ()).is_none());
        assert!(apple.pointer(7, 1, (20, 20), || ()).is_none());
        assert!(apple.pointer(7, 0, (20, 20), || ()).is_none());
        assert!(apple.input(|| ()).is_some());
        drop(apple);
        assert!(rdp.input(|| ()).is_some());
        assert_eq!(
            *log.lock().unwrap(),
            [
                ("Apple", 1280, 720),
                ("Apple", 0, 0),
                ("RDP", 900, 600),
                ("RDP", 0, 0),
                ("Apple", 1280, 720),
                ("Apple", 0, 0),
                ("RDP", 900, 600)
            ]
        );
    }
}
