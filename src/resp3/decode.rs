//! Functions for decoding the RESP3 protocol into frames.
//!
//! <https://github.com/antirez/RESP3/blob/master/spec.md>

use crate::resp3::types::*;
use crate::resp3::utils as resp3_utils;
use crate::types::*;
use nom::bytes::streaming::{take as nom_take, take_until as nom_take_until};
use nom::combinator::{map as nom_map, map_res as nom_map_res, opt as nom_opt};
use nom::multi::count as nom_count;
use nom::number::streaming::be_u8;
use nom::sequence::terminated as nom_terminated;
use nom::{Err as NomErr, IResult};
use std::borrow::Cow;
use std::str;

macro_rules! e (
  ($err:expr) => {
    return Err($err.into_nom_error());
  }
);

macro_rules! etry (
  ($expr:expr) => {
    match $expr {
      Ok(result) => result,
      Err(e) => return Err(e.into_nom_error())
    }
  }
);

fn map_complete_frame(frame: Frame) -> DecodedFrame {
  DecodedFrame::Complete(frame)
}

fn unwrap_complete_frame<'a>(frame: DecodedFrame) -> Result<Frame, RedisParseError<&'a [u8]>> {
  frame
    .into_complete_frame()
    .map_err(|e| RedisParseError::new_custom("unwrap_complete_frame", format!("{:?}", e)))
}

fn to_usize(s: &str) -> Result<usize, RedisParseError<&[u8]>> {
  s.parse::<usize>()
    .map_err(|e| RedisParseError::new_custom("to_usize", format!("{:?}", e)))
}

fn to_isize(s: &str) -> Result<isize, RedisParseError<&[u8]>> {
  if s == "?" {
    Ok(-1)
  } else {
    s.parse::<isize>()
      .map_err(|e| RedisParseError::new_custom("to_isize", format!("{:?}", e)))
  }
}

fn isize_to_usize<'a>(n: isize) -> Result<usize, RedisParseError<&'a [u8]>> {
  if n.is_negative() {
    Err(RedisParseError::new_custom("isize_to_usize", "Invalid prefix length."))
  } else {
    Ok(n as usize)
  }
}

fn to_i64(s: &str) -> Result<i64, RedisParseError<&[u8]>> {
  s.parse::<i64>()
    .map_err(|e| RedisParseError::new_custom("to_i64", format!("{:?}", e)))
}

fn to_f64(s: &str) -> Result<f64, RedisParseError<&[u8]>> {
  s.parse::<f64>()
    .map_err(|e| RedisParseError::new_custom("to_f64", format!("{:?}", e)))
}

fn to_bool(s: &str) -> Result<bool, RedisParseError<&[u8]>> {
  match s.as_ref() {
    "t" => Ok(true),
    "f" => Ok(false),
    _ => Err(RedisParseError::new_custom("to_bool", "Invalid boolean value.")),
  }
}

fn to_verbatimstring_format(s: &str) -> Result<VerbatimStringFormat, RedisParseError<&[u8]>> {
  match s.as_ref() {
    "txt" => Ok(VerbatimStringFormat::Text),
    "mkd" => Ok(VerbatimStringFormat::Markdown),
    _ => Err(RedisParseError::new_custom(
      "to_verbatimstring_format",
      "Invalid format.",
    )),
  }
}

fn to_map<'a>(mut data: Vec<Frame>) -> Result<FrameMap, RedisParseError<&'a [u8]>> {
  if data.len() % 2 != 0 {
    return Err(RedisParseError::new_custom("to_map", "Invalid hashmap frame length."));
  }

  let mut out = resp3_utils::new_map(Some(data.len() / 2));
  while data.len() >= 2 {
    let value = data.pop().unwrap();
    let key = data.pop().unwrap();

    out.insert(key, value);
  }

  Ok(out)
}

fn to_set<'a>(data: Vec<Frame>) -> Result<FrameSet, RedisParseError<&'a [u8]>> {
  let mut out = resp3_utils::new_set(Some(data.len()));

  for frame in data.into_iter() {
    out.insert(frame);
  }

  Ok(out)
}

fn to_hello<'a>(version: u8, auth: Option<(&str, &str)>) -> Result<Frame, RedisParseError<&'a [u8]>> {
  let version = match version {
    2 => RespVersion::RESP2,
    3 => RespVersion::RESP3,
    _ => {
      return Err(RedisParseError::new_custom("parse_hello", "Invalid RESP version."));
    }
  };
  let auth = if let Some((username, password)) = auth {
    Some(Auth {
      username: Cow::Owned(username.to_owned()),
      password: Cow::Owned(password.to_owned()),
    })
  } else {
    None
  };

  Ok(Frame::Hello { version, auth })
}

fn attach_attributes<'a>(
  attributes: Attributes,
  mut frame: DecodedFrame,
) -> Result<DecodedFrame, RedisParseError<&'a [u8]>> {
  if let Err(e) = frame.add_attributes(attributes) {
    Err(RedisParseError::new_custom("attach_attributes", format!("{:?}", e)))
  } else {
    Ok(frame)
  }
}

fn d_read_to_crlf(input: &[u8]) -> IResult<&[u8], &[u8], RedisParseError<&[u8]>> {
  nom_terminated(nom_take_until(CRLF), nom_take(2_usize))(input)
}

fn d_read_to_crlf_s(input: &[u8]) -> IResult<&[u8], &str, RedisParseError<&[u8]>> {
  nom_map_res(d_read_to_crlf, str::from_utf8)(input)
}

fn d_read_prefix_len(input: &[u8]) -> IResult<&[u8], usize, RedisParseError<&[u8]>> {
  nom_map_res(d_read_to_crlf_s, to_usize)(input)
}

fn d_read_prefix_len_signed(input: &[u8]) -> IResult<&[u8], isize, RedisParseError<&[u8]>> {
  nom_map_res(d_read_to_crlf_s, to_isize)(input)
}

fn d_frame_type(input: &[u8]) -> IResult<&[u8], FrameKind, RedisParseError<&[u8]>> {
  let (input, byte) = be_u8(input)?;
  let kind = match FrameKind::from_byte(byte) {
    Some(k) => k,
    None => e!(RedisParseError::new_custom("frame_type", "Invalid frame type prefix.")),
  };

  Ok((input, kind))
}

fn d_parse_simplestring(input: &[u8]) -> IResult<&[u8], Frame, RedisParseError<&[u8]>> {
  let (input, data) = d_read_to_crlf_s(input)?;

  Ok((
    input,
    Frame::SimpleString {
      data: data.to_owned(),
      attributes: None,
    },
  ))
}

fn d_parse_simpleerror(input: &[u8]) -> IResult<&[u8], Frame, RedisParseError<&[u8]>> {
  let (input, data) = d_read_to_crlf_s(input)?;

  Ok((
    input,
    Frame::SimpleError {
      data: data.to_owned(),
      attributes: None,
    },
  ))
}

fn d_parse_number(input: &[u8]) -> IResult<&[u8], Frame, RedisParseError<&[u8]>> {
  let (input, data) = nom_map_res(d_read_to_crlf_s, to_i64)(input)?;

  Ok((input, Frame::Number { data, attributes: None }))
}

fn d_parse_double(input: &[u8]) -> IResult<&[u8], Frame, RedisParseError<&[u8]>> {
  let (input, data) = nom_map_res(d_read_to_crlf_s, to_f64)(input)?;

  Ok((input, Frame::Double { data, attributes: None }))
}

fn d_parse_boolean(input: &[u8]) -> IResult<&[u8], Frame, RedisParseError<&[u8]>> {
  let (input, data) = nom_map_res(d_read_to_crlf_s, to_bool)(input)?;

  Ok((input, Frame::Boolean { data, attributes: None }))
}

fn d_parse_null(input: &[u8]) -> IResult<&[u8], Frame, RedisParseError<&[u8]>> {
  let (input, _) = d_read_to_crlf_s(input)?;
  Ok((input, Frame::Null))
}

fn d_parse_blobstring(input: &[u8], len: usize) -> IResult<&[u8], Frame, RedisParseError<&[u8]>> {
  let (input, data) = nom_terminated(nom_take(len), nom_take(2_usize))(input)?;

  Ok((
    input,
    Frame::BlobString {
      data: data.to_vec(),
      attributes: None,
    },
  ))
}

fn d_parse_bloberror(input: &[u8]) -> IResult<&[u8], Frame, RedisParseError<&[u8]>> {
  let (input, len) = d_read_prefix_len(input)?;
  let (input, data) = nom_terminated(nom_take(len), nom_take(2_usize))(input)?;

  Ok((
    input,
    Frame::BlobError {
      data: data.to_vec(),
      attributes: None,
    },
  ))
}

fn d_parse_verbatimstring(input: &[u8]) -> IResult<&[u8], Frame, RedisParseError<&[u8]>> {
  let (input, len) = d_read_prefix_len(input)?;
  let (input, format) = nom_map_res(nom_terminated(nom_take(3_usize), nom_take(1_usize)), str::from_utf8)(input)?;
  let format = etry!(to_verbatimstring_format(format));
  let (input, data) = nom_terminated(nom_take(len - 4), nom_take(2_usize))(input)?;

  Ok((
    input,
    Frame::VerbatimString {
      data: data.to_vec(),
      format,
      attributes: None,
    },
  ))
}

fn d_parse_bignumber(input: &[u8]) -> IResult<&[u8], Frame, RedisParseError<&[u8]>> {
  let (input, data) = d_read_to_crlf(input)?;

  Ok((
    input,
    Frame::BigNumber {
      data: data.to_vec(),
      attributes: None,
    },
  ))
}

fn d_parse_array_frames(input: &[u8], len: usize) -> IResult<&[u8], Vec<Frame>, RedisParseError<&[u8]>> {
  nom_count(nom_map_res(d_parse_frame_or_attribute, unwrap_complete_frame), len)(input)
}

fn d_parse_kv_pairs(input: &[u8], len: usize) -> IResult<&[u8], FrameMap, RedisParseError<&[u8]>> {
  nom_map_res(
    nom_count(nom_map_res(d_parse_frame_or_attribute, unwrap_complete_frame), len * 2),
    to_map,
  )(input)
}

fn d_parse_array(input: &[u8], len: usize) -> IResult<&[u8], Frame, RedisParseError<&[u8]>> {
  let (input, data) = d_parse_array_frames(input, len)?;

  Ok((input, Frame::Array { data, attributes: None }))
}

fn d_parse_push(input: &[u8]) -> IResult<&[u8], Frame, RedisParseError<&[u8]>> {
  let (input, len) = d_read_prefix_len(input)?;
  let (input, data) = d_parse_array_frames(input, len)?;

  Ok((input, Frame::Push { data, attributes: None }))
}

fn d_parse_set(input: &[u8], len: usize) -> IResult<&[u8], Frame, RedisParseError<&[u8]>> {
  let (input, frames) = d_parse_array_frames(input, len)?;
  let set = etry!(to_set(frames));

  Ok((
    input,
    Frame::Set {
      data: set,
      attributes: None,
    },
  ))
}

fn d_parse_map(input: &[u8], len: usize) -> IResult<&[u8], Frame, RedisParseError<&[u8]>> {
  let (input, frames) = d_parse_kv_pairs(input, len)?;

  Ok((
    input,
    Frame::Map {
      data: frames,
      attributes: None,
    },
  ))
}

fn d_parse_attribute(input: &[u8]) -> IResult<&[u8], Attributes, RedisParseError<&[u8]>> {
  let (input, len) = d_read_prefix_len(input)?;
  let (input, attributes) = d_parse_kv_pairs(input, len)?;

  Ok((input, attributes))
}

fn d_parse_hello(input: &[u8]) -> IResult<&[u8], Frame, RedisParseError<&[u8]>> {
  let (input, _) = nom_map_res(nom_terminated(nom_take_until(HELLO), nom_take(1_usize)), str::from_utf8)(input)?;
  let (input, version) = be_u8(input)?;
  let (input, auth) = nom_opt(nom_map_res(
    nom_terminated(nom_take_until(AUTH), nom_take(1_usize)),
    str::from_utf8,
  ))(input)?;

  let (input, auth) = if auth.is_some() {
    let (input, username) = nom_map_res(
      nom_terminated(nom_take_until(EMPTY_SPACE), nom_take(1_usize)),
      str::from_utf8,
    )(input)?;
    let (input, password) = nom_map_res(
      nom_terminated(nom_take_until(EMPTY_SPACE), nom_take(1_usize)),
      str::from_utf8,
    )(input)?;

    (input, Some((username, password)))
  } else {
    (input, None)
  };

  Ok((input, etry!(to_hello(version, auth))))
}

/// Check for a streaming variant of a frame, and if found then return the prefix bytes only, otherwise return the complete frame.
///
/// Only supported for arrays, sets, maps, and blob strings.
fn d_check_streaming(input: &[u8], kind: FrameKind) -> IResult<&[u8], DecodedFrame, RedisParseError<&[u8]>> {
  let (input, len) = d_read_prefix_len_signed(input)?;
  let (input, frame) = if len == -1 {
    (input, DecodedFrame::Streaming(StreamedFrame::new(kind)))
  } else {
    let len = etry!(isize_to_usize(len));
    let (input, frame) = match kind {
      FrameKind::Array => d_parse_array(input, len)?,
      FrameKind::Set => d_parse_set(input, len)?,
      FrameKind::Map => d_parse_map(input, len)?,
      FrameKind::BlobString => d_parse_blobstring(input, len)?,
      _ => e!(RedisParseError::new_custom(
        "check_streaming",
        format!("Invalid frame type: {:?}", kind)
      )),
    };

    (input, DecodedFrame::Complete(frame))
  };

  Ok((input, frame))
}

fn d_parse_chunked_string(input: &[u8]) -> IResult<&[u8], DecodedFrame, RedisParseError<&[u8]>> {
  let (input, len) = d_read_prefix_len(input)?;
  let (input, frame) = if len == 0 {
    (input, Frame::new_end_stream())
  } else {
    let (input, contents) = nom_terminated(nom_take(len), nom_take(2_usize))(input)?;
    (input, Frame::ChunkedString(Vec::from(contents)))
  };

  Ok((input, DecodedFrame::Complete(frame)))
}

fn d_return_end_stream(input: &[u8]) -> IResult<&[u8], DecodedFrame, RedisParseError<&[u8]>> {
  let (input, _) = d_read_to_crlf(input)?;
  Ok((input, DecodedFrame::Complete(Frame::new_end_stream())))
}

fn d_parse_non_attribute_frame(input: &[u8], kind: FrameKind) -> IResult<&[u8], DecodedFrame, RedisParseError<&[u8]>> {
  let (input, frame) = match kind {
    FrameKind::Array => d_check_streaming(input, kind)?,
    FrameKind::BlobString => d_check_streaming(input, kind)?,
    FrameKind::Map => d_check_streaming(input, kind)?,
    FrameKind::Set => d_check_streaming(input, kind)?,
    FrameKind::SimpleString => nom_map(d_parse_simplestring, map_complete_frame)(input)?,
    FrameKind::SimpleError => nom_map(d_parse_simpleerror, map_complete_frame)(input)?,
    FrameKind::Number => nom_map(d_parse_number, map_complete_frame)(input)?,
    FrameKind::Null => nom_map(d_parse_null, map_complete_frame)(input)?,
    FrameKind::Double => nom_map(d_parse_double, map_complete_frame)(input)?,
    FrameKind::Boolean => nom_map(d_parse_boolean, map_complete_frame)(input)?,
    FrameKind::BlobError => nom_map(d_parse_bloberror, map_complete_frame)(input)?,
    FrameKind::VerbatimString => nom_map(d_parse_verbatimstring, map_complete_frame)(input)?,
    FrameKind::Push => nom_map(d_parse_push, map_complete_frame)(input)?,
    FrameKind::BigNumber => nom_map(d_parse_bignumber, map_complete_frame)(input)?,
    FrameKind::Hello => nom_map(d_parse_hello, map_complete_frame)(input)?,
    FrameKind::ChunkedString => d_parse_chunked_string(input)?,
    FrameKind::EndStream => d_return_end_stream(input)?,
    FrameKind::Attribute => {
      error!("Found unexpected attribute frame.");
      e!(RedisParseError::new_custom(
        "parse_non_attribute_frame",
        "Unexpected attribute frame.",
      ));
    }
  };

  Ok((input, frame))
}

fn d_parse_attribute_and_frame(input: &[u8]) -> IResult<&[u8], DecodedFrame, RedisParseError<&[u8]>> {
  let (input, attributes) = d_parse_attribute(input)?;
  let (input, kind) = d_frame_type(input)?;
  let (input, next_frame) = d_parse_non_attribute_frame(input, kind)?;
  let frame = etry!(attach_attributes(attributes, next_frame));

  Ok((input, frame))
}

fn d_parse_frame_or_attribute(input: &[u8]) -> IResult<&[u8], DecodedFrame, RedisParseError<&[u8]>> {
  let (input, kind) = d_frame_type(input)?;
  let (input, frame) = if let FrameKind::Attribute = kind {
    d_parse_attribute_and_frame(input)?
  } else {
    d_parse_non_attribute_frame(input, kind)?
  };

  Ok((input, frame))
}

/// Decoding functions for complete frames. **If a streamed frame is detected it will result in an error.**
///
/// Implement a [codec](https://docs.rs/tokio-util/0.6.6/tokio_util/codec/index.html) that only supports complete frames...
///
/// ```edition2018 no_run
/// # extern crate tokio_util;
/// # extern crate tokio;
/// # extern crate bytes;
///
/// use redis_protocol::resp3::types::*;
/// use redis_protocol::types::{RedisProtocolError, RedisProtocolErrorKind};
/// use redis_protocol::resp3::decode::complete::*;
/// use redis_protocol::resp3::encode::complete::*;
/// use bytes::BytesMut;
/// use tokio_util::codec::{Decoder, Encoder};
///
/// pub struct RedisCodec {}
///
/// impl Encoder<Frame> for RedisCodec {
///   type Error = RedisProtocolError;
///
///   fn encode(&mut self, item: Frame, dst: &mut BytesMut) -> Result<(), Self::Error> {
///     // in this example we only show support for encoding complete frames
///     let _ = encode_bytes(dst, &item)?;
///     Ok(())
///   }
/// }
///
/// impl Decoder for RedisCodec {
///   type Item = Frame;
///   type Error = RedisProtocolError;
///
///   fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
///     if src.is_empty() {
///       return Ok(None);
///     }
///
///     if let Some((frame, amt)) = decode(&src)? {
///       // clear the buffer up to the amount decoded so the same bytes aren't repeatedly processed
///       let _ = src.split_to(amt);
///
///       Ok(Some(frame))
///     }else{
///       Ok(None)
///     }
///   }
/// }
/// ```
///
pub mod complete {
  use super::*;

  /// Attempt to parse the contents of `buf`, returning the first valid frame and the number of bytes consumed.
  ///
  /// If the byte slice contains an incomplete frame then `None` is returned.
  pub fn decode(buf: &[u8]) -> Result<Option<(Frame, usize)>, RedisProtocolError> {
    let len = buf.len();

    match d_parse_frame_or_attribute(buf) {
      Ok((remaining, frame)) => Ok(Some((frame.into_complete_frame()?, len - remaining.len()))),
      Err(NomErr::Incomplete(_)) => Ok(None),
      Err(e) => Err(RedisParseError::from(e).into()),
    }
  }
}

/// Decoding structs and functions that support streaming frames. The caller is responsible for managing any returned state for streaming frames.
///
/// Examples:
///
/// Implement a [codec](https://docs.rs/tokio-util/0.6.6/tokio_util/codec/index.html) that supports decoding streams...
///
/// ```edition2018 no_run
/// # extern crate tokio_util;
/// # extern crate tokio;
/// # extern crate bytes;
///
/// use redis_protocol::resp3::types::*;
/// use redis_protocol::types::{RedisProtocolError, RedisProtocolErrorKind};
/// use redis_protocol::resp3::decode::streaming::*;
/// use redis_protocol::resp3::encode::complete::*;
/// use bytes::BytesMut;
/// use tokio_util::codec::{Decoder, Encoder};
/// use std::collections::VecDeque;
///
/// pub struct RedisCodec {
///   decoder_stream: Option<StreamedFrame>
/// }
///
/// impl Encoder<Frame> for RedisCodec {
///   type Error = RedisProtocolError;
///
///   fn encode(&mut self, item: Frame, dst: &mut BytesMut) -> Result<(), Self::Error> {
///     // in this example we only show support for encoding complete frames
///     let _ = encode_bytes(dst, &item)?;
///     Ok(())
///   }
/// }
///
/// impl Decoder for RedisCodec {
///   type Item = Frame;
///   type Error = RedisProtocolError;
///
///   // Buffer the results of streamed frame before returning the complete frame to the caller.
///   // Callers that want to surface streaming frame chunks up the stack would simply return after calling `decode` here.
///   fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
///     if src.is_empty() {
///       return Ok(None);
///     }
///
///     if let Some((frame, amt)) = decode(&src)? {
///       // clear the buffer up to the amount decoded so the same bytes aren't repeatedly processed
///       let _ = src.split_to(amt);
///
///       if self.decoder_stream.is_some() && frame.is_streaming() {
///         // it doesn't make sense to start a stream while inside another stream
///         return Err(RedisProtocolError::new(
///           RedisProtocolErrorKind::DecodeError,
///           "Cannot start a stream while already inside a stream."
///         ));
///       }
///
///       let result = if let Some(ref mut streamed_frame) = self.decoder_stream {
///         // we started receiving streamed data earlier
///
///         // we already checked for streams within streams above
///         let frame = frame.into_complete_frame()?;
///         streamed_frame.add_frame(frame);
///
///         if streamed_frame.is_finished() {
///            // convert the inner stream buffer into the final output frame
///            Some(streamed_frame.into_frame()?)
///         }else{
///           // buffer the stream in memory until it completes
///           None
///         }
///       }else{
///         // we're processing a complete frame or starting a new streamed frame
///         if frame.is_streaming() {
///           // start a new stream, saving the internal buffer to the codec state
///           self.decoder_stream = Some(frame.into_streaming_frame()?);
///           // don't return anything to the caller until the stream finishes (shown above)
///           None
///         }else{
///           // we're not in the middle of a stream and we found a complete frame
///           Some(frame.into_complete_frame()?)
///         }
///       };
///
///       if result.is_some() {
///         // we're either done with the stream or we found a complete frame. either way clear the buffer.
///         let _ = self.decoder_stream.take();
///       }
///
///       Ok(result)
///     }else{
///       Ok(None)
///     }
///   }
/// }
/// ```
///
pub mod streaming {
  use super::*;

  /// Attempt to parse the contents of `buf`, returning the first valid frame and the number of bytes consumed.
  ///
  /// If the byte slice contains an incomplete frame then `None` is returned.
  pub fn decode(buf: &[u8]) -> Result<Option<(DecodedFrame, usize)>, RedisProtocolError> {
    let len = buf.len();

    match d_parse_frame_or_attribute(buf) {
      Ok((remaining, frame)) => Ok(Some((frame, len - remaining.len()))),
      Err(NomErr::Incomplete(_)) => Ok(None),
      Err(e) => Err(RedisParseError::from(e).into()),
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::resp3::decode::complete::decode;
  use crate::resp3::decode::streaming::decode as stream_decode;
  use bytes::BytesMut;
  use std::str;

  const PADDING: &'static str = "FOOBARBAZ";

  fn pretty_print_panic(e: RedisProtocolError) {
    panic!("{:?}", e);
  }

  fn panic_no_decode() {
    panic!("Failed to decode bytes. None returned.")
  }

  fn decode_and_verify_some(bytes: &mut BytesMut, expected: &(Option<Frame>, usize)) {
    let (frame, len) = match complete::decode(&bytes) {
      Ok(Some((f, l))) => (Some(f), l),
      Ok(None) => return panic_no_decode(),
      Err(e) => return pretty_print_panic(e),
    };

    assert_eq!(frame, expected.0, "decoded frame matched");
    assert_eq!(len, expected.1, "decoded frame len matched");
  }

  fn decode_and_verify_padded_some(bytes: &mut BytesMut, expected: &(Option<Frame>, usize)) {
    bytes.extend_from_slice(PADDING.as_bytes());

    let (frame, len) = match complete::decode(&bytes) {
      Ok(Some((f, l))) => (Some(f), l),
      Ok(None) => return panic_no_decode(),
      Err(e) => return pretty_print_panic(e),
    };

    assert_eq!(frame, expected.0, "decoded frame matched");
    assert_eq!(len, expected.1, "decoded frame len matched");
  }

  fn decode_and_verify_none(bytes: &mut BytesMut) {
    let (frame, len) = match complete::decode(&bytes) {
      Ok(Some((f, l))) => (Some(f), l),
      Ok(None) => (None, 0),
      Err(e) => return pretty_print_panic(e),
    };

    assert!(frame.is_none());
    assert_eq!(len, 0);
  }

  // ----------------------- tests adapted from RESP2 ------------------------

  #[test]
  fn should_decode_llen_res_example() {
    let expected = (
      Some(Frame::Number {
        data: 48293,
        attributes: None,
      }),
      8,
    );
    let mut bytes: BytesMut = ":48293\r\n".into();

    decode_and_verify_some(&mut bytes, &expected);
    decode_and_verify_padded_some(&mut bytes, &expected);
  }

  #[test]
  fn should_decode_simple_string() {
    let expected = (
      Some(Frame::SimpleString {
        data: "string".into(),
        attributes: None,
      }),
      9,
    );
    let mut bytes: BytesMut = "+string\r\n".into();

    decode_and_verify_some(&mut bytes, &expected);
    decode_and_verify_padded_some(&mut bytes, &expected);
  }

  #[test]
  #[should_panic]
  fn should_decode_simple_string_incomplete() {
    let expected = (
      Some(Frame::SimpleString {
        data: "string".into(),
        attributes: None,
      }),
      9,
    );
    let mut bytes: BytesMut = "+stri".into();

    decode_and_verify_some(&mut bytes, &expected);
    decode_and_verify_padded_some(&mut bytes, &expected);
  }

  #[test]
  fn should_decode_blob_string() {
    let expected = (
      Some(Frame::BlobString {
        data: "foo".into(),
        attributes: None,
      }),
      9,
    );
    let mut bytes: BytesMut = "$3\r\nfoo\r\n".into();

    decode_and_verify_some(&mut bytes, &expected);
    decode_and_verify_padded_some(&mut bytes, &expected);
  }

  #[test]
  #[should_panic]
  fn should_decode_blob_string_incomplete() {
    let expected = (
      Some(Frame::BlobString {
        data: "foo".into(),
        attributes: None,
      }),
      9,
    );
    let mut bytes: BytesMut = "$3\r\nfo".into();

    decode_and_verify_some(&mut bytes, &expected);
    decode_and_verify_padded_some(&mut bytes, &expected);
  }

  #[test]
  fn should_decode_array_no_nulls() {
    let expected = (
      Some(Frame::Array {
        data: vec![
          Frame::SimpleString {
            data: "Foo".into(),
            attributes: None,
          },
          Frame::SimpleString {
            data: "Bar".into(),
            attributes: None,
          },
        ],
        attributes: None,
      }),
      16,
    );
    let mut bytes: BytesMut = "*2\r\n+Foo\r\n+Bar\r\n".into();

    decode_and_verify_some(&mut bytes, &expected);
    decode_and_verify_padded_some(&mut bytes, &expected);
  }

  #[test]
  fn should_decode_array_nulls() {
    let mut bytes: BytesMut = "*3\r\n$3\r\nFoo\r\n_\r\n$3\r\nBar\r\n".into();

    let expected = (
      Some(Frame::Array {
        data: vec![
          Frame::BlobString {
            data: "Foo".into(),
            attributes: None,
          },
          Frame::Null,
          Frame::BlobString {
            data: "Bar".into(),
            attributes: None,
          },
        ],
        attributes: None,
      }),
      bytes.len(),
    );

    decode_and_verify_some(&mut bytes, &expected);
    decode_and_verify_padded_some(&mut bytes, &expected);
  }

  #[test]
  fn should_decode_normal_error() {
    let mut bytes: BytesMut = "-WRONGTYPE Operation against a key holding the wrong kind of value\r\n".into();
    let expected = (
      Some(Frame::SimpleError {
        data: "WRONGTYPE Operation against a key holding the wrong kind of value".into(),
        attributes: None,
      }),
      bytes.len(),
    );

    decode_and_verify_some(&mut bytes, &expected);
    decode_and_verify_padded_some(&mut bytes, &expected);
  }

  #[test]
  fn should_decode_moved_error() {
    let mut bytes: BytesMut = "-MOVED 3999 127.0.0.1:6381\r\n".into();
    let expected = (
      Some(Frame::SimpleError {
        data: "MOVED 3999 127.0.0.1:6381".into(),
        attributes: None,
      }),
      bytes.len(),
    );

    decode_and_verify_some(&mut bytes, &expected);
    decode_and_verify_padded_some(&mut bytes, &expected);
  }

  #[test]
  fn should_decode_ask_error() {
    let mut bytes: BytesMut = "-ASK 3999 127.0.0.1:6381\r\n".into();
    let expected = (
      Some(Frame::SimpleError {
        data: "ASK 3999 127.0.0.1:6381".into(),
        attributes: None,
      }),
      bytes.len(),
    );

    decode_and_verify_some(&mut bytes, &expected);
    decode_and_verify_padded_some(&mut bytes, &expected);
  }

  #[test]
  fn should_decode_incomplete() {
    let mut bytes: BytesMut = "*3\r\n$3\r\nFoo\r\n_\r\n$3\r\nBar".into();
    decode_and_verify_none(&mut bytes);
  }

  #[test]
  #[should_panic]
  fn should_error_on_junk() {
    let bytes: BytesMut = "foobarbazwibblewobble".into();
    let _ = complete::decode(&bytes).map_err(|e| pretty_print_panic(e));
  }

  // ----------------- end tests adapted from RESP2 ------------------------

  #[test]
  fn should_decode_blob_error() {
    let expected = (
      Some(Frame::BlobError {
        data: "foo".into(),
        attributes: None,
      }),
      9,
    );
    let mut bytes: BytesMut = "!3\r\nfoo\r\n".into();

    decode_and_verify_some(&mut bytes, &expected);
    decode_and_verify_padded_some(&mut bytes, &expected);
  }

  #[test]
  #[should_panic]
  fn should_decode_blob_error_incomplete() {
    let expected = (
      Some(Frame::BlobError {
        data: "foo".into(),
        attributes: None,
      }),
      9,
    );
    let mut bytes: BytesMut = "!3\r\nfo".into();

    decode_and_verify_some(&mut bytes, &expected);
    decode_and_verify_padded_some(&mut bytes, &expected);
  }

  #[test]
  fn should_decode_simple_error() {
    let expected = (
      Some(Frame::SimpleError {
        data: "string".into(),
        attributes: None,
      }),
      9,
    );
    let mut bytes: BytesMut = "-string\r\n".into();

    decode_and_verify_some(&mut bytes, &expected);
    decode_and_verify_padded_some(&mut bytes, &expected);
  }

  #[test]
  #[should_panic]
  fn should_decode_simple_error_incomplete() {
    let expected = (
      Some(Frame::SimpleError {
        data: "string".into(),
        attributes: None,
      }),
      9,
    );
    let mut bytes: BytesMut = "-strin".into();

    decode_and_verify_some(&mut bytes, &expected);
    decode_and_verify_padded_some(&mut bytes, &expected);
  }

  #[test]
  fn should_decode_boolean_true() {
    let expected = (
      Some(Frame::Boolean {
        data: true,
        attributes: None,
      }),
      4,
    );
    let mut bytes: BytesMut = "#t\r\n".into();

    decode_and_verify_some(&mut bytes, &expected);
    decode_and_verify_padded_some(&mut bytes, &expected);
  }

  #[test]
  fn should_decode_boolean_false() {
    let expected = (
      Some(Frame::Boolean {
        data: false,
        attributes: None,
      }),
      4,
    );
    let mut bytes: BytesMut = "#f\r\n".into();

    decode_and_verify_some(&mut bytes, &expected);
    decode_and_verify_padded_some(&mut bytes, &expected);
  }

  #[test]
  fn should_decode_number() {
    let expected = (
      Some(Frame::Number {
        data: 42,
        attributes: None,
      }),
      5,
    );
    let mut bytes: BytesMut = ":42\r\n".into();

    decode_and_verify_some(&mut bytes, &expected);
    decode_and_verify_padded_some(&mut bytes, &expected);
  }

  #[test]
  fn should_decode_double_inf() {
    let expected = (
      Some(Frame::Double {
        data: f64::INFINITY,
        attributes: None,
      }),
      6,
    );
    let mut bytes: BytesMut = ",inf\r\n".into();

    decode_and_verify_some(&mut bytes, &expected);
    decode_and_verify_padded_some(&mut bytes, &expected);
  }

  #[test]
  fn should_decode_double_neg_inf() {
    let expected = (
      Some(Frame::Double {
        data: f64::NEG_INFINITY,
        attributes: None,
      }),
      7,
    );
    let mut bytes: BytesMut = ",-inf\r\n".into();

    decode_and_verify_some(&mut bytes, &expected);
    decode_and_verify_padded_some(&mut bytes, &expected);
  }

  #[test]
  #[should_panic]
  fn should_decode_double_nan() {
    let expected = (
      Some(Frame::Double {
        data: f64::NAN,
        attributes: None,
      }),
      7,
    );
    let mut bytes: BytesMut = ",foo\r\n".into();

    decode_and_verify_some(&mut bytes, &expected);
    decode_and_verify_padded_some(&mut bytes, &expected);
  }

  #[test]
  fn should_decode_double() {
    let expected = (
      Some(Frame::Double {
        data: 4.59193,
        attributes: None,
      }),
      10,
    );
    let mut bytes: BytesMut = ",4.59193\r\n".into();

    decode_and_verify_some(&mut bytes, &expected);
    decode_and_verify_padded_some(&mut bytes, &expected);

    let expected = (
      Some(Frame::Double {
        data: 4_f64,
        attributes: None,
      }),
      4,
    );
    let mut bytes: BytesMut = ",4\r\n".into();

    decode_and_verify_some(&mut bytes, &expected);
    decode_and_verify_padded_some(&mut bytes, &expected);
  }

  #[test]
  fn should_decode_bignumber() {
    let expected = (
      Some(Frame::BigNumber {
        data: "3492890328409238509324850943850943825024385".as_bytes().to_vec(),
        attributes: None,
      }),
      46,
    );
    let mut bytes: BytesMut = "(3492890328409238509324850943850943825024385\r\n".into();

    decode_and_verify_some(&mut bytes, &expected);
    decode_and_verify_padded_some(&mut bytes, &expected);
  }

  #[test]
  fn should_decode_null() {
    let expected = (Some(Frame::Null), 3);
    let mut bytes: BytesMut = "_\r\n".into();

    decode_and_verify_some(&mut bytes, &expected);
    decode_and_verify_padded_some(&mut bytes, &expected);
  }

  #[test]
  fn should_decode_verbatim_string_mkd() {
    let expected = (
      Some(Frame::VerbatimString {
        data: "Some string".as_bytes().to_vec(),
        format: VerbatimStringFormat::Markdown,
        attributes: None,
      }),
      22,
    );
    let mut bytes: BytesMut = "=15\r\nmkd:Some string\r\n".into();

    decode_and_verify_some(&mut bytes, &expected);
    decode_and_verify_padded_some(&mut bytes, &expected);
  }

  #[test]
  fn should_decode_verbatim_string_txt() {
    let expected = (
      Some(Frame::VerbatimString {
        data: "Some string".as_bytes().to_vec(),
        format: VerbatimStringFormat::Text,
        attributes: None,
      }),
      22,
    );
    let mut bytes: BytesMut = "=15\r\ntxt:Some string\r\n".into();

    decode_and_verify_some(&mut bytes, &expected);
    decode_and_verify_padded_some(&mut bytes, &expected);
  }

  #[test]
  fn should_decode_map_no_nulls() {
    let k1 = Frame::SimpleString {
      data: "first".into(),
      attributes: None,
    };
    let v1 = Frame::Number {
      data: 1,
      attributes: None,
    };
    let k2 = Frame::BlobString {
      data: "second".into(),
      attributes: None,
    };
    let v2 = Frame::Double {
      data: 4.2,
      attributes: None,
    };

    let mut expected_map = resp3_utils::new_map(None);
    expected_map.insert(k1, v1);
    expected_map.insert(k2, v2);
    let expected = (
      Some(Frame::Map {
        data: expected_map,
        attributes: None,
      }),
      34,
    );
    let mut bytes: BytesMut = "%2\r\n+first\r\n:1\r\n$6\r\nsecond\r\n,4.2\r\n".into();

    decode_and_verify_some(&mut bytes, &expected);
    decode_and_verify_padded_some(&mut bytes, &expected);
  }

  #[test]
  fn should_decode_map_with_nulls() {
    let k1 = Frame::SimpleString {
      data: "first".into(),
      attributes: None,
    };
    let v1 = Frame::Number {
      data: 1,
      attributes: None,
    };
    let k2 = Frame::Number {
      data: 2,
      attributes: None,
    };
    let v2 = Frame::Null;
    let k3 = Frame::BlobString {
      data: "second".into(),
      attributes: None,
    };
    let v3 = Frame::Double {
      data: 4.2,
      attributes: None,
    };

    let mut expected_map = resp3_utils::new_map(None);
    expected_map.insert(k1, v1);
    expected_map.insert(k2, v2);
    expected_map.insert(k3, v3);
    let expected = (
      Some(Frame::Map {
        data: expected_map,
        attributes: None,
      }),
      41,
    );
    let mut bytes: BytesMut = "%3\r\n+first\r\n:1\r\n:2\r\n_\r\n$6\r\nsecond\r\n,4.2\r\n".into();

    decode_and_verify_some(&mut bytes, &expected);
    decode_and_verify_padded_some(&mut bytes, &expected);
  }

  #[test]
  fn should_decode_set_no_nulls() {
    let mut expected_set = resp3_utils::new_set(None);
    expected_set.insert(Frame::Number {
      data: 1,
      attributes: None,
    });
    expected_set.insert(Frame::SimpleString {
      data: "2".into(),
      attributes: None,
    });
    expected_set.insert(Frame::BlobString {
      data: "foobar".into(),
      attributes: None,
    });
    expected_set.insert(Frame::Double {
      data: 4.2,
      attributes: None,
    });
    let expected = (
      Some(Frame::Set {
        data: expected_set,
        attributes: None,
      }),
      30,
    );
    let mut bytes: BytesMut = "~4\r\n:1\r\n+2\r\n$6\r\nfoobar\r\n,4.2\r\n".into();

    decode_and_verify_some(&mut bytes, &expected);
    decode_and_verify_padded_some(&mut bytes, &expected);
  }

  #[test]
  fn should_decode_set_with_nulls() {
    let mut expected_set = resp3_utils::new_set(None);
    expected_set.insert(Frame::Number {
      data: 1,
      attributes: None,
    });
    expected_set.insert(Frame::SimpleString {
      data: "2".into(),
      attributes: None,
    });
    expected_set.insert(Frame::Null);
    expected_set.insert(Frame::Double {
      data: 4.2,
      attributes: None,
    });
    let expected = (
      Some(Frame::Set {
        data: expected_set,
        attributes: None,
      }),
      21,
    );
    let mut bytes: BytesMut = "~4\r\n:1\r\n+2\r\n_\r\n,4.2\r\n".into();

    decode_and_verify_some(&mut bytes, &expected);
    decode_and_verify_padded_some(&mut bytes, &expected);
  }

  #[test]
  fn should_decode_push_pubsub() {
    let expected = (
      Some(Frame::Push {
        data: vec![
          Frame::SimpleString {
            data: "pubsub".into(),
            attributes: None,
          },
          Frame::SimpleString {
            data: "message".into(),
            attributes: None,
          },
          Frame::SimpleString {
            data: "somechannel".into(),
            attributes: None,
          },
          Frame::SimpleString {
            data: "this is the message".into(),
            attributes: None,
          },
        ],
        attributes: None,
      }),
      59,
    );
    let mut bytes: BytesMut = ">4\r\n+pubsub\r\n+message\r\n+somechannel\r\n+this is the message\r\n".into();

    decode_and_verify_some(&mut bytes, &expected);
    decode_and_verify_padded_some(&mut bytes, &expected);

    let (frame, _) = decode(&bytes).unwrap().unwrap();
    assert!(frame.is_pubsub_message());
    assert!(frame.is_normal_pubsub());
  }

  #[test]
  fn should_decode_push_pattern_pubsub() {
    let expected = (
      Some(Frame::Push {
        data: vec![
          Frame::SimpleString {
            data: "pubsub".into(),
            attributes: None,
          },
          Frame::SimpleString {
            data: "pmessage".into(),
            attributes: None,
          },
          Frame::SimpleString {
            data: "somechannel".into(),
            attributes: None,
          },
          Frame::SimpleString {
            data: "this is the message".into(),
            attributes: None,
          },
        ],
        attributes: None,
      }),
      60,
    );
    let mut bytes: BytesMut = ">4\r\n+pubsub\r\n+pmessage\r\n+somechannel\r\n+this is the message\r\n".into();

    decode_and_verify_some(&mut bytes, &expected);
    decode_and_verify_padded_some(&mut bytes, &expected);

    let (frame, _) = decode(&bytes).unwrap().unwrap();
    assert!(frame.is_pattern_pubsub_message());
    assert!(frame.is_pubsub_message());
  }

  #[test]
  fn should_decode_keyevent_message() {
    let expected = (
      Some(Frame::Push {
        data: vec![
          Frame::SimpleString {
            data: "pubsub".into(),
            attributes: None,
          },
          Frame::SimpleString {
            data: "pmessage".into(),
            attributes: None,
          },
          Frame::SimpleString {
            data: "__key*".into(),
            attributes: None,
          },
          Frame::SimpleString {
            data: "__keyevent@0__:set".into(),
            attributes: None,
          },
          Frame::SimpleString {
            data: "foo".into(),
            attributes: None,
          },
        ],
        attributes: None,
      }),
      60,
    );
    let mut bytes: BytesMut = ">5\r\n+pubsub\r\n+pmessage\r\n+__key*\r\n+__keyevent@0__:set\r\n+foo\r\n".into();

    decode_and_verify_some(&mut bytes, &expected);
    decode_and_verify_padded_some(&mut bytes, &expected);

    let (frame, _) = decode(&bytes).unwrap().unwrap();
    assert!(frame.is_pattern_pubsub_message());
    assert!(frame.is_pubsub_message());
  }

  #[test]
  fn should_parse_outer_attributes() {
    let mut expected_inner_attrs = resp3_utils::new_map(None);
    expected_inner_attrs.insert(
      Frame::BlobString {
        data: "a".into(),
        attributes: None,
      },
      Frame::Double {
        data: 0.1923,
        attributes: None,
      },
    );
    expected_inner_attrs.insert(
      Frame::BlobString {
        data: "b".into(),
        attributes: None,
      },
      Frame::Double {
        data: 0.0012,
        attributes: None,
      },
    );
    let expected_inner_attrs = Frame::Map {
      data: expected_inner_attrs,
      attributes: None,
    };

    let mut expected_attrs = resp3_utils::new_map(None);
    expected_attrs.insert(
      Frame::SimpleString {
        data: "key-popularity".into(),
        attributes: None,
      },
      expected_inner_attrs,
    );

    let expected = (
      Some(Frame::Array {
        data: vec![
          Frame::Number {
            data: 2039123,
            attributes: None,
          },
          Frame::Number {
            data: 9543892,
            attributes: None,
          },
        ],
        attributes: Some(expected_attrs.into()),
      }),
      81,
    );

    let mut bytes: BytesMut =
      "|1\r\n+key-popularity\r\n%2\r\n$1\r\na\r\n,0.1923\r\n$1\r\nb\r\n,0.0012\r\n*2\r\n:2039123\r\n:9543892\r\n"
        .into();

    decode_and_verify_some(&mut bytes, &expected);
    decode_and_verify_padded_some(&mut bytes, &expected);
  }

  #[test]
  fn should_parse_inner_attributes() {
    let mut expected_attrs = resp3_utils::new_map(None);
    expected_attrs.insert(
      Frame::SimpleString {
        data: "ttl".into(),
        attributes: None,
      },
      Frame::Number {
        data: 3600,
        attributes: None,
      },
    );

    let expected = (
      Some(Frame::Array {
        data: vec![
          Frame::Number {
            data: 1,
            attributes: None,
          },
          Frame::Number {
            data: 2,
            attributes: None,
          },
          Frame::Number {
            data: 3,
            attributes: Some(expected_attrs),
          },
        ],
        attributes: None,
      }),
      33,
    );
    let mut bytes: BytesMut = "*3\r\n:1\r\n:2\r\n|1\r\n+ttl\r\n:3600\r\n:3\r\n".into();

    decode_and_verify_some(&mut bytes, &expected);
    decode_and_verify_padded_some(&mut bytes, &expected);
  }

  #[test]
  fn should_decode_end_stream() {
    let bytes: BytesMut = ";0\r\n".into();
    let (frame, _) = stream_decode(&bytes).unwrap().unwrap();
    assert_eq!(frame, DecodedFrame::Complete(Frame::new_end_stream()))
  }

  #[test]
  fn should_decode_streaming_string() {
    let mut bytes: BytesMut = "$?\r\n;4\r\nHell\r\n;6\r\no worl\r\n;1\r\nd\r\n;0\r\n".into();

    let (frame, amt) = stream_decode(&bytes).unwrap().unwrap();
    assert_eq!(
      frame,
      DecodedFrame::Streaming(StreamedFrame::new(FrameKind::BlobString))
    );
    assert_eq!(amt, 4);
    let _ = bytes.split_to(amt);

    let (frame, amt) = stream_decode(&bytes).unwrap().unwrap();
    assert_eq!(frame, DecodedFrame::Complete(Frame::ChunkedString("Hell".into())));
    assert_eq!(amt, 10);
    let _ = bytes.split_to(amt);

    let (frame, amt) = stream_decode(&bytes).unwrap().unwrap();
    assert_eq!(frame, DecodedFrame::Complete(Frame::ChunkedString("o worl".into())));
    assert_eq!(amt, 12);
    let _ = bytes.split_to(amt);

    let (frame, amt) = stream_decode(&bytes).unwrap().unwrap();
    assert_eq!(frame, DecodedFrame::Complete(Frame::ChunkedString("d".into())));
    assert_eq!(amt, 7);
    let _ = bytes.split_to(amt);

    let (frame, amt) = stream_decode(&bytes).unwrap().unwrap();
    assert_eq!(frame, DecodedFrame::Complete(Frame::new_end_stream()));
    assert_eq!(amt, 4);
  }

  #[test]
  fn should_decode_streaming_array() {
    let mut bytes: BytesMut = "*?\r\n:1\r\n:2\r\n:3\r\n.\r\n".into();

    let (frame, amt) = stream_decode(&bytes).unwrap().unwrap();
    assert_eq!(frame, DecodedFrame::Streaming(StreamedFrame::new(FrameKind::Array)));
    assert_eq!(amt, 4);
    let _ = bytes.split_to(amt);
    let mut streamed = frame.into_streaming_frame().unwrap();

    let (frame, amt) = stream_decode(&bytes).unwrap().unwrap();
    assert_eq!(
      frame,
      DecodedFrame::Complete(Frame::Number {
        data: 1,
        attributes: None
      })
    );
    assert_eq!(amt, 4);
    let _ = bytes.split_to(amt);
    streamed.add_frame(frame.into_complete_frame().unwrap());

    let (frame, amt) = stream_decode(&bytes).unwrap().unwrap();
    assert_eq!(
      frame,
      DecodedFrame::Complete(Frame::Number {
        data: 2,
        attributes: None
      })
    );
    assert_eq!(amt, 4);
    let _ = bytes.split_to(amt);
    streamed.add_frame(frame.into_complete_frame().unwrap());

    let (frame, amt) = stream_decode(&bytes).unwrap().unwrap();
    assert_eq!(
      frame,
      DecodedFrame::Complete(Frame::Number {
        data: 3,
        attributes: None
      })
    );
    assert_eq!(amt, 4);
    let _ = bytes.split_to(amt);
    streamed.add_frame(frame.into_complete_frame().unwrap());

    let (frame, amt) = stream_decode(&bytes).unwrap().unwrap();
    assert_eq!(frame, DecodedFrame::Complete(Frame::new_end_stream()));
    assert_eq!(amt, 3);
    streamed.add_frame(frame.into_complete_frame().unwrap());

    assert!(streamed.is_finished());
    let actual = streamed.into_frame().unwrap();
    let expected = Frame::Array {
      data: vec![
        Frame::Number {
          data: 1,
          attributes: None,
        },
        Frame::Number {
          data: 2,
          attributes: None,
        },
        Frame::Number {
          data: 3,
          attributes: None,
        },
      ],
      attributes: None,
    };

    assert_eq!(actual, expected);
  }

  #[test]
  fn should_decode_streaming_set() {
    let mut bytes: BytesMut = "~?\r\n:1\r\n:2\r\n:3\r\n.\r\n".into();

    let (frame, amt) = stream_decode(&bytes).unwrap().unwrap();
    assert_eq!(frame, DecodedFrame::Streaming(StreamedFrame::new(FrameKind::Set)));
    assert_eq!(amt, 4);
    let _ = bytes.split_to(amt);
    let mut streamed = frame.into_streaming_frame().unwrap();

    let (frame, amt) = stream_decode(&bytes).unwrap().unwrap();
    assert_eq!(
      frame,
      DecodedFrame::Complete(Frame::Number {
        data: 1,
        attributes: None
      })
    );
    assert_eq!(amt, 4);
    let _ = bytes.split_to(amt);
    streamed.add_frame(frame.into_complete_frame().unwrap());

    let (frame, amt) = stream_decode(&bytes).unwrap().unwrap();
    assert_eq!(
      frame,
      DecodedFrame::Complete(Frame::Number {
        data: 2,
        attributes: None
      })
    );
    assert_eq!(amt, 4);
    let _ = bytes.split_to(amt);
    streamed.add_frame(frame.into_complete_frame().unwrap());

    let (frame, amt) = stream_decode(&bytes).unwrap().unwrap();
    assert_eq!(
      frame,
      DecodedFrame::Complete(Frame::Number {
        data: 3,
        attributes: None
      })
    );
    assert_eq!(amt, 4);
    let _ = bytes.split_to(amt);
    streamed.add_frame(frame.into_complete_frame().unwrap());

    let (frame, amt) = stream_decode(&bytes).unwrap().unwrap();
    assert_eq!(frame, DecodedFrame::Complete(Frame::new_end_stream()));
    assert_eq!(amt, 3);
    streamed.add_frame(frame.into_complete_frame().unwrap());

    assert!(streamed.is_finished());
    let actual = streamed.into_frame().unwrap();
    let mut expected_result = resp3_utils::new_set(None);
    expected_result.insert(Frame::Number {
      data: 1,
      attributes: None,
    });
    expected_result.insert(Frame::Number {
      data: 2,
      attributes: None,
    });
    expected_result.insert(Frame::Number {
      data: 3,
      attributes: None,
    });

    let expected = Frame::Set {
      data: expected_result,
      attributes: None,
    };

    assert_eq!(actual, expected);
  }

  #[test]
  fn should_decode_streaming_map() {
    let mut bytes: BytesMut = "%?\r\n+a\r\n:1\r\n+b\r\n:2\r\n.\r\n".into();

    let (frame, amt) = stream_decode(&bytes).unwrap().unwrap();
    assert_eq!(frame, DecodedFrame::Streaming(StreamedFrame::new(FrameKind::Map)));
    assert_eq!(amt, 4);
    let _ = bytes.split_to(amt);
    let mut streamed = frame.into_streaming_frame().unwrap();

    let (frame, amt) = stream_decode(&bytes).unwrap().unwrap();
    assert_eq!(
      frame,
      DecodedFrame::Complete(Frame::SimpleString {
        data: "a".into(),
        attributes: None
      })
    );
    assert_eq!(amt, 4);
    let _ = bytes.split_to(amt);
    streamed.add_frame(frame.into_complete_frame().unwrap());

    let (frame, amt) = stream_decode(&bytes).unwrap().unwrap();
    assert_eq!(
      frame,
      DecodedFrame::Complete(Frame::Number {
        data: 1.into(),
        attributes: None
      })
    );
    assert_eq!(amt, 4);
    let _ = bytes.split_to(amt);
    streamed.add_frame(frame.into_complete_frame().unwrap());

    let (frame, amt) = stream_decode(&bytes).unwrap().unwrap();
    assert_eq!(
      frame,
      DecodedFrame::Complete(Frame::SimpleString {
        data: "b".into(),
        attributes: None
      })
    );
    assert_eq!(amt, 4);
    let _ = bytes.split_to(amt);
    streamed.add_frame(frame.into_complete_frame().unwrap());

    let (frame, amt) = stream_decode(&bytes).unwrap().unwrap();
    assert_eq!(
      frame,
      DecodedFrame::Complete(Frame::Number {
        data: 2.into(),
        attributes: None
      })
    );
    assert_eq!(amt, 4);
    let _ = bytes.split_to(amt);
    streamed.add_frame(frame.into_complete_frame().unwrap());

    let (frame, amt) = stream_decode(&bytes).unwrap().unwrap();
    assert_eq!(frame, DecodedFrame::Complete(Frame::new_end_stream()));
    assert_eq!(amt, 3);
    streamed.add_frame(frame.into_complete_frame().unwrap());

    assert!(streamed.is_finished());
    let actual = streamed.into_frame().unwrap();
    let mut expected_result = resp3_utils::new_map(None);
    expected_result.insert(
      Frame::SimpleString {
        data: "a".into(),
        attributes: None,
      },
      Frame::Number {
        data: 1,
        attributes: None,
      },
    );
    expected_result.insert(
      Frame::SimpleString {
        data: "b".into(),
        attributes: None,
      },
      Frame::Number {
        data: 2,
        attributes: None,
      },
    );
    let expected = Frame::Map {
      data: expected_result,
      attributes: None,
    };

    assert_eq!(actual, expected);
  }
}
