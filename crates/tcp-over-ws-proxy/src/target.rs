//! TCP targets: parsing them from a request or a config rule, and the allowlist they must pass.

use std::num::ParseIntError;

/// A validated `host:port` the browser asked to reach.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TcpTarget {
    /// Trimmed, without IPv6 brackets.
    pub host: String,
    /// Never zero.
    pub port: u16,
}

impl std::fmt::Display for TcpTarget {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}:{}", self.host, self.port)
    }
}

/// Why a requested or configured TCP target was rejected.
#[derive(Debug, thiserror::Error)]
pub enum TargetError {
    /// The request named no `host`.
    #[error("missing host query parameter")]
    MissingHost,
    /// The request named a `host` without a `port`.
    #[error("missing port query parameter")]
    MissingPort,
    /// A `host:port` target had no `:` separator.
    #[error("target must be host:port")]
    MissingPortSeparator,
    /// The port is not a number in `0..=65535`.
    #[error("invalid port {port:?}")]
    InvalidPort {
        /// The rejected port text.
        port: String,
        /// Why it did not parse.
        #[source]
        source: ParseIntError,
    },
    /// The host is empty after trimming whitespace and IPv6 brackets.
    #[error("host must not be empty")]
    EmptyHost,
    /// The host contains whitespace.
    #[error("host must not contain whitespace")]
    HostContainsWhitespace,
    /// Port `0` cannot be connected to.
    #[error("port must be greater than zero")]
    ZeroPort,
}

/// TCP targets accepted by a [`crate::Proxy`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Allowlist {
    targets: Vec<AllowedTcpTarget>,
}

impl Allowlist {
    /// Allows exactly `targets`; an empty list refuses every connection.
    pub fn new<I>(targets: I) -> Self
    where
        I: IntoIterator<Item = AllowedTcpTarget>,
    {
        Self {
            targets: targets.into_iter().collect(),
        }
    }

    /// The rule that allows `target`, if any.
    ///
    /// The relay connects to the rule rather than the request, so everything downstream sees
    /// the configured spelling of the host, whatever case the client used.
    pub(crate) fn allowed(&self, target: &TcpTarget) -> Option<&AllowedTcpTarget> {
        self.targets.iter().find(|allowed| allowed.matches(target))
    }
}

/// One allowed TCP host and port pair.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AllowedTcpTarget {
    /// Trimmed and lowercase.
    host: String,
    port: u16,
}

impl AllowedTcpTarget {
    fn new(host: &str, port: u16) -> Self {
        Self {
            host: host.trim().to_ascii_lowercase(),
            port,
        }
    }

    /// Parses a `host:port` target rule.
    ///
    /// # Errors
    ///
    /// Returns [`TargetError::MissingPortSeparator`] without a `:`,
    /// [`TargetError::InvalidPort`] or [`TargetError::ZeroPort`] for an unusable port, and
    /// [`TargetError::EmptyHost`] or [`TargetError::HostContainsWhitespace`] for an unusable
    /// host.
    pub fn parse(value: &str) -> Result<Self, TargetError> {
        let target = parse_host_port(value)?;
        Ok(Self::new(&target.host, target.port))
    }

    /// Trimmed and lowercase.
    pub(crate) fn host(&self) -> &str {
        &self.host
    }

    pub(crate) fn port(&self) -> u16 {
        self.port
    }

    fn matches(&self, target: &TcpTarget) -> bool {
        target.host.eq_ignore_ascii_case(&self.host) && target.port == self.port
    }
}

impl std::fmt::Display for AllowedTcpTarget {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}:{}", self.host, self.port)
    }
}

/// Reads the target from a `host=…&port=…` query.
pub(crate) fn target_from_query(query: Option<&str>) -> Result<TcpTarget, TargetError> {
    let mut host = None;
    let mut port = None;
    for (key, value) in query
        .into_iter()
        .flat_map(|query| url::form_urlencoded::parse(query.as_bytes()))
    {
        match key.as_ref() {
            "host" => host = Some(value.into_owned()),
            "port" => port = Some(value.into_owned()),
            _ => {}
        }
    }

    let host = host.ok_or(TargetError::MissingHost)?;
    let port = port.ok_or(TargetError::MissingPort)?;
    validate_target(&host, parse_port(&port)?)
}

/// Parses `host:port`, splitting at the last `:` so a bracketed IPv6 host stays whole.
fn parse_host_port(target: &str) -> Result<TcpTarget, TargetError> {
    let (host, port) = target
        .rsplit_once(':')
        .ok_or(TargetError::MissingPortSeparator)?;
    validate_target(host, parse_port(port)?)
}

fn parse_port(port: &str) -> Result<u16, TargetError> {
    port.parse().map_err(|source| TargetError::InvalidPort {
        port: port.to_owned(),
        source,
    })
}

fn validate_target(host: &str, port: u16) -> Result<TcpTarget, TargetError> {
    let host = host.trim().trim_matches(['[', ']']).to_owned();
    if host.is_empty() {
        return Err(TargetError::EmptyHost);
    }
    if host.chars().any(char::is_whitespace) {
        return Err(TargetError::HostContainsWhitespace);
    }
    if port == 0 {
        return Err(TargetError::ZeroPort);
    }
    Ok(TcpTarget { host, port })
}

#[cfg(test)]
mod tests {
    use color_eyre::eyre::{self, bail};

    use super::*;

    #[test]
    fn parses_host_and_port_query() -> eyre::Result<()> {
        let target = target_from_query(Some("host=stream.example.org&port=18000"))?;
        assert_eq!(target.to_string(), "stream.example.org:18000");
        Ok(())
    }

    #[test]
    fn rejects_missing_target() {
        assert!(matches!(
            target_from_query(None),
            Err(TargetError::MissingHost)
        ));
    }

    #[test]
    fn rejects_unusable_ports() {
        // Not a number, out of range, and the unconnectable zero port
        assert!(matches!(
            target_from_query(Some("host=stream.example.org&port=http")),
            Err(TargetError::InvalidPort { .. })
        ));
        assert!(matches!(
            AllowedTcpTarget::parse("stream.example.org:70000"),
            Err(TargetError::InvalidPort { .. })
        ));
        assert!(matches!(
            AllowedTcpTarget::parse("stream.example.org:0"),
            Err(TargetError::ZeroPort)
        ));
    }

    fn example_allowlist() -> eyre::Result<Allowlist> {
        Ok(Allowlist::new([AllowedTcpTarget::parse(
            "stream.example.org:18000",
        )?]))
    }

    /// A host in any case is allowed, and resolves to the rule's own spelling, which is all the
    /// relay and its metric labels ever see.
    #[test]
    fn accepts_an_allowed_target_in_its_configured_spelling() -> eyre::Result<()> {
        let target = target_from_query(Some("host=STREAM.example.org&port=18000"))?;
        let allowlist = example_allowlist()?;
        let Some(allowed) = allowlist.allowed(&target) else {
            bail!("{target} should be allowed");
        };
        assert_eq!(allowed.to_string(), "stream.example.org:18000");
        Ok(())
    }

    #[test]
    fn rejects_other_hosts() -> eyre::Result<()> {
        let target = target_from_query(Some("host=example.net&port=18000"))?;
        assert!(example_allowlist()?.allowed(&target).is_none());
        Ok(())
    }

    #[test]
    fn rejects_other_ports() -> eyre::Result<()> {
        let target = target_from_query(Some("host=stream.example.org&port=22"))?;
        assert!(example_allowlist()?.allowed(&target).is_none());
        Ok(())
    }

    #[test]
    fn an_empty_allowlist_refuses_everything() -> eyre::Result<()> {
        let target = target_from_query(Some("host=stream.example.org&port=18000"))?;
        assert!(Allowlist::new([]).allowed(&target).is_none());
        Ok(())
    }
}
