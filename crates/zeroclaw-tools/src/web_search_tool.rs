use super::web_search_provider_routing::{WebSearchProviderRoute, resolve_web_search_provider};
use async_trait::async_trait;
use regex::Regex;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::time::Duration;
use zeroclaw_api::tool::{Tool, ToolResult};

/// Default Vane base URL: the bundled Vane container in
/// CM051 #32 listens on loopback at `127.0.0.1:3000`. Used when
/// neither the boot-time config nor the on-disk `config.toml` set
/// an explicit `[web_search] vane_url`.
pub const DEFAULT_VANE_URL: &str = "http://localhost:3000";

/// Web search tool for searching the internet.
/// Supports multiple providers: Vane (local, default for fresh
/// installs from CM051 #32 forward), DuckDuckGo (free), Brave
/// (requires API key), and SearXNG (self-hosted, requires instance
/// URL).
///
/// The Brave API key is resolved lazily at execution time: if the boot-time key
/// is missing or still encrypted, the tool re-reads `config.toml`, decrypts the
/// `[web_search] brave_api_key` field, and uses the result. This ensures that
/// keys set or rotated after boot, and encrypted keys, are correctly picked up.
pub struct WebSearchTool {
    /// Provider selector as configured by user. Routed via provider aliases at runtime.
    provider: String,
    /// Boot-time key snapshot (may be `None` if not yet configured at startup).
    boot_brave_api_key: Option<String>,
    /// SearXNG instance base URL (e.g. "https://searx.example.com").
    searxng_instance_url: Option<String>,
    /// Vane base URL (e.g. "http://localhost:3000"). When `None`, the
    /// runtime falls back to `DEFAULT_VANE_URL` after first checking
    /// `config.toml` for an override. Stored as `Option` (rather than
    /// pre-resolved) so the same lazy-reload path SearXNG uses applies
    /// here without a behavioural fork.
    vane_url: Option<String>,
    max_results: usize,
    timeout_secs: u64,
    /// Path to `config.toml` for lazy re-read of keys at execution time.
    config_path: PathBuf,
    /// Whether secret encryption is enabled (needed to create a `SecretStore`).
    secrets_encrypt: bool,
}

impl WebSearchTool {
    pub fn new(
        provider: String,
        brave_api_key: Option<String>,
        max_results: usize,
        timeout_secs: u64,
    ) -> Self {
        Self {
            provider: provider.trim().to_lowercase(),
            boot_brave_api_key: brave_api_key,
            searxng_instance_url: None,
            vane_url: None,
            max_results: max_results.clamp(1, 10),
            timeout_secs: timeout_secs.max(1),
            config_path: PathBuf::new(),
            secrets_encrypt: false,
        }
    }

    /// Create a `WebSearchTool` with config-reload and decryption support.
    ///
    /// `config_path` is the path to `config.toml` so the tool can re-read the
    /// Brave API key at execution time. `secrets_encrypt` controls whether the
    /// key is decrypted via `SecretStore`.
    #[allow(clippy::too_many_arguments)]
    pub fn new_with_config(
        provider: String,
        brave_api_key: Option<String>,
        searxng_instance_url: Option<String>,
        vane_url: Option<String>,
        max_results: usize,
        timeout_secs: u64,
        config_path: PathBuf,
        secrets_encrypt: bool,
    ) -> Self {
        Self {
            provider: provider.trim().to_lowercase(),
            boot_brave_api_key: brave_api_key,
            searxng_instance_url,
            vane_url,
            max_results: max_results.clamp(1, 10),
            timeout_secs: timeout_secs.max(1),
            config_path,
            secrets_encrypt,
        }
    }

    /// Resolve the Brave API key, preferring the boot-time value but falling
    /// back to a fresh config read + decryption when the boot-time value is
    /// absent.
    fn resolve_brave_api_key(&self) -> anyhow::Result<String> {
        // Fast path: boot-time key is present and usable (not an encrypted blob).
        if let Some(ref key) = self.boot_brave_api_key
            && !key.is_empty()
            && !zeroclaw_config::secrets::SecretStore::is_encrypted(key)
        {
            return Ok(key.clone());
        }

        // Slow path: re-read config.toml to pick up keys set/rotated after boot.
        self.reload_brave_api_key()
    }

    /// Re-read `config.toml` and decrypt `[web_search] brave_api_key`.
    fn reload_brave_api_key(&self) -> anyhow::Result<String> {
        let contents = std::fs::read_to_string(&self.config_path).map_err(|e| {
            anyhow::anyhow!(
                "Failed to read config file {} for Brave API key: {e}",
                self.config_path.display()
            )
        })?;

        let config: zeroclaw_config::schema::Config = toml::from_str(&contents).map_err(|e| {
            anyhow::anyhow!(
                "Failed to parse config file {} for Brave API key: {e}",
                self.config_path.display()
            )
        })?;

        let raw_key = config
            .web_search
            .brave_api_key
            .filter(|k| !k.is_empty())
            .ok_or_else(|| anyhow::anyhow!("Brave API key not configured"))?;

        // Decrypt if necessary.
        if zeroclaw_config::secrets::SecretStore::is_encrypted(&raw_key) {
            let zeroclaw_dir = self.config_path.parent().unwrap_or_else(|| Path::new("."));
            let store =
                zeroclaw_config::secrets::SecretStore::new(zeroclaw_dir, self.secrets_encrypt);
            let plaintext = store.decrypt(&raw_key)?;
            if plaintext.is_empty() {
                anyhow::bail!("Brave API key not configured (decrypted value is empty)");
            }
            Ok(plaintext)
        } else {
            Ok(raw_key)
        }
    }

    async fn search_duckduckgo(&self, query: &str) -> anyhow::Result<String> {
        let encoded_query = urlencoding::encode(query);
        let search_url = format!("https://html.duckduckgo.com/html/?q={}", encoded_query);

        let builder = reqwest::Client::builder()
            .timeout(Duration::from_secs(self.timeout_secs))
            .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36");
        let builder =
            zeroclaw_config::schema::apply_runtime_proxy_to_builder(builder, "tool.web_search");
        let client = builder.build()?;

        let response = client.get(&search_url).send().await?;

        if !response.status().is_success() {
            anyhow::bail!(
                "DuckDuckGo search failed with status: {}",
                response.status()
            );
        }

        let html = response.text().await?;
        self.parse_duckduckgo_results(&html, query)
    }

    fn parse_duckduckgo_results(&self, html: &str, query: &str) -> anyhow::Result<String> {
        // Extract result links: <a class="result__a" href="...">Title</a>
        let link_regex = Regex::new(
            r#"<a[^>]*class="[^"]*result__a[^"]*"[^>]*href="([^"]+)"[^>]*>([\s\S]*?)</a>"#,
        )?;

        // Extract snippets: <a class="result__snippet">...</a>
        let snippet_regex = Regex::new(r#"<a class="result__snippet[^"]*"[^>]*>([\s\S]*?)</a>"#)?;

        let link_matches: Vec<_> = link_regex
            .captures_iter(html)
            .take(self.max_results + 2)
            .collect();

        let snippet_matches: Vec<_> = snippet_regex
            .captures_iter(html)
            .take(self.max_results + 2)
            .collect();

        if link_matches.is_empty() {
            return Ok(format!("No results found for: {}", query));
        }

        let mut lines = vec![format!("Search results for: {} (via DuckDuckGo)", query)];

        let count = link_matches.len().min(self.max_results);

        for i in 0..count {
            let caps = &link_matches[i];
            let url_str = decode_ddg_redirect_url(&caps[1]);
            let title = strip_tags(&caps[2]);

            lines.push(format!("{}. {}", i + 1, title.trim()));
            lines.push(format!("   {}", url_str.trim()));

            // Add snippet if available
            if i < snippet_matches.len() {
                let snippet = strip_tags(&snippet_matches[i][1]);
                let snippet = snippet.trim();
                if !snippet.is_empty() {
                    lines.push(format!("   {}", snippet));
                }
            }
        }

        Ok(lines.join("\n"))
    }

    async fn search_brave(&self, query: &str) -> anyhow::Result<String> {
        let api_key = self.resolve_brave_api_key()?;

        let encoded_query = urlencoding::encode(query);
        let search_url = format!(
            "https://api.search.brave.com/res/v1/web/search?q={}&count={}",
            encoded_query, self.max_results
        );

        let builder = reqwest::Client::builder().timeout(Duration::from_secs(self.timeout_secs));
        let builder =
            zeroclaw_config::schema::apply_runtime_proxy_to_builder(builder, "tool.web_search");
        let client = builder.build()?;

        let response = client
            .get(&search_url)
            .header("Accept", "application/json")
            .header("X-Subscription-Token", &api_key)
            .send()
            .await?;

        if !response.status().is_success() {
            anyhow::bail!("Brave search failed with status: {}", response.status());
        }

        let json: serde_json::Value = response.json().await?;
        self.parse_brave_results(&json, query)
    }

    fn parse_brave_results(&self, json: &serde_json::Value, query: &str) -> anyhow::Result<String> {
        let results = json
            .get("web")
            .and_then(|w| w.get("results"))
            .and_then(|r| r.as_array())
            .ok_or_else(|| anyhow::anyhow!("Invalid Brave API response"))?;

        if results.is_empty() {
            return Ok(format!("No results found for: {}", query));
        }

        let mut lines = vec![format!("Search results for: {} (via Brave)", query)];

        for (i, result) in results.iter().take(self.max_results).enumerate() {
            let title = result
                .get("title")
                .and_then(|t| t.as_str())
                .unwrap_or("No title");
            let url = result.get("url").and_then(|u| u.as_str()).unwrap_or("");
            let description = result
                .get("description")
                .and_then(|d| d.as_str())
                .unwrap_or("");

            lines.push(format!("{}. {}", i + 1, title));
            lines.push(format!("   {}", url));
            if !description.is_empty() {
                lines.push(format!("   {}", description));
            }
        }

        Ok(lines.join("\n"))
    }

    /// Resolve the SearXNG instance URL from the boot-time config or by
    /// re-reading `config.toml` at runtime.
    fn resolve_searxng_instance_url(&self) -> anyhow::Result<String> {
        if let Some(ref url) = self.searxng_instance_url
            && !url.is_empty()
        {
            return Ok(url.clone());
        }

        // Slow path: re-read config.toml to pick up values set after boot.
        let contents = std::fs::read_to_string(&self.config_path).map_err(|e| {
            anyhow::anyhow!(
                "Failed to read config file {} for SearXNG instance URL: {e}",
                self.config_path.display()
            )
        })?;

        let config: zeroclaw_config::schema::Config = toml::from_str(&contents).map_err(|e| {
            anyhow::anyhow!(
                "Failed to parse config file {} for SearXNG instance URL: {e}",
                self.config_path.display()
            )
        })?;

        config
            .web_search
            .searxng_instance_url
            .filter(|u| !u.is_empty())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "SearXNG instance URL not configured. Set [web_search] searxng_instance_url \
                     in config.toml or the SEARXNG_INSTANCE_URL environment variable."
                )
            })
    }

    async fn search_searxng(&self, query: &str) -> anyhow::Result<String> {
        let instance_url = self.resolve_searxng_instance_url()?;
        let base_url = instance_url.trim_end_matches('/');

        let encoded_query = urlencoding::encode(query);
        let search_url = format!(
            "{}/search?q={}&format=json&pageno=1",
            base_url, encoded_query
        );

        let builder = reqwest::Client::builder()
            .timeout(Duration::from_secs(self.timeout_secs))
            .user_agent("ZeroClaw/1.0");
        let builder =
            zeroclaw_config::schema::apply_runtime_proxy_to_builder(builder, "tool.web_search");
        let client = builder.build()?;

        let response = client
            .get(&search_url)
            .header("Accept", "application/json")
            .send()
            .await?;

        if !response.status().is_success() {
            anyhow::bail!("SearXNG search failed with status: {}", response.status());
        }

        let json: serde_json::Value = response.json().await?;
        self.parse_searxng_results(&json, query)
    }

    fn parse_searxng_results(
        &self,
        json: &serde_json::Value,
        query: &str,
    ) -> anyhow::Result<String> {
        let results = json
            .get("results")
            .and_then(|r| r.as_array())
            .ok_or_else(|| anyhow::anyhow!("Invalid SearXNG API response"))?;

        if results.is_empty() {
            return Ok(format!("No results found for: {}", query));
        }

        let mut lines = vec![format!("Search results for: {} (via SearXNG)", query)];

        for (i, result) in results.iter().take(self.max_results).enumerate() {
            let title = result
                .get("title")
                .and_then(|t| t.as_str())
                .unwrap_or("No title");
            let url = result.get("url").and_then(|u| u.as_str()).unwrap_or("");
            let content = result.get("content").and_then(|c| c.as_str()).unwrap_or("");

            lines.push(format!("{}. {}", i + 1, title));
            lines.push(format!("   {}", url));
            if !content.is_empty() {
                lines.push(format!("   {}", content));
            }
        }

        Ok(lines.join("\n"))
    }

    /// Resolve the Vane base URL: boot-time value if set, else
    /// `config.toml`, else `DEFAULT_VANE_URL`. Always returns a value
    /// (never errors), because every fresh Ostler install bundles
    /// Vane on `localhost:3000` and we always have a sensible default
    /// to fall back to.
    fn resolve_vane_url(&self) -> String {
        if let Some(ref url) = self.vane_url
            && !url.trim().is_empty()
        {
            return url.trim().to_string();
        }

        if let Ok(contents) = std::fs::read_to_string(&self.config_path)
            && let Ok(config) = toml::from_str::<zeroclaw_config::schema::Config>(&contents)
            && let Some(url) = config.web_search.vane_url.as_deref()
            && !url.trim().is_empty()
        {
            return url.trim().to_string();
        }

        DEFAULT_VANE_URL.to_string()
    }

    /// Search via the bundled Vane container (Perplexica-compatible
    /// API at `<base>/api/search`).
    ///
    /// Vane returns both an LLM-synthesised `message` and a `sources`
    /// array of citations; both are surfaced to the caller. The
    /// minimum request body is `{query, sources, optimizationMode,
    /// history, stream}`; the model selection fields (`chatModel`,
    /// `embeddingModel`) are intentionally omitted so Vane uses the
    /// provider it was configured with at install time (typically the
    /// local Ollama).
    ///
    /// Graceful-degradation policy: if Vane is unreachable
    /// (container not running, port blocked) the caller gets a
    /// human-readable empty-results message rather than an error.
    /// 5xx from Vane is treated the same way (running but broken).
    /// 4xx still bubbles up because that points at a client bug we
    /// want to notice. See `parse_vane_results` for the success-path
    /// shape.
    async fn search_vane(&self, query: &str) -> anyhow::Result<String> {
        let base_url = self.resolve_vane_url();
        let trimmed = base_url.trim_end_matches('/');
        let url = format!("{}/api/search", trimmed);

        let body = json!({
            "query": query,
            "sources": ["web"],
            "optimizationMode": "speed",
            "history": [],
            "stream": false,
        });

        let builder = reqwest::Client::builder()
            .timeout(Duration::from_secs(self.timeout_secs))
            .user_agent("Ostler/1.0 (Vane web search)");
        let builder =
            zeroclaw_config::schema::apply_runtime_proxy_to_builder(builder, "tool.web_search");
        let client = builder.build()?;

        let response = match client
            .post(&url)
            .header("Accept", "application/json")
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(
                    "Vane web search unreachable at {}: {}. \
                     Returning empty results so the assistant can keep responding.",
                    base_url,
                    e
                );
                return Ok(format!(
                    "Web search via Vane is currently unavailable: the local Vane container at {} did not respond. \
                     Try `docker compose -f ~/.ostler/docker-compose.yml up -d vane` and run the search again.",
                    base_url
                ));
            }
        };

        let status = response.status();
        if status.is_server_error() {
            tracing::warn!(
                "Vane returned {} for {}. Treating as 'no results' so the assistant can continue.",
                status,
                url
            );
            return Ok(format!(
                "Web search via Vane returned a server error ({}) for: {}. \
                 The container may be starting up or its upstream search index is unreachable.",
                status, query
            ));
        }
        if !status.is_success() {
            anyhow::bail!("Vane search failed with status: {}", status);
        }

        let json: serde_json::Value = response.json().await?;
        self.parse_vane_results(&json, query)
    }

    fn parse_vane_results(&self, json: &serde_json::Value, query: &str) -> anyhow::Result<String> {
        // Vane response shape (Perplexica-compatible, non-streaming):
        //   { "message": "...synthesised answer...",
        //     "sources": [ { "content": "...", "metadata": { "title": "...", "url": "..." } } ] }
        // Either `message` or `sources` may be empty; we surface
        // whichever is present and emit a "no results" line if both
        // are empty.

        let message = json
            .get("message")
            .and_then(|m| m.as_str())
            .map(|s| s.trim())
            .filter(|s| !s.is_empty());

        let sources_array = json
            .get("sources")
            .and_then(|s| s.as_array())
            .cloned()
            .unwrap_or_default();

        if message.is_none() && sources_array.is_empty() {
            return Ok(format!("No results found for: {}", query));
        }

        let mut lines = vec![format!("Web search results for: {} (via Vane)", query)];

        if let Some(msg) = message {
            lines.push(String::new());
            lines.push("Vane's synthesis:".to_string());
            lines.push(msg.to_string());
        }

        if !sources_array.is_empty() {
            lines.push(String::new());
            lines.push("Sources:".to_string());
            for (i, source) in sources_array.iter().take(self.max_results).enumerate() {
                let title = source
                    .get("metadata")
                    .and_then(|m| m.get("title"))
                    .and_then(|t| t.as_str())
                    .unwrap_or("No title");
                let url = source
                    .get("metadata")
                    .and_then(|m| m.get("url"))
                    .and_then(|u| u.as_str())
                    .unwrap_or("");
                let content = source.get("content").and_then(|c| c.as_str()).unwrap_or("");

                lines.push(format!("{}. {}", i + 1, title));
                if !url.is_empty() {
                    lines.push(format!("   {}", url));
                }
                if !content.is_empty() {
                    // Trim long content snippets so the tool output
                    // does not overwhelm the LLM context window;
                    // keeping ~400 chars per source is enough for
                    // grounding without becoming the whole response.
                    let snippet: String = content.chars().take(400).collect();
                    let suffix = if content.chars().count() > 400 {
                        " ..."
                    } else {
                        ""
                    };
                    lines.push(format!("   {}{}", snippet.trim(), suffix));
                }
            }
        }

        Ok(lines.join("\n"))
    }
}

fn decode_ddg_redirect_url(raw_url: &str) -> String {
    if let Some(index) = raw_url.find("uddg=") {
        let encoded = &raw_url[index + 5..];
        let encoded = encoded.split('&').next().unwrap_or(encoded);
        if let Ok(decoded) = urlencoding::decode(encoded) {
            return decoded.into_owned();
        }
    }

    raw_url.to_string()
}

fn strip_tags(content: &str) -> String {
    let re = Regex::new(r"<[^>]+>").unwrap();
    re.replace_all(content, "").to_string()
}

#[async_trait]
impl Tool for WebSearchTool {
    fn name(&self) -> &str {
        "web_search_tool"
    }

    fn description(&self) -> &str {
        "Search the web for information. Returns relevant search results with titles, URLs, and descriptions. Use this to find current information, news, or research topics."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "The search query. Be specific for better results."
                }
            },
            "required": ["query"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let query = args
            .get("query")
            .and_then(|q| q.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing required parameter: query"))?;

        if query.trim().is_empty() {
            anyhow::bail!("Search query cannot be empty");
        }

        tracing::info!("Searching web for: {}", query);

        let resolution = resolve_web_search_provider(&self.provider);
        if resolution.used_fallback {
            tracing::warn!(
                "Unknown web search provider '{}'; falling back to '{}'",
                self.provider,
                resolution.canonical_provider
            );
        }

        let result = match resolution.route {
            WebSearchProviderRoute::DuckDuckGo | WebSearchProviderRoute::Tavily => {
                self.search_duckduckgo(query).await?
            } // TODO: implement Tavily search
            WebSearchProviderRoute::Brave => self.search_brave(query).await?,
            WebSearchProviderRoute::SearXNG => self.search_searxng(query).await?,
            WebSearchProviderRoute::Vane => self.search_vane(query).await?,
        };

        Ok(ToolResult {
            success: true,
            output: result,
            error: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tool_name() {
        let tool = WebSearchTool::new("duckduckgo".to_string(), None, 5, 15);
        assert_eq!(tool.name(), "web_search_tool");
    }

    #[test]
    fn test_tool_description() {
        let tool = WebSearchTool::new("duckduckgo".to_string(), None, 5, 15);
        assert!(tool.description().contains("Search the web"));
    }

    #[test]
    fn test_parameters_schema() {
        let tool = WebSearchTool::new("duckduckgo".to_string(), None, 5, 15);
        let schema = tool.parameters_schema();
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["query"].is_object());
    }

    #[test]
    fn test_strip_tags() {
        let html = "<b>Hello</b> <i>World</i>";
        assert_eq!(strip_tags(html), "Hello World");
    }

    #[test]
    fn test_parse_duckduckgo_results_empty() {
        let tool = WebSearchTool::new("duckduckgo".to_string(), None, 5, 15);
        let result = tool
            .parse_duckduckgo_results("<html>No results here</html>", "test")
            .unwrap();
        assert!(result.contains("No results found"));
    }

    #[test]
    fn test_parse_duckduckgo_results_with_data() {
        let tool = WebSearchTool::new("duckduckgo".to_string(), None, 5, 15);
        let html = r#"
            <a class="result__a" href="https://example.com">Example Title</a>
            <a class="result__snippet">This is a description</a>
        "#;
        let result = tool.parse_duckduckgo_results(html, "test").unwrap();
        assert!(result.contains("Example Title"));
        assert!(result.contains("https://example.com"));
    }

    #[test]
    fn test_parse_duckduckgo_results_decodes_redirect_url() {
        let tool = WebSearchTool::new("duckduckgo".to_string(), None, 5, 15);
        let html = r#"
            <a class="result__a" href="https://duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Fpath%3Fa%3D1&amp;rut=test">Example Title</a>
            <a class="result__snippet">This is a description</a>
        "#;
        let result = tool.parse_duckduckgo_results(html, "test").unwrap();
        assert!(result.contains("https://example.com/path?a=1"));
        assert!(!result.contains("rut=test"));
    }

    #[test]
    fn test_constructor_clamps_web_search_limits() {
        let tool = WebSearchTool::new("duckduckgo".to_string(), None, 0, 0);
        let html = r#"
            <a class="result__a" href="https://example.com">Example Title</a>
            <a class="result__snippet">This is a description</a>
        "#;
        let result = tool.parse_duckduckgo_results(html, "test").unwrap();
        assert!(result.contains("Example Title"));
    }

    #[tokio::test]
    async fn test_execute_missing_query() {
        let tool = WebSearchTool::new("duckduckgo".to_string(), None, 5, 15);
        let result = tool.execute(json!({})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_execute_empty_query() {
        let tool = WebSearchTool::new("duckduckgo".to_string(), None, 5, 15);
        let result = tool.execute(json!({"query": ""})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_execute_brave_without_api_key() {
        let tool = WebSearchTool::new("brave".to_string(), None, 5, 15);
        let result = tool.execute(json!({"query": "test"})).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("API key"));
    }

    #[test]
    fn test_resolve_brave_api_key_uses_boot_key() {
        let tool = WebSearchTool::new(
            "brave".to_string(),
            Some("sk-plaintext-key".to_string()),
            5,
            15,
        );
        let key = tool.resolve_brave_api_key().unwrap();
        assert_eq!(key, "sk-plaintext-key");
    }

    #[test]
    fn test_resolve_brave_api_key_reloads_from_config() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config_path = tmp.path().join("config.toml");
        std::fs::write(
            &config_path,
            "[web_search]\nbrave_api_key = \"fresh-key-from-disk\"\n",
        )
        .unwrap();

        // No boot key -- forces reload from config
        let tool = WebSearchTool::new_with_config(
            "brave".to_string(),
            None,
            None,
            None,
            5,
            15,
            config_path,
            false,
        );
        let key = tool.resolve_brave_api_key().unwrap();
        assert_eq!(key, "fresh-key-from-disk");
    }

    #[test]
    fn test_resolve_brave_api_key_decrypts_encrypted_key() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = zeroclaw_config::secrets::SecretStore::new(tmp.path(), true);
        let encrypted = store.encrypt("brave-secret-key").unwrap();

        let config_path = tmp.path().join("config.toml");
        std::fs::write(
            &config_path,
            format!("[web_search]\nbrave_api_key = \"{}\"\n", encrypted),
        )
        .unwrap();

        // Boot key is the encrypted blob -- should trigger reload + decrypt
        let tool = WebSearchTool::new_with_config(
            "brave".to_string(),
            Some(encrypted),
            None,
            None,
            5,
            15,
            config_path,
            true,
        );
        let key = tool.resolve_brave_api_key().unwrap();
        assert_eq!(key, "brave-secret-key");
    }

    #[tokio::test]
    async fn test_execute_searxng_without_instance_url() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config_path = tmp.path().join("config.toml");
        std::fs::write(&config_path, "[web_search]\n").unwrap();

        let tool = WebSearchTool::new_with_config(
            "searxng".to_string(),
            None,
            None,
            None,
            5,
            15,
            config_path,
            false,
        );
        let result = tool.execute(json!({"query": "test"})).await;
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("SearXNG instance URL not configured")
        );
    }

    #[test]
    fn test_parse_searxng_results_empty() {
        let tool = WebSearchTool::new("searxng".to_string(), None, 5, 15);
        let json = serde_json::json!({"results": []});
        let result = tool.parse_searxng_results(&json, "test").unwrap();
        assert!(result.contains("No results found"));
    }

    #[test]
    fn test_parse_searxng_results_with_data() {
        let tool = WebSearchTool::new("searxng".to_string(), None, 5, 15);
        let json = serde_json::json!({
            "results": [
                {
                    "title": "SearXNG Example",
                    "url": "https://example.com",
                    "content": "A privacy-respecting metasearch engine"
                },
                {
                    "title": "Another Result",
                    "url": "https://example.org",
                    "content": "More information here"
                }
            ]
        });
        let result = tool.parse_searxng_results(&json, "test").unwrap();
        assert!(result.contains("SearXNG Example"));
        assert!(result.contains("https://example.com"));
        assert!(result.contains("A privacy-respecting metasearch engine"));
        assert!(result.contains("via SearXNG"));
    }

    #[test]
    fn test_parse_searxng_results_invalid_response() {
        let tool = WebSearchTool::new("searxng".to_string(), None, 5, 15);
        let json = serde_json::json!({"error": "bad request"});
        let result = tool.parse_searxng_results(&json, "test");
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Invalid SearXNG API response")
        );
    }

    #[test]
    fn test_resolve_searxng_instance_url_from_boot() {
        let tool = WebSearchTool {
            provider: "searxng".to_string(),
            boot_brave_api_key: None,
            searxng_instance_url: Some("https://searx.example.com".to_string()),
            vane_url: None,
            max_results: 5,
            timeout_secs: 15,
            config_path: PathBuf::new(),
            secrets_encrypt: false,
        };
        let url = tool.resolve_searxng_instance_url().unwrap();
        assert_eq!(url, "https://searx.example.com");
    }

    #[test]
    fn test_resolve_searxng_instance_url_reloads_from_config() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config_path = tmp.path().join("config.toml");
        std::fs::write(
            &config_path,
            "[web_search]\nsearxng_instance_url = \"https://search.local\"\n",
        )
        .unwrap();

        let tool = WebSearchTool::new_with_config(
            "searxng".to_string(),
            None,
            None,
            None,
            5,
            15,
            config_path,
            false,
        );
        let url = tool.resolve_searxng_instance_url().unwrap();
        assert_eq!(url, "https://search.local");
    }

    #[test]
    fn test_resolve_brave_api_key_picks_up_runtime_update() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config_path = tmp.path().join("config.toml");

        // Start with no key in config
        std::fs::write(&config_path, "[web_search]\n").unwrap();

        let tool = WebSearchTool::new_with_config(
            "brave".to_string(),
            None,
            None,
            None,
            5,
            15,
            config_path.clone(),
            false,
        );

        // Key not configured yet -- should fail
        assert!(tool.resolve_brave_api_key().is_err());

        // Simulate runtime config update (e.g. via web_search_config set)
        std::fs::write(
            &config_path,
            "[web_search]\nbrave_api_key = \"runtime-updated-key\"\n",
        )
        .unwrap();

        // Now should succeed with the updated key
        let key = tool.resolve_brave_api_key().unwrap();
        assert_eq!(key, "runtime-updated-key");
    }

    // ── Vane provider tests ────────────────────────────────────────

    #[test]
    fn test_resolve_vane_url_falls_back_to_default() {
        // Empty config + no boot value -> DEFAULT_VANE_URL.
        let tool = WebSearchTool::new("vane".to_string(), None, 5, 15);
        let url = tool.resolve_vane_url();
        assert_eq!(url, DEFAULT_VANE_URL);
    }

    #[test]
    fn test_resolve_vane_url_from_boot() {
        let tool = WebSearchTool {
            provider: "vane".to_string(),
            boot_brave_api_key: None,
            searxng_instance_url: None,
            vane_url: Some("http://192.168.1.10:3000".to_string()),
            max_results: 5,
            timeout_secs: 15,
            config_path: PathBuf::new(),
            secrets_encrypt: false,
        };
        assert_eq!(tool.resolve_vane_url(), "http://192.168.1.10:3000");
    }

    #[test]
    fn test_resolve_vane_url_reloads_from_config() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config_path = tmp.path().join("config.toml");
        std::fs::write(
            &config_path,
            "[web_search]\nvane_url = \"http://hub.local:3000\"\n",
        )
        .unwrap();

        // No boot value, but config.toml has one.
        let tool = WebSearchTool::new_with_config(
            "vane".to_string(),
            None,
            None,
            None,
            5,
            15,
            config_path,
            false,
        );
        assert_eq!(tool.resolve_vane_url(), "http://hub.local:3000");
    }

    #[test]
    fn test_resolve_vane_url_blank_falls_through_to_config_then_default() {
        // Blank/whitespace boot value should not be treated as an
        // override; the resolver continues to config.toml, then to
        // DEFAULT_VANE_URL when neither is set.
        let tmp = tempfile::TempDir::new().unwrap();
        let config_path = tmp.path().join("config.toml");
        std::fs::write(&config_path, "[web_search]\n").unwrap();

        let tool = WebSearchTool::new_with_config(
            "vane".to_string(),
            None,
            None,
            Some("   ".to_string()),
            5,
            15,
            config_path,
            false,
        );
        assert_eq!(tool.resolve_vane_url(), DEFAULT_VANE_URL);
    }

    #[test]
    fn test_parse_vane_results_with_message_and_sources() {
        let tool = WebSearchTool::new("vane".to_string(), None, 5, 15);
        let json = serde_json::json!({
            "message": "Vane is a self-hosted AI search engine.",
            "sources": [
                {
                    "content": "Vane is innovative, open-source AI-powered search.",
                    "metadata": {
                        "title": "What is Vane",
                        "url": "https://example.com/vane"
                    }
                },
                {
                    "content": "It uses local LLMs for synthesis.",
                    "metadata": {
                        "title": "Vane architecture",
                        "url": "https://example.com/vane/arch"
                    }
                }
            ]
        });
        let result = tool.parse_vane_results(&json, "what is vane").unwrap();
        assert!(result.contains("via Vane"));
        assert!(result.contains("Vane's synthesis:"));
        assert!(result.contains("Vane is a self-hosted AI search engine."));
        assert!(result.contains("Sources:"));
        assert!(result.contains("1. What is Vane"));
        assert!(result.contains("https://example.com/vane"));
        assert!(result.contains("2. Vane architecture"));
    }

    #[test]
    fn test_parse_vane_results_message_only_no_sources() {
        let tool = WebSearchTool::new("vane".to_string(), None, 5, 15);
        let json = serde_json::json!({
            "message": "Synthesis with no citations.",
            "sources": []
        });
        let result = tool.parse_vane_results(&json, "test").unwrap();
        assert!(result.contains("Vane's synthesis:"));
        assert!(result.contains("Synthesis with no citations."));
        assert!(!result.contains("Sources:"));
    }

    #[test]
    fn test_parse_vane_results_sources_only_no_message() {
        let tool = WebSearchTool::new("vane".to_string(), None, 5, 15);
        let json = serde_json::json!({
            "sources": [
                {
                    "content": "Result snippet.",
                    "metadata": { "title": "Result", "url": "https://example.com/r" }
                }
            ]
        });
        let result = tool.parse_vane_results(&json, "test").unwrap();
        assert!(!result.contains("Vane's synthesis:"));
        assert!(result.contains("Sources:"));
        assert!(result.contains("1. Result"));
    }

    #[test]
    fn test_parse_vane_results_empty() {
        let tool = WebSearchTool::new("vane".to_string(), None, 5, 15);
        let json = serde_json::json!({ "message": "", "sources": [] });
        let result = tool.parse_vane_results(&json, "test").unwrap();
        assert!(result.contains("No results found for: test"));
    }

    #[test]
    fn test_parse_vane_results_missing_fields_treated_as_empty() {
        let tool = WebSearchTool::new("vane".to_string(), None, 5, 15);
        // Defensive: response missing both `message` and `sources`
        // (e.g. an unexpected shape) should not crash; resolves to
        // "no results" so the assistant can keep responding.
        let json = serde_json::json!({});
        let result = tool.parse_vane_results(&json, "test").unwrap();
        assert!(result.contains("No results found"));
    }

    #[test]
    fn test_parse_vane_results_truncates_long_content() {
        let tool = WebSearchTool::new("vane".to_string(), None, 5, 15);
        let long_content = "x".repeat(800);
        let json = serde_json::json!({
            "message": "synth",
            "sources": [{
                "content": long_content,
                "metadata": { "title": "T", "url": "https://e.com" }
            }]
        });
        let result = tool.parse_vane_results(&json, "test").unwrap();
        // 400-char cap + " ..." suffix; original 800 chars never appears verbatim
        assert!(result.contains("..."));
        assert!(!result.contains(&"x".repeat(500)));
    }

    #[test]
    fn test_parse_vane_results_clamps_to_max_results() {
        let tool = WebSearchTool::new("vane".to_string(), None, 2, 15);
        let json = serde_json::json!({
            "sources": [
                {"content": "a", "metadata": {"title": "A", "url": "https://a"}},
                {"content": "b", "metadata": {"title": "B", "url": "https://b"}},
                {"content": "c", "metadata": {"title": "C", "url": "https://c"}},
            ]
        });
        let result = tool.parse_vane_results(&json, "test").unwrap();
        assert!(result.contains("1. A"));
        assert!(result.contains("2. B"));
        assert!(!result.contains("3. C"));
    }

    #[tokio::test]
    async fn test_search_vane_graceful_degrade_when_unreachable() {
        // The tool must NOT bubble connect/server errors up to the
        // caller; it must return a friendly message so the assistant
        // can keep responding. Both connect-refused (Vane container
        // not running) and 5xx (Vane container starting up) routes
        // are graceful; this test exercises whichever the runtime
        // happens to hit.
        let tmp = tempfile::TempDir::new().unwrap();
        let config_path = tmp.path().join("config.toml");
        std::fs::write(&config_path, "[web_search]\n").unwrap();

        let tool = WebSearchTool::new_with_config(
            "vane".to_string(),
            None,
            None,
            // Port 1 (reserved TCPMUX) almost always refuses
            // connections; on rare hosts something returns 5xx.
            // Both are graceful paths.
            Some("http://127.0.0.1:1".to_string()),
            5,
            1,
            config_path,
            false,
        );
        let result = tool.execute(json!({"query": "anything"})).await;
        assert!(result.is_ok(), "graceful path must NOT bubble errors");
        let output = result.unwrap().output;
        assert!(
            output.contains("Vane")
                && (output.contains("unavailable")
                    || output.contains("did not respond")
                    || output.contains("server error")),
            "expected graceful Vane message; got: {output}"
        );
    }
}
