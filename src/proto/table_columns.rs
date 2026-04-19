use std::io::Result;

use super::wire::{ProtoRead, ProtoWrite};

// TableColumns describes column defaults for INSERT queries.
// Payload is a textual description of the column defaults, not a structured format.
#[derive(Debug, Default, Clone)]
pub struct TableColumns {
    pub external_table: String,
    pub columns_description: String,
}

impl TableColumns {
    pub fn encode(&self, w: &mut impl ProtoWrite) -> Result<()> {
        w.write_string(&self.external_table)?;
        w.write_string(&self.columns_description)?;
        Ok(())
    }

    pub fn decode(r: &mut impl ProtoRead) -> Result<TableColumns> {
        let external_table = r.read_string()?;
        let columns_description = r.read_string()?;
        Ok(TableColumns {
            external_table,
            columns_description,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn test_table_columns_roundtrip() {
        let tc = TableColumns {
            external_table: "".to_string(),
            columns_description: "id Int32, name String DEFAULT ''".to_string(),
        };
        let mut buf = Vec::new();
        tc.encode(&mut buf).unwrap();

        let mut cursor = Cursor::new(buf.as_slice());
        let decoded = TableColumns::decode(&mut cursor).unwrap();
        assert_eq!(decoded.external_table, "");
        assert_eq!(decoded.columns_description, "id Int32, name String DEFAULT ''");
    }

    #[test]
    fn test_table_columns_empty() {
        let tc = TableColumns::default();
        let mut buf = Vec::new();
        tc.encode(&mut buf).unwrap();

        let mut cursor = Cursor::new(buf.as_slice());
        let decoded = TableColumns::decode(&mut cursor).unwrap();
        assert_eq!(decoded.external_table, "");
        assert_eq!(decoded.columns_description, "");
    }
}
