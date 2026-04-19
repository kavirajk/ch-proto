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
}

impl Feature {
    pub fn in_version(self, version: u32) -> bool {
        version >= self.0
    }

    pub fn version(self) -> u32 {
        self.0
    }
}
