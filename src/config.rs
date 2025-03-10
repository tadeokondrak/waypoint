use crate::wl_gen::{WL_POINTER_AXIS_HORIZONTAL_SCROLL, WL_POINTER_AXIS_VERTICAL_SCROLL};
use anyhow::{bail, ensure, Context, Result};
use bitflags::bitflags;
use std::{cmp::Ordering, collections::HashMap, path::PathBuf};

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
    Scroll(u32, f64),
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
    bindings: HashMap<(Mods, kbvm::Keysym), Vec<Cmd>>,
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
            "scroll-up" => Some(Cmd::Scroll(WL_POINTER_AXIS_VERTICAL_SCROLL, -1.0)),
            "scroll-down" => Some(Cmd::Scroll(WL_POINTER_AXIS_VERTICAL_SCROLL, 1.0)),
            "scroll-left" => Some(Cmd::Scroll(WL_POINTER_AXIS_HORIZONTAL_SCROLL, -1.0)),
            "scroll-right" => Some(Cmd::Scroll(WL_POINTER_AXIS_HORIZONTAL_SCROLL, 1.0)),
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
                        let cmd_names: Vec<String> = if binding.params.is_empty() {
                            let mut cmd_names = Vec::new();
                            for binding_cmd in &binding.children {
                                ensure!(
                                    binding_cmd.params.is_empty(),
                                    "invalid config: line {}: binding with command should not have extra parameters",
                                    binding_cmd.line,
                                );

                                cmd_names.push(binding_cmd.name.clone());
                            }
                            cmd_names
                        } else {
                            ensure!(
                                binding.children.is_empty(),
                                "invalid config: line {}: binding with command should not have block",
                                binding.line,
                            );

                            ensure!(
                                binding.params.len() == 1,
                                "invalid config: line {}: binding with command should have exactly one parameter",
                                binding.line,
                            );

                            binding.params.clone()
                        };

                        let keys = &binding.name;
                        let mut cmds = Vec::new();

                        for cmd_name in cmd_names {
                            let Some(cmd) = Cmd::from_kebab_case(&cmd_name) else {
                                bail!(
                                    "invalid config: line {}: invalid command {:?}",
                                    binding.line,
                                    cmd_name,
                                );
                            };
                            cmds.push(cmd);
                        }

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
                                    let Some(parsed_keysym) =
                                        kbvm::Keysym::from_str_insensitive(element)
                                    else {
                                        bail!(
                                            "invalid config: line {}: invalid key {:?}",
                                            binding.line,
                                            element,
                                        );
                                    };
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

                        bindings.insert((modifiers, keysym), cmds);
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
    keymap: &kbvm::xkb::Keymap,
    config: &Config,
) -> HashMap<(kbvm::ModifierMask, kbvm::Keysym), Vec<Cmd>> {
    let lookup_table = keymap.to_builder().build_lookup_table();
    let specialized = config
        .bindings
        .iter()
        .flat_map(|(&(modifiers, keysym), cmds)| {
            let mut keysyms = Vec::new();
            for key in keymap.keys() {
                let lookup = lookup_table.lookup(
                    kbvm::GroupIndex::ZERO,
                    kbvm::ModifierMask::default(),
                    key.keycode(),
                );
                let Some(sym_props) = lookup.into_iter().next() else {
                    continue;
                };
                if sym_props.keysym() == keysym {
                    keysyms.push(keysym);
                }
            }
            let mod_mask = kbvm::ModifierMask(modifiers.bits().into());
            keysyms
                .into_iter()
                .map(move |keycode| ((mod_mask, keycode), cmds.clone()))
        })
        .collect();
    specialized
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
                    format!("Mods({})", modifier_name.to_uppercase()),
                );
            }

            check(modifier_name, modifier_name);
            check(modifier_name, &modifier_name.to_uppercase());
        }
    }
}
