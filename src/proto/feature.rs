pub struct Feature(u32);

impl Feature {
    pub const BLOCK_INFO: Feature = Feature(51903);
    pub const TIMEZONE: Feature = Feature(54058);
    pub const QUOTA_KEY_IN_CLIENT_INFO: Feature = Feature(54060);
    pub const DISPLAY_NAME: Feature = Feature(54372);
    pub const VERSION_PATCH: Feature = Feature(54401);
    pub const WRITE_CLIENT_INFO: Feature = Feature(54420);
    pub const SETTINGS_SERIALIZED_AS_STRINGS: Feature = Feature(54429);
    pub const INTERSERVER_SECRET: Feature = Feature(54441);
    pub const OPEN_TELEMETRY: Feature = Feature(54442);
    pub const DISTRIBUTED_DEPTH: Feature = Feature(54448);
    pub const INITIAL_QUERY_START_TIME: Feature = Feature(54449);
    pub const PARALLEL_REPLICAS: Feature = Feature(54453);
    pub const CUSTOM_SERIALIZATION: Feature = Feature(54454);
    pub const ADDENDUM: Feature = Feature(54458);
    pub const PARAMETERS: Feature = Feature(54459);
    pub const SERVER_QUERY_TIME_IN_PROGRESS: Feature = Feature(54460);
    pub const PASSWORD_COMPLEXITY_RULES: Feature = Feature(54461);
    pub const INTERSERVER_SECRET_V2: Feature = Feature(54462);
    pub const TOTAL_BYTES_IN_PROGRESS: Feature = Feature(54463);
    pub const TIMEZONE_UPDATES: Feature = Feature(54464);
    pub const SPARSE_SERIALIZATION: Feature = Feature(54465);
    pub const SSH_AUTHENTICATION: Feature = Feature(54466);
    /// Adds the `is_readonly` flag to `TablesStatusResponse`. No impact on
    /// external clients that don't use the `TablesStatusRequest` packet.
    pub const TABLE_READ_ONLY_CHECK: Feature = Feature(54467);
    /// Server populates `system.keywords` so clients can autocomplete SQL
    /// keywords. No wire change — purely a system-table addition.
    pub const SYSTEM_KEYWORDS_TABLE: Feature = Feature(54468);
    pub const ROWS_BEFORE_AGGREGATION: Feature = Feature(54469);
    pub const CHUNKED_PROTOCOL: Feature = Feature(54470);
    pub const VERSIONED_PARALLEL_REPLICAS_PROTOCOL: Feature = Feature(54471);
    /// Adds a `String external_roles` field to the Query packet body,
    /// positioned between the settings terminator and the interserver
    /// secret. External clients send an empty role list (one byte: VarUInt 0).
    pub const INTERSERVER_EXTERNALLY_GRANTED_ROLES: Feature = Feature(54472);
    /// V2 serialization of Dynamic and JSON column types. Wire impact is
    /// inside the column body's serialization-version prefix — gated to
    /// emit V2 instead of V1 for `Dynamic` and `JSON` types. We don't
    /// implement Dynamic / JSON Tier 2 yet (see NATIVE_FORMAT.md §3.4.5),
    /// so this is a docs-only entry. Bumping past it is safe because the
    /// column-body serialization version is the only place it surfaces and
    /// our decoder rejects unknown versions.
    pub const V2_DYNAMIC_AND_JSON_SERIALIZATION: Feature = Feature(54473);
    pub const SERVER_SETTINGS: Feature = Feature(54474);
}

impl Feature {
    pub fn in_version(self, version: u32) -> bool {
        version >= self.0
    }

    pub fn version(self) -> u32 {
        self.0
    }
}
