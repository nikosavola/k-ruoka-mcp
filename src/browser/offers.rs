//! OmaPlussa-edut: the account's personalised Plussa offers.
//!
//! Read-only, and per-account like the cart, so `Session` reaches it with the same
//! cookies `Cart` and `Catalog` already carry.

use crate::browser::session::{ApiError, KrApi};
use crate::types::PersonalOffersResponse;

pub struct Offers<'a> {
    api: &'a dyn KrApi,
}

impl<'a> Offers<'a> {
    pub fn new(api: &'a dyn KrApi) -> Self {
        Self { api }
    }

    /// The account's personalised offers, scoped to a store.
    ///
    /// Measured against two real stores: the set differs rather than being identical
    /// everywhere, matching the restriction text K-Ruoka attaches to each offer ("valid
    /// in the K-food stores where the product is available").
    ///
    /// Every offer observed so far was already loaded onto the Plussa card, applying
    /// one is just buying a listed product -- not verified as a universal guarantee,
    /// since nothing here has tried to find or call a separate activation endpoint.
    /// Time-limited (each offer carries its own `validUntil`), so the caller must
    /// never cache this. An anonymous session returns `{"offers": []}` rather than an
    /// error, measured against a fresh, never-logged-in profile.
    pub async fn personal_offers(
        &self,
        store_id: &str,
    ) -> Result<PersonalOffersResponse, ApiError> {
        let body = serde_json::json!({ "storeId": store_id });
        let value = self
            .api
            .call("POST", "/kr-api/tos-offers", Some(&body))
            .await?;
        serde_json::from_value(value)
            .map_err(|e| ApiError::Other(anyhow::anyhow!("unexpected tos-offers shape: {e}")))
    }
}
