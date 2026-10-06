//! OpenAI-compatible embedding transport (the `remote-embedding` feature).
//!
//! An independent, versioned provider adapter: it imports no gateway
//! configuration, database stores, chat orchestration, or Home Lab paths.
//! Endpoint and model are operator-selected; the secret resolves only from
//! the environment variable named in the config.

use std::time::Duration;

use repoise_core::Result;
use repoise_core::embed::{EmbeddingBatch, EmbeddingProfile, EmbeddingProvider};
use repoise_core::error::Error;

use serde::Deserialize;

/// One operator-configured OpenAI-compatible embedding endpoint.
pub struct OpenAiCompatibleProvider {
    endpoint: String,
    api_key: String,
    model: String,
    dimension: u32,
    timeout: Duration,
}

#[derive(Deserialize)]
struct EmbeddingResponse {
    data: Vec<DataPoint>,
}

#[derive(Deserialize)]
struct DataPoint {
    embedding: Vec<f32>,
}

impl OpenAiCompatibleProvider {
    /// Validates the endpoint and constructs the provider.
    pub fn new(
        endpoint: &str,
        api_key: &str,
        model: &str,
        dimension: u32,
        timeout: Duration,
    ) -> Result<Self> {
        let endpoint = endpoint.trim().trim_end_matches('/').to_string();
        if !endpoint.starts_with("http://") && !endpoint.starts_with("https://") {
            return Err(Error::Provider(format!(
                "embedding endpoint must be an http(s) URL: {endpoint}"
            )));
        }
        Ok(Self {
            endpoint,
            api_key: api_key.to_string(),
            model: model.to_string(),
            dimension,
            timeout,
        })
    }

    fn request_url(&self) -> String {
        if self.endpoint.ends_with("/embeddings") {
            self.endpoint.clone()
        } else {
            format!("{}/embeddings", self.endpoint)
        }
    }
}

impl EmbeddingProvider for OpenAiCompatibleProvider {
    fn profile(&self) -> EmbeddingProfile {
        EmbeddingProfile {
            provider: "openai-compatible".to_string(),
            model: self.model.clone(),
            dimension: self.dimension,
            profile_version: repoise_core::embed::PROFILE_VERSION,
        }
    }

    fn embed(&self, inputs: &[String]) -> Result<EmbeddingBatch> {
        let config = ureq::config::Config::builder()
            .timeout_global(Some(self.timeout))
            .build();
        let agent = ureq::Agent::new_with_config(config);
        let response = agent
            .post(&self.request_url())
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .send_json(serde_json::json!({ "model": self.model, "input": inputs }))
            .map_err(|err| Error::Provider(format!("embedding request failed: {err}")))?;
        let status = response.status();
        let mut body = response.into_body();
        let text = body
            .read_to_string()
            .map_err(|err| Error::Provider(format!("embedding response unreadable: {err}")))?;
        if !status.is_success() {
            return Err(Error::Provider(format!(
                "embedding endpoint returned {status}: {text}"
            )));
        }
        let parsed: EmbeddingResponse = serde_json::from_str(&text)
            .map_err(|err| Error::Provider(format!("invalid embedding response: {err}")))?;
        if parsed.data.len() != inputs.len() {
            return Err(Error::Provider(format!(
                "embedding response returned {} vectors for {} inputs",
                parsed.data.len(),
                inputs.len()
            )));
        }
        Ok(EmbeddingBatch {
            vectors: parsed
                .data
                .into_iter()
                .map(|point| point.embedding)
                .collect(),
            model_revision: None,
        })
    }
}
