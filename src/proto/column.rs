use std::io::{Error, ErrorKind, Result};

use super::wire::{ProtoRead, ProtoWrite};

// Column represents a single column in ClickHouse term.
#[derive(Debug)]
pub struct Column {
    pub name: String,
    pub data_type: String,
    pub data: ColumnData,
}

// ColumnData is in-memory representation of a single column data in ClickHouse terms
// Every value has single type.
#[derive(Debug)]
pub enum ColumnData {
    Uint8(Vec<u8>),
    String(Vec<String>),
}

impl Column {
    pub fn encode(&self, w: &mut impl ProtoWrite) -> Result<()> {
        w.write_string(&self.name)?;
        w.write_string(&self.data_type)?;
        self.data.encode(w)?;
        Ok(())
    }

    pub fn decode(r: &mut impl ProtoRead, rows: usize) -> Result<Column> {
        let name = r.read_string()?;
        let data_type = r.read_string()?;
        let data = ColumnData::decode(r, &data_type, rows)?;
        Ok(Column {
            name,
            data_type,
            data,
        })
    }
}

impl ColumnData {
    pub fn encode(&self, w: &mut impl ProtoWrite) -> Result<()> {
        match self {
            ColumnData::Uint8(v) => {
                for &b in v {
                    w.write_u8(b)?;
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
        match data_type {
            "UInt8" => {
                let mut v = Vec::with_capacity(rows);
                for _ in 0..rows {
                    v.push(r.read_u8()?);
                }
                Ok(ColumnData::Uint8(v))
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

    #[test]
    fn test_column_uint8_roundtrip() {
        let col = Column {
            name: "id".to_string(),
            data_type: "UInt8".to_string(),
            data: ColumnData::Uint8(vec![1, 2, 3, 255, 0]),
        };
        let mut buf = Vec::new();
        col.encode(&mut buf).unwrap();

        let mut cursor = Cursor::new(buf.as_slice());
        let decoded = Column::decode(&mut cursor, 5).unwrap();
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
            data: ColumnData::String(vec![
                "hello".to_string(),
                "".to_string(),
                "world".to_string(),
            ]),
        };
        let mut buf = Vec::new();
        col.encode(&mut buf).unwrap();

        let mut cursor = Cursor::new(buf.as_slice());
        let decoded = Column::decode(&mut cursor, 3).unwrap();
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
            data: ColumnData::Uint8(vec![]),
        };
        let mut buf = Vec::new();
        col.encode(&mut buf).unwrap();

        let mut cursor = Cursor::new(buf.as_slice());
        let decoded = Column::decode(&mut cursor, 0).unwrap();
        match decoded.data {
            ColumnData::Uint8(v) => assert_eq!(v.len(), 0),
            _ => panic!("expected UInt8"),
        }
    }

    #[test]
    fn test_column_unsupported_type() {
        // Manually encode: name="x", type="DateTime", no data
        let mut buf = Vec::new();
        buf.write_string("x").unwrap();
        buf.write_string("DateTime").unwrap();

        let mut cursor = Cursor::new(buf.as_slice());
        let err = Column::decode(&mut cursor, 0).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Unsupported);
    }

    #[test]
    fn test_column_unicode_values() {
        let col = Column {
            name: "名前".to_string(),
            data_type: "String".to_string(),
            data: ColumnData::String(vec!["日本語".to_string(), "пароль".to_string()]),
        };
        let mut buf = Vec::new();
        col.encode(&mut buf).unwrap();

        let mut cursor = Cursor::new(buf.as_slice());
        let decoded = Column::decode(&mut cursor, 2).unwrap();
        assert_eq!(decoded.name, "名前");
        match decoded.data {
            ColumnData::String(v) => assert_eq!(v, vec!["日本語", "пароль"]),
            _ => panic!("expected String"),
        }
    }
}
