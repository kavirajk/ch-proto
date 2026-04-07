use std::io::Result;

use crate::{
    packet::ServerPacket,
    proto::{ProtoRead, ProtoWrite},
};

#[derive(Debug)]
pub struct ServerException {
    code: i32,
    name: String,
    message: String,
    stack_trace: String,
    nested: bool,
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
