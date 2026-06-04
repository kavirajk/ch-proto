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

    // feature-gated: TOTAL_BYTES_IN_PROGRESS (v54463)
    // Sits on the wire BETWEEN total_rows and wrote_rows — matches the
    // canonical decode order in `Progress.cpp::ProgressValues::read`.
    pub total_bytes: Option<u64>,

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

        if Feature::TOTAL_BYTES_IN_PROGRESS.in_version(protocol) {
            w.write_varuint(self.total_bytes.unwrap_or(0))?;
        }

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

        let total_bytes = if Feature::TOTAL_BYTES_IN_PROGRESS.in_version(protocol) {
            Some(r.read_varuint()?)
        } else {
            None
        };

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
            total_bytes,
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
            total_bytes: None,
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
        assert_eq!(decoded.total_bytes, None);
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
            total_bytes: None,
            wrote_rows: None,
            wrote_bytes: None,
            elapsed_ns: None,
        };
        let mut buf = Vec::new();
        p.encode(&mut buf, old_protocol).unwrap();

        let mut cursor = Cursor::new(buf.as_slice());
        let decoded = Progress::decode(&mut cursor, old_protocol).unwrap();
        assert_eq!(decoded.rows, 10);
        assert_eq!(decoded.total_bytes, None);
        assert_eq!(decoded.wrote_rows, None);
        assert_eq!(decoded.elapsed_ns, None);
    }

    #[test]
    fn test_progress_roundtrip_with_total_bytes_v54463() {
        // At v54463+, `total_bytes` sits on the wire between `total_rows`
        // and `wrote_rows`. A roundtrip verifies the byte order matches the
        // canonical layout in `IO/Progress.cpp::read`.
        let protocol = Feature::TOTAL_BYTES_IN_PROGRESS.version();
        let p = Progress {
            rows: 100,
            bytes: 2048,
            total_rows: 1000,
            total_bytes: Some(40_960),
            wrote_rows: Some(50),
            wrote_bytes: Some(512),
            elapsed_ns: Some(1_000_000),
        };
        let mut buf = Vec::new();
        p.encode(&mut buf, protocol).unwrap();

        let mut cursor = Cursor::new(buf.as_slice());
        let decoded = Progress::decode(&mut cursor, protocol).unwrap();
        assert_eq!(decoded.total_bytes, Some(40_960));
        assert_eq!(decoded.wrote_rows, Some(50));
        assert_eq!(decoded.elapsed_ns, Some(1_000_000));
    }

    #[test]
    fn test_progress_absent_below_v54463() {
        // Stream-position check: at v54462 the gate is closed, decode must
        // leave the cursor exactly at end of buffer.
        let protocol = 54462;
        let p = Progress {
            rows: 1,
            bytes: 2,
            total_rows: 3,
            total_bytes: None,
            wrote_rows: Some(4),
            wrote_bytes: Some(5),
            elapsed_ns: Some(6),
        };
        let mut buf = Vec::new();
        p.encode(&mut buf, protocol).unwrap();

        let mut cursor = Cursor::new(buf.as_slice());
        let decoded = Progress::decode(&mut cursor, protocol).unwrap();
        assert_eq!(decoded.total_bytes, None);
        assert_eq!(cursor.position() as usize, buf.len());
    }
}
