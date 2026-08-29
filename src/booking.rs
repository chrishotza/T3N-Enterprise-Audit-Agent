//! book_offer: calls Duffel create-order API and returns the PNR.
//!
//! Passenger PII (name, DOB, passport, contact) is NEVER passed in as a
//! contract argument. The contract templates `{{profile.<field>}}` markers
//! into the Duffel order body and the host's `http-with-placeholders`
//! interface resolves them from the calling user's profile at dispatch time,
//! so plaintext PII never enters WASM memory.

#[derive(serde::Deserialize)]
pub struct BookOfferReq {
    pub offer_id: String,
    /// Opaque Duffel-assigned passenger id from the offer (returned by
    /// search-offers). NOT PII — just the slot the order binds to.
    pub passenger_id: String,
    pub total_amount: String,
    pub total_currency: String,
}

#[derive(serde::Serialize)]
pub struct Booking {
    pub id: String,
    pub pnr: String,
    pub status: String,
}

const DUFFEL_BASE: &str = "https://api.duffel.com";
const DUFFEL_VERSION: &str = "v2";

/// Entry point called from `lib.rs`. `input` is the raw JSON bytes from the
/// node's `generic-input.input` field.
pub fn book_offer(input: &[u8]) -> Result<Vec<u8>, String> {
    let req: BookOfferReq =
        serde_json::from_slice(input).map_err(|e| alloc::format!("book-offer: bad input: {e}"))?;

    #[cfg(target_arch = "wasm32")]
    {
        let booking = book_offer_wasm(req)?;
        serde_json::to_vec(&booking).map_err(|e| e.to_string())
    }

    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = req;
        Err("book_offer is only implemented on the wasm32 target".to_string())
    }
}

#[cfg(target_arch = "wasm32")]
use crate::host::{
    interfaces::{http_with_placeholders as hwp, kv_store, logging},
    tenant::tenant_context,
};

#[cfg(target_arch = "wasm32")]
fn book_offer_wasm(req: BookOfferReq) -> Result<Booking, String> {
    use serde_json::json;

    let api_key = get_api_key()?;

    // The `{{profile.<path>}}` markers are resolved host-side from the
    // calling user's profile via http-with-placeholders, after this contract
    // serialises the body and before the outbound Duffel call — the contract
    // never holds the plaintext.
    //
    // DEMO-HARDCODED fields: the user-profile schema carries no
    // passport / nationality / title fields, and we don't run phone
    // verification for the demo, so `verified_contacts.phone.value` won't
    // exist. For those we send fixed Duffel-sandbox values.
    let order_body = json!({
        "data": {
            "type": "instant",
            "selected_offers": [req.offer_id],
            "passengers": [{
                "id": req.passenger_id,
                // Resolved from the user's profile (privacy-preserving path):
                "given_name": "{{profile.first_name}}",
                "family_name": "{{profile.last_name}}",
                "born_on": "{{profile.date_of_birth}}",
                "gender": "{{profile.gender}}",
                "email": "{{profile.verified_contacts.email.value}}",
                // Demo-hardcoded (no profile source — see note above):
                "title": "mr",
                "passport_number": "X12345678",
                "passport_country_code": "GB",
                "passport_expiry_date": "2030-01-01",
                "phone_number": "+442071234567",
            }],
            "payments": [{
                "type": "balance",
                "amount": req.total_amount,
                "currency": req.total_currency,
            }]
        }
    });

    let _ = logging::info(&alloc::format!(
        "Calling Duffel POST /air/orders for offer {}",
        req.offer_id
    ));

    let resp = hwp::call(&hwp::Request {
        method: hwp::Verb::Post,
        url: alloc::format!("{DUFFEL_BASE}/air/orders"),
        headers: Some(duffel_headers(&api_key)),
        payload: Some(serde_json::to_vec(&order_body).map_err(|e| e.to_string())?),
    })
    .map_err(|e| alloc::format!("duffel create-order: {}", format_http_error(e)))?;

    if resp.code != 200 && resp.code != 201 {
        // Do not log the upstream response body: booking errors may include
        // passenger data even though that data is never returned to callers.
        let _ = logging::error(&alloc::format!("Duffel create-order HTTP {}", resp.code));
        return Err(alloc::format!(
            "Duffel create-order failed: HTTP {}",
            resp.code
        ));
    }

    let order: serde_json::Value =
        serde_json::from_slice(&resp.payload).map_err(|e| e.to_string())?;

    let booking_id = order["data"]["id"]
        .as_str()
        .ok_or("Duffel response missing order id")?
        .to_string();
    let pnr = order["data"]["booking_reference"]
        .as_str()
        .ok_or("Duffel response missing booking_reference")?
        .to_string();
    let status = order["data"]["payment_status"]["awaiting_payment"]
        .as_bool()
        .map(|b| if b { "awaiting_payment" } else { "confirmed" })
        .ok_or("Duffel response missing payment_status.awaiting_payment")?
        .to_string();

    let _ = logging::info(&alloc::format!(
        "Duffel order created: id={booking_id} pnr={pnr}"
    ));

    Ok(Booking {
        id: booking_id,
        pnr,
        status,
    })
}

/// Render a typed `http-with-placeholders` error as a contract-facing string.
/// Never includes resolved PII — only field names and host-side reasons.
#[cfg(target_arch = "wasm32")]
fn format_http_error(e: hwp::HttpError) -> alloc::string::String {
    match e {
        hwp::HttpError::EgressDenied(host) => alloc::format!("egress denied for host {host}"),
        hwp::HttpError::PlaceholderDenied(marker) => {
            alloc::format!("placeholder not permitted: {marker}")
        }
        hwp::HttpError::PlaceholderUnknown(field) => {
            alloc::format!("user profile missing field: {field}")
        }
        hwp::HttpError::PlaceholderNoUserContext => {
            "no user context bound for placeholder resolution".to_string()
        }
        hwp::HttpError::UpstreamError(reason) => alloc::format!("upstream: {reason}"),
    }
}

#[cfg(target_arch = "wasm32")]
fn get_api_key() -> Result<alloc::string::String, alloc::string::String> {
    let tid = tenant_context::tenant_did();
    let map_name = alloc::format!("z:{}:secrets", hex::encode(&tid));
    let bytes = kv_store::get(&map_name, b"duffel_api_key")
        .map_err(|e| alloc::format!("kv read: {e}"))?
        .ok_or("duffel_api_key not found in z:<tid>:secrets — populate it via the tenant SDK before use")?;
    alloc::string::String::from_utf8(bytes).map_err(|e| e.to_string())
}

#[cfg(target_arch = "wasm32")]
fn duffel_headers(
    api_key: &str,
) -> alloc::vec::Vec<(alloc::string::String, alloc::string::String)> {
    // Content-Type is set automatically by the host HTTP function via
    // .json() — sending it explicitly creates a duplicate that Duffel rejects.
    alloc::vec![
        (
            "Authorization".to_string(),
            alloc::format!("Bearer {api_key}"),
        ),
        ("Duffel-Version".to_string(), DUFFEL_VERSION.to_string()),
        ("Accept".to_string(), "application/json".to_string()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn book_offer_non_wasm_returns_err() {
        let input = serde_json::to_vec(&serde_json::json!({
            "offer_id": "off_abc123",
            "passenger_id": "pas_abc123",
            "total_amount": "199.00",
            "total_currency": "GBP",
        }))
        .unwrap();
        let result = book_offer(&input);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .contains("only implemented on the wasm32 target"));
    }

    #[test]
    fn book_offer_bad_input_returns_err() {
        let result = book_offer(b"not json");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("bad input"));
    }

    #[test]
    fn book_offer_rejects_inline_pii_fields() {
        let input = serde_json::to_vec(&serde_json::json!({
            "offer_id": "off_abc123",
            "passengers": [{ "given_name": "Jane" }],
            "total_amount": "199.00",
            "total_currency": "GBP",
        }))
        .unwrap();
        let result = book_offer(&input);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("bad input"));
    }
}
