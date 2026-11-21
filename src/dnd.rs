//! Dragging the shown photo out of glance, onto a file manager (which copies
//! it) or a terminal (which types its path).
//!
//! winit can receive drops but can't start a drag, so on Wayland glance talks
//! to the compositor itself: it opens its own event queue on winit's
//! connection, watches its own `wl_pointer` for the serial of the button press
//! that starts the drag, and offers the file as `text/uri-list` from a
//! `wl_data_source`. X11 isn't supported yet; [`Dnd::new`] returns `None` there.

use std::io::Write;
use std::os::fd::AsFd;
use std::path::Path;
use std::sync::{Arc, Mutex};

use eframe::egui;
use raw_window_handle::{RawDisplayHandle, RawWindowHandle};
use wayland_client::backend::{Backend, ObjectId};
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_data_device_manager::{DndAction, WlDataDeviceManager};
use wayland_client::protocol::wl_data_offer::WlDataOffer;
use wayland_client::protocol::wl_pointer::{self, WlPointer};
use wayland_client::protocol::wl_seat::{self, WlSeat};
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::protocol::{
    wl_buffer::WlBuffer, wl_compositor::WlCompositor, wl_data_device, wl_data_device::WlDataDevice,
    wl_data_source, wl_data_source::WlDataSource, wl_registry, wl_shm, wl_shm::WlShm,
    wl_shm_pool::WlShmPool,
};
use wayland_client::{
    Connection, Dispatch, Proxy, QueueHandle, WEnum, delegate_noop, event_created_child,
};

/// What the event thread and the UI thread share.
#[derive(Default)]
struct Shared {
    /// Serial of the latest button press in glance's window.
    serial: Option<u32>,
    /// The drag icon, kept alive until the drag ends.
    icon: Option<(WlSurface, WlBuffer)>,
    /// A drag just ended (dropped or cancelled) and the UI hasn't been told yet.
    ended: bool,
}

/// State of the event thread.
struct State {
    shared: Arc<Mutex<Shared>>,
    ctx: egui::Context,
    pointer: Option<WlPointer>,
    /// Data offers the compositor gives every client (clipboard, drags over
    /// the window). glance never reads them, so they're just destroyed.
    offers: Vec<WlDataOffer>,
}

pub struct Dnd {
    conn: Connection,
    qh: QueueHandle<State>,
    shared: Arc<Mutex<Shared>>,
    /// glance's window, which the drag starts from.
    window: WlSurface,
    compositor: WlCompositor,
    shm: WlShm,
    manager: WlDataDeviceManager,
    device: WlDataDevice,
}

impl Dnd {
    /// Connects to the compositor glance's window lives on. `None` on X11, or
    /// if the compositor lacks something the drag needs.
    pub fn new(
        display: RawDisplayHandle,
        window: RawWindowHandle,
        ctx: &egui::Context,
    ) -> Option<Self> {
        let (RawDisplayHandle::Wayland(d), RawWindowHandle::Wayland(w)) = (display, window) else {
            return None;
        };
        // SAFETY: both pointers come from winit, whose display and window
        // outlive the app (and so this struct and its thread).
        let conn = Connection::from_backend(unsafe {
            Backend::from_foreign_display(d.display.as_ptr().cast())
        });
        let window_id =
            unsafe { ObjectId::from_ptr(WlSurface::interface(), w.surface.as_ptr().cast()) }
                .ok()?;
        let window = WlSurface::from_id(&conn, window_id).ok()?;

        let (globals, mut queue) = registry_queue_init::<State>(&conn).ok()?;
        let qh = queue.handle();
        // Version 4 at most: from 5 on, wl_surface.attach can't offset the icon.
        let compositor: WlCompositor = globals.bind(&qh, 3..=4, ()).ok()?;
        let shm: WlShm = globals.bind(&qh, 1..=1, ()).ok()?;
        let seat: WlSeat = globals.bind(&qh, 1..=5, ()).ok()?;
        let manager: WlDataDeviceManager = globals.bind(&qh, 1..=3, ()).ok()?;
        let device = manager.get_data_device(&seat, &qh, ());

        let shared = Arc::new(Mutex::new(Shared::default()));
        let mut state = State {
            shared: shared.clone(),
            ctx: ctx.clone(),
            pointer: None,
            offers: Vec::new(),
        };
        // Learn the seat's capabilities (and so get the pointer) before returning.
        queue.roundtrip(&mut state).ok()?;
        std::thread::Builder::new()
            .name("glance-dnd".into())
            .spawn(move || while queue.blocking_dispatch(&mut state).is_ok() {})
            .ok()?;
        Some(Self {
            conn,
            qh,
            shared,
            window,
            compositor,
            shm,
            manager,
            device,
        })
    }

    /// Starts dragging `path` out of the window. `thumb` is the drag icon
    /// (RGBA pixels, width, height), `hotspot` where the pointer holds it, in
    /// the icon's pixels, and `scale` the window's pixels per point.
    /// Returns false if there's no button press to start the drag from.
    pub fn start(
        &self,
        path: &Path,
        thumb: Option<(&[u8], u32, u32)>,
        hotspot: (i32, i32),
        scale: i32,
    ) -> bool {
        let mut sh = self.shared.lock().unwrap();
        let Some(serial) = sh.serial else {
            return false;
        };
        let source = self.manager.create_data_source(&self.qh, file_uri(path));
        source.offer(URI_LIST.into());
        if self.manager.version() >= 3 {
            source.set_actions(DndAction::Copy);
        }
        let icon = thumb.and_then(|(px, w, h)| self.icon(px, w, h, hotspot, scale));
        self.device.start_drag(
            Some(&source),
            &self.window,
            icon.as_ref().map(|i| &i.0),
            serial,
        );
        sh.icon = icon;
        sh.ended = false;
        drop(sh);
        self.conn.flush().is_ok()
    }

    /// True once after each drag ends.
    pub fn take_ended(&self) -> bool {
        std::mem::take(&mut self.shared.lock().unwrap().ended)
    }

    /// A surface showing `px` (RGBA, `w`×`h`), to follow the pointer.
    fn icon(
        &self,
        px: &[u8],
        w: u32,
        h: u32,
        hotspot: (i32, i32),
        scale: i32,
    ) -> Option<(WlSurface, WlBuffer)> {
        let len = (w * h * 4) as usize;
        let fd =
            rustix::fs::memfd_create("glance-drag-icon", rustix::fs::MemfdFlags::CLOEXEC).ok()?;
        let mut file = std::fs::File::from(fd);
        // wl_shm's ARGB8888 is little-endian: B, G, R, A, with alpha premultiplied.
        let mut bgra = Vec::with_capacity(len);
        for p in px.as_chunks::<4>().0 {
            let a = p[3] as u32;
            let m = |c: u8| ((c as u32 * a + 127) / 255) as u8;
            bgra.extend_from_slice(&[m(p[2]), m(p[1]), m(p[0]), p[3]]);
        }
        file.write_all(&bgra).ok()?;
        let pool: WlShmPool = self.shm.create_pool(file.as_fd(), len as i32, &self.qh, ());
        let buffer = pool.create_buffer(
            0,
            w as i32,
            h as i32,
            w as i32 * 4,
            wl_shm::Format::Argb8888,
            &self.qh,
            (),
        );
        pool.destroy();
        let surface = self.compositor.create_surface(&self.qh, ());
        surface.set_buffer_scale(scale);
        // The icon's top-left starts at the pointer; shift it so the pointer
        // holds the photo where it was grabbed (offsets are in points).
        surface.attach(Some(&buffer), -hotspot.0 / scale, -hotspot.1 / scale);
        surface.damage(0, 0, i32::MAX, i32::MAX);
        surface.commit();
        Some((surface, buffer))
    }
}

const URI_LIST: &str = "text/uri-list";

/// `path` as a `file://` URI, with everything but unreserved characters and
/// `/` percent-encoded.
fn file_uri(path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    let abs = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let mut uri = String::from("file://");
    for &b in abs.as_os_str().as_bytes() {
        if b.is_ascii_alphanumeric() || b"/-._~".contains(&b) {
            uri.push(b as char);
        } else {
            uri.push_str(&format!("%{b:02X}"));
        }
    }
    uri
}

impl State {
    fn end_drag(&mut self, source: &WlDataSource) {
        source.destroy();
        let mut sh = self.shared.lock().unwrap();
        if let Some((surface, buffer)) = sh.icon.take() {
            surface.destroy();
            buffer.destroy();
        }
        sh.ended = true;
        drop(sh);
        self.ctx.request_repaint();
    }
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlSeat, ()> for State {
    fn event(
        state: &mut Self,
        seat: &WlSeat,
        event: wl_seat::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Capabilities {
            capabilities: WEnum::Value(caps),
        } = event
        {
            let has = caps.contains(wl_seat::Capability::Pointer);
            match (&state.pointer, has) {
                (None, true) => state.pointer = Some(seat.get_pointer(qh, ())),
                (Some(p), false) => {
                    if p.version() >= 3 {
                        p.release();
                    }
                    state.pointer = None;
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<WlPointer, ()> for State {
    fn event(
        state: &mut Self,
        _: &WlPointer,
        event: wl_pointer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_pointer::Event::Button {
            serial,
            state: WEnum::Value(wl_pointer::ButtonState::Pressed),
            ..
        } = event
        {
            state.shared.lock().unwrap().serial = Some(serial);
        }
    }
}

impl Dispatch<WlDataDevice, ()> for State {
    fn event(
        state: &mut Self,
        _: &WlDataDevice,
        event: wl_data_device::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_data_device::Event::DataOffer { id } => state.offers.push(id),
            wl_data_device::Event::Selection { .. }
            | wl_data_device::Event::Leave
            | wl_data_device::Event::Drop => {
                for o in state.offers.drain(..) {
                    o.destroy();
                }
            }
            _ => {}
        }
    }

    event_created_child!(State, WlDataDevice, [
        wl_data_device::EVT_DATA_OFFER_OPCODE => (WlDataOffer, ()),
    ]);
}

impl Dispatch<WlDataSource, String> for State {
    fn event(
        state: &mut Self,
        source: &WlDataSource,
        event: wl_data_source::Event,
        uri: &String,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_data_source::Event::Send { mime_type, fd } => {
                if mime_type == URI_LIST {
                    let _ = std::fs::File::from(fd).write_all(format!("{uri}\r\n").as_bytes());
                }
            }
            wl_data_source::Event::Cancelled | wl_data_source::Event::DndFinished => {
                state.end_drag(source)
            }
            _ => {}
        }
    }
}

delegate_noop!(State: ignore WlDataOffer);
delegate_noop!(State: ignore WlCompositor);
delegate_noop!(State: ignore WlSurface);
delegate_noop!(State: ignore WlShm);
delegate_noop!(State: ignore WlShmPool);
delegate_noop!(State: ignore WlBuffer);
delegate_noop!(State: WlDataDeviceManager);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uri_escapes_spaces_and_unicode() {
        assert_eq!(
            file_uri(Path::new("/photos/Été 2026/IMG #1.jpg")),
            "file:///photos/%C3%89t%C3%A9%202026/IMG%20%231.jpg"
        );
    }
}
