use std::io::{Error, ErrorKind, Result};

use super::{
    feature::Feature,
    wire::{ProtoRead, ProtoWrite},
};

// Column represents a single column in ClickHouse term.
#[derive(Debug, Clone)]
pub struct Column {
    pub name: String,
    pub data_type: String,
    pub serialization: Serialization,
    pub data: ColumnData,
}

// Serialization describes the on-wire format used for this column's data.
// Feature-gated by CUSTOM_SERIALIZATION (v54454). External clients almost
// always produce Default; server may send non-Default for sparse /
// low-cardinality columns.
#[derive(Debug, Clone)]
pub enum Serialization {
    /// Default per-type serialization — values encoded directly, one per row.
    Default,
    /// Server-emitted non-default format (sparse, low-cardinality dictionary,
    /// etc.). The `kind_stack` is opaque metadata describing the variant.
    /// Decoding support is not yet implemented.
    Custom { kind_stack: Vec<u8> },
}

// ColumnData is in-memory representation of a single column data in ClickHouse terms
// Every value has single type.
#[derive(Debug, Clone)]
pub enum ColumnData {
    Uint8(Vec<u8>),
    Uint16(Vec<u16>),
    Uint32(Vec<u32>),
    Uint64(Vec<u64>),
    Int8(Vec<i8>),
    Int32(Vec<i32>),
    Int64(Vec<i64>),
    String(Vec<String>),
    // DateTime is UInt32 seconds-since-epoch on the wire.
    DateTime(Vec<u32>),
}

impl Column {
    pub fn encode(&self, w: &mut impl ProtoWrite, protocol: u32) -> Result<()> {
        w.write_string(&self.name)?;
        w.write_string(&self.data_type)?;

        if Feature::CUSTOM_SERIALIZATION.in_version(protocol) {
            match &self.serialization {
                Serialization::Default => w.write_u8(0)?,
                Serialization::Custom { kind_stack } => {
                    w.write_u8(1)?;
                    w.write_all(kind_stack)?;
                }
            }
        }

        self.data.encode(w)?;
        Ok(())
    }

    pub fn decode(r: &mut impl ProtoRead, rows: usize, protocol: u32) -> Result<Column> {
        let name = r.read_string()?;
        let data_type = r.read_string()?;

        let serialization = if Feature::CUSTOM_SERIALIZATION.in_version(protocol) {
            let has_custom = r.read_u8()?;
            match has_custom {
                0 => Serialization::Default,
                1 => {
                    return Err(Error::new(
                        ErrorKind::Unsupported,
                        format!("custom serialization for column '{name}' not yet supported"),
                    ));
                }
                other => {
                    return Err(Error::new(
                        ErrorKind::InvalidData,
                        format!("invalid has_custom_serialization byte: {other}"),
                    ));
                }
            }
        } else {
            Serialization::Default
        };

        let data = ColumnData::decode(r, &data_type, rows)?;
        Ok(Column {
            name,
            data_type,
            serialization,
            data,
        })
    }
}

impl ColumnData {
    pub fn encode(&self, w: &mut impl ProtoWrite) -> Result<()> {
        match self {
            ColumnData::Uint8(v) => {
                for &x in v {
                    w.write_u8(x)?;
                }
            }
            ColumnData::Uint16(v) => {
                for &x in v {
                    w.write_u16(x)?;
                }
            }
            ColumnData::Uint32(v) | ColumnData::DateTime(v) => {
                for &x in v {
                    w.write_u32(x)?;
                }
            }
            ColumnData::Uint64(v) => {
                for &x in v {
                    w.write_u64(x)?;
                }
            }
            ColumnData::Int8(v) => {
                for &x in v {
                    w.write_u8(x as u8)?;
                }
            }
            ColumnData::Int32(v) => {
                for &x in v {
                    w.write_i32(x)?;
                }
            }
            ColumnData::Int64(v) => {
                for &x in v {
                    w.write_i64(x)?;
                }
            }
            ColumnData::String(v) => {
                for s in v {
                    w.write_string(s)?;
                }
            }
        }
        Ok(())
    }

    pub fn decode(r: &mut impl ProtoRead, data_type: &str, rows: usize) -> Result<ColumnData> {
        // Strip parameterized types like "DateTime('UTC')" → "DateTime"
        let base_type = data_type.split('(').next().unwrap_or(data_type).trim();

        match base_type {
            "UInt8" => {
                let mut v = Vec::with_capacity(rows);
                for _ in 0..rows {
                    v.push(r.read_u8()?);
                }
                Ok(ColumnData::Uint8(v))
            }
            "UInt16" => {
                let mut v = Vec::with_capacity(rows);
                for _ in 0..rows {
                    v.push(r.read_u16()?);
                }
                Ok(ColumnData::Uint16(v))
            }
            "UInt32" => {
                let mut v = Vec::with_capacity(rows);
                for _ in 0..rows {
                    v.push(r.read_u32()?);
                }
                Ok(ColumnData::Uint32(v))
            }
            "UInt64" => {
                let mut v = Vec::with_capacity(rows);
                for _ in 0..rows {
                    v.push(r.read_u64()?);
                }
                Ok(ColumnData::Uint64(v))
            }
            // Enum8 is wire-compatible with Int8 (single byte per row).
            "Int8" | "Enum8" => {
                let mut v = Vec::with_capacity(rows);
                for _ in 0..rows {
                    v.push(r.read_u8()? as i8);
                }
                Ok(ColumnData::Int8(v))
            }
            "Int32" => {
                let mut v = Vec::with_capacity(rows);
                for _ in 0..rows {
                    v.push(r.read_i32()?);
                }
                Ok(ColumnData::Int32(v))
            }
            "Int64" => {
                let mut v = Vec::with_capacity(rows);
                for _ in 0..rows {
                    v.push(r.read_i64()?);
                }
                Ok(ColumnData::Int64(v))
            }
            "DateTime" => {
                let mut v = Vec::with_capacity(rows);
                for _ in 0..rows {
                    v.push(r.read_u32()?);
                }
                Ok(ColumnData::DateTime(v))
            }
            "String" => {
                let mut v = Vec::with_capacity(rows);
                for _ in 0..rows {
                    v.push(r.read_string()?);
                }
                Ok(ColumnData::String(v))
            }
            _ => Err(Error::new(
                ErrorKind::Unsupported,
                format!("column type '{data_type}' not yet supported"),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    const PROTOCOL: u32 = 54459;
    const PROTOCOL_PRE_CUSTOM: u32 = 54453; // before CUSTOM_SERIALIZATION (54454)

    #[test]
    fn test_column_uint8_roundtrip() {
        let col = Column {
            name: "id".to_string(),
            data_type: "UInt8".to_string(),
            serialization: Serialization::Default,
            data: ColumnData::Uint8(vec![1, 2, 3, 255, 0]),
        };
        let mut buf = Vec::new();
        col.encode(&mut buf, PROTOCOL).unwrap();

        let mut cursor = Cursor::new(buf.as_slice());
        let decoded = Column::decode(&mut cursor, 5, PROTOCOL).unwrap();
        assert_eq!(decoded.name, "id");
        assert_eq!(decoded.data_type, "UInt8");
        match decoded.data {
            ColumnData::Uint8(v) => assert_eq!(v, vec![1, 2, 3, 255, 0]),
            _ => panic!("expected UInt8"),
        }
    }

    #[test]
    fn test_column_string_roundtrip() {
        let col = Column {
            name: "name".to_string(),
            data_type: "String".to_string(),
            serialization: Serialization::Default,
            data: ColumnData::String(vec![
                "hello".to_string(),
                "".to_string(),
                "world".to_string(),
            ]),
        };
        let mut buf = Vec::new();
        col.encode(&mut buf, PROTOCOL).unwrap();

        let mut cursor = Cursor::new(buf.as_slice());
        let decoded = Column::decode(&mut cursor, 3, PROTOCOL).unwrap();
        assert_eq!(decoded.name, "name");
        match decoded.data {
            ColumnData::String(v) => assert_eq!(v, vec!["hello", "", "world"]),
            _ => panic!("expected String"),
        }
    }

    #[test]
    fn test_column_empty() {
        let col = Column {
            name: "x".to_string(),
            data_type: "UInt8".to_string(),
            serialization: Serialization::Default,
            data: ColumnData::Uint8(vec![]),
        };
        let mut buf = Vec::new();
        col.encode(&mut buf, PROTOCOL).unwrap();

        let mut cursor = Cursor::new(buf.as_slice());
        let decoded = Column::decode(&mut cursor, 0, PROTOCOL).unwrap();
        match decoded.data {
            ColumnData::Uint8(v) => assert_eq!(v.len(), 0),
            _ => panic!("expected UInt8"),
        }
    }

    #[test]
    fn test_column_unsupported_type() {
        // Manually encode: name="x", type="Array(Int32)" (not yet supported), has_custom=0
        let mut buf = Vec::new();
        buf.write_string("x").unwrap();
        buf.write_string("Array(Int32)").unwrap();
        buf.write_u8(0).unwrap();

        let mut cursor = Cursor::new(buf.as_slice());
        let err = Column::decode(&mut cursor, 0, PROTOCOL).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Unsupported);
    }

    #[test]
    fn test_column_unicode_values() {
        let col = Column {
            name: "名前".to_string(),
            data_type: "String".to_string(),
            serialization: Serialization::Default,
            data: ColumnData::String(vec!["日本語".to_string(), "пароль".to_string()]),
        };
        let mut buf = Vec::new();
        col.encode(&mut buf, PROTOCOL).unwrap();

        let mut cursor = Cursor::new(buf.as_slice());
        let decoded = Column::decode(&mut cursor, 2, PROTOCOL).unwrap();
        assert_eq!(decoded.name, "名前");
        match decoded.data {
            ColumnData::String(v) => assert_eq!(v, vec!["日本語", "пароль"]),
            _ => panic!("expected String"),
        }
    }

    #[test]
    fn test_column_wire_includes_custom_serialization_byte() {
        // At PROTOCOL (>= 54454), the wire must include a `has_custom_serialization`
        // byte between the type string and the column data.
        let col = Column {
            name: "x".to_string(),
            data_type: "UInt8".to_string(),
            serialization: Serialization::Default,
            data: ColumnData::Uint8(vec![]),
        };
        let mut buf = Vec::new();
        col.encode(&mut buf, PROTOCOL).unwrap();

        // Expected wire: [1 "x"] [5 "UInt8"] [0 (has_custom)]
        assert_eq!(buf, vec![0x01, b'x', 0x05, b'U', b'I', b'n', b't', b'8', 0x00]);
    }

    #[test]
    fn test_column_wire_omits_custom_serialization_byte_pre_feature() {
        // Before CUSTOM_SERIALIZATION (54454), the byte is NOT written.
        let col = Column {
            name: "x".to_string(),
            data_type: "UInt8".to_string(),
            serialization: Serialization::Default,
            data: ColumnData::Uint8(vec![]),
        };
        let mut buf = Vec::new();
        col.encode(&mut buf, PROTOCOL_PRE_CUSTOM).unwrap();

        // Expected wire: [1 "x"] [5 "UInt8"] (no has_custom byte)
        assert_eq!(buf, vec![0x01, b'x', 0x05, b'U', b'I', b'n', b't', b'8']);
    }

    #[test]
    fn test_column_rejects_custom_serialization_on_decode() {
        // Server sends has_custom=1, we should reject with Unsupported.
        let mut buf = Vec::new();
        buf.write_string("x").unwrap();
        buf.write_string("UInt8").unwrap();
        buf.write_u8(1).unwrap();

        let mut cursor = Cursor::new(buf.as_slice());
        let err = Column::decode(&mut cursor, 0, PROTOCOL).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Unsupported);
    }
}
