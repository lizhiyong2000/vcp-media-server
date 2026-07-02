/// RTMP Chunk Stream protocol encoder/decoder
/// Handles chunk basic header, message header, and chunk data assembly.
use bytes::{Buf, BufMut, BytesMut};
use std::collections::HashMap;
use tracing::{debug, error, info, trace, warn};

/// Chunk format types
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ChunkFmt {
    /// Full header (11 bytes): timestamp + msg_len + msg_type + msg_stream_id
    Type0 = 0,
    /// Abbreviated (7 bytes): timestamp_delta + msg_len + msg_type
    Type1 = 1,
    /// Minimal (3 bytes): timestamp_delta only
    Type2 = 2,
    /// No header (continuation)
    Type3 = 3,
}

/// Parsed chunk basic + message header info
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChunkHeader {
    pub fmt: ChunkFmt,
    pub chunk_stream_id: u32,
    pub timestamp: u32,
    pub msg_length: u32,
    pub msg_type_id: u8,
    pub msg_stream_id: u32,
    pub has_extended_timestamp: bool,
}

/// State for reassembling chunks into complete messages
#[derive(Debug)]
pub struct ChunkAssembler {
    timestamp: u32,
    msg_length: u32,
    msg_type_id: u8,
    msg_stream_id: u32,
    received: usize,
    buffer: Vec<u8>,
}

impl Default for ChunkAssembler {
    fn default() -> Self {
        Self {
            timestamp: 0,
            msg_length: 0,
            msg_type_id: 0,
            msg_stream_id: 0,
            received: 0,
            buffer: Vec::new(),
        }
    }
}

/// A complete RTMP message assembled from chunks
#[derive(Debug)]
pub struct RtmpMessage {
    pub msg_type: u8,
    pub timestamp: u32,
    pub msg_stream_id: u32,
    pub payload: Vec<u8>,
}

/// Parse a chunk from the buffer. Returns (header, header_size) or None if not enough data.
pub fn parse_chunk_header(
    buf: &[u8],
    chunk_message_headers: &mut HashMap<u32, ChunkHeader>,
) -> Option<(ChunkHeader, usize)> {
    if buf.is_empty() {
        return None;
    }

    if buf.len() > 0 {
        let preview_len = std::cmp::min(64, buf.len());
        let preview: Vec<String> = buf[..preview_len]
            .iter()
            .map(|b| format!("{:02x}", *b))
            .collect();
        debug!(
            "[RTMP] before parse: buf={} bytes, preview: {}",
            buf.len(),
            preview.join(" ")
        );
    }

    let fmt_val = (buf[0] >> 6) & 0x03;
    let cs_id_raw = buf[0] & 0x3F;

    let (basic_header_size, chunk_stream_id) = match cs_id_raw {
        0 => {
            if buf.len() < 2 {
                return None;
            }
            (2, 64 + buf[1] as u32)
        }
        1 => {
            if buf.len() < 3 {
                return None;
            }
            (3, 64 + buf[1] as u32 + (buf[2] as u32) * 256)
        }
        _ => (1, cs_id_raw as u32),
    };

    let fmt = match fmt_val {
        0 => ChunkFmt::Type0,
        1 => ChunkFmt::Type1,
        2 => ChunkFmt::Type2,
        3 => ChunkFmt::Type3,
        _ => return None,
    };

    let msg_header_size = match fmt {
        ChunkFmt::Type0 => 11,
        ChunkFmt::Type1 => 7,
        ChunkFmt::Type2 => 3,
        ChunkFmt::Type3 => 0,
    };

    if buf.len() < basic_header_size + msg_header_size {
        return None;
    }

    let mut offset = basic_header_size;
    let mut timestamp = 0u32;
    let mut msg_length = 0u32;
    let mut msg_type_id = 0u8;
    let mut msg_stream_id = 0u32;
    let mut has_extended_timestamp = false;

    // Parse timestamp (3 bytes for fmt 0, 1, 2)
    if matches!(fmt, ChunkFmt::Type0 | ChunkFmt::Type1 | ChunkFmt::Type2) {
        timestamp = ((buf[offset] as u32) << 16)
            | ((buf[offset + 1] as u32) << 8)
            | (buf[offset + 2] as u32);
        offset += 3;
    }

    // Parse msg_length + msg_type_id (for fmt 0, 1)
    if matches!(fmt, ChunkFmt::Type0 | ChunkFmt::Type1) {
        msg_length = ((buf[offset] as u32) << 16)
            | ((buf[offset + 1] as u32) << 8)
            | (buf[offset + 2] as u32);
        msg_type_id = buf[offset + 3];
        offset += 4;
    }

    // Parse msg_stream_id (for fmt 0 only, little-endian)
    if matches!(fmt, ChunkFmt::Type0) {
        msg_stream_id = (buf[offset] as u32)
            | ((buf[offset + 1] as u32) << 8)
            | ((buf[offset + 2] as u32) << 16)
            | ((buf[offset + 3] as u32) << 24);
        offset += 4;
    }

    // Extended timestamp
    if timestamp == 0xFFFFFF {
        if buf.len() < offset + 4 {
            return None;
        }
        timestamp = ((buf[offset] as u32) << 24)
            | ((buf[offset + 1] as u32) << 16)
            | ((buf[offset + 2] as u32) << 8)
            | (buf[offset + 3] as u32);
        offset += 4;
        has_extended_timestamp = true;
    }

    if fmt != ChunkFmt::Type0 {
        if let Some(last_header) = chunk_message_headers.get(&chunk_stream_id) {
            match fmt {
                ChunkFmt::Type1 => {
                    timestamp += last_header.timestamp;
                    msg_stream_id = last_header.msg_stream_id;
                }
                ChunkFmt::Type2 => {
                    timestamp += last_header.timestamp;
                    msg_length = last_header.msg_length;
                    msg_type_id = last_header.msg_type_id;
                    msg_stream_id = last_header.msg_stream_id;
                }
                ChunkFmt::Type3 => {
                    timestamp = last_header.timestamp;
                    msg_length = last_header.msg_length;
                    msg_type_id = last_header.msg_type_id;
                    msg_stream_id = last_header.msg_stream_id;
                }
                _ => {}
            }
            //
        } else {
            error!(
                "previous header for CSID {} not found in headers",
                chunk_stream_id
            );
            return None;
        }
    }

    let chunk_header = ChunkHeader {
        fmt,
        chunk_stream_id,
        timestamp,
        msg_length,
        msg_type_id,
        msg_stream_id,
        has_extended_timestamp,
    };

    debug!("[RTMP] header parse success: {:?}", chunk_header);
    chunk_message_headers.insert(chunk_stream_id, chunk_header.clone());

    Some((chunk_header, offset))
}

/// Parse chunks from a buffer and reassemble into complete messages.
/// Returns a complete message if one is assembled.
pub fn parse_chunks(
    buf: &mut BytesMut,
    assemblers: &mut HashMap<u32, ChunkAssembler>,
    chunk_message_headers: &mut HashMap<u32, ChunkHeader>,
    chunk_size: usize,
) -> Option<RtmpMessage> {
    loop {
        let (header, header_size) = parse_chunk_header(buf, chunk_message_headers)?;

        let mut remaining_msg = header.msg_length as usize;

        if let Some(assembler) = assemblers.get(&header.chunk_stream_id) {
            if assembler.msg_length > 0 {
                // info!("[RTMP] message parse CSID {} has assembler: {} received: {} header: {}", header.chunk_stream_id, assembler.msg_length, assembler.received, header.msg_length);
                remaining_msg = (assembler.msg_length as usize).saturating_sub(assembler.received);
            }
        }

        let data_size = std::cmp::min(chunk_size, remaining_msg);

        // info!("[RTMP] message parse: buf {} bytes, header_size: {}, remaining_msg: {}, data_size: {}", buf.len(), header_size, remaining_msg, data_size);

        if buf.len() < header_size + data_size {
            debug!("[RTMP] message parse failed: not enough data buf {} bytes, header_size: {}, remaining_msg: {}, data_size: {}", buf.len(), header_size, remaining_msg, data_size);
            return None;
        }

        let data = &buf[header_size..header_size + data_size];

        let assembler = assemblers
            .entry(header.chunk_stream_id)
            .or_insert_with(ChunkAssembler::default);
        assembler.timestamp = header.timestamp;
        assembler.msg_length = header.msg_length;
        assembler.msg_type_id = header.msg_type_id;
        assembler.msg_stream_id = header.msg_stream_id;
        // Update state based on fmt

        // match header.fmt {
        //     ChunkFmt::Type0 => {
        //         assembler.timestamp = header.timestamp;
        //         assembler.msg_length = header.msg_length;
        //         assembler.msg_type_id = header.msg_type_id;
        //         assembler.msg_stream_id = header.msg_stream_id;
        //
        //     }
        //     ChunkFmt::Type1 => {
        //         assembler.timestamp = header.timestamp;
        //         assembler.msg_length = header.msg_length;
        //         assembler.msg_type_id = header.msg_type_id;
        //     }
        //
        //     ChunkFmt::Type2 => {
        //         assembler.timestamp = assembler.timestamp.wrapping_add(header.timestamp);
        //         // Use existing state
        //     }
        //
        //     ChunkFmt::Type3 => {
        //         // Use existing state
        //         // assembler.timestamp = header.timestamp;
        //         // assembler.msg_length = header.msg_length;
        //         // assembler.msg_type_id = header.msg_type_id;
        //         // assembler.msg_stream_id = header.msg_stream_id;
        //     }
        // }

        assembler.buffer.extend_from_slice(data);
        assembler.received += data_size;

        buf.advance(header_size + data_size);

        // Check if message is complete
        if assembler.received >= assembler.msg_length as usize && assembler.msg_length > 0 {
            let msg = RtmpMessage {
                msg_type: assembler.msg_type_id,
                timestamp: assembler.timestamp,
                msg_stream_id: assembler.msg_stream_id,
                payload: assembler.buffer.clone(),
            };
            assembler.msg_length = 0;
            assembler.received = 0;
            assembler.buffer.clear();

            // info!("[RTMP] message parse assembler for CSID {} update: {:?}", header.chunk_stream_id, assembler);

            // assemblers.remove(&header.chunk_stream_id);
            // info!("[RTMP] message parse assembler for CSID {} deleted", header.chunk_stream_id);
            return Some(msg);
        }
    }
}

/// RTMP chunk stream IDs (Adobe convention).
pub const CSID_PROTOCOL: u32 = 2;
pub const CSID_COMMAND: u32 = 3;
pub const CSID_AUDIO: u32 = 4;
pub const CSID_VIDEO: u32 = 6;

/// Encode a complete RTMP message as chunks
pub fn encode_message(
    msg_type: u8,
    timestamp: u32,
    msg_stream_id: u32,
    payload: &[u8],
    chunk_size: usize,
    chunk_stream_id: u32,
) -> Vec<u8> {
    let mut output = Vec::new();
    let mut offset = 0;
    let mut first = true;

    while offset < payload.len() {
        let data_len = std::cmp::min(chunk_size, payload.len() - offset);

        if first {
            // Type 0 chunk header
            encode_basic_header(&mut output, 0, chunk_stream_id);
            encode_type0_header(
                &mut output,
                timestamp,
                payload.len() as u32,
                msg_type,
                msg_stream_id,
            );
            first = false;
        } else {
            // Type 3 chunk header (continuation)
            encode_basic_header(&mut output, 3, chunk_stream_id);
        }

        output.extend_from_slice(&payload[offset..offset + data_len]);
        offset += data_len;
    }

    // Handle empty payload
    if payload.is_empty() {
        encode_basic_header(&mut output, 0, chunk_stream_id);
        encode_type0_header(&mut output, timestamp, 0, msg_type, msg_stream_id);
    }

    output
}

/// Encode chunk basic header
fn encode_basic_header(buf: &mut Vec<u8>, fmt: u8, cs_id: u32) {
    if cs_id < 64 {
        buf.push((fmt << 6) | (cs_id as u8));
    } else if cs_id < 320 {
        buf.push((fmt << 6) | 0);
        buf.push((cs_id - 64) as u8);
    } else {
        buf.push((fmt << 6) | 1);
        let v = cs_id - 64;
        buf.push((v & 0xFF) as u8);
        buf.push(((v >> 8) & 0xFF) as u8);
    }
}

/// Encode Type 0 message header
fn encode_type0_header(
    buf: &mut Vec<u8>,
    timestamp: u32,
    msg_length: u32,
    msg_type: u8,
    msg_stream_id: u32,
) {
    if timestamp >= 0xFFFFFF {
        buf.extend_from_slice(&[0xFF, 0xFF, 0xFF]);
    } else {
        buf.push(((timestamp >> 16) & 0xFF) as u8);
        buf.push(((timestamp >> 8) & 0xFF) as u8);
        buf.push((timestamp & 0xFF) as u8);
    }

    buf.push(((msg_length >> 16) & 0xFF) as u8);
    buf.push(((msg_length >> 8) & 0xFF) as u8);
    buf.push((msg_length & 0xFF) as u8);

    buf.push(msg_type);

    // msg_stream_id (little-endian)
    buf.push((msg_stream_id & 0xFF) as u8);
    buf.push(((msg_stream_id >> 8) & 0xFF) as u8);
    buf.push(((msg_stream_id >> 16) & 0xFF) as u8);
    buf.push(((msg_stream_id >> 24) & 0xFF) as u8);

    // Extended timestamp
    if timestamp >= 0xFFFFFF {
        buf.extend_from_slice(&timestamp.to_be_bytes());
    }
}

/// Encode a Set Chunk Size protocol control message
pub fn encode_set_chunk_size(new_size: u32) -> Vec<u8> {
    encode_message(0x01, 0, 0, &new_size.to_be_bytes(), 128, 2)
}

/// Encode a Window Ack Size message
pub fn encode_window_ack_size(size: u32) -> Vec<u8> {
    encode_message(0x05, 0, 0, &size.to_be_bytes(), 128, 2)
}

/// Encode a Set Peer Bandwidth message
pub fn encode_set_peer_bandwidth(size: u32, limit_type: u8) -> Vec<u8> {
    let mut payload = Vec::with_capacity(5);
    payload.extend_from_slice(&size.to_be_bytes());
    payload.push(limit_type);
    encode_message(0x06, 0, 0, &payload, 128, 2)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encode_decode_roundtrip() {
        let payload = b"hello world";
        let encoded = encode_message(0x14, 100, 1, payload, 128, 3);

        let mut buf = BytesMut::from(&encoded[..]);
        let mut assemblers = HashMap::new();

        let mut chunk_message_headers = HashMap::new();
        let msg = parse_chunks(&mut buf, &mut assemblers, &mut chunk_message_headers, 128).unwrap();

        assert_eq!(msg.msg_type, 0x14);
        assert_eq!(msg.timestamp, 100);
        assert_eq!(msg.msg_stream_id, 1);
        assert_eq!(msg.payload, payload);
    }

    #[test]
    fn test_chunked_message() {
        let payload = vec![0u8; 300];
        let encoded = encode_message(0x09, 50, 1, &payload, 128, 3);

        let mut buf = BytesMut::from(&encoded[..]);
        let mut assemblers = HashMap::new();
        let mut chunk_message_headers = HashMap::new();
        let msg = parse_chunks(&mut buf, &mut assemblers, &mut chunk_message_headers, 128).unwrap();

        assert_eq!(msg.msg_type, 0x09);
        assert_eq!(msg.payload.len(), 300);
    }
}
