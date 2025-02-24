//! # Redis Protocol
//!
//! Structs and functions for implementing the [RESP2](https://redis.io/topics/protocol) and [RESP3](https://github.com/antirez/RESP3/blob/master/spec.md) protocol.
//!
//!
//! ## Examples
//!
//! ```rust
//! extern crate redis_protocol;
//! extern crate bytes;
//!
//! use redis_protocol::resp2::prelude::*;
//! use bytes::BytesMut;
//!
//! fn main() {
//!   let frame = Frame::BulkString("foobar".into());
//!   let mut buf = BytesMut::new();
//!
//!   let len = match encode_bytes(&mut buf, &frame) {
//!     Ok(l) => l,
//!     Err(e) => panic!("Error encoding frame: {:?}", e)
//!   };
//!   println!("Encoded {} bytes into buffer with contents {:?}", len, buf);
//!
//!   let buf: BytesMut = "*3\r\n$3\r\nFoo\r\n$-1\r\n$3\r\nBar\r\n".into();
//!   let (frame, consumed) = match decode(&buf) {
//!     Ok(Some((f, c))) => (f, c),
//!     Ok(None) => panic!("Incomplete frame."),
//!     Err(e) => panic!("Error parsing bytes: {:?}", e)
//!   };
//!   println!("Parsed frame {:?} and consumed {} bytes", frame, consumed);
//!
//!   let key = "foobarbaz";
//!   println!("Hash slot for {}: {}", key, redis_keyslot(key));
//! }
//! ```

#[macro_use]
extern crate log;
extern crate bytes;
extern crate crc16;
extern crate pretty_env_logger;
#[macro_use]
extern crate cookie_factory;
#[macro_use]
extern crate nom;
extern crate float_cmp;

#[cfg(feature = "index-map")]
extern crate indexmap;

#[macro_use]
pub(crate) mod utils;

/// Types and functions for implementing the RESP2 protocol.
pub mod resp2;
/// Types and functions for implementing the RESP3 protocol.
pub mod resp3;
/// Error types and general redis protocol types.
pub mod types;

pub use utils::{digits_in_number, redis_keyslot, resp2_frame_to_resp3, resp3_frame_to_resp2, ZEROED_KB};
