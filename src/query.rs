use std::io::{self, Error, ErrorKind, Result};

use crate::{
    client_info::ClientInfo,
    feature::Feature,
    packet::ClientPacket,
    proto::{ProtoRead, ProtoWrite},
};

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
    pub fn decode(r: &mut impl ProtoRead, protocol: u32) -> Result<Query> {
        let query_id = r.read_string()?;

        let client_info = if Feature::WRITE_CLIENT_INFO.in_version(protocol) {
            ClientInfo::decode(r, protocol)?
        } else {
            return Err(Error::new(
                ErrorKind::InvalidData,
                format!("protocol {protocol} too old: WRITE_CLIENT_INFO required"),
            ));
        };

        // Settings: read until empty key
        let mut settings = Vec::new();
        if Feature::SETTINGS_SERIALIZED_AS_STRINGS.in_version(protocol) {
            loop {
                let s = Setting::decode(r)?;
                if s.key.is_empty() {
                    break;
                }
                settings.push(s);
            }
        } else {
            return Err(Error::new(
                ErrorKind::InvalidData,
                format!("protocol {protocol} too old: SETTINGS_SERIALIZED_AS_STRINGS required"),
            ));
        }

        let cluster_secret = if Feature::INTERSERVER_SECRET.in_version(protocol) {
            r.read_string()?
        } else {
            String::new()
        };

        let stage = Stage::try_from(r.read_varuint()?)?;
        let compression = r.read_varuint()? != 0;
        let body = r.read_string()?;

        let mut params = Vec::new();
        if Feature::PARAMETERS.in_version(protocol) {
            loop {
                let p = Param::decode(r)?;
                if p.key.is_empty() {
                    break;
                }
                params.push(p);
            }
        }

        Ok(Query {
            query_id,
            client_info,
            settings,
            cluster_secret,
            stage,
            compression,
            body,
            params,
            protocol_version: protocol as u64,
        })
    }

    pub fn encode(&self, w: &mut impl ProtoWrite) -> Result<()> {
        let protocol = self.protocol_version as u32;

        w.write_varuint(ClientPacket::Query as u64)?;
        w.write_string(&self.query_id)?;
        if Feature::WRITE_CLIENT_INFO.in_version(protocol) {
            self.client_info.encode(w, protocol)?;
        }

        if Feature::SETTINGS_SERIALIZED_AS_STRINGS.in_version(protocol) {
            for s in &self.settings {
                s.encode(w)?;
            }
        }
        w.write_string("")?; // empty string denotes end of settings

        if Feature::INTERSERVER_SECRET.in_version(protocol) {
            w.write_string(&self.cluster_secret)?;
        }

        w.write_varuint(self.stage as u64)?;
        w.write_varuint(self.compression as u64)?;
        w.write_string(&self.body)?;

        if Feature::PARAMETERS.in_version(protocol) {
            for p in &self.params {
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
    pub fn encode(&self, w: &mut impl ProtoWrite) -> Result<()> {
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

    pub fn decode(r: &mut impl ProtoRead) -> Result<Setting> {
        let key = r.read_string()?;
        let flags = r.read_varuint()?;
        let value = r.read_string()?;

        Ok(Setting {
            key,
            value,
            important: flags & SettingsFlag::Important as u64 != 0,
            custom: flags & SettingsFlag::Custom as u64 != 0,
            obsolete: flags & SettingsFlag::Obsolete as u64 != 0,
        })
    }
}

pub struct Param {
    key: String,
    value: String,
}

impl Param {
    pub fn encode(&self, w: &mut impl ProtoWrite) -> Result<()> {
        Setting {
            key: self.key.clone(),
            value: self.value.clone(),
            custom: true,
            obsolete: false,
            important: false,
        }
        .encode(w)
    }

    pub fn decode(r: &mut impl ProtoRead) -> Result<Param> {
        let s = Setting::decode(r)?;
        Ok(Param {
            key: s.key,
            value: s.value,
        })
    }
}

// Stage tells till what stage query has to be executed?
#[derive(Copy, Clone)]
pub enum Stage {
    // Server just returns the columns (schema)
    FetchColumns = 0,
    // Useful only internally in distributed nodes
    WithMergeableState = 1,
    // Normal for client. Server completes the whole query and return the results
    Complete = 2,
}

impl TryFrom<u64> for Stage {
    type Error = io::Error;
    fn try_from(value: u64) -> std::result::Result<Self, Self::Error> {
        match value {
            0 => Ok(Stage::FetchColumns),
            1 => Ok(Stage::WithMergeableState),
            2 => Ok(Stage::Complete),
            _ => Err(Error::new(
                ErrorKind::InvalidData,
                format!("received invalid Stage {value}"),
            )),
        }
    }
}
