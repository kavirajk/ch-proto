use std::io::Result;

use crate::{client_info::ClientInfo, feature::Feature, packet::ClientPacket, proto::ProtoWrite};

// Fields are ordered to match wire encoding order.
pub struct Query {
    query_id: String,
    client_info: ClientInfo, // feature-gated: WRITE_CLIENT_INFO
    settings: Vec<Setting>,  // feature-gated: SETTINGS_SERIALIZED_AS_STRINGS
    cluster_secret: String,  // feature-gated: INTERSERVER_SECRET
    stage: Stage,
    compression: bool,
    body: String,
    params: Vec<Param>, // feature-gated: PARAMETERS

    protocol_version: u64,
}

impl Query {
    fn encode(&mut self, w: &mut impl ProtoWrite) -> Result<()> {
        w.write_varuint(ClientPacket::Query as u64)?;
        w.write_string(&self.query_id)?;
        if Feature::WRITE_CLIENT_INFO.in_version(self.protocol_version as u32) {
            self.client_info.encode(w, self.protocol_version as u32)?;
        }

        if Feature::SETTINGS_SERIALIZED_AS_STRINGS.in_version(self.protocol_version as u32) {
            for s in &mut self.settings {
                s.encode(w)?;
            }
        }
        w.write_string("")?; // empty string denotes end of settings

        if Feature::INTERSERVER_SECRET.in_version(self.protocol_version as u32) {
            w.write_string(&self.cluster_secret)?;
        }

        w.write_varuint(self.stage as u64)?;
        w.write_varuint(self.compression as u64)?;
        w.write_string(&self.body)?;

        if Feature::PARAMETERS.in_version(self.protocol_version as u32) {
            for p in &mut self.params {
                p.encode(w)?;
            }
            w.write_string("")?; // empty string marks end of params
        }

        Ok(())
    }
}

pub enum SettingsFlag {
    Important = 0x01,
    Custom = 0x02,
    // This obsolete flag is no longer present on server side. I don't know why.
    Obsolete = 0x04,
}

pub struct Setting {
    key: String,
    value: String,
    important: bool,
    custom: bool,
    obsolete: bool,
}

impl Setting {
    fn encode(&mut self, w: &mut impl ProtoWrite) -> Result<()> {
        w.write_string(&self.key)?;
        let mut flags: u64 = 0;
        if self.important {
            flags |= SettingsFlag::Important as u64;
        }
        if self.custom {
            flags |= SettingsFlag::Custom as u64;
        }
        if self.obsolete {
            flags |= SettingsFlag::Obsolete as u64;
        }
        w.write_varuint(flags)?;
        w.write_string(&self.value)?;

        Ok(())
    }
}

pub struct Param {
    key: String,
    value: String,
}

impl Param {
    fn encode(&mut self, w: &mut impl ProtoWrite) -> Result<()> {
        let mut set = Setting {
            key: self.key.clone(),
            value: self.value.clone(),
            custom: true,
            obsolete: false,
            important: false,
        };
        set.encode(w)
    }
}

// State tells till what stage query has to be executed?
#[derive(Copy, Clone)]
pub enum Stage {
    // Server just returns the columns (schema)
    FetchColumns = 0,
    // Useful only internally in distributed nodes
    WithMergeableState = 1,
    // Normal for client. Server completes the whole query and return the results
    Complete = 2,
}
