use serde::Deserialize;

/// Validate a Postgres schema name to prevent SQL injection.
///
/// Rules:
/// - 1-63 characters (Postgres identifier limit)
/// - Only ASCII alphanumeric characters and underscores
/// - Must not start with a digit
pub fn validate_schema_name(name: &str) -> std::result::Result<(), String> {
    if name.is_empty() {
        return Err("schema name must not be empty".to_string());
    }
    if name.len() > 63 {
        return Err(format!(
            "schema name '{}' exceeds 63-character limit",
            name
        ));
    }
    let mut chars = name.chars();
    // Safety: we already checked non-empty above
    let first = chars.next().unwrap();
    if first.is_ascii_digit() {
        return Err(format!(
            "schema name '{}' must not start with a digit",
            name
        ));
    }
    for ch in std::iter::once(first).chain(chars) {
        if !ch.is_ascii_alphanumeric() && ch != '_' {
            return Err(format!(
                "schema name '{}' contains invalid character '{}' (only ASCII alphanumeric and underscore allowed)",
                name, ch
            ));
        }
    }
    Ok(())
}

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
