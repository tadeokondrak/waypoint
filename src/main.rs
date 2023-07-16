#![allow(clippy::single_match)]

use anyhow::{Context as _, Result};
use handy::typed::{TypedHandle, TypedHandleMap};
use memmap2::{MmapMut, MmapOptions};
use std::{
    cell::OnceCell,
    io::Write,
    os::fd::{AsRawFd, IntoRawFd},
};
use tiny_skia::{Color, Paint, PathBuilder, Shader, Stroke, Transform};
use wayland_client::{
    globals::{registry_queue_init, Global, GlobalListContents},
    protocol::{
        wl_buffer::WlBuffer,
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

struct App {
    will_quit: bool,
    _conn: Connection,
    globals: Globals,
    seats: TypedHandleMap<Seat>,
    outputs: TypedHandleMap<Output>,
    surfaces: TypedHandleMap<Surface>,
    buffers: TypedHandleMap<Buffer>,
}

type SeatId = TypedHandle<Seat>;
type OutputId = TypedHandle<Output>;
type SurfaceId = TypedHandle<Surface>;
type BufferId = TypedHandle<Buffer>;

struct Globals {
    wl_shm: WlShm,
    wl_compositor: WlCompositor,
    layer_shell: ZwlrLayerShellV1,
    virtual_pointer_manager: ZwlrVirtualPointerManagerV1,
}

#[derive(Default)]
struct Region {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

struct Seat {
    name: Option<String>,
    wl_seat: Option<WlSeat>,
    xkb: xkb::Context,
    xkb_state: Option<xkb::State>,
}

impl Seat {
    fn default() -> Seat {
        Seat {
            name: None,
            wl_seat: None,
            xkb: xkb::Context::new(xkb::CONTEXT_NO_FLAGS),
            xkb_state: None,
        }
    }
}

#[derive(Default)]
struct Output {
    name: Option<String>,
    wl_output: Option<WlOutput>,
}

#[derive(Default)]
struct Surface {
    wl_surface: OnceCell<WlSurface>,
    layer_surface: OnceCell<ZwlrLayerSurfaceV1>,
    virtual_pointer: OnceCell<ZwlrVirtualPointerV1>,
    width: u32,
    height: u32,
    region: Region,
}

#[derive(Default)]
struct Buffer {
    pool: Option<WlShmPool>,
    wl_buffer: Option<WlBuffer>,
    mmap: Option<MmapMut>,
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

empty_dispatch![
    WlShm,
    WlCompositor,
    WlRegion,
    ZwlrLayerShellV1,
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
        _proxy: &WlSeat,
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
                    _proxy.get_keyboard(qhandle, data);
                }
            }
            Event::Name { name } => {
                this.name = Some(name);
            }
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
            Event::Keymap { format, fd, size } => match format {
                WEnum::Value(KeymapFormat::XkbV1) => {
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
                    }
                }
                WEnum::Value(_) | WEnum::Unknown(_) => {}
            },
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
                state: key_state,
            } => {
                const BTN_LEFT: u32 = 0x110;
                const BTN_RIGHT: u32 = 0x111;
                let Some(xkb_state) = this.xkb_state.as_mut() else {
                    return;
                };
                if key_state != WEnum::Value(KeyState::Pressed) {
                    return;
                }
                for sym in xkb_state.key_get_syms(key + 8).iter().copied() {
                    let mut should_click = None;
                    match xkb::keysym_get_name(sym).as_str() {
                        "Escape" => {
                            state.will_quit = true;
                        }
                        "h" | "Left" => {
                            for surface in state.surfaces.iter_mut() {
                                surface.region.width /= 2;
                            }
                        }
                        "j" | "Down" => {
                            for surface in state.surfaces.iter_mut() {
                                surface.region.height /= 2;
                                surface.region.y += surface.region.height;
                            }
                        }
                        "k" | "Up" => {
                            for surface in state.surfaces.iter_mut() {
                                surface.region.height /= 2;
                            }
                        }
                        "l" | "Right" => {
                            for surface in state.surfaces.iter_mut() {
                                surface.region.width /= 2;
                                surface.region.x += surface.region.width;
                            }
                        }
                        "H" => {
                            for surface in state.surfaces.iter_mut() {
                                if let Some(x) = surface.region.x.checked_sub(surface.region.width)
                                {
                                    surface.region.x = x;
                                }
                            }
                        }
                        "J" => {
                            for surface in state.surfaces.iter_mut() {
                                if let Some(y) = surface.region.y.checked_add(surface.region.height)
                                {
                                    surface.region.y = y;
                                }
                            }
                        }
                        "K" => {
                            for surface in state.surfaces.iter_mut() {
                                if let Some(y) = surface.region.y.checked_sub(surface.region.height)
                                {
                                    surface.region.y = y;
                                }
                            }
                        }
                        "L" => {
                            for surface in state.surfaces.iter_mut() {
                                if let Some(x) = surface.region.x.checked_add(surface.region.width)
                                {
                                    surface.region.x = x;
                                }
                            }
                        }
                        "Return" => {
                            should_click = Some(
                                if xkb_state.mod_name_is_active(
                                    xkb::MOD_NAME_SHIFT,
                                    xkb::STATE_MODS_EFFECTIVE,
                                ) {
                                    BTN_RIGHT
                                } else {
                                    BTN_LEFT
                                },
                            );
                            state.will_quit = true;
                        }
                        _ => {}
                    }
                    for surface in state.surfaces.iter_mut() {
                        draw(
                            &state.globals,
                            &mut state.buffers,
                            qhandle,
                            surface.width,
                            surface.height,
                            surface,
                        );
                        let virtual_pointer = &surface.virtual_pointer.get().unwrap();
                        virtual_pointer.motion_absolute(
                            0,
                            surface.region.x + surface.region.width / 2,
                            surface.region.y + surface.region.height / 2,
                            surface.width,
                            surface.height,
                        );
                        virtual_pointer.frame();
                        if let Some(btn) = should_click {
                            virtual_pointer.button(0, btn, ButtonState::Pressed);
                            virtual_pointer.frame();
                            virtual_pointer.button(0, btn, ButtonState::Released);
                            virtual_pointer.frame();
                        }
                    }
                    if state.will_quit {
                        break;
                    }
                }
            }
            Event::Modifiers {
                serial: _,
                mods_depressed,
                mods_latched,
                mods_locked,
                group,
            } => {
                this.xkb_state.as_mut().unwrap().update_mask(
                    mods_depressed,
                    mods_latched,
                    mods_locked,
                    0,
                    0,
                    group,
                );
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
                flags: _,
                width: _,
                height: _,
                refresh: _,
            } => {}
            Event::Done => {}
            Event::Scale { factor: _ } => {}
            Event::Name { name } => {
                this.name = Some(name);
            }
            Event::Description { description: _ } => {}
            _ => {}
        }
    }
}

impl Dispatch<WlSurface, SurfaceId> for App {
    fn event(
        state: &mut Self,
        _proxy: &WlSurface,
        event: wl_surface::Event,
        &data: &SurfaceId,
        _conn: &Connection,
        _qhandle: &QueueHandle<Self>,
    ) {
        use wl_surface::Event;
        let _this = &mut state.surfaces[data];
        match event {
            Event::Enter { output: _ } => {}
            Event::Leave { output: _ } => {}
            _ => {}
        }
    }
}

impl Dispatch<ZwlrLayerSurfaceV1, SurfaceId> for App {
    fn event(
        state: &mut Self,
        proxy: &ZwlrLayerSurfaceV1,
        event: zwlr_layer_surface_v1::Event,
        &data: &SurfaceId,
        _conn: &Connection,
        qhandle: &QueueHandle<Self>,
    ) {
        use zwlr_layer_surface_v1::Event;
        let this = &mut state.surfaces[data];
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
                    this,
                );
            }
            Event::Closed => {
                state.surfaces.remove(data);
            }
            _ => {}
        }
    }
}

impl Dispatch<ZwlrVirtualPointerV1, SurfaceId> for App {
    fn event(
        _state: &mut Self,
        _proxy: &ZwlrVirtualPointerV1,
        _event: <ZwlrVirtualPointerV1 as wayland_client::Proxy>::Event,
        _data: &SurfaceId,
        _conn: &Connection,
        _qhandle: &QueueHandle<Self>,
    ) {
        todo!()
    }
}

fn draw(
    globals: &Globals,
    buffers: &mut TypedHandleMap<Buffer>,
    qhandle: &QueueHandle<App>,
    width: u32,
    height: u32,
    this: &mut Surface,
) {
    let wl_surface = this.wl_surface.get().unwrap();
    let buffer_data = make_buffer(
        globals,
        buffers,
        qhandle,
        width as i32,
        height as i32,
        (width * 4) as i32,
        Format::Argb8888,
    )
    .unwrap();
    let buffer = &mut buffers[buffer_data];
    let mut pixmap =
        tiny_skia::PixmapMut::from_bytes(buffer.mmap.as_deref_mut().unwrap(), width, height)
            .unwrap();
    {
        {
            let mut path = PathBuilder::new();
            path.move_to(this.region.x as f32, this.region.y as f32);
            path.line_to(
                (this.region.x + this.region.width) as f32,
                this.region.y as f32,
            );
            path.line_to(
                (this.region.x + this.region.width) as f32,
                (this.region.y + this.region.height) as f32,
            );
            path.line_to(
                this.region.x as f32,
                (this.region.y + this.region.height) as f32,
            );
            path.close();
            let path = path.finish().unwrap();
            let paint = Paint {
                shader: Shader::SolidColor(Color::WHITE),
                ..Default::default()
            };
            let transform = Transform::default();
            let stroke = Stroke {
                width: 2.0,
                ..Default::default()
            };
            _ = pixmap.stroke_path(&path, &paint, &stroke, transform, None);

            let mut path = path.clear();
            path.move_to(
                this.region.x as f32,
                (this.region.y + this.region.height / 2) as f32,
            );
            path.line_to(
                (this.region.x + this.region.width) as f32,
                (this.region.y + this.region.height / 2) as f32,
            );
            path.close();
            path.move_to(
                (this.region.x + this.region.width / 2) as f32,
                this.region.y as f32,
            );
            path.line_to(
                (this.region.x + this.region.width / 2) as f32,
                (this.region.y + this.region.height) as f32,
            );
            let path = path.finish().unwrap();
            let mut paint = Paint::default();
            let mut color = Color::WHITE;
            color.apply_opacity(0.25);
            paint.shader = Shader::SolidColor(color);
            let transform = Transform::default();
            let stroke = Stroke {
                width: 2.0,
                ..Default::default()
            };
            _ = pixmap.stroke_path(&path, &paint, &stroke, transform, None);
        }
    }
    wl_surface.attach(Some(buffer.wl_buffer.as_ref().unwrap()), 0, 0);
    wl_surface.damage_buffer(0, 0, i32::MAX, i32::MAX);
    wl_surface.commit();
}

impl Dispatch<WlShmPool, BufferId> for App {
    fn event(
        _state: &mut Self,
        _proxy: &WlShmPool,
        _event: <WlShmPool as wayland_client::Proxy>::Event,
        _data: &BufferId,
        _conn: &Connection,
        _qhandle: &QueueHandle<Self>,
    ) {
        todo!()
    }
}

impl Dispatch<WlBuffer, BufferId> for App {
    fn event(
        _state: &mut Self,
        _proxy: &WlBuffer,
        event: <WlBuffer as wayland_client::Proxy>::Event,
        _data: &BufferId,
        _conn: &Connection,
        _qhandle: &QueueHandle<Self>,
    ) {
        use wayland_client::protocol::wl_buffer::Event;
        match event {
            Event::Release => {}
            _ => {}
        }
    }
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
    let len = stride * height;
    memfd.as_file().write_all(&vec![0u8; len as usize])?;
    let pool = globals
        .wl_shm
        .create_pool(memfd.as_raw_fd(), len, qhandle, buffer_id);
    let wl_buffer = pool.create_buffer(0, width, height, stride, format, qhandle, buffer_id);
    let mmap = unsafe {
        MmapOptions::new()
            .len(usize::try_from(len).unwrap())
            .map_mut(memfd.as_file())?
    };
    this.pool = Some(pool);
    this.wl_buffer = Some(wl_buffer);
    this.mmap = Some(mmap);
    Ok(buffer_id)
}

fn main() -> Result<()> {
    let conn = Connection::connect_to_env()?;
    let (global_list, mut queue) = registry_queue_init::<App>(&conn)?;
    let qhandle = queue.handle();
    let mut app = App {
        will_quit: false,
        _conn: conn,
        globals: Globals {
            wl_shm: global_list
                .bind::<WlShm, App, ()>(&qhandle, 1..=1, ())
                .context("compositor doesn't support wl_shm")?,
            wl_compositor: global_list
                .bind::<WlCompositor, App, ()>(&qhandle, 4..=4, ())
                .context("compositor doesn't support wl_compositor")?,
            layer_shell: global_list
                .bind::<ZwlrLayerShellV1, App, ()>(&qhandle, 1..=1, ())
                .context("compositor doesn't support zwlr_layer_shell_v1")?,
            virtual_pointer_manager: global_list
                .bind::<ZwlrVirtualPointerManagerV1, App, ()>(&qhandle, 1..=1, ())
                .context("compositor doesn't support zwlr_virtual_pointer_manager_v1")?,
        },
        seats: TypedHandleMap::new(),
        outputs: TypedHandleMap::new(),
        surfaces: TypedHandleMap::new(),
        buffers: TypedHandleMap::new(),
    };
    global_list.contents().with_list(|list| {
        for &Global {
            name,
            ref interface,
            version,
        } in list
        {
            match interface.as_str() {
                "wl_seat" => {
                    let seat_id = app.seats.insert(Seat::default());
                    let wl_seat = global_list.registry().bind::<WlSeat, SeatId, App>(
                        name,
                        version.max(1),
                        &qhandle,
                        seat_id,
                    );
                    app.seats[seat_id].wl_seat = Some(wl_seat);
                }
                "wl_output" => {
                    let output_id = app.outputs.insert(Output::default());
                    let wl_output = global_list.registry().bind::<WlOutput, OutputId, App>(
                        name,
                        version.max(1),
                        &qhandle,
                        output_id,
                    );
                    app.outputs[output_id].wl_output = Some(wl_output);
                }
                _ => {}
            }
        }
    });
    queue.roundtrip(&mut app)?;
    {
        let surface_id = app.surfaces.insert(Surface::default());
        let this = &mut app.surfaces[surface_id];
        let output = app.outputs.iter().next().context("no outputs")?;
        let surface = app
            .globals
            .wl_compositor
            .create_surface(&qhandle, surface_id);
        let region = app.globals.wl_compositor.create_region(&qhandle, ());
        surface.set_input_region(Some(&region));
        let layer_surface = app.globals.layer_shell.get_layer_surface(
            &surface,
            Some(output.wl_output.as_ref().unwrap()),
            Layer::Overlay,
            String::from("waypoint"),
            &qhandle,
            surface_id,
        );
        layer_surface.set_size(0, 0);
        layer_surface.set_anchor(Anchor::Top | Anchor::Bottom | Anchor::Left | Anchor::Right);
        layer_surface.set_exclusive_zone(-1);
        layer_surface.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
        surface.commit();
        let virtual_pointer = app
            .globals
            .virtual_pointer_manager
            .create_virtual_pointer(None, &qhandle, surface_id);
        this.wl_surface.set(surface).unwrap();
        this.layer_surface.set(layer_surface).unwrap();
        this.virtual_pointer.set(virtual_pointer).unwrap();
    }
    while !app.will_quit {
        queue.blocking_dispatch(&mut app)?;
    }
    queue.flush()?;
    Ok(())
}
