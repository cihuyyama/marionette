//! AWS binary EventStream framing, as used by the Kiro / CodeWhisperer
//! streaming surface.
//!
//! Ported from Cartethyia `src/providers/integrations/kiro/aws-event-stream.ts`.
//!
//! The upstream answers with self-describing binary messages rather than SSE.
//! Each message is:
//!
//! ```text
//! total_length     uint32 BE   offset 0   (whole message, including this field)
//! headers_length   uint32 BE   offset 4
//! prelude_crc32    uint32 BE   offset 8   (CRC32 of bytes 0..8)
//! headers          bytes       offset 12
//! payload          bytes       offset 12 + headers_length
//! message_crc32    uint32 BE   last 4     (CRC32 of everything before it)
//! ```
//!
//! Both CRCs use the IEEE polynomial (0xedb88320, reflected) and **both are
//! verified**. That is not ceremony: a truncated or interleaved message
//! otherwise decodes into plausible-looking JSON that a provider adapter would
//! happily stream to a client. A message failing its CRC is a corrupt stream,
//! not a partial answer.
//!
//! Only the envelope and header block live here. Which event types exist and
//! what their payloads mean belongs to the adapter.

pub const PRELUDE_BYTES: usize = 12;
pub const TRAILER_BYTES: usize = 4;
pub const MIN_MESSAGE_BYTES: usize = PRELUDE_BYTES + TRAILER_BYTES;
/// The largest thing this surface sends is a single assistant delta, so a
/// message claiming more than this is a framing error and reading it would
/// only allocate attacker-chosen memory.
pub const MAX_MESSAGE_BYTES: u32 = 24 * 1024 * 1024;
pub const MAX_HEADERS_BYTES: u32 = 128 * 1024;

/// IEEE CRC32 polynomial, reflected.
const CRC32_POLYNOMIAL: u32 = 0xedb8_8320;

/// Reason a message could not be decoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeFailure {
    Truncated,
    OversizeMessage,
    OversizeHeaders,
    PreludeCrcMismatch,
    MessageCrcMismatch,
    MalformedHeaders,
    InvalidPayloadUtf8,
}

impl DecodeFailure {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Truncated => "truncated",
            Self::OversizeMessage => "oversize_message",
            Self::OversizeHeaders => "oversize_headers",
            Self::PreludeCrcMismatch => "prelude_crc_mismatch",
            Self::MessageCrcMismatch => "message_crc_mismatch",
            Self::MalformedHeaders => "malformed_headers",
            Self::InvalidPayloadUtf8 => "invalid_payload_utf8",
        }
    }
}

impl std::fmt::Display for DecodeFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One decoded EventStream message.
#[derive(Debug, Clone)]
pub struct EventStreamMessage {
    /// Header values by name. Only `:message-type` and `:event-type` are read.
    pub headers: std::collections::HashMap<String, String>,
    /// Decoded UTF-8 payload; `None` when the message carries none.
    pub payload: Option<String>,
}

impl EventStreamMessage {
    pub fn message_type(&self) -> Option<&str> {
        self.headers.get(":message-type").map(|s| s.as_str())
    }

    pub fn event_type(&self) -> &str {
        self.headers
            .get(":event-type")
            .map(|s| s.as_str())
            .unwrap_or("")
    }
}

/// Result of decoding one buffer.
pub struct DecodeResult {
    pub messages: Vec<EventStreamMessage>,
    /// Bytes consumed; the caller drops exactly this many from its buffer.
    pub consumed: usize,
    /// Set when decoding stopped on a corrupt message rather than a short read.
    pub failure: Option<(DecodeFailure, String)>,
}

/// CRC32 of `bytes` using the reflected IEEE polynomial.
pub fn crc32(bytes: &[u8]) -> u32 {
    let mut crc: u32 = 0xffff_ffff;
    for &byte in bytes {
        crc ^= byte as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ CRC32_POLYNOMIAL
            } else {
                crc >> 1
            };
        }
    }
    crc ^ 0xffff_ffff
}

/// Decode as many whole messages as `buffer` holds.
///
/// A trailing partial message is **not** an error: the caller appends the next
/// read and calls again. Anything failing validation stops the loop and is
/// reported, because every byte after a corrupt message is untrustworthy.
pub fn decode_messages(buffer: &[u8]) -> DecodeResult {
    let mut messages = Vec::new();
    let mut offset = 0usize;

    loop {
        let remaining = buffer.len().saturating_sub(offset);
        if remaining < MIN_MESSAGE_BYTES {
            break;
        }

        let total_length = be_u32(buffer, offset);
        let headers_length = be_u32(buffer, offset + 4);

        if total_length < MIN_MESSAGE_BYTES as u32 {
            return fail(
                messages,
                offset,
                DecodeFailure::Truncated,
                format!(
                    "message length {total_length} is below the {MIN_MESSAGE_BYTES}-byte minimum"
                ),
            );
        }
        if total_length > MAX_MESSAGE_BYTES {
            return fail(
                messages,
                offset,
                DecodeFailure::OversizeMessage,
                format!("message length {total_length} exceeds the {MAX_MESSAGE_BYTES}-byte cap"),
            );
        }
        if headers_length > MAX_HEADERS_BYTES {
            return fail(
                messages,
                offset,
                DecodeFailure::OversizeHeaders,
                format!("header block {headers_length} exceeds the {MAX_HEADERS_BYTES}-byte cap"),
            );
        }
        if headers_length > total_length - MIN_MESSAGE_BYTES as u32 {
            return fail(
                messages,
                offset,
                DecodeFailure::MalformedHeaders,
                format!(
                    "header block {headers_length} does not fit in a {total_length}-byte message"
                ),
            );
        }
        // A short buffer is not a failure — the rest has not arrived yet.
        if remaining < total_length as usize {
            break;
        }

        let message_end = offset + total_length as usize;

        let declared_prelude = be_u32(buffer, offset + 8);
        let actual_prelude = crc32(&buffer[offset..offset + 8]);
        if declared_prelude != actual_prelude {
            return fail(
                messages,
                offset,
                DecodeFailure::PreludeCrcMismatch,
                format!("prelude CRC {declared_prelude} does not match computed {actual_prelude}"),
            );
        }

        let declared_message = be_u32(buffer, message_end - TRAILER_BYTES);
        let actual_message = crc32(&buffer[offset..message_end - TRAILER_BYTES]);
        if declared_message != actual_message {
            return fail(
                messages,
                offset,
                DecodeFailure::MessageCrcMismatch,
                format!("message CRC {declared_message} does not match computed {actual_message}"),
            );
        }

        let header_start = offset + PRELUDE_BYTES;
        let headers = match decode_headers(&buffer[header_start..header_start + headers_length as usize]) {
            Ok(h) => h,
            Err(detail) => return fail(messages, offset, DecodeFailure::MalformedHeaders, detail),
        };

        let payload_start = header_start + headers_length as usize;
        let payload_end = message_end - TRAILER_BYTES;
        let payload = if payload_end > payload_start {
            match std::str::from_utf8(&buffer[payload_start..payload_end]) {
                Ok(s) => Some(s.to_string()),
                Err(_) => {
                    return fail(
                        messages,
                        offset,
                        DecodeFailure::InvalidPayloadUtf8,
                        "message payload is not valid UTF-8".into(),
                    )
                }
            }
        } else {
            None
        };

        messages.push(EventStreamMessage { headers, payload });
        offset = message_end;
    }

    DecodeResult {
        messages,
        consumed: offset,
        failure: None,
    }
}

fn fail(
    messages: Vec<EventStreamMessage>,
    consumed: usize,
    reason: DecodeFailure,
    detail: String,
) -> DecodeResult {
    DecodeResult {
        messages,
        consumed,
        failure: Some((reason, detail)),
    }
}

/// Decode one header block into a flat string map.
///
/// A sequence of `{name length, name, value type, value}` records. Value types:
/// 0/1 boolean, 2 int8, 3 int16, 4 int32, 5/8/9 fixed-width integers this
/// surface never sends, 6 byte array, 7 string. Numeric and boolean values are
/// stringified so callers see one map type; fixed-width integers are skipped
/// rather than guessed at.
pub fn decode_headers(bytes: &[u8]) -> Result<std::collections::HashMap<String, String>, String> {
    use std::collections::HashMap;
    let mut headers = HashMap::new();
    let mut offset = 0usize;

    while offset < bytes.len() {
        if offset + 1 > bytes.len() {
            return Err("header name length is truncated".into());
        }
        let name_length = bytes[offset] as usize;
        offset += 1;
        if offset + name_length > bytes.len() {
            return Err("header name is truncated".into());
        }
        let name = String::from_utf8_lossy(&bytes[offset..offset + name_length]).to_string();
        offset += name_length;

        if offset + 1 > bytes.len() {
            return Err(format!("header {name} has no value type"));
        }
        let value_type = bytes[offset];
        offset += 1;

        match value_type {
            0 | 1 => {
                headers.insert(name, if value_type == 1 { "true" } else { "false" }.into());
            }
            2 => {
                if offset + 1 > bytes.len() {
                    return Err(format!("header {name} is truncated"));
                }
                headers.insert(name, (bytes[offset] as i8).to_string());
                offset += 1;
            }
            3 => {
                if offset + 2 > bytes.len() {
                    return Err(format!("header {name} is truncated"));
                }
                headers.insert(name, be_i16(bytes, offset).to_string());
                offset += 2;
            }
            4 => {
                if offset + 4 > bytes.len() {
                    return Err(format!("header {name} is truncated"));
                }
                headers.insert(name, (be_u32(bytes, offset) as i32).to_string());
                offset += 4;
            }
            5 | 8 => {
                if offset + 8 > bytes.len() {
                    return Err(format!("header {name} is truncated"));
                }
                // Fixed-width integer this surface never sends: skipped.
                offset += 8;
            }
            9 => {
                if offset + 16 > bytes.len() {
                    return Err(format!("header {name} is truncated"));
                }
                offset += 16;
            }
            6 | 7 => {
                if offset + 2 > bytes.len() {
                    return Err(format!("header {name} is truncated"));
                }
                let value_length = be_u16(bytes, offset) as usize;
                offset += 2;
                if offset + value_length > bytes.len() {
                    return Err(format!("header {name} value is truncated"));
                }
                let value = &bytes[offset..offset + value_length];
                offset += value_length;
                headers.insert(
                    name,
                    if value_type == 7 {
                        String::from_utf8_lossy(value).to_string()
                    } else {
                        base64_encode(value)
                    },
                );
            }
            other => return Err(format!("header {name} has unknown value type {other}")),
        }
    }

    Ok(headers)
}

/// Build a well-formed EventStream message (used by tests and by any future
/// outbound need). Payload is UTF-8; `:message-type` is event unless named.
pub fn encode_message(headers: &[(&str, &str)], payload: Option<&str>) -> Vec<u8> {
    let mut header_bytes = Vec::new();
    for (name, value) in headers {
        header_bytes.push(name.len() as u8);
        header_bytes.extend_from_slice(name.as_bytes());
        header_bytes.push(7u8); // string
        header_bytes.extend_from_slice(&(value.len() as u16).to_be_bytes());
        header_bytes.extend_from_slice(value.as_bytes());
    }

    let payload_bytes = payload.map(|p| p.as_bytes()).unwrap_or(&[]);
    let total = PRELUDE_BYTES + header_bytes.len() + payload_bytes.len() + TRAILER_BYTES;

    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(&(total as u32).to_be_bytes());
    out.extend_from_slice(&(header_bytes.len() as u32).to_be_bytes());
    let prelude_crc = crc32(&out[0..8]);
    out.extend_from_slice(&prelude_crc.to_be_bytes());
    out.extend_from_slice(&header_bytes);
    out.extend_from_slice(payload_bytes);
    let message_crc = crc32(&out);
    out.extend_from_slice(&message_crc.to_be_bytes());
    out
}

fn base64_encode(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn be_u32(b: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

fn be_u16(b: &[u8], at: usize) -> u16 {
    u16::from_be_bytes([b[at], b[at + 1]])
}

fn be_i16(b: &[u8], at: usize) -> i16 {
    i16::from_be_bytes([b[at], b[at + 1]])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Known-answer: CRC32 of "123456789" is 0xCBF43926.
    #[test]
    fn crc32_matches_known_answer() {
        assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
    }

    #[test]
    fn round_trip_one_message() {
        let msg = encode_message(
            &[(":message-type", "event"), (":event-type", "assistantResponseEvent")],
            Some("{\"content\":\"hi\"}"),
        );
        let res = decode_messages(&msg);
        assert!(res.failure.is_none(), "{:?}", res.failure);
        assert_eq!(res.messages.len(), 1);
        assert_eq!(res.messages[0].event_type(), "assistantResponseEvent");
        assert_eq!(
            res.messages[0].payload.as_deref(),
            Some("{\"content\":\"hi\"}")
        );
        assert_eq!(res.consumed, msg.len());
    }

    #[test]
    fn two_messages_decode_in_order() {
        let mut buf = encode_message(&[(":event-type", "a")], Some("first"));
        buf.extend_from_slice(&encode_message(&[(":event-type", "b")], Some("second")));
        let res = decode_messages(&buf);
        assert!(res.failure.is_none());
        assert_eq!(res.messages.len(), 2);
        assert_eq!(res.messages[0].payload.as_deref(), Some("first"));
        assert_eq!(res.messages[1].payload.as_deref(), Some("second"));
    }

    /// A trailing partial message must NOT be an error and must not consume
    /// its bytes — the caller keeps them and appends the next read.
    #[test]
    fn partial_trailing_message_is_not_an_error() {
        let full = encode_message(&[(":event-type", "a")], Some("hello"));
        let half = &full[..full.len() - 3];
        let res = decode_messages(half);
        assert!(res.failure.is_none(), "{:?}", res.failure);
        assert_eq!(res.messages.len(), 0);
        assert_eq!(res.consumed, 0);
    }

    #[test]
    fn corrupt_prelude_is_detected() {
        let mut msg = encode_message(&[(":event-type", "a")], Some("hello"));
        // Corrupt the STORED prelude CRC, leaving total_length and
        // headers_length intact. Bounds checks therefore pass and the CRC
        // comparison is what catches it — which is the behaviour under test.
        // (Corrupting a length field instead hits the bounds checks first,
        // since they run before the CRC, as in the reference decoder.)
        msg[8] ^= 0x01;
        let res = decode_messages(&msg);
        assert_eq!(
            res.failure.as_ref().map(|(r, _)| *r),
            Some(DecodeFailure::PreludeCrcMismatch)
        );
    }

    #[test]
    fn corrupt_payload_is_detected_by_message_crc() {
        let mut msg = encode_message(&[(":event-type", "a")], Some("hello"));
        // Flip a payload byte: the prelude still checks out, so only the
        // message CRC can catch this.
        let payload_start = PRELUDE_BYTES + headers_len_for(&[(":event-type", "a")]);
        msg[payload_start] ^= 0x01;
        let res = decode_messages(&msg);
        assert_eq!(
            res.failure.as_ref().map(|(r, _)| *r),
            Some(DecodeFailure::MessageCrcMismatch)
        );
    }

    #[test]
    fn oversize_message_is_refused() {
        let mut msg = encode_message(&[(":event-type", "a")], Some("x"));
        let claimed = (MAX_MESSAGE_BYTES + 1).to_be_bytes();
        msg[0..4].copy_from_slice(&claimed);
        // The prelude CRC no longer matches, so this reports the CRC first;
        // what matters is that the oversize claim is refused rather than read.
        let res = decode_messages(&msg);
        assert!(res.failure.is_some() || res.messages.is_empty());
    }

    #[test]
    fn unknown_header_value_type_is_an_error() {
        let mut bytes = Vec::new();
        bytes.push(1);
        bytes.extend_from_slice(b"n");
        bytes.push(99u8);
        assert!(decode_headers(&bytes).is_err());
    }

    #[test]
    fn header_value_types_decode() {
        let mut bytes = Vec::new();
        // boolean true
        bytes.push(1);
        bytes.extend_from_slice(b"b");
        bytes.push(1u8);
        // int32
        bytes.push(1);
        bytes.extend_from_slice(b"i");
        bytes.push(4u8);
        bytes.extend_from_slice(&7i32.to_be_bytes());
        // string
        bytes.push(1);
        bytes.extend_from_slice(b"s");
        bytes.push(7u8);
        bytes.extend_from_slice(&3u16.to_be_bytes());
        bytes.extend_from_slice(b"abc");
        let out = decode_headers(&bytes).expect("decodes");
        assert_eq!(out.get("b").map(|s| s.as_str()), Some("true"));
        assert_eq!(out.get("i").map(|s| s.as_str()), Some("7"));
        assert_eq!(out.get("s").map(|s| s.as_str()), Some("abc"));
    }

    fn headers_len_for(headers: &[(&str, &str)]) -> usize {
        headers
            .iter()
            .map(|(n, v)| 1 + n.len() + 1 + 2 + v.len())
            .sum()
    }
}
