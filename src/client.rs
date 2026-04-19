use std::{
    io::{self, Error, Result, Write},
    net::TcpStream,
};

use crate::{
    block::Block,
    query_result::QueryResult,
    proto::{
        self,
        client_info::{ClientInfo, QueryKind},
        exception::ServerException,
        external_table::ExternalTable,
        feature::Feature,
        hello::{ClientHello, ServerHello},
        packet::{ClientPacket, ServerPacket, ServerResponse},
        profile::ProfileInfo,
        progress::Progress,
        query::{Query, Stage},
        table_columns::TableColumns,
        wire::{ProtoRead, ProtoWrite},
    },
};

#[derive(Debug)]
pub struct Connection {
    inner: TcpStream,
    database: Option<String>,
    user: Option<String>,
    password: Option<String>,

    protocol: u64,
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
            inner: stream,
            database: database.map(String::from),
            user: user.map(String::from),
            password: password.map(String::from),
            protocol: Feature::ADDENDUM.version() as u64,
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
                // negotiate the protocol version. Should be the minimum of server and client.
                self.protocol = u64::min(ch.protocol_version, sh.protocol_version);

                // send final ammendum message. Just an empty string (ClickHouse call it quota_key)
                if Feature::ADDENDUM.in_version(self.protocol as u32) {
                    self.inner.write_string("")?;
                    self.inner.flush()?;
                }

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
        let protocol = self.protocol as u32;
        let query_id = uuid::Uuid::new_v4().to_string();

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
            settings: vec![],
            cluster_secret: "".to_string(),
            stage: Stage::Complete,
            compression: false,
            body: sql.to_string(),
            params: vec![],
            protocol_version: self.protocol,
        };

        q.encode(&mut self.inner)?;

        // Send empty Data packet (marks end of client data)
        ExternalTable::encode_empty(&mut self.inner, protocol)?;
        self.inner.flush()?;

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
            timezone: Some("UTC".to_string()),
            display_name: Some("test-server".to_string()),
            version_patch: Some(3),
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
