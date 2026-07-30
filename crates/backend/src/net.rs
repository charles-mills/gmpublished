//! Shared HTTP agent construction.

use std::time::Duration;

/// Project-owned HTTP policy. The `ureq` typestate builder stays inside this
/// module; callers describe the behavior they need without coupling their API
/// to a particular client's construction types.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct HttpAgentConfig {
    global_timeout: Option<Duration>,
    connect_timeout: Option<Duration>,
    receive_response_timeout: Option<Duration>,
    receive_body_timeout: Option<Duration>,
    max_idle_connections_per_host: Option<usize>,
    http_status_as_error: Option<bool>,
}

impl HttpAgentConfig {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            global_timeout: None,
            connect_timeout: None,
            receive_response_timeout: None,
            receive_body_timeout: None,
            max_idle_connections_per_host: None,
            http_status_as_error: None,
        }
    }

    #[must_use]
    /// Sets the wall-clock budget for the entire request.
    pub const fn global_timeout(mut self, timeout: Duration) -> Self {
        self.global_timeout = Some(timeout);
        self
    }

    #[must_use]
    /// Sets the budget for establishing a connection.
    pub const fn connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = Some(timeout);
        self
    }

    /// Sets ureq's deadline for receiving response headers.
    ///
    /// In ureq 3.x this deadline remains an upper bound while the body is
    /// read, so streaming transfers should normally omit it and use
    /// [`Self::receive_body_timeout`] as their rolling idle budget instead.
    #[must_use]
    pub const fn receive_response_timeout(mut self, timeout: Duration) -> Self {
        self.receive_response_timeout = Some(timeout);
        self
    }

    /// Sets the rolling idle budget for receiving response-body data.
    #[must_use]
    pub const fn receive_body_timeout(mut self, timeout: Duration) -> Self {
        self.receive_body_timeout = Some(timeout);
        self
    }

    #[must_use]
    /// Bounds idle keep-alive connections retained for one host.
    pub const fn max_idle_connections_per_host(mut self, maximum: usize) -> Self {
        self.max_idle_connections_per_host = Some(maximum);
        self
    }

    #[must_use]
    /// Chooses whether non-success status codes become client errors.
    ///
    /// When unset, ureq's default applies, which currently treats them as
    /// errors (`true`).
    pub const fn http_status_as_error(mut self, enabled: bool) -> Self {
        self.http_status_as_error = Some(enabled);
        self
    }
}

/// Builds an HTTP agent that always verifies TLS against the platform trust
/// store, then applies the caller's project-owned policy.
#[must_use]
pub fn build_http_agent(config: HttpAgentConfig) -> ureq::Agent {
    let builder = ureq::Agent::config_builder().tls_config(
        ureq::tls::TlsConfig::builder()
            .root_certs(ureq::tls::RootCerts::PlatformVerifier)
            .build(),
    );
    let builder = match config.global_timeout {
        Some(timeout) => builder.timeout_global(Some(timeout)),
        None => builder,
    };
    let builder = match config.connect_timeout {
        Some(timeout) => builder.timeout_connect(Some(timeout)),
        None => builder,
    };
    let builder = match config.receive_response_timeout {
        Some(timeout) => builder.timeout_recv_response(Some(timeout)),
        None => builder,
    };
    let builder = match config.receive_body_timeout {
        Some(timeout) => builder.timeout_recv_body(Some(timeout)),
        None => builder,
    };
    let builder = match config.max_idle_connections_per_host {
        Some(maximum) => builder.max_idle_connections_per_host(maximum),
        None => builder,
    };
    let builder = match config.http_status_as_error {
        Some(enabled) => builder.http_status_as_error(enabled),
        None => builder,
    };

    builder.build().into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn receive_timeouts_remain_independent() {
        let response = Duration::from_secs(7);
        let body = Duration::from_secs(11);
        let agent = build_http_agent(
            HttpAgentConfig::new()
                .receive_response_timeout(response)
                .receive_body_timeout(body),
        );
        let timeouts = agent.config().timeouts();

        assert_eq!(timeouts.recv_response, Some(response));
        assert_eq!(timeouts.recv_body, Some(body));
    }

    #[test]
    fn omitted_receive_phase_is_not_configured_implicitly() {
        let body = Duration::from_secs(11);
        let agent = build_http_agent(HttpAgentConfig::new().receive_body_timeout(body));
        let timeouts = agent.config().timeouts();

        assert_eq!(timeouts.recv_response, None);
        assert_eq!(timeouts.recv_body, Some(body));
    }
}
