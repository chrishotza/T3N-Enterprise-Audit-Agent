//! search_offers: calls Duffel offer search API and returns available flights.
//!
//! Duffel search is two calls:
//!   1. POST /air/offer_requests?return_offers=false → returns a small response with offer_request_id
//!   2. GET /air/offers?offer_request_id=<id> → returns offer list

#[derive(serde::Deserialize)]
pub struct SearchOffersReq {
    pub origin: String,
    pub destination: String,
    pub departure_date: String,
    pub cabin_class: String,
    pub adult_count: u32,
}

#[derive(serde::Serialize)]
pub struct Offer {
    pub id: String,
    pub total_amount: String,
    pub total_currency: String,
    pub expires_at: String,
    /// Duffel-assigned passenger IDs for this offer (must be used verbatim in book-offer).
    pub passenger_ids: alloc::vec::Vec<alloc::string::String>,
}

#[derive(serde::Serialize)]
pub struct SearchOffersResp {
    pub offers: Vec<Offer>,
}

const DUFFEL_BASE: &str = "https://api.duffel.com";
const DUFFEL_VERSION: &str = "v2";

/// Entry point called from `lib.rs`. `input` is the raw JSON bytes from the
/// node's `generic-input.input` field.
pub fn search_offers(input: &[u8]) -> Result<Vec<u8>, String> {
    let req: SearchOffersReq = serde_json::from_slice(input)
        .map_err(|e| alloc::format!("search-offers: bad input: {e}"))?;

    #[cfg(target_arch = "wasm32")]
    {
        let resp = search_offers_wasm(req)?;
        serde_json::to_vec(&resp).map_err(|e| e.to_string())
    }

    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = req;
        Err("search_offers is only implemented on the wasm32 target".to_string())
    }
}

#[cfg(target_arch = "wasm32")]
use crate::host::{
    interfaces::{http as http_iface, kv_store, logging},
    tenant::tenant_context,
};

#[cfg(target_arch = "wasm32")]
fn search_offers_wasm(req: SearchOffersReq) -> Result<SearchOffersResp, String> {
    use serde_json::json;

    let api_key = get_api_key()?;

    // Step 1: create offer request.
    // `return_offers=false` is a QUERY PARAMETER — Duffel ignores it if put in
    // the body. With it the response is ~2 KB instead of ~1.7 MB.
    let offer_request_body = json!({
        "data": {
            "slices": [{
                "origin": req.origin,
                "destination": req.destination,
                "departure_date": req.departure_date,
            }],
            "passengers": build_passenger_count(req.adult_count),
            "cabin_class": req.cabin_class,
        }
    });

    let offer_req_resp = http_iface::call(&http_iface::Request {
        method: http_iface::Verb::Post,
        url: alloc::format!("{DUFFEL_BASE}/air/offer_requests?return_offers=false"),
        headers: Some(duffel_headers(&api_key)),
        payload: Some(serde_json::to_vec(&offer_request_body).map_err(|e| e.to_string())?),
    })
    .map_err(|e| alloc::format!("duffel offer-request: {e}"))?;

    if offer_req_resp.code != 201 {
        let body = alloc::string::String::from_utf8_lossy(&offer_req_resp.payload);
        return Err(alloc::format!(
            "Duffel offer-request failed: HTTP {} — {body}",
            offer_req_resp.code
        ));
    }

    // Guard: if Duffel ignores return_offers=false the response is ~1.7 MB and
    // serde_json will OOM inside WASM. 64 KB is well above the ~2 KB we expect.
    const MAX_OFFER_REQ_BYTES: usize = 65_536;
    if offer_req_resp.payload.len() > MAX_OFFER_REQ_BYTES {
        return Err(alloc::format!(
            "offer-request response too large ({} bytes) — return_offers=false may be ignored",
            offer_req_resp.payload.len()
        ));
    }

    let offer_req_json: serde_json::Value =
        serde_json::from_slice(&offer_req_resp.payload).map_err(|e| e.to_string())?;
    let offer_request_id = offer_req_json["data"]["id"]
        .as_str()
        .ok_or("offer-request response: missing data.id")?
        .to_string();

    let _ = logging::info(&alloc::format!(
        "Duffel offer request created: {offer_request_id}"
    ));

    // Step 2: fetch offers
    let offers_resp = http_iface::call(&http_iface::Request {
        method: http_iface::Verb::Get,
        url: alloc::format!(
            "{DUFFEL_BASE}/air/offers?offer_request_id={offer_request_id}&max_connections=0&limit=5"
        ),
        headers: Some(duffel_headers(&api_key)),
        payload: None,
    })
    .map_err(|e| alloc::format!("duffel offers: {e}"))?;

    if offers_resp.code != 200 {
        let body = alloc::string::String::from_utf8_lossy(&offers_resp.payload);
        return Err(alloc::format!(
            "Duffel offers fetch failed: HTTP {} — {body}",
            offers_resp.code
        ));
    }

    let offers_json: serde_json::Value =
        serde_json::from_slice(&offers_resp.payload).map_err(|e| e.to_string())?;

    let offers: Result<alloc::vec::Vec<Offer>, alloc::string::String> = offers_json["data"]
        .as_array()
        .ok_or("missing offers array")?
        .iter()
        .map(|o| {
            let id = o["id"].as_str().ok_or("offer missing id")?.to_string();
            let total_amount = o["total_amount"]
                .as_str()
                .ok_or("offer missing total_amount")?
                .to_string();
            let total_currency = o["total_currency"]
                .as_str()
                .ok_or("offer missing total_currency")?
                .to_string();
            let expires_at = o["expires_at"].as_str().unwrap_or("").to_string();
            let passenger_ids = o["passengers"]
                .as_array()
                .unwrap_or(&alloc::vec![])
                .iter()
                .filter_map(|p| p["id"].as_str().map(|s| s.to_string()))
                .collect();
            Ok(Offer {
                id,
                total_amount,
                total_currency,
                expires_at,
                passenger_ids,
            })
        })
        .collect();
    let offers = offers?;

    let _ = logging::info(&alloc::format!(
        "Duffel offers: {} offers parsed",
        offers.len()
    ));

    Ok(SearchOffersResp { offers })
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

#[cfg(target_arch = "wasm32")]
fn build_passenger_count(adult_count: u32) -> alloc::vec::Vec<serde_json::Value> {
    use serde_json::json;
    (0..adult_count)
        .map(|_| json!({ "type": "adult" }))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_offers_non_wasm_returns_err() {
        let input = serde_json::to_vec(&serde_json::json!({
            "origin": "LHR",
            "destination": "JFK",
            "departure_date": "2026-07-15",
            "cabin_class": "economy",
            "adult_count": 1,
        }))
        .unwrap();
        let result = search_offers(&input);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .contains("only implemented on the wasm32 target"));
    }

    #[test]
    fn search_offers_bad_input_returns_err() {
        let result = search_offers(b"not json");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("bad input"));
    }
}
