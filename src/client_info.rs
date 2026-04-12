use std::io::{self, Error, Result};

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
    version_patch: u64,        // feature-gated: VERSION_PATCH (TCP only)
    // Skip tracing for now        // feature-gated: OPEN_TELEMETRY
    // span: SpanContext,
    collaborate_with_initiator: bool, // feature-gated: PARALLEL_REPLICAS
    obsolete_count_participating_replicas: u64, // feature-gated: PARALLEL_REPLICAS
    count_current_replicas: u64,      // feature-gated: PARALLEL_REPLICAS
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
        Ok(ClientInfo {
            query_kind: QueryKind::try_from(r.read_u8()?)?,
            initial_user: r.read_string()?,
            initial_query_id: r.read_string()?,
            initial_address: r.read_string()?,
            initial_time: if Feature::INITIAL_QUERY_START_TIME.in_version(protocol) {
                Some(r.read_varuint()? as i64)
            } else {
                None
            },
            query_interface: r.read_u8()?,
            os_user: (),
            client_hostname: (),
            client_name: (),
            version_major: (),
            version_minor: (),
            protocol_version: (),
            quota_key: if Feature::QUOTA_KEY_IN_CLIENT_INFO.in_version(protocol) {
                Some(r.read_string()?)
            } else {
                None
            },
            distributed_depth: if Feature::DISTRIBUTED_DEPTH.in_version(protocol) {
                Some(r.read_varuint()? as i32)
            } else {
                None
            },
            version_patch: (),
            collaborate_with_initiator: (),
            obsolete_count_participating_replicas: (),
            count_current_replicas: (),
        })
    }
    pub fn encode(&mut self, w: &mut impl ProtoWrite, protocol: u32) -> Result<()> {
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

        if self.query_interface == QUERY_INTERFACE_TCP
            && Feature::VERSION_PATCH.in_version(self.protocol_version as u32)
        {
            w.write_varuint(self.version_patch)?;
        }

        if Feature::OPEN_TELEMETRY.in_version(self.protocol_version as u32) {
            // Currently just skip it
            w.write_u8(0)?;
        }

        if Feature::PARALLEL_REPLICAS.in_version(self.protocol_version as u32) {
            w.write_varuint(self.collaborate_with_initiator as u64)?;
            w.write_varuint(self.obsolete_count_participating_replicas)?;
            w.write_varuint(self.count_current_replicas)?;
        }

        // skip FEATURE::QUERY_AND_LINE_NUMBER and Feature::JWT

        Ok(())
    }
}
