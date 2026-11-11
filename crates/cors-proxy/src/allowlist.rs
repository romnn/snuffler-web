//! The exact HTTP targets the proxy forwards to, as configured by its caller.

use reqwest::Url;

use crate::ProxyError;

/// Why a configured HTTP target rule was rejected.
#[derive(Debug, thiserror::Error)]
pub enum TargetError {
    /// The rule is not an absolute URL.
    #[error("invalid allowed URL target")]
    InvalidUrl(#[from] url::ParseError),
    /// The URL has no host, as in `file:///path`.
    #[error("allowed URL target must include a host")]
    MissingHost,
    /// The URL names no port and its scheme has no default one.
    #[error("allowed URL target must have a known or explicit port")]
    UnknownPort,
}

/// Exact HTTP targets accepted by a [`crate::Proxy`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Allowlist {
    rules: Vec<AllowedHttpTarget>,
}

impl Allowlist {
    /// Allows exactly `rules`; an empty list refuses every request.
    pub fn new<I>(rules: I) -> Self
    where
        I: IntoIterator<Item = AllowedHttpTarget>,
    {
        Self {
            rules: rules.into_iter().collect(),
        }
    }

    pub(crate) fn check(&self, url: &Url) -> Result<(), ProxyError> {
        if self.rules.iter().any(|rule| rule.matches(url)) {
            Ok(())
        } else {
            // Name only the parts the allowlist compares, leaving out the query.
            Err(ProxyError::NotAllowed(format!(
                "{}://{}{}",
                url.scheme(),
                url.host_str().unwrap_or_default(),
                url.path()
            )))
        }
    }
}

/// One allowed scheme, host, port, and exact path.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AllowedHttpTarget {
    /// Lowercase.
    scheme: String,
    /// Lowercase.
    host: String,
    port: u16,
    path: String,
}

impl AllowedHttpTarget {
    fn new(scheme: &str, host: &str, port: u16, path: &str) -> Self {
        Self {
            scheme: scheme.to_ascii_lowercase(),
            host: host.to_ascii_lowercase(),
            port,
            path: path.to_owned(),
        }
    }

    /// Parses an absolute URL into an exact target rule.
    ///
    /// # Errors
    ///
    /// Returns [`TargetError::InvalidUrl`] for a value that is not an absolute URL,
    /// [`TargetError::MissingHost`] without a host, and [`TargetError::UnknownPort`] when
    /// neither the URL nor its scheme determines a port.
    pub fn parse(value: &str) -> Result<Self, TargetError> {
        let url = Url::parse(value)?;
        let host = url.host_str().ok_or(TargetError::MissingHost)?;
        let port = url
            .port_or_known_default()
            .ok_or(TargetError::UnknownPort)?;
        Ok(Self::new(url.scheme(), host, port, url.path()))
    }

    fn matches(&self, url: &Url) -> bool {
        url.scheme() == self.scheme
            && url.host_str().map(str::to_ascii_lowercase).as_deref() == Some(self.host.as_str())
            && url.port_or_known_default() == Some(self.port)
            && url.path() == self.path
    }
}

#[cfg(test)]
mod tests {
    use color_eyre::eyre;

    use super::*;

    #[test]
    fn rejects_unusable_target_rules() {
        assert!(matches!(
            AllowedHttpTarget::parse("not a url"),
            Err(TargetError::InvalidUrl(_))
        ));
        assert!(matches!(
            AllowedHttpTarget::parse("file:///etc/passwd"),
            Err(TargetError::MissingHost)
        ));
        assert!(matches!(
            AllowedHttpTarget::parse("gopher://example.org/"),
            Err(TargetError::UnknownPort)
        ));
    }

    fn example_allowlist() -> eyre::Result<Allowlist> {
        Ok(Allowlist::new([AllowedHttpTarget::parse(
            "https://data.example.org/api/query",
        )?]))
    }

    #[test]
    fn accepts_an_allowed_url_with_any_query() -> eyre::Result<()> {
        let url = Url::parse("https://data.example.org/api/query?station=ABC")?;
        assert!(example_allowlist()?.check(&url).is_ok());
        Ok(())
    }

    #[test]
    fn rejects_other_pages_on_an_allowed_host() -> eyre::Result<()> {
        let url = Url::parse("https://data.example.org/index.html")?;
        assert!(example_allowlist()?.check(&url).is_err());
        Ok(())
    }

    #[test]
    fn rejects_other_ports_on_an_allowed_host() -> eyre::Result<()> {
        let url = Url::parse("https://data.example.org:444/api/query")?;
        assert!(example_allowlist()?.check(&url).is_err());
        Ok(())
    }

    #[test]
    fn rejects_other_hosts() -> eyre::Result<()> {
        let url = Url::parse("https://example.net/api/query")?;
        assert!(example_allowlist()?.check(&url).is_err());
        Ok(())
    }

    #[test]
    fn an_empty_allowlist_refuses_everything() -> eyre::Result<()> {
        let url = Url::parse("https://data.example.org/api/query")?;
        assert!(Allowlist::new([]).check(&url).is_err());
        Ok(())
    }
}
