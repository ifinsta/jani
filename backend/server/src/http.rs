//! Shared defaults for the crate's outbound HTTP transports. Every client
//! refuses redirects: credentials and update payloads must never be replayed
//! against a host other than the configured origin.

use reqwest::blocking::{Client, ClientBuilder};
use reqwest::redirect;
use std::time::Duration;

pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(180);

pub fn client_builder() -> ClientBuilder {
    Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .redirect(redirect::Policy::none())
}
