// Server-side subset used by crosvm's display_wl backend.
use super::Capture;
use wayland_protocols::{
    wp::linux_dmabuf::zv1::server::{
        zwp_linux_buffer_params_v1 as params, zwp_linux_dmabuf_v1 as dma,
    },
    xdg::shell::server::{xdg_surface, xdg_toplevel, xdg_wm_base},
};
use wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
    backend::{ClientId, ObjectId},
    protocol::{
        wl_buffer, wl_callback, wl_compositor, wl_region, wl_seat, wl_shm, wl_subcompositor,
        wl_subsurface, wl_surface,
    },
};

#[derive(Default)]
struct Commit {
    buffer: Option<Option<wl_buffer::WlBuffer>>,
    callbacks: Vec<wl_callback::WlCallback>,
}
#[derive(Default)]
pub struct Surface {
    pending: Commit,
    cached: Option<Commit>,
    configure: Option<(xdg_surface::XdgSurface, xdg_toplevel::XdgToplevel)>,
    pub parent: Option<ObjectId>,
    pub position: (i32, i32),
    synchronized: bool,
}
pub fn register(d: &DisplayHandle) {
    d.create_global::<Capture, wl_compositor::WlCompositor, _>(3, ());
    d.create_global::<Capture, wl_subcompositor::WlSubcompositor, _>(1, ());
    d.create_global::<Capture, wl_shm::WlShm, _>(1, ());
    d.create_global::<Capture, wl_seat::WlSeat, _>(5, ());
    d.create_global::<Capture, xdg_wm_base::XdgWmBase, _>(1, ());
    d.create_global::<Capture, dma::ZwpLinuxDmabufV1, _>(1, ());
}

macro_rules! global {
    ($ty:ty, |$res:ident| $body:block) => {
        impl GlobalDispatch<$ty, ()> for Capture {
            fn bind(
                _: &mut Self,
                _: &DisplayHandle,
                _: &Client,
                new: New<$ty>,
                _: &(),
                init: &mut DataInit<'_, Self>,
            ) {
                let $res = init.init(new, ());
                $body
            }
        }
    };
}
global!(wl_compositor::WlCompositor, |_res| {});
global!(wl_subcompositor::WlSubcompositor, |_res| {});
global!(xdg_wm_base::XdgWmBase, |_res| {});
global!(dma::ZwpLinuxDmabufV1, |_res| {});
global!(wl_shm::WlShm, |res| {
    res.format(wl_shm::Format::Argb8888);
    res.format(wl_shm::Format::Xrgb8888);
});
global!(wl_seat::WlSeat, |res| {
    res.capabilities(wl_seat::Capability::empty());
    res.name("capture".into());
});

// Protocol object dispatch signatures differ only in the request and user-data types.
macro_rules! handler {
    ($ty:ty,$data:ty,|$state:ident,$res:ident,$req:ident,$user:ident,$init:ident| $body:block) => {
        impl Dispatch<$ty,$data> for Capture {
           fn request($state:&mut Self,_:&Client,$res:&$ty,$req:<$ty as Resource>::Request,$user:&$data,_:&DisplayHandle,$init:&mut DataInit<'_,Self>) $body
        }
    };
}
handler!(
    wl_compositor::WlCompositor,
    (),
    |state, _res, req, _data, init| {
        match req {
            wl_compositor::Request::CreateSurface { id } => {
                let surface = init.init(id, ());
                state.surfaces.insert(surface.id(), Surface::default());
            }
            wl_compositor::Request::CreateRegion { id } => {
                init.init(id, ());
            }
            _ => {}
        }
    }
);
handler!(
    wl_region::WlRegion,
    (),
    |_state, _res, _req, _data, _init| {}
);
handler!(
    wl_callback::WlCallback,
    (),
    |_state, _res, _req, _data, _init| {}
);
handler!(wl_seat::WlSeat, (), |_state, res, req, _data, _init| {
    if !matches!(req, wl_seat::Request::Release) {
        res.post_error(
            wl_seat::Error::MissingCapability,
            "input uses virtio sockets",
        );
    }
});
handler!(
    xdg_wm_base::XdgWmBase,
    (),
    |_state, res, req, _data, init| {
        match req {
            xdg_wm_base::Request::GetXdgSurface { id, surface } => {
                init.init(id, surface);
            }
            xdg_wm_base::Request::CreatePositioner { .. } => res.post_error(
                xdg_wm_base::Error::InvalidPositioner,
                "only crosvm toplevels are supported",
            ),
            _ => {}
        }
    }
);
handler!(
    xdg_surface::XdgSurface,
    wl_surface::WlSurface,
    |state, res, req, surface, init| {
        match req {
            xdg_surface::Request::GetToplevel { id } => {
                let top = init.init(id, ());
                if let Some(s) = state.surfaces.get_mut(&surface.id()) {
                    s.configure = Some((res.clone(), top));
                }
            }
            xdg_surface::Request::GetPopup { .. } => res.post_error(
                xdg_surface::Error::NotConstructed,
                "only crosvm toplevels are supported",
            ),
            _ => {}
        }
    }
);
handler!(
    xdg_toplevel::XdgToplevel,
    (),
    |_state, _res, _req, _data, _init| {}
);
handler!(
    dma::ZwpLinuxDmabufV1,
    (),
    |_state, _res, req, _data, init| {
        if let dma::Request::CreateParams { params_id } = req {
            init.init(params_id, ());
        }
    }
);
handler!(
    params::ZwpLinuxBufferParamsV1,
    (),
    |_state, res, req, _data, _init| {
        // Advertise no importable formats. The receiver consumes SHM only.
        if let params::Request::Create { .. } = req {
            res.failed();
        }
    }
);
handler!(
    wl_subcompositor::WlSubcompositor,
    (),
    |state, res, req, _data, init| {
        if let wl_subcompositor::Request::GetSubsurface {
            id,
            surface,
            parent,
        } = req
        {
            if surface == parent
                || state
                    .surfaces
                    .get(&parent.id())
                    .is_none_or(|p| p.parent.is_some())
            {
                res.post_error(
                    wl_subcompositor::Error::BadSurface,
                    "expected a scanout parent",
                );
                return;
            }
            let child = state.surfaces.get_mut(&surface.id()).unwrap();
            child.parent = Some(parent.id());
            child.synchronized = true;
            init.init(id, surface);
        }
    }
);
handler!(
    wl_subsurface::WlSubsurface,
    wl_surface::WlSurface,
    |state, _res, req, surface, _init| {
        let Some(child) = state.surfaces.get_mut(&surface.id()) else {
            return;
        };
        match req {
            wl_subsurface::Request::SetPosition { x, y } => child.position = (x, y),
            wl_subsurface::Request::SetDesync => {
                child.synchronized = false;
                if child.cached.is_some() {
                    state.present(surface);
                }
            }
            wl_subsurface::Request::SetSync => child.synchronized = true,
            wl_subsurface::Request::Destroy => child.parent = None,
            _ => {}
        }
    }
);

impl Capture {
    fn present(&mut self, surface: &wl_surface::WlSurface) {
        let s = self.surfaces.get_mut(&surface.id()).unwrap();
        let Some(commit) = s.cached.take() else {
            return;
        };
        if let Some((xdg, top)) = s.configure.take() {
            self.serial = self.serial.wrapping_add(1);
            top.configure(0, 0, vec![]);
            xdg.configure(self.serial);
        }
        match commit.buffer {
            Some(Some(buffer)) => {
                if let Err(e) = self.capture(surface, &buffer) {
                    eprintln!("capture failed: {e}");
                }
                buffer.release();
            }
            Some(None) if s.parent.is_some() => {
                if let super::Output::Stream(sink) = &mut self.output {
                    sink.cursor(0, 0, &[], 0, 0);
                }
            }
            _ => {}
        }
        for cb in commit.callbacks {
            cb.done(self.start.elapsed().as_millis() as u32);
        }
    }
}

impl Dispatch<wl_surface::WlSurface, ()> for Capture {
    fn request(
        state: &mut Self,
        _: &Client,
        surface: &wl_surface::WlSurface,
        req: wl_surface::Request,
        _: &(),
        dh: &DisplayHandle,
        init: &mut DataInit<'_, Self>,
    ) {
        let s = state.surfaces.get_mut(&surface.id()).unwrap();
        match req {
            wl_surface::Request::Attach { buffer, .. } => s.pending.buffer = Some(buffer),
            wl_surface::Request::Frame { callback } => {
                s.pending.callbacks.push(init.init(callback, ()))
            }
            wl_surface::Request::Commit => {
                let cached = s.cached.get_or_insert_with(Commit::default);
                if let Some(buffer) = s.pending.buffer.take() {
                    if let Some(Some(previous)) = cached.buffer.replace(buffer) {
                        previous.release();
                    }
                }
                cached.callbacks.append(&mut s.pending.callbacks);
                if s.synchronized {
                    return;
                }
                state.present(surface);
                let children: Vec<_> = state
                    .surfaces
                    .iter()
                    .filter(|(_, s)| s.parent.as_ref() == Some(&surface.id()) && s.cached.is_some())
                    .map(|(id, _)| id.clone())
                    .collect();
                for id in children {
                    if let Ok(child) = wl_surface::WlSurface::from_id(dh, id) {
                        state.present(&child);
                    }
                }
            }
            _ => {}
        }
    }
    fn destroyed(state: &mut Self, _: ClientId, surface: &wl_surface::WlSurface, _: &()) {
        state.surfaces.remove(&surface.id());
        state.written.remove(&surface.id());
        if let Some(Some(id)) = state.scanouts.remove(&surface.id()) {
            println!("removed scanout {id}");
        }
    }
}
