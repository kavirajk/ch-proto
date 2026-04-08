use std::io::{self, Error};

use crate::{exception::ServerException, hello::ServerHello};

pub enum ServerPacket {
    /// Name, version, revision.
    Hello = 0,
    /// A block of data (compressed or not).
    Data = 1,
    /// The exception during query execution.
    Exception = 2,
    /// Query execution progress: rows read, bytes read.
    Progress = 3,
    /// Ping response
    Pong = 4,
    /// All packets were transmitted
    EndOfStream = 5,
    /// Packet with profiling info.
    ProfileInfo = 6,
    /// A block with totals (compressed or not).
    Totals = 7,
    /// A block with minimums and maximums (compressed or not).
    Extremes = 8,
    /// A response to TablesStatus request.
    TablesStatusResponse = 9,
    /// System logs of the query execution
    Log = 10,
    /// Columns' description for default values calculation
    TableColumns = 11,
    /// List of unique parts ids.
    PartUUIDs = 12,
    /// String (UUID) describes a request for which next task is needed
    /// This is such an inverted logic, where server sends requests
    /// And client returns back response
    ReadTaskRequest = 13,
    /// Packet with profile events from server.
    ProfileEvents = 14,
    MergeTreeAllRangesAnnouncement = 15,
    /// Request from a MergeTree replica to a coordinator
    MergeTreeReadTaskRequest = 16,
    /// Receive server's (session-wide) default timezone
    TimezoneUpdate = 17,
    /// Return challenge for SSH signature signing
    SSHChallenge = 18,
}

impl TryFrom<u8> for ServerPacket {
    type Error = io::Error;
    fn try_from(value: u8) -> io::Result<Self> {
        match value {
            0 => Ok(ServerPacket::Hello),
            1 => Ok(ServerPacket::Data),
            2 => Ok(ServerPacket::Exception),
            3 => Ok(ServerPacket::Progress),
            4 => Ok(ServerPacket::Pong),
            5 => Ok(ServerPacket::EndOfStream),
            6 => Ok(ServerPacket::ProfileInfo),
            7 => Ok(ServerPacket::Totals),
            8 => Ok(ServerPacket::Extremes),
            9 => Ok(ServerPacket::TablesStatusResponse),
            10 => Ok(ServerPacket::Log),
            11 => Ok(ServerPacket::TableColumns),
            12 => Ok(ServerPacket::PartUUIDs),
            13 => Ok(ServerPacket::ReadTaskRequest),
            14 => Ok(ServerPacket::ProfileEvents),
            15 => Ok(ServerPacket::MergeTreeAllRangesAnnouncement),
            16 => Ok(ServerPacket::MergeTreeReadTaskRequest),
            17 => Ok(ServerPacket::TimezoneUpdate),
            18 => Ok(ServerPacket::SSHChallenge),
            _ => Err(Error::new(
                io::ErrorKind::InvalidData,
                format!("decoded unrecognized server response type {value}"),
            )),
        }
    }
}

pub enum ServerResponse {
    Hello(ServerHello),
    Exception(ServerException),
    Pong,
}

pub enum ClientPacket {
    /// Name, version, revision, default DB
    Hello = 0,
    /// Query id, query settings, stage up to which the query must be executed,
    /// whether the compression must be used,
    /// query text (without data for INSERTs).
    Query = 1,
    /// A block of data (compressed or not).
    Data = 2,
    /// Cancel the query execution.
    Cancel = 3,
    /// Check that connection to the server is alive.
    Ping = 4,
    /// Check status of tables on the server.
    TablesStatusRequest = 5,
    /// Keep the connection alive
    KeepAlive = 6,
    /// A block of data (compressed or not).
    Scalar = 7,
    /// List of unique parts ids to exclude from query processing
    IgnoredPartUUIDs = 8,
    /// A filename to read from s3 (used in s3Cluster)
    ReadTaskResponse = 9,
    /// Coordinator's decision with a modified set of mark ranges allowed to read
    MergeTreeReadTaskResponse = 10,
    /// Request SSH signature challenge
    SSHChallengeRequest = 11,
    /// Reply to SSH signature challenge
    SSHChallengeResponse = 12,
    /// Query plan
    QueryPlan = 13,
}
