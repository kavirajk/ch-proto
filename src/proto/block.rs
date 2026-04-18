use std::io::{Error, ErrorKind, Result};

use super::{column::Column, feature::Feature, wire::ProtoWrite};

// Block is the unit of data processing in ClickHouse.
// It is group of some number of columns.
// Think of it like
// 1. Column -> Whole Vec<T> of single column values
// 2. Block -> Horizontal split of j
// It has some metadata information
pub struct Block {
    info: Option<BlockInfo>,
    pub columns: Vec<Column>,
    pub rows: usize,
}

impl Block {
    pub fn encode(&mut self, w: &mut impl ProtoWrite, protocol: u32) -> Result<()> {
        if Feature::BLOCK_INFO.in_version(protocol) {
            self.info
                .clone()
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
        Ok(())
    }
    pub fn new() -> Self {
        Block {
            info: Some(BlockInfo {
                overflows: false,
                bucket_number: 0,
            }),
            columns: vec![],
            rows: 0,
        }
    }
}

// BlockInfo is a metadata about the block
#[derive(Copy, Clone)]
pub struct BlockInfo {
    overflows: bool,
    bucket_number: i32,
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
}
