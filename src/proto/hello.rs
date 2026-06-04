use super::{
    feature::Feature,
    packet::{ClientPacket, ServerPacket},
    query::Setting,
    wire::{ProtoRead, ProtoWrite},
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

// Fields are ordered to match wire encoding order.
#[derive(Debug)]
pub struct ServerHello {
    pub name: String,
    pub version_major: u64,
    pub version_minor: u64,
    pub protocol_version: u64,
    /// Server's parallel-replicas coordination protocol version. Feature-gated
    /// by `VERSIONED_PARALLEL_REPLICAS_PROTOCOL` (v54471). Wire position is
    /// **immediately after `protocol_version`** (before `timezone`) — earlier
    /// on the wire than every other optional field despite the highest version
    /// gate so far. Mirrors `Server/TCPHandler.cpp::sendHello:2099-2100`.
    pub parallel_replicas_protocol_version: Option<u64>,
    pub timezone: Option<String>,     // feature-gated: TIMEZONE
    pub display_name: Option<String>, // feature-gated: DISPLAY_NAME
    pub version_patch: Option<u64>,   // feature-gated: VERSION_PATCH
    /// Server's preferred outbound chunked mode — one of "chunked",
    /// "notchunked", "chunked_optional", "notchunked_optional". Feature-gated
    /// by CHUNKED_PROTOCOL (v54470). Sits on the wire BEFORE
    /// `password_complexity_rules` even though its version gate is higher,
    /// per `Server/TCPHandler.cpp::sendHello`.
    pub proto_send_chunked_srv: Option<String>,
    /// Server's preferred inbound chunked mode.
    pub proto_recv_chunked_srv: Option<String>,
    // talks about what each password complexity rules are. Individual rule are basically
    // a pair of tuple of (Regex-Pattern, Explanation).
    // 1. Regex-Pattern - express the password rule (e.g: no special symbols)
    // 2. Explanation - A string that explains the password rule if not met.
    pub password_complexity_rules: Option<Vec<(String, String)>>, // feature-gated: PASSWORD_COMPLEXITY_RULES
    /// Inter-server signing nonce — 8 bytes UInt64 LE, fixed-width on the wire.
    /// Feature-gated by INTERSERVER_SECRET_V2 (v54462). External clients decode
    /// it to keep stream alignment and otherwise ignore the value.
    pub nonce: Option<u64>,
    /// Server's non-default settings, broadcast at v54474+. Empty list when
    /// the server has no overrides to share or when running in inter-server
    /// mode. Format on the wire matches the Query packet's settings list
    /// (key, flags, value triples terminated by an empty key).
    pub server_settings: Option<Vec<Setting>>,
}

impl ServerHello {
    pub fn encode(&self, w: &mut impl ProtoWrite, protocol: u32) -> io::Result<()> {
        w.write_varuint(ServerPacket::Hello as u64)?;
        w.write_string(&self.name)?;
        w.write_varuint(self.version_major)?;
        w.write_varuint(self.version_minor)?;
        w.write_varuint(self.protocol_version)?;

        if Feature::VERSIONED_PARALLEL_REPLICAS_PROTOCOL.in_version(protocol) {
            let v = self.parallel_replicas_protocol_version.ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("parallel_replicas_protocol_version required at v{protocol}"),
                )
            })?;
            w.write_varuint(v)?;
        }

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

        // Chunked-protocol negotiation strings. On the wire these go BEFORE
        // `password_complexity_rules` (v54461) and `nonce` (v54462) — the
        // C++ writer's order in `TCPHandler.cpp::sendHello` is what matters,
        // not the strictly-ascending feature versions.
        if Feature::CHUNKED_PROTOCOL.in_version(protocol) {
            let send_pref = self.proto_send_chunked_srv.as_deref().ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("proto_send_chunked_srv required for this protocol version ({protocol})"),
                )
            })?;
            let recv_pref = self.proto_recv_chunked_srv.as_deref().ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("proto_recv_chunked_srv required for this protocol version ({protocol})"),
                )
            })?;
            w.write_string(send_pref)?;
            w.write_string(recv_pref)?;
        }

        if Feature::PASSWORD_COMPLEXITY_RULES.in_version(protocol) {
            let rules = self.password_complexity_rules.as_ref().ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("password_complexity_rules required for this protocol version ({protocol})"),
                )
            })?;
            w.write_varuint(rules.len() as u64)?;
            for (pattern, message) in rules {
                w.write_string(pattern)?;
                w.write_string(message)?;
            }
        }

        if Feature::INTERSERVER_SECRET_V2.in_version(protocol) {
            let nonce = self.nonce.ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("nonce required for this protocol version ({protocol})"),
                )
            })?;
            w.write_u64(nonce)?;
        }

        if Feature::SERVER_SETTINGS.in_version(protocol) {
            let settings = self.server_settings.as_ref().ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("server_settings required at v{protocol}"),
                )
            })?;
            for s in settings {
                s.encode(w)?;
            }
            // Empty-key terminator matches the wire's "end of settings list".
            w.write_string("")?;
        }

        Ok(())
    }
    pub fn decode(r: &mut impl ProtoRead, protocol: u32) -> io::Result<ServerHello> {
        Ok(Self {
            name: r.read_string()?,
            version_major: r.read_varuint()?,
            version_minor: r.read_varuint()?,
            protocol_version: r.read_varuint()?,
            parallel_replicas_protocol_version:
                if Feature::VERSIONED_PARALLEL_REPLICAS_PROTOCOL.in_version(protocol) {
                    Some(r.read_varuint()?)
                } else {
                    None
                },
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
            proto_send_chunked_srv: if Feature::CHUNKED_PROTOCOL.in_version(protocol) {
                Some(r.read_string()?)
            } else {
                None
            },
            proto_recv_chunked_srv: if Feature::CHUNKED_PROTOCOL.in_version(protocol) {
                Some(r.read_string()?)
            } else {
                None
            },
            password_complexity_rules: if Feature::PASSWORD_COMPLEXITY_RULES.in_version(protocol) {
                let count = r.read_varuint()?;
                if count > MAX_PASSWORD_COMPLEXITY_RULES {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!(
                            "password_complexity_rules count {count} exceeds maximum of {MAX_PASSWORD_COMPLEXITY_RULES}"
                        ),
                    ));
                }
                let mut rules = Vec::with_capacity(count as usize);
                for _ in 0..count {
                    let pattern = r.read_string()?;
                    let message = r.read_string()?;
                    rules.push((pattern, message));
                }
                Some(rules)
            } else {
                None
            },
            nonce: if Feature::INTERSERVER_SECRET_V2.in_version(protocol) {
                Some(r.read_u64()?)
            } else {
                None
            },
            server_settings: if Feature::SERVER_SETTINGS.in_version(protocol) {
                let mut out = Vec::new();
                loop {
                    let s = Setting::decode(r)?;
                    if s.key.is_empty() {
                        break;
                    }
                    out.push(s);
                }
                Some(out)
            } else {
                None
            },
        })
    }
}

// Defensive cap on `ServerHello.password_complexity_rules` count. Matches the
// canonical server- and client-side limit `DBMS_MAX_PASSWORD_COMPLEXITY_RULES`
// in `ClickHouse/src/Core/ProtocolDefines.h`. Prevents a hostile or
// misconfigured server from forcing an unbounded allocation via the
// `Vec::with_capacity(count)` call above. See IMPLEMENTATION_NOTES §1.11.
const MAX_PASSWORD_COMPLEXITY_RULES: u64 = 256;

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
            parallel_replicas_protocol_version: None,
            timezone: Some("UTC".to_string()),
            display_name: Some("production-1".to_string()),
            version_patch: Some(3),
            proto_send_chunked_srv: None,
            proto_recv_chunked_srv: None,
            password_complexity_rules: None,
            nonce: None,
            server_settings: None,
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
            parallel_replicas_protocol_version: None,
            timezone: None,
            display_name: None,
            version_patch: None,
            proto_send_chunked_srv: None,
            proto_recv_chunked_srv: None,
            password_complexity_rules: None,
            nonce: None,
            server_settings: None,
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
            parallel_replicas_protocol_version: None,
            timezone: Some("Europe/Moscow".to_string()),
            display_name: None,
            version_patch: None,
            proto_send_chunked_srv: None,
            proto_recv_chunked_srv: None,
            password_complexity_rules: None,
            nonce: None,
            server_settings: None,
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
            parallel_replicas_protocol_version: None,
            timezone: Some("Asia/Kolkata".to_string()),
            display_name: Some("replica-2".to_string()),
            version_patch: None,
            proto_send_chunked_srv: None,
            proto_recv_chunked_srv: None,
            password_complexity_rules: None,
            nonce: None,
            server_settings: None,
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
            parallel_replicas_protocol_version: None,
            timezone: None, // missing but required
            display_name: Some("test".to_string()),
            version_patch: Some(1),
            proto_send_chunked_srv: None,
            proto_recv_chunked_srv: None,
            password_complexity_rules: None,
            nonce: None,
            server_settings: None,
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
            parallel_replicas_protocol_version: None,
            timezone: Some("UTC".to_string()),
            display_name: None, // missing but required
            version_patch: Some(1),
            proto_send_chunked_srv: None,
            proto_recv_chunked_srv: None,
            password_complexity_rules: None,
            nonce: None,
            server_settings: None,
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
            parallel_replicas_protocol_version: None,
            timezone: Some("UTC".to_string()),
            display_name: Some("test".to_string()),
            version_patch: None, // missing but required
            proto_send_chunked_srv: None,
            proto_recv_chunked_srv: None,
            password_complexity_rules: None,
            nonce: None,
            server_settings: None,
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
            parallel_replicas_protocol_version: None,
            timezone: None,
            display_name: None,
            version_patch: None,
            proto_send_chunked_srv: None,
            proto_recv_chunked_srv: None,
            password_complexity_rules: None,
            nonce: None,
            server_settings: None,
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

    // -- Password complexity rules (v54461) --

    fn make_server_hello_v54461(rules: Option<Vec<(String, String)>>) -> ServerHello {
        ServerHello {
            name: "ClickHouse".to_string(),
            version_major: 23,
            version_minor: 1,
            protocol_version: Feature::PASSWORD_COMPLEXITY_RULES.version() as u64,
            parallel_replicas_protocol_version: None,
            timezone: Some("UTC".to_string()),
            display_name: Some("test".to_string()),
            version_patch: Some(0),
            proto_send_chunked_srv: None,
            proto_recv_chunked_srv: None,
            password_complexity_rules: rules,
            nonce: None,
            server_settings: None,
        }
    }

    #[test]
    fn test_server_hello_roundtrip_with_password_rules() {
        let protocol = Feature::PASSWORD_COMPLEXITY_RULES.version();
        let rules = vec![
            (
                ".{12,}".to_string(),
                "Password must be at least 12 characters".to_string(),
            ),
            (
                "[0-9]".to_string(),
                "Password must contain a digit".to_string(),
            ),
        ];
        let hello = make_server_hello_v54461(Some(rules.clone()));

        let mut buf = Vec::new();
        hello.encode(&mut buf, protocol).unwrap();

        let mut cursor = Cursor::new(&buf[1..]); // skip packet type
        let decoded = ServerHello::decode(&mut cursor, protocol).unwrap();
        assert_eq!(decoded.password_complexity_rules, Some(rules));
    }

    #[test]
    fn test_server_hello_roundtrip_zero_password_rules() {
        // The common real-world case: server has no password policy configured.
        // The feature is active so the field is present, but the list is empty.
        let protocol = Feature::PASSWORD_COMPLEXITY_RULES.version();
        let hello = make_server_hello_v54461(Some(vec![]));

        let mut buf = Vec::new();
        hello.encode(&mut buf, protocol).unwrap();

        let mut cursor = Cursor::new(&buf[1..]);
        let decoded = ServerHello::decode(&mut cursor, protocol).unwrap();
        assert_eq!(decoded.password_complexity_rules, Some(vec![]));
    }

    #[test]
    fn test_server_hello_password_rules_absent_below_v54461() {
        // At v54460 the field gate is closed — encode emits nothing, decode
        // returns None, and the stream position is identical to a v54460 hello
        // without the field. (Regression guard against a future change that
        // accidentally always emits the field.)
        let protocol = 54460;
        let hello = ServerHello {
            name: "ClickHouse".to_string(),
            version_major: 22,
            version_minor: 0,
            protocol_version: 54460,
            parallel_replicas_protocol_version: None,
            timezone: Some("UTC".to_string()),
            display_name: Some("test".to_string()),
            version_patch: Some(0),
            proto_send_chunked_srv: None,
            proto_recv_chunked_srv: None,
            password_complexity_rules: None,
            nonce: None,
            server_settings: None,
        };
        let mut buf = Vec::new();
        hello.encode(&mut buf, protocol).unwrap();

        let mut cursor = Cursor::new(&buf[1..]);
        let decoded = ServerHello::decode(&mut cursor, protocol).unwrap();
        assert_eq!(decoded.password_complexity_rules, None);
        // Stream must be fully consumed — no trailing bytes from a stray field.
        assert_eq!(cursor.position() as usize, buf.len() - 1);
    }

    #[test]
    fn test_server_hello_encode_errors_on_missing_password_rules() {
        let protocol = Feature::PASSWORD_COMPLEXITY_RULES.version();
        let hello = make_server_hello_v54461(None);

        let mut buf = Vec::new();
        let err = hello.encode(&mut buf, protocol).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn test_server_hello_decode_rejects_oversized_rules_count() {
        // A hostile server could send a huge VarUInt count, hoping a naive
        // decoder will pre-allocate a vector of that size. Verify the cap
        // rejects it with InvalidData before any allocation happens.
        let protocol = Feature::PASSWORD_COMPLEXITY_RULES.version();
        let mut buf = Vec::new();
        // Hand-craft a stream that gets past all gated fields and lands on
        // an oversized count. The packet-type byte is not part of `decode`
        // so we skip it.
        buf.write_string("ClickHouse").unwrap();
        buf.write_varuint(23).unwrap(); // version_major
        buf.write_varuint(1).unwrap(); // version_minor
        buf.write_varuint(protocol as u64).unwrap(); // protocol_version
        buf.write_string("UTC").unwrap(); // timezone
        buf.write_string("test").unwrap(); // display_name
        buf.write_varuint(0).unwrap(); // version_patch
        buf.write_varuint(1000).unwrap(); // OVERSIZED rules count

        let mut cursor = Cursor::new(buf.as_slice());
        let err = ServerHello::decode(&mut cursor, protocol).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        let msg = err.to_string();
        assert!(
            msg.contains("1000") && msg.contains("256"),
            "expected error to mention both the bad count and the limit; got: {msg}"
        );
    }

    #[test]
    fn test_server_hello_wire_size_grows_with_password_rules() {
        // Sanity check: encoding one rule produces strictly more bytes than
        // encoding zero rules. Guards against a regression where the encoder
        // accidentally drops the per-rule strings.
        let protocol = Feature::PASSWORD_COMPLEXITY_RULES.version();
        let empty = make_server_hello_v54461(Some(vec![]));
        let populated = make_server_hello_v54461(Some(vec![(
            ".{8,}".to_string(),
            "min 8 chars".to_string(),
        )]));

        let mut buf_empty = Vec::new();
        empty.encode(&mut buf_empty, protocol).unwrap();
        let mut buf_pop = Vec::new();
        populated.encode(&mut buf_pop, protocol).unwrap();

        assert!(buf_pop.len() > buf_empty.len());
    }

    // -- Interserver secret v2 nonce (v54462) --

    #[test]
    fn test_server_hello_nonce_roundtrip() {
        // At v54462+, ServerHello carries an 8-byte UInt64 LE nonce. The
        // canonical server emits a random value; external clients must
        // decode it to keep stream alignment.
        let protocol = Feature::INTERSERVER_SECRET_V2.version();
        let hello = ServerHello {
            name: "ClickHouse".to_string(),
            version_major: 24,
            version_minor: 2,
            protocol_version: protocol as u64,
            parallel_replicas_protocol_version: None,
            timezone: Some("UTC".to_string()),
            display_name: Some("t".to_string()),
            version_patch: Some(0),
            proto_send_chunked_srv: None,
            proto_recv_chunked_srv: None,
            password_complexity_rules: Some(vec![]),
            nonce: Some(0xDEAD_BEEF_CAFE_BABE),
            server_settings: None,
        };

        let mut buf = Vec::new();
        hello.encode(&mut buf, protocol).unwrap();

        // Confirm the last 8 bytes are the LE-encoded nonce.
        assert_eq!(
            &buf[buf.len() - 8..],
            &0xDEAD_BEEF_CAFE_BABE_u64.to_le_bytes()
        );

        let mut cursor = Cursor::new(&buf[1..]); // skip packet type
        let decoded = ServerHello::decode(&mut cursor, protocol).unwrap();
        assert_eq!(decoded.nonce, Some(0xDEAD_BEEF_CAFE_BABE));
    }

    #[test]
    fn test_server_hello_nonce_absent_below_v54462() {
        // At v54461 the nonce gate is closed — encode emits nothing, decode
        // returns None. Stream must be fully consumed after decoding.
        let protocol = 54461;
        let hello = ServerHello {
            name: "ClickHouse".to_string(),
            version_major: 23,
            version_minor: 1,
            protocol_version: 54461,
            parallel_replicas_protocol_version: None,
            timezone: Some("UTC".to_string()),
            display_name: Some("t".to_string()),
            version_patch: Some(0),
            proto_send_chunked_srv: None,
            proto_recv_chunked_srv: None,
            password_complexity_rules: Some(vec![]),
            nonce: None,
            server_settings: None,
        };
        let mut buf = Vec::new();
        hello.encode(&mut buf, protocol).unwrap();

        let mut cursor = Cursor::new(&buf[1..]);
        let decoded = ServerHello::decode(&mut cursor, protocol).unwrap();
        assert_eq!(decoded.nonce, None);
        assert_eq!(cursor.position() as usize, buf.len() - 1);
    }

    #[test]
    fn test_server_hello_encode_errors_on_missing_nonce() {
        let protocol = Feature::INTERSERVER_SECRET_V2.version();
        let hello = ServerHello {
            name: "ClickHouse".to_string(),
            version_major: 24,
            version_minor: 2,
            protocol_version: protocol as u64,
            parallel_replicas_protocol_version: None,
            timezone: Some("UTC".to_string()),
            display_name: Some("t".to_string()),
            version_patch: Some(0),
            proto_send_chunked_srv: None,
            proto_recv_chunked_srv: None,
            password_complexity_rules: Some(vec![]),
            nonce: None, // missing but required at v54462
            server_settings: None,
        };
        let mut buf = Vec::new();
        let err = hello.encode(&mut buf, protocol).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    // -- Parallel-replicas protocol version (v54471) --

    #[test]
    fn test_server_hello_parallel_replicas_version_roundtrip() {
        // At v54471+, ServerHello carries a VarUInt parallel-replicas
        // protocol version IMMEDIATELY AFTER `protocol_version`. Verifies
        // both the wire ordering and the field roundtrip.
        let protocol = Feature::VERSIONED_PARALLEL_REPLICAS_PROTOCOL.version();
        let hello = ServerHello {
            name: "ClickHouse".to_string(),
            version_major: 24,
            version_minor: 3,
            protocol_version: protocol as u64,
            parallel_replicas_protocol_version: Some(7),
            timezone: Some("UTC".to_string()),
            display_name: Some("t".to_string()),
            version_patch: Some(0),
            proto_send_chunked_srv: Some("notchunked_optional".to_string()),
            proto_recv_chunked_srv: Some("notchunked_optional".to_string()),
            password_complexity_rules: Some(vec![]),
            nonce: Some(0),
            server_settings: None,
        };

        let mut buf = Vec::new();
        hello.encode(&mut buf, protocol).unwrap();
        let mut cursor = Cursor::new(&buf[1..]);
        let decoded = ServerHello::decode(&mut cursor, protocol).unwrap();
        assert_eq!(decoded.parallel_replicas_protocol_version, Some(7));
        // And the rest still survives — proves wire position.
        assert_eq!(decoded.timezone, Some("UTC".to_string()));
        assert_eq!(decoded.nonce, Some(0));
    }

    #[test]
    fn test_server_hello_parallel_replicas_version_absent_below_v54471() {
        let protocol = 54470;
        let hello = ServerHello {
            name: "ClickHouse".to_string(),
            version_major: 24,
            version_minor: 3,
            protocol_version: 54470,
            parallel_replicas_protocol_version: None,
            timezone: Some("UTC".to_string()),
            display_name: Some("t".to_string()),
            version_patch: Some(0),
            proto_send_chunked_srv: Some("notchunked_optional".to_string()),
            proto_recv_chunked_srv: Some("notchunked_optional".to_string()),
            password_complexity_rules: Some(vec![]),
            nonce: Some(0),
            server_settings: None,
        };
        let mut buf = Vec::new();
        hello.encode(&mut buf, protocol).unwrap();
        let mut cursor = Cursor::new(&buf[1..]);
        let decoded = ServerHello::decode(&mut cursor, protocol).unwrap();
        assert_eq!(decoded.parallel_replicas_protocol_version, None);
        assert_eq!(cursor.position() as usize, buf.len() - 1);
    }
}
