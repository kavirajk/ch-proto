use crate::proto::{
    external_table::ExternalTable,
    query::{Param, Setting},
};

pub struct Options {
    params: Option<Vec<Param>>,
    settings: Option<Vec<Setting>>,
    // external_table only make sense for SELECT queries.
    // It's ignored for other queries
    external_table: Option<ExternalTable>,
}
