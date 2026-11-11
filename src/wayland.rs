use crate::config::Config;
use std::collections::HashMap;
use std::ffi::CString;
use std::os::fd::{BorrowedFd, OwnedFd};
use wayland_client::protocol::{
    wl_buffer, wl_compositor, wl_output, wl_registry, wl_seat, wl_shm, wl_shm_pool, wl_surface,
    wl_touch,
};
use wayland_client::{delegate_noop, Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum};
use wayland_protocols::wp::single_pixel_buffer::v1::client::wp_single_pixel_buffer_manager_v1::WpSinglePixelBufferManagerV1;
use wayland_protocols::wp::viewporter::client::{
    wp_viewport::WpViewport, wp_viewporter::WpViewporter,
};
use wayland_protocols::xdg::xdg_output::zv1::client::{
    zxdg_output_manager_v1::ZxdgOutputManagerV1,
    zxdg_output_v1::{self, ZxdgOutputV1},
};
use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1::{Layer, ZwlrLayerShellV1},
    zwlr_layer_surface_v1::{self, Anchor, KeyboardInteractivity, ZwlrLayerSurfaceV1},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Side {
    Left,
    Right,
}

#[derive(Debug, Clone, Copy)]
pub enum TouchEvent {
    Down { side: Side, id: i32, y: f64 },
    Motion { side: Side, id: i32, y: f64 },
    Up { side: Side, id: i32 },
    Cancel,
}

struct OutputTrack {
    global_name: u32,
    wl_output: wl_output::WlOutput,
    xdg_output: Option<ZxdgOutputV1>,
    name: Option<String>,
    logical_size: Option<(i32, i32)>,
    mode_size: Option<(i32, i32)>,
    scale: i32,
    transform: WEnum<wl_output::Transform>,
}

impl OutputTrack {
    fn logical_height(&self) -> u32 {
        if let Some((_, h)) = self.logical_size {
            if h > 0 {
                return h as u32;
            }
        }
        if let Some((mw, mh)) = self.mode_size {
            let rotated = matches!(
                self.transform,
                WEnum::Value(
                    wl_output::Transform::_90
                        | wl_output::Transform::_270
                        | wl_output::Transform::Flipped90
                        | wl_output::Transform::Flipped270
                )
            );
            let raw_h = if rotated { mw } else { mh };
            let s = self.scale.max(1);
            return (raw_h / s).max(100) as u32;
        }
        540
    }
}

struct EdgeStrip {
    surface: wl_surface::WlSurface,
    layer_surface: ZwlrLayerSurfaceV1,
    viewport: Option<WpViewport>,
    shm_buffer: Option<wl_buffer::WlBuffer>,
    applied_margin: i32,
}

impl EdgeStrip {
    fn destroy(self) {
        self.layer_surface.destroy();
        if let Some(vp) = self.viewport {
            vp.destroy();
        }
        self.surface.destroy();
        if let Some(buf) = self.shm_buffer {
            buf.destroy();
        }
    }
}

pub struct WaylandState {
    target_output_cfg: String,
    edge_width_px: u32,
    vertical_margin_ratio: f64,

    compositor: Option<wl_compositor::WlCompositor>,
    layer_shell: Option<ZwlrLayerShellV1>,
    single_pixel_mgr: Option<WpSinglePixelBufferManagerV1>,
    viewporter: Option<WpViewporter>,
    shm: Option<wl_shm::WlShm>,
    xdg_output_mgr: Option<ZxdgOutputManagerV1>,

    seat: Option<wl_seat::WlSeat>,
    touch: Option<wl_touch::WlTouch>,

    outputs: Vec<OutputTrack>,
    single_pixel_buf: Option<wl_buffer::WlBuffer>,

    desired_mapped: bool,
    mapped_output_global: Option<u32>,
    left_strip: Option<EdgeStrip>,
    right_strip: Option<EdgeStrip>,

    touch_slots: HashMap<i32, Side>,
    pub pending_events: Vec<TouchEvent>,
    pub closed_by_compositor: bool,
}

pub struct WaylandRuntime {
    pub conn: Connection,
    pub event_queue: EventQueue<WaylandState>,
    pub qh: QueueHandle<WaylandState>,
    pub state: WaylandState,
}

impl WaylandRuntime {
    pub fn connect(cfg: &Config) -> Result<Self, String> {
        let conn = Connection::connect_to_env()
            .map_err(|e| format!("failed to connect to Wayland display: {e}"))?;
        let mut event_queue = conn.new_event_queue();
        let qh = event_queue.handle();

        let _registry = conn.display().get_registry(&qh, ());

        let mut state = WaylandState {
            target_output_cfg: cfg.output.clone(),
            edge_width_px: cfg.edge_width_px,
            vertical_margin_ratio: cfg.vertical_margin_ratio,
            compositor: None,
            layer_shell: None,
            single_pixel_mgr: None,
            viewporter: None,
            shm: None,
            xdg_output_mgr: None,
            seat: None,
            touch: None,
            outputs: Vec::new(),
            single_pixel_buf: None,
            desired_mapped: false,
            mapped_output_global: None,
            left_strip: None,
            right_strip: None,
            touch_slots: HashMap::new(),
            pending_events: Vec::new(),
            closed_by_compositor: false,
        };

        // Two roundtrips: 1) discover globals, 2) receive initial output names/sizes & seat capabilities.
        event_queue
            .roundtrip(&mut state)
            .map_err(|e| format!("Wayland initial roundtrip failed: {e}"))?;
        event_queue
            .roundtrip(&mut state)
            .map_err(|e| format!("Wayland secondary roundtrip failed: {e}"))?;

        if state.compositor.is_none() {
            return Err("Wayland compositor missing wl_compositor".to_string());
        }
        if state.layer_shell.is_none() {
            return Err("Wayland compositor does not advertise zwlr_layer_shell_v1".to_string());
        }

        Ok(Self {
            conn,
            event_queue,
            qh,
            state,
        })
    }

    pub fn set_mapped(&mut self, mapped: bool) {
        self.state.set_desired_mapped(mapped, &self.qh);
        let _ = self.conn.flush();
    }

    pub fn active_logical_height(&self) -> u32 {
        self.state.active_logical_height()
    }

    pub fn active_output_name(&self) -> String {
        self.state
            .selected_output()
            .and_then(|o| o.name.clone())
            .unwrap_or_else(|| "auto".to_string())
    }
}

impl WaylandState {
    fn selected_output(&self) -> Option<&OutputTrack> {
        if self.target_output_cfg != "auto" && !self.target_output_cfg.is_empty() {
            if let Some(exact) = self
                .outputs
                .iter()
                .find(|o| o.name.as_deref() == Some(self.target_output_cfg.as_str()))
            {
                return Some(exact);
            }
        }

        // Prefer built-in laptop panels (eDP-*, DSI-*, LVDS-*) when set to "auto".
        if let Some(internal) = self.outputs.iter().find(|o| {
            o.name.as_deref().is_some_and(|n| {
                n.starts_with("eDP-") || n.starts_with("DSI-") || n.starts_with("LVDS-")
            })
        }) {
            return Some(internal);
        }

        self.outputs.first()
    }

    pub fn active_logical_height(&self) -> u32 {
        if let Some(global) = self.mapped_output_global {
            if let Some(out) = self.outputs.iter().find(|o| o.global_name == global) {
                return out.logical_height();
            }
        }
        self.selected_output()
            .map(OutputTrack::logical_height)
            .unwrap_or(540)
    }

    pub fn set_desired_mapped(&mut self, mapped: bool, qh: &QueueHandle<Self>) {
        self.desired_mapped = mapped;
        self.reconcile_surfaces(qh);
    }

    fn unmap_strips(&mut self) {
        if let Some(left) = self.left_strip.take() {
            left.destroy();
        }
        if let Some(right) = self.right_strip.take() {
            right.destroy();
        }
        self.mapped_output_global = None;
        if !self.touch_slots.is_empty() {
            self.touch_slots.clear();
            self.pending_events.push(TouchEvent::Cancel);
        }
    }

    fn reconcile_surfaces(&mut self, qh: &QueueHandle<Self>) {
        if !self.desired_mapped {
            if self.left_strip.is_some() || self.right_strip.is_some() {
                self.unmap_strips();
            }
            return;
        }

        let Some((target_global, wl_out, logical_h)) = self
            .selected_output()
            .map(|o| (o.global_name, o.wl_output.clone(), o.logical_height()))
        else {
            return;
        };

        // If target output changed (e.g. output hotplug), recreate strips on the right output.
        if self.mapped_output_global.is_some() && self.mapped_output_global != Some(target_global) {
            self.unmap_strips();
        }

        let margin = ((logical_h as f64) * self.vertical_margin_ratio).round() as i32;

        if self.left_strip.is_some() && self.right_strip.is_some() {
            // Update margins dynamically if logical height changed (e.g. 90-degree tablet rotation).
            for strip in [self.left_strip.as_mut(), self.right_strip.as_mut()]
                .into_iter()
                .flatten()
            {
                if strip.applied_margin != margin {
                    strip.applied_margin = margin;
                    strip.layer_surface.set_margin(margin, 0, margin, 0);
                    strip.surface.commit();
                }
            }
            return;
        }

        let (Some(comp), Some(ls)) = (self.compositor.as_ref(), self.layer_shell.as_ref()) else {
            return;
        };

        self.left_strip = Some(Self::create_strip(
            comp,
            ls,
            self.viewporter.as_ref(),
            &wl_out,
            Side::Left,
            self.edge_width_px,
            margin,
            qh,
        ));
        self.right_strip = Some(Self::create_strip(
            comp,
            ls,
            self.viewporter.as_ref(),
            &wl_out,
            Side::Right,
            self.edge_width_px,
            margin,
            qh,
        ));
        self.mapped_output_global = Some(target_global);
    }

    #[allow(clippy::too_many_arguments)]
    fn create_strip(
        comp: &wl_compositor::WlCompositor,
        ls: &ZwlrLayerShellV1,
        viewporter: Option<&WpViewporter>,
        wl_out: &wl_output::WlOutput,
        side: Side,
        edge_width_px: u32,
        margin: i32,
        qh: &QueueHandle<Self>,
    ) -> EdgeStrip {
        let surface = comp.create_surface(qh, ());
        let viewport = viewporter.map(|vp| vp.get_viewport(&surface, qh, ()));

        let namespace = match side {
            Side::Left => "swipe-adjust-left",
            Side::Right => "swipe-adjust-right",
        };
        let layer_surface = ls.get_layer_surface(
            &surface,
            Some(wl_out),
            Layer::Overlay,
            namespace.to_string(),
            qh,
            side,
        );

        let side_anchor = match side {
            Side::Left => Anchor::Left,
            Side::Right => Anchor::Right,
        };
        layer_surface.set_anchor(Anchor::Top | Anchor::Bottom | side_anchor);
        layer_surface.set_size(edge_width_px, 0);
        layer_surface.set_margin(margin, 0, margin, 0);
        layer_surface.set_exclusive_zone(-1);
        layer_surface.set_keyboard_interactivity(KeyboardInteractivity::None);

        // Initial commit without buffer triggers the compositor's initial Configure event.
        surface.commit();

        EdgeStrip {
            surface,
            layer_surface,
            viewport,
            shm_buffer: None,
            applied_margin: margin,
        }
    }

    fn configure_strip(
        &mut self,
        side: Side,
        serial: u32,
        width: u32,
        height: u32,
        qh: &QueueHandle<Self>,
    ) {
        let w = if width > 0 { width } else { self.edge_width_px };
        let h = if height > 0 { height } else { 400 };

        // Ensure reusable 1x1 transparent single-pixel buffer exists if supported.
        if self.single_pixel_buf.is_none() {
            if let Some(mgr) = self.single_pixel_mgr.as_ref() {
                self.single_pixel_buf = Some(mgr.create_u32_rgba_buffer(0, 0, 0, 0, qh, ()));
            }
        }

        let sp_buf = self.single_pixel_buf.clone();
        let shm = self.shm.clone();

        let strip = match side {
            Side::Left => self.left_strip.as_mut(),
            Side::Right => self.right_strip.as_mut(),
        };
        let Some(strip) = strip else {
            return;
        };

        strip.layer_surface.ack_configure(serial);

        if let (Some(buf), Some(vp)) = (sp_buf.as_ref(), strip.viewport.as_ref()) {
            strip.surface.attach(Some(buf), 0, 0);
            vp.set_destination(w as i32, h as i32);
            strip.surface.commit();
            return;
        }

        // Fallback: allocate a transparent ARGB8888 wl_shm buffer via memfd_create.
        if let Some(shm) = shm.as_ref() {
            if let Some(old) = strip.shm_buffer.take() {
                old.destroy();
            }
            if let Some(buf) = create_transparent_shm_buffer(shm, w, h, qh) {
                strip.surface.attach(Some(&buf), 0, 0);
                strip.surface.damage_buffer(0, 0, w as i32, h as i32);
                strip.shm_buffer = Some(buf);
                strip.surface.commit();
            }
        }
    }

    fn side_for_surface(&self, surface: &wl_surface::WlSurface) -> Option<Side> {
        if self
            .left_strip
            .as_ref()
            .is_some_and(|s| s.surface.id() == surface.id())
        {
            return Some(Side::Left);
        }
        if self
            .right_strip
            .as_ref()
            .is_some_and(|s| s.surface.id() == surface.id())
        {
            return Some(Side::Right);
        }
        None
    }
}

fn create_transparent_shm_buffer(
    shm: &wl_shm::WlShm,
    width: u32,
    height: u32,
    qh: &QueueHandle<WaylandState>,
) -> Option<wl_buffer::WlBuffer> {
    let stride = (width as i32).checked_mul(4)?;
    let size = stride.checked_mul(height as i32)?;
    let name = CString::new("swipe-adjust-shm").ok()?;
    let raw_fd = unsafe { libc::memfd_create(name.as_ptr(), libc::MFD_CLOEXEC) };
    if raw_fd < 0 {
        return None;
    }
    let fd = unsafe { OwnedFd::from_raw_fd(raw_fd) };
    if unsafe { libc::ftruncate(raw_fd, size as libc::off_t) } < 0 {
        return None;
    }
    let borrowed: BorrowedFd<'_> = unsafe { BorrowedFd::borrow_raw(raw_fd) };
    let pool = shm.create_pool(borrowed, size, qh, ());
    let buffer = pool.create_buffer(
        0,
        width as i32,
        height as i32,
        stride,
        wl_shm::Format::Argb8888,
        qh,
        (),
    );
    pool.destroy();
    drop(fd);
    Some(buffer)
}

use std::os::fd::FromRawFd;

impl Dispatch<wl_registry::WlRegistry, ()> for WaylandState {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            wl_registry::Event::Global {
                name,
                interface,
                version,
            } => match interface.as_str() {
                "wl_compositor" => {
                    state.compositor = Some(registry.bind(name, version.min(6), qh, ()));
                }
                "zwlr_layer_shell_v1" => {
                    state.layer_shell = Some(registry.bind(name, version.min(4), qh, ()));
                }
                "wp_single_pixel_buffer_manager_v1" => {
                    state.single_pixel_mgr = Some(registry.bind(name, 1, qh, ()));
                }
                "wp_viewporter" => {
                    state.viewporter = Some(registry.bind(name, 1, qh, ()));
                }
                "wl_shm" => {
                    state.shm = Some(registry.bind(name, 1, qh, ()));
                }
                "zxdg_output_manager_v1" => {
                    let mgr: ZxdgOutputManagerV1 = registry.bind(name, version.min(3), qh, ());
                    for out in &mut state.outputs {
                        if out.xdg_output.is_none() {
                            out.xdg_output =
                                Some(mgr.get_xdg_output(&out.wl_output, qh, out.global_name));
                        }
                    }
                    state.xdg_output_mgr = Some(mgr);
                }
                "wl_seat" => {
                    if state.seat.is_none() {
                        state.seat = Some(registry.bind(name, version.min(8), qh, ()));
                    }
                }
                "wl_output" => {
                    let wl_out: wl_output::WlOutput = registry.bind(name, version.min(4), qh, name);
                    let xdg_out = state
                        .xdg_output_mgr
                        .as_ref()
                        .map(|mgr| mgr.get_xdg_output(&wl_out, qh, name));
                    state.outputs.push(OutputTrack {
                        global_name: name,
                        wl_output: wl_out,
                        xdg_output: xdg_out,
                        name: None,
                        logical_size: None,
                        mode_size: None,
                        scale: 1,
                        transform: WEnum::Value(wl_output::Transform::Normal),
                    });
                }
                _ => {}
            },
            wl_registry::Event::GlobalRemove { name } => {
                if let Some(pos) = state.outputs.iter().position(|o| o.global_name == name) {
                    let removed = state.outputs.remove(pos);
                    if let Some(xdg) = removed.xdg_output {
                        xdg.destroy();
                    }
                    if removed.wl_output.version() >= 3 {
                        removed.wl_output.release();
                    }
                    if state.mapped_output_global == Some(name) {
                        state.unmap_strips();
                        state.reconcile_surfaces(qh);
                    }
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for WaylandState {
    fn event(
        state: &mut Self,
        seat: &wl_seat::WlSeat,
        event: wl_seat::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Capabilities {
            capabilities: WEnum::Value(caps),
        } = event
        {
            let has_touch = caps.contains(wl_seat::Capability::Touch);
            if has_touch && state.touch.is_none() {
                state.touch = Some(seat.get_touch(qh, ()));
            } else if !has_touch {
                if let Some(touch) = state.touch.take() {
                    if touch.version() >= 3 {
                        touch.release();
                    }
                }
            }
        }
    }
}

impl Dispatch<wl_touch::WlTouch, ()> for WaylandState {
    fn event(
        state: &mut Self,
        _: &wl_touch::WlTouch,
        event: wl_touch::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_touch::Event::Down { surface, id, y, .. } => {
                if let Some(side) = state.side_for_surface(&surface) {
                    state.touch_slots.insert(id, side);
                    state.pending_events.push(TouchEvent::Down { side, id, y });
                }
            }
            wl_touch::Event::Motion { id, y, .. } => {
                if let Some(&side) = state.touch_slots.get(&id) {
                    state
                        .pending_events
                        .push(TouchEvent::Motion { side, id, y });
                }
            }
            wl_touch::Event::Up { id, .. } => {
                if let Some(side) = state.touch_slots.remove(&id) {
                    state.pending_events.push(TouchEvent::Up { side, id });
                }
            }
            wl_touch::Event::Cancel if !state.touch_slots.is_empty() => {
                state.touch_slots.clear();
                state.pending_events.push(TouchEvent::Cancel);
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_output::WlOutput, u32> for WaylandState {
    fn event(
        state: &mut Self,
        _: &wl_output::WlOutput,
        event: wl_output::Event,
        &global_name: &u32,
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        let Some(out) = state
            .outputs
            .iter_mut()
            .find(|o| o.global_name == global_name)
        else {
            return;
        };

        match event {
            wl_output::Event::Name { name } => {
                out.name = Some(name);
            }
            wl_output::Event::Geometry { transform, .. } => {
                out.transform = transform;
            }
            wl_output::Event::Mode {
                flags: WEnum::Value(flags),
                width,
                height,
                ..
            } => {
                if flags.contains(wl_output::Mode::Current) {
                    out.mode_size = Some((width, height));
                }
            }
            wl_output::Event::Scale { factor } => {
                out.scale = factor;
            }
            wl_output::Event::Done => {
                state.reconcile_surfaces(qh);
            }
            _ => {}
        }
    }
}

impl Dispatch<ZxdgOutputV1, u32> for WaylandState {
    fn event(
        state: &mut Self,
        _: &ZxdgOutputV1,
        event: zxdg_output_v1::Event,
        &global_name: &u32,
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        let Some(out) = state
            .outputs
            .iter_mut()
            .find(|o| o.global_name == global_name)
        else {
            return;
        };

        match event {
            zxdg_output_v1::Event::Name { name } => {
                if out.name.is_none() {
                    out.name = Some(name);
                }
            }
            zxdg_output_v1::Event::LogicalSize { width, height } => {
                out.logical_size = Some((width, height));
            }
            zxdg_output_v1::Event::Done => {
                state.reconcile_surfaces(qh);
            }
            _ => {}
        }
    }
}

impl Dispatch<ZwlrLayerSurfaceV1, Side> for WaylandState {
    fn event(
        state: &mut Self,
        _: &ZwlrLayerSurfaceV1,
        event: zwlr_layer_surface_v1::Event,
        &side: &Side,
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_layer_surface_v1::Event::Configure {
                serial,
                width,
                height,
            } => {
                state.configure_strip(side, serial, width, height, qh);
            }
            zwlr_layer_surface_v1::Event::Closed => {
                state.unmap_strips();
                state.reconcile_surfaces(qh);
            }
            _ => {}
        }
    }
}

// Objects that may emit informational compositor events we safely ignore:
impl Dispatch<wl_surface::WlSurface, ()> for WaylandState {
    fn event(
        _: &mut Self,
        _: &wl_surface::WlSurface,
        _: wl_surface::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_buffer::WlBuffer, ()> for WaylandState {
    fn event(
        _: &mut Self,
        _: &wl_buffer::WlBuffer,
        _: wl_buffer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_shm::WlShm, ()> for WaylandState {
    fn event(
        _: &mut Self,
        _: &wl_shm::WlShm,
        _: wl_shm::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_shm_pool::WlShmPool, ()> for WaylandState {
    fn event(
        _: &mut Self,
        _: &wl_shm_pool::WlShmPool,
        _: wl_shm_pool::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

// Purely eventless protocol objects:
delegate_noop!(WaylandState: ignore wl_compositor::WlCompositor);
delegate_noop!(WaylandState: ignore ZwlrLayerShellV1);
delegate_noop!(WaylandState: ignore WpSinglePixelBufferManagerV1);
delegate_noop!(WaylandState: ignore WpViewporter);
delegate_noop!(WaylandState: ignore WpViewport);
delegate_noop!(WaylandState: ignore ZxdgOutputManagerV1);
