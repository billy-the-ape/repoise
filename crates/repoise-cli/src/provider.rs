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
    /// Position of this point in the request input (optional per spec).
    index: Option<usize>,
}

/// Orders the response data points back into request input order. When the
/// endpoint reports the optional `index` field, it is authoritative (points
/// without one keep their own position); otherwise the provider's return
/// order is assumed to match the input order.
fn order_by_index(points: Vec<DataPoint>) -> Vec<Vec<f32>> {
    let mut points = points;
    if points.iter().any(|point| point.index.is_some()) {
        for (position, point) in points.iter_mut().enumerate() {
            point.index.get_or_insert(position);
        }
        points.sort_by_key(|point| point.index.expect("index backfilled"));
    }
    points.into_iter().map(|point| point.embedding).collect()
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
            vectors: order_by_index(parsed.data),
            model_revision: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn response_points_ordered_by_reported_index() {
        let points = vec![
            DataPoint {
                embedding: vec![1.0],
                index: Some(1),
            },
            DataPoint {
                embedding: vec![2.0],
                index: Some(0),
            },
        ];
        let vectors = order_by_index(points);
        assert_eq!(vectors, vec![vec![2.0], vec![1.0]]);
    }

    #[test]
    fn response_points_without_index_keep_input_order() {
        let points = vec![
            DataPoint {
                embedding: vec![1.0],
                index: None,
            },
            DataPoint {
                embedding: vec![2.0],
                index: None,
            },
        ];
        let vectors = order_by_index(points);
        assert_eq!(vectors, vec![vec![1.0], vec![2.0]]);
    }
}
