mod protocol;

use crate::protocol::{
    Arg, ArgKind, Enum, Interface, Message, MessageKind, ParseContext, Protocol,
};
use heck::{ToShoutySnakeCase, ToSnakeCase, ToUpperCamelCase};
use proc_macro2::TokenStream;
use protocol::Description;
use quote::{format_ident, quote, ToTokens};
use std::{
    cmp::max,
    collections::{BTreeMap, HashSet},
    iter,
    path::PathBuf,
};

#[derive(Default)]
pub struct Config {
    pub protocols: Vec<PathBuf>,
    pub globals: Vec<(String, u32)>,
}

impl Config {
    pub fn protocol(&mut self, path: impl Into<PathBuf>) -> &mut Self {
        self.protocols.push(path.into());
        self
    }

    pub fn global(&mut self, name: impl Into<String>, version: u32) -> &mut Self {
        self.globals.push((name.into(), version));
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
                .map(preprocess_protocol)
                .unwrap()
            })
            .collect::<Vec<_>>();

        let interfaces = protocols
            .into_iter()
            .flat_map(|protocol| protocol.interfaces)
            .map(|interface| (interface.name.clone(), interface))
            .collect();

        let dependency_graph = make_dependency_graph(&interfaces);

        let global_allowlist = interfaces
            .keys()
            .filter(|interface| {
                !dependency_graph.iter().any(|((_from, to), kind)| {
                    interface == &to && kind == &DependencyKind::SameVersion
                })
            })
            .cloned()
            .collect::<HashSet<String>>();

        for (global, version) in &self.globals {
            let interface = &interfaces[global.as_str()];
            if interface.version < *version {
                panic!(
                    "version too high on {global}, want {version}, protocol has {}",
                    interface.version
                );
            }
            if !global_allowlist.contains(global) {
                panic!("{global} is not a global interface");
            }
        }

        let mut wanted_interfaces: BTreeMap<String, u32> = BTreeMap::new();
        for (global, version) in &self.globals {
            let &version = version;
            wanted_interfaces.insert(global.clone(), version);

            go(
                &dependency_graph,
                &mut wanted_interfaces,
                global.clone(),
                version,
                DependencyKind::SameVersion,
            );

            fn go<'a>(
                dependency_graph: &BTreeMap<(String, String), DependencyKind>,
                wanted_interfaces: &mut BTreeMap<String, u32>,
                global: String,
                version: u32,
                dependency_kind: DependencyKind,
            ) {
                let dependencies = dependency_graph
                    .range(&(global.clone(), String::from(""))..)
                    .take_while(|&((from, _), _)| from == &global);
                for ((_, dependency), kind) in dependencies {
                    match kind.min(&dependency_kind) {
                        DependencyKind::AnyVersion => {
                            match wanted_interfaces.entry(dependency.clone()) {
                                std::collections::btree_map::Entry::Vacant(entry) => {
                                    entry.insert(0);
                                }
                                std::collections::btree_map::Entry::Occupied(_) => {}
                            }
                            go(
                                dependency_graph,
                                wanted_interfaces,
                                dependency.clone(),
                                version,
                                DependencyKind::AnyVersion,
                            );
                        }
                        DependencyKind::SameVersion => {
                            match wanted_interfaces.entry(dependency.clone()) {
                                std::collections::btree_map::Entry::Vacant(entry) => {
                                    entry.insert(version);
                                }
                                std::collections::btree_map::Entry::Occupied(mut entry) => {
                                    let existing = *entry.get();
                                    *entry.get_mut() = max(existing, version);
                                }
                            }
                            go(
                                dependency_graph,
                                wanted_interfaces,
                                dependency.clone(),
                                version,
                                DependencyKind::SameVersion,
                            );
                        }
                    }
                }
            }
        }

        let interfaces = preprocess_interfaces(interfaces, wanted_interfaces);

        let tokens = GenContext {
            interfaces: &interfaces,
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
    AnyVersion,
    SameVersion,
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
                        ArgKind::NewId => DependencyKind::SameVersion,
                        ArgKind::Object => DependencyKind::AnyVersion,
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

fn preprocess_protocol(mut protocol: Protocol) -> Protocol {
    for interface in &mut protocol.interfaces {
        for message in interface
            .requests
            .iter_mut()
            .chain(interface.events.iter_mut())
        {
            let new_id_without_interface_arg_indices = message
                .args
                .iter()
                .enumerate()
                .filter(|(_i, arg)| arg.kind == ArgKind::NewId && arg.interface.is_none())
                .map(|(i, _arg)| i)
                .collect::<Vec<_>>();
            for i in new_id_without_interface_arg_indices {
                message.args.insert(
                    i,
                    Arg {
                        name: "version".into(),
                        kind: ArgKind::Uint,
                        ..Arg::default()
                    },
                );
                message.args.insert(
                    i,
                    Arg {
                        name: "interface".into(),
                        kind: ArgKind::String,
                        ..Arg::default()
                    },
                );
            }
        }
    }
    protocol
}

struct GenContext<'a> {
    interfaces: &'a BTreeMap<String, Interface>,
}

impl<'a> GenContext<'a> {
    fn gen(&self) -> TokenStream {
        let interfaces = self
            .interfaces
            .values()
            .map(|interface| self.gen_interface(interface));
        let interface_enum = self.gen_global_interface_enum();
        let request_enum =
            self.gen_global_message_enum(|interface| &interface.requests, MessageKind::Request);
        let event_enum =
            self.gen_global_message_enum(|interface| &interface.events, MessageKind::Event);
        quote! {
            extern crate wayland;
            use wayland::{Arg, Connection, Message, Fixed, Object};
            #interface_enum
            #request_enum
            #event_enum
            #(#interfaces)*
        }
    }

    fn gen_interface(&self, interface: &Interface) -> TokenStream {
        let type_name = format_ident!("{}", interface.name.to_upper_camel_case());
        let request_type_name = format_ident!("{}Request", interface.name.to_upper_camel_case());
        let request_type_needs_lifetime = message_type_needs_lifetime(&interface.requests);
        let request_generics = if request_type_needs_lifetime {
            quote!(<'a>)
        } else {
            quote!()
        };
        let event_type_name = format_ident!("{}Event", interface.name.to_upper_camel_case());
        let event_type_needs_lifetime = message_type_needs_lifetime(&interface.events);
        let event_generics = if event_type_needs_lifetime {
            quote!(<'a>)
        } else {
            quote!()
        };
        let interface_struct = quote! {
            #[derive(Debug, Clone, Copy, Default, Eq, PartialEq, Hash)]
            pub struct #type_name(pub u32);

        };
        let interface_struct_object_impl = quote! {
            impl Object<Interface> for #type_name {
                const INTERFACE: Interface = Interface::#type_name;
                type Request<'a> = #request_type_name #request_generics;
                type Event<'a> = #event_type_name #event_generics;
                fn new(id: u32) -> #type_name { #type_name(id) }
                fn id(self) -> u32 { self.0 }
            }
        };
        let request_enums = self.gen_messages(interface, &interface.requests, MessageKind::Request);
        let event_enums = self.gen_messages(interface, &interface.events, MessageKind::Event);
        let enum_values = interface
            .enums
            .iter()
            .map(|enm| self.gen_interface_enum(interface, enm));
        let doc = self.gen_doc_attr(interface.description.as_ref());
        if interface.version == 0 {
            quote! {
                #interface_struct
            }
        } else {
            quote! {
                #doc
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
            .map(|message| self.gen_message_enum_variant(interface, message));
        let type_needs_lifetime = message_type_needs_lifetime(messages);
        let generic = if type_needs_lifetime {
            quote!('a)
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
            impl<'a> From<#type_name #generics> for #global_enum_name<'a> {
                fn from(v: #type_name #generics) -> #global_enum_name<'a> {
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
        let doc = self.gen_doc_attr(message.description.as_ref());
        quote! {
            #doc
            #variant_name {
                #interface_field_name: #interface_type_name,
                #(#fields)*
            },
        }
    }

    fn gen_message_enum_variant_field(&self, arg: &Arg) -> TokenStream {
        let field_name = format_ident!("{}", arg.name.to_snake_case());
        let field_type = self.gen_arg_field_type(arg);
        let doc = self.gen_doc_attr_with_summary(arg.summary.as_deref(), arg.description.as_ref());
        quote! {
            #doc
            #field_name: #field_type,
        }
    }

    fn gen_arg_field_type(&self, arg: &Arg) -> TokenStream {
        if let Some(interface) = &arg.interface {
            let type_name = format_ident!("{}", interface.to_upper_camel_case());
            return quote!(#type_name);
        }
        let tokens = match arg.kind {
            ArgKind::NewId => quote!(u32),
            ArgKind::Int => quote!(i32),
            ArgKind::Uint => quote!(u32),
            ArgKind::Fixed => quote!(Fixed),
            ArgKind::String if arg.allow_null => quote!(Option<std::borrow::Cow<'a, str>>),
            ArgKind::String => quote!(std::borrow::Cow<'a, str>),
            ArgKind::Object => quote!(u32),
            ArgKind::Array => quote!(std::borrow::Cow<'a, [u8]>),
            ArgKind::Fd => quote!(wayland::rustix::fd::OwnedFd),
        };
        tokens
    }

    fn gen_global_message_enum(
        &self,
        selector: impl for<'b> Fn(&'b Interface) -> &'b [Message],
        kind: MessageKind,
    ) -> TokenStream {
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
                let needs_lifetime = message_type_needs_lifetime(selector(interface));
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
        quote! {
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
        }
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
        let since_name = format_ident!(
            "{}_{}_SINCE_VERSION",
            interface.name.to_shouty_snake_case(),
            enm.name.to_shouty_snake_case(),
        );
        let since = enm.since;
        let entries = enm.entries.iter().map(|entry| {
            let const_name = format_ident!(
                "{}_{}_{}",
                interface.name.to_shouty_snake_case(),
                enm.name.to_shouty_snake_case(),
                entry.name.to_shouty_snake_case(),
            );
            let since_name = format_ident!("{const_name}_SINCE_VERSION");
            let since = entry.since;
            let value = entry.value;
            let doc = self
                .gen_doc_attr_with_summary(entry.summary.as_deref(), entry.description.as_ref());
            quote! {
                #doc
                pub const #const_name: u32 = #value;
                pub const #since_name: u32 = #since;
            }
        });
        let doc = self.gen_doc_attr(enm.description.as_ref());
        quote!(
            #doc
            pub const #since_name: u32 = #since;
            #(#entries)*
        )
    }

    fn gen_message_unmarshaler(
        &self,
        interface: &Interface,
        messages: &[Message],
        kind: MessageKind,
    ) -> TokenStream {
        let type_name = format_ident!("{}{kind}", interface.name.to_upper_camel_case());
        let needs_lifetime = message_type_needs_lifetime(messages);
        let generics = if needs_lifetime {
            quote!(<'a>)
        } else {
            quote!()
        };
        let variants = messages.iter().enumerate().map(|(i, message)| {
            self.gen_message_reader_variant(u16::try_from(i).unwrap(), interface, message, kind)
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
        let needs_lifetime = message_type_needs_lifetime(messages);
        let generics = if needs_lifetime {
            quote!(<'a>)
        } else {
            quote!()
        };
        let variants = messages.iter().enumerate().map(|(i, message)| {
            self.gen_message_marshaler_variant(u16::try_from(i).unwrap(), interface, message, kind)
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
        i: u16,
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
        i: u16,
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
                    ArgKind::NewId => quote!(Arg::Uint(#ident)),
                    ArgKind::Int => quote!(Arg::Int(#ident)),
                    ArgKind::Uint => quote!(Arg::Uint(#ident)),
                    ArgKind::Fixed => quote!(Arg::Fixed(#ident)),
                    ArgKind::String if arg.allow_null => {
                        quote!(Arg::String(#ident.as_deref()))
                    }
                    ArgKind::String => quote!(Arg::String(Some(#ident.as_ref()))),
                    ArgKind::Object => quote!(Arg::Uint(#ident)),
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
                quote!(msg.read_uint().map(#type_name)?)
            }
            ArgKind::NewId => quote!(msg.read_uint()?),
            ArgKind::Int => quote!(msg.read_int()?),
            ArgKind::Uint => quote!(msg.read_uint()?),
            ArgKind::Fixed => quote!(msg.read_fixed()?),
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
            ArgKind::Object => quote!(msg.read_uint()?),
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
            #[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
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

    fn gen_doc_attr_with_summary(
        &self,
        summary: Option<&str>,
        description: Option<&Description>,
    ) -> TokenStream {
        debug_assert!(
            !(summary.is_some() && description.is_some()),
            "something has both a summary attribute and a description element",
        );
        let summary = summary
            .map(|summary| format!(" {summary}"))
            .map(|summary| quote!(#[doc = #summary]));
        let description = self.gen_doc_attr(description);
        quote! {
            #summary
            #description
        }
    }

    fn gen_doc_attr(&self, description: Option<&Description>) -> TokenStream {
        let Some(Description { summary, body }) = description else {
            return quote!();
        };
        let body = trim_multiline(body);
        let text = format!(" {}\n\n ---\n\n{}\n", summary.trim(), body.trim_end());
        let lines = text.lines().map(|line| quote!(#[doc = #line]));
        quote! {
            #(#lines)*
        }
    }
}

fn message_type_needs_lifetime(messages: &[Message]) -> bool {
    messages.iter().any(|message| {
        message
            .args
            .iter()
            .any(|arg| matches!(arg.kind, ArgKind::String | ArgKind::Array))
    })
}

fn trim_multiline(s: &str) -> String {
    let mut common_prefix: Option<&str> = None;
    for line in s.lines() {
        let Some(i) = line.find(|c: char| !c.is_whitespace()) else {
            continue;
        };
        let ws = &line[..i];
        if ws.is_empty() {
            continue;
        }
        match common_prefix {
            Some(cp) => {
                if ws == cp {
                    continue;
                }
                // If cp is prefix to ws, we don't care.
                if ws.strip_prefix(cp).is_some() {
                    continue;
                }
                // If ws is prefix to cp, we shrink cp.
                if let Some(not_common) = cp.strip_prefix(ws) {
                    let newcp_len = cp.len() - not_common.len();
                    let newcp = &cp[..newcp_len];
                    common_prefix = Some(newcp);
                }
            }
            None => common_prefix = Some(ws),
        }
    }
    let mut result = String::new();
    let mut nonempty_line_added = false;
    for line in s.lines() {
        if line.trim().is_empty() {
            if nonempty_line_added {
                result.push('\n');
            }
            continue;
        }
        result.push(' ');
        result.push_str(
            line.strip_prefix(common_prefix.unwrap_or(""))
                .unwrap_or(line),
        );
        result.push('\n');
        nonempty_line_added = true;
    }
    result
}
