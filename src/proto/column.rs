// Column represents a single column in ClickHouse term.
pub struct Column {
    name: String,
    data_type: String,
    data: ColumnData,
}
// ColumnData is in-memory representation of a single column data in ClickHouse terms
// Every value has single type.
pub enum ColumnData {
    Uint8(Vec<u8>),
    String(Vec<String>),
}
