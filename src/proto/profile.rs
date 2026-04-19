use std::io::Result;

use super::{
    feature::Feature,
    wire::{ProtoRead, ProtoWrite},
};

// ProfileInfo is post-execution statistics sent once per query.
// Particularly useful for reporting row counts with LIMIT clauses.
#[derive(Debug, Default, Clone)]
pub struct ProfileInfo {
    pub rows: u64,
    pub blocks: u64,
    pub bytes: u64,
    pub applied_limit: bool,
    pub rows_before_limit: u64,
    pub calculated_rows_before_limit: bool,

    // feature-gated: ROWS_BEFORE_AGGREGATION (v54469)
    pub applied_aggregation: Option<bool>,
    pub rows_before_aggregation: Option<u64>,
}

impl ProfileInfo {
    pub fn encode(&self, w: &mut impl ProtoWrite, protocol: u32) -> Result<()> {
        w.write_varuint(self.rows)?;
        w.write_varuint(self.blocks)?;
        w.write_varuint(self.bytes)?;
        w.write_bool(self.applied_limit)?;
        w.write_varuint(self.rows_before_limit)?;
        w.write_bool(self.calculated_rows_before_limit)?;

        if Feature::ROWS_BEFORE_AGGREGATION.in_version(protocol) {
            w.write_bool(self.applied_aggregation.unwrap_or(false))?;
            w.write_varuint(self.rows_before_aggregation.unwrap_or(0))?;
        }
        Ok(())
    }

    pub fn decode(r: &mut impl ProtoRead, protocol: u32) -> Result<ProfileInfo> {
        let rows = r.read_varuint()?;
        let blocks = r.read_varuint()?;
        let bytes = r.read_varuint()?;
        let applied_limit = r.read_bool()?;
        let rows_before_limit = r.read_varuint()?;
        let calculated_rows_before_limit = r.read_bool()?;

        let (applied_aggregation, rows_before_aggregation) =
            if Feature::ROWS_BEFORE_AGGREGATION.in_version(protocol) {
                (Some(r.read_bool()?), Some(r.read_varuint()?))
            } else {
                (None, None)
            };

        Ok(ProfileInfo {
            rows,
            blocks,
            bytes,
            applied_limit,
            rows_before_limit,
            calculated_rows_before_limit,
            applied_aggregation,
            rows_before_aggregation,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    const PROTOCOL_WITH_AGG: u32 = 54469;
    const PROTOCOL_WITHOUT_AGG: u32 = 54460;

    #[test]
    fn test_profile_info_roundtrip_with_aggregation() {
        let p = ProfileInfo {
            rows: 100,
            blocks: 2,
            bytes: 2048,
            applied_limit: true,
            rows_before_limit: 500,
            calculated_rows_before_limit: true,
            applied_aggregation: Some(true),
            rows_before_aggregation: Some(250),
        };
        let mut buf = Vec::new();
        p.encode(&mut buf, PROTOCOL_WITH_AGG).unwrap();

        let mut cursor = Cursor::new(buf.as_slice());
        let decoded = ProfileInfo::decode(&mut cursor, PROTOCOL_WITH_AGG).unwrap();
        assert_eq!(decoded.rows, 100);
        assert_eq!(decoded.blocks, 2);
        assert_eq!(decoded.applied_limit, true);
        assert_eq!(decoded.rows_before_limit, 500);
        assert_eq!(decoded.applied_aggregation, Some(true));
        assert_eq!(decoded.rows_before_aggregation, Some(250));
    }

    #[test]
    fn test_profile_info_roundtrip_without_aggregation() {
        let p = ProfileInfo {
            rows: 10,
            blocks: 1,
            bytes: 64,
            applied_limit: false,
            rows_before_limit: 0,
            calculated_rows_before_limit: false,
            applied_aggregation: None,
            rows_before_aggregation: None,
        };
        let mut buf = Vec::new();
        p.encode(&mut buf, PROTOCOL_WITHOUT_AGG).unwrap();

        let mut cursor = Cursor::new(buf.as_slice());
        let decoded = ProfileInfo::decode(&mut cursor, PROTOCOL_WITHOUT_AGG).unwrap();
        assert_eq!(decoded.applied_aggregation, None);
        assert_eq!(decoded.rows_before_aggregation, None);
    }
}
