use crate::{
    feature::Feature,
    packet::{ClientPacket, ServerPacket},
    proto::{ProtoRead, ProtoWrite},
};

use std::io::{self};

pub struct ClientHello {
    pub name: String,
    pub version_major: u64,
    pub version_minor: u64,
    pub protocol_version: u64,
    pub database: String,
    pub user: String,
    pub password: String,
}

impl ClientHello {
    pub fn encode(&self, w: &mut impl ProtoWrite) -> io::Result<()> {
        w.write_varuint(ClientPacket::Hello as u64)?;
        w.write_string(&self.name)?;
        w.write_varuint(self.version_major)?;
        w.write_varuint(self.version_minor)?;
        w.write_varuint(self.protocol_version)?;
        w.write_string(&self.database)?;
        w.write_string(&self.user)?;
        w.write_string(&self.password)?;
        Ok(())
    }

    pub fn decode(r: &mut impl ProtoRead) -> io::Result<ClientHello> {
        Ok(Self {
            name: r.read_string()?,
            version_major: r.read_varuint()?,
            version_minor: r.read_varuint()?,
            protocol_version: r.read_varuint()?,
            database: r.read_string()?,
            user: r.read_string()?,
            password: r.read_string()?,
        })
    }
}

pub struct ServerHello {
    pub name: String,
    pub version_major: u64,
    pub version_minor: u64,
    pub protocol_version: u64,

    // following things are optional depending on protocol version
    pub timezone: Option<String>,
    pub display_name: Option<String>,
    pub version_patch: Option<u64>,
}

impl ServerHello {
    pub fn encode(&self, w: &mut impl ProtoWrite, protocol: u32) -> io::Result<()> {
        w.write_varuint(ServerPacket::Hello as u64)?;
        w.write_string(&self.name)?;
        w.write_varuint(self.version_major)?;
        w.write_varuint(self.version_minor)?;
        w.write_varuint(self.protocol_version)?;

        if Feature::TIMEZONE.in_version(protocol) {
            w.write_string(&self.timezone.as_deref().ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("timezone required for this protocol version({protocol})"),
                )
            })?)?;
        }

        if Feature::DISPLAY_NAME.in_version(protocol) {
            w.write_string(&self.display_name.as_deref().ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("display_name required for this protocol version({protocol})",),
                )
            })?)?;
        }

        if Feature::VERSION_PATCH.in_version(protocol) {
            w.write_varuint(self.version_patch.ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("version patch required for this protocol version ({protocol})"),
                )
            })?)?;
        }

        Ok(())
    }
    pub fn decode(r: &mut impl ProtoRead, protocol: u32) -> io::Result<ServerHello> {
        Ok(Self {
            name: r.read_string()?,
            version_major: r.read_varuint()?,
            version_minor: r.read_varuint()?,
            protocol_version: r.read_varuint()?,
            timezone: if Feature::TIMEZONE.in_version(protocol) {
                Some(r.read_string()?)
            } else {
                None
            },
            display_name: if Feature::DISPLAY_NAME.in_version(protocol) {
                Some(r.read_string()?)
            } else {
                None
            },
            version_patch: if Feature::VERSION_PATCH.in_version(protocol) {
                Some(r.read_varuint()?)
            } else {
                None
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn make_client_hello() -> ClientHello {
        ClientHello {
            name: "clickhouse-rs".to_string(),
            version_major: 21,
            version_minor: 8,
            protocol_version: 54460,
            database: "default".to_string(),
            user: "admin".to_string(),
            password: "secret".to_string(),
        }
    }

    fn make_server_hello_full() -> ServerHello {
        ServerHello {
            name: "ClickHouse".to_string(),
            version_major: 21,
            version_minor: 8,
            protocol_version: 54460,
            timezone: Some("UTC".to_string()),
            display_name: Some("production-1".to_string()),
            version_patch: Some(3),
        }
    }

    // -- ClientHello --

    #[test]
    fn test_client_hello_roundtrip() {
        let hello = make_client_hello();
        let mut buf = Vec::new();
        hello.encode(&mut buf).unwrap();

        // Skip the packet type byte
        let mut cursor = Cursor::new(&buf[1..]);
        let decoded = ClientHello::decode(&mut cursor).unwrap();

        assert_eq!(decoded.name, hello.name);
        assert_eq!(decoded.version_major, hello.version_major);
        assert_eq!(decoded.version_minor, hello.version_minor);
        assert_eq!(decoded.protocol_version, hello.protocol_version);
        assert_eq!(decoded.database, hello.database);
        assert_eq!(decoded.user, hello.user);
        assert_eq!(decoded.password, hello.password);
    }

    #[test]
    fn test_client_hello_packet_type() {
        let hello = make_client_hello();
        let mut buf = Vec::new();
        hello.encode(&mut buf).unwrap();

        let mut cursor = Cursor::new(buf.as_slice());
        let packet_type = cursor.read_varuint().unwrap();
        assert_eq!(packet_type, ClientPacket::Hello as u64);
    }

    #[test]
    fn test_client_hello_empty_fields() {
        let hello = ClientHello {
            name: "".to_string(),
            version_major: 0,
            version_minor: 0,
            protocol_version: 0,
            database: "".to_string(),
            user: "".to_string(),
            password: "".to_string(),
        };
        let mut buf = Vec::new();
        hello.encode(&mut buf).unwrap();

        let mut cursor = Cursor::new(&buf[1..]);
        let decoded = ClientHello::decode(&mut cursor).unwrap();

        assert_eq!(decoded.name, "");
        assert_eq!(decoded.version_major, 0);
        assert_eq!(decoded.database, "");
        assert_eq!(decoded.user, "");
        assert_eq!(decoded.password, "");
    }

    #[test]
    fn test_client_hello_unicode_fields() {
        let hello = ClientHello {
            name: "клиент".to_string(),
            version_major: 1,
            version_minor: 0,
            protocol_version: 54460,
            database: "база_данных".to_string(),
            user: "пользователь".to_string(),
            password: "пароль🔑".to_string(),
        };
        let mut buf = Vec::new();
        hello.encode(&mut buf).unwrap();

        let mut cursor = Cursor::new(&buf[1..]);
        let decoded = ClientHello::decode(&mut cursor).unwrap();

        assert_eq!(decoded.name, "клиент");
        assert_eq!(decoded.database, "база_данных");
        assert_eq!(decoded.password, "пароль🔑");
    }

    // -- ServerHello --

    #[test]
    fn test_server_hello_roundtrip_all_features() {
        let protocol = 54460; // all features enabled
        let hello = make_server_hello_full();
        let mut buf = Vec::new();
        hello.encode(&mut buf, protocol).unwrap();

        let mut cursor = Cursor::new(&buf[1..]); // skip packet type
        let decoded = ServerHello::decode(&mut cursor, protocol).unwrap();

        assert_eq!(decoded.name, hello.name);
        assert_eq!(decoded.version_major, hello.version_major);
        assert_eq!(decoded.version_minor, hello.version_minor);
        assert_eq!(decoded.protocol_version, hello.protocol_version);
        assert_eq!(decoded.timezone, Some("UTC".to_string()));
        assert_eq!(decoded.display_name, Some("production-1".to_string()));
        assert_eq!(decoded.version_patch, Some(3));
    }

    #[test]
    fn test_server_hello_no_features() {
        let protocol = 50000; // before all features
        let hello = ServerHello {
            name: "ClickHouse".to_string(),
            version_major: 19,
            version_minor: 1,
            protocol_version: 50000,
            timezone: None,
            display_name: None,
            version_patch: None,
        };
        let mut buf = Vec::new();
        hello.encode(&mut buf, protocol).unwrap();

        let mut cursor = Cursor::new(&buf[1..]);
        let decoded = ServerHello::decode(&mut cursor, protocol).unwrap();

        assert_eq!(decoded.name, "ClickHouse");
        assert_eq!(decoded.timezone, None);
        assert_eq!(decoded.display_name, None);
        assert_eq!(decoded.version_patch, None);
    }

    #[test]
    fn test_server_hello_only_timezone() {
        let protocol = 54058; // only timezone enabled
        let hello = ServerHello {
            name: "ClickHouse".to_string(),
            version_major: 20,
            version_minor: 0,
            protocol_version: 54058,
            timezone: Some("Europe/Moscow".to_string()),
            display_name: None,
            version_patch: None,
        };
        let mut buf = Vec::new();
        hello.encode(&mut buf, protocol).unwrap();

        let mut cursor = Cursor::new(&buf[1..]);
        let decoded = ServerHello::decode(&mut cursor, protocol).unwrap();

        assert_eq!(decoded.timezone, Some("Europe/Moscow".to_string()));
        assert_eq!(decoded.display_name, None);
        assert_eq!(decoded.version_patch, None);
    }

    #[test]
    fn test_server_hello_timezone_and_display_name() {
        let protocol = 54372; // timezone + display_name, no patch
        let hello = ServerHello {
            name: "ClickHouse".to_string(),
            version_major: 21,
            version_minor: 0,
            protocol_version: 54372,
            timezone: Some("Asia/Kolkata".to_string()),
            display_name: Some("replica-2".to_string()),
            version_patch: None,
        };
        let mut buf = Vec::new();
        hello.encode(&mut buf, protocol).unwrap();

        let mut cursor = Cursor::new(&buf[1..]);
        let decoded = ServerHello::decode(&mut cursor, protocol).unwrap();

        assert_eq!(decoded.timezone, Some("Asia/Kolkata".to_string()));
        assert_eq!(decoded.display_name, Some("replica-2".to_string()));
        assert_eq!(decoded.version_patch, None);
    }

    #[test]
    fn test_server_hello_packet_type() {
        let hello = make_server_hello_full();
        let mut buf = Vec::new();
        hello.encode(&mut buf, 54460).unwrap();

        let mut cursor = Cursor::new(buf.as_slice());
        let packet_type = cursor.read_varuint().unwrap();
        assert_eq!(packet_type, ServerPacket::Hello as u64);
    }

    #[test]
    fn test_server_hello_encode_errors_on_missing_timezone() {
        let protocol = 54460;
        let hello = ServerHello {
            name: "ClickHouse".to_string(),
            version_major: 21,
            version_minor: 8,
            protocol_version: 54460,
            timezone: None, // missing but required
            display_name: Some("test".to_string()),
            version_patch: Some(1),
        };
        let mut buf = Vec::new();
        let err = hello.encode(&mut buf, protocol).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn test_server_hello_encode_errors_on_missing_display_name() {
        let protocol = 54460;
        let hello = ServerHello {
            name: "ClickHouse".to_string(),
            version_major: 21,
            version_minor: 8,
            protocol_version: 54460,
            timezone: Some("UTC".to_string()),
            display_name: None, // missing but required
            version_patch: Some(1),
        };
        let mut buf = Vec::new();
        let err = hello.encode(&mut buf, protocol).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn test_server_hello_encode_errors_on_missing_patch() {
        let protocol = 54460;
        let hello = ServerHello {
            name: "ClickHouse".to_string(),
            version_major: 21,
            version_minor: 8,
            protocol_version: 54460,
            timezone: Some("UTC".to_string()),
            display_name: Some("test".to_string()),
            version_patch: None, // missing but required
        };
        let mut buf = Vec::new();
        let err = hello.encode(&mut buf, protocol).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    // -- Wire format consistency --

    #[test]
    fn test_server_hello_wire_size_varies_with_protocol() {
        let hello = make_server_hello_full();

        let mut buf_old = Vec::new();
        // Use a protocol version before all features — only base fields
        let mut base_hello = ServerHello {
            name: hello.name.clone(),
            version_major: hello.version_major,
            version_minor: hello.version_minor,
            protocol_version: hello.protocol_version,
            timezone: None,
            display_name: None,
            version_patch: None,
        };
        base_hello.encode(&mut buf_old, 50000).unwrap();

        let mut buf_new = Vec::new();
        hello.encode(&mut buf_new, 54460).unwrap();

        // Full-featured encoding must be larger
        assert!(buf_new.len() > buf_old.len());
    }

    // -- Sequential messages --

    #[test]
    fn test_client_then_server_hello_on_same_stream() {
        let mut buf = Vec::new();

        let client_hello = make_client_hello();
        client_hello.encode(&mut buf).unwrap();

        let server_hello = make_server_hello_full();
        let protocol = 54460;
        server_hello.encode(&mut buf, protocol).unwrap();

        let mut cursor = Cursor::new(buf.as_slice());

        // Read client hello
        let pkt = cursor.read_varuint().unwrap();
        assert_eq!(pkt, ClientPacket::Hello as u64);
        let decoded_client = ClientHello::decode(&mut cursor).unwrap();
        assert_eq!(decoded_client.name, "clickhouse-rs");

        // Read server hello
        let pkt = cursor.read_varuint().unwrap();
        assert_eq!(pkt, ServerPacket::Hello as u64);
        let decoded_server = ServerHello::decode(&mut cursor, protocol).unwrap();
        assert_eq!(decoded_server.name, "ClickHouse");
        assert_eq!(decoded_server.timezone, Some("UTC".to_string()));
    }
}
