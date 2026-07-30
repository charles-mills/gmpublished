//! Application HTTP policy layered over the backend's TLS-safe agent factory.

use std::time::Duration;

/// Named timeout budgets applied to an HTTP agent.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct HttpTimeouts {
    global: Option<Duration>,
    connect: Option<Duration>,
    receive_response: Option<Duration>,
    receive_body: Option<Duration>,
}

impl HttpTimeouts {
    /// Starts an HTTP policy without explicit timeout overrides.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            global: None,
            connect: None,
            receive_response: None,
            receive_body: None,
        }
    }

    /// Sets the wall-clock budget for the complete request.
    #[must_use]
    pub const fn global_timeout(mut self, timeout: Duration) -> Self {
        self.global = Some(timeout);
        self
    }

    /// Sets the connection-establishment budget.
    #[must_use]
    pub const fn connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect = Some(timeout);
        self
    }

    /// Sets ureq's response-header deadline, which also caps later body reads.
    #[must_use]
    pub const fn receive_response_timeout(mut self, timeout: Duration) -> Self {
        self.receive_response = Some(timeout);
        self
    }

    /// Sets the rolling idle budget while receiving body data.
    #[must_use]
    pub const fn receive_body_timeout(mut self, timeout: Duration) -> Self {
        self.receive_body = Some(timeout);
        self
    }

    fn backend_config(self) -> gmpublished_backend::HttpAgentConfig {
        let mut config = gmpublished_backend::HttpAgentConfig::new().http_status_as_error(true);
        if let Some(timeout) = self.global {
            config = config.global_timeout(timeout);
        }
        if let Some(timeout) = self.connect {
            config = config.connect_timeout(timeout);
        }
        if let Some(timeout) = self.receive_response {
            config = config.receive_response_timeout(timeout);
        }
        if let Some(timeout) = self.receive_body {
            config = config.receive_body_timeout(timeout);
        }
        config
    }
}

/// Builds an agent with the supplied named timeout policy.
#[must_use]
pub fn build_agent(timeouts: HttpTimeouts) -> ureq::Agent {
    gmpublished_backend::build_http_agent(timeouts.backend_config())
}

/// Builds an agent with named timeouts and a bounded per-host idle pool.
#[must_use]
pub fn build_agent_with_max_idle_connections_per_host(
    timeouts: HttpTimeouts,
    max_idle_connections_per_host: usize,
) -> ureq::Agent {
    gmpublished_backend::build_http_agent(
        timeouts
            .backend_config()
            .max_idle_connections_per_host(max_idle_connections_per_host),
    )
}
