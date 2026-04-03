use std::io::{self, Read, Write};

// According to ClickHouse protocol spec.
// It uses VarInt encoding (LEB-128)
// The VarUint goes max cap of 9 bytes (9 * 7 data bit == 63-bits. We use 1 bit per each byte as continuation flag)
// https://github.com/ClickHouse/ClickHouse/blob/master/src/IO/VarInt.h#L11
static MAX_UVARINT_BYTES_LEN: u8 = 9;

pub trait ProtoWrite: Write {
    fn write_varuint(&mut self, mut x: u64) -> io::Result<()> {
        loop {
            let mut curr: u8 = 0;
            let data = (x & 0x7F) as u8;
            x >>= 7;
            curr |= data;
            if x != 0 {
                curr |= 0x80;
            }

            self.write(&[curr])?;
            if x == 0 {
                break;
            }
        }
        Ok(())
    }

    fn write_string(&self, s: &str) -> io::Result<()> {
        unimplemented!();
    }
}

impl<W: Write> ProtoWrite for W {}

pub trait ProtoRead: Read {
    fn read_varuint(&mut self) -> io::Result<u64> {
        let mut res: u64 = 0;
        let mut shift = 0;
        let mut buf: Vec<u8> = vec![0; 1];

        loop {
            self.read_exact(&mut buf)?;

            if buf.is_empty() {
                break;
            }

            let x = buf[0];

            let mut data = (x & 0x7F) as u64;
            let cont_bit = (x & 0x80) as u8;
            data <<= shift;
            shift += 7;
            res |= data;

            if cont_bit == 0u8 {
                break;
            }
        }
        Ok(res)
    }

    fn read_string(&self) -> io::Result<String> {
        unimplemented!();
    }
}

impl<R: Read> ProtoRead for R {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// Helper: encode a value into a Vec<u8> via ProtoWrite
    fn encode(x: u64) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.write_varuint(x).unwrap();
        buf
    }

    /// Helper: decode from a byte slice via ProtoRead
    fn decode(bytes: &[u8]) -> u64 {
        let mut cursor = Cursor::new(bytes);
        cursor.read_varuint().unwrap()
    }

    #[test]
    fn test_zero() {
        let encoded = encode(0);
        assert_eq!(
            encoded,
            vec![0],
            "encoded should be zero bytes but {encoded:?}"
        );
        assert_eq!(decode(&encoded), 0,);
    }

    #[test]
    fn test_single_byte_values() {
        for x in 1..=127u64 {
            let encoded = encode(x);
            assert_eq!(encoded.len(), 1, "value {x} should encode to 1 byte");
            assert_eq!(decode(&encoded), x, "roundtrip failed for {x}");
        }
    }

    #[test]
    fn test_two_byte_values() {
        let cases = [128, 255, 256, 1000, 16383];
        for x in cases {
            let encoded = encode(x);
            assert_eq!(encoded.len(), 2, "value {x} should encode to 2 bytes");
            assert_eq!(decode(&encoded), x, "roundtrip failed for {x}");
        }
    }

    #[test]
    fn test_byte_boundaries() {
        for n in 1..=9u32 {
            let bits = 7 * n;
            let max_for_n = if bits >= 64 {
                u64::MAX
            } else {
                (1u64 << bits) - 1
            };

            let encoded = encode(max_for_n);
            assert_eq!(
                encoded.len(),
                n as usize,
                "value {max_for_n} (2^{bits}-1) should encode to {n} bytes"
            );
            assert_eq!(decode(&encoded), max_for_n);

            if n < 9 {
                let one_above = max_for_n + 1;
                let encoded = encode(one_above);
                assert_eq!(
                    encoded.len(),
                    (n + 1) as usize,
                    "value {one_above} (2^{bits}) should encode to {} bytes",
                    n + 1
                );
                assert_eq!(decode(&encoded), one_above);
            }
        }
    }

    #[test]
    fn test_max_u64() {
        let encoded = encode(u64::MAX);
        assert_eq!(encoded.len(), 10, "u64::MAX needs 10 bytes in LEB-128");
        assert_eq!(decode(&encoded), u64::MAX);
    }

    #[test]
    fn test_powers_of_two() {
        for shift in 0..64u32 {
            let x = 1u64 << shift;
            assert_eq!(decode(&encode(x)), x, "roundtrip failed for 2^{shift}");
        }
    }

    #[test]
    fn test_known_encodings() {
        assert_eq!(encode(1), vec![0x01]);
        assert_eq!(encode(127), vec![0x7F]);
        assert_eq!(encode(128), vec![0x80, 0x01]);
        assert_eq!(encode(300), vec![0xAC, 0x02]);
        assert_eq!(encode(16384), vec![0x80, 0x80, 0x01]);
    }

    #[test]
    fn test_sequential_writes_and_reads() {
        let values: Vec<u64> = vec![0, 1, 127, 128, 16383, 16384, u64::MAX >> 1, u64::MAX];
        let mut buf = Vec::new();
        for &v in &values {
            buf.write_varuint(v).unwrap();
        }

        let mut cursor = Cursor::new(buf.as_slice());
        for &expected in &values {
            assert_eq!(cursor.read_varuint().unwrap(), expected);
        }
    }

    #[test]
    fn test_read_consumes_only_varint_bytes() {
        let mut buf = Vec::new();
        buf.write_varuint(300).unwrap();
        buf.write_varuint(42).unwrap();

        let mut cursor = Cursor::new(buf.as_slice());
        assert_eq!(cursor.read_varuint().unwrap(), 300);
        assert_eq!(cursor.read_varuint().unwrap(), 42);
    }
}
