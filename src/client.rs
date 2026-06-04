use std::{
    io::{self, Error, Result, Write},
    net::TcpStream,
};

use crate::{
    block::Block,
    options::QueryOptions,
    proto::{
        self,
        chunked::{self, ChunkedStream},
        client_info::{ClientInfo, QueryKind},
        exception::ServerException,
        external_table::ExternalTable,
        feature::Feature,
        hello::{ClientHello, ServerHello},
        packet::{ClientPacket, ServerPacket, ServerResponse},
        profile::ProfileInfo,
        progress::Progress,
        query::{Query, Setting, Stage},
        table_columns::TableColumns,
        wire::{ProtoRead, ProtoWrite},
    },
    query_result::QueryResult,
};

/// Auto-inject `output_format_native_write_json_as_string=1` so JSON columns
/// always come back in Tier 1 (String fallback) shape — the only JSON
/// serialization version this client decodes (see SPEC §8.4.2.1). Skipped if
/// the user already set this setting explicitly (their value wins).
fn inject_json_string_setting(mut settings: Vec<Setting>) -> Vec<Setting> {
    const KEY: &str = "output_format_native_write_json_as_string";
    if !settings.iter().any(|s| s.key == KEY) {
        settings.push(Setting {
            key: KEY.to_string(),
            value: "1".to_string(),
            important: false,
            custom: false,
            obsolete: false,
        });
    }
    settings
}

#[derive(Debug)]
pub struct Connection {
    /// All bytes to/from the server flow through this wrapper. Both
    /// directions start in passthrough mode; if the v54470+ chunked
    /// protocol negotiation in the Addendum lands on "chunked" for either
    /// direction, we flip the corresponding flag on this stream after the
    /// Addendum has been sent.
    inner: ChunkedStream<TcpStream>,
    database: Option<String>,
    user: Option<String>,
    password: Option<String>,

    protocol: u64,
    /// Negotiated chunked-mode result. `"chunked"` or `"notchunked"` once
    /// the handshake completes. Useful for diagnostics.
    proto_send_chunked: &'static str,
    proto_recv_chunked: &'static str,
}

impl Connection {
    pub fn connect(
        addr: &str,
        database: Option<&str>,
        user: Option<&str>,
        password: Option<&str>,
    ) -> io::Result<Connection> {
        let stream = TcpStream::connect(addr)?;
        let mut conn = Connection {
            inner: ChunkedStream::new(stream),
            database: database.map(String::from),
            user: user.map(String::from),
            password: password.map(String::from),
            // Client declares max supported protocol; negotiated down by server during handshake.
            protocol: Feature::V2_DYNAMIC_AND_JSON_SERIALIZATION.version() as u64,
            proto_send_chunked: "notchunked",
            proto_recv_chunked: "notchunked",
        };
        conn.handsake()?;
        Ok(conn)
    }

    fn handsake(&mut self) -> Result<()> {
        let ch = ClientHello {
            name: "toy-client".to_string(),
            version_major: 1,
            version_minor: 0,
            protocol_version: self.protocol,
            database: self.database.clone().unwrap_or("default".to_string()),
            user: self.user.clone().unwrap_or("default".to_string()),
            password: self.password.clone().unwrap_or("".to_string()),
        };

        ch.encode(&mut self.inner)?;
        self.inner.flush()?;
        match self.read_response()? {
            ServerResponse::Hello(sh) => {
                // Negotiate the protocol version — the smaller of what we
                // declared and what the server supports.
                self.protocol = u64::min(ch.protocol_version, sh.protocol_version);
                let negotiated = self.protocol as u32;

                // Chunked-protocol negotiation. We prefer chunked on both
                // directions but accept the server's wish via the _optional
                // suffix. The pairing is intentional: client SEND mode
                // negotiates against server RECV preference and vice versa.
                let (send_pref, recv_pref) = ("chunked_optional", "chunked_optional");
                let mut final_send = "notchunked";
                let mut final_recv = "notchunked";
                if Feature::CHUNKED_PROTOCOL.in_version(negotiated) {
                    let srv_recv = sh.proto_recv_chunked_srv.as_deref().unwrap_or("notchunked");
                    let srv_send = sh.proto_send_chunked_srv.as_deref().unwrap_or("notchunked");
                    final_send = chunked::negotiate(srv_recv, send_pref, "send")?;
                    final_recv = chunked::negotiate(srv_send, recv_pref, "recv")?;
                }

                // Send Addendum: quota_key (always at ADDENDUM+), then the
                // chunked preferences at v54470+. Still on the plain wire —
                // chunked framing only activates AFTER this is flushed.
                if Feature::ADDENDUM.in_version(negotiated) {
                    self.inner.write_string("")?; // quota_key
                    if Feature::CHUNKED_PROTOCOL.in_version(negotiated) {
                        self.inner.write_string(final_send)?;
                        self.inner.write_string(final_recv)?;
                    }
                    if Feature::VERSIONED_PARALLEL_REPLICAS_PROTOCOL.in_version(negotiated) {
                        // Send our supported parallel-replicas coordination
                        // version. Matches `DBMS_PARALLEL_REPLICAS_PROTOCOL_VERSION
                        // = 7` in ClickHouse/src/Core/ProtocolDefines.h. We
                        // don't actually participate in parallel-replica
                        // coordination from this client, but we must advertise
                        // a version so the server's protocol-version check
                        // succeeds.
                        self.inner.write_varuint(7)?;
                    }
                    self.inner.flush()?;
                }

                // Switch to chunked framing for the rest of the connection.
                if final_send == "chunked" {
                    self.inner.enable_write_chunked();
                }
                if final_recv == "chunked" {
                    self.inner.enable_read_chunked();
                }
                self.proto_send_chunked = final_send;
                self.proto_recv_chunked = final_recv;

                Ok(())
            }
            ServerResponse::Exception(e) => Err(Error::new(
                io::ErrorKind::InvalidData,
                format!("exception occurred {e:?}"),
            )),
            _ => Err(Error::new(
                io::ErrorKind::InvalidData,
                "expected ServerHello response but got unexpeced response",
            )),
        }
    }

    pub fn ping(&mut self) -> Result<()> {
        // according to the spec just send varuint(4) and expect varuint(4)
        // src/Client/Connection.cpp
        self.inner.write_varuint(ClientPacket::Ping as u64)?;
        self.inner.flush()?;
        match self.read_response()? {
            ServerResponse::Pong => Ok(()),
            ServerResponse::Exception(e) => Err(Error::new(
                io::ErrorKind::InvalidData,
                format!("exception occurred {e:?}"),
            )),
            _ => Err(Error::new(
                io::ErrorKind::InvalidData,
                "expected ServerHello response but got unexpeced response",
            )),
        }
    }

    pub fn query(&mut self, sql: &str) -> Result<QueryResult> {
        self.query_with(sql, QueryOptions::new())
    }

    pub fn query_with(&mut self, sql: &str, opts: QueryOptions) -> Result<QueryResult> {
        let protocol = self.protocol as u32;
        self.send_query_and_tables(sql, opts)?;
        self.read_query_response(protocol)
    }

    /// Internal: build and send a Query packet + the external tables block,
    /// followed by the empty-data terminator that signals "no more client
    /// input" (used for SELECTs and after INSERT body too).
    fn send_query_and_tables(&mut self, sql: &str, opts: QueryOptions) -> Result<()> {
        let protocol = self.protocol as u32;
        let query_id = opts
            .query_id
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

        let q = Query {
            query_id: query_id.clone(),
            client_info: ClientInfo {
                query_kind: QueryKind::InitialQuery,
                initial_user: self.user.clone().unwrap_or("default".to_string()),
                initial_query_id: query_id,
                initial_address: "127.0.0.1:0".to_string(),
                initial_time: Some(0),
                query_interface: 1, // TCP
                os_user: std::env::var("USER").unwrap_or_default(),
                client_hostname: hostname::get()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string(),
                client_name: "toy-client".to_string(),
                version_major: 1,
                version_minor: 0,
                protocol_version: self.protocol,
                quota_key: Some("".to_string()),
                distributed_depth: Some(0),
                version_patch: Some(0),
                collaborate_with_initiator: Some(false),
                obsolete_count_participating_replicas: Some(0),
                count_current_replicas: Some(0),
            },
            settings: inject_json_string_setting(opts.settings),
            cluster_secret: "".to_string(),
            stage: opts.stage.unwrap_or(Stage::Complete),
            compression: opts.compression.unwrap_or(false),
            body: sql.to_string(),
            params: opts.params,
            protocol_version: self.protocol,
        };

        q.encode(&mut self.inner)?;

        // External tables first (if any), then the empty Data marker that
        // tells the server "no more external tables".
        for table in &opts.external_tables {
            table.encode(&mut self.inner, protocol)?;
        }
        ExternalTable::encode_empty(&mut self.inner, protocol)?;
        self.inner.flush()?;
        Ok(())
    }

    /// Internal: drain the response stream until EndOfStream / Exception,
    /// returning a populated QueryResult. Used for SELECT-style queries.
    fn read_query_response(&mut self, _protocol: u32) -> Result<QueryResult> {

        let mut result = QueryResult::new();

        // Read response packets until EndOfStream or Exception.
        // See SPEC.md §6.4 for the full dispatch table.
        loop {
            match self.read_response()? {
                ServerResponse::Data(block) => {
                    if result.header.is_none() {
                        // First Data packet is the schema header (0 rows expected)
                        result.header = Some(block.into());
                    } else if block.rows > 0 || !block.columns.is_empty() {
                        // Result blocks. Skip truly empty boundary markers (0/0).
                        result.rows.push(block.into());
                    }
                }
                ServerResponse::Progress(_) => {
                    // Cumulative metrics — for now, ignore. Could aggregate.
                }
                ServerResponse::ProfileInfo(pi) => {
                    result.profile = Some(pi);
                }
                ServerResponse::Totals(block) => {
                    result.totals = Some(block.into());
                }
                ServerResponse::Extremes(block) => {
                    result.extremes = Some(block.into());
                }
                ServerResponse::Log(block) => {
                    // Multiple Log packets may arrive; last-write-wins for now.
                    result.logs = Some(block.into());
                }
                ServerResponse::ProfileEvents(block) => {
                    result.profile_events = Some(block.into());
                }
                ServerResponse::Exception(e) => {
                    return Err(Error::new(
                        io::ErrorKind::Other,
                        format!("query failed: {e:?}"),
                    ));
                }
                ServerResponse::EndOfStream => break,
                ServerResponse::TableColumns(_) => {
                    // Column defaults metadata — ignore for SELECT queries.
                }
                ServerResponse::TimezoneUpdate(_) => {
                    // v54464+: server announces session-default timezone
                    // change. We don't use the value yet (DateTime formatter
                    // is fixed to UTC) — decoded to keep stream alignment.
                }
                _ => {
                    return Err(Error::new(
                        io::ErrorKind::InvalidData,
                        "unexpected response to query",
                    ));
                }
            }
        }

        Ok(result)
    }

    /// Execute an INSERT with a single in-memory block of data.
    ///
    /// Flow (SPEC §6.5):
    /// 1. Send `Query("INSERT INTO ... VALUES")`.
    /// 2. Send the external-tables terminator.
    /// 3. Read a Data packet from the server — the **schema block** (0 rows,
    ///    columns describe what the server expects).
    /// 4. Send the user's data as one or more Data packets.
    /// 5. Send the empty-block terminator.
    /// 6. Drain response packets until EndOfStream / Exception.
    ///
    /// `block.columns` must match the server's expected column shape (names
    /// and types). The schema block returned in step 3 is consulted only to
    /// observe what the server expected; this implementation does not
    /// auto-translate or reorder columns. Mismatches are surfaced as a
    /// server-side Exception.
    pub fn insert(&mut self, sql: &str, block: proto::block::Block) -> Result<()> {
        self.insert_blocks(sql, vec![block])
    }

    /// Streaming INSERT: send `blocks` in order, then terminator, then drain
    /// the response. Empty `blocks` is allowed (just sends a terminator) — the
    /// server will accept and acknowledge as a no-op insert.
    pub fn insert_blocks(&mut self, sql: &str, blocks: Vec<proto::block::Block>) -> Result<()> {
        let protocol = self.protocol as u32;
        self.send_query_and_tables(sql, QueryOptions::new())?;

        // Step 3: drain metadata packets (TableColumns, Progress, ...) and
        // wait for the schema Data packet (rows = 0). The server may emit
        // these intermediate packets in any order before the schema block.
        loop {
            match self.read_response()? {
                ServerResponse::Data(_schema) => {
                    // Schema received — we don't currently consult it; the
                    // caller is responsible for matching column shapes.
                    break;
                }
                ServerResponse::Exception(e) => {
                    return Err(Error::new(
                        io::ErrorKind::Other,
                        format!("INSERT setup failed: {e:?}"),
                    ));
                }
                // Ignorable metadata that may precede the schema block.
                ServerResponse::TableColumns(_)
                | ServerResponse::Progress(_)
                | ServerResponse::ProfileInfo(_)
                | ServerResponse::Log(_)
                | ServerResponse::ProfileEvents(_)
                | ServerResponse::TimezoneUpdate(_) => {}
                other => {
                    return Err(Error::new(
                        io::ErrorKind::InvalidData,
                        format!("expected schema Data packet from server, got {other:?}"),
                    ));
                }
            }
        }

        // Step 4: send the user's blocks as Data packets.
        for block in &blocks {
            self.inner
                .write_varuint(proto::packet::ClientPacket::Data as u64)?;
            // Empty table_name — INSERT data isn't tied to an external table.
            self.inner.write_string("")?;
            block.encode(&mut self.inner, protocol)?;
        }

        // Step 5: empty-block terminator (signals end-of-input).
        self.inner
            .write_varuint(proto::packet::ClientPacket::Data as u64)?;
        self.inner.write_string("")?;
        proto::block::Block::new().encode(&mut self.inner, protocol)?;
        self.inner.flush()?;

        // Step 6: drain until EndOfStream / Exception.
        loop {
            match self.read_response()? {
                ServerResponse::EndOfStream => break,
                ServerResponse::Exception(e) => {
                    return Err(Error::new(
                        io::ErrorKind::Other,
                        format!("INSERT failed: {e:?}"),
                    ));
                }
                // Server may emit Progress / ProfileInfo / Log / etc. between
                // sending the body and EndOfStream. Drain quietly.
                ServerResponse::Progress(_)
                | ServerResponse::ProfileInfo(_)
                | ServerResponse::Log(_)
                | ServerResponse::ProfileEvents(_)
                | ServerResponse::TableColumns(_)
                | ServerResponse::TimezoneUpdate(_) => {}
                other => {
                    return Err(Error::new(
                        io::ErrorKind::InvalidData,
                        format!("unexpected response during INSERT: {other:?}"),
                    ));
                }
            }
        }

        Ok(())
    }

    fn read_response(&mut self) -> Result<ServerResponse> {
        let code_byte = self.inner.read_varuint()? as u8;
        let code = ServerPacket::try_from(code_byte)?;
        let protocol = self.protocol as u32;

        match code {
            ServerPacket::Hello => Ok(ServerResponse::Hello(ServerHello::decode(
                &mut self.inner,
                protocol,
            )?)),
            ServerPacket::Exception => Ok(ServerResponse::Exception(ServerException::decode(
                &mut self.inner,
            )?)),
            ServerPacket::Pong => Ok(ServerResponse::Pong),
            ServerPacket::Data => {
                let _table_name = self.inner.read_string()?;
                let b = proto::block::Block::decode(&mut self.inner, protocol)?;
                Ok(ServerResponse::Data(b))
            }
            ServerPacket::EndOfStream => Ok(ServerResponse::EndOfStream),
            ServerPacket::ProfileInfo => Ok(ServerResponse::ProfileInfo(ProfileInfo::decode(
                &mut self.inner,
                protocol,
            )?)),
            ServerPacket::Progress => Ok(ServerResponse::Progress(Progress::decode(
                &mut self.inner,
                protocol,
            )?)),
            ServerPacket::Totals => {
                let _table_name = self.inner.read_string()?;
                let b = proto::block::Block::decode(&mut self.inner, protocol)?;
                Ok(ServerResponse::Totals(b))
            }
            ServerPacket::Extremes => {
                let _table_name = self.inner.read_string()?;
                let b = proto::block::Block::decode(&mut self.inner, protocol)?;
                Ok(ServerResponse::Extremes(b))
            }
            ServerPacket::Log => {
                let _table_name = self.inner.read_string()?;
                let b = proto::block::Block::decode(&mut self.inner, protocol)?;
                Ok(ServerResponse::Log(b))
            }
            ServerPacket::ProfileEvents => {
                let _table_name = self.inner.read_string()?;
                let b = proto::block::Block::decode(&mut self.inner, protocol)?;
                Ok(ServerResponse::ProfileEvents(b))
            }
            ServerPacket::TableColumns => Ok(ServerResponse::TableColumns(TableColumns::decode(
                &mut self.inner,
            )?)),
            ServerPacket::TimezoneUpdate => {
                // v54464+: body is a single String — the new session
                // timezone. Sent when `SET session_timezone = '...'` mutates
                // the session-default tz during query execution. We decode
                // to keep stream alignment; the value isn't yet wired into
                // our DateTime formatter.
                let tz = self.inner.read_string()?;
                Ok(ServerResponse::TimezoneUpdate(tz))
            }
            _ => Err(Error::new(
                io::ErrorKind::InvalidData,
                format!("unhandled server packet type {code_byte}"),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    // -- Unit tests (no server needed) --

    #[test]
    fn test_handshake_encodes_client_hello() {
        // Verify ClientHello is well-formed by encoding to a buffer
        let ch = ClientHello {
            name: "toy-client".to_string(),
            version_major: 1,
            version_minor: 0,
            protocol_version: Feature::VERSION_PATCH.version() as u64,
            database: "default".to_string(),
            user: "default".to_string(),
            password: "".to_string(),
        };
        let mut buf = Vec::new();
        ch.encode(&mut buf).unwrap();

        let mut cursor = Cursor::new(buf.as_slice());
        let pkt = cursor.read_varuint().unwrap();
        assert_eq!(pkt, ClientPacket::Hello as u64);

        let decoded = ClientHello::decode(&mut cursor).unwrap();
        assert_eq!(decoded.name, "toy-client");
        assert_eq!(decoded.database, "default");
        assert_eq!(decoded.user, "default");
        assert_eq!(decoded.password, "");
        assert_eq!(
            decoded.protocol_version,
            Feature::VERSION_PATCH.version() as u64
        );
    }

    #[test]
    fn test_read_response_hello() {
        // Simulate a ServerHello response in a buffer
        let protocol = Feature::VERSION_PATCH.version();
        let sh = ServerHello {
            name: "ClickHouse".to_string(),
            version_major: 21,
            version_minor: 8,
            protocol_version: 54401,
            parallel_replicas_protocol_version: None,
            timezone: Some("UTC".to_string()),
            display_name: Some("test-server".to_string()),
            version_patch: Some(3),
            proto_send_chunked_srv: None,
            proto_recv_chunked_srv: None,
            password_complexity_rules: None,
            nonce: None,
        };
        let mut buf = Vec::new();
        sh.encode(&mut buf, protocol).unwrap();

        // Test the dispatch logic directly
        let mut cursor = Cursor::new(buf.as_slice());
        let code = ServerPacket::try_from(cursor.read_varuint().unwrap() as u8).unwrap();
        match code {
            ServerPacket::Hello => {
                let decoded = ServerHello::decode(&mut cursor, protocol).unwrap();
                assert_eq!(decoded.name, "ClickHouse");
                assert_eq!(decoded.version_major, 21);
                assert_eq!(decoded.timezone, Some("UTC".to_string()));
            }
            _ => panic!("expected Hello packet"),
        }
    }

    #[test]
    fn test_read_response_timezone_update() {
        // v54464: server emits packet type 17 + String timezone.
        // Hand-craft the bytes and feed through the dispatch via the
        // ServerPacket enum to verify the body decodes correctly.
        let mut buf = Vec::new();
        buf.write_varuint(ServerPacket::TimezoneUpdate as u64).unwrap();
        buf.write_string("Europe/Berlin").unwrap();

        let mut cursor = Cursor::new(buf.as_slice());
        let code = ServerPacket::try_from(cursor.read_varuint().unwrap() as u8).unwrap();
        assert_eq!(code as u8, 17);
        let tz = cursor.read_string().unwrap();
        assert_eq!(tz, "Europe/Berlin");
    }

    #[test]
    fn test_read_response_exception() {
        // Manually construct an exception packet on the wire:
        // varuint(2) + i32(13) + string("DB::Exception") + string("Unexpected packet") + string("") + bool(false)
        let mut buf = Vec::new();
        buf.write_varuint(ServerPacket::Exception as u64).unwrap();
        buf.write_i32(13).unwrap();
        buf.write_string("DB::Exception").unwrap();
        buf.write_string("Unexpected packet").unwrap();
        buf.write_string("").unwrap();
        buf.write_bool(false).unwrap();

        let mut cursor = Cursor::new(buf.as_slice());
        let code = ServerPacket::try_from(cursor.read_varuint().unwrap() as u8).unwrap();
        match code {
            ServerPacket::Exception => {
                let decoded = ServerException::decode(&mut cursor).unwrap();
                assert_eq!(decoded.code, 13);
                assert_eq!(decoded.name, "DB::Exception");
                assert_eq!(decoded.message, "Unexpected packet");
            }
            _ => panic!("expected Exception packet"),
        }
    }

    #[test]
    fn test_read_response_unknown_packet() {
        let mut buf = Vec::new();
        buf.write_varuint(255).unwrap(); // invalid packet type
        let mut cursor = Cursor::new(buf.as_slice());
        let result = ServerPacket::try_from(cursor.read_varuint().unwrap() as u8);
        assert!(result.is_err());
    }

    #[test]
    fn test_default_values() {
        // Verify defaults when None is passed
        let ch = ClientHello {
            name: "toy-client".to_string(),
            version_major: 1,
            version_minor: 0,
            protocol_version: 54401,
            database: None::<String>.unwrap_or("default".to_string()),
            user: None::<String>.unwrap_or("default".to_string()),
            password: None::<String>.unwrap_or("".to_string()),
        };
        assert_eq!(ch.database, "default");
        assert_eq!(ch.user, "default");
        assert_eq!(ch.password, "");
    }

    #[test]
    fn test_ping_packet_wire_format() {
        let mut buf = Vec::new();
        buf.write_varuint(ClientPacket::Ping as u64).unwrap();
        // Ping is just a single varuint byte with value 4
        assert_eq!(buf, vec![0x04]);
    }

    #[test]
    fn test_pong_packet_dispatch() {
        let mut buf = Vec::new();
        buf.write_varuint(ServerPacket::Pong as u64).unwrap();

        let mut cursor = Cursor::new(buf.as_slice());
        let code = ServerPacket::try_from(cursor.read_varuint().unwrap() as u8).unwrap();
        match code {
            ServerPacket::Pong => {} // expected
            _ => panic!("expected Pong packet"),
        }
    }

    #[test]
    fn test_pong_is_payload_less() {
        // Pong is just the packet type byte — no payload
        let mut buf = Vec::new();
        buf.write_varuint(ServerPacket::Pong as u64).unwrap();
        assert_eq!(buf.len(), 1);
        assert_eq!(buf[0], 0x04);
    }
}
