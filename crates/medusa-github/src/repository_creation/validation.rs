use crate::*;

use super::RepositoryVisibility;

pub(crate) fn validate_owner(value: &str) -> MedusaResult<()> {
    if value.is_empty() || value.len() > 100 {
        return Err(invalid_input(
            "repository owner must contain 1 to 100 characters",
        ));
    }
    if value.starts_with('-')
        || value.ends_with('-')
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return Err(invalid_input(
            "repository owner may contain only ASCII letters, digits, and interior hyphens",
        ));
    }
    Ok(())
}

pub(crate) fn validate_repository_name(value: &str) -> MedusaResult<()> {
    if value.is_empty() || value.len() > 100 || value == "." || value == ".." {
        return Err(invalid_input(
            "repository name must contain 1 to 100 characters",
        ));
    }
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(invalid_input(
            "repository name may contain only ASCII letters, digits, hyphens, underscores, and periods",
        ));
    }
    Ok(())
}

pub(crate) fn validate_repository_identity(value: &str) -> MedusaResult<()> {
    let mut parts = value.split('/');
    let owner = parts.next().unwrap_or_default();
    let name = parts.next().unwrap_or_default();
    if parts.next().is_some() {
        return Err(invalid_input(
            "repository identity must use owner/name form",
        ));
    }
    validate_owner(owner)?;
    validate_repository_name(name)
}

pub(crate) fn validate_branch(value: &str) -> MedusaResult<()> {
    let invalid_component = value.split('/').any(|component| {
        component.is_empty() || component.starts_with('.') || component.ends_with(".lock")
    });
    if value.is_empty()
        || value.len() > 255
        || value == "@"
        || value.starts_with('-')
        || value.starts_with('/')
        || value.ends_with('.')
        || value.ends_with('/')
        || value.contains("..")
        || value.contains("//")
        || value.contains("@{")
        || invalid_component
        || value.bytes().any(|byte| {
            byte.is_ascii_control()
                || byte.is_ascii_whitespace()
                || matches!(byte, b'~' | b'^' | b':' | b'?' | b'*' | b'[' | b'\\')
        })
    {
        return Err(invalid_input(
            "default branch name is not a safe Git branch reference",
        ));
    }
    Ok(())
}

pub(crate) fn validate_optional_text(
    field: &str,
    value: Option<&str>,
    maximum: usize,
) -> MedusaResult<()> {
    let Some(value) = value else {
        return Ok(());
    };
    if value.trim().is_empty()
        || value.len() > maximum
        || value
            .bytes()
            .any(|byte| byte == 0 || byte.is_ascii_control())
    {
        return Err(invalid_input(format!("{field} is invalid or too large")));
    }
    Ok(())
}

pub(crate) fn validate_optional_template(field: &str, value: Option<&str>) -> MedusaResult<()> {
    let Some(value) = value else {
        return Ok(());
    };
    let value = value.trim();
    if value.is_empty()
        || value.len() > 100
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(invalid_input(format!("{field} is invalid")));
    }
    Ok(())
}

pub(crate) fn validate_optional_url(value: Option<&str>) -> MedusaResult<()> {
    let Some(value) = value else {
        return Ok(());
    };
    let value = value.trim();
    if value.len() > 2048
        || !(value.starts_with("https://") || value.starts_with("http://"))
        || value
            .bytes()
            .any(|byte| byte.is_ascii_control() || byte.is_ascii_whitespace())
    {
        return Err(invalid_input("homepage must be a valid HTTP or HTTPS URL"));
    }
    Ok(())
}

pub(crate) fn parse_visibility(value: &str) -> MedusaResult<RepositoryVisibility> {
    match value.trim().to_ascii_lowercase().as_str() {
        "public" => Ok(RepositoryVisibility::Public),
        "private" => Ok(RepositoryVisibility::Private),
        "internal" => Ok(RepositoryVisibility::Internal),
        _ => Err(internal_error(
            "GitHub returned an unknown repository visibility",
        )),
    }
}

pub(crate) fn repository_missing(stderr: &str) -> bool {
    let value = stderr.to_ascii_lowercase();
    value.contains("could not resolve to a repository")
        || value.contains("not found")
        || value.contains("404")
}

pub(crate) fn sanitize_external_error(stderr: &str) -> String {
    let value = stderr.trim();
    if value.contains("ghp_")
        || value.contains("github_pat_")
        || value.to_ascii_lowercase().contains("authorization: bearer")
    {
        "external command failed with redacted credential-like output".to_owned()
    } else if value.chars().count() > 2048 {
        format!("{}…", value.chars().take(2048).collect::<String>())
    } else {
        value.to_owned()
    }
}

pub(crate) fn percent_encode_path_segment(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}
