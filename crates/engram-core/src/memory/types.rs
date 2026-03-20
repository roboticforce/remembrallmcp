use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Core memory entry - a unit of organizational knowledge.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Memory {
    pub id: Uuid,
    pub content: String,
    pub summary: Option<String>,
    pub memory_type: MemoryType,
    pub source: Source,
    pub scope: Scope,
    pub tags: Vec<String>,
    pub metadata: serde_json::Value,
    pub importance: f32,
    pub access_count: i32,
    pub content_fingerprint: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub last_accessed_at: Option<DateTime<Utc>>,
    pub expires_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "text")]
pub enum MemoryType {
    Decision,
    Pattern,
    ErrorPattern,
    Preference,
    Outcome,
    CodeContext,
    Guideline,
    Incident,
    Architecture,
}

impl std::fmt::Display for MemoryType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Decision => write!(f, "decision"),
            Self::Pattern => write!(f, "pattern"),
            Self::ErrorPattern => write!(f, "error_pattern"),
            Self::Preference => write!(f, "preference"),
            Self::Outcome => write!(f, "outcome"),
            Self::CodeContext => write!(f, "code_context"),
            Self::Guideline => write!(f, "guideline"),
            Self::Incident => write!(f, "incident"),
            Self::Architecture => write!(f, "architecture"),
        }
    }
}

impl std::str::FromStr for MemoryType {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "decision" => Ok(Self::Decision),
            "pattern" => Ok(Self::Pattern),
            "error_pattern" => Ok(Self::ErrorPattern),
            "preference" => Ok(Self::Preference),
            "outcome" => Ok(Self::Outcome),
            "code_context" => Ok(Self::CodeContext),
            "guideline" => Ok(Self::Guideline),
            "incident" => Ok(Self::Incident),
            "architecture" => Ok(Self::Architecture),
            _ => Err(format!("Unknown memory type: {s}")),
        }
    }
}

/// Where this memory came from.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Source {
    pub system: String,       // "github", "slack", "confluence", "manual"
    pub identifier: String,   // PR URL, Slack thread ID, page URL, etc.
    pub author: Option<String>,
}

/// Visibility scope for access control.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Scope {
    pub organization: Option<String>,
    pub team: Option<String>,
    pub project: Option<String>,
}

/// Search query parameters.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryQuery {
    pub query: String,
    pub memory_types: Option<Vec<MemoryType>>,
    pub scope: Option<Scope>,
    pub tags: Option<Vec<String>>,
    pub limit: Option<i64>,
    pub min_similarity: Option<f32>,
}

/// Search result with relevance score.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemorySearchResult {
    pub memory: Memory,
    pub score: f32,
    pub match_type: MatchType,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MatchType {
    Semantic,
    FullText,
    Hybrid,
}

/// Input for creating a new memory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateMemory {
    pub content: String,
    pub summary: Option<String>,
    pub memory_type: MemoryType,
    pub source: Source,
    pub scope: Scope,
    pub tags: Vec<String>,
    pub metadata: Option<serde_json::Value>,
    pub importance: Option<f32>,
    pub expires_at: Option<DateTime<Utc>>,
}
