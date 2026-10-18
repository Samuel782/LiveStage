/// One DMX512 universe (slots 1..=512 stored at indices 0..512).
#[derive(Clone, PartialEq, Eq)]
pub struct Universe {
    pub data: [u8; 512],
}

impl Default for Universe {
    fn default() -> Self {
        Self { data: [0; 512] }
    }
}

impl std::fmt::Debug for Universe {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let used = self.data.iter().rposition(|&v| v != 0).map_or(0, |i| i + 1);
        write!(f, "Universe({:?}…)", &self.data[..used.min(16)])
    }
}

impl Universe {
    /// Sets a 1-based channel; out-of-range channels are ignored.
    pub fn set(&mut self, channel: u16, value: u8) {
        if (1..=512).contains(&channel) {
            self.data[channel as usize - 1] = value;
        }
    }
    pub fn get(&self, channel: u16) -> u8 {
        if (1..=512).contains(&channel) {
            self.data[channel as usize - 1]
        } else {
            0
        }
    }
}
