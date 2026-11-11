use regex::Regex;
use serde::Serialize;
use std::{net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr}, sync::OnceLock, time::Duration};
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
        if let Ok(ip) = host.trim_start_matches('[').trim_end_matches(']').parse::<IpAddr>() {
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
    let search_url = url::Url::parse(SEARCH_ENDPOINT).map_err(|_| "FRIDAY's search endpoint is invalid.".to_string())?;
    let pinned = pinned_public_address(&search_url).await?;
    let mut builder = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(8))
        .timeout(Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::none())
        .user_agent("FRIDAY-Personal-Assistant/0.2 (web research)");
    if let Some(address) = pinned { builder = builder.resolve(search_url.host_str().unwrap_or("html.duckduckgo.com"), address); }
    let client = builder.build()
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


const MAX_PAGE_BYTES: usize = 1_500_000;
const MAX_PAGE_TEXT_BYTES: usize = 14_000;
const MAX_REDIRECTS: usize = 3;

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct WebPage {
    pub title: String,
    pub url: String,
    pub text: String,
}

fn public_ipv4(ip: Ipv4Addr) -> bool {
    let octets = ip.octets();
    !(ip.is_private() || ip.is_loopback() || ip.is_link_local() || ip.is_broadcast()
        || ip.is_unspecified() || ip.is_multicast()
        || octets[0] == 0 || octets[0] >= 240
        || (octets[0] == 100 && (64..=127).contains(&octets[1]))
        || (octets[0] == 192 && octets[1] == 0 && octets[2] == 0)
        || (octets[0] == 192 && octets[1] == 0 && octets[2] == 2)
        || (octets[0] == 198 && (octets[1] == 18 || octets[1] == 19 || (octets[1] == 51 && octets[2] == 100)))
        || (octets[0] == 203 && octets[1] == 0 && octets[2] == 113))
}

fn public_ipv6(ip: Ipv6Addr) -> bool {
    let segments = ip.segments();
    (segments[0] & 0xe000) == 0x2000
        && !(segments[0] == 0x2001 && (segments[1] == 0x0db8 || (segments[1] & 0xfff0) == 0x0010))
        && !ip.is_loopback() && !ip.is_unspecified()
        && !ip.is_unique_local() && !ip.is_unicast_link_local() && !ip.is_multicast()
}

fn validate_page_url(raw: &str) -> Result<url::Url, String> {
    let url = url::Url::parse(raw).map_err(|_| "This source link is not a valid URL.".to_string())?;
    if url.scheme() != "https" || url.username() != "" || url.password().is_some() || url.host_str().is_none() {
        return Err("For safety, FRIDAY can read HTTPS webpages only.".into());
    }
    Ok(url)
}

async fn pinned_public_address(url: &url::Url) -> Result<Option<SocketAddr>, String> {
    let host = url.host_str().ok_or("Source URL has no hostname.")?;
    let port = url.port_or_known_default().ok_or("Source URL has no valid port.")?;
    if let Ok(ip) = host.parse::<IpAddr>() {
        let safe = match ip {
            IpAddr::V4(value) => public_ipv4(value),
            IpAddr::V6(value) => public_ipv6(value),
        };
        return if safe { Ok(None) } else { Err("FRIDAY blocked a source that points to a private or reserved IP address.".into()) };
    }
    let addresses: Vec<SocketAddr> = tokio::net::lookup_host((host, port)).await
        .map_err(|_| "FRIDAY could not resolve the source website's hostname.".to_string())?
        .collect();
    if addresses.is_empty() || addresses.iter().any(|address| match address.ip() {
        IpAddr::V4(value) => !public_ipv4(value),
        IpAddr::V6(value) => !public_ipv6(value),
    }) {
        return Err("FRIDAY blocked a source hostname that resolves to a private or reserved network address.".into());
    }
    Ok(addresses.first().copied())
}

fn bounded_utf8(text: &str, max_bytes: usize) -> String {
    let mut output = String::new();
    for character in text.chars() {
        if output.len().saturating_add(character.len_utf8()) > max_bytes { break; }
        output.push(character);
    }
    output
}

fn extract_page_text(html: &str) -> (String, String) {
    static TITLE: OnceLock<Regex> = OnceLock::new();
    static ACTIVE: OnceLock<Regex> = OnceLock::new();
    let title_re = TITLE.get_or_init(|| Regex::new(r"(?is)<title[^>]*>(.*?)</title>").expect("valid title regex"));
    let active_re = ACTIVE.get_or_init(|| Regex::new(r"(?is)<(script|style|noscript)[^>]*>.*?</\1>").expect("valid active-content regex"));
    let title = title_re.captures(html).and_then(|capture| capture.get(1))
        .map(|value| strip_tags(value.as_str()).split_whitespace().collect::<Vec<_>>().join(" "))
        .unwrap_or_else(|| "Web source".into());
    let without_active = active_re.replace_all(html, " ");
    let text = strip_tags(&without_active).split_whitespace().collect::<Vec<_>>().join(" ");
    let bounded = bounded_utf8(&text, MAX_PAGE_TEXT_BYTES);
    (title.chars().take(200).collect(), bounded)
}

#[tauri::command]
pub async fn web_read_page(url: String) -> Result<WebPage, String> {
    let mut current = validate_page_url(&url)?;
    for redirect_count in 0..=MAX_REDIRECTS {
        let host = current.host_str().ok_or("Source URL has no hostname.")?;
        let pinned = pinned_public_address(&current).await?;
        let mut builder = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(8))
            .timeout(Duration::from_secs(18))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent("FRIDAY-Personal-Assistant/0.2 (web research)");
        if let Some(address) = pinned { builder = builder.resolve(host, address); }
        let client = builder.build().map_err(|_| "FRIDAY could not prepare the source connection.".to_string())?;
        let mut response = client.get(current.clone()).header(reqwest::header::ACCEPT, "text/html, text/plain;q=0.9").send().await
            .map_err(|_| "FRIDAY could not connect to this source webpage.".to_string())?;
        if response.status().is_redirection() {
            if redirect_count == MAX_REDIRECTS { return Err("This source redirected too many times.".into()); }
            let location = response.headers().get(reqwest::header::LOCATION)
                .and_then(|value| value.to_str().ok()).ok_or("The source returned an invalid redirect.")?;
            current = current.join(location).map_err(|_| "The source returned an invalid redirect URL.".to_string())?;
            validate_page_url(current.as_str())?;
            continue;
        }
        if !response.status().is_success() {
            return Err(format!("The source website returned HTTP {}.", response.status()));
        }
        let content_type = response.headers().get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()).unwrap_or("");
        if !(content_type.contains("text/html") || content_type.contains("text/plain")) {
            return Err("This source is not an HTML or plain-text webpage.".into());
        }
        if response.content_length().is_some_and(|length| length > MAX_PAGE_BYTES as u64) {
            return Err("This webpage exceeds FRIDAY's 1.5 MB reading limit.".into());
        }
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| "The source download was interrupted.".to_string())? {
            if body.len().saturating_add(chunk.len()) > MAX_PAGE_BYTES {
                return Err("This webpage exceeds FRIDAY's 1.5 MB reading limit.".into());
            }
            body.extend_from_slice(&chunk);
        }
        let html = String::from_utf8_lossy(&body);
        let (title, text) = extract_page_text(&html);
        if text.trim().is_empty() { return Err("FRIDAY could not extract readable text from this webpage.".into()); }
        return Ok(WebPage { title, url: current.to_string(), text });
    }
    Err("FRIDAY could not safely follow this source link.".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_private_and_reserved_network_addresses() {
        assert!(!public_ipv4(Ipv4Addr::new(10, 0, 0, 1)));
        assert!(!public_ipv4(Ipv4Addr::new(192, 168, 1, 2)));
        assert!(!public_ipv4(Ipv4Addr::new(100, 64, 0, 1)));
        assert!(public_ipv4(Ipv4Addr::new(1, 1, 1, 1)));
        assert!(!public_ipv6(Ipv6Addr::LOCALHOST));
        assert!(public_ipv6("2606:4700:4700::1111".parse().unwrap()));
    }

    #[test]
    fn extracted_page_text_respects_utf8_byte_limit() {
        assert_eq!(bounded_utf8("A🦊B", 5), "A🦊");
        assert_eq!(bounded_utf8("A🦊B", 4), "A");
    }

    #[test]
    fn source_reader_requires_https() {
        assert!(validate_page_url("http://example.com").is_err());
        assert!(validate_page_url("file:///C:/Windows").is_err());
        assert!(validate_page_url("https://example.com/article").is_ok());
    }

    #[test]
    fn extracts_page_title_and_visible_text_without_scripts() {
        let (title, text) = extract_page_text("<html><title>Friday Page</title><script>secret()</script><body>Hello <b>world</b></body></html>");
        assert_eq!(title, "Friday Page");
        assert!(text.len() <= MAX_PAGE_TEXT_BYTES);
        assert!(text.contains("Hello world"));
        assert!(!text.contains("secret"));
    }

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
        assert!("bad\nquery".contains(char::is_control));
    }
}
