use std::time::Duration;

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub struct Config {
    pub epoch_change_delta: Duration,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            epoch_change_delta: Duration::from_secs(300),
        }
    }
}
