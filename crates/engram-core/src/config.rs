use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub database_url: String,
    #[serde(default = "default_pool_size")]
    pub pool_size: u32,
    #[serde(default = "default_schema")]
    pub schema: String,
}

fn default_pool_size() -> u32 {
    25
}

fn default_schema() -> String {
    "engram".to_string()
}

impl Config {
    pub fn from_env() -> Self {
        Self {
            database_url: std::env::var("ENGRAM_DATABASE_URL")
                .or_else(|_| std::env::var("DATABASE_URL"))
                .expect("ENGRAM_DATABASE_URL or DATABASE_URL must be set"),
            pool_size: std::env::var("ENGRAM_POOL_SIZE")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(25),
            schema: std::env::var("ENGRAM_SCHEMA")
                .unwrap_or_else(|_| "engram".to_string()),
        }
    }
}
