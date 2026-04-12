use std::io::{self, Error, ErrorKind, Result};

use crate::{
    client_info::ClientInfo,
    feature::Feature,
    packet::ClientPacket,
    proto::{ProtoRead, ProtoWrite},
};

// Fields are ordered to match wire encoding order.
pub struct Query {
    pub query_id: String,
    pub client_info: ClientInfo, // feature-gated: WRITE_CLIENT_INFO
    pub settings: Vec<Setting>,  // feature-gated: SETTINGS_SERIALIZED_AS_STRINGS
    pub cluster_secret: String,  // feature-gated: INTERSERVER_SECRET
    pub stage: Stage,
    pub compression: bool,
    pub body: String,
    pub params: Vec<Param>, // feature-gated: PARAMETERS

    pub protocol_version: u64,
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
    pub key: String,
    pub value: String,
    pub important: bool,
    pub custom: bool,
    pub obsolete: bool,
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
        // Empty key is the sentinel marking end of settings/params list.
        // No flags or value follow it on the wire.
        if key.is_empty() {
            return Ok(Setting {
                key,
                value: String::new(),
                important: false,
                custom: false,
                obsolete: false,
            });
        }
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
    pub key: String,
    pub value: String,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client_info::{ClientInfo, QueryKind};
    use std::io::Cursor;

    const PROTOCOL: u32 = 54460;

    fn make_client_info() -> ClientInfo {
        ClientInfo {
            query_kind: QueryKind::InitialQuery,
            initial_user: "default".to_string(),
            initial_query_id: "q-1".to_string(),
            initial_address: "127.0.0.1:0".to_string(),
            initial_time: Some(0),
            query_interface: 1,
            os_user: "user".to_string(),
            client_hostname: "host".to_string(),
            client_name: "client".to_string(),
            version_major: 1,
            version_minor: 0,
            protocol_version: PROTOCOL as u64,
            quota_key: Some("".to_string()),
            distributed_depth: Some(0),
            version_patch: Some(0),
            collaborate_with_initiator: Some(false),
            obsolete_count_participating_replicas: Some(0),
            count_current_replicas: Some(0),
        }
    }

    fn make_query() -> Query {
        Query {
            query_id: "test-query-1".to_string(),
            client_info: make_client_info(),
            settings: vec![],
            cluster_secret: "".to_string(),
            stage: Stage::Complete,
            compression: false,
            body: "SELECT 1".to_string(),
            params: vec![],
            protocol_version: PROTOCOL as u64,
        }
    }

    // -- Setting tests --

    #[test]
    fn test_setting_roundtrip() {
        let s = Setting {
            key: "max_threads".to_string(),
            value: "4".to_string(),
            important: true,
            custom: false,
            obsolete: false,
        };
        let mut buf = Vec::new();
        s.encode(&mut buf).unwrap();

        let mut cursor = Cursor::new(buf.as_slice());
        let decoded = Setting::decode(&mut cursor).unwrap();

        assert_eq!(decoded.key, "max_threads");
        assert_eq!(decoded.value, "4");
        assert!(decoded.important);
        assert!(!decoded.custom);
        assert!(!decoded.obsolete);
    }

    #[test]
    fn test_setting_custom_flag() {
        let s = Setting {
            key: "param_x".to_string(),
            value: "hello".to_string(),
            important: false,
            custom: true,
            obsolete: false,
        };
        let mut buf = Vec::new();
        s.encode(&mut buf).unwrap();

        let mut cursor = Cursor::new(buf.as_slice());
        let decoded = Setting::decode(&mut cursor).unwrap();

        assert!(decoded.custom);
        assert!(!decoded.important);
    }

    #[test]
    fn test_setting_all_flags() {
        let s = Setting {
            key: "k".to_string(),
            value: "v".to_string(),
            important: true,
            custom: true,
            obsolete: true,
        };
        let mut buf = Vec::new();
        s.encode(&mut buf).unwrap();

        let mut cursor = Cursor::new(buf.as_slice());
        let decoded = Setting::decode(&mut cursor).unwrap();

        assert!(decoded.important);
        assert!(decoded.custom);
        assert!(decoded.obsolete);
    }

    #[test]
    fn test_setting_wire_format() {
        let s = Setting {
            key: "k".to_string(),
            value: "v".to_string(),
            important: false,
            custom: true,
            obsolete: false,
        };
        let mut buf = Vec::new();
        s.encode(&mut buf).unwrap();

        let mut cursor = Cursor::new(buf.as_slice());
        assert_eq!(cursor.read_string().unwrap(), "k");
        let flags = cursor.read_varuint().unwrap();
        assert_eq!(flags, 0x02); // CUSTOM
        assert_eq!(cursor.read_string().unwrap(), "v");
    }

    #[test]
    fn test_sequential_settings() {
        let settings = vec![
            Setting { key: "a".to_string(), value: "1".to_string(), important: false, custom: false, obsolete: false },
            Setting { key: "b".to_string(), value: "2".to_string(), important: true, custom: false, obsolete: false },
        ];
        let mut buf = Vec::new();
        for s in &settings {
            s.encode(&mut buf).unwrap();
        }
        // Terminator
        buf.write_string("").unwrap();

        let mut cursor = Cursor::new(buf.as_slice());
        let mut decoded = Vec::new();
        loop {
            let s = Setting::decode(&mut cursor).unwrap();
            if s.key.is_empty() {
                break;
            }
            decoded.push(s);
        }
        assert_eq!(decoded.len(), 2);
        assert_eq!(decoded[0].key, "a");
        assert_eq!(decoded[1].key, "b");
        assert!(decoded[1].important);
    }

    // -- Param tests --

    #[test]
    fn test_param_roundtrip() {
        let p = Param {
            key: "user_id".to_string(),
            value: "42".to_string(),
        };
        let mut buf = Vec::new();
        p.encode(&mut buf).unwrap();

        let mut cursor = Cursor::new(buf.as_slice());
        let decoded = Param::decode(&mut cursor).unwrap();

        assert_eq!(decoded.key, "user_id");
        assert_eq!(decoded.value, "42");
    }

    #[test]
    fn test_param_encodes_as_custom_setting() {
        let p = Param {
            key: "name".to_string(),
            value: "test".to_string(),
        };
        let mut buf = Vec::new();
        p.encode(&mut buf).unwrap();

        // Decode as Setting to verify custom flag
        let mut cursor = Cursor::new(buf.as_slice());
        let s = Setting::decode(&mut cursor).unwrap();
        assert!(s.custom);
        assert!(!s.important);
    }

    // -- Stage tests --

    #[test]
    fn test_stage_try_from() {
        assert!(matches!(Stage::try_from(0u64).unwrap(), Stage::FetchColumns));
        assert!(matches!(Stage::try_from(1u64).unwrap(), Stage::WithMergeableState));
        assert!(matches!(Stage::try_from(2u64).unwrap(), Stage::Complete));
    }

    #[test]
    fn test_stage_invalid() {
        assert!(Stage::try_from(99u64).is_err());
    }

    // -- Query tests --

    #[test]
    fn test_query_roundtrip_simple() {
        let q = make_query();
        let mut buf = Vec::new();
        q.encode(&mut buf).unwrap();

        // Skip packet type byte
        let mut cursor = Cursor::new(&buf[1..]);
        let decoded = Query::decode(&mut cursor, PROTOCOL).unwrap();

        assert_eq!(decoded.query_id, "test-query-1");
        assert_eq!(decoded.body, "SELECT 1");
        assert!(matches!(decoded.stage, Stage::Complete));
        assert!(!decoded.compression);
        assert!(decoded.settings.is_empty());
        assert!(decoded.params.is_empty());
    }

    #[test]
    fn test_query_with_settings() {
        let mut q = make_query();
        q.settings = vec![
            Setting { key: "max_threads".to_string(), value: "4".to_string(), important: true, custom: false, obsolete: false },
            Setting { key: "max_memory_usage".to_string(), value: "1000000".to_string(), important: false, custom: false, obsolete: false },
        ];
        let mut buf = Vec::new();
        q.encode(&mut buf).unwrap();

        let mut cursor = Cursor::new(&buf[1..]);
        let decoded = Query::decode(&mut cursor, PROTOCOL).unwrap();

        assert_eq!(decoded.settings.len(), 2);
        assert_eq!(decoded.settings[0].key, "max_threads");
        assert_eq!(decoded.settings[0].value, "4");
        assert!(decoded.settings[0].important);
        assert_eq!(decoded.settings[1].key, "max_memory_usage");
    }

    #[test]
    fn test_query_with_params() {
        let mut q = make_query();
        q.body = "SELECT {x:UInt64}".to_string();
        q.params = vec![
            Param { key: "x".to_string(), value: "42".to_string() },
        ];
        let mut buf = Vec::new();
        q.encode(&mut buf).unwrap();

        let mut cursor = Cursor::new(&buf[1..]);
        let decoded = Query::decode(&mut cursor, PROTOCOL).unwrap();

        assert_eq!(decoded.body, "SELECT {x:UInt64}");
        assert_eq!(decoded.params.len(), 1);
        assert_eq!(decoded.params[0].key, "x");
        assert_eq!(decoded.params[0].value, "42");
    }

    #[test]
    fn test_query_with_settings_and_params() {
        let mut q = make_query();
        q.settings = vec![
            Setting { key: "max_threads".to_string(), value: "2".to_string(), important: false, custom: false, obsolete: false },
        ];
        q.params = vec![
            Param { key: "a".to_string(), value: "1".to_string() },
            Param { key: "b".to_string(), value: "hello".to_string() },
        ];
        let mut buf = Vec::new();
        q.encode(&mut buf).unwrap();

        let mut cursor = Cursor::new(&buf[1..]);
        let decoded = Query::decode(&mut cursor, PROTOCOL).unwrap();

        assert_eq!(decoded.settings.len(), 1);
        assert_eq!(decoded.params.len(), 2);
        assert_eq!(decoded.params[1].value, "hello");
    }

    #[test]
    fn test_query_packet_type() {
        let q = make_query();
        let mut buf = Vec::new();
        q.encode(&mut buf).unwrap();

        let mut cursor = Cursor::new(buf.as_slice());
        let pkt = cursor.read_varuint().unwrap();
        assert_eq!(pkt, ClientPacket::Query as u64);
    }

    #[test]
    fn test_query_all_stages() {
        for (stage, val) in [(Stage::FetchColumns, 0u64), (Stage::WithMergeableState, 1), (Stage::Complete, 2)] {
            let mut q = make_query();
            q.stage = stage;
            let mut buf = Vec::new();
            q.encode(&mut buf).unwrap();

            let mut cursor = Cursor::new(&buf[1..]);
            let decoded = Query::decode(&mut cursor, PROTOCOL).unwrap();
            assert_eq!(decoded.stage as u64, val);
        }
    }

    #[test]
    fn test_query_compression_flag() {
        let mut q = make_query();
        q.compression = true;
        let mut buf = Vec::new();
        q.encode(&mut buf).unwrap();

        let mut cursor = Cursor::new(&buf[1..]);
        let decoded = Query::decode(&mut cursor, PROTOCOL).unwrap();
        assert!(decoded.compression);
    }

    #[test]
    fn test_query_unicode_body() {
        let mut q = make_query();
        q.body = "SELECT '日本語テスト'".to_string();
        let mut buf = Vec::new();
        q.encode(&mut buf).unwrap();

        let mut cursor = Cursor::new(&buf[1..]);
        let decoded = Query::decode(&mut cursor, PROTOCOL).unwrap();
        assert_eq!(decoded.body, "SELECT '日本語テスト'");
    }
}
