use std::io::Result;

use crate::proto::packet::ClientPacket;

use super::{
    block::Block,
    wire::{ProtoRead, ProtoWrite},
};

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
    pub fn encode(&self, w: &mut impl ProtoWrite, protocol: u32) -> Result<()> {
        w.write_varuint(ClientPacket::Data as u64)?;
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

    pub fn decode(r: &mut impl ProtoRead, protocol: u32) -> Result<ExternalTable> {
        let table_name = r.read_string()?;
        let block = Block::decode(r, protocol)?;
        Ok(ExternalTable { table_name, block })
    }

    pub fn is_end_marker(&self) -> bool {
        self.table_name.is_empty() && self.block.rows == 0 && self.block.columns.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    const PROTOCOL: u32 = 54460;

    #[test]
    fn test_empty_external_table_roundtrip() {
        let mut buf = Vec::new();
        ExternalTable::encode_empty(&mut buf, PROTOCOL).unwrap();

        // Skip the packet type byte that encode writes
        let mut cursor = Cursor::new(&buf[1..]);
        let decoded = ExternalTable::decode(&mut cursor, PROTOCOL).unwrap();
        assert_eq!(decoded.table_name, "");
        assert!(decoded.is_end_marker());
    }

    #[test]
    fn test_empty_external_table_wire_size() {
        // packet_type (1) + empty table_name (1) + empty block (10) = 12 bytes total
        let mut buf = Vec::new();
        ExternalTable::encode_empty(&mut buf, PROTOCOL).unwrap();
        assert_eq!(buf.len(), 12);
    }

    #[test]
    fn test_external_table_with_name() {
        let et = ExternalTable {
            table_name: "ext_table".to_string(),
            block: Block::new(),
        };
        let mut buf = Vec::new();
        et.encode(&mut buf, PROTOCOL).unwrap();

        let mut cursor = Cursor::new(&buf[1..]);
        let decoded = ExternalTable::decode(&mut cursor, PROTOCOL).unwrap();
        assert_eq!(decoded.table_name, "ext_table");
        assert!(!decoded.is_end_marker()); // has a name, not an end marker
    }

    #[test]
    fn test_is_end_marker_false_when_named() {
        let et = ExternalTable {
            table_name: "x".to_string(),
            block: Block::new(),
        };
        assert!(!et.is_end_marker());
    }

    #[test]
    fn test_is_end_marker_true_for_empty() {
        let et = ExternalTable {
            table_name: "".to_string(),
            block: Block::new(),
        };
        assert!(et.is_end_marker());
    }
}
