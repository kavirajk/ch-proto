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
    pub const ROWS_BEFORE_AGGREGATION: Feature = Feature(54469);
}

impl Feature {
    pub fn in_version(self, version: u32) -> bool {
        version >= self.0
    }

    pub fn version(self) -> u32 {
        self.0
    }
}
