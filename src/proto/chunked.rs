// Chunked-packet framing layer for the v54470+ "chunked protocol".
//
// Wire format (matches `ClickHouse/src/IO/{Read,Write}BufferFromPocoSocketChunked.{h,cpp}`):
//
//   <packet> = <chunk>+ <0-terminator>
//   <chunk>  = [4 bytes LE chunk_size] [chunk_size bytes]
//   <0-terminator> = [4 bytes LE = 0]
//
// A single packet may span multiple chunks if the writer's buffer fills mid-
// packet. The trailing 4-byte zero marks "end of packet" — the reader skips
// it transparently when crossing a packet boundary mid-read sequence.
//
// Negotiation happens during Addendum: each side declares one of "chunked",
// "notchunked", "chunked_optional", "notchunked_optional" for both send and
// receive directions. After the handshake, both sides switch to chunked
// framing for the rest of the connection if the final negotiated value for
// that direction is "chunked".

use std::io::{self, Read, Write};

const TERMINATOR_LEN: usize = 4;

/// Wraps a byte-oriented stream `S`, optionally applying chunked framing
/// to each direction independently.
///
/// - When `read_chunked` is `false`, reads pass through unchanged.
/// - When `read_chunked` is `true`, the next chunk header is fetched lazily
///   when the current chunk drains. A zero-size header (the packet
///   terminator) is consumed transparently; the caller never sees it.
/// - When `write_chunked` is `false`, writes pass through unchanged.
/// - When `write_chunked` is `true`, bytes are buffered until `flush()` is
///   called, at which point one `[size][data][0]` packet is emitted.
///
/// The two flags are independent so the caller can mirror the negotiated
/// per-direction outcome (`proto_send_chunked` / `proto_recv_chunked`).
#[derive(Debug)]
pub struct ChunkedStream<S> {
    inner: S,
    read_chunked: bool,
    write_chunked: bool,
    /// Bytes remaining in the current chunk on the read side. Meaningful
    /// only when `read_chunked` is `true`.
    read_remaining: u32,
    /// Pending bytes to flush as the next outgoing chunk. Meaningful only
    /// when `write_chunked` is `true`.
    write_buf: Vec<u8>,
}

impl<S> ChunkedStream<S> {
    /// Construct a passthrough wrapper. Both directions are unframed until
    /// `enable_read_chunked` / `enable_write_chunked` are called.
    pub fn new(inner: S) -> Self {
        Self {
            inner,
            read_chunked: false,
            write_chunked: false,
            read_remaining: 0,
            write_buf: Vec::new(),
        }
    }

    /// Switch the read side to chunked framing. Must be called BEFORE any
    /// chunked bytes appear on the wire.
    pub fn enable_read_chunked(&mut self) {
        self.read_chunked = true;
    }

    /// Switch the write side to chunked framing.
    pub fn enable_write_chunked(&mut self) {
        self.write_chunked = true;
        if self.write_buf.capacity() == 0 {
            self.write_buf.reserve(8 * 1024);
        }
    }

    pub fn read_chunked(&self) -> bool {
        self.read_chunked
    }

    pub fn write_chunked(&self) -> bool {
        self.write_chunked
    }

    pub fn get_ref(&self) -> &S {
        &self.inner
    }
}

impl<S: Read> Read for ChunkedStream<S> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if !self.read_chunked {
            return self.inner.read(buf);
        }
        while self.read_remaining == 0 {
            let mut header = [0u8; 4];
            self.inner.read_exact(&mut header)?;
            let size = u32::from_le_bytes(header);
            if size == 0 {
                // Packet terminator. Skip it; loop to read the next header
                // (which starts the next packet's first chunk).
                continue;
            }
            self.read_remaining = size;
        }
        let n = std::cmp::min(buf.len(), self.read_remaining as usize);
        let read = self.inner.read(&mut buf[..n])?;
        self.read_remaining -= read as u32;
        Ok(read)
    }
}

impl<S: Write> Write for ChunkedStream<S> {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        if !self.write_chunked {
            return self.inner.write(data);
        }
        self.write_buf.extend_from_slice(data);
        Ok(data.len())
    }

    /// In chunked mode: flushes the buffered bytes as one chunk + the
    /// trailing zero terminator (one packet). In passthrough mode: just
    /// flushes the inner stream.
    fn flush(&mut self) -> io::Result<()> {
        if self.write_chunked && !self.write_buf.is_empty() {
            let size = self.write_buf.len() as u32;
            self.inner.write_all(&size.to_le_bytes())?;
            self.inner.write_all(&self.write_buf)?;
            self.inner.write_all(&[0u8; TERMINATOR_LEN])?;
            self.write_buf.clear();
        }
        self.inner.flush()
    }
}

/// Negotiation logic for one direction. Returns the agreed-on string
/// (either `"chunked"` or `"notchunked"`).
///
/// Mirrors the `is_chunked` lambda in `Client/Connection.cpp::connect`:
/// - If the server is optional, use the client's preference.
/// - Else if the client is optional, use the server's preference.
/// - Else they must agree.
pub fn negotiate(
    server: &str,
    client: &str,
    direction: &str,
) -> io::Result<&'static str> {
    let server_wants_chunked = server.starts_with("chunked");
    let server_optional = server.ends_with("_optional");
    let client_wants_chunked = client.starts_with("chunked");
    let client_optional = client.ends_with("_optional");

    let agreed_chunked = if server_optional {
        client_wants_chunked
    } else if client_optional {
        server_wants_chunked
    } else if server_wants_chunked != client_wants_chunked {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "chunked protocol incompatible on {direction}: server wants {}, client wants {}",
                if server_wants_chunked { "chunked" } else { "notchunked" },
                if client_wants_chunked { "chunked" } else { "notchunked" },
            ),
        ));
    } else {
        server_wants_chunked
    };

    Ok(if agreed_chunked { "chunked" } else { "notchunked" })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    // -- Read side --

    fn make_chunked_reader(wire: Vec<u8>) -> ChunkedStream<Cursor<Vec<u8>>> {
        let mut s = ChunkedStream::new(Cursor::new(wire));
        s.enable_read_chunked();
        s
    }

    fn make_chunked_writer() -> ChunkedStream<Cursor<Vec<u8>>> {
        let mut s = ChunkedStream::new(Cursor::new(Vec::new()));
        s.enable_write_chunked();
        s
    }

    #[test]
    fn read_single_chunk_packet() {
        let mut wire = Vec::new();
        wire.extend(&5u32.to_le_bytes());
        wire.extend(b"hello");
        wire.extend(&0u32.to_le_bytes());
        let mut chunked = make_chunked_reader(wire);

        let mut buf = [0u8; 5];
        chunked.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"hello");
    }

    #[test]
    fn read_packet_split_across_two_chunks() {
        let mut wire = Vec::new();
        wire.extend(&3u32.to_le_bytes());
        wire.extend(b"abc");
        wire.extend(&2u32.to_le_bytes());
        wire.extend(b"de");
        wire.extend(&0u32.to_le_bytes());
        let mut chunked = make_chunked_reader(wire);

        let mut buf = [0u8; 5];
        chunked.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"abcde");
    }

    #[test]
    fn read_two_consecutive_packets() {
        let mut wire = Vec::new();
        wire.extend(&3u32.to_le_bytes());
        wire.extend(b"foo");
        wire.extend(&0u32.to_le_bytes());
        wire.extend(&3u32.to_le_bytes());
        wire.extend(b"bar");
        wire.extend(&0u32.to_le_bytes());
        let mut chunked = make_chunked_reader(wire);

        let mut buf1 = [0u8; 3];
        chunked.read_exact(&mut buf1).unwrap();
        assert_eq!(&buf1, b"foo");
        let mut buf2 = [0u8; 3];
        chunked.read_exact(&mut buf2).unwrap();
        assert_eq!(&buf2, b"bar");
    }

    #[test]
    fn read_partial_returns_available() {
        let mut wire = Vec::new();
        wire.extend(&5u32.to_le_bytes());
        wire.extend(b"hello");
        wire.extend(&0u32.to_le_bytes());
        let mut chunked = make_chunked_reader(wire);

        let mut buf = [0u8; 10];
        let n = chunked.read(&mut buf).unwrap();
        assert_eq!(n, 5);
        assert_eq!(&buf[..5], b"hello");
    }

    #[test]
    fn read_passthrough_when_not_enabled() {
        // Without enable_read_chunked, reads pass straight through.
        let wire = b"raw".to_vec();
        let mut chunked = ChunkedStream::new(Cursor::new(wire));
        let mut buf = [0u8; 3];
        chunked.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"raw");
    }

    // -- Write side --

    #[test]
    fn write_single_packet_layout() {
        let mut chunked = make_chunked_writer();
        chunked.write_all(b"hello").unwrap();
        chunked.flush().unwrap();

        let buf = chunked.inner.into_inner();
        assert_eq!(buf.len(), 4 + 5 + 4);
        assert_eq!(&buf[0..4], &5u32.to_le_bytes());
        assert_eq!(&buf[4..9], b"hello");
        assert_eq!(&buf[9..13], &0u32.to_le_bytes());
    }

    #[test]
    fn write_empty_flush_is_noop() {
        let mut chunked = make_chunked_writer();
        chunked.flush().unwrap();
        let buf = chunked.inner.into_inner();
        assert_eq!(buf.len(), 0);
    }

    #[test]
    fn write_passthrough_when_not_enabled() {
        let mut chunked = ChunkedStream::new(Cursor::new(Vec::new()));
        chunked.write_all(b"raw").unwrap();
        chunked.flush().unwrap();
        // No framing — bytes appear verbatim.
        assert_eq!(chunked.inner.into_inner(), b"raw");
    }

    #[test]
    fn write_then_read_roundtrip_loopback() {
        let mut writer = make_chunked_writer();
        writer.write_all(b"abc").unwrap();
        writer.flush().unwrap();
        writer.write_all(b"defg").unwrap();
        writer.flush().unwrap();
        let wire = writer.inner.into_inner();

        let mut reader = make_chunked_reader(wire);
        let mut b1 = [0u8; 3];
        reader.read_exact(&mut b1).unwrap();
        assert_eq!(&b1, b"abc");
        let mut b2 = [0u8; 4];
        reader.read_exact(&mut b2).unwrap();
        assert_eq!(&b2, b"defg");
    }

    // -- Negotiation --

    #[test]
    fn negotiate_both_optional_picks_client_pref() {
        // Both sides _optional → server defers to client.
        assert_eq!(
            negotiate("chunked_optional", "chunked_optional", "send").unwrap(),
            "chunked"
        );
        assert_eq!(
            negotiate("chunked_optional", "notchunked_optional", "send").unwrap(),
            "notchunked"
        );
    }

    #[test]
    fn negotiate_server_strict_overrides_client_optional() {
        assert_eq!(
            negotiate("chunked", "notchunked_optional", "send").unwrap(),
            "chunked"
        );
    }

    #[test]
    fn negotiate_strict_mismatch_errors() {
        let err = negotiate("chunked", "notchunked", "send").unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn negotiate_both_strict_match() {
        assert_eq!(negotiate("chunked", "chunked", "send").unwrap(), "chunked");
        assert_eq!(negotiate("notchunked", "notchunked", "send").unwrap(), "notchunked");
    }
}
