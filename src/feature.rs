pub struct Feature(u32);

impl Feature {
    pub const TIMEZONE: Feature = Feature(54058);
    pub const DISPLAY_NAME: Feature = Feature(54372);
    pub const VERSION_PATCH: Feature = Feature(54401);
    pub const ADDENDUM: Feature = Feature(54458);
}

impl Feature {
    pub fn in_version(self, version: u32) -> bool {
        version >= self.0
    }

    pub fn version(self) -> u32 {
        self.0
    }
}
