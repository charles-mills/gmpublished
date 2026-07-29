//! Shared HTTP agent construction.

/// A `ureq` agent builder with certificate verification already configured.
///
/// The workspace `ureq` build carries no bundled webpki roots, so every agent
/// must verify against the OS trust store. An agent built without it cannot
/// complete a TLS handshake, so this is the one place the choice is made
/// rather than a line repeated at each construction site.
///
/// Timeouts are deliberately left to the caller: a CDN payload download and a
/// Web API call want very different deadlines.
#[must_use]
pub fn tls_agent_builder() -> ureq::config::ConfigBuilder<ureq::typestate::AgentScope> {
    ureq::Agent::config_builder().tls_config(
        ureq::tls::TlsConfig::builder()
            .root_certs(ureq::tls::RootCerts::PlatformVerifier)
            .build(),
    )
}
