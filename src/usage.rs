use std::ops::AddAssign;

use chrono::{DateTime, Utc};
use serde::Serialize;

/// Token counts for one or more model responses.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Tokens {
    pub input: u64,
    pub output: u64,
    pub cache_write: u64,
    pub cache_read: u64,
}

impl Tokens {
    pub fn total(&self) -> u64 {
        self.input + self.output + self.cache_write + self.cache_read
    }
}

impl AddAssign for Tokens {
    fn add_assign(&mut self, other: Self) {
        self.input += other.input;
        self.output += other.output;
        self.cache_write += other.cache_write;
        self.cache_read += other.cache_read;
    }
}

/// One model response, as read from a provider's logs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    pub provider: &'static str,
    pub model: String,
    pub timestamp: DateTime<Utc>,
    pub tokens: Tokens,
}
