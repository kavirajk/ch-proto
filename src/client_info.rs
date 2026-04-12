use std::io::{self, Error, ErrorKind, Result};

use crate::{
    feature::Feature,
    proto::{self, ProtoRead, ProtoWrite},
    query::Query,
};

static QUERY_INTERFACE_TCP: u8 = 1;

// ClientInfo is something part of Query packet that you sent to server
// query specific information.
// Fields are ordered to match wire encoding order.
pub struct ClientInfo {
    query_kind: QueryKind,

    initial_user: String,
    initial_query_id: String,
    initial_address: String,
    initial_time: Option<i64>, // feature-gated: INITIAL_QUERY_START_TIME

    // we will always use query_interface value 1 (TCP)
    // there are other enums. but we don't bother
    query_interface: u8,

    os_user: String,
    client_hostname: String,
    client_name: String,
    version_major: u64,
    version_minor: u64,
    protocol_version: u64,

    quota_key: Option<String>, // feature-gated: QUOTA_KEY_IN_CLIENT_INFO
    distributed_depth: Option<i32>, // feature-gated: DISTRIBUTED_DEPTH
    version_patch: Option<u64>, // feature-gated: VERSION_PATCH (TCP only)

    // Skip tracing for now        // feature-gated: OPEN_TELEMETRY
    // span: SpanContext,
    collaborate_with_initiator: Option<bool>, // feature-gated: PARALLEL_REPLICAS
    obsolete_count_participating_replicas: Option<u64>, // feature-gated: PARALLEL_REPLICAS
    count_current_replicas: Option<u64>,      // feature-gated: PARALLEL_REPLICAS
}

#[derive(Copy, Clone, PartialEq, PartialOrd)]
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
            Some(r.read_varuint()? as i64)
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
            w.write_varuint(self.initial_time.ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("initial_time is required for this protocol ({protocol})"),
                )
            })? as u64)?;
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

        // skip FEATURE::QUERY_AND_LINE_NUMBER and Feature::JWT

        Ok(())
    }
}
