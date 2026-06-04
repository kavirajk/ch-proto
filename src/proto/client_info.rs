use std::io::{self, Error, ErrorKind, Result};

use super::{
    feature::Feature,
    wire::{ProtoRead, ProtoWrite},
};

static QUERY_INTERFACE_TCP: u8 = 1;

// ClientInfo is something part of Query packet that you sent to server
// query specific information.
// Fields are ordered to match wire encoding order.
pub struct ClientInfo {
    pub query_kind: QueryKind,

    pub initial_user: String,
    pub initial_query_id: String,
    pub initial_address: String,
    pub initial_time: Option<i64>, // feature-gated: INITIAL_QUERY_START_TIME

    // we will always use query_interface value 1 (TCP)
    // there are other enums. but we don't bother
    pub query_interface: u8,

    pub os_user: String,
    pub client_hostname: String,
    pub client_name: String,
    pub version_major: u64,
    pub version_minor: u64,
    pub protocol_version: u64,

    pub quota_key: Option<String>, // feature-gated: QUOTA_KEY_IN_CLIENT_INFO
    pub distributed_depth: Option<i32>, // feature-gated: DISTRIBUTED_DEPTH
    pub version_patch: Option<u64>, // feature-gated: VERSION_PATCH (TCP only)

    // Skip tracing for now        // feature-gated: OPEN_TELEMETRY
    // span: SpanContext,
    pub collaborate_with_initiator: Option<bool>, // feature-gated: PARALLEL_REPLICAS
    pub obsolete_count_participating_replicas: Option<u64>, // feature-gated: PARALLEL_REPLICAS
    pub count_current_replicas: Option<u64>,      // feature-gated: PARALLEL_REPLICAS

    /// 1-indexed statement position in a multi-statement script. Appears
    /// after the parallel-replicas block at v54475+. External clients that
    /// run single-statement queries send `0`.
    pub script_query_number: Option<u64>,
    /// 1-indexed line number of the statement within the source script.
    /// Also new in v54475. External clients send `0`.
    pub script_line_number: Option<u64>,
}

#[derive(Debug, Copy, Clone, PartialEq, PartialOrd)]
pub enum QueryKind {
    /// Uninitialized object.
    NoQuery = 0,
    InitialQuery = 1,
    /// Query that was initiated by another query for distributed or ON CLUSTER query execution.
    SecondaryQuery = 2,
}

impl TryFrom<u8> for QueryKind {
    type Error = io::Error;
    fn try_from(value: u8) -> std::result::Result<Self, Self::Error> {
        match value {
            0 => Ok(QueryKind::NoQuery),
            1 => Ok(QueryKind::InitialQuery),
            2 => Ok(QueryKind::SecondaryQuery),
            _ => Err(Error::new(
                io::ErrorKind::InvalidData,
                format!("received invalid QueryKind {value}"),
            )),
        }
    }
}

impl ClientInfo {
    pub fn decode(r: &mut impl ProtoRead, protocol: u32) -> Result<ClientInfo> {
        let query_kind = QueryKind::try_from(r.read_u8()?)?;
        let initial_user = r.read_string()?;
        let initial_query_id = r.read_string()?;
        let initial_address = r.read_string()?;
        let initial_time = if Feature::INITIAL_QUERY_START_TIME.in_version(protocol) {
            Some(r.read_i64()?)
        } else {
            None
        };

        let query_interface = r.read_u8()?;

        // These fields are only present for TCP interface
        let (os_user, client_hostname, client_name, version_major, version_minor, protocol_version) =
            if query_interface == QUERY_INTERFACE_TCP {
                (
                    r.read_string()?,
                    r.read_string()?,
                    r.read_string()?,
                    r.read_varuint()?,
                    r.read_varuint()?,
                    r.read_varuint()?,
                )
            } else {
                return Err(Error::new(ErrorKind::InvalidInput, "client supports only QUERY_INTERFACE_TCP but received invalid interface ({query_interface})"));
                // (String::new(), String::new(), String::new(), 0, 0, 0);
            };

        let quota_key = if Feature::QUOTA_KEY_IN_CLIENT_INFO.in_version(protocol) {
            Some(r.read_string()?)
        } else {
            None
        };
        let distributed_depth = if Feature::DISTRIBUTED_DEPTH.in_version(protocol) {
            Some(r.read_varuint()? as i32)
        } else {
            None
        };
        if query_interface != QUERY_INTERFACE_TCP {
            return Err(Error::new(ErrorKind::InvalidInput, "client supports only QUERY_INTERFACE_TCP but received invalid interface ({query_interface})"));
        }
        let version_patch = if Feature::VERSION_PATCH.in_version(protocol) {
            Some(r.read_varuint()?)
        } else {
            None
        };

        if Feature::OPEN_TELEMETRY.in_version(protocol) {
            // Skip: read the has_trace flag, if set skip trace data
            let has_trace = r.read_u8()?;
            if has_trace != 0 {
                // TraceID (16 bytes) + SpanID (8 bytes)
                let mut skip_buf = [0u8; 24];
                r.read_exact(&mut skip_buf)?;
                let _trace_state = r.read_string()?;
                let _trace_flags = r.read_u8()?;
            }
        }

        let (
            collaborate_with_initiator,
            obsolete_count_participating_replicas,
            count_current_replicas,
        ) = if Feature::PARALLEL_REPLICAS.in_version(protocol) {
            (
                Some(r.read_varuint()? != 0),
                Some(r.read_varuint()?),
                Some(r.read_varuint()?),
            )
        } else {
            (None, None, None)
        };

        let (script_query_number, script_line_number) =
            if Feature::QUERY_AND_LINE_NUMBERS.in_version(protocol) {
                (Some(r.read_varuint()?), Some(r.read_varuint()?))
            } else {
                (None, None)
            };

        Ok(ClientInfo {
            query_kind,
            initial_user,
            initial_query_id,
            initial_address,
            initial_time,
            query_interface,
            os_user,
            client_hostname,
            client_name,
            version_major,
            version_minor,
            protocol_version,
            quota_key,
            distributed_depth,
            version_patch,
            collaborate_with_initiator,
            obsolete_count_participating_replicas,
            count_current_replicas,
            script_query_number,
            script_line_number,
        })
    }
    pub fn encode(&self, w: &mut impl ProtoWrite, protocol: u32) -> Result<()> {
        w.write_u8(self.query_kind as u8)?;
        if self.query_kind == QueryKind::NoQuery {
            return Ok(());
        }
        w.write_string(&self.initial_user)?;
        w.write_string(&self.initial_query_id)?;
        w.write_string(&self.initial_address)?;
        if Feature::INITIAL_QUERY_START_TIME.in_version(protocol) {
            w.write_i64(self.initial_time.ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("initial_time is required for this protocol ({protocol})"),
                )
            })?)?;
        }

        w.write_u8(self.query_interface)?;

        if self.query_interface == QUERY_INTERFACE_TCP {
            w.write_string(&self.os_user)?;
            w.write_string(&self.client_hostname)?;
            w.write_string(&self.client_name)?;
            w.write_varuint(self.version_major)?;
            w.write_varuint(self.version_minor)?;
            w.write_varuint(self.protocol_version)?;
        } else {
            return Err(Error::new(ErrorKind::InvalidInput, "client supports only QUERY_INTERFACE_TCP but received invalid interface ({self.query_interface})"));
        }

        if Feature::QUOTA_KEY_IN_CLIENT_INFO.in_version(protocol) {
            w.write_string(&self.quota_key.clone().ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("quota_key is required for this protocol ({protocol})"),
                )
            })?)?;
        }

        if Feature::DISTRIBUTED_DEPTH.in_version(self.protocol_version as u32) {
            w.write_varuint(self.distributed_depth.ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("distributed_depth is required for this protocol ({protocol})"),
                )
            })? as u64)?;
        }
        if self.query_interface != QUERY_INTERFACE_TCP {
            return Err(Error::new(ErrorKind::InvalidInput, "client supports only QUERY_INTERFACE_TCP but received invalid interface ({self.query_interface})"));
        }

        if Feature::VERSION_PATCH.in_version(self.protocol_version as u32) {
            w.write_varuint(self.version_patch.ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("version_patch is required for this protocol ({protocol})"),
                )
            })?)?;
        }

        if Feature::OPEN_TELEMETRY.in_version(self.protocol_version as u32) {
            // Currently just skip it
            w.write_u8(0)?;
        }

        if Feature::PARALLEL_REPLICAS.in_version(self.protocol_version as u32) {
            w.write_varuint(self.collaborate_with_initiator.ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "collaborate_with_initiator is required for this protocol ({protocol})"
                    ),
                )
            })? as u64)?;
            w.write_varuint(self.obsolete_count_participating_replicas.ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "obsolete_count_participating_replicas is required for this protocol ({protocol})"
                    ),
                )
            })?)?;
            w.write_varuint(self.count_current_replicas.ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("count_current_replicas is required for this protocol ({protocol})"),
                )
            })?)?;
        }

        if Feature::QUERY_AND_LINE_NUMBERS.in_version(self.protocol_version as u32) {
            w.write_varuint(self.script_query_number.unwrap_or(0))?;
            w.write_varuint(self.script_line_number.unwrap_or(0))?;
        }

        // FUTURE: Feature::JWT_IN_INTERSERVER (v54476) — interserver only.

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    const PROTOCOL_ALL_FEATURES: u32 = 54460;
    const PROTOCOL_MINIMAL: u32 = 54058;

    fn make_client_info_full() -> ClientInfo {
        ClientInfo {
            query_kind: QueryKind::InitialQuery,
            initial_user: "default".to_string(),
            initial_query_id: "query-123".to_string(),
            initial_address: "127.0.0.1:9000".to_string(),
            initial_time: Some(1234567890),
            query_interface: QUERY_INTERFACE_TCP,
            os_user: "kavi".to_string(),
            client_hostname: "localhost".to_string(),
            client_name: "toy-client".to_string(),
            version_major: 1,
            version_minor: 0,
            protocol_version: PROTOCOL_ALL_FEATURES as u64,
            quota_key: Some("".to_string()),
            distributed_depth: Some(0),
            version_patch: Some(3),
            collaborate_with_initiator: Some(false),
            obsolete_count_participating_replicas: Some(0),
            count_current_replicas: Some(0),
            script_query_number: None,
            script_line_number: None,
        }
    }

    fn make_client_info_minimal() -> ClientInfo {
        ClientInfo {
            query_kind: QueryKind::InitialQuery,
            initial_user: "default".to_string(),
            initial_query_id: "q-1".to_string(),
            initial_address: "".to_string(),
            initial_time: None,
            query_interface: QUERY_INTERFACE_TCP,
            os_user: "user".to_string(),
            client_hostname: "host".to_string(),
            client_name: "client".to_string(),
            version_major: 1,
            version_minor: 0,
            protocol_version: PROTOCOL_MINIMAL as u64,
            quota_key: None,
            distributed_depth: None,
            version_patch: None,
            collaborate_with_initiator: None,
            obsolete_count_participating_replicas: None,
            count_current_replicas: None,
            script_query_number: None,
            script_line_number: None,
        }
    }

    #[test]
    fn test_roundtrip_all_features() {
        let ci = make_client_info_full();
        let mut buf = Vec::new();
        ci.encode(&mut buf, PROTOCOL_ALL_FEATURES).unwrap();

        let mut cursor = Cursor::new(buf.as_slice());
        let decoded = ClientInfo::decode(&mut cursor, PROTOCOL_ALL_FEATURES).unwrap();

        assert_eq!(decoded.query_kind, QueryKind::InitialQuery);
        assert_eq!(decoded.initial_user, "default");
        assert_eq!(decoded.initial_query_id, "query-123");
        assert_eq!(decoded.initial_address, "127.0.0.1:9000");
        assert_eq!(decoded.initial_time, Some(1234567890));
        assert_eq!(decoded.query_interface, QUERY_INTERFACE_TCP);
        assert_eq!(decoded.os_user, "kavi");
        assert_eq!(decoded.client_hostname, "localhost");
        assert_eq!(decoded.client_name, "toy-client");
        assert_eq!(decoded.version_major, 1);
        assert_eq!(decoded.version_minor, 0);
        assert_eq!(decoded.quota_key, Some("".to_string()));
        assert_eq!(decoded.distributed_depth, Some(0));
        assert_eq!(decoded.version_patch, Some(3));
        assert_eq!(decoded.collaborate_with_initiator, Some(false));
        assert_eq!(decoded.obsolete_count_participating_replicas, Some(0));
        assert_eq!(decoded.count_current_replicas, Some(0));
    }

    #[test]
    fn test_roundtrip_minimal_protocol() {
        let ci = make_client_info_minimal();
        let mut buf = Vec::new();
        ci.encode(&mut buf, PROTOCOL_MINIMAL).unwrap();

        let mut cursor = Cursor::new(buf.as_slice());
        let decoded = ClientInfo::decode(&mut cursor, PROTOCOL_MINIMAL).unwrap();

        assert_eq!(decoded.query_kind, QueryKind::InitialQuery);
        assert_eq!(decoded.initial_user, "default");
        assert_eq!(decoded.initial_time, None);
        assert_eq!(decoded.quota_key, None);
        assert_eq!(decoded.distributed_depth, None);
        assert_eq!(decoded.version_patch, None);
        assert_eq!(decoded.collaborate_with_initiator, None);
    }

    #[test]
    fn test_wire_size_varies_with_protocol() {
        let ci_full = make_client_info_full();
        let ci_min = make_client_info_minimal();

        let mut buf_full = Vec::new();
        ci_full
            .encode(&mut buf_full, PROTOCOL_ALL_FEATURES)
            .unwrap();

        let mut buf_min = Vec::new();
        ci_min.encode(&mut buf_min, PROTOCOL_MINIMAL).unwrap();

        assert!(buf_full.len() > buf_min.len());
    }

    #[test]
    fn test_no_query_encodes_only_kind() {
        let ci = ClientInfo {
            query_kind: QueryKind::NoQuery,
            initial_user: "".to_string(),
            initial_query_id: "".to_string(),
            initial_address: "".to_string(),
            initial_time: None,
            query_interface: QUERY_INTERFACE_TCP,
            os_user: "".to_string(),
            client_hostname: "".to_string(),
            client_name: "".to_string(),
            version_major: 0,
            version_minor: 0,
            protocol_version: 0,
            quota_key: None,
            distributed_depth: None,
            version_patch: None,
            collaborate_with_initiator: None,
            obsolete_count_participating_replicas: None,
            count_current_replicas: None,
            script_query_number: None,
            script_line_number: None,
        };
        let mut buf = Vec::new();
        ci.encode(&mut buf, PROTOCOL_ALL_FEATURES).unwrap();
        // NoQuery should write only the query_kind byte
        assert_eq!(buf.len(), 1);
        assert_eq!(buf[0], 0);
    }

    #[test]
    fn test_query_kind_roundtrip() {
        for (val, expected) in [
            (0u8, QueryKind::NoQuery),
            (1, QueryKind::InitialQuery),
            (2, QueryKind::SecondaryQuery),
        ] {
            let qk = QueryKind::try_from(val).unwrap();
            assert_eq!(qk, expected);
        }
    }

    #[test]
    fn test_query_kind_invalid() {
        let result = QueryKind::try_from(99u8);
        assert!(result.is_err());
    }

    #[test]
    fn test_open_telemetry_skip_no_trace() {
        // Encode with OTEL feature, no trace (just 0 byte)
        let ci = make_client_info_full();
        let mut buf = Vec::new();
        ci.encode(&mut buf, PROTOCOL_ALL_FEATURES).unwrap();

        let mut cursor = Cursor::new(buf.as_slice());
        let decoded = ClientInfo::decode(&mut cursor, PROTOCOL_ALL_FEATURES).unwrap();
        // Should decode successfully — OTEL section consumed correctly
        assert_eq!(decoded.initial_user, "default");
    }

    #[test]
    fn test_unicode_fields() {
        let mut ci = make_client_info_full();
        ci.initial_user = "пользователь".to_string();
        ci.client_name = "клиент".to_string();
        ci.os_user = "用户".to_string();

        let mut buf = Vec::new();
        ci.encode(&mut buf, PROTOCOL_ALL_FEATURES).unwrap();

        let mut cursor = Cursor::new(buf.as_slice());
        let decoded = ClientInfo::decode(&mut cursor, PROTOCOL_ALL_FEATURES).unwrap();

        assert_eq!(decoded.initial_user, "пользователь");
        assert_eq!(decoded.client_name, "клиент");
        assert_eq!(decoded.os_user, "用户");
    }
}
