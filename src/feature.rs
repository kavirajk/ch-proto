pub struct Feature(u32);

impl Feature {
    pub const CLIENT_INFO: Feature = Feature(54032);
}

impl Feature {
    pub fn in_version(self, version: u32) -> bool {
        version >= self.0
    }
}
