use crate::proto::{
    external_table::ExternalTable,
    query::{Param, Setting, Stage},
};

// QueryOptions customizes a single query. All fields are optional and
// default to sensible values. Build with `QueryOptions::new()` and the
// `with_*` methods.
//
// Protocol-level customizations covered here:
// - query_id: override the auto-generated UUID
// - stage: how far the server should execute the query
// - compression: per-query compression flag
// - settings: per-query ClickHouse settings
// - params: bind values for parameterized queries
// - external_tables: temp tables attached to the query
#[derive(Debug, Default, Clone)]
pub struct QueryOptions {
    pub(crate) query_id: Option<String>,
    pub(crate) stage: Option<Stage>,
    pub(crate) compression: Option<bool>,
    pub(crate) settings: Vec<Setting>,
    pub(crate) params: Vec<Param>,
    pub(crate) external_tables: Vec<ExternalTable>,
}

impl QueryOptions {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_query_id(mut self, id: impl Into<String>) -> Self {
        self.query_id = Some(id.into());
        self
    }

    pub fn with_stage(mut self, stage: Stage) -> Self {
        self.stage = Some(stage);
        self
    }

    pub fn with_compression(mut self, enabled: bool) -> Self {
        self.compression = Some(enabled);
        self
    }

    pub fn with_setting(
        mut self,
        key: impl Into<String>,
        value: impl Into<String>,
    ) -> Self {
        self.settings.push(Setting {
            key: key.into(),
            value: value.into(),
            important: false,
            custom: false,
            obsolete: false,
        });
        self
    }

    /// Convenience for attaching a query parameter (`SELECT {name:Type}`).
    pub fn with_param(
        mut self,
        name: impl Into<String>,
        value: impl Into<String>,
    ) -> Self {
        self.params.push(Param {
            key: name.into(),
            value: value.into(),
        });
        self
    }

    pub fn with_external_table(mut self, table: ExternalTable) -> Self {
        self.external_tables.push(table);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_is_empty() {
        let o = QueryOptions::new();
        assert!(o.query_id.is_none());
        assert!(o.stage.is_none());
        assert!(o.compression.is_none());
        assert!(o.settings.is_empty());
        assert!(o.params.is_empty());
        assert!(o.external_tables.is_empty());
    }

    #[test]
    fn test_builder_chains() {
        let o = QueryOptions::new()
            .with_query_id("q-123")
            .with_stage(Stage::FetchColumns)
            .with_compression(true)
            .with_setting("max_threads", "4")
            .with_setting("max_memory_usage", "1000000")
            .with_param("x", "42");

        assert_eq!(o.query_id.as_deref(), Some("q-123"));
        assert!(matches!(o.stage, Some(Stage::FetchColumns)));
        assert_eq!(o.compression, Some(true));
        assert_eq!(o.settings.len(), 2);
        assert_eq!(o.settings[0].key, "max_threads");
        assert_eq!(o.settings[0].value, "4");
        assert_eq!(o.params.len(), 1);
        assert_eq!(o.params[0].key, "x");
        assert_eq!(o.params[0].value, "42");
        // Params are always encoded with the Custom flag set (§7.10)
        // but the builder doesn't set it — Param::encode does that.
    }

    #[test]
    fn test_setting_defaults_no_flags() {
        let o = QueryOptions::new().with_setting("max_threads", "4");
        assert!(!o.settings[0].important);
        assert!(!o.settings[0].custom);
        assert!(!o.settings[0].obsolete);
    }
}
