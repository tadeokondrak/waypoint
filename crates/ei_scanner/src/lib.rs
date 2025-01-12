mod protocol;

use crate::protocol::{Arg, ArgKind, Enum, Interface, Message, MessageKind, ParseContext};
use heck::{ToShoutySnakeCase, ToSnakeCase, ToUpperCamelCase};
use proc_macro2::TokenStream;
use quote::{format_ident, quote, ToTokens};
use std::{cmp::max, collections::BTreeMap, iter, path::PathBuf};

#[derive(Default, Clone, Copy, PartialEq, Eq, Debug)]
pub enum ContextType {
    #[default]
    Receiver,
    Sender,
}

#[derive(Default)]
pub struct Config {
    pub context_type: Option<ContextType>,
    pub protocols: Vec<PathBuf>,
    pub interfaces: BTreeMap<String, u32>,
}

impl Config {
    pub fn context_type(&mut self, context_type: ContextType) -> &mut Self {
        self.context_type = Some(context_type);
        self
    }

    pub fn protocol(&mut self, path: impl Into<PathBuf>) -> &mut Self {
        self.protocols.push(path.into());
        self
    }

    pub fn interface(&mut self, name: impl Into<String>, version: u32) -> &mut Self {
        assert!(
            self.interfaces.insert(name.into(), version).is_none(),
            "duplicate interface version"
        );
        self
    }

    pub fn generate(&self) -> String {
        let protocols = self
            .protocols
            .clone()
            .into_iter()
            .map(|path| {
                let text = std::fs::read_to_string(path).unwrap();
                ParseContext {
                    parser: txml::Parser::new(&text),
                    attrs: None,
                }
                .parse()
                .unwrap()
            })
            .collect::<Vec<_>>();

        let interfaces = protocols
            .into_iter()
            .flat_map(|protocol| protocol.interfaces)
            .map(|interface| (interface.name.clone(), interface))
            .collect();

        let dependency_graph = make_dependency_graph(&interfaces);

        for (name, version) in &self.interfaces {
            let interface = &interfaces[name.as_str()];
            if interface.version < *version {
                panic!(
                    "version too high on {name}, want {version}, protocol has {}",
                    interface.version
                );
            }
        }

        let mut wanted_interfaces: BTreeMap<String, DependencyKind> = BTreeMap::new();
        for (global, version) in &self.interfaces {
            let &version = version;
            wanted_interfaces.insert(global.clone(), DependencyKind::Real);

            go(
                &dependency_graph,
                &mut wanted_interfaces,
                global.clone(),
                version,
                DependencyKind::Real,
            );

            fn go<'a>(
                dependency_graph: &BTreeMap<(String, String), DependencyKind>,
                wanted_interfaces: &mut BTreeMap<String, DependencyKind>,
                global: String,
                version: u32,
                dependency_kind: DependencyKind,
            ) {
                let dependencies = dependency_graph
                    .range(&(global.clone(), String::from(""))..)
                    .take_while(|&((from, _), _)| from == &global);
                for ((_, dependency), kind) in dependencies {
                    match kind.min(&dependency_kind) {
                        DependencyKind::Dummy => {
                            wanted_interfaces
                                .entry(dependency.clone())
                                .or_insert(DependencyKind::Dummy);
                            go(
                                dependency_graph,
                                wanted_interfaces,
                                dependency.clone(),
                                version,
                                DependencyKind::Dummy,
                            );
                        }
                        DependencyKind::Real => {
                            wanted_interfaces.insert(dependency.clone(), DependencyKind::Real);
                            go(
                                dependency_graph,
                                wanted_interfaces,
                                dependency.clone(),
                                version,
                                DependencyKind::Real,
                            );
                        }
                    }
                }
            }
        }

        let wanted_interface_versions = wanted_interfaces
            .into_iter()
            .map(|(interface, kind)| match kind {
                DependencyKind::Dummy => (interface, 0),
                DependencyKind::Real => {
                    let Some(&version) = self.interfaces.get(&interface) else {
                        panic!("must specify interface version for {interface}")
                    };
                    (interface, version)
                }
            })
            .collect::<BTreeMap<String, u32>>();

        let interfaces = preprocess_interfaces(interfaces, wanted_interface_versions);

        let tokens = GenContext {
            interfaces: &interfaces,
            context_type: self.context_type,
            request_needs_lifetime: false,
            event_needs_lifetime: false,
        }
        .gen();

        prettyplease::unparse(&syn::parse2(tokens.to_token_stream()).unwrap())
    }
}

fn preprocess_interfaces(
    interfaces: BTreeMap<String, Interface>,
    wanted_interfaces: BTreeMap<String, u32>,
) -> BTreeMap<String, Interface> {
    interfaces
        .into_iter()
        .filter_map(|(name, mut interface)| {
            let &version = wanted_interfaces.get(&name)?;
            interface.version = version;
            interface
                .requests
                .retain(|message| message.since <= version);
            interface.events.retain(|message| message.since <= version);
            interface.enums.retain(|enm| enm.since <= version);
            for enm in &mut interface.enums {
                enm.entries.retain(|entry| entry.since <= version);
            }
            Some((name, interface))
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum DependencyKind {
    Dummy,
    Real,
}

fn make_dependency_graph(
    interfaces: &BTreeMap<String, Interface>,
) -> BTreeMap<(String, String), DependencyKind> {
    let mut dependency_graph: BTreeMap<(String, String), DependencyKind> = BTreeMap::new();

    for interface in interfaces.values() {
        let messages = interface.requests.iter().chain(interface.events.iter());
        for message in messages {
            for arg in &message.args {
                if let Some(arg_interface) = &arg.interface {
                    if arg_interface == &interface.name {
                        continue;
                    }
                    let dep_kind = match arg.kind {
                        ArgKind::NewId => DependencyKind::Real,
                        ArgKind::ObjectId => DependencyKind::Dummy,
                        _ => unreachable!(),
                    };
                    match dependency_graph.entry((interface.name.clone(), arg_interface.clone())) {
                        std::collections::btree_map::Entry::Vacant(entry) => {
                            entry.insert(dep_kind);
                        }
                        std::collections::btree_map::Entry::Occupied(mut entry) => {
                            let existing = *entry.get();
                            *entry.get_mut() = max(existing, dep_kind);
                        }
                    }
                }
            }
        }
    }

    dependency_graph
}

struct GenContext<'a> {
    interfaces: &'a BTreeMap<String, Interface>,
    context_type: Option<ContextType>,
    request_needs_lifetime: bool,
    event_needs_lifetime: bool,
}

impl<'a> GenContext<'a> {
    fn gen(mut self) -> TokenStream {
        let interface_enum = self.gen_global_interface_enum();
        let (request_enum, request_needs_lifetime) =
            self.gen_global_message_enum(|interface| &interface.requests, MessageKind::Request);
        let (event_enum, event_needs_lifetime) =
            self.gen_global_message_enum(|interface| &interface.events, MessageKind::Event);
        self.request_needs_lifetime = request_needs_lifetime;
        self.event_needs_lifetime = event_needs_lifetime;
        let interfaces = self
            .interfaces
            .values()
            .map(|interface| self.gen_interface(interface));
        quote! {
            extern crate ei;
            use ei::{Arg, Connection, Message, Object};
            #interface_enum
            #request_enum
            #event_enum
            #(#interfaces)*
        }
    }

    fn gen_interface(&self, interface: &Interface) -> TokenStream {
        let type_name = format_ident!("{}", interface.name.to_upper_camel_case());
        let request_type_name = format_ident!("{}Request", interface.name.to_upper_camel_case());
        let request_type_needs_lifetime =
            message_type_needs_lifetime(&interface.requests, self.context_type);
        let request_generics = if request_type_needs_lifetime {
            quote!(<'a>)
        } else {
            quote!()
        };
        let event_type_name = format_ident!("{}Event", interface.name.to_upper_camel_case());
        let event_type_needs_lifetime =
            message_type_needs_lifetime(&interface.events, self.context_type);
        let event_generics = if event_type_needs_lifetime {
            quote!(<'a>)
        } else {
            quote!()
        };
        let interface_struct = quote! {
            #[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
            pub struct #type_name(pub u64);

        };
        let interface_struct_object_impl = quote! {
            impl Object<Interface> for #type_name {
                const INTERFACE: Interface = Interface::#type_name;
                type Request<'a> = #request_type_name #request_generics;
                type Event<'a> = #event_type_name #event_generics;
                fn new(id: u64) -> #type_name { #type_name(id) }
                fn id(self) -> u64 { self.0 }
            }
        };

        let request_enums = self.gen_messages(interface, &interface.requests, MessageKind::Request);
        let event_enums = self.gen_messages(interface, &interface.events, MessageKind::Event);
        let enum_values = interface
            .enums
            .iter()
            .map(|enm| self.gen_interface_enum(interface, enm));
        if interface.version == 0 {
            quote! {
                #interface_struct
            }
        } else {
            quote! {
                #interface_struct
                #interface_struct_object_impl
                #(#enum_values)*
                #request_enums
                #event_enums
            }
        }
    }

    fn gen_messages(
        &self,
        interface: &Interface,
        messages: &[Message],
        kind: MessageKind,
    ) -> TokenStream {
        let global_enum_name = format_ident!("{kind}");
        let interface_type_name = format_ident!("{}", interface.name.to_upper_camel_case());
        let type_name = format_ident!("{}{kind}", interface.name.to_upper_camel_case());
        let variants = messages
            .iter()
            .filter(|msg| {
                self.context_type.is_none() || msg.context_type.is_none() || {
                    msg.context_type
                        .is_some_and(|it| it == self.context_type.unwrap())
                }
            })
            .map(|message| self.gen_message_enum_variant(interface, message));
        let type_needs_lifetime = message_type_needs_lifetime(messages, self.context_type);
        let generic = if type_needs_lifetime {
            quote!('a)
        } else {
            quote!()
        };
        let global_generic = if match kind {
            MessageKind::Request => self.request_needs_lifetime,
            MessageKind::Event => self.event_needs_lifetime,
        } {
            quote!(<'a>)
        } else {
            quote!()
        };
        let generics = quote!(<#generic>);
        let reader = self.gen_message_unmarshaler(interface, messages, kind);
        let writer = self.gen_message_marshaler(interface, messages, kind);
        quote! {
            #[derive(Debug)]
            pub enum #type_name #generics {
                #(#variants)*
            }
            #reader
            #writer
            // TODO make this lifetime optional
            impl #global_generic From<#type_name #generics> for #global_enum_name #global_generic {
                fn from(v: #type_name #generics) -> #global_enum_name #global_generic {
                    #global_enum_name::#interface_type_name(v)
                }
            }
        }
    }

    fn gen_message_enum_variant(&self, interface: &Interface, message: &Message) -> TokenStream {
        let interface_field_name = format_ident!("{}", interface.name.to_snake_case());
        let interface_type_name = format_ident!("{}", interface.name.to_upper_camel_case());
        let variant_name = format_ident!("{}", message.name.to_upper_camel_case());
        let fields = message
            .args
            .iter()
            .map(|arg| self.gen_message_enum_variant_field(arg));
        quote! {
            #variant_name {
                #interface_field_name: #interface_type_name,
                #(#fields)*
            },
        }
    }

    fn gen_message_enum_variant_field(&self, arg: &Arg) -> TokenStream {
        let field_name = format_ident!("{}", arg.name.to_snake_case());
        let field_type = self.gen_arg_field_type(arg);
        quote! {
            #field_name: #field_type,
        }
    }

    fn gen_arg_field_type(&self, arg: &Arg) -> TokenStream {
        if let Some(interface) = &arg.interface {
            let type_name = format_ident!("{}", interface.to_upper_camel_case());
            return quote!(#type_name);
        }
        let tokens = match arg.kind {
            ArgKind::NewId => quote!(u64),
            ArgKind::Int32 => quote!(i32),
            ArgKind::Uint32 => quote!(u32),
            ArgKind::Int64 => quote!(i64),
            ArgKind::Uint64 => quote!(u64),
            ArgKind::Float => quote!(f32),
            ArgKind::String if arg.allow_null => quote!(Option<std::borrow::Cow<'a, str>>),
            ArgKind::String => quote!(std::borrow::Cow<'a, str>),
            ArgKind::ObjectId => quote!(u64),
            ArgKind::Array => quote!(std::borrow::Cow<'a, [u8]>),
            ArgKind::Fd => quote!(wayland::rustix::fd::OwnedFd),
        };
        tokens
    }

    fn gen_global_message_enum(
        &self,
        selector: impl for<'b> Fn(&'b Interface) -> &'b [Message],
        kind: MessageKind,
    ) -> (TokenStream, bool) {
        let type_name = format_ident!("{kind}");
        let mut any_variant_needs_lifetime = false;
        let enabled_interfaces = self
            .interfaces
            .values()
            .filter(|interface| interface.version != 0);
        let disabled_interfaces = self
            .interfaces
            .values()
            .filter(|interface| interface.version == 0)
            .map(|interface| {
                let interface_type_name = format_ident!("{}", interface.name.to_upper_camel_case());
                format_ident!("{interface_type_name}")
            });
        let variants = enabled_interfaces
            .clone()
            .map(|interface| {
                let needs_lifetime =
                    message_type_needs_lifetime(selector(interface), self.context_type);
                any_variant_needs_lifetime |= needs_lifetime;
                self.gen_global_message_enum_variant(interface, kind, needs_lifetime)
            })
            .collect::<Vec<_>>();
        let kind_ident = format_ident!("{kind}");
        let read_variants = enabled_interfaces.clone().map(|interface| {
            let interface_type_name = format_ident!("{}", interface.name.to_upper_camel_case());
            let enum_type_name = format_ident!("{}{kind}", interface.name.to_upper_camel_case());
            quote! {
                Interface::#interface_type_name => #kind_ident::#interface_type_name(#enum_type_name::unmarshal(msg)?),
            }
        });
        let read_disabled_variants = disabled_interfaces.clone().map(|interface_type_name| {
            quote! {
                Interface::#interface_type_name => unreachable!("disabled"),
            }
        });
        let write_variants = enabled_interfaces.map(|interface| {
            let interface_type_name = format_ident!("{}", interface.name.to_upper_camel_case());
            quote! {
                #kind_ident::#interface_type_name(it) => it.marshal(conn),
            }
        });
        let generics = if any_variant_needs_lifetime {
            quote!(<'a>)
        } else {
            quote!()
        };
        let tokens = quote! {
            #[derive(Debug)]
            pub enum #type_name #generics {
                #(#variants)*
            }
            impl #generics #type_name #generics {
                pub fn unmarshal(interface: Interface, mut msg: Message<'_>) -> Option<#type_name #generics> {
                    Some(match interface {
                        #(#read_variants)*
                        #(#read_disabled_variants)*
                    })
                }
                pub fn marshal(self, conn: &mut Connection) {
                    match self {
                        #(#write_variants)*
                    }
                }
            }
        };
        (tokens, any_variant_needs_lifetime)
    }

    fn gen_global_message_enum_variant(
        &self,
        interface: &Interface,
        kind: MessageKind,
        needs_lifetime: bool,
    ) -> TokenStream {
        let variant_name = format_ident!("{}", interface.name.to_upper_camel_case());
        let type_name = format_ident!("{}{kind}", interface.name.to_upper_camel_case());
        let generics = if needs_lifetime {
            quote!(<'a>)
        } else {
            quote!()
        };
        quote! {
            #variant_name(#type_name #generics),
        }
    }

    fn gen_interface_enum(&self, interface: &Interface, enm: &Enum) -> TokenStream {
        let entries = enm.entries.iter().map(|entry| {
            let const_name = format_ident!(
                "{}_{}_{}",
                interface.name.to_shouty_snake_case(),
                enm.name.to_shouty_snake_case(),
                entry.name.to_shouty_snake_case(),
            );
            let value = entry.value;
            quote! {
                pub const #const_name: u32 = #value;
            }
        });
        quote!(#(#entries)*)
    }

    fn gen_message_unmarshaler(
        &self,
        interface: &Interface,
        messages: &[Message],
        kind: MessageKind,
    ) -> TokenStream {
        let type_name = format_ident!("{}{kind}", interface.name.to_upper_camel_case());
        let needs_lifetime = message_type_needs_lifetime(messages, self.context_type);
        let generics = if needs_lifetime {
            quote!(<'a>)
        } else {
            quote!()
        };
        let variants = messages
            .iter()
            .enumerate()
            .filter(|(_, msg)| {
                self.context_type.is_none() || msg.context_type.is_none() || {
                    msg.context_type
                        .is_some_and(|it| it == self.context_type.unwrap())
                }
            })
            .map(|(i, message)| {
                self.gen_message_reader_variant(u32::try_from(i).unwrap(), interface, message, kind)
            });
        quote! {
            impl #generics #type_name #generics {
                pub fn unmarshal(mut msg: Message<'_>) -> Option<#type_name #generics> {
                    match msg.opcode() {
                        #(#variants)*
                        _ => None
                    }
                }
            }
        }
    }

    fn gen_message_marshaler(
        &self,
        interface: &Interface,
        messages: &[Message],
        kind: MessageKind,
    ) -> TokenStream {
        let type_name = format_ident!("{}{kind}", interface.name.to_upper_camel_case());
        let needs_lifetime = message_type_needs_lifetime(messages, self.context_type);
        let generics = if needs_lifetime {
            quote!(<'a>)
        } else {
            quote!()
        };
        let variants = messages
            .iter()
            .enumerate()
            .filter(|(_, msg)| {
                self.context_type.is_none() || msg.context_type.is_none() || {
                    msg.context_type
                        .is_some_and(|it| it == self.context_type.unwrap())
                }
            })
            .map(|(i, message)| {
                self.gen_message_marshaler_variant(
                    u32::try_from(i).unwrap(),
                    interface,
                    message,
                    kind,
                )
            });
        quote! {
            impl #generics #type_name #generics {
                pub fn marshal(self, conn: &mut Connection) {
                    match self {
                        #(#variants)*
                    }
                }
            }
        }
    }

    fn gen_message_reader_variant(
        &self,
        i: u32,
        interface: &Interface,
        message: &Message,
        kind: MessageKind,
    ) -> TokenStream {
        let interface_field_name = format_ident!("{}", interface.name.to_snake_case());
        let interface_type_name = format_ident!("{}", interface.name.to_upper_camel_case());
        let enum_type_name = format_ident!("{}{kind}", interface.name.to_upper_camel_case());
        let variant_name = format_ident!("{}", message.name.to_upper_camel_case());
        let fields = message
            .args
            .iter()
            .map(|arg| self.gen_message_reader_variant_arg(arg));
        quote! {
            #i => Some(#enum_type_name::#variant_name {
                #interface_field_name: #interface_type_name(msg.object()),
                #(#fields)*
            }),
        }
    }

    fn gen_message_marshaler_variant(
        &self,
        i: u32,
        interface: &Interface,
        message: &Message,
        kind: MessageKind,
    ) -> TokenStream {
        let interface_field_name = format_ident!("{}", interface.name.to_snake_case());
        let interface_type_name = format_ident!("{}", interface.name.to_upper_camel_case());
        let type_name = format_ident!("{}{kind}", interface.name.to_upper_camel_case());
        let variant_name = format_ident!("{}", message.name.to_upper_camel_case());
        let arg_field_names = iter::once(format_ident!("{}", interface_field_name)).chain(
            message
                .args
                .iter()
                .map(|arg| format_ident!("{}", arg.name.to_snake_case())),
        );
        let arg_bindings = iter::once({
            let ident = format_ident!("object");
            quote!(#interface_type_name(#ident))
        })
        .chain(message.args.iter().enumerate().map(|(i, arg)| {
            let ident = format_ident!("arg{i}");
            if let Some(interface) = &arg.interface {
                let type_name = format_ident!("{}", interface.to_upper_camel_case());
                quote!(#type_name(#ident))
            } else {
                quote!(#ident)
            }
        }));
        let arg_values = message
            .args
            .iter()
            .enumerate()
            .filter(|&(_i, arg)| arg.kind != ArgKind::Fd)
            .map(|(i, arg)| {
                let ident = format_ident!("arg{i}");
                match arg.kind {
                    ArgKind::NewId => quote!(Arg::Uint64(#ident)),
                    ArgKind::Int32 => quote!(Arg::Int32(#ident)),
                    ArgKind::Uint32 => quote!(Arg::Uint32(#ident)),
                    ArgKind::Int64 => quote!(Arg::Int64(#ident)),
                    ArgKind::Uint64 => quote!(Arg::Uint64(#ident)),
                    ArgKind::Float => quote!(Arg::Float(#ident)),
                    ArgKind::String if arg.allow_null => {
                        quote!(Arg::String(#ident.as_deref()))
                    }
                    ArgKind::String => quote!(Arg::String(Some(#ident.as_ref()))),
                    ArgKind::ObjectId => quote!(Arg::Uint64(#ident)),
                    ArgKind::Array => quote!(Arg::Array(#ident.as_ref())),
                    ArgKind::Fd => unreachable!(),
                }
            });
        let fd_values = message
            .args
            .iter()
            .enumerate()
            .filter(|&(_i, arg)| arg.kind == ArgKind::Fd)
            .map(|(i, _arg)| format_ident!("arg{i}"));
        quote! {
            #type_name::#variant_name { #(#arg_field_names: #arg_bindings),* } => {
                conn.write_message(object, #i, &[#(#arg_values),*], [#(#fd_values),*])
            },
        }
    }

    fn gen_message_reader_variant_arg(&self, arg: &Arg) -> TokenStream {
        let field_name = format_ident!("{}", arg.name.to_snake_case());
        let field_value = match arg.kind {
            _ if arg.interface.is_some() => {
                let type_name =
                    format_ident!("{}", arg.interface.as_ref().unwrap().to_upper_camel_case());
                quote!(msg.read_uint64().map(#type_name)?)
            }
            ArgKind::NewId => quote!(msg.read_uint64()?),
            ArgKind::Int32 => quote!(msg.read_int32()?),
            ArgKind::Uint32 => quote!(msg.read_uint32()?),
            ArgKind::Int64 => quote!(msg.read_int64()?),
            ArgKind::Uint64 => quote!(msg.read_uint64()?),
            ArgKind::Float => quote!(msg.read_float()?),
            ArgKind::String if arg.allow_null => {
                quote!(msg
                    .read_string()
                    .map(|opt| opt.map(std::borrow::Cow::Owned))?)
            }
            ArgKind::String => {
                quote!(msg
                    .read_string()
                    .map(|opt| opt.unwrap())
                    .map(std::borrow::Cow::Owned)?)
            }
            ArgKind::ObjectId => quote!(msg.read_uint64()?),
            ArgKind::Array => quote!(msg.read_array().map(std::borrow::Cow::Owned)?),
            ArgKind::Fd => quote!(msg.read_fd()?),
        };
        quote! {
            #field_name: #field_value,
        }
    }

    fn gen_global_interface_enum(&self) -> TokenStream {
        let variants = self
            .interfaces
            .values()
            .map(|interface| format_ident!("{}", interface.name.to_upper_camel_case()));
        let name_variants =
            self.interfaces
                .values()
                .zip(variants.clone())
                .map(|(interface, variant)| {
                    let name = &interface.name;
                    quote! {
                        Interface::#variant => #name,
                    }
                });
        let version_variants =
            self.interfaces
                .values()
                .zip(variants.clone())
                .map(|(interface, variant)| {
                    let version = interface.version;
                    quote! {
                        Interface::#variant => #version,
                    }
                });
        quote! {
            #[derive(Debug, Clone, Copy, Eq, PartialEq)]
            pub enum Interface {
                #(#variants,)*
            }

            impl Interface {
                pub const fn name(self) -> &'static str {
                    match self {
                        #(#name_variants)*
                    }
                }
                pub const fn version(self) -> u32 {
                    match self {
                        #(#version_variants)*
                    }
                }
            }
        }
    }
}

fn message_type_needs_lifetime(messages: &[Message], context_type: Option<ContextType>) -> bool {
    messages
        .iter()
        .filter(|msg| {
            context_type.is_none() || msg.context_type.is_none() || msg.context_type == context_type
        })
        .any(|message| {
            message
                .args
                .iter()
                .any(|arg| matches!(arg.kind, ArgKind::String | ArgKind::Array))
        })
}
