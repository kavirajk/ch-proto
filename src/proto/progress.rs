use std::io::Result;

use super::{
    feature::Feature,
    wire::{ProtoRead, ProtoWrite},
};

// Progress is sent by the server periodically during query execution.
//
// Each packet carries an **increment** (delta) since the previous Progress,
// not a running total — the server sends
// `state.progress.fetchValuesAndResetPiecewiseAtomically()` (see
// `TCPHandler::sendProgress`). A client that wants running totals sums the
// deltas itself; see [`Progress::accumulate`].
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
    /// Fold a delta `Progress` packet into this running total by summing each
    /// counter. `Option` fields sum when either side is present (a missing
    /// field counts as 0), so accumulating a feature-gated field across a
    /// stream where it's always present yields a `Some` total.
    pub fn accumulate(&mut self, delta: &Progress) {
        self.rows += delta.rows;
        self.bytes += delta.bytes;
        self.total_rows += delta.total_rows;
        let add = |acc: &mut Option<u64>, d: Option<u64>| {
            if let Some(d) = d {
                *acc = Some(acc.unwrap_or(0) + d);
            }
        };
        add(&mut self.total_bytes, delta.total_bytes);
        add(&mut self.wrote_rows, delta.wrote_rows);
        add(&mut self.wrote_bytes, delta.wrote_bytes);
        add(&mut self.elapsed_ns, delta.elapsed_ns);
    }

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
    fn test_progress_accumulate_sums_deltas() {
        // Progress packets are deltas; accumulate sums each counter and
        // promotes Option fields to Some once any delta carries them.
        let mut total = Progress::default();
        total.accumulate(&Progress {
            rows: 1,
            bytes: 10,
            total_rows: 100,
            total_bytes: Some(0),
            wrote_rows: Some(2),
            wrote_bytes: Some(20),
            elapsed_ns: Some(5),
        });
        total.accumulate(&Progress {
            rows: 3,
            bytes: 30,
            total_rows: 0,
            total_bytes: Some(0),
            wrote_rows: Some(0),
            wrote_bytes: Some(40),
            elapsed_ns: Some(7),
        });
        assert_eq!(total.rows, 4);
        assert_eq!(total.bytes, 40);
        assert_eq!(total.total_rows, 100);
        assert_eq!(total.wrote_rows, Some(2));
        assert_eq!(total.wrote_bytes, Some(60));
        assert_eq!(total.elapsed_ns, Some(12));
    }

    #[test]
    fn test_progress_accumulate_keeps_none_when_absent() {
        // A delta with all-None Option fields leaves the running total's
        // Option fields None (nothing observed yet).
        let mut total = Progress::default();
        total.accumulate(&Progress {
            rows: 5,
            bytes: 50,
            total_rows: 0,
            total_bytes: None,
            wrote_rows: None,
            wrote_bytes: None,
            elapsed_ns: None,
        });
        assert_eq!(total.rows, 5);
        assert_eq!(total.wrote_rows, None);
        assert_eq!(total.elapsed_ns, None);
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
