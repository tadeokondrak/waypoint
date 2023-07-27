#![allow(clippy::single_match, clippy::match_single_binding)]

mod config;
mod region;
mod scfg;

use crate::config::{specialize_bindings, Cmd, Config, Direction};
use crate::region::Region;
use anyhow::{Context as _, Result};
use bytemuck::{Pod, Zeroable};
use handy::typed::{TypedHandle, TypedHandleMap};
use memmap2::{MmapMut, MmapOptions};
use std::{
    collections::{HashMap, HashSet},
    io::Write,
    os::fd::{AsRawFd, IntoRawFd},
};
use tiny_skia::{Color, Paint, PathBuilder, Shader, Stroke, Transform};
use wayland_client::{
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
    will_quit: bool,
    _conn: Connection,
    globals: Globals,
    seats: TypedHandleMap<Seat>,
    outputs: TypedHandleMap<Output>,
    buffers: TypedHandleMap<Buffer>,
    surface: Option<Surface>,
    config: Config,
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
    layer_shell: ZwlrLayerShellV1,
    virtual_pointer_manager: ZwlrVirtualPointerManagerV1,
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
    wl_output: Option<WlOutput>,
    state: DoubleBuffered<OutputState>,
}

#[derive(Default, Clone)]
struct OutputState {
    scale: u32,
    width: u32,
    height: u32,
}

#[derive(Default)]
struct Surface {
    output: OutputId,
    wl_surface: Option<WlSurface>,
    layer_surface: Option<ZwlrLayerSurfaceV1>,
    width: u32,
    height: u32,
    region: Region,
    region_history: Vec<Region>,
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

macro_rules! empty_dispatch {
    ($($t:ty),*) => {
        $(
            impl Dispatch<$t, ()> for App {
                fn event(
                    _: &mut Self,
                    _: &$t,
                    _: <$t as wayland_client::Proxy>::Event,
                    _: &(),
                    _: &Connection,
                    _: &QueueHandle<Self>,
                ) {
                }
            }
        )*
    };
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
    fn scaled_region(&self) -> Region {
        let current = self.state.current.as_ref().unwrap();
        Region {
            x: 0,
            y: 0,
            width: current.width,
            height: current.height,
        }
        .inverse_scale(current.scale)
    }
}

fn handle_key_pressed(state: &mut App, key: u32, seat_id: SeatId, qhandle: &QueueHandle<App>) {
    fn update(surface: &mut Surface, output: &mut Output, cut: fn(Region) -> Region) {
        surface.region_history.push(surface.region);
        let bounds = output.scaled_region();
        let new_region = cut(surface.region);
        if bounds.contains_region(&new_region) {
            surface.region = new_region;
        }
    }

    let Some(surface) = state.surface.as_mut() else {
        return;
    };
    let seat = &mut state.seats[seat_id];
    let output = &mut state.outputs[surface.output];

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
                state.will_quit = true;
            }
            Cmd::Undo => {
                if let Some(region) = surface.region_history.pop() {
                    surface.region = region;
                }
            }
            Cmd::Cut(dir) => update(
                surface,
                output,
                match dir {
                    Direction::Up => Region::cut_up,
                    Direction::Down => Region::cut_down,
                    Direction::Left => Region::cut_left,
                    Direction::Right => Region::cut_right,
                },
            ),
            Cmd::Move(dir) => update(
                surface,
                output,
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
                state.will_quit = true;
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

    draw(
        &state.globals,
        &mut state.buffers,
        qhandle,
        surface.width,
        surface.height,
        output.state.current.as_ref().unwrap().scale,
        surface,
    )
    .unwrap();

    let virtual_pointer = &seat.virtual_pointer.as_ref().unwrap();
    virtual_pointer.motion_absolute(
        0,
        surface.region.center().x,
        surface.region.center().y,
        surface.width,
        surface.height,
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

fn draw(
    globals: &Globals,
    buffers: &mut TypedHandleMap<Buffer>,
    qhandle: &QueueHandle<App>,
    width: u32,
    height: u32,
    scale: u32,
    this: &mut Surface,
) -> Result<()> {
    let wl_surface = this.wl_surface.as_ref().unwrap();
    let buffer_data = make_buffer(
        globals,
        buffers,
        qhandle,
        i32::try_from(width * scale).unwrap(),
        i32::try_from(height * scale).unwrap(),
        i32::try_from(width * scale * 4).unwrap(),
        Format::Argb8888,
    )?;
    let buffer = &mut buffers[buffer_data];
    let mut pixmap = tiny_skia::PixmapMut::from_bytes(
        buffer.mmap.as_deref_mut().unwrap(),
        width * scale,
        height * scale,
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
        this.region,
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
    let pool = globals
        .wl_shm
        .create_pool(memfd.as_raw_fd(), len_i32, qhandle, ());
    let wl_buffer = pool.create_buffer(0, width, height, stride, format, qhandle, buffer_id);
    let mmap = unsafe { MmapOptions::new().len(len_usize).map_mut(memfd.as_file())? };
    this.pool = Some(pool);
    this.wl_buffer = Some(wl_buffer);
    this.mmap = Some(mmap);
    Ok(buffer_id)
}

fn main() -> Result<()> {
    let conn = Connection::connect_to_env()?;
    let (global_list, mut queue) = registry_queue_init(&conn)?;
    let qhandle = queue.handle();
    let mut app = App {
        will_quit: false,
        _conn: conn,
        globals: Globals {
            wl_shm: global_list
                .bind(&qhandle, 1..=1, ())
                .context("compositor doesn't support wl_shm")?,
            wl_compositor: global_list
                .bind(&qhandle, 4..=4, ())
                .context("compositor doesn't support wl_compositor")?,
            layer_shell: global_list
                .bind(&qhandle, 1..=1, ())
                .context("compositor doesn't support zwlr_layer_shell_v1")?,
            virtual_pointer_manager: global_list
                .bind(&qhandle, 1..=1, ())
                .context("compositor doesn't support zwlr_virtual_pointer_manager_v1")?,
        },
        seats: TypedHandleMap::new(),
        outputs: TypedHandleMap::new(),
        buffers: TypedHandleMap::new(),
        surface: None,
        config: Config::load()?,
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
                    let virtual_pointer_manager = &app.globals.virtual_pointer_manager;
                    let virtual_pointer = virtual_pointer_manager.create_virtual_pointer(
                        Some(&wl_seat),
                        &qhandle,
                        (),
                    );
                    seat.virtual_pointer = Some(virtual_pointer);
                    seat.wl_seat = Some(wl_seat);
                }
                "wl_output" => {
                    let output_id = app.outputs.insert(Output::default());
                    let output = &mut app.outputs[output_id];
                    let wl_output = registry.bind(name, version.max(1), &qhandle, output_id);
                    output.wl_output = Some(wl_output);
                }
                _ => {}
            }
        }
    });
    queue.roundtrip(&mut app)?;
    {
        app.surface = Some(Surface::default());
        let this = app.surface.as_mut().unwrap();
        let (output_id, output) = app
            .outputs
            .iter_with_handles()
            .next()
            .context("no outputs")?;
        let wl_output = output.wl_output.as_ref().unwrap();
        let surface = app.globals.wl_compositor.create_surface(&qhandle, ());
        let layer_surface = app.globals.layer_shell.get_layer_surface(
            &surface,
            Some(wl_output),
            Layer::Overlay,
            String::from("waypoint"),
            &qhandle,
            (),
        );
        layer_surface.set_size(0, 0);
        layer_surface.set_anchor(Anchor::Top | Anchor::Bottom | Anchor::Left | Anchor::Right);
        layer_surface.set_exclusive_zone(-1);
        layer_surface.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
        let region = app.globals.wl_compositor.create_region(&qhandle, ());
        surface.set_input_region(Some(&region));
        surface.commit();
        this.output = output_id;
        this.wl_surface = Some(surface);
        this.layer_surface = Some(layer_surface);
    }
    while !app.will_quit {
        queue.blocking_dispatch(&mut app)?;
    }
    for seat in app.seats.iter() {
        for &button in &seat.buttons_down {
            let virtual_pointer = seat.virtual_pointer.as_ref().unwrap();
            virtual_pointer.button(0, button, ButtonState::Released);
            virtual_pointer.frame();
        }
    }
    queue.flush()?;
    Ok(())
}

empty_dispatch![
    WlShm,
    WlShmPool,
    WlCompositor,
    WlRegion,
    ZwlrLayerShellV1,
    ZwlrVirtualPointerV1,
    ZwlrVirtualPointerManagerV1
];

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
            Event::Geometry {
                x: _,
                y: _,
                physical_width: _,
                physical_height: _,
                subpixel: _,
                make: _,
                model: _,
                transform: _,
            } => {}
            Event::Mode {
                flags,
                width,
                height,
                refresh: _,
            } => {
                let WEnum::Value(flags) = flags else {
                    return;
                };
                if flags.contains(wl_output::Mode::Current) {
                    this.state.pending.width = u32::try_from(width).expect("negative output width");
                    this.state.pending.height =
                        u32::try_from(height).expect("negative output height");
                }
            }
            Event::Done => {
                this.state.commit();
            }
            Event::Scale { factor } => {
                this.state.pending.scale = u32::try_from(factor).expect("negative scale factor");
            }
            Event::Name { name: _ } => {}
            Event::Description { description: _ } => {}
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

impl Dispatch<ZwlrLayerSurfaceV1, ()> for App {
    fn event(
        state: &mut Self,
        proxy: &ZwlrLayerSurfaceV1,
        event: zwlr_layer_surface_v1::Event,
        &(): &(),
        _conn: &Connection,
        qhandle: &QueueHandle<Self>,
    ) {
        use zwlr_layer_surface_v1::Event;
        let this = &mut state.surface.as_mut().unwrap();
        let output = &mut state.outputs[this.output];
        match event {
            Event::Configure {
                serial,
                width,
                height,
            } => {
                proxy.ack_configure(serial);
                proxy.set_size(width, height);
                this.width = width;
                this.height = height;
                this.region = Region {
                    x: 0,
                    y: 0,
                    width,
                    height,
                };
                draw(
                    &state.globals,
                    &mut state.buffers,
                    qhandle,
                    width,
                    height,
                    output.state.current.as_ref().unwrap().scale,
                    this,
                )
                .unwrap();
            }
            Event::Closed => {
                state.surface = None;
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
