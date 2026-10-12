use std::time::Duration;

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
    pub const fn global_timeout(mut self, timeout: Duration) -> Self {
        self.global_timeout = Some(timeout);
        self
    }

    #[must_use]
    pub const fn connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = Some(timeout);
        self
    }

    /// ureq 3.x keeps this deadline active while reading the body too.
    /// Streaming transfers should use `receive_body_timeout` instead.
    #[must_use]
    pub const fn receive_response_timeout(mut self, timeout: Duration) -> Self {
        self.receive_response_timeout = Some(timeout);
        self
    }

    #[must_use]
    pub const fn receive_body_timeout(mut self, timeout: Duration) -> Self {
        self.receive_body_timeout = Some(timeout);
        self
    }

    #[must_use]
    pub const fn max_idle_connections_per_host(mut self, maximum: usize) -> Self {
        self.max_idle_connections_per_host = Some(maximum);
        self
    }

    #[must_use]
    pub const fn http_status_as_error(mut self, enabled: bool) -> Self {
        self.http_status_as_error = Some(enabled);
        self
    }
}

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
