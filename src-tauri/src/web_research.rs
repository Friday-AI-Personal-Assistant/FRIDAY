use regex::Regex;
use serde::Serialize;
use std::{net::IpAddr, sync::OnceLock, time::Duration};
use tauri::AppHandle;

const SEARCH_ENDPOINT: &str = "https://html.duckduckgo.com/html/";
const MAX_QUERY_BYTES: usize = 500;
const MAX_RESPONSE_BYTES: usize = 1_000_000;
const MAX_RESULTS: usize = 8;

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct WebSearchResult {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

fn result_regex() -> &'static Regex {
    static VALUE: OnceLock<Regex> = OnceLock::new();
    VALUE.get_or_init(|| Regex::new(r#"(?is)<a[^>]*class=["'][^"']*result__a[^"']*["'][^>]*href=["']([^"']+)["'][^>]*>(.*?)</a>"#).expect("valid result regex"))
}

fn snippet_regex() -> &'static Regex {
    static VALUE: OnceLock<Regex> = OnceLock::new();
    VALUE.get_or_init(|| Regex::new(r#"(?is)<(?:a|div)[^>]*class=["'][^"']*result__snippet[^"']*["'][^>]*>(.*?)</(?:a|div)>"#).expect("valid snippet regex"))
}

fn strip_tags(value: &str) -> String {
    static TAGS: OnceLock<Regex> = OnceLock::new();
    let tags = TAGS.get_or_init(|| Regex::new(r"(?is)<[^>]*>").expect("valid tag regex"));
    decode_entities(&tags.replace_all(value, " "))
}

fn decode_entities(value: &str) -> String {
    value.replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#x27;", "'")
        .replace("&#39;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&nbsp;", " ")
}

fn safe_result_url(raw: &str) -> Option<String> {
    let mut url = url::Url::parse(raw).ok()?;
    if url.host_str() == Some("duckduckgo.com") && url.path() == "/l/" {
        let target = url.query_pairs().find(|(key, _)| key == "uddg")?.1.into_owned();
        url = url::Url::parse(&target).ok()?;
    }
    if !matches!(url.scheme(), "https" | "http")
        || url.username() != ""
        || url.password().is_some()
        || url.host_str().is_none()
    {
        return None;
    }
    if let Some(host) = url.host_str() {
        if let Ok(ip) = host.parse::<IpAddr>() {
            let blocked = match ip {
                IpAddr::V4(value) => value.is_loopback() || value.is_private() || value.is_link_local() || value.is_unspecified() || value.is_multicast(),
                IpAddr::V6(value) => value.is_loopback() || value.is_unique_local() || value.is_unicast_link_local() || value.is_unspecified() || value.is_multicast(),
            };
            if blocked { return None; }
        }
    }
    url.set_fragment(None);
    Some(url.to_string())
}

fn parse_results(html: &str) -> Vec<WebSearchResult> {
    let snippets: Vec<String> = snippet_regex().captures_iter(html)
        .filter_map(|capture| capture.get(1).map(|value| strip_tags(value.as_str())))
        .map(|value| value.trim().to_string())
        .collect();
    let mut results = Vec::new();
    for capture in result_regex().captures_iter(html).take(MAX_RESULTS) {
        let Some(raw_url) = capture.get(1).map(|value| value.as_str()) else { continue; };
        let Some(url) = safe_result_url(raw_url) else { continue; };
        let title = capture.get(2).map(|value| strip_tags(value.as_str()))
            .unwrap_or_default().split_whitespace().collect::<Vec<_>>().join(" ");
        if title.is_empty() { continue; }
        let snippet = snippets.get(results.len()).cloned().unwrap_or_default();
        results.push(WebSearchResult { title, url, snippet });
    }
    results
}

#[tauri::command]
pub async fn web_search(_app: AppHandle, query: String) -> Result<Vec<WebSearchResult>, String> {
    let query = query.trim();
    if query.is_empty() || query.len() > MAX_QUERY_BYTES || query.chars().any(char::is_control) {
        return Err("Enter a search query between 1 and 500 characters.".into());
    }

    // The destination is fixed; redirects are disabled and result links are never fetched here.
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(8))
        .timeout(Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::none())
        .user_agent("FRIDAY-Personal-Assistant/0.2 (web research)")
        .build()
        .map_err(|_| "FRIDAY could not prepare the web search connection.".to_string())?;

    let mut response = client.get(SEARCH_ENDPOINT).query(&[("q", query)]).send().await
        .map_err(|_| "FRIDAY could not reach the web search provider. Check your internet connection.".to_string())?;
    if !response.status().is_success() {
        return Err(format!("The web search provider returned HTTP {}.", response.status()));
    }
    if response.content_length().is_some_and(|length| length > MAX_RESPONSE_BYTES as u64) {
        return Err("The web search response exceeded FRIDAY's 1 MB safety limit.".into());
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await
        .map_err(|_| "The web search response was interrupted.".to_string())?
    {
        if body.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
            return Err("The web search response exceeded FRIDAY's 1 MB safety limit.".into());
        }
        body.extend_from_slice(&chunk);
    }
    let html = String::from_utf8_lossy(&body);
    let results = parse_results(&html);
    if results.is_empty() {
        return Err("No readable search results were returned. Try a different query.".into());
    }
    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unsafe_and_non_web_result_urls() {
        assert!(safe_result_url("file:///C:/Windows").is_none());
        assert!(safe_result_url("http://127.0.0.1/admin").is_none());
        assert!(safe_result_url("http://192.168.1.1/").is_none());
        assert!(safe_result_url("https://user:pass@example.com/").is_none());
        assert!(safe_result_url("https://example.com/path").is_some());
    }

    #[test]
    fn unwraps_search_provider_redirects() {
        let url = safe_result_url("https://duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Farticle").unwrap();
        assert_eq!(url, "https://example.com/article");
    }

    #[test]
    fn removes_markup_and_decodes_basic_entities() {
        assert_eq!(strip_tags("<b>Friday &amp; friends</b>"), "Friday & friends");
    }

    #[test]
    fn validates_search_query_size_and_controls() {
        assert!("hello world".len() <= MAX_QUERY_BYTES);
        assert!("a".repeat(MAX_QUERY_BYTES + 1).len() > MAX_QUERY_BYTES);
        assert!("bad\\nquery".contains(char::is_control));
    }
}
