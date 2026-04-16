use std::io::Result;

use super::{block::Block, wire::ProtoWrite};

// ExternalTable is a additional data that is sent from Client -> Server only for read-only queries
// (SELECT)
// It doesn't make sense for Server -> Client nor Client -> Server for INSERT queries
pub struct ExternalTable {
    table_name: String,
    // only one block (multiple columns are allowed per table?)
    // if you want more blocks, make it separate external table?
    block: Block,
}

impl ExternalTable {
    pub fn encode(&mut self, w: &mut impl ProtoWrite, protocol: u32) -> Result<()> {
        w.write_string(&self.table_name)?;
        self.block.encode(w, protocol)?;
        Ok(())
    }

    pub fn encode_empty(w: &mut impl ProtoWrite, protocol: u32) -> Result<()> {
        ExternalTable {
            table_name: "".to_string(),
            block: Block::new(),
        }
        .encode(w, protocol)?;
        Ok(())
    }
}
