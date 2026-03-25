use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Serialize, Deserialize)]
pub struct EngramConfig {
    pub mode: String, // "local", "external", "hosted"

    #[serde(default)]
    pub database: DatabaseConfig,

    #[serde(default)]
    pub docker: DockerConfig,

    #[serde(default)]
    pub embedding: EmbeddingConfig,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DatabaseConfig {
    pub url: String,
    pub schema: String,
    pub pool_size: u32,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DockerConfig {
    pub container_name: String,
    pub image: String,
    pub port: u16,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct EmbeddingConfig {
    pub model: String,
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            url: "postgres://postgres:postgres@localhost:5450/engram".to_string(),
            schema: "engram".to_string(),
            pool_size: 10,
        }
    }
}

impl Default for DockerConfig {
    fn default() -> Self {
        Self {
            container_name: "engram-db".to_string(),
            image: "pgvector/pgvector:pg16".to_string(),
            port: 5450,
        }
    }
}

impl Default for EmbeddingConfig {
    fn default() -> Self {
        Self {
            model: "all-MiniLM-L6-v2".to_string(),
        }
    }
}

impl Default for EngramConfig {
    fn default() -> Self {
        Self {
            mode: "local".to_string(),
            database: DatabaseConfig::default(),
            docker: DockerConfig::default(),
            embedding: EmbeddingConfig::default(),
        }
    }
}

impl EngramConfig {
    /// Load config with priority: env vars > config file > defaults
    pub fn load() -> Self {
        let config_path = Self::config_path();
        let mut config = if config_path.exists() {
            let content = std::fs::read_to_string(&config_path).unwrap_or_default();
            toml::from_str(&content).unwrap_or_default()
        } else {
            Self::default()
        };

        // Env var overrides
        if let Ok(url) = std::env::var("ENGRAM_DATABASE_URL")
            .or_else(|_| std::env::var("DATABASE_URL"))
        {
            config.database.url = url;
        }
        if let Ok(schema) = std::env::var("ENGRAM_SCHEMA") {
            config.database.schema = schema;
        }

        // Validate the schema name to prevent SQL injection via configuration.
        if let Err(e) = engram_core::config::validate_schema_name(&config.database.schema) {
            panic!("Invalid ENGRAM_SCHEMA: {}", e);
        }

        config
    }

    /// Path to config file: ~/.engram/config.toml
    pub fn config_path() -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".engram")
            .join("config.toml")
    }

    /// Write config to disk
    pub fn save(&self) -> anyhow::Result<()> {
        let path = Self::config_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let content = toml::to_string_pretty(self)?;
        std::fs::write(&path, &content)?;
        // Restrict access to the owner only - the file contains the database URL.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(())
    }
}
