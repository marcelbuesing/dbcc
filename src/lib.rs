use can_dbc::{
    ByteOrder, Message, MessageId, MultiplexIndicator, Signal, SignalExtendedValueType,
    ValueDescription, ValueType, DBC,
};
use proc_macro2::Literal;

use heck::{CamelCase, ShoutySnakeCase, SnakeCase};
use log::warn;
use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use socketcan::{CANFilter, EFF_MASK, SFF_MASK};

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

        for chr in chars {
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

pub fn signal_enum(dbc: &DBC, val_desc: &ValueDescription) -> TokenStream {
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

        let signal_enum_impl_from =
            signal_enum_impl_from(&dbc, val_desc).unwrap_or_else(|| quote!());

        let xvalue = if let Some(signal) = dbc.signal_by_name(*message_id, signal_name) {
            let decoded_type = signal_decoded_type(dbc, *message_id, signal);
            quote! { XValue(#decoded_type) }
        } else {
            quote! { XValue(u64) }
        };

        quote! {
            #[allow(dead_code)]
            #[derive(Debug, Clone, Copy, PartialEq)]
            #[repr(u64)]
            pub enum #enum_name {
                #(#enum_variants),*,
                #xvalue,
            }

            #signal_enum_impl_from
        }
    } else {
        quote!()
    }
}

pub fn signal_enum_impl_from(dbc: &DBC, val_desc: &ValueDescription) -> Option<TokenStream> {
    if let ValueDescription::Signal {
        ref message_id,
        ref signal_name,
        ref value_descriptions,
    } = val_desc
    {
        let enum_name = to_enum_name(message_id, signal_name);

        let signal = dbc
            .signal_by_name(*message_id, signal_name)
            .expect(&format!("Value description missing signal {:#?}", val_desc));
        let signal_type = signal_decoded_type(dbc, *message_id, signal);

        let value_descriptions = value_descriptions.iter().map(|value_description| {
            let match_left = Literal::u64_unsuffixed(*value_description.a() as u64);
            let match_right =
                format_ident!("{}", value_description.b().to_camel_case().to_type_name());
            quote! {
                #match_left => Self::#match_right
            }
        });

        Some(quote! {
            impl From<#signal_type> for #enum_name {
                #[allow(dead_code)]
                fn from(val: #signal_type) -> Self {
                    match val as u64 {
                        #(#value_descriptions),*,
                        _ => Self::XValue(val),
                    }
                }
            }
        })
    } else {
        None
    }
}

pub fn signal_field(dbc: &DBC, signal: &Signal, message_id: MessageId) -> Result<TokenStream> {
    let signal_name_raw = format_ident!("{}_{}", signal.name().to_snake_case(), RAW_FN_SUFFIX);
    let signal_decoded_type = signal_decoded_type(dbc, message_id, signal);

    let ret_type = wrap_multiplex_indicator_type(signal, signal_decoded_type);
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

    Ok(quote! {
        #[doc = #doc_msg]
        #signal_name_raw: #ret_type,
    })
}

pub fn message_decoder(opt: &DbccOpt, dbc: &DBC, message: &Message) -> Result<TokenStream> {
    let raw_signal_decoder = message
        .signals()
        .iter()
        .map(|signal| decode_signal_raw(dbc, signal, *message.message_id()).unwrap());
    let signal_enum_decoder = message.signals().iter().filter_map(|signal| {
        let signal_shift = shift_amount(
            *signal.byte_order(),
            *signal.start_bit(),
            *signal.signal_size(),
        );

        if signal_shift > 63 {
            warn!(
                "Message: {} Signal: {} exceeds 8 byte shift and will be skipped",
                message.message_id().0,
                signal.name()
            );
            None
        } else {
            // Check if this signal can be turned into an enum
            let enum_type = dbc
                .value_descriptions_for_signal(*message.message_id(), signal.name())
                .map(|_| to_enum_name(message.message_id(), signal.name()));
            if let Some(enum_type) = enum_type {
                Some(decode_signal_enum(signal, enum_type).unwrap())
            } else {
                None
            }
        }
    });
    let signal_fields = message.signals().iter().filter_map(|signal| {
        let signal_shift = shift_amount(
            *signal.byte_order(),
            *signal.start_bit(),
            *signal.signal_size(),
        );

        if signal_shift > 63 {
            warn!(
                "Message: {} Signal: {} exceeds 8 byte shift and will be skipped",
                message.message_id().0,
                signal.name()
            );
            None
        } else {
            Some(format_ident!(
                "{}_{}",
                signal.name().to_snake_case(),
                RAW_FN_SUFFIX
            ))
        }
    });

    let signal_enum_fields = message.signals().iter().filter_map(|signal| {
        let signal_shift = shift_amount(
            *signal.byte_order(),
            *signal.start_bit(),
            *signal.signal_size(),
        );

        if signal_shift > 63 {
            warn!(
                "Message: {} Signal: {} exceeds 8 byte shift and will be skipped",
                message.message_id().0,
                signal.name()
            );
            None
        } else {
            let enum_type = dbc
                .value_descriptions_for_signal(*message.message_id(), signal.name())
                .map(|_| to_enum_name(message.message_id(), signal.name()));
            if let Some(enum_type) = enum_type {
                Some(format_ident!("{}", &signal.name().to_snake_case()))
            } else {
                None
            }
        }
    });
    let struct_name = format_ident!("{}", &message.message_name().to_camel_case());
    let frame_id = match message.message_id().0 & EFF_MASK {
        0..=SFF_MASK => {
            let sff_id = (message.message_id().0 & SFF_MASK) as u32;
            quote! {#sff_id}
        }
        SFF_MASK..=EFF_MASK => {
            let eff_id = message.message_id().0 & EFF_MASK as u32;
            quote! { #eff_id }
        }
        _ => unreachable!(),
    };

    Ok(quote! {
        #frame_id => {
            #(#raw_signal_decoder)*
            #(#signal_enum_decoder)*

            DecodedFrame::#struct_name {
                #(#signal_fields,)*
                #(#signal_enum_fields,)*
            }
        },
    })
}

pub fn decode_signal_raw(dbc: &DBC, signal: &Signal, message_id: MessageId) -> Result<TokenStream> {
    let signal_name_raw = format_ident!("{}_{}", signal.name().to_snake_case(), RAW_FN_SUFFIX);

    let signal_decoded_type = signal_decoded_type(dbc, message_id, signal);

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

    // Multiplexed signals are only available when the multiplexer switch value matches
    // the multiplexed indicator value defined in the DBC.
    let multiplexor_switch_fn = if let MultiplexIndicator::MultiplexedSignal(switch_value) =
        signal.multiplexer_indicator()
    {
        let multiplexor_switch = dbc.message_multiplexor_switch(message_id).expect(&format!(
            "Multiplexed signal missing multiplex signal switch in message: {:#?}",
            signal
        ));

        let multiplexor_switch_fn = format_ident!(
            "{}_{}",
            multiplexor_switch.name().to_snake_case(),
            RAW_FN_SUFFIX
        );

        let switch_value = Literal::u64_unsuffixed(*switch_value);
        quote! {
            todo!();
            // if self.#multiplexor_switch_fn() != #switch_value {
            //     return None;
            // }
        }
    } else {
        quote!()
    };

    let read_byte_order = match signal.byte_order() {
        ByteOrder::LittleEndian => quote! {
            let frame_payload: u64 = LE::read_u64(frame.data());
        },
        ByteOrder::BigEndian => quote! {
            let  frame_payload: u64 = BE::read_u64(frame.data());
        },
    };

    let bit_msk_const = 2u64.saturating_pow(*signal.signal_size() as u32) - 1;
    let signal_shift = shift_amount(
        *signal.byte_order(),
        *signal.start_bit(),
        *signal.signal_size(),
    );

    if signal_shift > 63 {
        warn!(
            "Message: {} Signal: {} exceeds 8 byte shift and will be skipped",
            message_id.0,
            signal.name()
        );
        return Ok(quote!());
    }

    let calc = calc_raw(signal, &signal_decoded_type, signal_shift, bit_msk_const)?;

    let wrapped_calc = wrap_multiplex_indicator_value(signal, calc);
    let ret_type = wrap_multiplex_indicator_type(signal, signal_decoded_type);

    Ok(quote! {
        #multiplexor_switch_fn
        #read_byte_order
        let #signal_name_raw: #ret_type = #wrapped_calc;
    })
}

pub fn decode_signal_enum(signal: &Signal, enum_type: TokenStream) -> Result<TokenStream> {
    let field_name = format_ident!("{}", &signal.name().to_snake_case());
    let field_name_raw = format_ident!("{}_{}", signal.name().to_snake_case(), RAW_FN_SUFFIX);

    // let ret = wrap_multiplex_indicator_type(signal, enum_type.clone());

    // Multiplexed signals are only available when th
    // the multiplexed indicator value defined in the DBC.
    let from = match signal.multiplexer_indicator() {
        MultiplexIndicator::MultiplexedSignal(_) => {
            quote!(todo!())
            // quote! { #field_name().map(#enum_type::from) }
        }
        _ => quote! { #enum_type::from(#field_name_raw) },
    };

    Ok(quote! {
        let #field_name = #from;
    })
}

fn calc_raw(
    signal: &Signal,
    signal_decoded_type: &TokenStream,
    signal_shift: u64,
    bit_msk_const: u64,
) -> Result<TokenStream> {
    let boolean_signal =
        *signal.signal_size() == 1 && *signal.factor() == 1.0 && *signal.offset() == 0.0;
    let bit_msk_const = Literal::u64_unsuffixed(bit_msk_const);
    // No shift required if start_bit == 0
    let shift = if signal_shift != 0 {
        let signal_shift = Literal::u64_unsuffixed(signal_shift);
        quote! {
            (frame_payload >> #signal_shift)
        }
    } else {
        quote! {
            frame_payload
        }
    };

    let cast = if !boolean_signal {
        quote! { as #signal_decoded_type }
    } else {
        quote!()
    };

    let factor = if *signal.factor() != 1.0 {
        let signal_factor = Literal::f64_unsuffixed(*signal.factor());
        quote! { * #signal_factor }
    } else {
        quote!()
    };

    let offset = if *signal.offset() != 0.0 {
        let offset = Literal::f64_unsuffixed(*signal.offset());
        quote! { + #offset }
    } else {
        quote!()
    };

    // boolean signal
    if boolean_signal {
        Ok(quote! {
            ((#shift & #bit_msk_const) #cast #factor #offset) == 1
        })
    } else {
        Ok(quote! {
            (#shift & #bit_msk_const) #cast #factor #offset
        })
    }
}

/// This wraps multiplex indicators in  Option types.
/// Multiplexed signals are only available when the multiplexer switch value matches
/// the multiplexed indicator value defined in the DBC.
fn wrap_multiplex_indicator_type(signal: &Signal, signal_type: TokenStream) -> TokenStream {
    match signal.multiplexer_indicator() {
        MultiplexIndicator::MultiplexedSignal(_) => quote! { Option<#signal_type> },
        _ => signal_type,
    }
}

/// This wraps multiplex indicators in  Option types.
/// Multiplexed signals are only available when the multiplexer switch value matches
/// the multiplexed indicator value defined in the DBC.
fn wrap_multiplex_indicator_value(signal: &Signal, signal_value: TokenStream) -> TokenStream {
    match signal.multiplexer_indicator() {
        MultiplexIndicator::MultiplexedSignal(_) => quote! { Some(#signal_value) },
        _ => signal_value,
    }
}

fn signal_decoded_type(dbc: &DBC, message_id: MessageId, signal: &Signal) -> TokenStream {
    if let Some(extended_value_type) = dbc.extended_value_type_for_signal(message_id, signal.name())
    {
        match extended_value_type {
            SignalExtendedValueType::IEEEfloat32Bit => {
                return quote! { f32 };
            }
            SignalExtendedValueType::IEEEdouble64bit => {
                return quote! { f64 };
            }
            SignalExtendedValueType::SignedOrUnsignedInteger => (), // Handled below, also part of the Signal itself
        }
    }

    if !(*signal.offset() == 0.0 && *signal.factor() == 1.0) {
        return quote! { f64 };
    }

    match (signal.value_type(), signal.signal_size()) {
        (_, signal_size) if *signal_size == 1 => quote! { bool },
        (ValueType::Signed, signal_size) if *signal_size > 1 && *signal_size <= 8 => quote! { i8 },
        (ValueType::Unsigned, signal_size) if *signal_size > 1 && *signal_size <= 8 => {
            quote! { u8 }
        }
        (ValueType::Signed, signal_size) if *signal_size > 8 && *signal_size <= 16 => {
            quote! { i16 }
        }
        (ValueType::Unsigned, signal_size) if *signal_size > 8 && *signal_size <= 16 => {
            quote! { u16 }
        }
        (ValueType::Signed, signal_size) if *signal_size > 16 && *signal_size <= 32 => {
            quote! { i32 }
        }
        (ValueType::Unsigned, signal_size) if *signal_size > 16 && *signal_size <= 32 => {
            quote! { u32 }
        }
        (ValueType::Signed, _) => quote!(i64),
        (ValueType::Unsigned, _) => quote!(u64),
    }
}

fn shift_amount(byte_order: ByteOrder, start_bit: u64, signal_size: u64) -> u64 {
    match byte_order {
        ByteOrder::LittleEndian => start_bit,
        ByteOrder::BigEndian => 63 - signal_size - ((start_bit / 8) * 8 + (7 - (start_bit % 8))),
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

    let doc_msg = if let Some(message_comment) = dbc.message_comment(*message.message_id()) {
        message_comment
    } else {
        ""
    };

    let signal_fields = message.signals().iter().filter_map(|signal| {
        let signal_shift = shift_amount(
            *signal.byte_order(),
            *signal.start_bit(),
            *signal.signal_size(),
        );

        if signal_shift > 63 {
            warn!(
                "Message: {} Signal: {} exceeds 8 byte shift and will be skipped",
                message.message_id().0,
                signal.name()
            );
            None
        } else {
            Some(signal_field(dbc, signal, *message.message_id()).unwrap())
        }
    });

    let signal_enum_fields = message.signals().iter().filter_map(|signal| {
        let signal_shift = shift_amount(
            *signal.byte_order(),
            *signal.start_bit(),
            *signal.signal_size(),
        );

        if signal_shift > 63 {
            warn!(
                "Message: {} Signal: {} exceeds 8 byte shift and will be skipped",
                message.message_id().0,
                signal.name()
            );
            None
        } else {
            let enum_type = dbc
                .value_descriptions_for_signal(*message.message_id(), signal.name())
                .map(|_| to_enum_name(message.message_id(), signal.name()));
            if let Some(enum_type) = enum_type {
                let field_name = format_ident!("{}", &signal.name().to_snake_case());

                let default_signal_comment =
                    format!("Read {} signal from can frame", signal.name());
                let signal_comment = dbc
                    .signal_comment(*message.message_id(), signal.name())
                    .unwrap_or(&default_signal_comment);

                let signal_unit = if !signal.unit().is_empty() {
                    format!("\nUnit: {}", signal.unit())
                } else {
                    String::default()
                };

                let doc_msg = format!("{}{}", signal_comment, signal_unit);

                Some(quote! {
                    #[doc = #doc_msg]
                    #field_name: #enum_type,
                })
            } else {
                None
            }
        }
    });

    Ok(quote! {
      #[doc = #doc_msg]
      #struct_name {
        #(#signal_fields)*
        #(#signal_enum_fields)*
      }
    })
}

/// Generate message stream using socketcan's Broadcast Manager filters via socketcan-tokio.
fn message_stream(message: &Message) -> TokenStream {
    let message_id = match message.message_id().0 & EFF_MASK {
        0..=SFF_MASK => {
            quote! {
                let message_id = Self::ID;
            }
        }

        SFF_MASK..=EFF_MASK => {
            quote! {
                let message_id = Self::ID | EFF_FLAG ;
            }
        }
        _ => unreachable!(),
    };

    let message_name = format_ident!("{}", message.message_name().to_camel_case());

    quote! {
        #[allow(dead_code)]
        pub fn stream(can_interface: &str, _ival1: &std::time::Duration, _ival2: &std::time::Duration) -> std::io::Result<impl Stream<Item = Result<Self, std::io::Error>>> {
            let socket = tokio_socketcan::CANSocket::open(&can_interface).unwrap();
            #message_id
            // Classic Filter
            // 0x2000_0000 == INV_FILTER
            let message_id  = message_id as u32 | 0x2000_0000;
            let mask = message_id as u32 | EFF_FLAG;
            let can_filter = CANFilter::new(message_id.into(), mask).unwrap();
            socket.set_filter(&[can_filter]).unwrap();
            let f = socket.map(|frame| frame.map(|frame| #message_name::new(frame.data().to_vec())));
            Ok(f)
        }
    }
}

/// Genérate code for reading CAN signals
///
/// Example:
/// ```
/// use blake2::{Blake2b, Digest};
/// use dbcc::{can_code_gen, DbccOpt};
/// use generic_array::GenericArray;
/// use typenum::U64;
///
/// use std::fs;
/// use std::io::{self, prelude::*};
/// use std::path::{Path, PathBuf};
///
/// fn dbc_file_hash(dbc_path: &Path) -> io::Result<GenericArray<u8, U64>> {
///     let mut file = fs::File::open(&dbc_path)?;
///     let mut hasher = Blake2b::new();
///     let _n = io::copy(&mut file, &mut hasher)?;
///     Ok(hasher.result())
/// }
///
/// fn main() -> io::Result<()> {
///    let file_path_buf = PathBuf::from("./examples/j1939.dbc");
///    let file_path = file_path_buf.as_path();
///    let file_name = file_path.file_name().and_then(|f| f.to_str()).unwrap_or_else(|| "N/A");
///    let file_hash = dbc_file_hash(file_path)?;
///    let file_hash = format!("Blake2b: {:X}", file_hash);
///    let mut f = fs::File::open("./examples/j1939.dbc").expect("Failed to open input file");
///    let mut buffer = Vec::new();
///    f.read_to_end(&mut buffer).expect("Failed to read file");
///    let dbc_content = can_dbc::DBC::from_slice(&buffer).expect("Failed to parse DBC file");
///    let opt = DbccOpt { with_tokio: true };
///    let code = can_code_gen(&opt, &dbc_content, file_name, &file_hash).expect("Failed to generate rust code");
///    println!("{}", code.to_string());
///    Ok(())
/// }
///```
pub fn can_code_gen(
    opt: &DbccOpt,
    dbc: &DBC,
    file_name: &str,
    file_hash: &str,
) -> Result<TokenStream> {
    let imports = quote! {
        use byteorder::{ByteOrder, LE, BE};
    };

    let tokio_imports = if opt.with_tokio {
        quote! {
            use socketcan::{CANFilter, EFF_MASK, EFF_FLAG, SFF_MASK};
            use tokio_socketcan::CANSocket;
            use futures::stream::{Stream, StreamExt, TryStreamExt};
        }
    } else {
        quote!()
    };

    let message_constants = dbc.messages().iter().map(message_const);

    let signal_enums = dbc
        .value_descriptions()
        .iter()
        .map(|vd| signal_enum(&dbc, vd));

    let message_enums = dbc
        .messages()
        .iter()
        .map(|message| message_struct(opt, &dbc, message).unwrap());

    let message_decoders = dbc
        .messages()
        .iter()
        .map(|message| message_decoder(opt, &dbc, message).unwrap());

    let doc_msg = format!(
        "Generated based on\nFile Name: {}\nDBC Version: {}\n{}",
        file_name,
        dbc.version().0,
        file_hash
    );

    Ok(quote! {

        #[doc = #doc_msg]

        #imports

        #tokio_imports

        #(#message_constants)*

        #(#signal_enums)*

        #[derive(Debug, Clone)]
        pub enum DecodedFrame {
            /// Frame that is not listed in dbc
            UnknownFrame(tokio_socketcan::CANFrame),
            #(#message_enums,)*
        }

        pub struct DecodedFrameStream {
            can_socket: tokio_socketcan::CANSocket,
        }

        impl DecodedFrameStream {
            pub fn new(can_socket: tokio_socketcan::CANSocket) -> Self {
                Self {
                    can_socket
                }
            }

            pub fn stream(self) -> impl futures::Stream<Item = Result<DecodedFrame, std::io::Error>> {
                self.can_socket.map_ok(|frame| {
                    match frame.id() {
                        #(#message_decoders)*
                        _ => DecodedFrame::UnknownFrame(frame), // Ignore, unknown frame will not be decoded
                    }
                })
            }
        }
    })
}
