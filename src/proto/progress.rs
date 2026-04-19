use std::io::Result;

use super::{
    feature::Feature,
    wire::{ProtoRead, ProtoWrite},
};

// Progress is sent by the server periodically during query execution.
// Cumulative totals (not deltas) across multiple Progress packets.
#[derive(Debug, Default, Clone)]
pub struct Progress {
    pub rows: u64,
    pub bytes: u64,
    pub total_rows: u64,

    // feature-gated: WRITE_CLIENT_INFO (v54420) — INSERT-specific
    pub wrote_rows: Option<u64>,
    pub wrote_bytes: Option<u64>,

    // feature-gated: SERVER_QUERY_TIME_IN_PROGRESS (v54460)
    pub elapsed_ns: Option<u64>,
}

impl Progress {
    pub fn encode(&self, w: &mut impl ProtoWrite, protocol: u32) -> Result<()> {
        w.write_varuint(self.rows)?;
        w.write_varuint(self.bytes)?;
        w.write_varuint(self.total_rows)?;

        if Feature::WRITE_CLIENT_INFO.in_version(protocol) {
            w.write_varuint(self.wrote_rows.unwrap_or(0))?;
            w.write_varuint(self.wrote_bytes.unwrap_or(0))?;
        }

        if Feature::SERVER_QUERY_TIME_IN_PROGRESS.in_version(protocol) {
            w.write_varuint(self.elapsed_ns.unwrap_or(0))?;
        }

        Ok(())
    }

    pub fn decode(r: &mut impl ProtoRead, protocol: u32) -> Result<Progress> {
        let rows = r.read_varuint()?;
        let bytes = r.read_varuint()?;
        let total_rows = r.read_varuint()?;

        let (wrote_rows, wrote_bytes) = if Feature::WRITE_CLIENT_INFO.in_version(protocol) {
            (Some(r.read_varuint()?), Some(r.read_varuint()?))
        } else {
            (None, None)
        };

        let elapsed_ns = if Feature::SERVER_QUERY_TIME_IN_PROGRESS.in_version(protocol) {
            Some(r.read_varuint()?)
        } else {
            None
        };

        Ok(Progress {
            rows,
            bytes,
            total_rows,
            wrote_rows,
            wrote_bytes,
            elapsed_ns,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    const PROTOCOL: u32 = 54460;

    #[test]
    fn test_progress_roundtrip_all_features() {
        let p = Progress {
            rows: 100,
            bytes: 2048,
            total_rows: 1000,
            wrote_rows: Some(50),
            wrote_bytes: Some(512),
            elapsed_ns: Some(1_000_000),
        };
        let mut buf = Vec::new();
        p.encode(&mut buf, PROTOCOL).unwrap();

        let mut cursor = Cursor::new(buf.as_slice());
        let decoded = Progress::decode(&mut cursor, PROTOCOL).unwrap();
        assert_eq!(decoded.rows, 100);
        assert_eq!(decoded.bytes, 2048);
        assert_eq!(decoded.total_rows, 1000);
        assert_eq!(decoded.wrote_rows, Some(50));
        assert_eq!(decoded.wrote_bytes, Some(512));
        assert_eq!(decoded.elapsed_ns, Some(1_000_000));
    }

    #[test]
    fn test_progress_roundtrip_minimal() {
        let old_protocol: u32 = 54000; // before WRITE_CLIENT_INFO
        let p = Progress {
            rows: 10,
            bytes: 100,
            total_rows: 20,
            wrote_rows: None,
            wrote_bytes: None,
            elapsed_ns: None,
        };
        let mut buf = Vec::new();
        p.encode(&mut buf, old_protocol).unwrap();

        let mut cursor = Cursor::new(buf.as_slice());
        let decoded = Progress::decode(&mut cursor, old_protocol).unwrap();
        assert_eq!(decoded.rows, 10);
        assert_eq!(decoded.wrote_rows, None);
        assert_eq!(decoded.elapsed_ns, None);
    }
}
