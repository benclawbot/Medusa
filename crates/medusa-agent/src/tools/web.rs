use std::{collections::BTreeSet, io::Read, time::Duration};

use medusa_capabilities::{ResolvedTarget, resolve_public_target};
use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use reqwest::{
    Url,
    blocking::Client,
    header::{LOCATION, USER_AGENT},
    redirect::Policy,
};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

const MAX_RESPONSE_BYTES: usize = 750_000;
const MAX_REDIRECTS: usize = 4;
const MAX_SEARCH_RESULTS: usize = 5;
const MAX_OUTPUT_BYTES: usize = 48_000;
const MAX_EXCERPT_LINES: usize = 240;
const USER_AGENT_VALUE: &str = "Medusa/1.0 (public web research)";

pub(crate) fn search(
    query: &str,
    allowed_domains: Vec<String>,
    blocked_domains: Vec<String>,
) -> MedusaResult<String> {
    let query = query.trim();
    if query.is_empty() {
        return Err(invalid_input("query must not be empty"));
    }
    let allowed = normalize_domains(allowed_domains)?;
    let blocked = normalize_domains(blocked_domains)?;
    let search_query = if allowed.is_empty() {
        query.to_owned()
    } else {
        format!(
            "{query} ({})",
            allowed
                .iter()
                .map(|domain| format!("site:{domain}"))
                .collect::<Vec<_>>()
                .join(" OR ")
        )
    };
    let mut url = Url::parse("https://www.bing.com/search?format=rss")
        .map_err(|error| web_error(format!("could not construct search URL: {error}")))?;
    url.query_pairs_mut().append_pair("q", &search_query);
    let response = request(url)?;
    let results = parse_bing_rss(&String::from_utf8_lossy(&response.body))
        .into_iter()
        .filter(|result| {
            Url::parse(&result.url).is_ok_and(|url| {
                (allowed.is_empty() || matches_domain(&url, &allowed))
                    && !matches_domain(&url, &blocked)
            })
        })
        .take(MAX_SEARCH_RESULTS)
        .collect::<Vec<_>>();
    let retrieved_at = retrieved_at();
    let (output, status) = if results.is_empty() {
        (
            format!(
                "web_search status=no_results query={query:?} source=bing_rss retrieved_at={retrieved_at} response_truncated={}\nNo public web results found for this query; do not infer that the source is unavailable without trying another bounded search.",
                response.content_truncated
            ),
            "no_results",
        )
    } else {
        let mut output = format!(
            "web_search status=results query={query:?} source=bing_rss retrieved_at={retrieved_at} response_truncated={} results={}",
            response.content_truncated,
            results.len()
        );
        for (index, result) in results.iter().enumerate() {
            output.push_str(&format!(
                "\n\n{}. {}\n{}\n{}",
                index + 1,
                result.title,
                result.url,
                result.snippet
            ));
        }
        (output, "results")
    };
    let (output, output_truncated) = bounded_output(output);
    Ok(format!(
        "{output}\noutput_status={status} output_truncated={output_truncated}"
    ))
}

pub(crate) fn fetch(url: &str, prompt: Option<&str>) -> MedusaResult<String> {
    let parsed = Url::parse(url.trim())
        .map_err(|error| invalid_input(format!("invalid web URL: {error}")))?;
    let response = request(parsed)?;
    let raw = String::from_utf8_lossy(&response.body);
    let title = title_from_html(&raw).unwrap_or_else(|| "untitled".to_owned());
    let content = readable_text(&raw);
    let (excerpt, extraction_status, excerpt_truncated) = requested_excerpt(&content, prompt);
    let output = if excerpt.is_empty() {
        format!(
            "web_fetch requested_url={url:?} final_url={:?} retrieved_at={} title={title:?} content_truncated={} extraction_status={extraction_status} untrusted_page_content=true\nNo readable text was available for the requested extraction.",
            response.final_url,
            retrieved_at(),
            response.content_truncated
        )
    } else {
        format!(
            "web_fetch requested_url={url:?} final_url={:?} retrieved_at={} title={title:?} content_truncated={} extraction_status={extraction_status} untrusted_page_content=true\n\n{excerpt}",
            response.final_url,
            retrieved_at(),
            response.content_truncated
        )
    };
    let (output, output_truncated) = bounded_output(output);
    Ok(format!(
        "{output}\nexcerpt_truncated={excerpt_truncated} output_truncated={output_truncated}"
    ))
}

struct WebResponse {
    final_url: Url,
    body: Vec<u8>,
    content_truncated: bool,
}

fn request(mut url: Url) -> MedusaResult<WebResponse> {
    for _ in 0..=MAX_REDIRECTS {
        let target = resolve_url(&url).map_err(invalid_input)?;
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
            resolve_url(&url).map_err(invalid_input)?;
            continue;
        }
        if !response.status().is_success() {
            return Err(web_error(format!(
                "web request returned HTTP {}",
                response.status()
            )));
        }
        let (body, content_truncated) = read_limited(&mut response)?;
        return Ok(WebResponse {
            final_url: url,
            body,
            content_truncated,
        });
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

fn read_limited(response: &mut impl Read) -> MedusaResult<(Vec<u8>, bool)> {
    let mut body = Vec::new();
    response
        .take((MAX_RESPONSE_BYTES + 1) as u64)
        .read_to_end(&mut body)
        .map_err(|error| web_error(format!("could not read web response: {error}")))?;
    let truncated = body.len() > MAX_RESPONSE_BYTES;
    if truncated {
        body.truncate(MAX_RESPONSE_BYTES);
    }
    Ok((body, truncated))
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
    let value = value.strip_prefix("<![CDATA[").unwrap_or(value);
    Some(value.strip_suffix("]]>").unwrap_or(value).to_owned())
}

fn title_from_html(value: &str) -> Option<String> {
    let title = tag_value(value, "title")?;
    let title = readable_text(&title);
    (!title.is_empty()).then_some(title)
}

fn normalize_domains(domains: Vec<String>) -> MedusaResult<Vec<String>> {
    domains
        .into_iter()
        .map(|domain| {
            let domain = domain.trim().trim_start_matches('.').to_ascii_lowercase();
            if domain.is_empty()
                || domain.contains(['/', ':', '@'])
                || domain.split('.').any(str::is_empty)
            {
                return Err(invalid_input(format!("invalid web domain: {domain}")));
            }
            Ok(domain)
        })
        .collect()
}

fn matches_domain(url: &Url, domains: &[String]) -> bool {
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
    let mut plain = String::with_capacity(value.len());
    let mut inside_tag = false;
    for character in value.chars() {
        match character {
            '<' => {
                inside_tag = true;
                plain.push('\n');
            }
            '>' => {
                inside_tag = false;
                plain.push('\n');
            }
            _ if !inside_tag => plain.push(character),
            _ => {}
        }
    }
    plain
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn requested_excerpt(content: &str, prompt: Option<&str>) -> (String, &'static str, bool) {
    let lines = content.lines().collect::<Vec<_>>();
    let Some(prompt) = prompt.map(str::trim).filter(|value| !value.is_empty()) else {
        let (excerpt, truncated) = bounded_lines(&lines);
        return (excerpt, "not_requested", truncated);
    };
    let terms = prompt
        .split(|character: char| !character.is_alphanumeric())
        .map(str::trim)
        .filter(|term| term.chars().count() >= 3)
        .map(str::to_ascii_lowercase)
        .collect::<BTreeSet<_>>();
    if terms.is_empty() {
        return (String::new(), "no_matching_excerpt", false);
    }
    let matching_lines = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| {
            let lower = line.to_ascii_lowercase();
            terms.iter().any(|term| lower.contains(term))
        })
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    if matching_lines.is_empty() {
        return (String::new(), "no_matching_excerpt", false);
    }
    let mut selected = BTreeSet::new();
    for index in matching_lines {
        let start = index.saturating_sub(2);
        let end = (index + 2).min(lines.len().saturating_sub(1));
        selected.extend(start..=end);
    }
    let selected_lines = selected
        .into_iter()
        .map(|index| lines[index])
        .collect::<Vec<_>>();
    let (excerpt, truncated) = bounded_lines(&selected_lines);
    (excerpt, "matched", truncated)
}

fn bounded_lines(lines: &[&str]) -> (String, bool) {
    let truncated = lines.len() > MAX_EXCERPT_LINES;
    let end = lines.len().min(MAX_EXCERPT_LINES);
    (lines[..end].join("\n"), truncated)
}

fn bounded_output(value: String) -> (String, bool) {
    if value.len() <= MAX_OUTPUT_BYTES {
        return (value, false);
    }
    let suffix = "\n[web output truncated]";
    let prefix = safe_prefix(&value, MAX_OUTPUT_BYTES.saturating_sub(suffix.len()));
    (format!("{prefix}{suffix}"), true)
}

fn safe_prefix(value: &str, maximum_bytes: usize) -> &str {
    if value.len() <= maximum_bytes {
        return value;
    }
    let mut end = maximum_bytes;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

fn retrieved_at() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| OffsetDateTime::now_utc().unix_timestamp().to_string())
}

fn invalid_input(message: impl Into<String>) -> MedusaError {
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

    use medusa_capabilities::is_public_ip;

    use super::*;

    #[test]
    fn parses_bing_rss_results() {
        let feed = "<item><title>Example &amp; One</title><link>https://example.com</link><description>Result</description></item>";
        let results = parse_bing_rss(feed);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "Example & One");
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
    fn oversized_web_output_is_bounded_without_claiming_completeness() {
        let (output, truncated) = bounded_output("x".repeat(MAX_OUTPUT_BYTES + 64));
        assert!(truncated);
        assert!(output.len() <= MAX_OUTPUT_BYTES);
        assert!(output.contains("web output truncated"));
    }

    #[test]
    fn missing_extraction_match_is_reported_instead_of_fabricated() {
        let (excerpt, status, truncated) = requested_excerpt(
            "A page about bounded public research.",
            Some("quantum gravity"),
        );
        assert!(excerpt.is_empty());
        assert_eq!(status, "no_matching_excerpt");
        assert!(!truncated);
    }

    #[test]
    fn requested_fetch_text_is_explicitly_selected_and_titled() {
        let raw = "<html><head><title>Official Guide</title></head><body><p>Install the tool.</p><p>Use the bounded request.</p></body></html>";
        let title = title_from_html(raw).expect("title");
        let text = readable_text(raw);
        let (excerpt, status, _) = requested_excerpt(&text, Some("bounded request"));
        assert_eq!(title, "Official Guide");
        assert!(excerpt.contains("Use the bounded request."));
        assert_eq!(status, "matched");
    }
}
