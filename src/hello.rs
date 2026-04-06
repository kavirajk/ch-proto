use crate::{
    feature::Feature,
    packet::{ClientPacket, ServerPacket},
};

use super::proto::{ProtoRead, ProtoWrite};
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
