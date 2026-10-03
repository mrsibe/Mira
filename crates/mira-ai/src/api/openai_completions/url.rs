//! Validation and endpoint construction for OpenAI-compatible base URLs.

use reqwest::Url;

use crate::error::AiError;

/// Validate a base URL and derive the Chat Completions endpoint from it.
///
/// The configured base URL is the API prefix, for example `https://api.deepseek.com/v1`; the
/// transport appends `/chat/completions` so a configured `/v1` is preserved. Userinfo is
/// rejected so a credential cannot be smuggled through the endpoint, and the URL never
/// reaches an error message.
pub(crate) fn endpoint_url(base_url: &str) -> Result<Url, AiError> {
    let url = Url::parse(base_url)
        .map_err(|_| AiError::InvalidBaseUrl("must be an absolute http(s) URL".to_string()))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(AiError::InvalidBaseUrl(
            "the scheme must be http or https".to_string(),
        ));
    }
    if url.host_str().map(str::is_empty).unwrap_or(true) {
        return Err(AiError::InvalidBaseUrl(
            "the URL must include a host".to_string(),
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(AiError::InvalidBaseUrl(
            "the URL must not include userinfo; credentials are supplied per request".to_string(),
        ));
    }
    if url.query().is_some() {
        return Err(AiError::InvalidBaseUrl(
            "the URL must not include a query string".to_string(),
        ));
    }
    if url.fragment().is_some() {
        return Err(AiError::InvalidBaseUrl(
            "the URL must not include a fragment".to_string(),
        ));
    }
    let path = url.path().trim_end_matches('/').to_string();
    if path.ends_with("/chat/completions") {
        return Err(AiError::InvalidBaseUrl(
            "the URL must be the API prefix without /chat/completions".to_string(),
        ));
    }

    let mut endpoint = url;
    endpoint.set_path(&format!("{path}/chat/completions"));
    Ok(endpoint)
}

/// Validate a base URL without building the endpoint.
pub(crate) fn validate_base_url(base_url: &str) -> Result<(), AiError> {
    endpoint_url(base_url).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn endpoint(base_url: &str) -> String {
        endpoint_url(base_url).expect("valid").to_string()
    }

    #[test]
    fn appends_chat_completions_and_preserves_the_configured_prefix() {
        assert_eq!(
            endpoint("https://api.deepseek.com"),
            "https://api.deepseek.com/chat/completions"
        );
        assert_eq!(
            endpoint("https://api.deepseek.com/v1"),
            "https://api.deepseek.com/v1/chat/completions"
        );
        assert_eq!(
            endpoint("https://api.deepseek.com/v1/"),
            "https://api.deepseek.com/v1/chat/completions"
        );
        assert_eq!(
            endpoint("http://127.0.0.1:11434/v1"),
            "http://127.0.0.1:11434/v1/chat/completions"
        );
    }

    #[test]
    fn rejects_unusable_base_urls() {
        for base_url in [
            "",
            "not a url",
            "ftp://api.example.com/v1",
            "https://",
            "https://user:secret@api.example.com/v1",
            "https://user@api.example.com/v1",
            "https://api.example.com/v1?api-version=1",
            "https://api.example.com/v1#fragment",
            "https://api.example.com/v1/chat/completions",
        ] {
            assert!(
                endpoint_url(base_url).is_err(),
                "expected {base_url:?} to be rejected"
            );
        }
    }

    #[test]
    fn error_messages_never_echo_the_url() {
        let error = endpoint_url("https://user:secret@api.example.com/v1")
            .expect_err("userinfo")
            .to_string();
        assert!(!error.contains("secret"));
        assert!(!error.contains("api.example.com"));
    }
}
