//! GitHub read-only enrichment transport (the `github-enrichment` feature).
//!
//! An independent host adapter: it verifies commit-to-change-request
//! associations (`GET /repos/{owner}/{repo}/commits/{sha}/pulls`, bounded
//! pages, first page ETag-cached through the session host cache) and fetches
//! bounded change-request data (`GET /repos/{owner}/{repo}/pulls/{number}`).
//! The token resolves only from the environment variable named in the
//! config; a missing or empty token disables enrichment entirely (no
//! unauthenticated requests are ever sent) and local indexing is unaffected.
//! Rate limits (primary or secondary) stop the build's enrichment;
//! permission denial fails closed (the session invalidates the cached remote
//! content). Note: a revoked token on a private repository typically yields
//! 404, which is classified as a per-item failure (`Other`), not a
//! permission denial.

use std::time::Duration;

use repoise_core::history::{
    EnrichmentError, EnrichmentOutcome, EnrichmentProvider, HostCache, PrAssociation, RequestBudget,
};

/// Transport timeout per request.
const TIMEOUT: Duration = Duration::from_secs(30);
/// Hints examined per item (each verification is a host request).
const MAX_HINTS_PER_ITEM: usize = 3;
/// Maximum pages walked for one commit's PR list (100 PRs per page).
const MAX_VERIFICATION_PAGES: u32 = 10;

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
#[derive(serde::Deserialize, serde::Serialize)]
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
    /// `Link` header's `rel="next"` page URL, when the host sent one.
    link_next: Option<String>,
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
            Err(ureq::Error::StatusCode(code)) => return Err(classify_status(code, None, None)),
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
        let retry_after = headers
            .get("retry-after")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let link_next = headers
            .get("link")
            .and_then(|value| value.to_str().ok())
            .and_then(link_next_url);
        let body = response
            .into_body()
            .read_to_string()
            .map_err(|err| EnrichmentError::Other(format!("unreadable response: {err}")))?;
        if status_code.is_success() || status == 304 {
            Ok(RawResponse {
                status,
                body,
                etag,
                link_next,
            })
        } else {
            Err(classify_status(
                status,
                rate_remaining.as_deref(),
                retry_after.as_deref(),
            ))
        }
    }
}

/// Extracts the `rel="next"` page URL from a `Link` header, if present.
fn link_next_url(header: &str) -> Option<String> {
    for segment in header.split(',') {
        let Some((raw_url, rest)) = segment.split_once(';') else {
            continue;
        };
        let rel_next = rest.split(';').any(|part| part.trim() == r#"rel="next""#);
        if rel_next {
            let url = raw_url.trim().trim_start_matches('<').trim_end_matches('>');
            if !url.is_empty() {
                return Some(url.to_string());
            }
        }
    }
    None
}

/// Classifies a non-success status into an enrichment error. GitHub signals
/// primary rate limits with `403` plus `x-ratelimit-remaining: 0` (or
/// `429`) and secondary rate limits with `403` (or `429`) plus a
/// `Retry-After` header; both stop the enrichment pass without wiping the
/// cached host content. A `403` without either signal is a permission
/// denial (fail closed).
pub fn classify_status(
    code: u16,
    rate_limit_remaining: Option<&str>,
    retry_after: Option<&str>,
) -> EnrichmentError {
    match code {
        429 => EnrichmentError::RateLimited,
        403 if rate_limit_remaining == Some("0") || retry_after.is_some() => {
            EnrichmentError::RateLimited
        }
        401 | 403 => EnrichmentError::PermissionDenied,
        // A revoked token on a private repository typically yields 404 here;
        // it is a per-item failure, not a revocation signal.
        404 => EnrichmentError::Other("host reported no such record".into()),
        other => EnrichmentError::Other(format!("host returned {other}")),
    }
}

impl GitHubEnrichment {
    /// Verifies the item's hints against the host and fetches bounded
    /// change-request data for the first verified association. The commit's
    /// PR list is fetched once per commit (bounded pages; the first page is
    /// ETag-cached through the session host cache) and every hint is matched
    /// against the accumulated list, so no request repeats per hint; the
    /// change request itself is fetched once, also ETag-cached.
    fn verify_and_fetch(
        &self,
        owner: &str,
        repo: &str,
        sha: &str,
        hints: &[u32],
        cache: &HostCache,
        budget: &mut RequestBudget,
    ) -> Result<EnrichmentOutcome, EnrichmentError> {
        let hints: Vec<u32> = hints.iter().take(MAX_HINTS_PER_ITEM).copied().collect();
        let first_page_key = format!("commit:{owner}:{repo}:{sha}:pulls:1");
        let mut next_url: Option<String> = Some(format!(
            "{}/repos/{owner}/{repo}/commits/{sha}/pulls?per_page=100",
            self.base
        ));
        let mut page = 0u32;
        let mut numbers: Vec<u32> = Vec::new();
        while let Some(url) = next_url {
            page += 1;
            if page > MAX_VERIFICATION_PAGES {
                break;
            }
            if !budget.charge() {
                return Err(EnrichmentError::BudgetExhausted);
            }
            // Only the first page is cached; pagination rarely repeats.
            let cached = if page == 1 {
                cache.get(&first_page_key)
            } else {
                None
            };
            let response = self.get(
                &url,
                cached.as_ref().and_then(|entry| entry.etag.as_deref()),
            )?;
            let list: Vec<CommitPrEntry> = if response.status == 304 {
                match cached.and_then(|entry| serde_json::from_value(entry.payload).ok()) {
                    Some(list) => list,
                    None => {
                        return Err(EnrichmentError::Other(
                            "cached commit PR list unreadable".into(),
                        ));
                    }
                }
            } else {
                match serde_json::from_str(&response.body) {
                    Ok(list) => list,
                    Err(err) => {
                        return Err(EnrichmentError::Other(format!(
                            "invalid host response: {err}"
                        )));
                    }
                }
            };
            if page == 1 {
                let _ = cache.put(
                    &first_page_key,
                    repoise_core::history::HostCacheEntry {
                        etag: response.etag.clone(),
                        fetched_at_ms: repoise_core::indexing::now_ms(),
                        payload: serde_json::to_value(&list).unwrap_or(serde_json::Value::Null),
                    },
                );
            }
            numbers.extend(list.iter().map(|entry| entry.number));
            // Stop paging as soon as one of the hints is verified.
            next_url = if hints.iter().any(|hint| numbers.contains(hint)) {
                None
            } else {
                response.link_next
            };
        }
        let verified = hints.iter().find(|hint| numbers.contains(hint)).copied();
        let Some(number) = verified else {
            return Ok(EnrichmentOutcome::NoAssociation);
        };
        self.fetch_pull_request(owner, repo, number, cache, budget)
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
        assert_eq!(
            classify_status(429, None, None),
            EnrichmentError::RateLimited
        );
        assert_eq!(
            classify_status(403, Some("0"), None),
            EnrichmentError::RateLimited
        );
        // Secondary rate limits: a retry-after header (with quota remaining)
        // is a limit, not a revocation.
        assert_eq!(
            classify_status(403, Some("100"), Some("12")),
            EnrichmentError::RateLimited
        );
        assert_eq!(
            classify_status(429, Some("100"), Some("12")),
            EnrichmentError::RateLimited
        );
    }

    #[test]
    fn permission_denials_classify_as_denied_not_limited() {
        assert_eq!(
            classify_status(401, Some("100"), None),
            EnrichmentError::PermissionDenied
        );
        assert_eq!(
            classify_status(403, Some("100"), None),
            EnrichmentError::PermissionDenied
        );
        assert_eq!(
            classify_status(403, None, None),
            EnrichmentError::PermissionDenied
        );
    }

    #[test]
    fn link_header_yields_the_next_page_url() {
        let link = r#"<https://api.github.com/x?page=2>; rel="next", <https://api.github.com/x?page=9>; rel="last""#;
        assert_eq!(
            link_next_url(link).as_deref(),
            Some("https://api.github.com/x?page=2")
        );
        assert_eq!(
            link_next_url(r#"<https://api.github.com/x>; rel="last""#),
            None
        );
    }

    fn http_response(status: &str, extra_headers: &str, body: &str) -> String {
        format!(
            "HTTP/1.1 {status}\r\n{extra_headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    /// Exercises the real HTTP flow (hint matching across pages, budget
    /// charging and 304 revalidation) against a local loopback fake host.
    #[test]
    fn enrichment_walks_pages_charges_budget_and_revalidates_with_304() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let next_page = format!("http://{addr}/repos/o/r/commits/a/pulls?per_page=100&page=2");
        let page1 = http_response(
            "200 OK",
            &format!("ETag: \"L1\"\r\nLink: <{next_page}>; rel=\"next\"\r\n"),
            r#"[{"number":12}]"#,
        );
        let page1_modified = http_response("304 Not Modified", "ETag: \"L1\"\r\n", "");
        let page2 = http_response("200 OK", "", r#"[{"number":47}]"#);
        let pull = http_response("200 OK", "", r#"{"title":"T","state":"open"}"#);
        let server = std::thread::spawn(move || {
            for stream in listener.incoming().take(6) {
                let Ok(mut stream) = stream else { continue };
                let mut buf = [0u8; 16384];
                let n = stream.read(&mut buf).unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..n]).to_string();
                let path = request.split_whitespace().nth(1).unwrap_or("");
                let response = if path.starts_with("/repos/o/r/commits/a/pulls?per_page=100&page=2")
                {
                    page2.clone()
                } else if path.starts_with("/repos/o/r/pulls/47") {
                    pull.clone()
                } else if request.contains(r#"If-None-Match: "L1""#) {
                    page1_modified.clone()
                } else {
                    page1.clone()
                };
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.flush();
            }
        });
        let dir = std::env::temp_dir().join(format!("repoise-fake-host-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let cache = HostCache::open(dir.join("host.json"), "github");
        let provider = GitHubEnrichment::new(&format!("http://{addr}"), Some("tok".into()), 100);
        // Hint 47 is only on page 2, so the walk must follow the Link header.
        let mut budget = RequestBudget { remaining: 5 };
        let outcome = provider
            .verify_and_fetch("o", "r", "a", &[47], &cache, &mut budget)
            .unwrap();
        assert!(matches!(outcome, EnrichmentOutcome::Verified(ref a) if a.number == 47));
        assert_eq!(budget.remaining, 2, "page1 + page2 + pull");
        // Second pass: the first page revalidates via 304 (cached payload).
        let mut budget2 = RequestBudget { remaining: 5 };
        let outcome2 = provider
            .verify_and_fetch("o", "r", "a", &[47], &cache, &mut budget2)
            .unwrap();
        assert!(matches!(outcome2, EnrichmentOutcome::Verified(ref a) if a.number == 47));
        assert_eq!(budget2.remaining, 2);
        drop(server);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
