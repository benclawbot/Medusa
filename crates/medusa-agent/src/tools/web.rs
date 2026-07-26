use std::{io::Read, time::Duration};

use medusa_browser_client::network_policy::{
    ResolvedTarget, is_public_ip, resolve_public_target,
};
use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use reqwest::{
    Url,
    blocking::Client,
    header::{LOCATION, USER_AGENT},
    redirect::Policy,
};

const MAX_RESPONSE_BYTES: usize = 750_000;
const MAX_REDIRECTS: usize = 4;
const MAX_SEARCH_RESULTS: usize = 5;
const USER_AGENT_VALUE: &str = "Medusa/1.0 (public web research)";

pub(crate) fn search(
    query: &str,
    allowed_domains: Vec<String>,
    blocked_domains: Vec<String>,
) -> MedusaResult<String> {
    let query = query.trim();
    if query.is_empty() {
        return Err(invalid_web_input("query must not be empty"));
    }
    let allowed_domains = normalize_domains(allowed_domains)?;
    let blocked_domains = normalize_domains(blocked_domains)?;
    let search_query = if allowed_domains.is_empty() {
        query.to_owned()
    } else {
        format!(
            "{query} ({})",
            allowed_domains
                .iter()
                .map(|domain| format!("site:{domain}"))
                .collect::<Vec<_>>()
                .join(" OR ")
        )
    };
    let mut url = Url::parse("https://www.bing.com/search?format=rss")
        .map_err(|error| web_error(format!("could not construct search URL: {error}")))?;
    url.query_pairs_mut().append_pair("q", &search_query);
    let (_, body) = request(url)?;
    let results = parse_bing_rss(&String::from_utf8_lossy(&body))
        .into_iter()
        .filter(|result| {
            let Ok(url) = Url::parse(&result.url) else {
                return false;
            };
            (allowed_domains.is_empty() || matches_any_domain(&url, &allowed_domains))
                && !matches_any_domain(&url, &blocked_domains)
        })
        .take(MAX_SEARCH_RESULTS)
        .collect::<Vec<_>>();

    if results.is_empty() {
        return Ok(format!("No public web results found for: {query}"));
    }
    let mut output = format!("Web search results for: {query}");
    for (index, result) in results.iter().enumerate() {
        output.push_str(&format!(
            "\n\n{}. {}\n{}\n{}",
            index + 1,
            result.title,
            result.url,
            result.snippet
        ));
    }
    Ok(output)
}

pub(crate) fn fetch(url: &str, prompt: Option<&str>) -> MedusaResult<String> {
    let (final_url, body) = request(parse_public_url(url)?)?;
    let content = readable_text(&String::from_utf8_lossy(&body));
    if content.is_empty() {
        return Ok(format!(
            "Fetched {final_url} but it did not contain readable text."
        ));
    }
    let requested = prompt
        .map(str::trim)
        .filter(|prompt| !prompt.is_empty())
        .map(|prompt| format!("\nRequested extraction: {prompt}"))
        .unwrap_or_default();
    Ok(format!("Fetched: {final_url}{requested}\n\n{content}"))
}

fn request(mut url: Url) -> MedusaResult<(Url, Vec<u8>)> {
    for _ in 0..=MAX_REDIRECTS {
        let target = resolve_url(&url).map_err(invalid_web_input)?;
        let client = Client::builder()
            .redirect(Policy::none())
            .timeout(Duration::from_secs(15))
            .resolve_to_addrs(target.host(), target.addresses())
            .build()
            .map_err(|error| web_error(format!("could not initialize web client: {error}")))?;
        let mut response = client
            .get(url.clone())
            .header(USER_AGENT, USER_AGENT_VALUE)
            .send()
            .map_err(|error| web_error(format!("web request failed: {error}")))?;
        if response.status().is_redirection() {
            let location = response
                .headers()
                .get(LOCATION)
                .and_then(|value| value.to_str().ok())
                .ok_or_else(|| web_error("web redirect did not provide a valid Location header"))?;
            url = url
                .join(location)
                .map_err(|error| web_error(format!("invalid redirect target: {error}")))?;
            resolve_url(&url).map_err(invalid_web_input)?;
            continue;
        }
        if !response.status().is_success() {
            let status = response.status();
            let body = read_limited(&mut response).unwrap_or_default();
            return Err(web_error(format!(
                "web request returned HTTP {status}: {}",
                truncate_inline(&String::from_utf8_lossy(&body), 400)
            )));
        }
        if response
            .content_length()
            .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
        {
            return Err(web_error(format!(
                "web response exceeds the {MAX_RESPONSE_BYTES} byte limit"
            )));
        }
        let body = read_limited(&mut response)?;
        return Ok((url, body));
    }
    Err(web_error("web request exceeded the redirect limit"))
}

fn resolve_url(url: &Url) -> Result<ResolvedTarget, String> {
    let host = url
        .host_str()
        .ok_or_else(|| "web URL must include a host".to_owned())?;
    resolve_public_target(
        url.scheme(),
        url.username(),
        url.password().is_some(),
        url.port(),
        host,
        url.port_or_known_default().unwrap_or(443),
    )
}

fn read_limited(response: &mut impl Read) -> MedusaResult<Vec<u8>> {
    let mut body = Vec::new();
    response
        .take((MAX_RESPONSE_BYTES + 1) as u64)
        .read_to_end(&mut body)
        .map_err(|error| web_error(format!("could not read web response: {error}")))?;
    if body.len() > MAX_RESPONSE_BYTES {
        return Err(web_error(format!(
            "web response exceeds the {MAX_RESPONSE_BYTES} byte limit"
        )));
    }
    Ok(body)
}

fn parse_public_url(value: &str) -> MedusaResult<Url> {
    let url = Url::parse(value.trim())
        .map_err(|error| invalid_web_input(format!("invalid web URL: {error}")))?;
    resolve_url(&url).map_err(invalid_web_input)?;
    Ok(url)
}

#[derive(Debug, Eq, PartialEq)]
struct SearchResult {
    title: String,
    url: String,
    snippet: String,
}

fn parse_bing_rss(feed: &str) -> Vec<SearchResult> {
    feed.split("<item>")
        .skip(1)
        .filter_map(|item| item.split_once("</item>").map(|(item, _)| item))
        .filter_map(|item| {
            Some(SearchResult {
                title: readable_text(&tag_value(item, "title")?),
                url: readable_text(&tag_value(item, "link")?),
                snippet: readable_text(&tag_value(item, "description").unwrap_or_default()),
            })
        })
        .filter(|result| !result.title.is_empty() && !result.url.is_empty())
        .collect()
}

fn tag_value(value: &str, tag: &str) -> Option<String> {
    let start = format!("<{tag}>");
    let end = format!("</{tag}>");
    let value = value.split_once(&start)?.1.split_once(&end)?.0.trim();
    Some(
        value
            .strip_prefix("<![CDATA[")
            .and_then(|value| value.strip_suffix("]]>") )
            .unwrap_or(value)
            .to_owned(),
    )
}

fn normalize_domains(domains: Vec<String>) -> MedusaResult<Vec<String>> {
    domains
        .into_iter()
        .map(|domain| {
            let domain = domain.trim().trim_start_matches('.').to_ascii_lowercase();
            if domain.is_empty()
                || domain.contains(['/', ':', '@'])
                || domain.split('.').any(|label| label.is_empty())
            {
                return Err(invalid_web_input(format!("invalid web domain: {domain}")));
            }
            Ok(domain)
        })
        .collect()
}

fn matches_any_domain(url: &Url, domains: &[String]) -> bool {
    let Some(host) = url.host_str() else {
        return false;
    };
    let host = host.trim_start_matches("www.").to_ascii_lowercase();
    domains.iter().any(|domain| {
        let domain = domain.trim_start_matches("www.");
        host == domain || host.ends_with(&format!(".{domain}"))
    })
}

fn readable_text(value: &str) -> String {
    let mut text = value.to_owned();
    for tag in ["script", "style", "noscript"] {
        text = remove_tagged_sections(&text, tag);
    }
    for tag in ["br", "p", "div", "li", "h1", "h2", "h3", "h4", "tr"] {
        text = text.replace(&format!("<{tag}>"), "\n");
        text = text.replace(&format!("</{tag}>"), "\n");
    }
    let mut plain = String::with_capacity(text.len());
    let mut inside_tag = false;
    for character in text.chars() {
        match character {
            '<' => inside_tag = true,
            '>' => inside_tag = false,
            _ if !inside_tag => plain.push(character),
            _ => {}
        }
    }
    decode_entities(&plain)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn remove_tagged_sections(value: &str, tag: &str) -> String {
    let lowercase = value.to_ascii_lowercase();
    let start_tag = format!("<{tag}");
    let end_tag = format!("</{tag}>");
    let mut result = String::new();
    let mut offset = 0;
    while let Some(start) = lowercase[offset..].find(&start_tag) {
        let start = offset + start;
        result.push_str(&value[offset..start]);
        let after_start = lowercase[start..]
            .find('>')
            .map(|index| start + index + 1)
            .unwrap_or(value.len());
        let Some(end) = lowercase[after_start..].find(&end_tag) else {
            return result;
        };
        offset = after_start + end + end_tag.len();
    }
    result.push_str(&value[offset..]);
    result
}

fn decode_entities(value: &str) -> String {
    value
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
}

fn truncate_inline(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_owned();
    }
    value
        .chars()
        .take(max_chars)
        .chain(std::iter::once('\n'))
        .chain("[truncated]".chars())
        .collect()
}

fn invalid_web_input(message: impl Into<String>) -> MedusaError {
    MedusaError::new(
        ErrorCode::InvalidConfiguration,
        ErrorCategory::Validation,
        message,
    )
}

fn web_error(message: impl Into<String>) -> MedusaError {
    MedusaError::new(
        ErrorCode::DependencyUnavailable,
        ErrorCategory::Transient,
        message,
    )
    .with_retryable(true)
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    use super::*;

    #[test]
    fn parses_and_filters_bing_rss_results() {
        let feed = r#"
            <rss><channel><item><title>Example &amp; One</title><link>https://docs.example.com/one</link><description><![CDATA[First <b>result</b>]]></description></item>
            <item><title>Example Two</title><link>https://other.example.net/two</link><description>Second result</description></item></channel></rss>
        "#;
        let results = parse_bing_rss(feed);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].title, "Example & One");
        assert_eq!(results[0].snippet, "First result");
        let allowed = vec!["example.com".to_owned()];
        assert!(matches_any_domain(
            &Url::parse(&results[0].url).expect("result URL"),
            &allowed
        ));
        assert!(!matches_any_domain(
            &Url::parse(&results[1].url).expect("result URL"),
            &allowed
        ));
    }

    #[test]
    fn readable_text_removes_markup_and_active_content() {
        assert_eq!(
            readable_text("<h1>Title</h1><script>bad()</script><p>One &amp; two</p>"),
            "Title\nOne & two"
        );
    }

    #[test]
    fn shared_policy_rejects_private_and_mapped_addresses() {
        for address in [
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
            IpAddr::V6(Ipv6Addr::LOCALHOST),
            "::ffff:127.0.0.1".parse().expect("mapped loopback"),
        ] {
            assert!(!is_public_ip(address), "{address}");
        }
        assert!(is_public_ip(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8))));
    }

    #[test]
    fn domains_are_normalized_and_validated() {
        assert_eq!(
            normalize_domains(vec!["Docs.Example.com".to_owned()]),
            Ok(vec!["docs.example.com".to_owned()])
        );
        assert!(normalize_domains(vec!["https://example.com".to_owned()]).is_err());
    }

    #[test]
    #[ignore = "requires public network access"]
    fn live_public_search_returns_result_text() {
        let output =
            search("Rust programming language", Vec::new(), Vec::new()).expect("search public web");
        assert!(output.starts_with("Web search results for:"));
    }

    #[test]
    #[ignore = "requires public network access"]
    fn live_public_fetch_returns_readable_text() {
        let output = fetch("https://www.rust-lang.org", Some("identify the page"))
            .expect("fetch public web");
        assert!(output.starts_with("Fetched: https://"));
    }
}
