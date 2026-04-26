//! ClickHouse compression frame format (SPEC §9).
//!
//! Each compressed block on the wire is wrapped in a 25-byte header (16 byte
//! checksum + 9 byte size/method header) followed by the compressed body:
//!
//! ```text
//! [16 bytes: CityHash128 checksum over the 9-byte header + compressed body]
//! [1 byte:   method: 0x82 = LZ4, 0x90 = ZSTD, 0x02 = NONE]
//! [4 bytes LE: compressed size (includes the 9-byte header, excludes checksum)]
//! [4 bytes LE: uncompressed size]
//! [N bytes:  compressed body]
//! ```
//!
//! ClickHouse uses CityHash v1.0.2 (the historical variant), NOT modern
//! Google CityHash. The `clickhouse-rs-cityhash-sys` crate is the Rust
//! binding that produces compatible bytes.

use std::io::{Error, ErrorKind, Read, Result, Write};

use clickhouse_rs_cityhash_sys::city_hash_128;

/// Method byte at offset 16 in every frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum CompressionMethod {
    /// `0x02` — uncompressed body. Body is the raw bytes; the wrapping
    /// frame is otherwise identical (checksum still computed over the
    /// 9-byte header + body).
    None = 0x02,
    /// `0x82` — LZ4 block format (NOT the LZ4 frame format with magic
    /// number). `lz4_flex::block` decompresses this directly.
    Lz4 = 0x82,
    /// `0x90` — Raw zstd single-frame stream (no header beyond what zstd
    /// itself emits).
    Zstd = 0x90,
}

impl CompressionMethod {
    pub fn from_byte(b: u8) -> Option<Self> {
        match b {
            0x02 => Some(Self::None),
            0x82 => Some(Self::Lz4),
            0x90 => Some(Self::Zstd),
            _ => None,
        }
    }
}

/// Length of the framed-data prefix that the checksum covers (method byte +
/// compressed-size u32 + uncompressed-size u32).
const HEADER_LEN: usize = 9;
const CHECKSUM_LEN: usize = 16;

/// Encode `data` into a single compression frame (checksum + header + body).
///
/// Returns the framed bytes ready to be written to the wire.
pub fn encode_frame(data: &[u8], method: CompressionMethod) -> Result<Vec<u8>> {
    let body: Vec<u8> = match method {
        CompressionMethod::None => data.to_vec(),
        CompressionMethod::Lz4 => lz4_flex::block::compress(data),
        CompressionMethod::Zstd => zstd::stream::encode_all(data, 3)
            .map_err(|e| Error::new(ErrorKind::Other, format!("zstd encode: {e}")))?,
    };

    let compressed_size = (HEADER_LEN + body.len()) as u32;
    let uncompressed_size = data.len() as u32;

    // Build the 9-byte header + compressed body. Checksum covers exactly
    // these 9+N bytes.
    let mut header_and_body = Vec::with_capacity(HEADER_LEN + body.len());
    header_and_body.push(method as u8);
    header_and_body.extend_from_slice(&compressed_size.to_le_bytes());
    header_and_body.extend_from_slice(&uncompressed_size.to_le_bytes());
    header_and_body.extend_from_slice(&body);

    let checksum = city_hash_128(&header_and_body);

    let mut frame = Vec::with_capacity(CHECKSUM_LEN + header_and_body.len());
    frame.extend_from_slice(&checksum.lo.to_le_bytes());
    frame.extend_from_slice(&checksum.hi.to_le_bytes());
    frame.extend_from_slice(&header_and_body);
    Ok(frame)
}

/// Read one compression frame from `r` and return the decompressed body.
///
/// Verifies the CityHash128 checksum. Returns `InvalidData` on checksum
/// mismatch (corruption indicator — fail loudly).
pub fn decode_frame<R: Read>(r: &mut R) -> Result<Vec<u8>> {
    let mut checksum_bytes = [0u8; CHECKSUM_LEN];
    r.read_exact(&mut checksum_bytes)?;
    let expected_lo = u64::from_le_bytes(checksum_bytes[..8].try_into().unwrap());
    let expected_hi = u64::from_le_bytes(checksum_bytes[8..].try_into().unwrap());

    let mut header = [0u8; HEADER_LEN];
    r.read_exact(&mut header)?;
    let method_byte = header[0];
    let method = CompressionMethod::from_byte(method_byte).ok_or_else(|| {
        Error::new(
            ErrorKind::InvalidData,
            format!("unknown compression method byte: 0x{method_byte:02x}"),
        )
    })?;
    let compressed_size = u32::from_le_bytes([header[1], header[2], header[3], header[4]]) as usize;
    let uncompressed_size =
        u32::from_le_bytes([header[5], header[6], header[7], header[8]]) as usize;

    if compressed_size < HEADER_LEN {
        return Err(Error::new(
            ErrorKind::InvalidData,
            format!("compressed_size {compressed_size} < header length {HEADER_LEN}"),
        ));
    }
    let body_len = compressed_size - HEADER_LEN;
    let mut body = vec![0u8; body_len];
    r.read_exact(&mut body)?;

    // Recompute checksum over the 9-byte header + body (the same bytes the
    // sender hashed) and compare to the received checksum.
    let mut to_hash = Vec::with_capacity(HEADER_LEN + body_len);
    to_hash.extend_from_slice(&header);
    to_hash.extend_from_slice(&body);
    let actual = city_hash_128(&to_hash);
    if actual.lo != expected_lo || actual.hi != expected_hi {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "compression frame checksum mismatch (CityHash128) — corruption suspected",
        ));
    }

    let decompressed = match method {
        CompressionMethod::None => body,
        CompressionMethod::Lz4 => lz4_flex::block::decompress(&body, uncompressed_size)
            .map_err(|e| Error::new(ErrorKind::InvalidData, format!("lz4 decode: {e}")))?,
        CompressionMethod::Zstd => zstd::stream::decode_all(&body[..])
            .map_err(|e| Error::new(ErrorKind::InvalidData, format!("zstd decode: {e}")))?,
    };

    if decompressed.len() != uncompressed_size {
        return Err(Error::new(
            ErrorKind::InvalidData,
            format!(
                "decompressed length {} != uncompressed_size {} declared in header",
                decompressed.len(),
                uncompressed_size
            ),
        ));
    }
    Ok(decompressed)
}

/// Convenience: write a frame directly to `w`.
pub fn write_frame<W: Write>(w: &mut W, data: &[u8], method: CompressionMethod) -> Result<()> {
    let frame = encode_frame(data, method)?;
    w.write_all(&frame)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn test_method_byte_roundtrip() {
        assert_eq!(CompressionMethod::from_byte(0x02), Some(CompressionMethod::None));
        assert_eq!(CompressionMethod::from_byte(0x82), Some(CompressionMethod::Lz4));
        assert_eq!(CompressionMethod::from_byte(0x90), Some(CompressionMethod::Zstd));
        assert_eq!(CompressionMethod::from_byte(0x00), None);
    }

    #[test]
    fn test_none_frame_roundtrip() {
        let payload = b"hello world".to_vec();
        let frame = encode_frame(&payload, CompressionMethod::None).unwrap();
        // Frame layout: 16 (checksum) + 9 (header) + 11 (body) = 36 bytes.
        assert_eq!(frame.len(), 16 + 9 + payload.len());
        // Method byte at offset 16.
        assert_eq!(frame[16], 0x02);
        let mut cursor = Cursor::new(frame.as_slice());
        let decoded = decode_frame(&mut cursor).unwrap();
        assert_eq!(decoded, payload);
    }

    #[test]
    fn test_lz4_frame_roundtrip() {
        // Use repetitive data so LZ4 actually compresses.
        let payload: Vec<u8> = b"abcabcabc".repeat(50);
        let frame = encode_frame(&payload, CompressionMethod::Lz4).unwrap();
        assert_eq!(frame[16], 0x82);
        let mut cursor = Cursor::new(frame.as_slice());
        let decoded = decode_frame(&mut cursor).unwrap();
        assert_eq!(decoded, payload);
    }

    #[test]
    fn test_zstd_frame_roundtrip() {
        let payload: Vec<u8> = b"some-data-".repeat(100);
        let frame = encode_frame(&payload, CompressionMethod::Zstd).unwrap();
        assert_eq!(frame[16], 0x90);
        let mut cursor = Cursor::new(frame.as_slice());
        let decoded = decode_frame(&mut cursor).unwrap();
        assert_eq!(decoded, payload);
    }

    #[test]
    fn test_empty_payload_lz4() {
        let payload: Vec<u8> = Vec::new();
        let frame = encode_frame(&payload, CompressionMethod::Lz4).unwrap();
        let mut cursor = Cursor::new(frame.as_slice());
        let decoded = decode_frame(&mut cursor).unwrap();
        assert_eq!(decoded, payload);
    }

    #[test]
    fn test_checksum_mismatch_detected() {
        let payload = b"abcdefgh".to_vec();
        let mut frame = encode_frame(&payload, CompressionMethod::None).unwrap();
        // Corrupt the body (offset 25 = first byte after the 25-byte
        // checksum+header).
        frame[25] ^= 0xFF;
        let mut cursor = Cursor::new(frame.as_slice());
        let err = decode_frame(&mut cursor).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidData);
        assert!(err.to_string().contains("checksum"));
    }

    #[test]
    fn test_unknown_method_byte_rejected() {
        // Build a plausible-looking frame with an invalid method byte.
        let body = b"x".to_vec();
        let mut header_and_body = Vec::new();
        header_and_body.push(0xAB); // unknown method
        header_and_body.extend_from_slice(&((HEADER_LEN + body.len()) as u32).to_le_bytes());
        header_and_body.extend_from_slice(&(body.len() as u32).to_le_bytes());
        header_and_body.extend_from_slice(&body);
        let cs = city_hash_128(&header_and_body);
        let mut frame = Vec::new();
        frame.extend_from_slice(&cs.lo.to_le_bytes());
        frame.extend_from_slice(&cs.hi.to_le_bytes());
        frame.extend_from_slice(&header_and_body);
        let mut cursor = Cursor::new(frame.as_slice());
        let err = decode_frame(&mut cursor).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidData);
    }

    #[test]
    fn test_header_layout() {
        // For a 7-byte payload with NONE method:
        //   compressed_size = 9 (header) + 7 (body) = 16
        //   uncompressed_size = 7
        let payload = b"abcdefg".to_vec();
        let frame = encode_frame(&payload, CompressionMethod::None).unwrap();
        // Method byte
        assert_eq!(frame[16], 0x02);
        // compressed_size at offset 17 (LE u32)
        let cs = u32::from_le_bytes([frame[17], frame[18], frame[19], frame[20]]);
        assert_eq!(cs, 16);
        // uncompressed_size at offset 21 (LE u32)
        let us = u32::from_le_bytes([frame[21], frame[22], frame[23], frame[24]]);
        assert_eq!(us, 7);
        // body at offset 25
        assert_eq!(&frame[25..], payload.as_slice());
    }

    #[test]
    fn test_write_frame_helper() {
        let payload = b"sample".to_vec();
        let mut buf = Vec::new();
        write_frame(&mut buf, &payload, CompressionMethod::Lz4).unwrap();
        let mut cursor = Cursor::new(buf.as_slice());
        let decoded = decode_frame(&mut cursor).unwrap();
        assert_eq!(decoded, payload);
    }
}
