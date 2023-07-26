use crate::{scfg, ModIndices};
use anyhow::{bail, ensure, Context, Result};
use bitflags::bitflags;
use std::{cmp::Ordering, collections::HashMap, path::PathBuf};
use wayland_client::protocol::wl_pointer::Axis;
use xkbcommon::xkb;

#[derive(Clone, Copy, Debug)]
pub(crate) enum Direction {
    Up,
    Down,
    Left,
    Right,
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum Button {
    Left,
    Right,
    Middle,
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum Cmd {
    Quit,
    Undo,
    Click(Button),
    Press(Button),
    Release(Button),
    Cut(Direction),
    Move(Direction),
    Scroll(Axis, f64),
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

pub(crate) struct Config {
    bindings: HashMap<(Mods, xkb::Keysym), Cmd>,
}

impl Button {
    pub(crate) fn code(self) -> u32 {
        const BTN_LEFT: u32 = 0x110;
        const BTN_RIGHT: u32 = 0x111;
        const BTN_MIDDLE: u32 = 0x112;

        match self {
            Button::Left => BTN_LEFT,
            Button::Right => BTN_RIGHT,
            Button::Middle => BTN_MIDDLE,
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
            "scroll-up" => Some(Cmd::Scroll(Axis::VerticalScroll, -10.0)),
            "scroll-down" => Some(Cmd::Scroll(Axis::VerticalScroll, 10.0)),
            "scroll-left" => Some(Cmd::Scroll(Axis::HorizontalScroll, -10.0)),
            "scroll-right" => Some(Cmd::Scroll(Axis::HorizontalScroll, 10.0)),
            _ => None,
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

impl Config {
    pub(crate) fn load() -> Result<Config> {
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

pub(crate) fn specialize_bindings(
    keymap: &xkb::Keymap,
    config: &Config,
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

    let specialized = config
        .bindings
        .iter()
        .flat_map(|(&(modifiers, keysym), &cmd)| {
            let mut keycodes = Vec::new();

            keymap.key_for_each(|_, keycode| {
                let got_keysym = state.key_get_one_sym(keycode);
                if got_keysym != xkb::KEY_NoSymbol && got_keysym == keysym {
                    keycodes.push(keycode);
                }
            });

            let mod_index_array: &[xkb::ModMask; 8] = bytemuck::cast_ref(&mod_indices);

            let mod_mask: xkb::ModMask = modifiers
                .into_iter()
                .map(|modifier| 1 << mod_index_array[modifier.bits().trailing_zeros() as usize])
                .fold(0, |acc, it| acc | it);

            keycodes
                .into_iter()
                .map(move |keycode| ((mod_mask, keycode), cmd))
        })
        .collect();

    (mod_indices, specialized)
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
