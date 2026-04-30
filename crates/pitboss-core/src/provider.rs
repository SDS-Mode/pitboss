use std::borrow::Cow;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// LLM provider selected for a pitboss actor.
///
/// Goose is the dispatch mechanism for every provider; this enum is a typed
/// tag for Goose argv, cost lookup, health buckets, and reporting.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Provider {
    Anthropic,
    OpenAi,
    Google,
    Ollama,
    OpenRouter,
    Azure,
    Bedrock,
    Other(String),
}

impl Default for Provider {
    fn default() -> Self {
        Self::Anthropic
    }
}

impl Provider {
    /// String passed to `goose run --provider`.
    #[must_use]
    pub fn goose_arg(&self) -> Cow<'_, str> {
        match self {
            Self::Anthropic => Cow::Borrowed("anthropic"),
            Self::OpenAi => Cow::Borrowed("openai"),
            Self::Google => Cow::Borrowed("google"),
            Self::Ollama => Cow::Borrowed("ollama"),
            Self::OpenRouter => Cow::Borrowed("openrouter"),
            Self::Azure => Cow::Borrowed("azure"),
            Self::Bedrock => Cow::Borrowed("bedrock"),
            Self::Other(name) => Cow::Borrowed(name.as_str()),
        }
    }

    /// Stable reporting key for JSON summaries, cost tables, and health buckets.
    #[must_use]
    pub fn as_key(&self) -> String {
        match self {
            Self::Anthropic => "anthropic".to_string(),
            Self::OpenAi => "openai".to_string(),
            Self::Google => "google".to_string(),
            Self::Ollama => "ollama".to_string(),
            Self::OpenRouter => "openrouter".to_string(),
            Self::Azure => "azure".to_string(),
            Self::Bedrock => "bedrock".to_string(),
            Self::Other(name) => format!("other:{name}"),
        }
    }

    #[must_use]
    pub const fn is_dispatchable(&self) -> bool {
        true
    }
}

impl FromStr for Provider {
    type Err = String;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        let value = raw.trim();
        if value.is_empty() {
            return Err("provider cannot be empty".to_string());
        }
        Ok(match value.to_ascii_lowercase().as_str() {
            "anthropic" | "claude" => Self::Anthropic,
            "openai" => Self::OpenAi,
            "google" => Self::Google,
            "ollama" => Self::Ollama,
            "openrouter" => Self::OpenRouter,
            "azure" => Self::Azure,
            "bedrock" => Self::Bedrock,
            _ => Self::Other(value.to_ascii_lowercase()),
        })
    }
}

impl std::fmt::Display for Provider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.goose_arg())
    }
}

impl Serialize for Provider {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.goose_arg())
    }
}

impl<'de> Deserialize<'de> for Provider {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        Self::from_str(&raw).map_err(serde::de::Error::custom)
    }
}
