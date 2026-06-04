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
    /// Adds `script_query_number` and `script_line_number` VarUInts to
    /// ClientInfo. Used by clickhouse-client for multi-statement script
    /// error reporting. External clients send `0` for both.
    pub const QUERY_AND_LINE_NUMBERS: Feature = Feature(54475);
    /// Adds a JWT-presence byte (+ optional JWT String) at the tail of
    /// ClientInfo. External clients without JWT auth write a single 0 byte.
    pub const JWT_IN_INTERSERVER: Feature = Feature(54476);
    /// Adds a `VarUInt query_plan_serialization_version` at the tail of
    /// ServerHello. Inter-server only — external clients decode and ignore.
    pub const QUERY_PLAN_SERIALIZATION: Feature = Feature(54477);
    /// Server may emit columns wrapped in `ColumnBLOB` (compressed inline)
    /// when (a) compression is active on the query and (b) the block has
    /// > 1 row. Since our client never sets `compression = true` on outgoing
    /// queries, the server-side `convertColumnsToBLOBs` short-circuits and
    /// emits the regular dense form — no wire impact on us. Will need
    /// proper handling if/when compression integration lands (Problem 42/43).
    pub const PARALLEL_BLOCK_MARSHALLING: Feature = Feature(54478);
    pub const VERSIONED_CLUSTER_FUNCTION_PROTOCOL: Feature = Feature(54479);
    /// Adds field 3 (`out_of_order_buckets: Vec<Int32>`) to BlockInfo.
    /// Only emitted when the block carries delayed-bucket aggregation
    /// state from `ConvertingAggregatedToChunksTransform`. External clients
    /// rarely see non-empty values here.
    pub const OUT_OF_ORDER_BUCKETS_IN_AGGREGATION: Feature = Feature(54480);
    /// Server may wrap `Log` and `ProfileEvents` packet bodies in the
    /// compression frame at v54481+. The wrap activates only when the
    /// query has `compression = true` (`getCompressionCodec` returns a
    /// non-null codec). Our client always sets `compression = false`, so
    /// this code path stays inactive.
    pub const COMPRESSED_LOGS_PROFILE_EVENTS_COLUMNS: Feature = Feature(54481);
    /// Server may emit a column with `Kind::REPLICATED` (`kind_stack = 0x04`)
    /// for repeated-value columns. Below v54482 the writer expands those
    /// columns before sending; at v54482+ the wire data is the compact
    /// replicated form. Our decoder currently rejects this kind with a
    /// clear `Unsupported` error. Decoder implementation deferred.
    pub const REPLICATED_SERIALIZATION: Feature = Feature(54482);
    /// Composes sparse with `Nullable(T)`. Below v54483, the writer expanded
    /// sparse for Nullable columns before sending (`recursiveRemoveSparse`).
    /// At v54483+ the wire data is sparse-over-Nullable: the inner values
    /// stream is a Nullable column with the non-default values; positions
    /// not in the offset list default to NULL.
    pub const NULLABLE_SPARSE_SERIALIZATION: Feature = Feature(54483);
}

impl Feature {
    pub fn in_version(self, version: u32) -> bool {
        version >= self.0
    }

    pub fn version(self) -> u32 {
        self.0
    }
}
