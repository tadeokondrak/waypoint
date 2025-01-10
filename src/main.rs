#![allow(clippy::single_match, clippy::match_single_binding)]

mod generated {
    #![allow(unused)]
    include!(concat!(env!("OUT_DIR"), "/wayland.rs"));
}

extern crate waypoint_scfg as scfg;

mod config;
mod region;

use crate::{
    config::{specialize_bindings, Cmd, Config, Direction},
    region::Region,
};
use anyhow::{Context as _, Result};
use bytemuck::{Pod, Zeroable};
use calloop::{generic::Generic, LoopSignal};
use generated::{
    Event, Interface, Protocols, Request, WlBuffer, WlBufferEvent, WlBufferRequest, WlCallback,
    WlCallbackEvent, WlCompositor, WlCompositorRequest, WlDisplay, WlDisplayEvent,
    WlDisplayRequest, WlKeyboard, WlKeyboardEvent, WlOutput, WlOutputEvent, WlPointer,
    WlPointerEvent, WlRegistry, WlRegistryEvent, WlRegistryRequest, WlSeat, WlSeatEvent,
    WlSeatRequest, WlShm, WlShmEvent, WlShmPool, WlShmPoolRequest, WlShmRequest, WlSurface,
    WlSurfaceEvent, WlSurfaceRequest, WlTouchEvent, ZwlrLayerShellV1, ZwlrLayerShellV1Request,
    ZwlrLayerSurfaceV1, ZwlrLayerSurfaceV1Event, ZwlrLayerSurfaceV1Request,
    ZwlrVirtualPointerManagerV1, ZwlrVirtualPointerManagerV1Request, ZwlrVirtualPointerV1,
    ZwlrVirtualPointerV1Request, ZxdgOutputManagerV1, ZxdgOutputManagerV1Request, ZxdgOutputV1,
    ZxdgOutputV1Event,
};
use handy::typed::{TypedHandle, TypedHandleMap};
use memmap2::{MmapMut, MmapOptions};
use std::{
    collections::{HashMap, HashSet},
    io::Write,
    ops::RangeInclusive,
    os::fd::{AsFd, AsRawFd, BorrowedFd, IntoRawFd},
};
use tiny_skia::{Color, Paint, PathBuilder, Shader, Stroke, Transform};
use wayland::{Object as _, Protocols as _};
use xkbcommon::xkb;

type SeatId = TypedHandle<Seat>;
type OutputId = TypedHandle<Output>;
type BufferId = TypedHandle<Buffer>;

struct App {
    loop_signal: LoopSignal,
    globals: Globals,
    seats: TypedHandleMap<Seat>,
    outputs: TypedHandleMap<Output>,
    buffers: TypedHandleMap<Buffer>,
    config: Config,
    region: Region,
    region_history: Vec<Region>,
    global_bounds: Region,
}

#[derive(Default, Clone, Copy)]
#[repr(C)] // Note: implements Zeroable and Pod
struct ModIndices {
    shift: xkb::ModIndex,
    caps: xkb::ModIndex,
    ctrl: xkb::ModIndex,
    alt: xkb::ModIndex,
    num: xkb::ModIndex,
    mod3: xkb::ModIndex,
    logo: xkb::ModIndex,
    mod5: xkb::ModIndex,
}

struct Globals {
    wl_shm: WlShm,
    wl_compositor: WlCompositor,
    xdg_output: ZxdgOutputManagerV1,
    layer_shell: ZwlrLayerShellV1,
    virtual_pointer_manager: ZwlrVirtualPointerManagerV1,
}

struct Seat {
    wl_seat: WlSeat,
    virtual_pointer: ZwlrVirtualPointerV1,
    xkb: xkb::Context,
    xkb_state: Option<xkb::State>,
    keyboard: WlKeyboard,
    buttons_down: HashSet<u32>,
    mod_indices: ModIndices,
    specialized_bindings: HashMap<(xkb::ModMask, xkb::Keycode), Vec<Cmd>>,
}

#[derive(Default)]
struct Output {
    surface: Option<Surface>,
    wl_output: WlOutput,
    xdg_output: ZxdgOutputV1,
    state: DoubleBuffered<OutputState>,
}

#[derive(Default, Copy, Clone)]
struct OutputState {
    integer_scale: u32,
    logical_x: i32,
    logical_y: i32,
    logical_width: i32,
    logical_height: i32,
}

#[derive(Default)]
struct Surface {
    output: OutputId,
    wl_surface: WlSurface,
    layer_surface: ZwlrLayerSurfaceV1,
    width: u32,
    height: u32,
}

#[derive(Default)]
struct Buffer {
    pool: WlShmPool,
    wl_buffer: WlBuffer,
    mmap: Option<MmapMut>,
}

#[derive(Default)]
struct DoubleBuffered<T> {
    pending: T,
    current: Option<T>,
}

unsafe impl Zeroable for ModIndices {}
unsafe impl Pod for ModIndices {}

impl<T: Clone> DoubleBuffered<T> {
    fn commit(&mut self) {
        match self.current.as_mut() {
            Some(current) => current.clone_from(&self.pending),
            None => self.current = Some(self.pending.clone()),
        }
    }
}

impl Default for Seat {
    fn default() -> Seat {
        Seat {
            xkb: xkb::Context::new(xkb::CONTEXT_NO_FLAGS),
            wl_seat: Default::default(),
            virtual_pointer: Default::default(),
            xkb_state: Default::default(),
            keyboard: Default::default(),
            buttons_down: Default::default(),
            mod_indices: Default::default(),
            specialized_bindings: Default::default(),
        }
    }
}

impl Output {
    fn region(&self) -> Region {
        let current = self.state.current.as_ref().unwrap();
        Region {
            x: current.logical_x,
            y: current.logical_y,
            width: current.logical_width,
            height: current.logical_height,
        }
    }
}

fn handle_key_pressed(
    state: &mut App,
    time: u32,
    key: u32,
    seat_id: SeatId,
    conn: &mut Connection,
) {
    fn update(
        region: &mut Region,
        region_history: &mut Vec<Region>,
        global_bounds: Region,
        cut: fn(Region) -> Region,
    ) {
        region_history.push(*region);
        let new_region = cut(*region);
        if global_bounds.contains_region(&new_region) {
            *region = new_region;
        }
    }

    let seat = &mut state.seats[seat_id];

    let keycode = key + 8;
    let mod_mask = {
        let Some(xkb_state) = seat.xkb_state.as_mut() else {
            return;
        };
        let keymap = xkb_state.get_keymap();
        let mut mod_mask: xkb::ModMask = 0;
        for i in 0..keymap.num_mods() {
            let is_active = xkb_state.mod_index_is_active(i, xkb::STATE_MODS_EFFECTIVE);
            let _is_consumed = xkb_state.mod_index_is_consumed(key, i);
            let is_relevant = i != seat.mod_indices.caps && i != seat.mod_indices.num;
            if is_active && is_relevant {
                mod_mask |= 1 << i;
            }
        }
        mod_mask
    };

    let mut should_press = None;
    let mut should_release = None;
    let mut should_scroll = Vec::new();

    for cmd in seat
        .specialized_bindings
        .get(&(mod_mask, keycode))
        .map(Vec::as_slice)
        .unwrap_or_default()
    {
        match *cmd {
            Cmd::Quit => {
                state.loop_signal.stop();
            }
            Cmd::Undo => {
                if let Some(region) = state.region_history.pop() {
                    state.region = region;
                }
            }
            Cmd::Cut(dir) => update(
                &mut state.region,
                &mut state.region_history,
                state.global_bounds,
                match dir {
                    Direction::Up => Region::cut_up,
                    Direction::Down => Region::cut_down,
                    Direction::Left => Region::cut_left,
                    Direction::Right => Region::cut_right,
                },
            ),
            Cmd::Move(dir) => update(
                &mut state.region,
                &mut state.region_history,
                state.global_bounds,
                match dir {
                    Direction::Up => Region::move_up,
                    Direction::Down => Region::move_down,
                    Direction::Left => Region::move_left,
                    Direction::Right => Region::move_right,
                },
            ),
            Cmd::Click(btn) => {
                should_press = Some(btn.code());
                should_release = Some(btn.code());
                state.loop_signal.stop();
            }
            Cmd::Press(btn) => {
                should_press = Some(btn.code());
            }
            Cmd::Release(btn) => {
                should_release = Some(btn.code());
            }
            Cmd::Scroll(axis, amount) => {
                should_scroll.push((axis, amount));
            }
        }
    }

    for output in state.outputs.iter() {
        let surface = output.surface.as_ref().unwrap();
        draw(
            &state.globals,
            &mut state.buffers,
            conn,
            output.state.current.as_ref().unwrap().integer_scale,
            surface,
            Region {
                x: state.region.x - output.state.current.unwrap().logical_x,
                y: state.region.y - output.state.current.unwrap().logical_y,
                ..state.region
            },
        )
        .unwrap();
    }

    if !seat.virtual_pointer.is_null() {
        conn.send(ZwlrVirtualPointerV1Request::MotionAbsolute {
            zwlr_virtual_pointer_v1: seat.virtual_pointer,
            time,
            x: state.region.center().x as u32,
            y: state.region.center().y as u32,
            x_extent: state.global_bounds.width as u32,
            y_extent: state.global_bounds.height as u32,
        });
        conn.send(ZwlrVirtualPointerV1Request::Frame {
            zwlr_virtual_pointer_v1: seat.virtual_pointer,
        });

        for (axis, amount) in should_scroll {
            conn.send(ZwlrVirtualPointerV1Request::Axis {
                zwlr_virtual_pointer_v1: seat.virtual_pointer,
                time,
                axis,
                value: wayland::Fixed::from(amount as f32),
            });
            conn.send(ZwlrVirtualPointerV1Request::Frame {
                zwlr_virtual_pointer_v1: seat.virtual_pointer,
            });
        }

        if let Some(button) = should_press {
            if seat.buttons_down.insert(button) {
                conn.send(ZwlrVirtualPointerV1Request::Button {
                    zwlr_virtual_pointer_v1: seat.virtual_pointer,
                    time,
                    button,
                    state: WlPointer::BUTTON_STATE_PRESSED,
                });
                conn.send(ZwlrVirtualPointerV1Request::Frame {
                    zwlr_virtual_pointer_v1: seat.virtual_pointer,
                });
            }
        }

        if let Some(button) = should_release {
            if seat.buttons_down.remove(&button) {
                conn.send(ZwlrVirtualPointerV1Request::Button {
                    zwlr_virtual_pointer_v1: seat.virtual_pointer,
                    time,
                    button,
                    state: WlPointer::BUTTON_STATE_RELEASED,
                });
                conn.send(ZwlrVirtualPointerV1Request::Frame {
                    zwlr_virtual_pointer_v1: seat.virtual_pointer,
                });
            }
        }
    }
}

fn draw(
    globals: &Globals,
    buffers: &mut TypedHandleMap<Buffer>,
    conn: &mut Connection,
    scale: u32,
    surface: &Surface,
    region: Region,
) -> Result<()> {
    let buffer_data = make_buffer(
        globals,
        buffers,
        conn,
        i32::try_from(surface.width * scale).unwrap(),
        i32::try_from(surface.height * scale).unwrap(),
        i32::try_from(surface.width * scale * 4).unwrap(),
        WlShm::FORMAT_ABGR8888,
    )?;
    let buffer = &mut buffers[buffer_data];
    let mut pixmap = tiny_skia::PixmapMut::from_bytes(
        buffer.mmap.as_deref_mut().unwrap(),
        surface.width * scale,
        surface.height * scale,
    )
    .expect("PixmapMut creation failed");
    let border_color = Color::WHITE;
    let cross_color = {
        let mut color = Color::WHITE;
        color.apply_opacity(0.25);
        color
    };
    let border_thickness = 1.0;
    let cross_thickness = 2.0;
    draw_inner(
        region,
        scale,
        &mut pixmap,
        border_color,
        border_thickness,
        cross_color,
        cross_thickness,
    );
    conn.send(WlSurfaceRequest::SetBufferScale {
        wl_surface: surface.wl_surface,
        scale: i32::try_from(scale).unwrap(),
    });
    conn.send(WlSurfaceRequest::Attach {
        wl_surface: surface.wl_surface,
        buffer: buffer.wl_buffer,
        x: 0,
        y: 0,
    });
    conn.send(WlSurfaceRequest::DamageBuffer {
        wl_surface: surface.wl_surface,
        x: 0,
        y: 0,
        width: i32::MAX,
        height: i32::MAX,
    });
    conn.send(WlSurfaceRequest::Commit {
        wl_surface: surface.wl_surface,
    });
    Ok(())
}

fn draw_inner(
    region: Region,
    scale: u32,
    pixmap: &mut tiny_skia::PixmapMut<'_>,
    border_color: Color,
    border_thickness: f32,
    cross_color: Color,
    cross_thickness: f32,
) {
    let region = region.scale(scale);
    let region_x = region.x as f32;
    let region_y = region.y as f32;
    let region_width = region.width as f32;
    let region_height = region.height as f32;

    let border_paint = Paint {
        shader: Shader::SolidColor(border_color),
        ..Default::default()
    };

    let border_stroke = Stroke {
        width: border_thickness,
        ..Default::default()
    };

    let cross_paint = Paint {
        shader: Shader::SolidColor(cross_color),
        ..Default::default()
    };

    let cross_stroke = Stroke {
        width: cross_thickness,
        ..Default::default()
    };

    let mut path = PathBuilder::new();
    path.move_to(region_x, region_y);
    path.line_to(region_x + region_width, region_y);
    path.line_to(region_x + region_width, region_y + region_height);
    path.line_to(region_x, region_y + region_height);
    path.close();
    let path = path.finish().expect("invalid path created");

    _ = pixmap.stroke_path(
        &path,
        &border_paint,
        &border_stroke,
        Transform::default(),
        None,
    );

    let mut path = path.clear();
    path.move_to(region_x, region_y + region_height / 2.0);
    path.line_to(region_x + region_width, region_y + region_height / 2.0);
    path.close();
    path.move_to(region_x + region_width / 2.0, region_y);
    path.line_to(region_x + region_width / 2.0, region_y + region_height);
    let path = path.finish().expect("invalid path created");

    _ = pixmap.stroke_path(
        &path,
        &cross_paint,
        &cross_stroke,
        Transform::default(),
        None,
    );
}

fn make_buffer(
    globals: &Globals,
    buffers: &mut TypedHandleMap<Buffer>,
    conn: &mut Connection,
    width: i32,
    height: i32,
    stride: i32,
    format: u32,
) -> Result<BufferId> {
    let buffer_id = buffers.insert(Buffer::default());
    let this = &mut buffers[buffer_id];
    let memfd = memfd::MemfdOptions::new().create("waypoint-buffer")?;
    let len_i32 = stride.checked_mul(height).expect("buffer too big");
    let len_usize = usize::try_from(len_i32).expect("buffer too big");
    memfd.as_file().write_all(&vec![0u8; len_i32 as usize])?;
    let borrowed_memfd = unsafe { BorrowedFd::borrow_raw(memfd.as_raw_fd()) };
    let wl_shm_pool = conn.send_constructor(0, |id| WlShmRequest::CreatePool {
        wl_shm: globals.wl_shm,
        id,
        fd: borrowed_memfd.as_fd().try_clone_to_owned().unwrap(),
        size: len_i32,
    });
    let wl_buffer =
        conn.send_constructor(buffer_id.into_raw(), |id| WlShmPoolRequest::CreateBuffer {
            wl_shm_pool,
            id,
            offset: 0,
            width,
            height,
            stride,
            format,
        });
    let mmap = unsafe { MmapOptions::new().len(len_usize).map_mut(memfd.as_file())? };
    this.pool = wl_shm_pool;
    this.wl_buffer = wl_buffer;
    this.mmap = Some(mmap);
    Ok(buffer_id)
}

#[derive(Debug)]
struct IdAllocator<I: std::fmt::Debug> {
    next: u32,
    free: Vec<u32>,
    data: Vec<I>,
}

impl<I: std::fmt::Debug> IdAllocator<I> {
    fn new(first_id: u32) -> IdAllocator<I> {
        Self {
            next: first_id,
            free: Vec::new(),
            data: Vec::new(),
        }
    }

    pub fn allocate(&mut self, data: I) -> u32 {
        match self.free.pop() {
            Some(id) => {
                self.data[usize::try_from(id).unwrap() - 1] = data;
                id
            }
            None => {
                let id = self.next;
                self.next += 1;
                self.data.push(data);
                id
            }
        }
    }

    pub fn release(&mut self, id: u32) {
        self.free.push(id);
    }

    pub fn data_for(&self, id: u32) -> &I {
        &self.data[usize::try_from(id).unwrap() - 1]
    }
}

#[derive(Debug)]
struct Connection {
    wire: wayland::Connection,
    ids: IdAllocator<ObjectData>,
    sync_callback: WlCallback,
    sync_done: bool,
}

#[derive(Debug)]
struct ObjectData {
    interface: Interface,
    data: u64,
}

impl Connection {
    fn send<'a>(&mut self, request: impl Into<Request<'a>>) {
        let request = request.into();
        // eprintln!("-> {request:?}");
        Protocols::marshal_request(request, &mut self.wire)
    }

    fn send_constructor<'a, O, F, IR>(&mut self, data: u64, f: F) -> O
    where
        O: wayland::Object<Protocols>,
        F: Fn(O) -> IR,
        IR: Into<Request<'a>>,
    {
        let obj = self.create(data);
        let request = f(obj).into();
        // eprintln!("-> {request:?}");
        request.marshal(&mut self.wire);
        obj
    }

    fn create<O: wayland::Object<Protocols>>(&mut self, data: u64) -> O {
        let id = self.ids.allocate(ObjectData {
            interface: O::INTERFACE,
            data,
        });
        O::new(id)
    }

    fn handle_events(&mut self, mut handler: impl FnMut(&mut Connection, Event)) {
        while let Some(event) = self
            .wire
            .read_message(|msg| Event::unmarshal(self.ids.data_for(msg.object()).interface, msg))
        {
            // eprintln!("<- {event:?}");
            match event {
                Event::WlDisplay(WlDisplayEvent::DeleteId { wl_display: _, id }) => {
                    self.ids.release(id);
                }
                Event::WlCallback(WlCallbackEvent::Done {
                    wl_callback,
                    callback_data: _,
                }) if wl_callback == self.sync_callback => {
                    self.sync_done = true;
                }
                _ => handler(self, event),
            }
        }
    }

    fn roundtrip(&mut self, mut handler: impl FnMut(&mut Connection, Event)) {
        self.sync_done = false;
        self.sync_callback = self.send_constructor(0, |callback| WlDisplayRequest::Sync {
            wl_display: WlDisplay(1),
            callback,
        });
        while !self.sync_done {
            self.wire.flush_blocking().unwrap();
            self.wire.read_blocking().unwrap();
            self.handle_events(&mut handler);
        }
    }
}

impl AsFd for Connection {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.wire.as_fd()
    }
}

fn main() -> Result<()> {
    let wayland_fd = wayland::client_socket_from_env()?.context("no wayland display available")?;
    let wire_conn = wayland::Connection::new(wayland_fd);
    let mut conn = Connection {
        wire: wire_conn,
        ids: IdAllocator::new(1),
        sync_callback: Default::default(),
        sync_done: false,
    };

    let wl_display: WlDisplay = conn.create(0);
    let wl_registry = conn.send_constructor(0, |registry| WlDisplayRequest::GetRegistry {
        wl_display,
        registry,
    });
    let mut global_list: HashMap<String, Vec<(u32, u32)>> = HashMap::new();
    conn.roundtrip(|_conn, event| match event {
        Event::WlRegistry(WlRegistryEvent::Global {
            wl_registry: r,
            name,
            interface,
            version,
        }) if r == wl_registry => {
            global_list
                .entry(interface.into_owned())
                .or_default()
                .push((name, version));
        }
        _ => {
            eprintln!("warning: unexpected event: {event:?}")
        }
    });

    fn bind_global<O: wayland::Object<Protocols>>(
        conn: &mut Connection,
        registry: WlRegistry,
        globals: &HashMap<String, Vec<(u32, u32)>>,
        version: RangeInclusive<u32>,
    ) -> Option<O> {
        for &(name, sversion) in globals.get(O::INTERFACE.name())? {
            if &sversion >= version.start() {
                return Some(conn.send_constructor(0, |new_id: O| {
                    Request::WlRegistry(WlRegistryRequest::Bind {
                        wl_registry: registry,
                        name,
                        interface: O::INTERFACE.name().into(),
                        version: sversion.min(*version.end()),
                        id: new_id.id(),
                    })
                }));
            }
        }
        None
    }

    let globals = Globals {
        wl_shm: bind_global(&mut conn, wl_registry, &global_list, 1..=1)
            .context("compositor doesn't support wl_shm")?,
        wl_compositor: bind_global(&mut conn, wl_registry, &global_list, 4..=4)
            .context("compositor doesn't support wl_compositor")?,
        xdg_output: bind_global(&mut conn, wl_registry, &global_list, 3..=3)
            .context("compositor doesn't support xdg_output_manager_v1")?,
        layer_shell: bind_global(&mut conn, wl_registry, &global_list, 1..=1)
            .context("compositor doesn't support zwlr_layer_shell_v1")?,
        virtual_pointer_manager: bind_global(&mut conn, wl_registry, &global_list, 1..=1)
            .unwrap_or_default(),
    };

    let mut event_loop: calloop::EventLoop<'_, App> = calloop::EventLoop::try_new().unwrap();
    let mut app = App {
        loop_signal: event_loop.get_signal(),
        globals,
        seats: TypedHandleMap::new(),
        outputs: TypedHandleMap::new(),
        buffers: TypedHandleMap::new(),
        config: Config::load()?,
        region: Region::default(),
        region_history: Vec::new(),
        global_bounds: Region::default(),
    };

    if let Some(seat_list) = global_list.get(Interface::WlSeat.name()) {
        for &(name, sversion) in seat_list {
            let seat_id = app.seats.insert(Seat::default());
            let wl_seat = conn.send_constructor(seat_id.into_raw(), |WlSeat(id)| {
                Request::WlRegistry(WlRegistryRequest::Bind {
                    wl_registry,
                    name,
                    interface: Interface::WlSeat.name().into(),
                    version: sversion.min(1),
                    id,
                })
            });
            let seat = &mut app.seats[seat_id];
            if !app.globals.virtual_pointer_manager.is_null() {
                let virtual_pointer = conn.send_constructor(0, |id| {
                    Request::ZwlrVirtualPointerManagerV1(
                        ZwlrVirtualPointerManagerV1Request::CreateVirtualPointer {
                            zwlr_virtual_pointer_manager_v1: app.globals.virtual_pointer_manager,
                            seat: wl_seat,
                            id,
                        },
                    )
                });
                seat.virtual_pointer = virtual_pointer;
            }
            seat.wl_seat = wl_seat;
        }
    }
    if let Some(output_list) = global_list.get(Interface::WlOutput.name()) {
        for &(name, sversion) in output_list {
            assert!(sversion >= 2);
            let output_id = app.outputs.insert(Output::default());
            let output = &mut app.outputs[output_id];
            let wl_output = conn.send_constructor(output_id.into_raw(), |WlOutput(id)| {
                Request::WlRegistry(WlRegistryRequest::Bind {
                    wl_registry,
                    name,
                    interface: Interface::WlOutput.name().into(),
                    version: sversion.min(2),
                    id,
                })
            });
            let xdg_output = conn.send_constructor(output_id.into_raw(), |id| {
                Request::ZxdgOutputManagerV1(ZxdgOutputManagerV1Request::GetXdgOutput {
                    zxdg_output_manager_v1: app.globals.xdg_output,
                    id,
                    output: wl_output,
                })
            });
            output.wl_output = wl_output;
            output.xdg_output = xdg_output;
        }
    }
    conn.roundtrip(|conn, event| {
        app.handle_event(conn, event);
    });

    for output in app.outputs.iter() {
        app.global_bounds = app.global_bounds.union(&output.region());
    }

    app.region = app.global_bounds;

    for (output_id, output) in app.outputs.iter_mut_with_handles() {
        output.surface = Some(Surface::default());
        let surface = output.surface.as_mut().unwrap();

        let wl_surface = conn.send_constructor(0, |id| WlCompositorRequest::CreateSurface {
            wl_compositor: app.globals.wl_compositor,
            id,
        });
        let layer_surface = conn.send_constructor(output_id.into_raw(), |id| {
            ZwlrLayerShellV1Request::GetLayerSurface {
                zwlr_layer_shell_v1: app.globals.layer_shell,
                id,
                surface: wl_surface,
                output: output.wl_output,
                layer: ZwlrLayerShellV1::LAYER_OVERLAY,
                namespace: "waypoint".into(),
            }
        });
        conn.send(ZwlrLayerSurfaceV1Request::SetSize {
            zwlr_layer_surface_v1: layer_surface,
            width: 0,
            height: 0,
        });
        conn.send(ZwlrLayerSurfaceV1Request::SetAnchor {
            zwlr_layer_surface_v1: layer_surface,
            anchor: ZwlrLayerSurfaceV1::ANCHOR_TOP
                | ZwlrLayerSurfaceV1::ANCHOR_BOTTOM
                | ZwlrLayerSurfaceV1::ANCHOR_LEFT
                | ZwlrLayerSurfaceV1::ANCHOR_RIGHT,
        });
        conn.send(ZwlrLayerSurfaceV1Request::SetExclusiveZone {
            zwlr_layer_surface_v1: layer_surface,
            zone: -1,
        });
        conn.send(ZwlrLayerSurfaceV1Request::SetKeyboardInteractivity {
            zwlr_layer_surface_v1: layer_surface,
            keyboard_interactivity: ZwlrLayerSurfaceV1::KEYBOARD_INTERACTIVITY_EXCLUSIVE,
        });
        let region = conn.send_constructor(0, |id| WlCompositorRequest::CreateRegion {
            wl_compositor: app.globals.wl_compositor,
            id,
        });
        conn.send(WlSurfaceRequest::SetInputRegion { wl_surface, region });
        conn.send(WlSurfaceRequest::Commit { wl_surface });

        surface.output = output_id;
        surface.wl_surface = wl_surface;
        surface.layer_surface = layer_surface;
    }

    for seat in app.seats.iter() {
        conn.send(ZwlrVirtualPointerV1Request::MotionAbsolute {
            zwlr_virtual_pointer_v1: seat.virtual_pointer,
            time: 0,
            x: app.region.center().x as u32,
            y: app.region.center().y as u32,
            x_extent: app.global_bounds.width as u32,
            y_extent: app.global_bounds.height as u32,
        });
        conn.send(ZwlrVirtualPointerV1Request::Frame {
            zwlr_virtual_pointer_v1: seat.virtual_pointer,
        });
    }

    conn.wire.flush_blocking()?;

    let conn_source = Generic::new(conn, calloop::Interest::READ, calloop::Mode::Level);
    let dispatcher = calloop::Dispatcher::new(
        conn_source,
        |_, conn: &mut calloop::generic::NoIoDrop<Connection>, app: &mut App| {
            let conn = unsafe { conn.get_mut() };
            conn.wire.read_nonblocking().unwrap();
            // unsound: we can drop the connection inside the handler and leave a dangling fd
            conn.handle_events(|conn, event| app.handle_event(conn, event));
            Ok(calloop::PostAction::Continue)
        },
    );

    event_loop
        .handle()
        .register_dispatcher(dispatcher.clone())?;

    event_loop.run(None, &mut app, |_| {
        let mut conn = dispatcher.as_source_mut();
        // safety: we don't invalidate the fd in this block
        let conn = unsafe { conn.get_mut() };
        conn.wire.flush_blocking().unwrap();
    })?;

    {
        let mut conn = dispatcher.as_source_mut();
        // safety: we don't invalidate the fd in this block
        let conn = unsafe { conn.get_mut() };

        for seat in app.seats.iter() {
            for &button in &seat.buttons_down {
                conn.send(ZwlrVirtualPointerV1Request::Button {
                    zwlr_virtual_pointer_v1: seat.virtual_pointer,
                    time: 0,
                    button,
                    state: WlPointer::BUTTON_STATE_RELEASED,
                });
                conn.send(ZwlrVirtualPointerV1Request::Frame {
                    zwlr_virtual_pointer_v1: seat.virtual_pointer,
                });
            }
        }

        conn.wire.flush_blocking()?;
    }

    Ok(())
}

impl App {
    fn handle_event(&mut self, conn: &mut Connection, event: Event) {
        match event {
            Event::WlSeat(event) => match event {
                WlSeatEvent::Capabilities {
                    wl_seat,
                    capabilities,
                } => {
                    let seat_id = SeatId::from_raw(conn.ids.data_for(wl_seat.id()).data);
                    let seat = &mut self.seats[seat_id];
                    if capabilities & WlSeat::CAPABILITY_KEYBOARD != 0 {
                        seat.keyboard = conn.send_constructor(seat_id.into_raw(), |id| {
                            WlSeatRequest::GetKeyboard { wl_seat, id }
                        });
                    }
                }
            },
            Event::WlKeyboard(event) => match event {
                WlKeyboardEvent::Keymap {
                    wl_keyboard,
                    format,
                    fd,
                    size,
                } => {
                    if format == WlKeyboard::KEYMAP_FORMAT_XKB_V1 {
                        let seat_id = SeatId::from_raw(conn.ids.data_for(wl_keyboard.id()).data);
                        let seat = &mut self.seats[seat_id];
                        let keymap = unsafe {
                            xkb::Keymap::new_from_fd(
                                &seat.xkb,
                                fd.into_raw_fd(),
                                size as usize,
                                xkb::KEYMAP_FORMAT_TEXT_V1,
                                xkb::COMPILE_NO_FLAGS,
                            )
                        }
                        .ok()
                        .flatten();
                        if let Some(keymap) = keymap.as_ref() {
                            seat.xkb_state = Some(xkb::State::new(keymap));
                            (seat.mod_indices, seat.specialized_bindings) =
                                specialize_bindings(keymap, &self.config);
                        }
                    }
                }
                WlKeyboardEvent::Enter { .. } => {}
                WlKeyboardEvent::Leave { .. } => {}
                WlKeyboardEvent::Key {
                    wl_keyboard,
                    serial: _,
                    time,
                    key,
                    state,
                } => {
                    let seat_id = SeatId::from_raw(conn.ids.data_for(wl_keyboard.id()).data);
                    if state == WlKeyboard::KEY_STATE_PRESSED {
                        handle_key_pressed(self, time, key, seat_id, conn);
                    }
                }
                WlKeyboardEvent::Modifiers {
                    wl_keyboard,
                    serial: _,
                    mods_depressed,
                    mods_latched,
                    mods_locked,
                    group,
                } => {
                    let seat_id = SeatId::from_raw(conn.ids.data_for(wl_keyboard.id()).data);
                    let seat = &mut self.seats[seat_id];
                    let state = seat.xkb_state.as_mut().unwrap();
                    state.update_mask(mods_depressed, mods_latched, mods_locked, 0, 0, group);
                }
            },
            Event::WlOutput(event) => match event {
                WlOutputEvent::Geometry { .. } => {}
                WlOutputEvent::Mode { .. } => {}
                WlOutputEvent::Done { wl_output } => {
                    let output_id = OutputId::from_raw(conn.ids.data_for(wl_output.id()).data);
                    let output = &mut self.outputs[output_id];
                    output.state.commit();
                }
                WlOutputEvent::Scale { wl_output, factor } => {
                    let output_id = OutputId::from_raw(conn.ids.data_for(wl_output.id()).data);
                    let output = &mut self.outputs[output_id];
                    output.state.pending.integer_scale =
                        u32::try_from(factor).expect("negative scale factor");
                }
            },
            Event::ZxdgOutputV1(event) => match event {
                ZxdgOutputV1Event::LogicalPosition {
                    zxdg_output_v1,
                    x,
                    y,
                } => {
                    let output_id = OutputId::from_raw(conn.ids.data_for(zxdg_output_v1.id()).data);
                    let output = &mut self.outputs[output_id];
                    output.state.pending.logical_x = x;
                    output.state.pending.logical_y = y;
                }
                ZxdgOutputV1Event::LogicalSize {
                    zxdg_output_v1,
                    width,
                    height,
                } => {
                    let output_id = OutputId::from_raw(conn.ids.data_for(zxdg_output_v1.id()).data);
                    let output = &mut self.outputs[output_id];
                    output.state.pending.logical_width = width;
                    output.state.pending.logical_height = height;
                }
                ZxdgOutputV1Event::Done { .. } => {}
                ZxdgOutputV1Event::Name { .. } => {}
                ZxdgOutputV1Event::Description { .. } => {}
            },

            Event::WlSurface(event) => match event {
                WlSurfaceEvent::Enter { .. } => {}
                WlSurfaceEvent::Leave { .. } => {}
            },
            Event::ZwlrLayerSurfaceV1(event) => match event {
                ZwlrLayerSurfaceV1Event::Configure {
                    zwlr_layer_surface_v1,
                    serial,
                    width,
                    height,
                } => {
                    let output_id =
                        OutputId::from_raw(conn.ids.data_for(zwlr_layer_surface_v1.id()).data);
                    let output = &mut self.outputs[output_id];
                    let surface = output.surface.as_mut().unwrap();
                    conn.send(ZwlrLayerSurfaceV1Request::AckConfigure {
                        zwlr_layer_surface_v1,
                        serial,
                    });
                    conn.send(ZwlrLayerSurfaceV1Request::SetSize {
                        zwlr_layer_surface_v1,
                        width,
                        height,
                    });
                    surface.width = width;
                    surface.height = height;
                    draw(
                        &self.globals,
                        &mut self.buffers,
                        conn,
                        output.state.current.as_ref().unwrap().integer_scale,
                        surface,
                        Region {
                            x: self.region.x - output.state.current.unwrap().logical_x,
                            y: self.region.y - output.state.current.unwrap().logical_y,
                            ..self.region
                        },
                    )
                    .unwrap();
                }
                ZwlrLayerSurfaceV1Event::Closed {
                    zwlr_layer_surface_v1,
                } => {
                    let output_id =
                        OutputId::from_raw(conn.ids.data_for(zwlr_layer_surface_v1.id()).data);
                    let output = &mut self.outputs[output_id];
                    output.surface = None;
                }
            },
            Event::WlBuffer(event) => match event {
                WlBufferEvent::Release { wl_buffer } => {
                    let buffer_id = BufferId::from_raw(conn.ids.data_for(wl_buffer.id()).data);
                    let buffer = &mut self.buffers[buffer_id];
                    conn.send(WlShmPoolRequest::Destroy {
                        wl_shm_pool: buffer.pool,
                    });
                    conn.send(WlBufferRequest::Destroy { wl_buffer });
                    self.buffers.remove(buffer_id);
                }
            },
            Event::WlShm(event) => match event {
                WlShmEvent::Format { .. } => {}
            },
            Event::WlCallback(event) => match event {
                WlCallbackEvent::Done { .. } => {}
            },
            Event::WlDisplay(event) => match event {
                WlDisplayEvent::Error { .. } => {}
                WlDisplayEvent::DeleteId { .. } => {}
            },
            Event::WlPointer(event) => match event {
                WlPointerEvent::Enter { .. } => {}
                WlPointerEvent::Leave { .. } => {}
                WlPointerEvent::Motion { .. } => {}
                WlPointerEvent::Button { .. } => {}
                WlPointerEvent::Axis { .. } => {}
            },
            Event::WlRegistry(event) => match event {
                WlRegistryEvent::Global { .. } => {}
                WlRegistryEvent::GlobalRemove { .. } => {}
            },
            Event::WlTouch(event) => match event {
                WlTouchEvent::Down { .. } => {}
                WlTouchEvent::Up { .. } => {}
                WlTouchEvent::Motion { .. } => {}
                WlTouchEvent::Frame { .. } => {}
                WlTouchEvent::Cancel { .. } => {}
            },
        }
    }
}
