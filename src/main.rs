#![allow(clippy::single_match, clippy::match_single_binding)]

mod wl_gen {
    #![allow(unused)]
    include!(concat!(env!("OUT_DIR"), "/wayland.rs"));
}

mod ei_gen {
    #![allow(unused)]
    include!(concat!(env!("OUT_DIR"), "/ei.rs"));
}

extern crate waypoint_scfg as scfg;

mod config;
mod region;

use crate::{
    config::{specialize_bindings, Cmd, Config, Direction},
    region::Region,
};
use anyhow::{Context as _, Result};
use ei::Object as _;
use ei_gen::{
    EiButton, EiButtonEvent, EiButtonRequest, EiCallbackEvent, EiConnectionEvent, EiDevice,
    EiDeviceEvent, EiDeviceRequest, EiHandshake, EiHandshakeEvent, EiHandshakeRequest,
    EiPingpongRequest, EiPointerAbsolute, EiPointerAbsoluteEvent, EiPointerAbsoluteRequest,
    EiScroll, EiScrollEvent, EiScrollRequest, EiSeatEvent, EiSeatRequest,
    EI_BUTTON_BUTTON_STATE_PRESS, EI_BUTTON_BUTTON_STATE_RELEASED,
    EI_HANDSHAKE_CONTEXT_TYPE_SENDER,
};
use handy::typed::{TypedHandle, TypedHandleMap};
use memmap2::{MmapMut, MmapOptions};
use rustix::event::{PollFd, PollFlags};
use std::{
    collections::{HashMap, HashSet},
    io::Write,
    ops::RangeInclusive,
    os::fd::{AsFd, AsRawFd, BorrowedFd},
    time::{Duration, Instant},
};
use tiny_skia::{Color, Paint, PathBuilder, Shader, Stroke, Transform};
use wayland::Object as _;
use wl_gen::{
    Event, Request, WlBuffer, WlBufferEvent, WlBufferRequest, WlCallback, WlCallbackEvent,
    WlCompositor, WlCompositorRequest, WlDisplay, WlDisplayEvent, WlDisplayRequest, WlKeyboard,
    WlKeyboardEvent, WlOutput, WlOutputEvent, WlPointerEvent, WlRegionRequest, WlRegistry,
    WlRegistryEvent, WlRegistryRequest, WlSeat, WlSeatEvent, WlSeatRequest, WlShm, WlShmEvent,
    WlShmPool, WlShmPoolRequest, WlShmRequest, WlSurface, WlSurfaceEvent, WlSurfaceRequest,
    WlTouchEvent, WpSinglePixelBufferManagerV1, WpSinglePixelBufferManagerV1Request,
    ZwlrLayerShellV1, ZwlrLayerShellV1Request, ZwlrLayerSurfaceV1, ZwlrLayerSurfaceV1Event,
    ZwlrLayerSurfaceV1Request, ZwlrVirtualPointerManagerV1, ZwlrVirtualPointerManagerV1Request,
    ZwlrVirtualPointerV1, ZwlrVirtualPointerV1Request, ZxdgOutputManagerV1,
    ZxdgOutputManagerV1Request, ZxdgOutputV1, ZxdgOutputV1Event, WL_KEYBOARD_KEYMAP_FORMAT_XKB_V1,
    WL_KEYBOARD_KEY_STATE_PRESSED, WL_KEYBOARD_KEY_STATE_RELEASED,
    WL_POINTER_AXIS_HORIZONTAL_SCROLL, WL_POINTER_AXIS_SOURCE_WHEEL,
    WL_POINTER_AXIS_VERTICAL_SCROLL, WL_POINTER_BUTTON_STATE_PRESSED,
    WL_POINTER_BUTTON_STATE_RELEASED, WL_SEAT_CAPABILITY_KEYBOARD, WL_SHM_FORMAT_ABGR8888,
    ZWLR_LAYER_SHELL_V1_LAYER_OVERLAY, ZWLR_LAYER_SURFACE_V1_ANCHOR_BOTTOM,
    ZWLR_LAYER_SURFACE_V1_ANCHOR_LEFT, ZWLR_LAYER_SURFACE_V1_ANCHOR_RIGHT,
    ZWLR_LAYER_SURFACE_V1_ANCHOR_TOP, ZWLR_LAYER_SURFACE_V1_KEYBOARD_INTERACTIVITY_EXCLUSIVE,
    ZWLR_LAYER_SURFACE_V1_KEYBOARD_INTERACTIVITY_NONE,
};

type SeatId = TypedHandle<Seat>;
type OutputId = TypedHandle<Output>;
type BufferId = TypedHandle<Buffer>;

struct App {
    quit: bool,
    globals: Globals,
    seats: TypedHandleMap<Seat>,
    outputs: TypedHandleMap<Output>,
    buffers: TypedHandleMap<Buffer>,
    config: Config,
    region: Region,
    region_history: Vec<Region>,
    global_bounds: Option<Region>,
    ei_state: EiState,
    input_surface: Option<Surface>,
    default_region: Option<Region>,
}

#[derive(Default)]
struct EiState {
    sequence: u32,
    last_serial: u32,
    seat_capabilities: HashMap<u64, u64>,
    devices: HashMap<u64, EiDeviceInterfaces>,
}

#[derive(Default)]
struct EiDeviceInterfaces {
    device: EiDevice,
    pointer_absolute: EiPointerAbsolute,
    button: EiButton,
    scroll: EiScroll,
}

struct Globals {
    wl_shm: WlShm,
    wl_compositor: WlCompositor,
    xdg_output: ZxdgOutputManagerV1,
    layer_shell: ZwlrLayerShellV1,
    single_pixel_buffer: WpSinglePixelBufferManagerV1,
    virtual_pointer_manager: ZwlrVirtualPointerManagerV1,
}

#[derive(Default)]
struct Seat {
    wl_seat: WlSeat,
    virtual_pointer: ZwlrVirtualPointerV1,
    xkb: kbvm::xkb::Context,
    lookup_table: Option<kbvm::lookup::LookupTable>,
    group: kbvm::GroupIndex,
    mods: kbvm::ModifierMask,
    keyboard: WlKeyboard,
    buttons_down: HashSet<u32>,
    specialized_bindings: HashMap<(kbvm::ModifierMask, kbvm::Keysym), Vec<Cmd>>,
    repeat_period: Duration,
    repeat_delay: Duration,
    key_repeat: Option<(Instant, kbvm::Keycode)>,
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
    wl_buffer: WlBuffer,
    pool: Option<WlShmPool>,
    mmap: Option<MmapMut>,
}

#[derive(Default)]
struct DoubleBuffered<T> {
    pending: T,
    current: Option<T>,
}

impl<T: Clone> DoubleBuffered<T> {
    fn commit(&mut self) {
        match self.current.as_mut() {
            Some(current) => current.clone_from(&self.pending),
            None => self.current = Some(self.pending.clone()),
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
    keycode: kbvm::Keycode,
    seat_id: SeatId,
    conn: &mut WaylandConnection,
    ei_conn: Option<&mut LibeiConnection>,
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

    let lookup =
        seat.lookup_table
            .as_ref()
            .unwrap()
            .lookup(seat.group, kbvm::ModifierMask::default(), keycode);
    let keysym = lookup.into_iter().next().unwrap().keysym();

    let mut should_press = None;
    let mut should_release = None;
    let mut should_scroll = Vec::new();

    for cmd in seat
        .specialized_bindings
        .get(&(seat.mods, keysym))
        .map(Vec::as_slice)
        .unwrap_or_default()
    {
        match *cmd {
            Cmd::Quit => {
                state.quit = true;
            }
            Cmd::Undo => {
                if let Some(region) = state.region_history.pop() {
                    state.region = region;
                }
            }
            Cmd::Cut(dir) => update(
                &mut state.region,
                &mut state.region_history,
                state.global_bounds.unwrap_or_default(),
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
                state.global_bounds.unwrap_or_default(),
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
                state.quit = true;
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

    if !seat.virtual_pointer.is_null() {
        conn.send(ZwlrVirtualPointerV1Request::MotionAbsolute {
            zwlr_virtual_pointer_v1: seat.virtual_pointer,
            time,
            x: state.region.center().x as u32,
            y: state.region.center().y as u32,
            x_extent: state.global_bounds.unwrap_or_default().width as u32,
            y_extent: state.global_bounds.unwrap_or_default().height as u32,
        });
        conn.send(ZwlrVirtualPointerV1Request::Frame {
            zwlr_virtual_pointer_v1: seat.virtual_pointer,
        });

        for (axis, amount) in should_scroll {
            conn.send(ZwlrVirtualPointerV1Request::Axis {
                zwlr_virtual_pointer_v1: seat.virtual_pointer,
                time,
                axis,
                value: wayland::Fixed::from(amount as f32 * 15.0),
            });
            conn.send(ZwlrVirtualPointerV1Request::AxisSource {
                zwlr_virtual_pointer_v1: seat.virtual_pointer,
                axis_source: WL_POINTER_AXIS_SOURCE_WHEEL,
            });
            conn.send(ZwlrVirtualPointerV1Request::AxisDiscrete {
                zwlr_virtual_pointer_v1: seat.virtual_pointer,
                time,
                axis,
                value: wayland::Fixed::from(amount as f32 * 15.0),
                discrete: amount.signum() as i32,
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
                    state: WL_POINTER_BUTTON_STATE_PRESSED,
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
                    state: WL_POINTER_BUTTON_STATE_RELEASED,
                });
                conn.send(ZwlrVirtualPointerV1Request::Frame {
                    zwlr_virtual_pointer_v1: seat.virtual_pointer,
                });
            }
        }
    } else if let (
        Some(ei_conn),
        Some(&EiDeviceInterfaces {
            device,
            pointer_absolute,
            button,
            scroll,
        }),
    ) = (ei_conn, state.ei_state.devices.values().next())
    {
        ei_conn.send(EiDeviceRequest::StartEmulating {
            ei_device: device,
            last_serial: state.ei_state.last_serial,
            sequence: state.ei_state.sequence,
        });
        state.ei_state.sequence += 1;

        ei_conn.send(EiPointerAbsoluteRequest::MotionAbsolute {
            ei_pointer_absolute: pointer_absolute,
            x: state.region.center().x as f32,
            y: state.region.center().y as f32,
        });
        ei_conn.send(EiDeviceRequest::Frame {
            ei_device: device,
            last_serial: state.ei_state.last_serial,
            timestamp: time.into(),
        });

        for (axis, amount) in should_scroll {
            ei_conn.send(EiScrollRequest::ScrollDiscrete {
                ei_scroll: scroll,
                x: if axis == WL_POINTER_AXIS_HORIZONTAL_SCROLL {
                    amount as i32 * 120
                } else {
                    0
                },
                y: if axis == WL_POINTER_AXIS_VERTICAL_SCROLL {
                    amount as i32 * 120
                } else {
                    0
                },
            });
            ei_conn.send(EiDeviceRequest::Frame {
                ei_device: device,
                last_serial: state.ei_state.last_serial,
                timestamp: time.into(),
            });
        }

        if let Some(button_index) = should_press {
            if seat.buttons_down.insert(button_index) {
                ei_conn.send(EiButtonRequest::Button {
                    ei_button: button,
                    button: button_index,
                    state: EI_BUTTON_BUTTON_STATE_PRESS,
                });
                ei_conn.send(EiDeviceRequest::Frame {
                    ei_device: device,
                    last_serial: state.ei_state.last_serial,
                    timestamp: time.into(),
                });
            }
        }

        if let Some(button_index) = should_release {
            if seat.buttons_down.remove(&button_index) {
                ei_conn.send(EiButtonRequest::Button {
                    ei_button: button,
                    button: button_index,
                    state: EI_BUTTON_BUTTON_STATE_RELEASED,
                });
                ei_conn.send(EiDeviceRequest::Frame {
                    ei_device: device,
                    last_serial: state.ei_state.last_serial,
                    timestamp: time.into(),
                });
            }
        }

        ei_conn.send(EiDeviceRequest::StopEmulating {
            ei_device: device,
            last_serial: state.ei_state.last_serial,
        });
    }

    redraw_all_outputs(state, conn);
}

fn redraw_all_outputs(state: &mut App, conn: &mut WaylandConnection) {
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
}

fn draw(
    globals: &Globals,
    buffers: &mut TypedHandleMap<Buffer>,
    conn: &mut WaylandConnection,
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
        WL_SHM_FORMAT_ABGR8888,
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

fn make_single_pixel_buffer(
    globals: &Globals,
    buffers: &mut TypedHandleMap<Buffer>,
    conn: &mut WaylandConnection,
    (r, g, b, a): (u32, u32, u32, u32),
) -> BufferId {
    let buffer_id = buffers.insert(Buffer::default());
    let this = &mut buffers[buffer_id];
    let wl_buffer = conn.send_constructor(buffer_id.into_raw(), |id| {
        WpSinglePixelBufferManagerV1Request::CreateU32RgbaBuffer {
            wp_single_pixel_buffer_manager_v1: globals.single_pixel_buffer,
            id,
            r,
            g,
            b,
            a,
        }
    });
    this.wl_buffer = wl_buffer;
    buffer_id
}

fn make_buffer(
    globals: &Globals,
    buffers: &mut TypedHandleMap<Buffer>,
    conn: &mut WaylandConnection,
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
    this.pool = Some(wl_shm_pool);
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
    fn new() -> IdAllocator<I> {
        Self {
            next: 1,
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
struct LibeiConnection {
    wire: ei::Connection,
    next_id: u64,
    interfaces: HashMap<u64, ei_gen::Interface>,
}

impl LibeiConnection {
    fn send<'a>(&mut self, request: impl Into<ei_gen::Request<'a>>) {
        let request = request.into();
        #[cfg(debug_assertions)]
        {
            if std::env::var("LIBEI_DEBUG").is_ok_and(|v| v != "0") {
                eprintln!("-> {request:?}");
            }
        }
        request.marshal(&mut self.wire);
    }

    fn create<O: ei::Object<ei_gen::Interface>>(&mut self) -> O {
        let id = self.next_id;
        self.next_id += 1;
        assert!(self.interfaces.insert(id, O::INTERFACE).is_none());
        O::new(id)
    }

    fn handle_events(&mut self, mut handler: impl FnMut(&mut LibeiConnection, ei_gen::Event<'_>)) {
        while let Some(event) = self.wire.read_message(|msg| {
            ei_gen::Event::unmarshal(self.interfaces.get(&msg.object()).copied().unwrap(), msg)
        }) {
            #[cfg(debug_assertions)]
            {
                if std::env::var("LIBEI_DEBUG").is_ok_and(|v| v != "0") {
                    eprintln!("<- {event:?}");
                }
            }
            match event {
                _ => handler(self, event),
            }
        }
    }
}

#[derive(Debug)]
struct WaylandConnection {
    wire: wayland::Connection,
    ids: IdAllocator<ObjectData>,
    sync_callback: WlCallback,
    sync_done: bool,
}

#[derive(Debug)]
struct ObjectData {
    interface: wl_gen::Interface,
    data: u64,
}

impl WaylandConnection {
    fn send<'a>(&mut self, request: impl Into<Request<'a>>) {
        let request = request.into();
        #[cfg(debug_assertions)]
        {
            if std::env::var("WAYLAND_DEBUG").is_ok_and(|v| v != "0") {
                eprintln!("-> {request:?}");
            }
        }
        request.marshal(&mut self.wire);
    }

    fn send_constructor<'a, O, F, IR>(&mut self, data: u64, f: F) -> O
    where
        O: wayland::Object<wl_gen::Interface>,
        F: Fn(O) -> IR,
        IR: Into<Request<'a>>,
    {
        let obj = self.create(data);
        let request = f(obj).into();
        #[cfg(debug_assertions)]
        {
            if std::env::var("WAYLAND_DEBUG").is_ok_and(|v| v != "0") {
                eprintln!("-> {request:?}");
            }
        }
        request.marshal(&mut self.wire);
        obj
    }

    fn create<O: wayland::Object<wl_gen::Interface>>(&mut self, data: u64) -> O {
        let id = self.ids.allocate(ObjectData {
            interface: O::INTERFACE,
            data,
        });
        O::new(id)
    }

    fn handle_events(&mut self, mut handler: impl FnMut(&mut WaylandConnection, Event<'_>)) {
        while let Some(event) = self
            .wire
            .read_message(|msg| Event::unmarshal(self.ids.data_for(msg.object()).interface, msg))
        {
            #[cfg(debug_assertions)]
            {
                if std::env::var("WAYLAND_DEBUG").is_ok_and(|v| v != "0") {
                    eprintln!("<- {event:?}");
                }
            }
            match event {
                Event::WlDisplay(event) => match event {
                    WlDisplayEvent::Error {
                        wl_display: _,
                        object_id,
                        code,
                        message,
                    } => {
                        panic!("Protocol error {code} on object {object_id}: {message}")
                    }
                    WlDisplayEvent::DeleteId { wl_display: _, id } => {
                        self.ids.release(id);
                    }
                },
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

    fn roundtrip(&mut self, mut handler: impl FnMut(&mut WaylandConnection, Event<'_>)) {
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

fn bind_global<O: wayland::Object<wl_gen::Interface>>(
    conn: &mut WaylandConnection,
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

fn main() -> Result<()> {
    let ei_fd = ei::client_socket_from_env()?;
    let ei_wire_conn = ei_fd.map(ei::Connection::new);
    let mut ei_conn = ei_wire_conn.map(|wire| LibeiConnection {
        wire,
        next_id: 0,
        interfaces: HashMap::new(),
    });

    if let Some(ei_conn) = ei_conn.as_mut() {
        ei_conn.create::<EiHandshake>();
        ei_conn.wire.read_blocking()?;
        ei_conn.handle_events(|ei_conn, event| match event {
            ei_gen::Event::EiHandshake(EiHandshakeEvent::HandshakeVersion {
                ei_handshake,
                version,
            }) => {
                ei_conn.send(EiHandshakeRequest::HandshakeVersion {
                    ei_handshake,
                    version,
                });
                ei_conn.send(EiHandshakeRequest::ContextType {
                    ei_handshake,
                    context_type: EI_HANDSHAKE_CONTEXT_TYPE_SENDER,
                });
                ei_conn.send(EiHandshakeRequest::Name {
                    ei_handshake,
                    name: "waypoint".into(),
                });
                for interface in [
                    ei_gen::Interface::EiCallback,
                    ei_gen::Interface::EiConnection,
                    ei_gen::Interface::EiSeat,
                    ei_gen::Interface::EiDevice,
                    ei_gen::Interface::EiPingpong,
                    ei_gen::Interface::EiPointerAbsolute,
                    ei_gen::Interface::EiButton,
                    ei_gen::Interface::EiScroll,
                ] {
                    ei_conn.send(EiHandshakeRequest::InterfaceVersion {
                        ei_handshake,
                        name: interface.name().into(),
                        version: interface.version(),
                    });
                }
                ei_conn.send(EiHandshakeRequest::Finish { ei_handshake });
            }
            _ => {
                eprintln!("unexpected event {event:?} ignored");
            }
        });
    }

    let wayland_fd = wayland::client_socket_from_env()?.context("no wayland display available")?;
    let wl_wire_conn = wayland::Connection::new(wayland_fd);
    let mut wl_conn = WaylandConnection {
        wire: wl_wire_conn,
        ids: IdAllocator::new(),
        sync_callback: Default::default(),
        sync_done: false,
    };

    let wl_display: WlDisplay = wl_conn.create(0);
    let wl_registry = wl_conn.send_constructor(0, |registry| WlDisplayRequest::GetRegistry {
        wl_display,
        registry,
    });
    let mut global_list: HashMap<String, Vec<(u32, u32)>> = HashMap::new();
    wl_conn.roundtrip(|_conn, event| match event {
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

    let mut app = App {
        quit: false,
        globals: Globals {
            wl_shm: bind_global(&mut wl_conn, wl_registry, &global_list, 1..=1)
                .context("compositor doesn't support wl_shm")?,
            wl_compositor: bind_global(&mut wl_conn, wl_registry, &global_list, 4..=4)
                .context("compositor doesn't support wl_compositor")?,
            xdg_output: bind_global(&mut wl_conn, wl_registry, &global_list, 3..=3)
                .context("compositor doesn't support xdg_output_manager_v1")?,
            layer_shell: bind_global(&mut wl_conn, wl_registry, &global_list, 1..=1)
                .context("compositor doesn't support zwlr_layer_shell_v1")?,
            single_pixel_buffer: bind_global(&mut wl_conn, wl_registry, &global_list, 1..=1)
                .context("compositor doesn't support wp_single_pixel_buffer_manager_v1")?,
            virtual_pointer_manager: bind_global(&mut wl_conn, wl_registry, &global_list, 1..=1)
                .unwrap_or_default(),
        },
        seats: TypedHandleMap::new(),
        outputs: TypedHandleMap::new(),
        buffers: TypedHandleMap::new(),
        config: Config::load()?,
        region: Region::default(),
        region_history: Vec::new(),
        global_bounds: None,
        ei_state: EiState::default(),
        input_surface: None,
        default_region: None,
    };

    if let Some(seat_list) = global_list.get(wl_gen::Interface::WlSeat.name()) {
        for &(name, sversion) in seat_list {
            let seat_id = app.seats.insert(Seat::default());
            let wl_seat = wl_conn.send_constructor(seat_id.into_raw(), |WlSeat(id)| {
                Request::WlRegistry(WlRegistryRequest::Bind {
                    wl_registry,
                    name,
                    interface: wl_gen::Interface::WlSeat.name().into(),
                    version: sversion.min(4),
                    id,
                })
            });
            let seat = &mut app.seats[seat_id];
            if !app.globals.virtual_pointer_manager.is_null() {
                let virtual_pointer = wl_conn.send_constructor(0, |id| {
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

    if let Some(output_list) = global_list.get(wl_gen::Interface::WlOutput.name()) {
        for &(name, sversion) in output_list {
            assert!(sversion >= 2);
            let output_id = app.outputs.insert(Output::default());
            let output = &mut app.outputs[output_id];
            let wl_output = wl_conn.send_constructor(output_id.into_raw(), |WlOutput(id)| {
                Request::WlRegistry(WlRegistryRequest::Bind {
                    wl_registry,
                    name,
                    interface: wl_gen::Interface::WlOutput.name().into(),
                    version: sversion.min(2),
                    id,
                })
            });
            let xdg_output = wl_conn.send_constructor(output_id.into_raw(), |id| {
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

    wl_conn.roundtrip(|conn, event| {
        app.handle_event(conn, ei_conn.as_mut(), event);
    });

    {
        app.input_surface = Some(Surface::default());
        let surface = app.input_surface.as_mut().unwrap();
        let wl_surface = wl_conn.send_constructor(OutputId::EMPTY.into_raw(), |id| {
            WlCompositorRequest::CreateSurface {
                wl_compositor: app.globals.wl_compositor,
                id,
            }
        });
        let layer_surface = wl_conn.send_constructor(OutputId::EMPTY.into_raw(), |id| {
            ZwlrLayerShellV1Request::GetLayerSurface {
                zwlr_layer_shell_v1: app.globals.layer_shell,
                id,
                surface: wl_surface,
                output: WlOutput(0),
                layer: ZWLR_LAYER_SHELL_V1_LAYER_OVERLAY,
                namespace: "waypoint.input".into(),
            }
        });
        wl_conn.send(ZwlrLayerSurfaceV1Request::SetSize {
            zwlr_layer_surface_v1: layer_surface,
            width: 1,
            height: 1,
        });
        wl_conn.send(ZwlrLayerSurfaceV1Request::SetKeyboardInteractivity {
            zwlr_layer_surface_v1: layer_surface,
            keyboard_interactivity: ZWLR_LAYER_SURFACE_V1_KEYBOARD_INTERACTIVITY_EXCLUSIVE,
        });
        let region = wl_conn.send_constructor(0, |id| WlCompositorRequest::CreateRegion {
            wl_compositor: app.globals.wl_compositor,
            id,
        });
        wl_conn.send(WlSurfaceRequest::SetInputRegion { wl_surface, region });
        wl_conn.send(WlRegionRequest::Destroy { wl_region: region });
        wl_conn.send(WlSurfaceRequest::Commit { wl_surface });

        surface.output = OutputId::EMPTY;
        surface.wl_surface = wl_surface;
        surface.layer_surface = layer_surface;
    }

    for (output_id, output) in app.outputs.iter_mut_with_handles() {
        output.surface = Some(Surface::default());
        let surface = output.surface.as_mut().unwrap();

        let wl_surface = wl_conn.send_constructor(output_id.into_raw(), |id| {
            WlCompositorRequest::CreateSurface {
                wl_compositor: app.globals.wl_compositor,
                id,
            }
        });
        let layer_surface = wl_conn.send_constructor(output_id.into_raw(), |id| {
            ZwlrLayerShellV1Request::GetLayerSurface {
                zwlr_layer_shell_v1: app.globals.layer_shell,
                id,
                surface: wl_surface,
                output: output.wl_output,
                layer: ZWLR_LAYER_SHELL_V1_LAYER_OVERLAY,
                namespace: "waypoint.drawing".into(),
            }
        });
        wl_conn.send(ZwlrLayerSurfaceV1Request::SetSize {
            zwlr_layer_surface_v1: layer_surface,
            width: 0,
            height: 0,
        });
        wl_conn.send(ZwlrLayerSurfaceV1Request::SetAnchor {
            zwlr_layer_surface_v1: layer_surface,
            anchor: ZWLR_LAYER_SURFACE_V1_ANCHOR_TOP
                | ZWLR_LAYER_SURFACE_V1_ANCHOR_BOTTOM
                | ZWLR_LAYER_SURFACE_V1_ANCHOR_LEFT
                | ZWLR_LAYER_SURFACE_V1_ANCHOR_RIGHT,
        });
        wl_conn.send(ZwlrLayerSurfaceV1Request::SetExclusiveZone {
            zwlr_layer_surface_v1: layer_surface,
            zone: -1,
        });
        wl_conn.send(ZwlrLayerSurfaceV1Request::SetKeyboardInteractivity {
            zwlr_layer_surface_v1: layer_surface,
            keyboard_interactivity: ZWLR_LAYER_SURFACE_V1_KEYBOARD_INTERACTIVITY_NONE,
        });
        let region = wl_conn.send_constructor(0, |id| WlCompositorRequest::CreateRegion {
            wl_compositor: app.globals.wl_compositor,
            id,
        });
        wl_conn.send(WlSurfaceRequest::SetInputRegion { wl_surface, region });
        wl_conn.send(WlRegionRequest::Destroy { wl_region: region });
        wl_conn.send(WlSurfaceRequest::Commit { wl_surface });

        surface.output = output_id;
        surface.wl_surface = wl_surface;
        surface.layer_surface = layer_surface;
    }

    if let Some(ei_conn) = ei_conn.as_mut() {
        ei_conn.wire.flush_blocking()?;
        ei_conn.wire.read_blocking()?;
        ei_conn.handle_events(|ei_conn, event| app.handle_ei_event(ei_conn, event));
    }

    wl_conn.roundtrip(|conn, event| {
        app.handle_event(conn, ei_conn.as_mut(), event);
    });

    let global_bounds = app
        .outputs
        .iter()
        .fold(Region::default(), |acc, output| acc.union(&output.region()));
    app.global_bounds = Some(global_bounds);
    app.region = app.default_region.unwrap_or(global_bounds);
    redraw_all_outputs(&mut app, &mut wl_conn);

    for seat in app.seats.iter() {
        if !seat.virtual_pointer.is_null() {
            wl_conn.send(ZwlrVirtualPointerV1Request::MotionAbsolute {
                zwlr_virtual_pointer_v1: seat.virtual_pointer,
                time: 0,
                x: app.region.center().x as u32,
                y: app.region.center().y as u32,
                x_extent: app.global_bounds.unwrap_or_default().width as u32,
                y_extent: app.global_bounds.unwrap_or_default().height as u32,
            });
            wl_conn.send(ZwlrVirtualPointerV1Request::Frame {
                zwlr_virtual_pointer_v1: seat.virtual_pointer,
            });
        } else if let (
            Some(ei_conn),
            Some(&EiDeviceInterfaces {
                device,
                pointer_absolute,
                ..
            }),
        ) = (ei_conn.as_mut(), app.ei_state.devices.values().next())
        {
            ei_conn.send(EiDeviceRequest::StartEmulating {
                ei_device: device,
                last_serial: app.ei_state.last_serial,
                sequence: app.ei_state.sequence,
            });
            app.ei_state.sequence += 1;

            ei_conn.send(EiPointerAbsoluteRequest::MotionAbsolute {
                ei_pointer_absolute: pointer_absolute,
                x: app.region.center().x as f32,
                y: app.region.center().y as f32,
            });
            ei_conn.send(EiDeviceRequest::Frame {
                ei_device: device,
                last_serial: app.ei_state.last_serial,
                timestamp: 0,
            });

            ei_conn.send(EiDeviceRequest::StopEmulating {
                ei_device: device,
                last_serial: app.ei_state.last_serial,
            });
        }
    }

    wl_conn.wire.flush_blocking()?;

    while !app.quit {
        let now = Instant::now();
        let next_timer = app
            .seats
            .iter()
            .filter_map(|seat| seat.key_repeat)
            .map(|(instant, _)| instant)
            .min();
        let timeout = match next_timer {
            Some(instant) => instant.duration_since(now).as_millis() as i32,
            None => -1,
        };
        let (wl_revents, ei_revents) = if let Some(ei_conn) = ei_conn.as_ref() {
            let mut pollfds = [
                PollFd::new(&wl_conn.wire, PollFlags::IN),
                PollFd::new(&ei_conn.wire, PollFlags::IN),
            ];
            rustix::event::poll(&mut pollfds, timeout)?;
            let wl_revents = pollfds[0].revents();
            let ei_revents = pollfds[1].revents();
            (wl_revents, ei_revents)
        } else {
            let mut pollfds = [PollFd::new(&wl_conn.wire, PollFlags::IN)];
            rustix::event::poll(&mut pollfds, timeout)?;
            let wl_revents = pollfds[0].revents();
            (wl_revents, PollFlags::empty())
        };
        if wl_revents.contains(PollFlags::IN) {
            wl_conn.wire.read_nonblocking()?;
            wl_conn.handle_events(|conn, event| app.handle_event(conn, ei_conn.as_mut(), event));
        }
        if ei_revents.contains(PollFlags::IN) {
            let ei_conn = ei_conn.as_mut().unwrap();
            ei_conn.wire.read_nonblocking()?;
            ei_conn.handle_events(|ei_conn, event| app.handle_ei_event(ei_conn, event));
        }
        if let Some(ei_conn) = ei_conn.as_mut() {
            ei_conn.wire.flush_blocking()?;
        }
        wl_conn.wire.flush_blocking()?;
        let mut seats = Vec::new();
        for (seat_id, seat) in app.seats.iter_mut_with_handles() {
            if let Some((instant, _)) = seat.key_repeat {
                if instant <= now {
                    seats.push(seat_id);
                }
            }
        }
        for seat_id in seats {
            let seat = &mut app.seats[seat_id];
            let (instant, keycode) = seat.key_repeat.unwrap();
            handle_key_pressed(
                &mut app,
                0,
                keycode,
                seat_id,
                &mut wl_conn,
                ei_conn.as_mut(),
            );
            let seat = &mut app.seats[seat_id];
            seat.key_repeat = Some((instant + seat.repeat_period, keycode))
        }
    }

    for seat in app.seats.iter() {
        for &button in &seat.buttons_down {
            wl_conn.send(ZwlrVirtualPointerV1Request::Button {
                zwlr_virtual_pointer_v1: seat.virtual_pointer,
                time: 0,
                button,
                state: WL_POINTER_BUTTON_STATE_RELEASED,
            });
            wl_conn.send(ZwlrVirtualPointerV1Request::Frame {
                zwlr_virtual_pointer_v1: seat.virtual_pointer,
            });
        }
    }
    wl_conn.wire.flush_blocking()?;

    Ok(())
}

impl App {
    fn handle_ei_event(&mut self, ei_conn: &mut LibeiConnection, event: ei_gen::Event) {
        match event {
            ei_gen::Event::EiHandshake(event) => match event {
                EiHandshakeEvent::HandshakeVersion { .. } => {}
                EiHandshakeEvent::InterfaceVersion { .. } => {}
                EiHandshakeEvent::Connection {
                    ei_handshake: _,
                    serial,
                    connection,
                    version: _,
                } => {
                    ei_conn
                        .interfaces
                        .insert(connection.id(), ei_gen::Interface::EiConnection);
                    self.ei_state.last_serial = serial;
                }
            },
            ei_gen::Event::EiCallback(event) => match event {
                EiCallbackEvent::Done { .. } => {}
            },
            ei_gen::Event::EiConnection(event) => match event {
                EiConnectionEvent::Disconnected { .. } => {}
                EiConnectionEvent::Seat {
                    ei_connection: _,
                    seat,
                    version: _,
                } => {
                    ei_conn
                        .interfaces
                        .insert(seat.id(), ei_gen::Interface::EiSeat);
                    self.ei_state.seat_capabilities.insert(seat.id(), 0);
                }
                EiConnectionEvent::InvalidObject {
                    ei_connection: _,
                    last_serial: _,
                    invalid_id: _,
                } => {}
                EiConnectionEvent::Ping {
                    ei_connection: _,
                    ping,
                    version: _,
                } => {
                    ei_conn.send(EiPingpongRequest::Done {
                        ei_pingpong: ping,
                        callback_data: 0,
                    });
                }
            },
            ei_gen::Event::EiDevice(event) => match event {
                EiDeviceEvent::Destroyed { .. } => {}
                EiDeviceEvent::Name { .. } => {}
                EiDeviceEvent::DeviceType { .. } => {}
                EiDeviceEvent::Dimensions { .. } => {}
                EiDeviceEvent::Region { .. } => {}
                EiDeviceEvent::Interface {
                    ei_device,
                    object,
                    interface_name,
                    version: _,
                } => match interface_name.as_ref() {
                    "ei_pointer_absolute" => {
                        ei_conn
                            .interfaces
                            .insert(object, ei_gen::Interface::EiPointerAbsolute);
                        let data = self.ei_state.devices.get_mut(&ei_device.id()).unwrap();
                        data.pointer_absolute = EiPointerAbsolute(object);
                    }
                    "ei_button" => {
                        ei_conn
                            .interfaces
                            .insert(object, ei_gen::Interface::EiButton);
                        let data = self.ei_state.devices.get_mut(&ei_device.id()).unwrap();
                        data.button = EiButton(object);
                    }
                    "ei_scroll" => {
                        ei_conn
                            .interfaces
                            .insert(object, ei_gen::Interface::EiScroll);
                        let data = self.ei_state.devices.get_mut(&ei_device.id()).unwrap();
                        data.scroll = EiScroll(object);
                    }
                    _ => {
                        unreachable!();
                    }
                },
                EiDeviceEvent::Done { .. } => {}
                EiDeviceEvent::Resumed { .. } => {}
                EiDeviceEvent::Paused { .. } => {}
                EiDeviceEvent::RegionMappingId { .. } => {}
            },
            ei_gen::Event::EiPingpong(event) => match event {},
            ei_gen::Event::EiSeat(event) => match event {
                EiSeatEvent::Destroyed { .. } => {}
                EiSeatEvent::Name { .. } => {}
                EiSeatEvent::Capability {
                    ei_seat,
                    mask,
                    interface,
                } => match interface.as_ref() {
                    "ei_pointer_absolute" | "ei_button" | "ei_scroll" => {
                        let caps = self
                            .ei_state
                            .seat_capabilities
                            .get_mut(&ei_seat.id())
                            .unwrap();
                        *caps |= mask;
                    }
                    _ => {}
                },
                EiSeatEvent::Done { ei_seat } => {
                    let capabilities = self
                        .ei_state
                        .seat_capabilities
                        .get(&ei_seat.id())
                        .copied()
                        .unwrap();
                    ei_conn.send(EiSeatRequest::Bind {
                        ei_seat,
                        capabilities,
                    });
                }
                EiSeatEvent::Device {
                    ei_seat: _,
                    device,
                    version: _,
                } => {
                    ei_conn
                        .interfaces
                        .insert(device.id(), ei_gen::Interface::EiDevice);
                    self.ei_state.devices.insert(
                        device.id(),
                        EiDeviceInterfaces {
                            device,
                            ..EiDeviceInterfaces::default()
                        },
                    );
                }
            },
            ei_gen::Event::EiButton(event) => match event {
                EiButtonEvent::Destroyed { .. } => {}
            },
            ei_gen::Event::EiPointerAbsolute(event) => match event {
                EiPointerAbsoluteEvent::Destroyed { .. } => {}
            },
            ei_gen::Event::EiScroll(event) => match event {
                EiScrollEvent::Destroyed { .. } => {}
            },
        }
    }

    fn handle_event(
        &mut self,
        conn: &mut WaylandConnection,
        ei_conn: Option<&mut LibeiConnection>,
        event: Event,
    ) {
        match event {
            Event::WlSeat(event) => match event {
                WlSeatEvent::Capabilities {
                    wl_seat,
                    capabilities,
                } => {
                    let seat_id = SeatId::from_raw(conn.ids.data_for(wl_seat.id()).data);
                    let seat = &mut self.seats[seat_id];
                    if capabilities & WL_SEAT_CAPABILITY_KEYBOARD != 0 {
                        seat.keyboard = conn.send_constructor(seat_id.into_raw(), |id| {
                            WlSeatRequest::GetKeyboard { wl_seat, id }
                        });
                    }
                }
                WlSeatEvent::Name { .. } => {}
            },
            Event::WlKeyboard(event) => match event {
                WlKeyboardEvent::Keymap {
                    wl_keyboard,
                    format,
                    fd,
                    size,
                } => {
                    if format == WL_KEYBOARD_KEYMAP_FORMAT_XKB_V1 {
                        let seat_id = SeatId::from_raw(conn.ids.data_for(wl_keyboard.id()).data);
                        let seat = &mut self.seats[seat_id];
                        let keymap = unsafe {
                            let map = MmapOptions::new()
                                .len(size as usize)
                                .map_copy_read_only(&fd)
                                .unwrap();
                            let mut diagnostics = Vec::new();
                            let keymap_result =
                                seat.xkb.keymap_from_bytes(&mut diagnostics, None, &map);
                            for diagnostic in diagnostics {
                                match diagnostic.kind().severity() {
                                    kbvm::xkb::diagnostic::Severity::Debug => {}
                                    _ => {
                                        eprintln!("{}", diagnostic.with_code());
                                    }
                                }
                            }
                            if let Err(e) = &keymap_result {
                                eprintln!("Error compiling keymap: {e}");
                            }
                            keymap_result
                        }
                        .ok();
                        if let Some(keymap) = keymap {
                            seat.lookup_table = Some(keymap.to_builder().build_lookup_table());
                            seat.specialized_bindings = specialize_bindings(&keymap, &self.config);
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
                    let seat = &mut self.seats[seat_id];
                    let key_repeat = seat.key_repeat;
                    let keycode = kbvm::Keycode::from_evdev(key);
                    let keycode_repeats = seat.lookup_table.as_ref().unwrap().repeats(keycode);
                    let repeat_delay = seat.repeat_delay;

                    if state == WL_KEYBOARD_KEY_STATE_PRESSED
                        && (key_repeat.is_none() || key_repeat.is_some_and(|(_, it)| it != keycode))
                    {
                        handle_key_pressed(self, time, keycode, seat_id, conn, ei_conn);
                        if keycode_repeats {
                            let seat_id =
                                SeatId::from_raw(conn.ids.data_for(wl_keyboard.id()).data);
                            let seat = &mut self.seats[seat_id];
                            seat.key_repeat = Some((Instant::now() + repeat_delay, keycode));
                        }
                    }

                    if state == WL_KEYBOARD_KEY_STATE_RELEASED
                        && key_repeat.is_some_and(|(_, it)| it == keycode)
                    {
                        let seat_id = SeatId::from_raw(conn.ids.data_for(wl_keyboard.id()).data);
                        let seat = &mut self.seats[seat_id];
                        seat.key_repeat = None;
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
                    seat.group = kbvm::GroupIndex(group);
                    seat.mods = kbvm::ModifierMask(mods_depressed | mods_latched | mods_locked);
                }
                WlKeyboardEvent::RepeatInfo {
                    wl_keyboard,
                    rate,
                    delay,
                } => {
                    let seat_id = SeatId::from_raw(conn.ids.data_for(wl_keyboard.id()).data);
                    let seat = &mut self.seats[seat_id];
                    seat.repeat_period = Duration::from_millis(1000 / rate as u64);
                    seat.repeat_delay = Duration::from_millis(delay as u64);
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
                WlSurfaceEvent::Enter { wl_surface, output } => {
                    let surface_data = conn.ids.data_for(wl_surface.id()).data;
                    if surface_data == OutputId::EMPTY.into_raw() {
                        let output_data = conn.ids.data_for(output.id()).data;
                        let output_id = OutputId::from_raw(output_data);
                        let output = &mut self.outputs[output_id];
                        self.default_region = Some(output.region());
                    }
                }
                WlSurfaceEvent::Leave { .. } => {}
            },
            Event::ZwlrLayerSurfaceV1(event) => match event {
                ZwlrLayerSurfaceV1Event::Configure {
                    zwlr_layer_surface_v1,
                    serial,
                    width,
                    height,
                } => {
                    let layer_surface_data = conn.ids.data_for(zwlr_layer_surface_v1.id()).data;
                    if layer_surface_data == OutputId::EMPTY.into_raw() {
                        let surface = self.input_surface.as_mut().unwrap();
                        // this is the input surface
                        conn.send(ZwlrLayerSurfaceV1Request::AckConfigure {
                            zwlr_layer_surface_v1,
                            serial,
                        });
                        conn.send(ZwlrLayerSurfaceV1Request::SetSize {
                            zwlr_layer_surface_v1,
                            width: 1,
                            height: 1,
                        });
                        let buffer_id = make_single_pixel_buffer(
                            &self.globals,
                            &mut self.buffers,
                            conn,
                            (0, 0, 0, 0),
                        );
                        let buffer = &mut self.buffers[buffer_id];
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
                    } else {
                        let output_id = OutputId::from_raw(layer_surface_data);
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
                }
                ZwlrLayerSurfaceV1Event::Closed {
                    zwlr_layer_surface_v1,
                } => {
                    let layer_surface_data = conn.ids.data_for(zwlr_layer_surface_v1.id()).data;
                    if layer_surface_data != OutputId::EMPTY.into_raw() {
                        let output_id = OutputId::from_raw(layer_surface_data);
                        let output = &mut self.outputs[output_id];
                        output.surface = None;
                    } else {
                        // TODO
                    }
                }
            },
            Event::WlBuffer(event) => match event {
                WlBufferEvent::Release { wl_buffer } => {
                    let buffer_id = BufferId::from_raw(conn.ids.data_for(wl_buffer.id()).data);
                    let buffer = &mut self.buffers[buffer_id];
                    if let Some(wl_shm_pool) = buffer.pool {
                        conn.send(WlShmPoolRequest::Destroy { wl_shm_pool });
                    }
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
            Event::WlDisplay(_) => unreachable!("handled elsewhere"),
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
