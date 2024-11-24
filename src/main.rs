#![allow(clippy::single_match, clippy::match_single_binding)]

extern crate waypoint_scfg as scfg;

mod config;
mod region;

use crate::{
    config::{specialize_bindings, Cmd, Config, Direction},
    region::Region,
};
use anyhow::{Context as _, Result};
use bytemuck::{Pod, Zeroable};
use calloop::LoopSignal;
use calloop_wayland_source::WaylandSource;
use handy::typed::{TypedHandle, TypedHandleMap};
use memmap2::{MmapMut, MmapOptions};
use std::{
    collections::{HashMap, HashSet},
    io::Write,
    os::fd::{AsRawFd, BorrowedFd, IntoRawFd},
};
use tiny_skia::{Color, Paint, PathBuilder, Shader, Stroke, Transform};
use wayland_client::{
    delegate_noop,
    globals::{registry_queue_init, Global, GlobalListContents},
    protocol::{
        wl_buffer::{self, WlBuffer},
        wl_compositor::WlCompositor,
        wl_keyboard::{self, KeyState, KeymapFormat, WlKeyboard},
        wl_output::{self, WlOutput},
        wl_pointer::ButtonState,
        wl_region::WlRegion,
        wl_registry::{self, WlRegistry},
        wl_seat::{self, Capability, WlSeat},
        wl_shm::{Format, WlShm},
        wl_shm_pool::WlShmPool,
        wl_surface::{self, WlSurface},
    },
    Connection, Dispatch, QueueHandle, WEnum,
};
use wayland_protocols::xdg::xdg_output::zv1::client::{
    zxdg_output_manager_v1::ZxdgOutputManagerV1,
    zxdg_output_v1::{self, ZxdgOutputV1},
};
use wayland_protocols_wlr::{
    layer_shell::v1::client::{
        zwlr_layer_shell_v1::{Layer, ZwlrLayerShellV1},
        zwlr_layer_surface_v1::{self, Anchor, KeyboardInteractivity, ZwlrLayerSurfaceV1},
    },
    virtual_pointer::v1::client::{
        zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1,
        zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1,
    },
};
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
    virtual_pointer_manager: Option<ZwlrVirtualPointerManagerV1>,
}

struct Seat {
    wl_seat: Option<WlSeat>,
    virtual_pointer: Option<ZwlrVirtualPointerV1>,
    xkb: xkb::Context,
    xkb_state: Option<xkb::State>,
    keyboard: Option<WlKeyboard>,
    buttons_down: HashSet<u32>,
    mod_indices: ModIndices,
    specialized_bindings: HashMap<(xkb::ModMask, xkb::Keycode), Vec<Cmd>>,
}

#[derive(Default)]
struct Output {
    surface: Option<Surface>,
    wl_output: Option<WlOutput>,
    xdg_output: Option<ZxdgOutputV1>,
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
    wl_surface: Option<WlSurface>,
    layer_surface: Option<ZwlrLayerSurfaceV1>,
    width: u32,
    height: u32,
}

#[derive(Default)]
struct Buffer {
    pool: Option<WlShmPool>,
    wl_buffer: Option<WlBuffer>,
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

fn handle_key_pressed(state: &mut App, key: u32, seat_id: SeatId, qhandle: &QueueHandle<App>) {
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
            qhandle,
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

    if let Some(virtual_pointer) = seat.virtual_pointer.as_ref() {
        virtual_pointer.motion_absolute(
            0,
            state.region.center().x as u32,
            state.region.center().y as u32,
            state.global_bounds.width as u32,
            state.global_bounds.height as u32,
        );
        virtual_pointer.frame();

        for (axis, amount) in should_scroll {
            virtual_pointer.axis(0, axis, amount);
            virtual_pointer.frame();
        }

        if let Some(btn) = should_press {
            if seat.buttons_down.insert(btn) {
                virtual_pointer.button(0, btn, ButtonState::Pressed);
                virtual_pointer.frame();
            }
        }

        if let Some(btn) = should_release {
            if seat.buttons_down.remove(&btn) {
                virtual_pointer.button(0, btn, ButtonState::Released);
                virtual_pointer.frame();
            }
        }
    }
}

fn draw(
    globals: &Globals,
    buffers: &mut TypedHandleMap<Buffer>,
    qhandle: &QueueHandle<App>,
    scale: u32,
    surface: &Surface,
    region: Region,
) -> Result<()> {
    let wl_surface = surface.wl_surface.as_ref().unwrap();
    let buffer_data = make_buffer(
        globals,
        buffers,
        qhandle,
        i32::try_from(surface.width * scale).unwrap(),
        i32::try_from(surface.height * scale).unwrap(),
        i32::try_from(surface.width * scale * 4).unwrap(),
        Format::Argb8888,
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
    let wl_buffer = buffer.wl_buffer.as_ref().unwrap();
    wl_surface.set_buffer_scale(i32::try_from(scale).unwrap());
    wl_surface.attach(Some(wl_buffer), 0, 0);
    wl_surface.damage_buffer(0, 0, i32::MAX, i32::MAX);
    wl_surface.commit();

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
    qhandle: &QueueHandle<App>,
    width: i32,
    height: i32,
    stride: i32,
    format: Format,
) -> Result<BufferId> {
    let buffer_id = buffers.insert(Buffer::default());
    let this = &mut buffers[buffer_id];
    let memfd = memfd::MemfdOptions::new().create("waypoint-buffer")?;
    let len_i32 = stride.checked_mul(height).expect("buffer too big");
    let len_usize = usize::try_from(len_i32).expect("buffer too big");
    memfd.as_file().write_all(&vec![0u8; len_i32 as usize])?;
    let borrowed_memfd = unsafe { BorrowedFd::borrow_raw(memfd.as_raw_fd()) };
    let pool = globals
        .wl_shm
        .create_pool(borrowed_memfd, len_i32, qhandle, ());
    let wl_buffer = pool.create_buffer(0, width, height, stride, format, qhandle, buffer_id);
    let mmap = unsafe { MmapOptions::new().len(len_usize).map_mut(memfd.as_file())? };
    this.pool = Some(pool);
    this.wl_buffer = Some(wl_buffer);
    this.mmap = Some(mmap);
    Ok(buffer_id)
}

fn main() -> Result<()> {
    let mut event_loop: calloop::EventLoop<'_, App> = calloop::EventLoop::try_new().unwrap();
    let conn = Connection::connect_to_env()?;
    let (global_list, queue) = registry_queue_init(&conn)?;
    let qhandle = queue.handle();
    let source = WaylandSource::new(conn, queue);
    let dispatcher = calloop::Dispatcher::new(source, |(), queue, app| queue.dispatch_pending(app));
    event_loop
        .handle()
        .register_dispatcher(dispatcher.clone())?;
    let mut app = App {
        loop_signal: event_loop.get_signal(),
        globals: Globals {
            wl_shm: global_list
                .bind(&qhandle, 1..=1, ())
                .context("compositor doesn't support wl_shm")?,
            wl_compositor: global_list
                .bind(&qhandle, 4..=4, ())
                .context("compositor doesn't support wl_compositor")?,
            xdg_output: global_list
                .bind(&qhandle, 3..=3, ())
                .context("compositor doesn't support xdg_output_manager_v1")?,
            layer_shell: global_list
                .bind(&qhandle, 1..=1, ())
                .context("compositor doesn't support zwlr_layer_shell_v1")?,
            virtual_pointer_manager: global_list
                .bind::<ZwlrVirtualPointerManagerV1, _, _>(&qhandle, 1..=1, ())
                .ok(),
        },
        seats: TypedHandleMap::new(),
        outputs: TypedHandleMap::new(),
        buffers: TypedHandleMap::new(),
        config: Config::load()?,
        region: Region::default(),
        region_history: Vec::new(),
        global_bounds: Region::default(),
    };
    global_list.contents().with_list(|list| {
        for &Global {
            name,
            ref interface,
            version,
        } in list
        {
            let registry = global_list.registry();
            match interface.as_str() {
                "wl_seat" => {
                    let seat_id = app.seats.insert(Seat::default());
                    let wl_seat = registry.bind(name, version.max(1), &qhandle, seat_id);
                    let seat = &mut app.seats[seat_id];
                    if let Some(virtual_pointer_manager) = &app.globals.virtual_pointer_manager {
                        let virtual_pointer = virtual_pointer_manager.create_virtual_pointer(
                            Some(&wl_seat),
                            &qhandle,
                            (),
                        );
                        seat.virtual_pointer = Some(virtual_pointer);
                    }
                    seat.wl_seat = Some(wl_seat);
                }
                "wl_output" => {
                    let output_id = app.outputs.insert(Output::default());
                    let output = &mut app.outputs[output_id];
                    let wl_output = registry.bind(name, version.max(1), &qhandle, output_id);
                    let xdg_output = app
                        .globals
                        .xdg_output
                        .get_xdg_output(&wl_output, &qhandle, output_id);
                    output.wl_output = Some(wl_output);
                    output.xdg_output = Some(xdg_output);
                }
                _ => {}
            }
        }
    });

    dispatcher.as_source_mut().queue().roundtrip(&mut app)?;

    for output in app.outputs.iter() {
        app.global_bounds = app.global_bounds.union(&output.region());
    }

    app.region = app.global_bounds;

    for (output_id, output) in app.outputs.iter_mut_with_handles() {
        output.surface = Some(Surface::default());
        let surface = output.surface.as_mut().unwrap();
        let wl_output = output.wl_output.as_ref().unwrap();
        let wl_surface = app.globals.wl_compositor.create_surface(&qhandle, ());
        let layer_surface = app.globals.layer_shell.get_layer_surface(
            &wl_surface,
            Some(wl_output),
            Layer::Overlay,
            String::from("waypoint"),
            &qhandle,
            output_id,
        );
        layer_surface.set_size(0, 0);
        layer_surface.set_anchor(Anchor::Top | Anchor::Bottom | Anchor::Left | Anchor::Right);
        layer_surface.set_exclusive_zone(-1);
        layer_surface.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
        let region = app.globals.wl_compositor.create_region(&qhandle, ());
        wl_surface.set_input_region(Some(&region));
        wl_surface.commit();
        surface.output = output_id;
        surface.wl_surface = Some(wl_surface);
        surface.layer_surface = Some(layer_surface);
    }
    event_loop.run(None, &mut app, |_| {})?;
    for seat in app.seats.iter() {
        for &button in &seat.buttons_down {
            if let Some(virtual_pointer) = seat.virtual_pointer.as_ref() {
                virtual_pointer.button(0, button, ButtonState::Released);
                virtual_pointer.frame();
            }
        }
    }
    dispatcher.as_source_mut().queue().flush()?;
    Ok(())
}

delegate_noop!(App: ignore WlShm);
delegate_noop!(App: ignore WlShmPool);
delegate_noop!(App: ignore WlCompositor);
delegate_noop!(App: ignore WlRegion);
delegate_noop!(App: ignore ZwlrLayerShellV1);
delegate_noop!(App: ignore ZwlrVirtualPointerV1);
delegate_noop!(App: ignore ZwlrVirtualPointerManagerV1);
delegate_noop!(App: ignore ZxdgOutputManagerV1);

impl Dispatch<WlRegistry, GlobalListContents> for App {
    fn event(
        _state: &mut Self,
        _proxy: &WlRegistry,
        _event: wl_registry::Event,
        _data: &GlobalListContents,
        _conn: &Connection,
        _qhandle: &QueueHandle<Self>,
    ) {
        use wl_registry::Event;
        match _event {
            Event::Global {
                name: _,
                interface: _,
                version: _,
            } => {}
            Event::GlobalRemove { name: _ } => {}
            _ => {}
        }
    }
}

impl Dispatch<WlSeat, SeatId> for App {
    fn event(
        state: &mut Self,
        proxy: &WlSeat,
        event: wl_seat::Event,
        &data: &SeatId,
        _conn: &Connection,
        qhandle: &QueueHandle<Self>,
    ) {
        use wl_seat::Event;
        let this = &mut state.seats[data];
        match event {
            Event::Capabilities { capabilities } => {
                let WEnum::Value(v) = capabilities else {
                    return;
                };
                if v.contains(Capability::Keyboard) {
                    this.keyboard = Some(proxy.get_keyboard(qhandle, data));
                }
            }
            Event::Name { name: _ } => {}
            _ => {}
        }
    }
}

impl Dispatch<WlKeyboard, SeatId> for App {
    fn event(
        state: &mut Self,
        _proxy: &WlKeyboard,
        event: wl_keyboard::Event,
        &data: &SeatId,
        _conn: &Connection,
        qhandle: &QueueHandle<Self>,
    ) {
        use wl_keyboard::Event;
        let this = &mut state.seats[data];
        match event {
            Event::Keymap {
                format: WEnum::Value(KeymapFormat::XkbV1),
                fd,
                size,
            } => {
                let keymap = unsafe {
                    xkb::Keymap::new_from_fd(
                        &this.xkb,
                        fd.into_raw_fd(),
                        size as usize,
                        xkb::KEYMAP_FORMAT_TEXT_V1,
                        xkb::COMPILE_NO_FLAGS,
                    )
                }
                .ok()
                .flatten();
                if let Some(keymap) = keymap.as_ref() {
                    this.xkb_state = Some(xkb::State::new(keymap));
                    (this.mod_indices, this.specialized_bindings) =
                        specialize_bindings(keymap, &state.config);
                }
            }
            Event::Enter {
                serial: _,
                surface: _,
                keys: _,
            } => {}
            Event::Leave {
                serial: _,
                surface: _,
            } => {}
            Event::Key {
                serial: _,
                time: _,
                key,
                state: WEnum::Value(KeyState::Pressed),
            } => {
                handle_key_pressed(state, key, data, qhandle);
            }
            Event::Modifiers {
                serial: _,
                mods_depressed,
                mods_latched,
                mods_locked,
                group,
            } => {
                let state = this.xkb_state.as_mut().unwrap();
                state.update_mask(mods_depressed, mods_latched, mods_locked, 0, 0, group);
            }
            Event::RepeatInfo { rate: _, delay: _ } => {}
            _ => {}
        }
    }
}

impl Dispatch<WlOutput, OutputId> for App {
    fn event(
        state: &mut Self,
        _proxy: &WlOutput,
        event: wl_output::Event,
        &data: &OutputId,
        _conn: &Connection,
        _qhandle: &QueueHandle<Self>,
    ) {
        use wl_output::Event;
        let this = &mut state.outputs[data];
        match event {
            Event::Geometry { .. } => {}
            Event::Mode { .. } => {}
            Event::Done => {
                this.state.commit();
            }
            Event::Scale { factor } => {
                this.state.pending.integer_scale =
                    u32::try_from(factor).expect("negative scale factor");
            }
            Event::Name { .. } => {}
            Event::Description { .. } => {}
            _ => {}
        }
    }
}

impl Dispatch<ZxdgOutputV1, OutputId> for App {
    fn event(
        state: &mut Self,
        _proxy: &ZxdgOutputV1,
        event: zxdg_output_v1::Event,
        &data: &OutputId,
        _conn: &Connection,
        _qhandle: &QueueHandle<Self>,
    ) {
        use zxdg_output_v1::Event;
        let this = &mut state.outputs[data];
        match event {
            Event::LogicalPosition { x, y } => {
                this.state.pending.logical_x = x;
                this.state.pending.logical_y = y;
            }
            Event::LogicalSize { width, height } => {
                this.state.pending.logical_width = width;
                this.state.pending.logical_height = height;
            }
            Event::Done => {
                // ignored; see spec
            }
            Event::Name { .. } => {}
            Event::Description { .. } => {}
            _ => {}
        }
    }
}

impl Dispatch<WlSurface, ()> for App {
    fn event(
        _state: &mut Self,
        _proxy: &WlSurface,
        event: wl_surface::Event,
        &(): &(),
        _conn: &Connection,
        _qhandle: &QueueHandle<Self>,
    ) {
        use wl_surface::Event;
        match event {
            Event::Enter { output: _ } => {}
            Event::Leave { output: _ } => {}
            _ => {}
        }
    }
}

impl Dispatch<ZwlrLayerSurfaceV1, OutputId> for App {
    fn event(
        state: &mut Self,
        proxy: &ZwlrLayerSurfaceV1,
        event: zwlr_layer_surface_v1::Event,
        &output_id: &OutputId,
        _conn: &Connection,
        qhandle: &QueueHandle<Self>,
    ) {
        use zwlr_layer_surface_v1::Event;
        let output = &mut state.outputs[output_id];
        match event {
            Event::Configure {
                serial,
                width,
                height,
            } => {
                let surface = output.surface.as_mut().unwrap();
                proxy.ack_configure(serial);
                proxy.set_size(width, height);
                surface.width = width;
                surface.height = height;
                draw(
                    &state.globals,
                    &mut state.buffers,
                    qhandle,
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
            Event::Closed => {
                output.surface = None;
            }
            _ => {}
        }
    }
}

impl Dispatch<WlBuffer, BufferId> for App {
    fn event(
        state: &mut Self,
        wl_buffer: &WlBuffer,
        event: wl_buffer::Event,
        &buffer_id: &BufferId,
        _conn: &Connection,
        _qhandle: &QueueHandle<Self>,
    ) {
        use wayland_client::protocol::wl_buffer::Event;
        let this = &mut state.buffers[buffer_id];
        match event {
            Event::Release => {
                let wl_shm_pool = this.pool.as_ref().unwrap();
                wl_shm_pool.destroy();
                wl_buffer.destroy();
                state.buffers.remove(buffer_id);
            }
            _ => {}
        }
    }
}
