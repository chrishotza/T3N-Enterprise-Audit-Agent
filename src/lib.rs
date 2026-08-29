//! z-tenant-flight v0.4.0 — Duffel flight booking showcase.
//!
//! Demonstrates the z-space tenant model:
//!   - `search-offers`: calls Duffel offer search API inside the Enclave (no PII).
//!   - `book-offer`: calls Duffel create-order API via the host's
//!     `http-with-placeholders` interface. Passenger PII is NEVER passed in as
//!     a contract argument: the contract templates `{{profile.<field>}}`
//!     markers into the order body and the host resolves them from the calling
//!     user's profile at dispatch time, so plaintext PII never enters WASM.
//!
//! The Duffel API key is read from the z: KV map `secrets` (key:
//! `duffel_api_key`). This map is created and populated by the tenant SDK
//! before the contract runs. Only the booking ID and PNR cross the WIT
//! boundary back to the caller.
//!
//! # Host-capability requirements
//!
//! Declare in manifest (access to a user's profile is gated by the on-chain
//! agent delegation grant, not a per-field allowlist):
//! ```json
//! {
//!   "host_capabilities": [
//!     "kv_store", "logging", "tenant_context", "http", "http_with_placeholders"
//!   ]
//! }
//! ```
//!
//! # Setup
//!
//! Before first use, the tenant SDK must create the `secrets` KV map and
//! write the Duffel API key:
//! ```text
//! // Via the tenant SDK (before contract first use):
//! z_sdk.kv("secrets").set("duffel_api_key", "duffel_test_your_key_here")
//! ```
#![warn(clippy::style, missing_debug_implementations)]
#![cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]

extern crate alloc;

pub const CONTRACT_VERSION: &str = "0.4.1";

wit_bindgen::generate!({
    world: "tenant-flight",
    path: "wit",
    additional_derives: [
        serde::Deserialize,
        serde::Serialize,
    ],
    generate_all,
});

mod booking;
mod search;

struct Component;

#[cfg(target_arch = "wasm32")]
impl exports::z::tenant_flight::contracts::Guest for Component {
    fn search_offers(
        req: exports::z::tenant_flight::contracts::GenericInput,
    ) -> Result<alloc::vec::Vec<u8>, alloc::string::String> {
        let input = req.input.ok_or("search-offers: missing input")?;
        search::search_offers(&input)
    }

    fn book_offer(
        req: exports::z::tenant_flight::contracts::GenericInput,
    ) -> Result<alloc::vec::Vec<u8>, alloc::string::String> {
        let input = req.input.ok_or("book-offer: missing input")?;
        booking::book_offer(&input)
    }
}

#[cfg(target_arch = "wasm32")]
export!(Component);

#[cfg(test)]
mod tests {
    use super::CONTRACT_VERSION;

    #[test]
    fn contract_version_is_semver() {
        let parts: Vec<&str> = CONTRACT_VERSION.split('.').collect();
        assert_eq!(parts.len(), 3, "CONTRACT_VERSION must be MAJOR.MINOR.PATCH");
        for part in parts {
            assert!(part.parse::<u32>().is_ok(), "each part must be a number");
        }
    }

    #[test]
    fn contract_version_is_v0_4_0() {
        assert_eq!(CONTRACT_VERSION, "0.4.1");
    }
}
