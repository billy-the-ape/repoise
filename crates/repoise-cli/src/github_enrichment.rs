//! GitHub read-only enrichment transport (the `github-enrichment` feature).
//!
//! An independent host adapter: it verifies commit-to-change-request
//! associations (`GET /repos/{owner}/{repo}/commits/{sha}/pulls`) and fetches
//! bounded change-request data (`GET /repos/{owner}/{repo}/pulls/{number}`).
//! The token resolves only from the environment variable named in the
//! config; a missing token means hints stay unverified (local indexing is
//! unaffected). Rate limits stop the build's enrichment; permission denial
//! fails closed (the session invalidates the cached remote content).

use std::time::Duration;

use repoise_core::history::{
    EnrichmentError, EnrichmentOutcome, EnrichmentProvider, HostCache, PrAssociation, RequestBudget,
};

/// Transport timeout per request.
const TIMEOUT: Duration = Duration::from_secs(30);
/// Hints examined per item (each verification is a host request).
const MAX_HINTS_PER_ITEM: usize = 3;

/// GitHub read-only enrichment provider.
pub struct GitHubEnrichment {
    /// API base URL (operator-overridable for tests; never a token source).
    base: String,
    /// Read-only credential, when the named environment variable is set.
    token: Option<String>,
    /// Maximum fetched body characters kept per record.
    max_body_chars: u32,
}

/// One entry of the commit's PR list (`.../commits/{sha}/pulls`).
#[derive(serde::Deserialize)]
struct CommitPrEntry {
    number: u32,
}

/// The bounded fields of a change request (`.../pulls/{number}`).
#[derive(serde::Deserialize)]
struct PullRequest {
    title: Option<String>,
    body: Option<String>,
    state: Option<String>,
    html_url: Option<String>,
    updated_at: Option<String>,
}

/// One host response read fully (status plus headers we need plus body).
struct RawResponse {
    /// HTTP status code.
    status: u16,
    /// Full response body.
    body: String,
    /// `ETag` header, when the host sent one.
    etag: Option<String>,
}

impl GitHubEnrichment {
    /// Constructs the provider. The base URL must be an http(s) URL.
    pub fn new(base: &str, token: Option<String>, max_body_chars: u32) -> Self {
        let base = base.trim().trim_end_matches('/').to_string();
        Self {
            base,
            token,
            max_body_chars,
        }
    }

    /// Sends one GET request. A non-success status is a classified error,
    /// not a transport failure; ETag revalidation uses `If-None-Match`.
    /// `304` (not modified) is a success for cached revalidation.
    fn get(&self, url: &str, if_none_match: Option<&str>) -> Result<RawResponse, EnrichmentError> {
        let config = ureq::config::Config::builder()
            .timeout_global(Some(TIMEOUT))
            .http_status_as_error(false)
            .build();
        let mut request = ureq::Agent::new_with_config(config)
            .get(url)
            .header("User-Agent", "repoise")
            .header("Accept", "application/vnd.github+json");
        if let Some(token) = &self.token {
            request = request.header("Authorization", format!("Bearer {token}"));
        }
        if let Some(etag) = if_none_match {
            request = request.header("If-None-Match", etag);
        }
        let response = match request.call() {
            Ok(response) => response,
            Err(ureq::Error::StatusCode(code)) => return Err(classify_status(code, None)),
            Err(other) => {
                return Err(EnrichmentError::Other(format!(
                    "host request failed: {other:?}"
                )));
            }
        };
        let status_code = response.status();
        let status = status_code.as_u16();
        let headers = response.headers();
        let rate_remaining = headers
            .get("x-ratelimit-remaining")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let etag = headers
            .get("etag")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let body = response
            .into_body()
            .read_to_string()
            .map_err(|err| EnrichmentError::Other(format!("unreadable response: {err}")))?;
        if status_code.is_success() || status == 304 {
            Ok(RawResponse { status, body, etag })
        } else {
            Err(classify_status(status, rate_remaining.as_deref()))
        }
    }
}

/// Classifies a non-success status into an enrichment error. GitHub signals
/// rate limits with `403` plus `x-ratelimit-remaining: 0` (or `429`).
pub fn classify_status(code: u16, rate_limit_remaining: Option<&str>) -> EnrichmentError {
    match code {
        429 => EnrichmentError::RateLimited,
        403 if rate_limit_remaining == Some("0") => EnrichmentError::RateLimited,
        401 | 403 => EnrichmentError::PermissionDenied,
        404 => EnrichmentError::Other("host reported no such record".into()),
        other => EnrichmentError::Other(format!("host returned {other}")),
    }
}

impl GitHubEnrichment {
    /// Verifies each hint against the host and fetches bounded
    /// change-request data for the first verified association.
    fn verify_and_fetch(
        &self,
        owner: &str,
        repo: &str,
        sha: &str,
        hints: &[u32],
        cache: &HostCache,
        budget: &mut RequestBudget,
    ) -> Result<EnrichmentOutcome, EnrichmentError> {
        for &hint in hints.iter().take(MAX_HINTS_PER_ITEM) {
            if !budget.charge() {
                return Err(EnrichmentError::BudgetExhausted);
            }
            let url = format!("{}/repos/{owner}/{repo}/commits/{sha}/pulls", self.base);
            let response = self.get(&url, None)?;
            let list: Vec<CommitPrEntry> = match serde_json::from_str(&response.body) {
                Ok(list) => list,
                Err(err) => {
                    return Err(EnrichmentError::Other(format!(
                        "invalid host response: {err}"
                    )));
                }
            };
            if !list.iter().any(|entry| entry.number == hint) {
                continue;
            }
            // Verified: fetch bounded change-request data (ETag-cached).
            return self.fetch_pull_request(owner, repo, hint, cache, budget);
        }
        Ok(EnrichmentOutcome::NoAssociation)
    }

    /// Fetches one change request, revalidating through the session cache.
    fn fetch_pull_request(
        &self,
        owner: &str,
        repo: &str,
        number: u32,
        cache: &HostCache,
        budget: &mut RequestBudget,
    ) -> Result<EnrichmentOutcome, EnrichmentError> {
        let key = format!("pr:{owner}:{repo}:{number}");
        let cached = cache.get(&key);
        if !budget.charge() {
            return Err(EnrichmentError::BudgetExhausted);
        }
        let url = format!("{}/repos/{owner}/{repo}/pulls/{number}", self.base);
        let response = self.get(
            &url,
            cached.as_ref().and_then(|entry| entry.etag.as_deref()),
        )?;
        if response.status == 304
            && let Some(cached) = cached
        {
            return association_from_payload(
                &cached.payload,
                number,
                cached.fetched_at_ms,
                self.max_body_chars,
            )
            .map(EnrichmentOutcome::Verified);
        }
        let payload: serde_json::Value = match serde_json::from_str(&response.body) {
            Ok(payload) => payload,
            Err(err) => {
                return Err(EnrichmentError::Other(format!(
                    "invalid host response: {err}"
                )));
            }
        };
        let fetched_at_ms = repoise_core::indexing::now_ms();
        let _ = cache.put(
            &key,
            repoise_core::history::HostCacheEntry {
                etag: response.etag,
                fetched_at_ms,
                payload: payload.clone(),
            },
        );
        association_from_payload(&payload, number, fetched_at_ms, self.max_body_chars)
            .map(EnrichmentOutcome::Verified)
    }
}

/// Builds a verified association from a host change-request payload
/// (bounded body; the URL is only the validated host permalink).
pub fn association_from_payload(
    payload: &serde_json::Value,
    number: u32,
    fetched_at_ms: i64,
    max_body_chars: u32,
) -> Result<PrAssociation, EnrichmentError> {
    let pull: PullRequest = serde_json::from_value(payload.clone())
        .map_err(|err| EnrichmentError::Other(format!("invalid change request payload: {err}")))?;
    let body = pull
        .body
        .as_ref()
        .map(|body| bound_body(body, max_body_chars));
    let updated_at_ms = pull.updated_at.as_deref().and_then(parse_host_timestamp_ms);
    Ok(PrAssociation {
        host: "github".to_string(),
        number,
        title: pull.title,
        body,
        state: pull.state,
        url: pull.html_url,
        updated_at_ms,
        fetched_at_ms,
        verified: true,
    })
}

/// Parses an RFC-3339 host timestamp (for example `2026-10-07T12:00:00Z`)
/// into milliseconds since the Unix epoch.
fn parse_host_timestamp_ms(stamp: &str) -> Option<i64> {
    let (date, time) = stamp.split_once('T')?;
    let date_parts: Vec<Option<i64>> = date.split('-').map(|part| part.parse().ok()).collect();
    let time_parts: Vec<Option<i64>> = time
        .trim_end_matches('Z')
        .split(':')
        .take(3)
        .map(|part| part.parse().ok())
        .collect();
    if date_parts.len() != 3 || time_parts.len() != 3 {
        return None;
    }
    let (year, month, day) = (date_parts[0]?, date_parts[1]?, date_parts[2]?);
    let (hours, minutes, seconds) = (time_parts[0]?, time_parts[1]?, time_parts[2]?);
    // Days from the civil calendar (Howard Hinnant's algorithm).
    let y = year - if month <= 2 { 1 } else { 0 };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let m_adj = if month > 2 { month - 3 } else { month + 9 };
    let doy = (153 * m_adj + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    Some(days * 86_400_000 + hours * 3_600_000 + minutes * 60_000 + seconds * 1_000)
}

/// Truncates a fetched body to the configured bound (with a marker).
pub fn bound_body(body: &str, max_chars: u32) -> String {
    let count = body.chars().count() as u32;
    if count <= max_chars {
        return body.to_string();
    }
    let truncated: String = body.chars().take(max_chars as usize).collect();
    format!("{truncated}…")
}

impl EnrichmentProvider for GitHubEnrichment {
    fn host(&self) -> &'static str {
        "github"
    }

    fn remote_host(&self) -> &'static str {
        "github.com"
    }

    fn enrich(
        &self,
        remote: &str,
        revision_id: &str,
        hints: &[u32],
        cache: &HostCache,
        budget: &mut RequestBudget,
    ) -> Result<EnrichmentOutcome, EnrichmentError> {
        // Only a validated GitHub remote with a full Git revision qualifies.
        let (owner, repo) = remote
            .strip_prefix("github.com/")
            .and_then(|rest| rest.split_once('/'))
            .ok_or_else(|| {
                EnrichmentError::Other("remote is not a validated GitHub repo".into())
            })?;
        let sha = revision_id
            .strip_prefix("git:")
            .ok_or_else(|| EnrichmentError::Other("revision is not a Git revision".into()))?;
        if sha.len() != 40 || !sha.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(EnrichmentError::Other(
                "revision is not a full Git SHA".into(),
            ));
        }
        self.verify_and_fetch(owner, repo, sha, hints, cache, budget)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recorded_commit_pr_list_verifies_only_matching_numbers() {
        let list: Vec<CommitPrEntry> =
            serde_json::from_str(r#"[{"number": 12, "url": "x"}, {"number": 47, "url": "y"}]"#)
                .unwrap();
        assert!(list.iter().any(|entry| entry.number == 47));
        assert!(!list.iter().any(|entry| entry.number == 99));
    }

    #[test]
    fn recorded_pull_payload_builds_a_verified_association() {
        let payload: serde_json::Value = serde_json::json!({
            "title": "Add history lane",
            "body": "Intentionally long body ".repeat(2000),
            "state": "merged",
            "html_url": "https://github.com/o/r/pull/47"
        });
        let association = association_from_payload(&payload, 47, 1234, 100).unwrap();
        assert!(association.verified);
        assert_eq!(association.number, 47);
        assert_eq!(association.state.as_deref(), Some("merged"));
        assert_eq!(association.title.as_deref(), Some("Add history lane"));
        let body = association.body.expect("body present");
        assert!(body.ends_with('…'));
        assert!(body.chars().count() <= 101);
    }

    #[test]
    fn rate_limit_statuses_classify_as_rate_limited() {
        assert_eq!(classify_status(429, None), EnrichmentError::RateLimited);
        assert_eq!(
            classify_status(403, Some("0")),
            EnrichmentError::RateLimited
        );
    }

    #[test]
    fn permission_denials_classify_as_denied_not_limited() {
        assert_eq!(
            classify_status(401, Some("100")),
            EnrichmentError::PermissionDenied
        );
        assert_eq!(
            classify_status(403, Some("100")),
            EnrichmentError::PermissionDenied
        );
        assert_eq!(
            classify_status(403, None),
            EnrichmentError::PermissionDenied
        );
    }
}
