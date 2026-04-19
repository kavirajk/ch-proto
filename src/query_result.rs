use crate::{block::Block, proto::profile::ProfileInfo};

// QueryResult bundles everything the server sends in response to a query.
//
// `header` is the schema block (always present on a successful query).
// `rows` holds the result blocks in order (each can contain many rows).
// Other fields are populated only if the server emitted them.
pub struct QueryResult {
    pub header: Option<Block>,
    pub rows: Vec<Block>,
    pub totals: Option<Block>,
    pub extremes: Option<Block>,
    pub profile: Option<ProfileInfo>,
    pub logs: Option<Block>,
    pub profile_events: Option<Block>,
}

impl QueryResult {
    pub fn new() -> Self {
        Self {
            header: None,
            rows: vec![],
            totals: None,
            extremes: None,
            profile: None,
            logs: None,
            profile_events: None,
        }
    }

    pub fn row_count(&self) -> usize {
        self.rows.iter().map(|b| b.rows_count).sum()
    }
}
