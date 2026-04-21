use crate::proto::{
    block,
    column::{Column, ColumnData},
};

// Block is basic data processing unit in ClickHouse
// It represents bunch of "columns" (of fixed row size)
#[derive(Debug)]
pub struct Block {
    pub columns: Vec<Column>,
    pub rows_count: usize,
}

impl Block {
    pub fn cols_count(&self) -> usize {
        self.columns.len()
    }
}

impl From<block::Block> for Block {
    fn from(value: block::Block) -> Self {
        Self {
            rows_count: value.rows,
            columns: value.columns,
        }
    }
}
