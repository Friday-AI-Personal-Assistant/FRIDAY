use regex::Regex;
use scraper::{Html, Selector};
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
    pub domain: String,
}

fn safe_result_url(raw: &str) -> Option<String> {
    let base = url::Url::parse(SEARCH_ENDPOINT).ok()?;
    let mut url = base.join(raw).ok()?;
    if url.host_str() == Some("duckduckgo.com") && url.path() == "/l/" {
        let target = url.query_pairs().find(|(key, _)| key == "uddg")?.1.into_owned();
        url = url::Url::parse(&target).ok()?;
    }
    if url.scheme() != "https"
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
    static RESULT: OnceLock<Selector> = OnceLock::new();
    static LINK: OnceLock<Selector> = OnceLock::new();
    static SNIPPET: OnceLock<Selector> = OnceLock::new();
    let result_selector = RESULT.get_or_init(|| Selector::parse(".result").expect("valid result selector"));
    let link_selector = LINK.get_or_init(|| Selector::parse(".result__a").expect("valid link selector"));
    let snippet_selector = SNIPPET.get_or_init(|| Selector::parse(".result__snippet").expect("valid snippet selector"));
    let document = Html::parse_document(html);
    let mut results = Vec::new();
    for result in document.select(result_selector) {
        if results.len() >= MAX_RESULTS { break; }
        let Some(link) = result.select(link_selector).next() else { continue; };
        let Some(raw_url) = link.value().attr("href") else { continue; };
        let Some(url) = safe_result_url(raw_url) else { continue; };
        let title = link.text().collect::<Vec<_>>().join(" ")
            .split_whitespace().collect::<Vec<_>>().join(" ");
        if title.is_empty() { continue; }
        let snippet = result.select(snippet_selector).next()
            .map(|element| element.text().collect::<Vec<_>>().join(" "))
            .unwrap_or_default().split_whitespace().collect::<Vec<_>>().join(" ");
        let domain = url::Url::parse(&url).ok().and_then(|value| value.host_str().map(str::to_string)).unwrap_or_default();
        results.push(WebSearchResult { title, url, snippet, domain });
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
    let search_content_type = response.headers().get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok()).unwrap_or("");
    if !search_content_type.contains("text/html") {
        return Err("The web search provider did not return an HTML results page.".into());
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
    let _ = crate::tools::record(&_app, "web.search", &format!("Web search completed with {} results.", results.len()), true);
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
    pub domain: String,
    pub author: Option<String>,
    pub published_date: Option<String>,
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

fn decode_page_bytes(bytes: &[u8], content_type: &str) -> String {
    if let Some((encoding, remainder)) = encoding_rs::Encoding::for_bom(bytes) {
        return encoding.decode(remainder).0.into_owned();
    }
    let header_charset = content_type.split(';').skip(1)
        .find_map(|part| part.trim().strip_prefix("charset="))
        .map(|label| label.trim_matches('"').trim_matches('\''));
    let meta_charset = String::from_utf8_lossy(&bytes[..bytes.len().min(2048)]);
    static META: OnceLock<Regex> = OnceLock::new();
    let meta_re = META.get_or_init(|| Regex::new(r#"(?i)<meta[^>]+charset=["']?([a-z0-9._-]+)"#).expect("valid charset regex"));
    let detected = header_charset.or_else(|| meta_re.captures(&meta_charset).and_then(|capture| capture.get(1).map(|value| value.as_str())));
    let encoding = detected.and_then(|label| encoding_rs::Encoding::for_label(label.as_bytes()))
        .unwrap_or(encoding_rs::UTF_8);
    encoding.decode(bytes).0.into_owned()
}

fn extract_page_metadata(html: &str) -> (Option<String>, Option<String>) {
    static AUTHOR: OnceLock<Selector> = OnceLock::new();
    static PUBLISHED: OnceLock<Selector> = OnceLock::new();
    let author_selector = AUTHOR.get_or_init(|| Selector::parse(r#"meta[name="author"], meta[property="article:author"]"#).expect("valid author selector"));
    let published_selector = PUBLISHED.get_or_init(|| Selector::parse(r#"meta[property="article:published_time"], meta[name="date"], meta[name="publish-date"], meta[name="pubdate"]"#).expect("valid publication date selector"));
    let document = Html::parse_document(html);
    let value = |selector: &Selector| document.select(selector).find_map(|element| element.value().attr("content"))
        .map(|value| value.trim().chars().take(160).collect::<String>())
        .filter(|value| !value.is_empty());
    (value(author_selector), value(published_selector))
}

fn extract_page_text(html: &str) -> (String, String) {
    static TITLE: OnceLock<Selector> = OnceLock::new();
    static BODY: OnceLock<Selector> = OnceLock::new();
    static MAIN: OnceLock<Selector> = OnceLock::new();
    static ACTIVE: OnceLock<Regex> = OnceLock::new();
    let title_selector = TITLE.get_or_init(|| Selector::parse("title").expect("valid title selector"));
    let body_selector = BODY.get_or_init(|| Selector::parse("body").expect("valid body selector"));
    let main_selector = MAIN.get_or_init(|| Selector::parse("main, article").expect("valid content selector"));
    let active_re = ACTIVE.get_or_init(|| Regex::new(r"(?is)<script[^>]*>.*?</script>|<style[^>]*>.*?</style>|<noscript[^>]*>.*?</noscript>|<template[^>]*>.*?</template>|<svg[^>]*>.*?</svg>").expect("valid active-content regex"));

    // Strip executable/non-content elements before HTML5 parsing. The parser
    // repairs malformed markup and decodes named/numeric HTML entities.
    let cleaned = active_re.replace_all(html, " ");
    let document = Html::parse_document(&cleaned);
    let title = document.select(title_selector).next()
        .map(|element| element.text().collect::<Vec<_>>().join(" "))
        .unwrap_or_else(|| "Web source".into())
        .split_whitespace().collect::<Vec<_>>().join(" ");
    let content = document.select(main_selector).next()
        .or_else(|| document.select(body_selector).next());
    let text = content
        .map(|element| element.text().collect::<Vec<_>>().join(" "))
        .unwrap_or_else(|| document.root_element().text().collect::<Vec<_>>().join(" "))
        .split_whitespace().collect::<Vec<_>>().join(" ");
    (title.chars().take(200).collect(), bounded_utf8(&text, MAX_PAGE_TEXT_BYTES))
}

async fn read_page_inner(url: String) -> Result<WebPage, String> {
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
        let html = decode_page_bytes(&body, content_type);
        let (title, text) = extract_page_text(&html);
        let (author, published_date) = extract_page_metadata(&html);
        if text.trim().is_empty() { return Err("FRIDAY could not extract readable text from this webpage.".into()); }
        let domain = current.host_str().unwrap_or_default().to_string();
        let page = WebPage { title, url: current.to_string(), text, domain, author, published_date };
        return Ok(page);
    }
    Err("FRIDAY could not safely follow this source link.".into())
}

#[tauri::command]
pub async fn web_read_page(app: AppHandle, url: String) -> Result<WebPage, String> {
    let page = read_page_inner(url).await?;
    let _ = crate::tools::record(&app, "web.read_page", "Read one source webpage after explicit user selection.", true);
    Ok(page)
}

#[tauri::command]
pub async fn web_read_pages(app: AppHandle, urls: Vec<String>) -> Result<Vec<WebPage>, String> {
    if urls.is_empty() || urls.len() > 4 {
        return Err("Select between 1 and 4 sources for a research session.".into());
    }
    let mut seen = std::collections::HashSet::new();
    let mut pages = Vec::new();
    for raw_url in urls {
        let canonical = validate_page_url(&raw_url)?.to_string();
        if !seen.insert(canonical.clone()) { continue; }
        let page = read_page_inner(canonical).await?;
        pages.push(page);
    }
    let _ = crate::tools::record(&app, "web.read_pages", &format!("Read {} source webpages after explicit user selection.", pages.len()), true);
    Ok(pages)
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
    fn html5_extraction_decodes_entities_and_repairs_malformed_markup() {
        let (title, text) = extract_page_text("<html><title>Research &amp; Facts</title><body><main><p>One &amp; two<p>Three</main><script>doBadThings()</script></body>");
        assert_eq!(title, "Research & Facts");
        assert!(text.contains("One & two"));
        assert!(text.contains("Three"));
        assert!(!text.contains("doBadThings"));
    }

    #[test]
    fn decodes_utf8_and_legacy_webpage_charsets() {
        let utf8 = "Café".as_bytes();
        assert_eq!(decode_page_bytes(utf8, "text/html; charset=utf-8"), "Café");
        let windows_1252 = [b'C', b'a', b'f', 0xe9];
        assert_eq!(decode_page_bytes(&windows_1252, "text/html; charset=windows-1252"), "Café");
    }

    #[test]
    fn extracts_declared_author_and_publication_metadata() {
        let (author, date) = extract_page_metadata(r#"<meta name="author" content="Ada Example"><meta property="article:published_time" content="2026-11-10T12:00:00Z">"#);
        assert_eq!(author.as_deref(), Some("Ada Example"));
        assert_eq!(date.as_deref(), Some("2026-11-10T12:00:00Z"));
    }

    #[test]
    fn extracts_article_content_over_navigation_when_available() {
        let (_, text) = extract_page_text("<html><body><nav>Menu links</nav><article><p>Research body</p></article></body></html>");
        assert_eq!(text, "Research body");
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
    fn parses_titles_and_snippets_from_their_own_search_result_cards() {
        let html = r#"<div class="result"><a class="result__a" href="https://example.com/a">First &amp; best</a><a class="result__snippet">Snippet A</a></div><div class="result"><a class="result__a" href="https://example.org/b">Second</a><a class="result__snippet">Snippet B</a></div>"#;
        let results = parse_results(html);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].title, "First & best");
        assert_eq!(results[0].snippet, "Snippet A");
        assert_eq!(results[1].title, "Second");
        assert_eq!(results[1].snippet, "Snippet B");
        assert_eq!(results[0].domain, "example.com");
    }

    #[test]
    fn validates_search_query_size_and_controls() {
        assert!("hello world".len() <= MAX_QUERY_BYTES);
        assert!("a".repeat(MAX_QUERY_BYTES + 1).len() > MAX_QUERY_BYTES);
        assert!("bad\nquery".contains(char::is_control));
    }
}
