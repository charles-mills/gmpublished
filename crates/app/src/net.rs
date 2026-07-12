use std::time::Duration;

pub fn build_agent(global: Duration, connect: Duration, response: Duration) -> ureq::Agent {
    agent_builder(global, connect, response).build().into()
}

pub fn build_agent_with_max_idle_connections_per_host(
    global: Duration,
    connect: Duration,
    response: Duration,
    max_idle_connections_per_host: usize,
) -> ureq::Agent {
    agent_builder(global, connect, response)
        .max_idle_connections_per_host(max_idle_connections_per_host)
        .build()
        .into()
}

fn agent_builder(
    global: Duration,
    connect: Duration,
    response: Duration,
) -> ureq::config::ConfigBuilder<ureq::typestate::AgentScope> {
    ureq::Agent::config_builder()
        // The workspace ureq build carries no bundled webpki roots; certificate
        // verification must go through the OS trust store (PlatformVerifier).
        .tls_config(
            ureq::tls::TlsConfig::builder()
                .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                .build(),
        )
        .http_status_as_error(true)
        .timeout_global(Some(global))
        .timeout_connect(Some(connect))
        .timeout_recv_response(Some(response))
        .timeout_recv_body(Some(response))
}
