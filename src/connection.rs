use std::{
    io::{self, Error, Result, Write},
    net::TcpStream,
};

use crate::{
    exception::ServerException,
    feature::Feature,
    hello::ClientHello,
    packet::{ServerPacket, ServerResponse},
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
            protocol: Feature::VERSION_PATCH.version() as u64,
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
                self.inner.write_string("")?;
                Ok(())
            }
            ServerResponse::Exception(e) => Err(Error::new(
                io::ErrorKind::InvalidData,
                format!("exception occurred {e:?}"),
            )),
        }

        // Ok(())
    }

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
    #[test]
    fn test_connect() {
        let conn = Connection::connect("127.0.0.1:9000", None, None, None).unwrap();
        println!("conn: {:?}", conn);
    }
}
