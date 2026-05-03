#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebSearchProviderRoute {
    DuckDuckGo,
    Brave,
    SearXNG,
    Tavily,
    Vane,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WebSearchProviderResolution {
    pub route: WebSearchProviderRoute,
    pub canonical_provider: &'static str,
    pub used_fallback: bool,
}

// Vane is the default for fresh installs because every Ostler install
// from CM051 #32 forward bundles the Vane container at
// http://localhost:3000. Its `search_vane` path graceful-degrades to
// an empty-results response when the container is not running, so
// "default" never crashes the assistant; it just returns nothing
// search-able for that one query.
pub const DEFAULT_WEB_SEARCH_PROVIDER: &str = "vane";
const DUCKDUCKGO_PROVIDER: &str = "duckduckgo";
const BRAVE_PROVIDER: &str = "brave";
const SEARXNG_PROVIDER: &str = "searxng";
const TAVILY_PROVIDER: &str = "tavily";
const VANE_PROVIDER: &str = "vane";

pub fn resolve_web_search_provider(raw_provider: &str) -> WebSearchProviderResolution {
    let normalized = raw_provider.trim().to_ascii_lowercase();
    match normalized.as_str() {
        // Empty or "default" maps to Vane (the bundled local provider).
        // Existing configs with explicit "duckduckgo" still get DDG;
        // see the dedicated arm below.
        "" | "default" | "vane" | "vane-search" | "vane_search" | "perplexica" => {
            WebSearchProviderResolution {
                route: WebSearchProviderRoute::Vane,
                canonical_provider: VANE_PROVIDER,
                used_fallback: false,
            }
        }
        "duckduckgo" | "ddg" | "duck-duck-go" | "duck_duck_go" => WebSearchProviderResolution {
            route: WebSearchProviderRoute::DuckDuckGo,
            canonical_provider: DUCKDUCKGO_PROVIDER,
            used_fallback: false,
        },
        "brave" | "brave-search" | "brave_search" => WebSearchProviderResolution {
            route: WebSearchProviderRoute::Brave,
            canonical_provider: BRAVE_PROVIDER,
            used_fallback: false,
        },
        "searxng" | "searx" | "searx-ng" | "searx_ng" => WebSearchProviderResolution {
            route: WebSearchProviderRoute::SearXNG,
            canonical_provider: SEARXNG_PROVIDER,
            used_fallback: false,
        },
        "tavily" | "tavily-search" | "tavily_search" => WebSearchProviderResolution {
            route: WebSearchProviderRoute::Tavily,
            canonical_provider: TAVILY_PROVIDER,
            used_fallback: false,
        },
        // Warns for unknown providers, falls back to default (Vane).
        _ => WebSearchProviderResolution {
            route: WebSearchProviderRoute::Vane,
            canonical_provider: DEFAULT_WEB_SEARCH_PROVIDER,
            used_fallback: true,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_aliases_to_duckduckgo() {
        let ddg_aliases = ["duckduckgo", "ddg", "duck-duck-go", "duck_duck_go"];
        for alias in ddg_aliases {
            let resolved = resolve_web_search_provider(alias);
            assert_eq!(resolved.route, WebSearchProviderRoute::DuckDuckGo);
            assert_eq!(resolved.canonical_provider, DUCKDUCKGO_PROVIDER);
            assert!(!resolved.used_fallback);
        }
    }

    #[test]
    fn resolve_aliases_to_vane() {
        let vane_aliases = ["vane", "vane-search", "vane_search", "perplexica"];
        for alias in vane_aliases {
            let resolved = resolve_web_search_provider(alias);
            assert_eq!(resolved.route, WebSearchProviderRoute::Vane);
            assert_eq!(resolved.canonical_provider, VANE_PROVIDER);
            assert!(!resolved.used_fallback);
        }
    }

    #[test]
    fn empty_and_default_resolve_to_vane() {
        for alias in ["", "default", "  "] {
            let resolved = resolve_web_search_provider(alias);
            assert_eq!(resolved.route, WebSearchProviderRoute::Vane);
            assert_eq!(resolved.canonical_provider, VANE_PROVIDER);
            assert!(!resolved.used_fallback);
        }
    }

    #[test]
    fn resolve_aliases_to_brave() {
        let brave_aliases = ["brave", "brave-search", "brave_search"];
        for alias in brave_aliases {
            let resolved = resolve_web_search_provider(alias);
            assert_eq!(resolved.route, WebSearchProviderRoute::Brave);
            assert_eq!(resolved.canonical_provider, BRAVE_PROVIDER);
            assert!(!resolved.used_fallback);
        }
    }

    #[test]
    fn resolve_aliases_to_searxng() {
        let searxng_aliases = ["searxng", "searx", "searx-ng", "searx_ng"];
        for alias in searxng_aliases {
            let resolved = resolve_web_search_provider(alias);
            assert_eq!(resolved.route, WebSearchProviderRoute::SearXNG);
            assert_eq!(resolved.canonical_provider, SEARXNG_PROVIDER);
            assert!(!resolved.used_fallback);
        }
    }

    #[test]
    fn resolve_unknown_provider_falls_back_to_default() {
        let resolved = resolve_web_search_provider("bing");
        assert_eq!(resolved.route, WebSearchProviderRoute::Vane);
        assert_eq!(resolved.canonical_provider, DEFAULT_WEB_SEARCH_PROVIDER);
        assert!(resolved.used_fallback);

        let resolved2 = resolve_web_search_provider("searxng-plus");
        assert_eq!(resolved2.route, WebSearchProviderRoute::Vane);
        assert_eq!(resolved2.canonical_provider, DEFAULT_WEB_SEARCH_PROVIDER);
        assert!(resolved2.used_fallback);
    }
}
