// According to ClickHouse protocol spec.
// It uses VarInt encoding (LEB-128)
// The VarUint goes max cap of 9 bytes (9 * 7 data bit == 63-bits. We use 1 bit per each byte as continuation flag)
// https://github.com/ClickHouse/ClickHouse/blob/master/src/IO/VarInt.h#L11
static MAX_UVARINT_BYTES_LEN: u8 = 9;
pub fn encode(mut x: u64) -> Vec<u8> {
    let mut res: Vec<u8> = Vec::new();
    while x != 0 {
        let mut curr: u8 = 0;
        let data = (x & 0x7F) as u8;
        x >>= 7;
        curr |= data;
        if x != 0 {
            curr |= 0x80;
        }
        res.push(curr);
    }
    res
}

pub fn decode(v: &[u8]) -> u64 {
    let mut res: u64 = 0;
    let mut shift = 0;
    for x in v {
        if x == &0u8 {
            continue;
        }

        let mut data = (x & 0x7F) as u64;
        data <<= shift;
        shift += 7;
        res |= data;
    }
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_zero() {
        let encoded = encode(0);
        assert_eq!(encoded, vec![]);
        assert_eq!(decode(&encoded), 0);
    }

    #[test]
    fn test_single_byte_values() {
        // Values 1..=127 fit in 1 byte (7 data bits)
        for x in 1..=127u64 {
            let encoded = encode(x);
            assert_eq!(encoded.len(), 1, "value {x} should encode to 1 byte");
            assert_eq!(decode(&encoded), x, "roundtrip failed for {x}");
        }
    }

    #[test]
    fn test_two_byte_values() {
        // Values 128..=16383 need 2 bytes (14 data bits)
        let cases = [128, 255, 256, 1000, 16383];
        for x in cases {
            let encoded = encode(x);
            assert_eq!(encoded.len(), 2, "value {x} should encode to 2 bytes");
            println!("x{} {:?}", x, encoded);
            assert_eq!(decode(&encoded), x, "roundtrip failed for {x}");
        }
    }

    #[test]
    fn test_byte_boundaries() {
        // Test values right at each byte-count boundary: 2^7, 2^14, ..., 2^63
        // n bytes encodes up to (2^(7*n) - 1)
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

            // One above the boundary needs n+1 bytes (except at 9 bytes which is the max)
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
        // Verify actual byte sequences against LEB-128 spec
        assert_eq!(encode(1), vec![0x01]);
        assert_eq!(encode(127), vec![0x7F]);
        assert_eq!(encode(128), vec![0x80, 0x01]);
        assert_eq!(encode(300), vec![0xAC, 0x02]);
        assert_eq!(encode(16384), vec![0x80, 0x80, 0x01]);
    }

    #[test]
    fn test_decode_ignores_trailing_bytes() {
        // decode processes all bytes given — verify behavior with extra bytes
        let mut bytes = encode(23);
        let original_len = bytes.len();
        bytes.push(0x00);
        // Extra zero byte doesn't change value since data bits are 0
        assert_eq!(decode(&bytes), 23);
    }
}
