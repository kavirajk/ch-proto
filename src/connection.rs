use std::{
    io::{self, Error, Result, Write},
    net::TcpStream,
};

use crate::{
    exception::ServerException,
    feature::Feature,
    hello::ClientHello,
    packet::{ClientPacket, ServerPacket, ServerResponse},
};
use crate::{
    hello::ServerHello,
    proto::{ProtoRead, ProtoWrite},
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

        // Ok(())
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

    // pub fn query<T>(&mut self) -> Result<T> {
    //
    // }

    fn read_response(&mut self) -> Result<ServerResponse> {
        let code = ServerPacket::try_from(self.inner.read_varuint()? as u8)?;

        match code {
            ServerPacket::Hello => Ok(ServerResponse::Hello(ServerHello::decode(
                &mut self.inner,
                self.protocol as u32,
            )?)),
            ServerPacket::Exception => Ok(ServerResponse::Exception(ServerException::decode(
                &mut self.inner,
            )?)),
            ServerPacket::Pong => Ok(ServerResponse::Pong),
            _ => Err(Error::new(
                io::ErrorKind::InvalidData,
                "unhandled server packet type (yet) {code}",
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
