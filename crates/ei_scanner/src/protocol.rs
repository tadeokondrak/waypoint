use crate::ContextType;
use std::{
    fmt::{Debug, Display},
    str::FromStr,
};

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum MessageKind {
    Request,
    Event,
}

impl Display for MessageKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        Debug::fmt(&self, f)
    }
}

#[derive(Clone, Default, Debug, Eq, PartialEq)]
pub struct Protocol {
    pub name: String,
    pub copyright: String,
    pub description: Option<Description>,
    pub interfaces: Vec<Interface>,
}

#[derive(Clone, Default, Debug, Eq, PartialEq)]
pub struct Interface {
    pub name: String,
    pub version: u32,
    pub description: Option<Description>,
    pub requests: Vec<Message>,
    pub events: Vec<Message>,
    pub enums: Vec<Enum>,
}

#[derive(Clone, Default, Debug, Eq, PartialEq)]
pub struct Message {
    pub name: String,
    pub destructor: bool,
    pub since: u32,
    pub description: Option<Description>,
    pub args: Vec<Arg>,
    pub context_type: Option<ContextType>,
}

#[derive(Clone, Default, Debug, Eq, PartialEq)]
pub struct Arg {
    pub name: String,
    pub kind: ArgKind,
    pub summary: Option<String>,
    pub interface: Option<String>,
    pub allow_null: bool,
    pub enumeration: Option<String>,
    pub description: Option<Description>,
}

#[derive(Clone, Default, Debug, Eq, PartialEq)]
pub enum ArgKind {
    #[default]
    NewId,
    Int32,
    Uint32,
    Int64,
    Uint64,
    Float,
    String,
    ObjectId,
    Array,
    Fd,
}

#[derive(Clone, Default, Debug, Eq, PartialEq)]
pub struct Enum {
    pub name: String,
    pub since: u32,
    pub bitfield: bool,
    pub description: Option<Description>,
    pub entries: Vec<Entry>,
}

#[derive(Clone, Default, Debug, Eq, PartialEq)]
pub struct Entry {
    pub name: String,
    pub value: u32,
    pub summary: Option<String>,
    pub since: u32,
    pub description: Option<Description>,
}

#[derive(Clone, Default, Debug, Eq, PartialEq)]
pub struct Description {
    pub summary: String,
    pub body: String,
}

impl FromStr for ArgKind {
    type Err = ();
    fn from_str(s: &str) -> Result<ArgKind, ()> {
        match s {
            "new_id" => Ok(ArgKind::NewId),
            "int32" => Ok(ArgKind::Int32),
            "uint32" => Ok(ArgKind::Uint32),
            "int64" => Ok(ArgKind::Int64),
            "uint64" => Ok(ArgKind::Uint64),
            "float" => Ok(ArgKind::Float),
            "string" => Ok(ArgKind::String),
            "object_id" => Ok(ArgKind::ObjectId),
            "array" => Ok(ArgKind::Array),
            "fd" => Ok(ArgKind::Fd),
            _ => Err(()),
        }
    }
}

impl FromStr for ContextType {
    type Err = ();
    fn from_str(s: &str) -> Result<ContextType, ()> {
        match s {
            "sender" => Ok(ContextType::Sender),
            "receiver" => Ok(ContextType::Receiver),
            _ => Err(()),
        }
    }
}

pub struct ParseContext<'a> {
    pub parser: txml::Parser<'a>,
    pub attrs: Option<txml::Attrs<'a>>,
}

impl<'a> ParseContext<'a> {
    pub fn next(&mut self) -> Option<txml::Event<'a>> {
        Some(self.parser.next()?)
    }

    pub fn attr<T>(&self, name: &str) -> Option<T>
    where
        T: FromStr,
    {
        self.attrs
            .clone()?
            .filter(|&(k, _)| k == name)
            .map(|(_, v)| v)
            .next()?
            .collect::<String>()
            .parse::<T>()
            .ok()
    }

    pub fn parse(&mut self) -> Option<Protocol> {
        Some(loop {
            match self.next()? {
                txml::Event::Open(name, attrs) if name == "protocol" => {
                    self.attrs = Some(attrs);
                    break self.protocol()?;
                }
                txml::Event::Close(name) if name == "protocol" => return None,
                _ => {}
            }
        })
    }

    pub fn protocol(&mut self) -> Option<Protocol> {
        let mut protocol = Protocol::default();
        protocol.name = self.attr("name")?;
        Some(loop {
            match self.next()? {
                txml::Event::Open(name, attrs) => {
                    self.attrs = Some(attrs);
                    match &*name {
                        "copyright" => protocol.copyright = self.copyright()?,
                        "description" => protocol.description = self.description()?.into(),
                        "interface" => protocol.interfaces.push(self.interface()?),
                        _ => return None,
                    }
                }
                txml::Event::Close(name) if name == "protocol" => break protocol,
                txml::Event::Close(..) => return None,
                txml::Event::Text(..) | txml::Event::Comment(..) | txml::Event::Pi(..) => {}
            }
        })
    }

    pub fn copyright(&mut self) -> Option<String> {
        let mut body = String::new();
        Some(loop {
            match self.next()? {
                txml::Event::Text(text) => body.extend(text),
                txml::Event::Close(name) if name == "copyright" => break body,
                txml::Event::Open(..) | txml::Event::Close(..) => return None,
                txml::Event::Comment(..) | txml::Event::Pi(..) => {}
            }
        })
    }

    pub fn interface(&mut self) -> Option<Interface> {
        let mut interface = Interface::default();
        interface.name = self.attr("name")?;
        interface.version = self.attr("version")?;
        Some(loop {
            match self.next()? {
                txml::Event::Open(name, attrs) => {
                    self.attrs = Some(attrs);
                    match &*name {
                        "description" => interface.description = self.description()?.into(),
                        "request" => interface.requests.push(self.message()?),
                        "event" => interface.events.push(self.message()?),
                        "enum" => interface.enums.push(self.enumeration()?),
                        _ => return None,
                    }
                }
                txml::Event::Close(name) if name == "interface" => break interface,
                txml::Event::Close(..) => return None,
                txml::Event::Text(..) | txml::Event::Comment(..) | txml::Event::Pi(..) => {}
            }
        })
    }

    pub fn message(&mut self) -> Option<Message> {
        let mut request = Message::default();
        request.name = self.attr("name")?;
        request.destructor = self
            .attr("type")
            .map(|t: String| t == "destructor")
            .unwrap_or(false);
        request.since = self.attr("since").unwrap_or(1);
        request.context_type = self.attr("context-type");
        Some(loop {
            match self.next()? {
                txml::Event::Open(name, attrs) => {
                    self.attrs = Some(attrs);
                    match &*name {
                        "description" => request.description = self.description()?.into(),
                        "arg" => request.args.push(self.arg()?),
                        _ => return None,
                    }
                }
                txml::Event::Close(name) if name == "request" || name == "event" => break request,
                txml::Event::Close(..) => return None,
                txml::Event::Text(..) | txml::Event::Comment(..) | txml::Event::Pi(..) => {}
            }
        })
    }

    pub fn arg(&mut self) -> Option<Arg> {
        let mut arg = Arg::default();
        arg.name = self.attr("name")?;
        arg.kind = self.attr("type")?;
        arg.summary = self.attr("summary");
        arg.interface = self.attr("interface");
        arg.allow_null = self.attr("allow-null").unwrap_or(false);
        arg.enumeration = self.attr("enum");
        Some(loop {
            match self.next()? {
                txml::Event::Open(name, attrs) if name == "description" => {
                    self.attrs = Some(attrs);
                    arg.description = self.description()?.into();
                }
                txml::Event::Close(name) if name == "arg" => break arg,
                txml::Event::Open(..) | txml::Event::Close(..) => return None,
                txml::Event::Text(..) | txml::Event::Comment(..) | txml::Event::Pi(..) => {}
            }
        })
    }

    pub fn enumeration(&mut self) -> Option<Enum> {
        let mut enumeration = Enum::default();
        enumeration.name = self.attr("name")?;
        enumeration.since = self.attr("since").unwrap_or(1);
        enumeration.bitfield = self.attr("bitfield").unwrap_or(false);
        Some(loop {
            match self.next()? {
                txml::Event::Open(name, attrs) => {
                    self.attrs = Some(attrs);
                    match &*name {
                        "description" => enumeration.description = self.description()?.into(),
                        "entry" => enumeration.entries.push(self.entry()?),
                        _ => return None,
                    }
                }
                txml::Event::Close(name) if name == "enum" => break enumeration,
                txml::Event::Close(..) => return None,
                txml::Event::Text(..) | txml::Event::Comment(..) | txml::Event::Pi(..) => {}
            }
        })
    }

    pub fn entry(&mut self) -> Option<Entry> {
        let mut entry = Entry::default();
        entry.name = self.attr("name")?;
        entry.value = {
            let value: String = self.attr("value")?;
            let (str, radix) = if value.starts_with("0x") {
                (&value[2..], 16)
            } else {
                (&value[..], 10)
            };
            u32::from_str_radix(str, radix).ok()?
        };
        entry.summary = self.attr("summary");
        entry.since = self.attr("since").unwrap_or(1);
        Some(loop {
            match self.next()? {
                txml::Event::Open(name, attrs) if name == "description" => {
                    self.attrs = Some(attrs);
                    entry.description = self.description()?.into();
                }
                txml::Event::Close(name) if name == "entry" => break entry,
                txml::Event::Open(..) | txml::Event::Close(..) => return None,
                txml::Event::Text(..) | txml::Event::Comment(..) | txml::Event::Pi(..) => {}
            }
        })
    }

    pub fn description(&mut self) -> Option<Description> {
        let mut description = Description::default();
        description.summary = self.attr("summary")?;
        Some(loop {
            match self.next()? {
                txml::Event::Text(text) => description.body.extend(text),
                txml::Event::Close(name) if name == "description" => {
                    break description;
                }
                txml::Event::Open(..) | txml::Event::Close(..) => return None,
                txml::Event::Comment(..) | txml::Event::Pi(..) => {}
            }
        })
    }
}
