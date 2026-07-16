//! Runtime skill catalog backed by ClawHub's public registry.
//!
//! Fetches skill listings from the ClawHub API (`/api/v1/search`) at runtime,
//! caching results in memory. No compile-time entries -- the catalog is always
//! up-to-date with the registry.
//!
//! Configuration:
//! - `CLAWHUB_REGISTRY` env var overrides the default base URL

use std::io::Read as _;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

/// Default ClawHub registry URL.
///
/// Points directly at the Convex backend, bypassing Vercel's edge which
/// rejects non-browser TLS fingerprints (JA3/JA4 filtering).
const DEFAULT_REGISTRY_URL: &str = "https://wry-manatee-359.convex.site";

/// How long cached search results remain valid (5 minutes).
const CACHE_TTL: Duration = Duration::from_secs(300);

/// Maximum number of results to return from a search.
const MAX_RESULTS: usize = 25;

/// HTTP request timeout for catalog queries.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Maximum compressed registry response accepted for a skill download.
const MAX_DOWNLOAD_BYTES: usize = 10 * 1024 * 1024;
/// Maximum number of files accepted from a registry skill archive.
const MAX_ARCHIVE_FILES: usize = 64;

/// Result of a catalog search, carrying both results and any error that occurred.
#[derive(Debug, Clone)]
pub struct CatalogSearchOutcome {
    /// Skill entries returned by the search (empty on error).
    pub results: Vec<CatalogEntry>,
    /// If the registry was unreachable or returned an error, a human-readable message.
    pub error: Option<String>,
}

/// A skill entry from the ClawHub catalog.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogEntry {
    /// Skill slug (unique identifier, e.g. "owner/skill-name").
    pub slug: String,
    /// Display name.
    pub name: String,
    /// Short description.
    #[serde(default)]
    pub description: String,
    /// Skill version (semver).
    #[serde(default)]
    pub version: String,
    /// Relevance score from the search API.
    #[serde(default)]
    pub score: f64,
    /// Last updated timestamp (epoch milliseconds from registry).
    #[serde(default)]
    pub updated_at: Option<u64>,
    /// Star count (populated via detail enrichment).
    #[serde(default)]
    pub stars: Option<u64>,
    /// Total download count (populated via detail enrichment).
    #[serde(default)]
    pub downloads: Option<u64>,
    /// Current install count (populated via detail enrichment).
    #[serde(default)]
    pub installs_current: Option<u64>,
    /// Owner handle (populated via detail enrichment).
    #[serde(default)]
    pub owner: Option<String>,
}

/// Top-level wrapper from the ClawHub `/api/v1/skills/{slug}` response.
///
/// The API returns `{"skill": {...}, "owner": {...}, "latestVersion": {...}}`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SkillDetailResponse {
    skill: SkillDetailInner,
    #[serde(default)]
    owner: Option<SkillOwner>,
    /// The latest published version (B-3: used for version/change detection).
    #[serde(default)]
    latest_version: Option<SkillLatestVersion>,
}

/// The `latestVersion` object from the ClawHub detail response (B-3).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillLatestVersion {
    pub version: Option<String>,
    #[serde(default)]
    pub created_at: Option<u64>,
    #[serde(default)]
    pub changelog: Option<String>,
}

/// Inner `skill` object within `SkillDetailResponse`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SkillDetailInner {
    pub slug: String,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub stats: Option<SkillStats>,
    #[serde(default)]
    pub updated_at: Option<u64>,
}

/// Detailed skill information from the ClawHub `/api/v1/skills/{slug}` endpoint.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillDetail {
    pub slug: String,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub stats: Option<SkillStats>,
    #[serde(default)]
    pub owner: Option<SkillOwner>,
    #[serde(default)]
    pub updated_at: Option<u64>,
}

/// Statistics for a skill from the ClawHub detail endpoint.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillStats {
    #[serde(default)]
    pub stars: Option<u64>,
    #[serde(default)]
    pub downloads: Option<u64>,
    #[serde(default)]
    pub installs_current: Option<u64>,
    #[serde(default)]
    pub installs_all_time: Option<u64>,
    #[serde(default)]
    pub versions: Option<u64>,
}

/// Owner information for a skill.
#[derive(Debug, Clone, Deserialize)]
pub struct SkillOwner {
    #[serde(default)]
    pub handle: Option<String>,
    #[serde(default, rename = "displayName")]
    pub display_name: Option<String>,
}

/// Cached search result with TTL.
struct CachedSearch {
    query: String,
    outcome: CatalogSearchOutcome,
    fetched_at: Instant,
}

/// Runtime skill catalog that queries ClawHub's API.
pub struct SkillCatalog {
    /// Base URL for the registry.
    registry_url: String,
    /// HTTP client (reused across requests).
    client: reqwest::Client,
    /// In-memory search cache keyed by query string.
    cache: RwLock<Vec<CachedSearch>>,
}

impl SkillCatalog {
    /// Create a new catalog.
    ///
    /// Reads `CLAWHUB_REGISTRY` (or legacy `CLAWDHUB_REGISTRY`) from the
    /// environment, falling back to the Convex backend.
    pub fn new() -> Self {
        let registry_url = std::env::var("CLAWHUB_REGISTRY")
            .or_else(|_| std::env::var("CLAWDHUB_REGISTRY"))
            .unwrap_or_else(|_| DEFAULT_REGISTRY_URL.to_string());

        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .user_agent(concat!("lunarwing/", env!("CARGO_PKG_VERSION")))
            .build()
            .unwrap_or_else(|e| {
                tracing::warn!("Failed to build HTTP client: {e}");
                reqwest::Client::default()
            });

        Self {
            registry_url,
            client,
            cache: RwLock::new(Vec::new()),
        }
    }

    /// Create a catalog with a custom registry URL (for testing).
    pub fn with_url(url: &str) -> Self {
        Self::with_url_and_timeout(url, REQUEST_TIMEOUT)
    }

    /// Create a catalog with a custom registry URL and timeout (for testing).
    pub fn with_url_and_timeout(url: &str, timeout: Duration) -> Self {
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .user_agent(concat!("lunarwing/", env!("CARGO_PKG_VERSION")))
            .build()
            .unwrap_or_else(|e| {
                tracing::warn!("Failed to build HTTP client: {e}");
                reqwest::Client::default()
            });

        Self {
            registry_url: url.to_string(),
            client,
            cache: RwLock::new(Vec::new()),
        }
    }

    /// Search for skills in the catalog.
    ///
    /// First checks the in-memory cache. If not cached or expired, fetches
    /// from the ClawHub API. Returns a [`CatalogSearchOutcome`] that carries
    /// both results and any error that occurred (catalog search is best-effort,
    /// never blocks the agent).
    pub async fn search(&self, query: &str) -> CatalogSearchOutcome {
        let query_lower = query.to_lowercase();

        // Check cache
        {
            let cache = self.cache.read().await;
            if let Some(cached) = cache.iter().find(|c| c.query == query_lower)
                && cached.fetched_at.elapsed() < CACHE_TTL
            {
                return cached.outcome.clone();
            }
        }

        // Fetch from API
        let outcome = self.fetch_search(&query_lower).await;

        // Update cache
        {
            let mut cache = self.cache.write().await;
            // Remove stale entry for this query
            cache.retain(|c| c.query != query_lower);
            // Limit cache size to prevent unbounded growth
            if cache.len() >= 50 {
                cache.remove(0);
            }
            cache.push(CachedSearch {
                query: query_lower,
                outcome: outcome.clone(),
                fetched_at: Instant::now(),
            });
        }

        outcome
    }

    /// Fetch search results from the ClawHub API.
    async fn fetch_search(&self, query: &str) -> CatalogSearchOutcome {
        let url = format!("{}/api/v1/search", self.registry_url);

        let response = match self.client.get(&url).query(&[("q", query)]).send().await {
            Ok(resp) => resp,
            Err(e) => {
                tracing::warn!("Catalog search failed (network): {}", e);
                return CatalogSearchOutcome {
                    results: Vec::new(),
                    error: Some("Registry unreachable".to_string()),
                };
            }
        };

        if !response.status().is_success() {
            let status = response.status();
            tracing::debug!(
                "Catalog search returned status {}: {}",
                status,
                response
                    .text()
                    .await
                    .unwrap_or_else(|_| "(no body)".to_string())
            );
            return CatalogSearchOutcome {
                results: Vec::new(),
                error: Some(format!("Registry returned status {status}")),
            };
        }

        // Parse the response body as text first so we can try multiple formats.
        let body = match response.text().await {
            Ok(b) => b,
            Err(e) => {
                tracing::debug!("Catalog search: failed to read response body: {}", e);
                return CatalogSearchOutcome {
                    results: Vec::new(),
                    error: Some("Failed to read registry response".to_string()),
                };
            }
        };

        // Try wrapped format first: {"results": [...]}
        // Then fall back to bare array: [...]
        let raw_results = if let Ok(envelope) = serde_json::from_str::<CatalogSearchEnvelope>(&body)
        {
            envelope.results
        } else if let Ok(arr) = serde_json::from_str::<Vec<CatalogSearchResult>>(&body) {
            arr
        } else {
            let preview = body.get(..200).unwrap_or(&body);
            tracing::debug!("Catalog search: failed to parse response: {}", preview);
            return CatalogSearchOutcome {
                results: Vec::new(),
                error: Some("Invalid response from registry".to_string()),
            };
        };

        CatalogSearchOutcome {
            results: raw_results
                .into_iter()
                .take(MAX_RESULTS)
                .map(|r| CatalogEntry {
                    slug: r.slug,
                    name: r.display_name.unwrap_or_default(),
                    description: r.summary.unwrap_or_default(),
                    version: r.version.unwrap_or_default(),
                    score: r.score.unwrap_or(0.0),
                    updated_at: r.updated_at,
                    stars: None,
                    downloads: None,
                    installs_current: None,
                    owner: None,
                })
                .collect(),
            error: None,
        }
    }

    /// Fetch detailed information for a single skill by slug.
    ///
    /// Calls `GET /api/v1/skills/{slug}` and returns the detail if available.
    /// Returns `None` on any network or parse error (best-effort).
    pub async fn fetch_skill_detail(&self, slug: &str) -> Option<SkillDetail> {
        let url = format!(
            "{}/api/v1/skills/{}",
            self.registry_url,
            urlencoding::encode(slug)
        );

        let response = self.client.get(&url).send().await.ok()?;
        if !response.status().is_success() {
            tracing::debug!(
                "Skill detail for '{}' returned status {}",
                slug,
                response.status()
            );
            return None;
        }

        let wrapper = response.json::<SkillDetailResponse>().await.ok()?;
        let inner = wrapper.skill;
        Some(SkillDetail {
            slug: inner.slug,
            display_name: inner.display_name,
            summary: inner.summary,
            // B-3: populate from latestVersion (previously dropped → always None).
            version: wrapper
                .latest_version
                .as_ref()
                .and_then(|v| v.version.clone()),
            stats: inner.stats,
            owner: wrapper.owner,
            updated_at: inner.updated_at,
        })
    }

    /// Enrich catalog entries with detail data (stars, downloads, owner).
    ///
    /// Fetches detail for up to `max` entries in parallel. Best-effort: entries
    /// that fail to enrich keep their `None` values.
    pub async fn enrich_search_results(&self, entries: &mut [CatalogEntry], max: usize) {
        let count = entries.len().min(max);
        if count == 0 {
            return;
        }

        let futures: Vec<_> = entries[..count]
            .iter()
            .map(|e| self.fetch_skill_detail(&e.slug))
            .collect();

        let details = futures::future::join_all(futures).await;

        for (entry, detail) in entries[..count].iter_mut().zip(details) {
            if let Some(detail) = detail {
                if let Some(ref stats) = detail.stats {
                    entry.stars = stats.stars;
                    entry.downloads = stats.downloads;
                    entry.installs_current = stats.installs_current;
                }
                if let Some(ref owner) = detail.owner {
                    entry.owner = owner.handle.clone().or_else(|| owner.display_name.clone());
                }
            }
        }
    }

    /// Get the registry base URL.
    pub fn registry_url(&self) -> &str {
        &self.registry_url
    }

    /// Clear the search cache.
    pub async fn clear_cache(&self) {
        self.cache.write().await.clear();
    }

    // ── B-3: cross-agent sharing (publish + update check) ─────────────────

    /// Publish a skill version to the registry (B-3).
    ///
    /// Posts a multipart form (`payload` JSON + `files` blobs) to
    /// `POST {registry_url}/api/v1/skills` with a `Bearer` token. Only
    /// proven, leak-free skills should reach here — the caller (bridge) is
    /// responsible for the publish-eligibility + leak-scan gates. Returns the
    /// registry-assigned skill/version IDs on success.
    pub async fn publish(
        &self,
        req: &PublishRequest,
        token: &str,
    ) -> Result<PublishResponse, CatalogError> {
        use reqwest::multipart;

        // Build the ClawHub `CliPublishRequest` payload (files list omits
        // size/sha256/storageId in multipart mode — the server computes them).
        let files_meta: Vec<serde_json::Value> = std::iter::once("SKILL.md")
            .chain(req.code_snippet_files.iter().map(|(name, _)| name.as_str()))
            .map(|path| serde_json::json!({ "path": path }))
            .collect();

        let payload = serde_json::json!({
            "slug": req.slug,
            "displayName": req.display_name,
            "version": req.version,
            "changelog": req.changelog,
            "tags": req.tags,
            "acceptLicenseTerms": true,
            "files": files_meta,
        });

        let mut form = multipart::Form::new().text("payload", payload.to_string());
        // SKILL.md body.
        form = form.part(
            "files",
            multipart::Part::text(req.skill_md_content.clone())
                .file_name("SKILL.md")
                .mime_str("text/markdown")
                .map_err(CatalogError::multipart)?,
        );
        // One part per code snippet (file_name = the snippet's module name).
        for (name, body) in &req.code_snippet_files {
            form = form.part(
                "files",
                multipart::Part::text(body.clone())
                    .file_name(name.clone())
                    .mime_str("text/x-python")
                    .map_err(CatalogError::multipart)?,
            );
        }

        let url = format!("{}/api/v1/skills", self.registry_url);
        let resp = self
            .client
            .post(&url)
            .bearer_auth(token)
            .multipart(form)
            .send()
            .await
            .map_err(|e| CatalogError::Publish(format!("publish request failed: {e}")))?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(CatalogError::Publish(format!(
                "registry returned {status}: {}",
                body.chars().take(500).collect::<String>()
            )));
        }

        let parsed = resp
            .json::<PublishResponse>()
            .await
            .map_err(|e| CatalogError::Publish(format!("parse publish response: {e}")))?;
        if !parsed.ok {
            return Err(CatalogError::Publish(
                "registry returned a non-success publish response".to_string(),
            ));
        }
        Ok(parsed)
    }

    /// Check whether a newer registry version exists for a slug (B-3 update
    /// detection). Hits `GET {registry_url}/api/v1/resolve?slug=...`. Returns
    /// `Some(UpdateInfo)` when the registry's latest version differs from
    /// `current_version`, `None` when up-to-date or the slug is unknown.
    pub async fn check_for_update(
        &self,
        slug: &str,
        current_version: &str,
        current_content_hash: Option<&str>,
    ) -> Result<Option<UpdateInfo>, CatalogError> {
        let url = format!("{}/api/v1/resolve", self.registry_url);
        let mut request = self.client.get(&url).query(&[("slug", slug)]);
        if let Some(hash) = current_content_hash.filter(|hash| !hash.is_empty()) {
            request = request.query(&[("hash", hash)]);
        }
        let resp = request
            .send()
            .await
            .map_err(|e| CatalogError::UpdateCheck(format!("resolve request failed: {e}")))?;

        let status = resp.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            return Ok(None); // slug unknown upstream — no update.
        }
        if !status.is_success() {
            return Err(CatalogError::UpdateCheck(format!(
                "resolve returned {status}"
            )));
        }

        let parsed: ResolveResponse = resp
            .json()
            .await
            .map_err(|e| CatalogError::UpdateCheck(format!("parse resolve response: {e}")))?;
        let Some(latest) = parsed.latest_version else {
            return Ok(None);
        };
        if latest.version.as_deref() == Some(current_version) {
            return Ok(None); // up to date.
        }
        Ok(Some(UpdateInfo {
            version: latest.version.unwrap_or_default(),
            registry_url: self.registry_url.clone(),
        }))
    }

    /// Download and decode a registry skill package. ClawHub returns a ZIP with
    /// `SKILL.md` and optional snippet files; compatible registries may return a
    /// plain UTF-8 `SKILL.md` body.
    pub async fn download(&self, slug: &str) -> Result<DownloadedSkill, CatalogError> {
        let url = skill_download_url(&self.registry_url, slug);
        let response = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| CatalogError::Download(format!("download request failed: {e}")))?;
        let status = response.status();
        if !status.is_success() {
            return Err(CatalogError::Download(format!(
                "registry returned {status}"
            )));
        }
        if response
            .content_length()
            .is_some_and(|size| size > MAX_DOWNLOAD_BYTES as u64)
        {
            return Err(CatalogError::Download(format!(
                "registry response exceeds {MAX_DOWNLOAD_BYTES} bytes"
            )));
        }
        let bytes = response
            .bytes()
            .await
            .map_err(|e| CatalogError::Download(format!("read download body: {e}")))?;
        if bytes.len() > MAX_DOWNLOAD_BYTES {
            return Err(CatalogError::Download(format!(
                "registry response exceeds {MAX_DOWNLOAD_BYTES} bytes"
            )));
        }
        decode_download(&bytes)
    }

    /// Scan skill content for leaked secrets before publishing (B-3
    /// defense-in-depth). Returns `Ok(())` if clean, `Err` with the match
    /// summary if any credential pattern is detected. The registry also scans
    /// server-side; this is the client-side pre-flight.
    pub fn leak_scan(content: &str) -> Result<(), CatalogError> {
        let detector = lunarwing_safety::LeakDetector::new();
        let result = detector.scan(content);
        if result.is_clean() {
            Ok(())
        } else {
            let summary = result
                .max_severity()
                .map(|s| format!("{s:?}"))
                .unwrap_or_else(|| "leak detected".to_string());
            Err(CatalogError::Leak(format!(
                "skill body failed leak scan ({summary}); refusing to publish"
            )))
        }
    }
}

/// Error from catalog publish/update operations (B-3).
#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
    #[error("publish failed: {0}")]
    Publish(String),
    #[error("update check failed: {0}")]
    UpdateCheck(String),
    #[error("download failed: {0}")]
    Download(String),
    #[error("leak detected: {0}")]
    Leak(String),
}

impl CatalogError {
    fn multipart(e: reqwest::Error) -> Self {
        CatalogError::Publish(format!("build multipart part: {e}"))
    }
}

/// A publish request (B-3). The caller gathers the skill's SKILL.md body and
/// any code-snippet files; the catalog builds the multipart form.
#[derive(Debug, Clone)]
pub struct PublishRequest {
    /// Target slug (e.g. "owner/skill-name").
    pub slug: String,
    /// Display name.
    pub display_name: String,
    /// Semver version string for this publish.
    pub version: String,
    /// Changelog for this version.
    pub changelog: String,
    /// Optional tags.
    pub tags: Vec<String>,
    /// The SKILL.md body.
    pub skill_md_content: String,
    /// Code-snippet files: (file_name, body) pairs (Python).
    pub code_snippet_files: Vec<(String, String)>,
}

/// The registry's response to a successful publish (B-3).
#[derive(Debug, Clone, Deserialize)]
pub struct PublishResponse {
    pub ok: bool,
    #[serde(rename = "skillId")]
    pub skill_id: String,
    #[serde(rename = "versionId")]
    pub version_id: String,
}

/// Info about a newer registry version available for update (B-3).
#[derive(Debug, Clone)]
pub struct UpdateInfo {
    pub version: String,
    pub registry_url: String,
}

/// Decoded files from a registry skill download.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadedSkill {
    pub skill_md_content: String,
    pub code_snippet_files: Vec<(String, String)>,
}

/// ClawHub `/api/v1/resolve` response shape.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ResolveResponse {
    #[serde(default)]
    latest_version: Option<ResolveVersion>,
}

#[derive(Debug, Deserialize)]
struct ResolveVersion {
    version: Option<String>,
}

fn decode_download(bytes: &[u8]) -> Result<DownloadedSkill, CatalogError> {
    if !bytes.starts_with(b"PK\x03\x04") {
        return Ok(DownloadedSkill {
            skill_md_content: decode_utf8_limited(bytes, crate::MAX_PROMPT_FILE_SIZE as usize)?,
            code_snippet_files: Vec::new(),
        });
    }

    let reader = std::io::Cursor::new(bytes);
    let mut archive = zip::ZipArchive::new(reader)
        .map_err(|e| CatalogError::Download(format!("invalid ZIP archive: {e}")))?;
    if archive.len() > MAX_ARCHIVE_FILES {
        return Err(CatalogError::Download(format!(
            "archive contains more than {MAX_ARCHIVE_FILES} files"
        )));
    }

    let mut skill_md_content = None;
    let mut code_snippet_files = Vec::new();
    for index in 0..archive.len() {
        let mut file = archive
            .by_index(index)
            .map_err(|e| CatalogError::Download(format!("read ZIP entry: {e}")))?;
        if file.is_dir() {
            continue;
        }
        let Some(path) = file.enclosed_name() else {
            return Err(CatalogError::Download(
                "archive contains an unsafe file path".to_string(),
            ));
        };
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if file_name == "SKILL.md" {
            skill_md_content = Some(read_zip_text(
                &mut file,
                crate::MAX_PROMPT_FILE_SIZE as usize,
                "SKILL.md",
            )?);
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("py") {
            let content =
                read_zip_text(&mut file, crate::MAX_PROMPT_FILE_SIZE as usize, file_name)?;
            code_snippet_files.push((file_name.to_string(), content));
        }
    }

    let skill_md_content = skill_md_content.ok_or_else(|| {
        CatalogError::Download("registry archive does not contain SKILL.md".to_string())
    })?;
    Ok(DownloadedSkill {
        skill_md_content,
        code_snippet_files,
    })
}

fn read_zip_text(
    reader: &mut impl std::io::Read,
    max_bytes: usize,
    label: &str,
) -> Result<String, CatalogError> {
    let mut bytes = Vec::new();
    reader
        .take(max_bytes as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| CatalogError::Download(format!("read {label}: {e}")))?;
    if bytes.len() > max_bytes {
        return Err(CatalogError::Download(format!(
            "{label} exceeds {max_bytes} bytes"
        )));
    }
    decode_utf8_limited(&bytes, max_bytes)
}

fn decode_utf8_limited(bytes: &[u8], max_bytes: usize) -> Result<String, CatalogError> {
    if bytes.len() > max_bytes {
        return Err(CatalogError::Download(format!(
            "skill content exceeds {max_bytes} bytes"
        )));
    }
    String::from_utf8(bytes.to_vec())
        .map_err(|e| CatalogError::Download(format!("skill content is not UTF-8: {e}")))
}

impl Default for SkillCatalog {
    fn default() -> Self {
        Self::new()
    }
}

/// Wrapper for ClawHub's `{"results": [...]}` envelope.
#[derive(Debug, Deserialize)]
struct CatalogSearchEnvelope {
    results: Vec<CatalogSearchResult>,
}

/// Internal type matching ClawHub's `/api/v1/search` response items.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CatalogSearchResult {
    slug: String,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    score: Option<f64>,
    #[serde(default)]
    updated_at: Option<u64>,
}

/// Construct the download URL for a skill's SKILL.md from the registry.
///
/// The slug is URL-encoded to prevent query string injection via special
/// characters like `&` or `#`.
pub fn skill_download_url(registry_url: &str, slug: &str) -> String {
    format!(
        "{}/api/v1/download?slug={}",
        registry_url,
        urlencoding::encode(slug)
    )
}

/// Convenience wrapper for creating a shared catalog.
pub fn shared_catalog() -> Arc<SkillCatalog> {
    Arc::new(SkillCatalog::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    async fn serve_once(
        content_type: &str,
        response_body: Vec<u8>,
    ) -> (String, tokio::sync::oneshot::Receiver<Vec<u8>>) {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test server");
        let address = listener.local_addr().expect("test server address");
        let (request_tx, request_rx) = tokio::sync::oneshot::channel();
        let content_type = content_type.to_string();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept request");
            let mut request = Vec::new();
            let mut buffer = [0_u8; 4096];
            let expected_len = loop {
                let read = socket.read(&mut buffer).await.expect("read request");
                assert!(read > 0, "request ended before headers");
                request.extend_from_slice(&buffer[..read]);
                if let Some(header_end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&request[..header_end]);
                    let content_length = headers
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().ok())
                                .flatten()
                        })
                        .unwrap_or(0);
                    break header_end + 4 + content_length;
                }
            };
            while request.len() < expected_len {
                let read = socket.read(&mut buffer).await.expect("read request body");
                assert!(read > 0, "request body ended early");
                request.extend_from_slice(&buffer[..read]);
            }
            let _ = request_tx.send(request);

            let response_head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                response_body.len()
            );
            socket
                .write_all(response_head.as_bytes())
                .await
                .expect("write response headers");
            socket
                .write_all(&response_body)
                .await
                .expect("write response body");
        });
        (format!("http://{address}"), request_rx)
    }

    fn make_skill_zip(skill_md: &str, snippet: &str) -> Vec<u8> {
        let cursor = std::io::Cursor::new(Vec::new());
        let mut writer = zip::ZipWriter::new(cursor);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        writer
            .start_file("SKILL.md", options)
            .expect("start SKILL.md");
        writer
            .write_all(skill_md.as_bytes())
            .expect("write SKILL.md");
        writer
            .start_file("helpers.py", options)
            .expect("start snippet");
        writer.write_all(snippet.as_bytes()).expect("write snippet");
        writer.finish().expect("finish ZIP").into_inner()
    }

    #[test]
    fn test_default_registry_url() {
        // When CLAWHUB_REGISTRY is not set, should use default
        let catalog = SkillCatalog::with_url(DEFAULT_REGISTRY_URL);
        assert_eq!(catalog.registry_url(), DEFAULT_REGISTRY_URL);
    }

    #[test]
    fn test_custom_registry_url() {
        let catalog = SkillCatalog::with_url("https://custom.registry.example");
        assert_eq!(catalog.registry_url(), "https://custom.registry.example");
    }

    #[tokio::test]
    async fn test_publish_contract_sends_authenticated_multipart() {
        let response = br#"{"ok":true,"skillId":"skill-1","versionId":"version-1"}"#.to_vec();
        let (url, request_rx) = serve_once("application/json", response).await;
        let catalog = SkillCatalog::with_url(&url);
        let request = PublishRequest {
            slug: "owner/example".to_string(),
            display_name: "Example".to_string(),
            version: "1.2.3".to_string(),
            changelog: "Fix behavior".to_string(),
            tags: vec!["utility".to_string()],
            skill_md_content: "---\nname: example\n---\n\nDo the thing.\n".to_string(),
            code_snippet_files: vec![(
                "helpers.py".to_string(),
                "def helper():\n    return 1\n".to_string(),
            )],
        };

        let published = catalog
            .publish(&request, "test-token")
            .await
            .expect("publish succeeds");
        let raw_request = request_rx.await.expect("captured request");
        let request_text = String::from_utf8_lossy(&raw_request);

        assert_eq!(published.skill_id, "skill-1");
        assert!(request_text.starts_with("POST /api/v1/skills HTTP/1.1\r\n"));
        assert!(request_text.contains("authorization: Bearer test-token\r\n"));
        assert!(request_text.contains("name=\"payload\""));
        assert!(request_text.contains("\"slug\":\"owner/example\""));
        assert!(request_text.contains("filename=\"SKILL.md\""));
        assert!(request_text.contains("filename=\"helpers.py\""));
    }

    #[tokio::test]
    async fn test_update_check_contract_sends_slug_and_hash() {
        let response = br#"{"latestVersion":{"version":"2.0.0"}}"#.to_vec();
        let (url, request_rx) = serve_once("application/json", response).await;
        let catalog = SkillCatalog::with_url(&url);

        let update = catalog
            .check_for_update("owner/example", "1.0.0", Some("sha256:old"))
            .await
            .expect("update check succeeds")
            .expect("new version returned");
        let raw_request = request_rx.await.expect("captured request");
        let request_text = String::from_utf8_lossy(&raw_request);

        assert_eq!(update.version, "2.0.0");
        assert!(request_text.starts_with("GET /api/v1/resolve?"));
        assert!(request_text.contains("slug=owner%2Fexample"));
        assert!(request_text.contains("hash=sha256%3Aold"));
    }

    #[tokio::test]
    async fn test_download_contract_decodes_skill_and_snippets() {
        let skill_md = "---\nname: example\nversion: 2.0.0\n---\n\nUpdated prompt.\n";
        let snippet = "def helper():\n    return 2\n";
        let response = make_skill_zip(skill_md, snippet);
        let (url, request_rx) = serve_once("application/zip", response).await;
        let catalog = SkillCatalog::with_url(&url);

        let downloaded = catalog
            .download("owner/example")
            .await
            .expect("download succeeds");
        let raw_request = request_rx.await.expect("captured request");
        let request_text = String::from_utf8_lossy(&raw_request);

        assert!(request_text.starts_with("GET /api/v1/download?slug=owner%2Fexample"));
        assert_eq!(downloaded.skill_md_content, skill_md);
        assert_eq!(
            downloaded.code_snippet_files,
            vec![("helpers.py".to_string(), snippet.to_string())]
        );
    }

    #[tokio::test]
    async fn test_search_returns_error_on_network_failure() {
        // Use RFC 5737 TEST-NET-1 (192.0.2.0/24) for reliable failure even behind proxies.
        // Short timeout so the test doesn't block for the full 10s REQUEST_TIMEOUT.
        let catalog =
            SkillCatalog::with_url_and_timeout("http://192.0.2.1:9999", Duration::from_secs(1));
        let outcome = catalog.search("test").await;
        assert!(outcome.results.is_empty());
        assert!(outcome.error.is_some());
        let error = outcome.error.unwrap();
        assert!(
            error.contains("Registry unreachable")
                || error.contains("connect")
                || error.contains("502")
                || error.contains("503")
                || error.contains("504"),
            "Expected connection or gateway error, got: {error}",
        );
    }

    #[tokio::test]
    async fn test_cache_is_populated_after_search() {
        let catalog = SkillCatalog::with_url("http://127.0.0.1:1");

        // First search populates cache (even with empty results)
        catalog.search("cached-query").await;

        let cache = catalog.cache.read().await;
        assert!(cache.iter().any(|c| c.query == "cached-query"));
    }

    #[tokio::test]
    async fn test_clear_cache() {
        let catalog = SkillCatalog::with_url("http://127.0.0.1:1");
        catalog.search("something").await;

        catalog.clear_cache().await;
        let cache = catalog.cache.read().await;
        assert!(cache.is_empty());
    }

    #[test]
    fn test_skill_download_url() {
        let url = skill_download_url("https://clawhub.ai", "owner/my-skill");
        assert_eq!(
            url,
            "https://clawhub.ai/api/v1/download?slug=owner%2Fmy-skill"
        );
    }

    #[test]
    fn test_skill_download_url_encodes_special_chars() {
        let url = skill_download_url("https://clawhub.ai", "foo&bar=baz#frag");
        assert!(url.contains("slug=foo%26bar%3Dbaz%23frag"));
    }

    #[test]
    fn test_parse_wrapped_response() {
        // ClawHub returns {"results": [...]} format
        let json = r#"{"results":[{"slug":"markdown","displayName":"Markdown","summary":"A skill","version":"1.0.0","score":3.5}]}"#;
        let envelope: CatalogSearchEnvelope = serde_json::from_str(json).unwrap();
        assert_eq!(envelope.results.len(), 1);
        assert_eq!(envelope.results[0].slug, "markdown");
        assert_eq!(
            envelope.results[0].display_name.as_deref(),
            Some("Markdown")
        );
    }

    #[test]
    fn test_parse_bare_array_response() {
        // Fallback: bare array format
        let json = r#"[{"slug":"markdown","displayName":"Markdown","summary":"A skill","version":"1.0.0","score":3.5}]"#;
        let results: Vec<CatalogSearchResult> = serde_json::from_str(json).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].slug, "markdown");
    }

    #[test]
    fn test_parse_skill_detail() {
        // Response format matches the actual ClawHub API: {"skill": {...}, "owner": {...}}
        let json = r#"{
            "skill": {
                "slug": "steipete/markdown-writer",
                "displayName": "Markdown Writer",
                "summary": "Write markdown docs",
                "stats": {
                    "stars": 142,
                    "downloads": 8400,
                    "installsCurrent": 55,
                    "installsAllTime": 200,
                    "versions": 5
                },
                "updatedAt": 1700000000000
            },
            "owner": {
                "handle": "steipete",
                "displayName": "Peter S."
            },
            "latestVersion": {
                "version": "1.2.3",
                "createdAt": 1700000000000,
                "changelog": ""
            }
        }"#;

        let wrapper: SkillDetailResponse = serde_json::from_str(json).unwrap();
        let inner = &wrapper.skill;
        assert_eq!(inner.slug, "steipete/markdown-writer");
        assert_eq!(inner.display_name.as_deref(), Some("Markdown Writer"));

        let stats = inner.stats.as_ref().unwrap();
        assert_eq!(stats.stars, Some(142));
        assert_eq!(stats.downloads, Some(8400));
        assert_eq!(stats.installs_current, Some(55));

        let owner = wrapper.owner.as_ref().unwrap();
        assert_eq!(owner.handle.as_deref(), Some("steipete"));
    }

    #[tokio::test]
    async fn test_fetch_skill_detail_returns_none_on_error() {
        let catalog = SkillCatalog::with_url("http://127.0.0.1:1");
        let result = catalog.fetch_skill_detail("nonexistent/skill").await;
        assert!(result.is_none());
    }

    #[test]
    fn test_catalog_entry_serde() {
        let entry = CatalogEntry {
            slug: "test/skill".to_string(),
            name: "Test Skill".to_string(),
            description: "A test".to_string(),
            version: "1.0.0".to_string(),
            score: 0.95,
            updated_at: Some(1700000000000),
            stars: Some(42),
            downloads: Some(1000),
            installs_current: None,
            owner: Some("tester".to_string()),
        };
        let json = serde_json::to_string(&entry).unwrap();
        let parsed: CatalogEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.slug, "test/skill");
        assert_eq!(parsed.name, "Test Skill");
    }

    #[test]
    fn test_leak_scan_refuses_credential_in_skill_body() {
        // A skill body containing an obvious API-key pattern must be refused.
        let body = "export OPENAI_API_KEY=\"sk-proj-abcdef1234567890ABCDEF1234567890abcdefAB\"";
        assert!(SkillCatalog::leak_scan(body).is_err());
    }

    #[test]
    fn test_leak_scan_accepts_clean_skill_body() {
        // A normal skill body with no credential patterns passes.
        let body = "---\nname: example\ndescription: an example skill\n---\n\nDo the thing.\n";
        assert!(SkillCatalog::leak_scan(body).is_ok());
    }

    #[test]
    fn test_skill_latest_version_populates_detail_version() {
        // The detail endpoint's latestVersion.version should populate
        // SkillDetail.version (previously dropped → always None).
        let json = r#"{
            "skill": {"slug": "owner/skill", "displayName": "Skill"},
            "owner": {"handle": "owner"},
            "latestVersion": {"version": "1.2.3", "createdAt": 1700000000000}
        }"#;
        let parsed: SkillDetailResponse = serde_json::from_str(json).unwrap();
        assert_eq!(
            parsed
                .latest_version
                .as_ref()
                .and_then(|v| v.version.as_deref()),
            Some("1.2.3")
        );
    }
}
