use std::io::Result;

use super::{
    packet::ServerPacket,
    wire::{ProtoRead, ProtoWrite},
};

#[derive(Debug)]
pub struct ServerException {
    pub code: i32,
    pub name: String,
    pub message: String,
    pub stack_trace: String,
    pub nested: bool,
}

impl ServerException {
    pub fn encode(&mut self, w: &mut impl ProtoWrite) -> Result<()> {
        w.write_varuint(ServerPacket::Exception as u64)?;
        w.write_i32(self.code)?;
        w.write_string(&self.name)?;
        w.write_string(&self.message)?;
        w.write_string(&self.stack_trace)?;
        w.write_bool(self.nested)?;
        Ok(())
    }

    pub fn decode(r: &mut impl ProtoRead) -> Result<ServerException> {
        Ok(ServerException {
            code: r.read_i32()?,
            name: r.read_string()?,
            message: r.read_string()?,
            stack_trace: r.read_string()?,
            nested: r.read_bool()?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn make_exception() -> ServerException {
        ServerException {
            code: 13,
            name: "DB::Exception".to_string(),
            message: "Unexpected packet from client".to_string(),
            stack_trace: "0. DB::Exception::Exception\n1. some_func".to_string(),
            nested: false,
        }
    }

    #[test]
    fn test_exception_roundtrip() {
        let mut exc = make_exception();
        let mut buf = Vec::new();
        exc.encode(&mut buf).unwrap();

        // skip packet type
        let mut cursor = Cursor::new(&buf[1..]);
        let decoded = ServerException::decode(&mut cursor).unwrap();

        assert_eq!(decoded.code, 13);
        assert_eq!(decoded.name, "DB::Exception");
        assert_eq!(decoded.message, "Unexpected packet from client");
        assert_eq!(decoded.stack_trace, "0. DB::Exception::Exception\n1. some_func");
        assert_eq!(decoded.nested, false);
    }

    #[test]
    fn test_exception_packet_type() {
        let mut exc = make_exception();
        let mut buf = Vec::new();
        exc.encode(&mut buf).unwrap();

        let mut cursor = Cursor::new(buf.as_slice());
        let pkt = cursor.read_varuint().unwrap();
        assert_eq!(pkt, ServerPacket::Exception as u64);
    }

    #[test]
    fn test_exception_negative_code() {
        let mut exc = ServerException {
            code: -1,
            name: "DB::Exception".to_string(),
            message: "error".to_string(),
            stack_trace: "".to_string(),
            nested: false,
        };
        let mut buf = Vec::new();
        exc.encode(&mut buf).unwrap();

        let mut cursor = Cursor::new(&buf[1..]);
        let decoded = ServerException::decode(&mut cursor).unwrap();
        assert_eq!(decoded.code, -1);
    }

    #[test]
    fn test_exception_nested_flag() {
        let mut exc = ServerException {
            code: 42,
            name: "DB::Exception".to_string(),
            message: "outer error".to_string(),
            stack_trace: "".to_string(),
            nested: true,
        };
        let mut buf = Vec::new();
        exc.encode(&mut buf).unwrap();

        let mut cursor = Cursor::new(&buf[1..]);
        let decoded = ServerException::decode(&mut cursor).unwrap();
        assert_eq!(decoded.nested, true);
    }

    #[test]
    fn test_exception_empty_fields() {
        let mut exc = ServerException {
            code: 0,
            name: "".to_string(),
            message: "".to_string(),
            stack_trace: "".to_string(),
            nested: false,
        };
        let mut buf = Vec::new();
        exc.encode(&mut buf).unwrap();

        let mut cursor = Cursor::new(&buf[1..]);
        let decoded = ServerException::decode(&mut cursor).unwrap();
        assert_eq!(decoded.code, 0);
        assert_eq!(decoded.name, "");
        assert_eq!(decoded.message, "");
        assert_eq!(decoded.stack_trace, "");
    }

    #[test]
    fn test_exception_wire_format() {
        let mut exc = ServerException {
            code: 13,
            name: "E".to_string(),
            message: "M".to_string(),
            stack_trace: "S".to_string(),
            nested: false,
        };
        let mut buf = Vec::new();
        exc.encode(&mut buf).unwrap();

        // packet type (varuint 2) + code (4 bytes LE) + 3 strings + bool
        let mut cursor = Cursor::new(buf.as_slice());
        assert_eq!(cursor.read_varuint().unwrap(), 2); // Exception packet type
        assert_eq!(cursor.read_i32().unwrap(), 13);    // code
        assert_eq!(cursor.read_string().unwrap(), "E"); // name
        assert_eq!(cursor.read_string().unwrap(), "M"); // message
        assert_eq!(cursor.read_string().unwrap(), "S"); // stack_trace
        assert_eq!(cursor.read_bool().unwrap(), false); // nested
    }
}
