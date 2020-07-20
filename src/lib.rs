#![feature(test)]

extern crate test;

use can_dbc::{
    ByteOrder, Message, MessageId, MultiplexIndicator, Signal, SignalExtendedValueType,
    ValueDescription, ValueType, DBC,
};

use heck::{CamelCase, ShoutySnakeCase, SnakeCase};
use log::warn;
use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use socketcan::{EFF_MASK, SFF_MASK};

#[cfg(test)]
mod tests {
    use byteorder;
    use byteorder::{ByteOrder, LE};
    use test::{black_box, Bencher};

    #[bench]
    fn bench_read_signal(b: &mut Bencher) {
        const byte_payload: &[u8] = &[
            0x4, 0x2, 0xA, 0xA, 0xF, 0xF, 0xE, 0xE, 0xD, 0xD, 0xA, 0xA, 0xF, 0xF, 0xD, 0xD,
        ];

        b.iter(|| {
            let frame_payload: u64 = LE::read_u64(byte_payload);
            let bit_msk_const = 2u64.saturating_pow(8 as u32) - 1;
            let factor: f64 = test::black_box(2.0);
            let start_bit: u64 = test::black_box(8);
            let offset: f64 = test::black_box(10.0);

            (((frame_payload >> start_bit) & bit_msk_const) as f64) * factor + offset
        });
    }
}

/// Character that is prefixed before type names that are
/// are not starting with an alphabetic char.
const PREFIX_CHAR: char = 'X';

/// Character that is used to replace invalid characters
/// in type names.
const REPLACEMENT_CHAR: char = 'X';

/// Suffix that is append to the raw signal function
const RAW_FN_SUFFIX: &str = "raw_value";

type Result<T> = std::result::Result<T, std::fmt::Error>;

#[derive(Debug)]
pub struct DbccOpt {
    /// Should tokio SocketCan BCM streams be generated.
    /// This requires the `tokio-socketcan-bcm` crate.
    pub with_tokio: bool,
}

pub trait TypeName: ToOwned {
    fn to_type_name(&self) -> Self::Owned;
}

impl TypeName for str {
    fn to_type_name(&self) -> String {
        let mut out = String::with_capacity(self.len() + 1);
        let mut chars = self.chars();
        if let Some(first) = chars.next() {
            if !first.is_alphabetic() && first != '_' {
                warn!("string: {} is prefixed with `{}`", self, PREFIX_CHAR);
                out.push(PREFIX_CHAR);
            }
            out.push(first);
        }

        while let Some(chr) = chars.next() {
            if chr.is_digit(10) || chr.is_alphabetic() || chr == '_' {
                out.push(chr);
            } else {
                warn!(
                    "`{}` character in string: {} is replaced by `{}`",
                    chr, self, REPLACEMENT_CHAR
                );
                out.push(REPLACEMENT_CHAR);
            }
        }

        out
    }
}

fn to_enum_name(message_id: &MessageId, signal_name: &str) -> TokenStream {
    let enum_name = format_ident!("{}{}", &signal_name.to_camel_case(), message_id.0);
    quote! { #enum_name }
}

pub fn signal_enum(val_desc: &ValueDescription) -> TokenStream {
    if let ValueDescription::Signal {
        ref message_id,
        ref signal_name,
        ref value_descriptions,
    } = val_desc
    {
        let enum_name = to_enum_name(message_id, signal_name);

        let enum_variants = value_descriptions.iter().map(|desc| {
            let name = format_ident!("{}", &desc.b().to_camel_case().to_type_name());
            quote! {
                #name
            }
        });

        let signal_enum_impl_from = signal_enum_impl_from(val_desc).unwrap_or_else(|| quote!());

        quote! {
            #[allow(dead_code)]
            #[derive(Debug, Clone, Copy, PartialEq, Eq)]
            #[repr(u64)]
            pub enum #enum_name {
                #(#enum_variants),*,
                XValue(u64),
            }

            #signal_enum_impl_from
        }
    } else {
        quote!()
    }
}

pub fn signal_enum_impl_from(val_desc: &ValueDescription) -> Option<TokenStream> {
    if let ValueDescription::Signal {
        ref message_id,
        ref signal_name,
        ref value_descriptions,
    } = val_desc
    {
        let enum_name = to_enum_name(message_id, signal_name);

        let value_descriptions = value_descriptions.iter().map(|value_description| {
            let match_left = *value_description.a() as u64;
            let match_right =
                format_ident!("{}", value_description.b().to_camel_case().to_type_name());
            quote! {
                #match_left => Self::#match_right
            }
        });

        Some(quote! {
            impl From<u64> for #enum_name {
                #[allow(dead_code)]
                fn from(val: u64) -> Self {
                    match val {
                        #(#value_descriptions),*,
                        value => Self::XValue(value),
                    }
                }
            }
        })
    } else {
        None
    }
}

pub fn signal_fn_raw(dbc: &DBC, signal: &Signal, message_id: &MessageId) -> Result<TokenStream> {
    let fn_name_raw = format_ident!("{}_{}", signal.name().to_snake_case(), RAW_FN_SUFFIX);

    let (signal_return_type, should_round) = signal_return_type(dbc, message_id, signal);

    let default_signal_comment = format!("Read {} signal from can frame", signal.name());
    let signal_comment = dbc
        .signal_comment(message_id, signal.name())
        .unwrap_or(&default_signal_comment);

    let signal_unit = if !signal.unit().is_empty() {
        format!("\nUnit: {}", signal.unit())
    } else {
        String::default()
    };

    let doc_msg = format!("{}{}", signal_comment, signal_unit);

    let read_byte_order = match signal.byte_order() {
        ByteOrder::LittleEndian => quote! {
            let frame_payload: u64 = LE::read_u64(&self.frame_payload);
        },
        ByteOrder::BigEndian => quote! {
            let  frame_payload: u64 = BE::read_u64(&self.frame_payload);
        },
    };

    let bit_msk_const = 2u64.saturating_pow(*signal.signal_size() as u32) - 1;
    let signal_shift = shift_amount(
        *signal.byte_order(),
        *signal.start_bit(),
        *signal.signal_size(),
    );

    let calc = calc_raw(
        signal,
        &signal_return_type,
        should_round,
        signal_shift,
        bit_msk_const,
    )?;

    Ok(quote! {
        #[doc = #doc_msg]
        #[allow(dead_code)]
        pub fn #fn_name_raw(&self) -> #signal_return_type {
            #read_byte_order
            #calc
        }

    })
}

pub fn signal_fn_enum(signal: &Signal, enum_type: TokenStream) -> Result<TokenStream> {
    let fn_name = format_ident!("{}", &signal.name().to_snake_case());
    let fn_name_raw = format_ident!("{}_{}", signal.name().to_snake_case(), RAW_FN_SUFFIX);

    Ok(quote! {
        #[allow(dead_code)]
        pub fn #fn_name(&self) -> #enum_type {
            #enum_type::from(self.#fn_name_raw() as u64)
        }
    })
}

fn calc_raw(
    signal: &Signal,
    signal_return_type: &TokenStream,
    should_round: bool,
    signal_shift: u64,
    bit_msk_const: u64,
) -> Result<TokenStream> {
    // No shift required if start_bit == 0
    let shift = if signal_shift != 0 {
        quote! {
            (frame_payload >> #signal_shift)
        }
    } else {
        quote! {
            frame_payload
        }
    };

    let cast = if *signal.signal_size() != 1 && *signal.factor() != 1.0 {
        if *signal.signal_size() <= 32 {
            quote! { as f32 }
        } else {
            quote! { as f64 }
        }
    } else {
        quote!()
    };

    let factor = if *signal.factor() != 1.0 {
        let factor = signal.factor();
        quote! { * #factor }
    } else {
        quote!()
    };

    let offset = if *signal.offset() != 0.0 && *signal.signal_size() <= 32 {
        let offset = signal.offset();
        quote! { as f32 + #offset }
    } else if *signal.offset() != 0.0 {
        let offset = signal.offset();
        quote! { as f64 + #offset }
    } else {
        quote!()
    };

    let signal_size = if *signal.signal_size() != 1 {
        if should_round {
            quote! {.round() as #signal_return_type }
        } else {
            quote! { as #signal_return_type }
        }
    } else {
        quote!()
    };

    // boolean signal
    let boolean_signal = if *signal.signal_size() == 1 {
        quote!( == 1)
    } else {
        quote!()
    };

    Ok(quote! {
        ((#shift & #bit_msk_const) #cast #factor #offset) #signal_size #boolean_signal
    })
}

fn signal_return_type(dbc: &DBC, message_id: &MessageId, signal: &Signal) -> (TokenStream, bool) {
    if let Some(extended_value_type) = dbc.extended_value_type_for_signal(message_id, signal.name())
    {
        match extended_value_type {
            SignalExtendedValueType::IEEEfloat32Bit => return (quote! { f32 }, false),
            SignalExtendedValueType::IEEEdouble64bit => return (quote! { f64 }, false),
            SignalExtendedValueType::SignedOrUnsignedInteger => (), // Handled below, also part of the Signal itself
        }
    }

    let should_round = signal.factor != 1.0;

    match (signal.value_type(), signal.signal_size()) {
        (_, signal_size) if *signal_size == 1 => (quote! { bool }, should_round),
        (ValueType::Signed, signal_size) if *signal_size > 1 && *signal_size <= 8 => {
            (quote! { i8 }, should_round)
        }
        (ValueType::Unsigned, signal_size) if *signal_size > 1 && *signal_size <= 8 => {
            (quote! { u8 }, should_round)
        }
        (ValueType::Signed, signal_size) if *signal_size > 8 && *signal_size <= 16 => {
            (quote! { i16 }, should_round)
        }
        (ValueType::Unsigned, signal_size) if *signal_size > 8 && *signal_size <= 16 => {
            (quote! { u16 }, should_round)
        }
        (ValueType::Signed, signal_size) if *signal_size > 16 && *signal_size <= 32 => {
            (quote! { i32 }, should_round)
        }
        (ValueType::Unsigned, signal_size) if *signal_size > 16 && *signal_size <= 32 => {
            (quote! { u32 }, should_round)
        }
        (ValueType::Signed, _) => (quote!(i64), should_round),
        (ValueType::Unsigned, _) => (quote!(u64), should_round),
    }
}

fn shift_amount(byte_order: ByteOrder, start_bit: u64, signal_size: u64) -> u64 {
    match byte_order {
        ByteOrder::LittleEndian => start_bit,
        ByteOrder::BigEndian => 64 - signal_size - ((start_bit / 8) * 8 + (7 - (start_bit % 8))),
    }
}

fn message_const(message: &Message) -> TokenStream {
    // let varname = syn::Ident::new(&concatenated, ident.span());
    let message_name = format_ident!(
        "MESSAGE_ID_{}",
        message.message_name().to_shouty_snake_case()
    );
    let message_id = message.message_id().0;
    quote! {
        #[allow(dead_code)]
        pub const #message_name: u32 = #message_id;

    }
}

fn message_struct(opt: &DbccOpt, dbc: &DBC, message: &Message) -> Result<TokenStream> {
    let struct_name = format_ident!("{}", &message.message_name().to_camel_case());

    let doc_msg = if let Some(message_comment) = dbc.message_comment(message.message_id()) {
        message_comment
    } else {
        ""
    };

    let message_impl = message_impl(opt, dbc, message)?;

    Ok(quote! {
      #[doc = #doc_msg]
      #[allow(dead_code)]
      #[derive(Clone, Debug)]
      pub struct #struct_name {
        frame_payload: Vec<u8>,
      }

      #message_impl
    })
}

fn message_impl(opt: &DbccOpt, dbc: &DBC, message: &Message) -> Result<TokenStream> {
    let struct_name = format_ident!("{}", &message.message_name().to_camel_case());

    let message_stream = if opt.with_tokio {
        message_stream(message)
    } else {
        quote!()
    };

    let signal_fns = message.signals().iter().map(|signal| {
        if *signal.multiplexer_indicator() != MultiplexIndicator::Plain {
            warn!("Multiplexed signals are currently not supported, the message `{}` signal `{}` will be skipped", message.message_name(), signal.name());
            return quote!();
        }

        let signal_fn_raw = signal_fn_raw(dbc, signal, message.message_id()).unwrap();

        // Check if this signal can be turned into an enum
        let enum_type = dbc
            .value_descriptions_for_signal(message.message_id(), signal.name())
            .map(|_| to_enum_name(message.message_id(), signal.name()));
        let signal_fn_enum = if let Some(enum_type) = enum_type {
            signal_fn_enum(signal, enum_type).unwrap()
        } else {
            quote!()
        };

        quote!{
            #signal_fn_raw

            #signal_fn_enum
        }
    });

    let message_id = match message.message_id().0 & EFF_MASK {
        0..=SFF_MASK => {
            let sff_id = message.message_id().0 & SFF_MASK;
            quote! {
                /// CAN Frame Identifier
                pub const ID: u16 = #sff_id;
            }
        }
        SFF_MASK..=EFF_MASK => {
            let eff_id = message.message_id().0 & EFF_MASK;
            quote! {
                /// CAN Frame Identifier
                pub const ID: u32 = #eff_id;
            }
        }
        _ => unreachable!(),
    };

    Ok(quote! {
        impl #struct_name {

            #message_id

            #[allow(dead_code)]
            pub fn new(mut frame_payload: Vec<u8>) -> #struct_name {
                frame_payload.resize(8, 0);
                #struct_name { frame_payload }
            }

            #message_stream

            #(#signal_fns)*
        }
    })
}

/// Generate message stream using socketcan's Broadcast Manager filters via socketcan-tokio.
fn message_stream(message: &Message) -> TokenStream {
    let message_id = match message.message_id().0 & EFF_MASK {
        0..=SFF_MASK => {
            quote! {
                let message_id = CANMessageId::SFF(Self::ID);
            }
        }

        SFF_MASK..=EFF_MASK => {
            quote! {
                let message_id = CANMessageId::EFF(Self::ID);
            }
        }
        _ => unreachable!(),
    };

    let message_name = format_ident!("{}", message.message_name().to_camel_case());

    quote! {
        #[allow(dead_code)]
        pub fn stream(can_interface: &str, ival1: &std::time::Duration, ival2: &std::time::Duration) -> std::io::Result<impl Stream<Item = Result<Self, std::io::Error>>> {
            let socket = BCMSocket::open_nb(&can_interface)?;
            #message_id

            let frame_stream = socket.filter_id_incoming_frames(message_id, ival1.clone(), ival2.clone())?.compat();
            let f = frame_stream.map(|frame| frame.map(|frame| #message_name::new(frame.data().to_vec())));
            Ok(f)
        }
    }
}

/// Genérate code for reading CAN signals
///
/// Example:
/// ```
/// use dbcc::{can_code_gen, DbccOpt};
/// use std::fs::File;
/// use std::io::prelude::*;
/// use std::path::PathBuf;
/// let mut f = File::open("./examples/j1939.dbc").expect("Failed to open input file");
/// let mut buffer = Vec::new();
/// f.read_to_end(&mut buffer).expect("Failed to read file");
/// let dbc_content = can_dbc::DBC::from_slice(&buffer).expect("Failed to parse DBC file");
/// let opt = DbccOpt { with_tokio: true };
/// let code = can_code_gen(&opt, &dbc_content).expect("Failed to generate rust code");
/// println!("{}", code.to_string());
///```
pub fn can_code_gen(opt: &DbccOpt, dbc: &DBC) -> Result<TokenStream> {
    let imports = quote! {
        use byteorder::{ByteOrder, LE, BE};
    };

    let tokio_imports = if opt.with_tokio {
        quote! {

            use tokio_socketcan_bcm::{CANMessageId, BCMSocket};
            use futures::stream::Stream;
            use futures_util::compat::Stream01CompatExt;
            use futures_util::stream::StreamExt;
        }
    } else {
        quote!()
    };

    let message_constants = dbc.messages().iter().map(message_const);

    let signal_enums = dbc.value_descriptions().iter().map(signal_enum);

    let message_structs = dbc
        .messages()
        .iter()
        .map(|message| message_struct(opt, &dbc, message).unwrap());

    Ok(quote! {
        #imports

        #tokio_imports

        #(#message_constants)*

        #(#signal_enums)*

        #(#message_structs)*
    })
}
