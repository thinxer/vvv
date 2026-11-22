use super::Capture;
use wayland_server;
use wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
    protocol::{wl_surface, wl_surface::WlSurface},
};

mod __interfaces {
    use wayland_server::protocol::__interfaces::*;
    wayland_scanner::generate_interfaces!("../../vendor/protocol/virtio-gpu-metadata-v1.xml");
}
use __interfaces::*;
wayland_scanner::generate_server_code!("../../vendor/protocol/virtio-gpu-metadata-v1.xml");
use wp_virtio_gpu_metadata_v1::{self as factory, WpVirtioGpuMetadataV1 as Factory};
use wp_virtio_gpu_surface_metadata_v1::{
    self as metadata, WpVirtioGpuSurfaceMetadataV1 as Metadata,
};

pub fn register(display: &DisplayHandle) {
    display.create_global::<Capture, Factory, _>(1, ());
}

impl GlobalDispatch<Factory, ()> for Capture {
    fn bind(
        _: &mut Self,
        _: &DisplayHandle,
        _: &Client,
        new: New<Factory>,
        _: &(),
        init: &mut DataInit<'_, Self>,
    ) {
        init.init(new, ());
    }
}
impl Dispatch<Factory, ()> for Capture {
    fn request(
        state: &mut Self,
        _: &Client,
        factory: &Factory,
        request: factory::Request,
        _: &(),
        _: &DisplayHandle,
        init: &mut DataInit<'_, Self>,
    ) {
        let factory::Request::GetSurfaceMetadata { id, surface } = request;
        if state.scanouts.contains_key(&surface.id()) {
            factory.post_error(
                factory::Error::SurfaceMetadataExists,
                "duplicate scanout metadata",
            );
            return;
        }
        state.scanouts.insert(surface.id(), None);
        init.init(id, surface);
    }
}
impl Dispatch<Metadata, WlSurface> for Capture {
    fn request(
        state: &mut Self,
        _: &Client,
        metadata: &Metadata,
        request: metadata::Request,
        surface: &WlSurface,
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
        let metadata::Request::SetScanoutId { scanout_id } = request;
        match state.scanouts.get_mut(&surface.id()) {
            Some(slot) => *slot = Some(scanout_id),
            None => metadata.post_error(metadata::Error::NoSurface, "scanout surface destroyed"),
        }
    }
}
