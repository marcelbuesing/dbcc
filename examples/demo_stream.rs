//! If you are using Rust 2018 no `external crate byteorder;` is necessary
//! The `gen` module exists because `cargo test` fails
//! if `j1939.rs` is directly in the examples folder
//! (since it's treated like an example and a `main` is expected).
//! Usually you could directly use `mod j1939;` here.
//! T
//!
//! Hazard Lamps Flashing
//! ```
//! cansend vcan0 0CFDCCFE#11111111111111;
//! ```
//!
//! ```
//!cansend vcan0 0CFDCCFE#00000000000000;
//! ```
//! Hazard Lights To Be Off

mod gen;

/// Generated module
use crate::gen::j1939::{DecodedFrame, DecodedFrameStream};

use futures_util::stream::StreamExt;
use std::io;
use tokio_socketcan::CANSocket;

#[tokio::main]
async fn main() -> io::Result<()> {
    let socket = CANSocket::open("vcan0").unwrap();
    let mut decoded_stream = DecodedFrameStream::new(socket).stream();

    while let Some(Ok(decoded_frame)) = decoded_stream.next().await {
        // Signal indicates the selected position of the operator's hazard light switch.
        match decoded_frame {
            frame @ DecodedFrame::Oel { .. } => {
                println!("{:#?}", frame);
            }
            _ => (), // Just ignore the rest
        }
    }

    Ok(())
}
