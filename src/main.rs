#![allow(clippy::single_match, clippy::match_single_binding)]

mod scfg;

use anyhow::{bail, ensure, Context as _, Result};
use bitflags::bitflags;
use handy::typed::{TypedHandle, TypedHandleMap};
use memmap2::{MmapMut, MmapOptions};
use std::{
    cmp::Ordering,
    collections::{HashMap, HashSet},
    io::Write,
    ops::Index,
    os::fd::{AsRawFd, IntoRawFd},
    path::PathBuf,
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
        wl_shm_pool::{self, WlShmPool},
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
        zwlr_virtual_pointer_v1::{self, ZwlrVirtualPointerV1},
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

#[derive(Clone, Copy, Debug)]
enum Direction {
    Up,
    Down,
    Left,
    Right,
}

#[derive(Clone, Copy, Debug)]
enum Button {
    Left,
    Right,
    Middle,
}

#[derive(Clone, Copy, Debug)]
enum Cmd {
    Quit,
    Undo,
    Click(Button),
    Press(Button),
    Release(Button),
    Cut(Direction),
    Move(Direction),
}

bitflags! {
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    struct Mods: u8 {
        const SHIFT = 1 << 0;
        const CAPS = 1 << 1;
        const CTRL = 1 << 2;
        const ALT = 1 << 3;
        const NUM = 1 << 4;
        const MOD3 = 1 << 5;
        const LOGO = 1 << 6;
        const MOD5 = 1 << 7;
    }
}

#[derive(Default)]
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

struct Config {
    bindings: HashMap<(Mods, xkb::Keysym), Cmd>,
}

struct Globals {
    wl_shm: WlShm,
    wl_compositor: WlCompositor,
    layer_shell: ZwlrLayerShellV1,
    virtual_pointer_manager: ZwlrVirtualPointerManagerV1,
}

#[derive(Default, Clone, Copy)]
struct Point {
    x: u32,
    y: u32,
}

#[derive(Default, Clone, Copy)]
struct Region {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

struct Seat {
    wl_seat: Option<WlSeat>,
    virtual_pointer: Option<ZwlrVirtualPointerV1>,
    xkb: xkb::Context,
    xkb_state: Option<xkb::State>,
    keyboard: Option<WlKeyboard>,
    buttons_down: HashSet<u32>,
    mod_indices: ModIndices,
    specialized_bindings: HashMap<(xkb::ModMask, xkb::Keycode), Cmd>,
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

impl<T: Clone> DoubleBuffered<T> {
    fn commit(&mut self) {
        match self.current.as_mut() {
            Some(current) => current.clone_from(&self.pending),
            None => self.current = Some(self.pending.clone()),
        }
    }
}

impl Mods {
    fn one_from_str(s: &str) -> Option<Mods> {
        fn strcasecmp(left: &str, right: &str) -> Ordering {
            left.bytes()
                .zip(right.bytes())
                .map(|(l, r)| l.to_ascii_lowercase().cmp(&r.to_ascii_lowercase()))
                .find(|&o| o != Ordering::Equal)
                .unwrap_or_else(|| left.len().cmp(&right.len()))
        }

        let pairs_sorted = [
            ("alt", Mods::ALT),
            ("caps", Mods::CAPS),
            ("ctrl", Mods::CTRL),
            ("logo", Mods::LOGO),
            ("mod3", Mods::MOD3),
            ("mod5", Mods::MOD5),
            ("num", Mods::NUM),
            ("shift", Mods::SHIFT),
        ];

        let i = pairs_sorted
            .binary_search_by(|&(name, _)| strcasecmp(name, s))
            .ok()?;

        let (_, modifier) = pairs_sorted[i];

        Some(modifier)
    }
}

impl Seat {
    fn default() -> Seat {
        Seat {
            wl_seat: None,
            virtual_pointer: None,
            xkb: xkb::Context::new(xkb::CONTEXT_NO_FLAGS),
            xkb_state: None,
            keyboard: None,
            buttons_down: HashSet::new(),
            mod_indices: ModIndices::default(),
            specialized_bindings: HashMap::new(),
        }
    }
}

impl Cmd {
    fn from_kebab_case(s: &str) -> Option<Cmd> {
        match s {
            "quit" => Some(Cmd::Quit),
            "undo" => Some(Cmd::Undo),
            "left-click" => Some(Cmd::Click(Button::Left)),
            "right-click" => Some(Cmd::Click(Button::Right)),
            "middle-click" => Some(Cmd::Click(Button::Middle)),
            "left-press" => Some(Cmd::Press(Button::Left)),
            "right-press" => Some(Cmd::Press(Button::Right)),
            "middle-press" => Some(Cmd::Press(Button::Middle)),
            "left-release" => Some(Cmd::Release(Button::Left)),
            "right-release" => Some(Cmd::Release(Button::Right)),
            "middle-release" => Some(Cmd::Release(Button::Middle)),
            "cut-up" => Some(Cmd::Cut(Direction::Up)),
            "cut-down" => Some(Cmd::Cut(Direction::Down)),
            "cut-left" => Some(Cmd::Cut(Direction::Left)),
            "cut-right" => Some(Cmd::Cut(Direction::Right)),
            "move-up" => Some(Cmd::Move(Direction::Up)),
            "move-down" => Some(Cmd::Move(Direction::Down)),
            "move-left" => Some(Cmd::Move(Direction::Left)),
            "move-right" => Some(Cmd::Move(Direction::Right)),
            _ => None,
        }
    }
}

impl Config {
    fn load() -> Result<Config> {
        let text = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                let home = PathBuf::from(std::env::var_os("HOME")?);
                Some(home.join(".config"))
            })
            .map(|path| path.join("waypoint/config"))
            .map(std::fs::read_to_string)
            .and_then(Result::ok)
            .unwrap_or_else(|| include_str!("../default_config").to_owned());
        Config::parse(&text)
    }

    fn parse(s: &str) -> Result<Config> {
        let directives = scfg::parse(s).context("invalid config")?;
        let mut bindings = HashMap::new();
        for directive in &directives {
            match directive.name.as_str() {
                "bindings" => {
                    ensure!(
                        directive.params.is_empty(),
                        "invalid config: line {}: too many parameters to directive 'bindings'",
                        directive.line,
                    );

                    for binding in &directive.children {
                        ensure!(
                            binding.children.is_empty(),
                            "invalid config: line {}: binding should not have block",
                            binding.line,
                        );

                        ensure!(
                            binding.params.len() == 1,
                            "invalid config: line {}: binding should have exactly one parameter",
                            binding.line,
                        );

                        let keys = &binding.name;
                        let cmd = &binding.params[0];

                        let Some(cmd) = Cmd::from_kebab_case(cmd) else {
                            bail!(
                                "invalid config: line {}: invalid command {:?}",
                                binding.line,
                                cmd,
                            );
                        };

                        let mut modifiers = Mods::empty();
                        let mut keysym = None;

                        for element in keys.split('+') {
                            match Mods::one_from_str(element) {
                                Some(modifier) => {
                                    let old_modifiers = modifiers;
                                    modifiers |= modifier;
                                    ensure!(
                                        old_modifiers != modifiers,
                                        "invalid config: line {}: duplicate modifier {:?}",
                                        binding.line,
                                        element,
                                    );
                                }
                                None => {
                                    let parsed_keysym = xkb::keysym_from_name(
                                        element,
                                        xkb::KEYSYM_CASE_INSENSITIVE,
                                    );
                                    ensure!(
                                        parsed_keysym != xkb::KEY_NoSymbol,
                                        "invalid config: line {}: invalid key {:?}",
                                        binding.line,
                                        element,
                                    );
                                    ensure!(
                                        keysym.is_none(),
                                        "invalid config: line {}: too many keys",
                                        binding.line,
                                    );
                                    keysym = Some(parsed_keysym);
                                }
                            }
                        }

                        let keysym = keysym
                            .context(format!("invalid config: line {}: no key", binding.line))?;

                        bindings.insert((modifiers, keysym), cmd);
                    }
                }
                _ => {
                    bail!(
                        "invalid config: line {}, invalid directive {:?}",
                        directive.line,
                        directive.name,
                    );
                }
            }
        }
        Ok(Config { bindings })
    }
}

impl Region {
    fn center(self) -> Point {
        Point {
            x: self.x + self.width / 2,
            y: self.y + self.height / 2,
        }
    }

    fn cut_up(mut self) -> Region {
        self.height /= 2;
        self
    }

    fn cut_down(mut self) -> Region {
        self.height /= 2;
        self.y += self.height;
        self
    }

    fn cut_left(mut self) -> Region {
        self.width /= 2;
        self
    }

    fn cut_right(mut self) -> Region {
        self.width /= 2;
        self.x += self.width;
        self
    }

    fn move_up(mut self) -> Region {
        self.y = self.y.saturating_sub(self.height);
        self
    }

    fn move_down(mut self) -> Region {
        self.y = self.y.saturating_add(self.height);
        self
    }

    fn move_left(mut self) -> Region {
        self.x = self.x.saturating_sub(self.width);
        self
    }

    fn move_right(mut self) -> Region {
        self.x = self.x.saturating_add(self.width);
        self
    }

    fn contains(&self, x: u32, y: u32) -> bool {
        x >= self.x && x < self.x + self.width && y >= self.y && y < self.y + self.height
    }

    fn contains_region(&self, other: &Region) -> bool {
        self.contains(other.x, other.y)
            && self.contains(other.x + other.width - 1, other.y + other.height - 1)
    }

    fn inverse_scale(&self, inverse_scale: u32) -> Region {
        Region {
            x: self.x / inverse_scale,
            y: self.y / inverse_scale,
            width: self.width / inverse_scale,
            height: self.height / inverse_scale,
        }
    }

    fn scale(&self, scale: u32) -> Region {
        Region {
            x: self.x * scale,
            y: self.y * scale,
            width: self.width * scale,
            height: self.height * scale,
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

impl Index<Mods> for ModIndices {
    type Output = xkb::ModIndex;

    fn index(&self, index: Mods) -> &xkb::ModIndex {
        match index {
            Mods::SHIFT => &self.shift,
            Mods::CAPS => &self.caps,
            Mods::CTRL => &self.ctrl,
            Mods::ALT => &self.alt,
            Mods::NUM => &self.num,
            Mods::MOD3 => &self.mod3,
            Mods::LOGO => &self.logo,
            Mods::MOD5 => &self.mod5,
            _ => {
                panic!("can only index by single modifier")
            }
        }
    }
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
                        (this.mod_indices, this.specialized_bindings) =
                            specialize_bindings(keymap, &state.config.bindings);
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
                fn update(surface: &mut Surface, output: &mut Output, cut: fn(Region) -> Region) {
                    surface.region_history.push(surface.region);
                    let bounds = output.scaled_region();
                    let new_region = cut(surface.region);
                    if bounds.contains_region(&new_region) {
                        surface.region = new_region;
                    }
                }

                const BTN_LEFT: u32 = 0x110;
                const BTN_RIGHT: u32 = 0x111;
                const BTN_MIDDLE: u32 = 0x112;

                if key_state != WEnum::Value(KeyState::Pressed) {
                    return;
                }

                let Some(surface) = state.surface.as_mut() else {
                    return;
                };

                let keycode = key + 8;
                let mod_mask = {
                    let Some(xkb_state) = this.xkb_state.as_mut() else {
                        return;
                    };
                    let keymap = xkb_state.get_keymap();
                    let mut mod_mask: xkb::ModMask = 0;
                    for i in 0..keymap.num_mods() {
                        let is_active = xkb_state.mod_index_is_active(i, xkb::STATE_MODS_EFFECTIVE);
                        let _is_consumed = xkb_state.mod_index_is_consumed(key, i);
                        let is_relevant = i != this.mod_indices.caps && i != this.mod_indices.num;
                        if is_active && is_relevant {
                            mod_mask |= 1 << i;
                        }
                    }
                    mod_mask
                };

                let output = &mut state.outputs[surface.output];
                let mut should_press = None;
                let mut should_release = None;
                match this.specialized_bindings.get(&(mod_mask, keycode)) {
                    Some(Cmd::Quit) => {
                        state.will_quit = true;
                    }
                    Some(Cmd::Undo) => {
                        if let Some(region) = surface.region_history.pop() {
                            surface.region = region;
                        }
                    }
                    Some(Cmd::Cut(dir)) => update(
                        surface,
                        output,
                        match dir {
                            Direction::Up => Region::cut_up,
                            Direction::Down => Region::cut_down,
                            Direction::Left => Region::cut_left,
                            Direction::Right => Region::cut_right,
                        },
                    ),
                    Some(Cmd::Move(dir)) => update(
                        surface,
                        output,
                        match dir {
                            Direction::Up => Region::move_up,
                            Direction::Down => Region::move_down,
                            Direction::Left => Region::move_left,
                            Direction::Right => Region::move_right,
                        },
                    ),
                    Some(Cmd::Click(btn)) => {
                        let code = match btn {
                            Button::Left => BTN_LEFT,
                            Button::Right => BTN_RIGHT,
                            Button::Middle => BTN_MIDDLE,
                        };
                        should_press = Some(code);
                        should_release = Some(code);
                        state.will_quit = true;
                    }
                    Some(Cmd::Press(btn)) => {
                        should_press = Some(match btn {
                            Button::Left => BTN_LEFT,
                            Button::Right => BTN_RIGHT,
                            Button::Middle => BTN_MIDDLE,
                        });
                    }
                    Some(Cmd::Release(btn)) => {
                        should_release = Some(match btn {
                            Button::Left => BTN_LEFT,
                            Button::Right => BTN_RIGHT,
                            Button::Middle => BTN_MIDDLE,
                        });
                    }
                    None => {}
                }
                draw(
                    &state.globals,
                    &mut state.buffers,
                    qhandle,
                    surface.width,
                    surface.height,
                    output.state.current.as_ref().unwrap().scale,
                    surface,
                );
                let virtual_pointer = &this.virtual_pointer.as_ref().unwrap();
                virtual_pointer.motion_absolute(
                    0,
                    surface.region.center().x,
                    surface.region.center().y,
                    surface.width,
                    surface.height,
                );
                virtual_pointer.frame();
                if let Some(btn) = should_press {
                    if this.buttons_down.insert(btn) {
                        virtual_pointer.button(0, btn, ButtonState::Pressed);
                        virtual_pointer.frame();
                    }
                }
                if let Some(btn) = should_release {
                    if this.buttons_down.remove(&btn) {
                        virtual_pointer.button(0, btn, ButtonState::Released);
                        virtual_pointer.frame();
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
                );
            }
            Event::Closed => {
                state.surface = None;
            }
            _ => {}
        }
    }
}

impl Dispatch<ZwlrVirtualPointerV1, ()> for App {
    fn event(
        _state: &mut Self,
        _proxy: &ZwlrVirtualPointerV1,
        event: zwlr_virtual_pointer_v1::Event,
        &(): &(),
        _conn: &Connection,
        _qhandle: &QueueHandle<Self>,
    ) {
        match event {
            _ => {}
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
) {
    let wl_surface = this.wl_surface.as_ref().unwrap();
    let buffer_data = make_buffer(
        globals,
        buffers,
        qhandle,
        i32::try_from(width * scale).unwrap(),
        i32::try_from(height * scale).unwrap(),
        i32::try_from(width * scale * 4).unwrap(),
        Format::Argb8888,
    )
    .unwrap();
    let buffer = &mut buffers[buffer_data];
    let mut pixmap = tiny_skia::PixmapMut::from_bytes(
        buffer.mmap.as_deref_mut().unwrap(),
        width * scale,
        height * scale,
    )
    .unwrap();
    {
        let region = this.region.scale(scale);
        let mut path = PathBuilder::new();
        let region_x = region.x as f32;
        let region_y = region.y as f32;
        let region_width = region.width as f32;
        let region_height = region.height as f32;
        path.move_to(region_x, region_y);
        path.line_to(region_x + region_width, region_y);
        path.line_to(region_x + region_width, region_y + region_height);
        path.line_to(region_x, region_y + region_height);
        path.close();
        let path = path.finish().unwrap();
        let paint = Paint {
            shader: Shader::SolidColor(Color::WHITE),
            ..Default::default()
        };
        let transform = Transform::default();
        let stroke = Stroke {
            width: 1.0,
            ..Default::default()
        };
        _ = pixmap.stroke_path(&path, &paint, &stroke, transform, None);

        let mut path = path.clear();
        path.move_to(region_x, region_y + region_height / 2.0);
        path.line_to(region_x + region_width, region_y + region_height / 2.0);
        path.close();
        path.move_to(region_x + region_width / 2.0, region_y);
        path.line_to(region_x + region_width / 2.0, region_y + region_height);
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
    wl_surface.set_buffer_scale(i32::try_from(scale).unwrap());
    wl_surface.attach(Some(buffer.wl_buffer.as_ref().unwrap()), 0, 0);
    wl_surface.damage_buffer(0, 0, i32::MAX, i32::MAX);
    wl_surface.commit();
}

impl Dispatch<WlShmPool, BufferId> for App {
    fn event(
        _state: &mut Self,
        _proxy: &WlShmPool,
        event: wl_shm_pool::Event,
        _data: &BufferId,
        _conn: &Connection,
        _qhandle: &QueueHandle<Self>,
    ) {
        match event {
            _ => {}
        }
    }
}

impl Dispatch<WlBuffer, BufferId> for App {
    fn event(
        state: &mut Self,
        _proxy: &WlBuffer,
        event: wl_buffer::Event,
        &buffer_id: &BufferId,
        _conn: &Connection,
        _qhandle: &QueueHandle<Self>,
    ) {
        use wayland_client::protocol::wl_buffer::Event;
        let this = &mut state.buffers[buffer_id];
        match event {
            Event::Release => {
                this.pool.as_ref().unwrap().destroy();
                this.wl_buffer.as_ref().unwrap().destroy();
                state.buffers.remove(buffer_id);
            }
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

fn specialize_bindings(
    keymap: &xkb::Keymap,
    bindings: &HashMap<(Mods, xkb::Keysym), Cmd>,
) -> (ModIndices, HashMap<(xkb::ModMask, xkb::Keycode), Cmd>) {
    let state = xkb::State::new(keymap);
    let mod_indices = ModIndices {
        shift: keymap.mod_get_index(xkb::MOD_NAME_SHIFT),
        caps: keymap.mod_get_index(xkb::MOD_NAME_CAPS),
        ctrl: keymap.mod_get_index(xkb::MOD_NAME_CTRL),
        alt: keymap.mod_get_index(xkb::MOD_NAME_ALT),
        num: keymap.mod_get_index(xkb::MOD_NAME_NUM),
        mod3: keymap.mod_get_index("Mod3"),
        logo: keymap.mod_get_index(xkb::MOD_NAME_LOGO),
        mod5: keymap.mod_get_index("Mod5"),
    };

    let specialized = bindings
        .iter()
        .flat_map(|(&(modifiers, keysym), &cmd)| {
            let mut keycodes = Vec::new();

            keymap.key_for_each(|_, keycode| {
                let got_keysym = state.key_get_one_sym(keycode);
                if got_keysym != xkb::KEY_NoSymbol && got_keysym == keysym {
                    keycodes.push(keycode);
                }
            });

            let mod_mask: xkb::ModMask = modifiers
                .into_iter()
                .map(|modifier| 1 << mod_indices[modifier])
                .fold(0, |acc, it| acc | it);

            keycodes
                .into_iter()
                .map(move |keycode| ((mod_mask, keycode), cmd))
        })
        .collect();

    (mod_indices, specialized)
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
            match interface.as_str() {
                "wl_seat" => {
                    let seat_id = app.seats.insert(Seat::default());
                    let wl_seat = global_list.registry().bind::<WlSeat, SeatId, App>(
                        name,
                        version.max(1),
                        &qhandle,
                        seat_id,
                    );
                    app.seats[seat_id].virtual_pointer =
                        Some(app.globals.virtual_pointer_manager.create_virtual_pointer(
                            Some(&wl_seat),
                            &qhandle,
                            (),
                        ));
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
        app.surface = Some(Surface::default());
        let this = app.surface.as_mut().unwrap();
        let (output_id, output) = app
            .outputs
            .iter_with_handles()
            .next()
            .context("no outputs")?;
        let surface = app.globals.wl_compositor.create_surface(&qhandle, ());
        let region = app.globals.wl_compositor.create_region(&qhandle, ());
        surface.set_input_region(Some(&region));
        let layer_surface = app.globals.layer_shell.get_layer_surface(
            &surface,
            Some(output.wl_output.as_ref().unwrap()),
            Layer::Overlay,
            String::from("waypoint"),
            &qhandle,
            (),
        );
        layer_surface.set_size(0, 0);
        layer_surface.set_anchor(Anchor::Top | Anchor::Bottom | Anchor::Left | Anchor::Right);
        layer_surface.set_exclusive_zone(-1);
        layer_surface.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_one_modifier_from_str() {
        #[track_caller]
        fn check(s: &str, expected: Option<Mods>) {
            assert_eq!(Mods::one_from_str(s), expected);
        }

        check("alt", Some(Mods::ALT));
        check("ALT", Some(Mods::ALT));
        check("Alt", Some(Mods::ALT));
        check("Alt-", None);
        check("None", None);

        for modifier_name in [
            "shift", "caps", "ctrl", "alt", "num", "mod3", "logo", "mod5",
        ] {
            #[track_caller]
            fn check(modifier_name: &str, input: &str) {
                assert_eq!(
                    format!("{:?}", Mods::one_from_str(input).unwrap()),
                    format!("Modifiers({})", modifier_name.to_uppercase()),
                );
            }

            check(modifier_name, modifier_name);
            check(modifier_name, &modifier_name.to_uppercase());
        }
    }
}
