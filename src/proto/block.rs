use std::io::{Error, ErrorKind, Result};

use super::{
    column::Column,
    feature::Feature,
    wire::{ProtoRead, ProtoWrite},
};

// Block is the unit of data processing in ClickHouse.
// It is group of some number of columns.
// Think of it like
// 1. Column -> Whole Vec<T> of single column values
// 2. Block -> Horizontal split of j
// It has some metadata information
pub struct Block {
    pub info: Option<BlockInfo>,
    pub columns: Vec<Column>,
    pub rows: usize,
}

impl Block {
    pub fn encode(&self, w: &mut impl ProtoWrite, protocol: u32) -> Result<()> {
        if Feature::BLOCK_INFO.in_version(protocol) {
            self.info
                .as_ref()
                .ok_or_else(|| {
                    Error::new(
                        ErrorKind::InvalidInput,
                        format!("block info is required in this protocol ({protocol})"),
                    )
                })?
                .encode(w)?;
        }
        w.write_varuint(self.columns.len() as u64)?;
        w.write_varuint(self.rows as u64)?;

        for col in &self.columns {
            col.encode(w, protocol)?;
        }
        Ok(())
    }
    pub fn new() -> Self {
        Block {
            info: Some(BlockInfo {
                overflows: false,
                bucket_number: -1,
            }),
            columns: vec![],
            rows: 0,
        }
    }

    pub fn decode(r: &mut impl ProtoRead, protocol: u32) -> Result<Block> {
        let info = if Feature::BLOCK_INFO.in_version(protocol) {
            Some(BlockInfo::decode(r)?)
        } else {
            None
        };

        let num_columns = r.read_varuint()? as usize;
        let num_rows = r.read_varuint()? as usize;

        // Empty block signals end-of-data. No column entries follow.
        if num_columns == 0 {
            return Ok(Block {
                info,
                columns: vec![],
                rows: num_rows,
            });
        }

        let mut columns = Vec::with_capacity(num_columns);
        for _ in 0..num_columns {
            columns.push(Column::decode(r, num_rows, protocol)?);
        }

        Ok(Block {
            info,
            columns,
            rows: num_rows,
        })
    }
}

// BlockInfo is a metadata about the block
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct BlockInfo {
    pub overflows: bool,
    pub bucket_number: i32,
}

// BlockInfoField is used in encoding delimeters for actual block info field values
pub enum BlockInfoField {
    Overflow = 1,
    BucketNumber = 2,
}

impl BlockInfo {
    pub fn encode(&self, w: &mut impl ProtoWrite) -> Result<()> {
        w.write_varuint(BlockInfoField::Overflow as u64)?;
        w.write_u8(self.overflows as u8)?;
        w.write_varuint(BlockInfoField::BucketNumber as u64)?;
        w.write_i32(self.bucket_number)?;

        w.write_varuint(0)?; // end marker of block info
        Ok(())
    }

    pub fn decode(r: &mut impl ProtoRead) -> Result<BlockInfo> {
        let mut info = BlockInfo {
            overflows: false,
            bucket_number: -1,
        };

        loop {
            let field_id = r.read_varuint()?;
            match field_id {
                0 => break, // end marker
                1 => info.overflows = r.read_u8()? != 0,
                2 => info.bucket_number = r.read_i32()?,
                _ => {
                    return Err(Error::new(
                        ErrorKind::InvalidData,
                        format!("unknown BlockInfo field id {field_id}"),
                    ));
                }
            }
        }

        Ok(info)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::column::{Column, ColumnData};
    use std::io::Cursor;

    const PROTOCOL: u32 = 54460;

    // -- BlockInfo --

    #[test]
    fn test_block_info_roundtrip_defaults() {
        let info = BlockInfo {
            overflows: false,
            bucket_number: -1,
        };
        let mut buf = Vec::new();
        info.encode(&mut buf).unwrap();

        let mut cursor = Cursor::new(buf.as_slice());
        let decoded = BlockInfo::decode(&mut cursor).unwrap();
        assert_eq!(decoded, info);
    }

    #[test]
    fn test_block_info_roundtrip_overflow() {
        let info = BlockInfo {
            overflows: true,
            bucket_number: 42,
        };
        let mut buf = Vec::new();
        info.encode(&mut buf).unwrap();

        let mut cursor = Cursor::new(buf.as_slice());
        let decoded = BlockInfo::decode(&mut cursor).unwrap();
        assert_eq!(decoded, info);
    }

    #[test]
    fn test_block_info_wire_format() {
        // Verify exact wire bytes for empty/default BlockInfo
        let info = BlockInfo {
            overflows: false,
            bucket_number: -1,
        };
        let mut buf = Vec::new();
        info.encode(&mut buf).unwrap();

        // field_id=1, UInt8(0), field_id=2, Int32(-1 = 0xFFFFFFFF), field_id=0
        assert_eq!(buf, vec![0x01, 0x00, 0x02, 0xFF, 0xFF, 0xFF, 0xFF, 0x00]);
    }

    #[test]
    fn test_block_info_unknown_field_id() {
        // Manually encode an invalid field_id
        let mut buf = Vec::new();
        buf.write_varuint(99).unwrap();
        buf.write_u8(0).unwrap();

        let mut cursor = Cursor::new(buf.as_slice());
        let err = BlockInfo::decode(&mut cursor).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidData);
    }

    // -- Block --

    #[test]
    fn test_empty_block_roundtrip() {
        let mut block = Block::new();
        let mut buf = Vec::new();
        block.encode(&mut buf, PROTOCOL).unwrap();

        let mut cursor = Cursor::new(buf.as_slice());
        let decoded = Block::decode(&mut cursor, PROTOCOL).unwrap();

        assert_eq!(decoded.columns.len(), 0);
        assert_eq!(decoded.rows, 0);
        assert_eq!(decoded.info, block.info);
    }

    #[test]
    fn test_empty_block_wire_size() {
        // Per spec section 6.11: empty block is approximately 10 bytes
        let mut block = Block::new();
        let mut buf = Vec::new();
        block.encode(&mut buf, PROTOCOL).unwrap();

        // BlockInfo (8 bytes) + num_columns varuint (1) + num_rows varuint (1) = 10
        assert_eq!(buf.len(), 10);
    }

    #[test]
    fn test_block_with_columns_roundtrip() {
        let mut block = Block {
            info: Some(BlockInfo {
                overflows: false,
                bucket_number: -1,
            }),
            columns: vec![
                Column {
                    name: "id".to_string(),
                    data_type: "UInt8".to_string(),
                    serialization: crate::proto::column::Serialization::Default,
                    data: ColumnData::Uint8(vec![1, 2, 3]),
                },
                Column {
                    name: "name".to_string(),
                    data_type: "String".to_string(),
                    serialization: crate::proto::column::Serialization::Default,
                    data: ColumnData::String(vec![
                        "a".to_string(),
                        "b".to_string(),
                        "c".to_string(),
                    ]),
                },
            ],
            rows: 3,
        };
        let mut buf = Vec::new();
        block.encode(&mut buf, PROTOCOL).unwrap();

        let mut cursor = Cursor::new(buf.as_slice());
        let decoded = Block::decode(&mut cursor, PROTOCOL).unwrap();
        assert_eq!(decoded.columns.len(), 2);
        assert_eq!(decoded.rows, 3);
        assert_eq!(decoded.columns[0].name, "id");
        assert_eq!(decoded.columns[1].name, "name");
    }

    #[test]
    fn test_block_without_block_info_feature() {
        // Protocol below BLOCK_INFO (51903) — info not encoded
        let old_protocol: u32 = 51000;

        // Manually construct a block without BlockInfo
        let mut buf = Vec::new();
        buf.write_varuint(0).unwrap(); // num_columns
        buf.write_varuint(0).unwrap(); // num_rows

        let mut cursor = Cursor::new(buf.as_slice());
        let decoded = Block::decode(&mut cursor, old_protocol).unwrap();
        assert!(decoded.info.is_none());
        assert_eq!(decoded.columns.len(), 0);
        assert_eq!(decoded.rows, 0);
    }
}
