//! The `/kr-api/basket/...` calls, on top of [`Session`](crate::browser::session::Session).

use crate::browser::session::{ApiError, KrApi};
use crate::types::{AmountInfo, Basket, BasketEvent, BasketItem, NewItem};

pub struct Cart<'a> {
    api: &'a dyn KrApi,
}

impl<'a> Cart<'a> {
    pub fn new(api: &'a dyn KrApi) -> Self {
        Self { api }
    }

    /// Fetch (or implicitly create) the active basket for a store.
    ///
    /// On an anonymous session this happily returns a fresh, valid basket rather
    /// than a 401 -- so a successful call is not evidence of being logged in.
    /// Check `userInfo` for that.
    pub async fn active(&self, store_id: &str) -> Result<Basket, ApiError> {
        let body = serde_json::json!({
            "storeId": store_id,
            "substitutionDefault": true,
        });
        let value = self
            .api
            .call("POST", "/kr-api/basket/active", Some(&body))
            .await
            .map_err(|e| clarify_store_error(e, store_id))?;
        parse(value)
    }

    /// Apply a batch of events. The endpoint always takes an array, even for one
    /// change, and returns the whole updated basket.
    pub async fn apply(&self, basket_id: &str, events: &[BasketEvent]) -> Result<Basket, ApiError> {
        let body = serde_json::to_value(events).map_err(|e| ApiError::Other(e.into()))?;
        let path = format!("/kr-api/basket/by-id/{basket_id}");
        let value = self.api.call("PATCH", &path, Some(&body)).await?;
        parse(value)
    }

    /// Read the cart, apply events to it, and return the result. Saves callers
    /// from threading the basket id around; `basket/active` is cheap.
    pub async fn mutate(&self, store_id: &str, events: &[BasketEvent]) -> Result<Basket, ApiError> {
        let basket = self.active(store_id).await?;
        self.apply(&basket.id, events).await
    }

    /// `amount` is the resulting quantity, not a delta.
    ///
    /// Observed live: `ADD-ITEM` for an EAN already in the basket *replaces* that
    /// item's amount rather than accumulating -- add 1 twice and the cart holds 1.
    /// K-Ruoka's own frontend never sends `ADD-ITEM` for an item already present
    /// (it switches to `SET-ITEM-AMOUNT`), so this path is outside what the site
    /// itself exercises; the behaviour was measured rather than assumed.
    pub async fn add(
        &self,
        store_id: &str,
        ean: &str,
        amount: f64,
        unit: &str,
        local_store_id: Option<String>,
        allow_substitutes: bool,
    ) -> Result<Basket, ApiError> {
        // Observed live: K-Ruoka accepts amount 0 or negative and adds nothing,
        // returning 200. Reject it here so "add nothing" cannot masquerade as a
        // successful add.
        if amount.is_nan() || amount <= 0.0 {
            return Err(ApiError::InvalidRequest(format!(
                "quantity must be greater than 0 (got {amount}). K-Ruoka would accept this \
                 and add nothing while reporting success."
            )));
        }
        // An empty EAN would sail through the phantom-product check below by matching
        // any returned item that carries no `ean` of its own, turning a request that
        // added nothing into a reported success on someone else's line item.
        if ean.trim().is_empty() {
            return Err(ApiError::InvalidRequest(
                "ean must not be empty. Use search_products to find a product's EAN \
                 barcode."
                    .to_string(),
            ));
        }
        let event = BasketEvent::AddItem {
            item: NewItem {
                ean: ean.to_string(),
                local_store_id,
                allow_substitutes,
                amount_info: AmountInfo {
                    amount,
                    unit: unit.to_string(),
                },
            },
        };
        let basket = self.mutate(store_id, &[event]).await?;

        // `ADD-ITEM` accepts any EAN and inserts "Tuntematon tuote" for one it does
        // not recognise, so an unknown barcode looks like a success. Undo it and say
        // so, rather than leaving a phantom line in the user's cart.
        // "The call returned 200" is not evidence the item is in the cart -- that is
        // the recurring failure mode of this API, so check rather than
        // assume.
        let Some(added) = basket.items.iter().find(|i| i.ean == ean) else {
            return Err(ApiError::Other(anyhow::anyhow!(
                "K-Ruoka accepted the add for EAN {ean} but the item is not in the cart it \
                 returned. Nothing was changed as far as can be told; check the cart."
            )));
        };
        if !added.is_known_product() {
            let item_id = added.id.clone();
            // Roll back through the basket id already in hand rather than `remove`,
            // which would issue another read; tool calls run concurrently and there
            // is no reason to widen that window.
            let undo = BasketEvent::RemoveItem {
                item_id: item_id.clone(),
            };
            // Check the returned basket, not the status. `REMOVE-ITEM` is one of the
            // calls known to answer 200 while changing nothing, and a phantom item is
            // exactly the off-the-tested-path case where that is most plausible. On
            // `is_ok()` alone this would report "nothing was added" while leaving
            // "Tuntematon tuote" in a real cart -- the outcome this whole check exists
            // to prevent.
            let rolled_back = self
                .apply(&basket.id, &[undo])
                .await
                .is_ok_and(|after| !after.items.iter().any(|i| i.id == item_id));
            return Err(ApiError::InvalidRequest(if rolled_back {
                format!("K-Ruoka has no product with EAN {ean}; nothing was added.")
            } else {
                // Never fail silently leaving junk behind -- that is the whole
                // point of this check.
                format!(
                    "K-Ruoka has no product with EAN {ean}. It was added to the cart as \
                     \"Unknown product\" and could not be removed again -- call \
                     remove_from_cart with item_id={item_id}."
                )
            }));
        }
        Ok(basket)
    }

    /// `amount` of 0 removes the item -- verified live, the server handles it
    /// rather than needing a `REMOVE-ITEM` translation the way the frontend does.
    ///
    /// `unit` defaults to the unit the item already carries. Defaulting it to
    /// `"kpl"` instead would silently convert a `kg` item to pieces, which is
    /// corruption rather than a no-op.
    pub async fn set_amount(
        &self,
        store_id: &str,
        item_id: &str,
        amount: f64,
        unit: Option<&str>,
    ) -> Result<Basket, ApiError> {
        // 0 is a documented remove; negative is almost certainly a caller bug, and
        // K-Ruoka treats it as a remove too, which would make two spellings of the
        // same operation with only one of them documented.
        if amount < 0.0 || amount.is_nan() {
            return Err(ApiError::InvalidRequest(format!(
                "quantity cannot be negative (got {amount}). Use 0 to remove the item."
            )));
        }
        let basket = self.active(store_id).await?;
        let item = find_item(&basket, item_id)?;
        let unit = unit.unwrap_or(&item.amount_info.unit).to_string();
        let event = BasketEvent::SetItemAmount {
            item_id: item_id.to_string(),
            value: AmountInfo { amount, unit },
        };
        let after = self.apply(&basket.id, &[event]).await?;
        confirm_amount(&after, item_id, amount)?;
        Ok(after)
    }

    pub async fn remove(&self, store_id: &str, item_id: &str) -> Result<Basket, ApiError> {
        let basket = self.active(store_id).await?;
        find_item(&basket, item_id)?;
        let event = BasketEvent::RemoveItem {
            item_id: item_id.to_string(),
        };
        let after = self.apply(&basket.id, &[event]).await?;
        confirm_absent(&after, item_id)?;
        Ok(after)
    }

    /// Empty the basket via `CLEAR-ITEMS`.
    ///
    /// `DELETE /kr-api/basket/by-id/{id}` also exists and destroys the basket
    /// itself. `CLEAR-ITEMS` is preferred: it leaves the basket and its settings
    /// in place, and returns the emptied basket so the caller can see the result.
    pub async fn clear(&self, store_id: &str) -> Result<Basket, ApiError> {
        self.mutate(store_id, &[BasketEvent::ClearItems]).await
    }
}

fn parse(value: serde_json::Value) -> Result<Basket, ApiError> {
    serde_json::from_value(value)
        .map_err(|e| ApiError::Other(anyhow::anyhow!("unexpected basket shape: {e}")))
}

/// K-Ruoka answers a bad store id with 422 `InvalidStoreIdError` whose message is
/// literally "Invalid store ID undefined" -- it does not echo the id, and an empty
/// store id and a nonexistent one produce the identical text. Say which one the
/// caller actually passed.
fn clarify_store_error(e: ApiError, store_id: &str) -> ApiError {
    match &e {
        ApiError::Api {
            status: 422,
            message,
        } if message.contains("InvalidStoreIdError") => ApiError::Api {
            status: 422,
            message: format!(
                "K-Ruoka rejected store id {store_id:?} as invalid. Store ids look like \
                     \"N137\"; use search_stores to find one."
            ),
        },
        _ => e,
    }
}

/// Resolve an item id against the cart, refusing one that is not there.
///
/// K-Ruoka accepts `REMOVE-ITEM` / `SET-ITEM-AMOUNT` for an item id that is not in
/// the basket and returns 200 with the basket unchanged, so a typo'd id looks like a
/// success. Since the caller most likely passed an EAN where an item id was wanted,
/// fail loudly and list what is valid.
///
/// Returns the item, not just `Ok(())`: `set_amount` needs its current unit, and a
/// second lookup would be a second chance to disagree.
fn find_item<'b>(basket: &'b Basket, item_id: &str) -> Result<&'b BasketItem, ApiError> {
    if let Some(item) = basket.items.iter().find(|i| i.id == item_id) {
        return Ok(item);
    }
    let available: Vec<&str> = basket.items.iter().map(|i| i.id.as_str()).collect();
    Err(ApiError::InvalidRequest(if available.is_empty() {
        format!("no item {item_id:?} in the cart -- the cart is empty")
    } else {
        format!(
            "no item {item_id:?} in the cart. Item ids currently in it: {}. \
             Note these are basket item ids from get_cart, not EANs.",
            available.join(", ")
        )
    }))
}

/// K-Ruoka clamps silently at 999, so an amount at or above it is not a discrepancy.
const MAX_AMOUNT: f64 = 999.0;

/// Confirm a `SET-ITEM-AMOUNT` actually took effect.
///
/// `find_item` checks the id against a *previous* read, and tool calls run
/// concurrently, so between that read and this write another call can remove the
/// item. K-Ruoka then answers 200 and changes nothing -- reinstating precisely the
/// silent no-op `find_item` exists to prevent, just through a two-call interleaving
/// instead of a typo. The response carries the basket, so checking costs nothing.
fn confirm_amount(after: &Basket, item_id: &str, wanted: f64) -> Result<(), ApiError> {
    let Some(item) = after.items.iter().find(|i| i.id == item_id) else {
        // Requesting 0 *is* the documented spelling of remove, so absence is success.
        if wanted == 0.0 {
            return Ok(());
        }
        return Err(ApiError::Other(anyhow::anyhow!(
            "K-Ruoka accepted setting item {item_id} to {wanted} but the item is not in \
             the cart it returned. Something else probably removed it at the same time; \
             call get_cart to see the current state."
        )));
    };
    if wanted == 0.0 {
        return Err(ApiError::Other(anyhow::anyhow!(
            "asked K-Ruoka to remove item {item_id} (amount 0) but it is still in the \
             cart it returned, with amount {}.",
            item.amount_info.amount
        )));
    }
    let got = item.amount_info.amount;
    if got != wanted && !(wanted > MAX_AMOUNT && got == MAX_AMOUNT) {
        return Err(ApiError::Other(anyhow::anyhow!(
            "asked K-Ruoka to set item {item_id} to {wanted} but the cart it returned \
             shows {got}."
        )));
    }
    Ok(())
}

/// Confirm a `REMOVE-ITEM` actually removed it, for the same reason as
/// [`confirm_amount`]: 200 is not evidence.
fn confirm_absent(after: &Basket, item_id: &str) -> Result<(), ApiError> {
    if after.items.iter().any(|i| i.id == item_id) {
        return Err(ApiError::Other(anyhow::anyhow!(
            "K-Ruoka accepted removing item {item_id} but it is still in the cart it \
             returned. Call get_cart to see the current state."
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn basket_with(ids: &[&str]) -> Basket {
        let items: Vec<_> = ids
            .iter()
            .map(|id| serde_json::json!({"id": id, "ean": id}))
            .collect();
        serde_json::from_value(serde_json::json!({"id": "b1", "items": items})).unwrap()
    }

    #[test]
    fn existing_item_is_found() {
        assert_eq!(find_item(&basket_with(&["abc"]), "abc").unwrap().id, "abc");
    }

    /// `InvalidRequest`, not `Api`: K-Ruoka was never asked, so an "API error"
    /// message would claim something untrue.
    #[test]
    fn missing_item_reports_the_valid_ids() {
        match find_item(&basket_with(&["abc", "def"]), "xyz") {
            Err(ApiError::InvalidRequest(message)) => {
                assert!(message.contains("abc, def"), "{message}");
                assert!(message.contains("not EANs"), "{message}");
            }
            other => panic!("expected InvalidRequest, got {:?}", other.map(|i| &i.id)),
        }
    }

    #[test]
    fn empty_cart_says_so() {
        match find_item(&basket_with(&[]), "xyz") {
            Err(ApiError::InvalidRequest(message)) => {
                assert!(message.contains("empty"), "{message}")
            }
            other => panic!("expected InvalidRequest, got {:?}", other.map(|i| &i.id)),
        }
    }

    fn basket_with_amounts(items: &[(&str, f64)]) -> Basket {
        let items: Vec<_> = items
            .iter()
            .map(|(id, amount)| {
                serde_json::json!({
                    "id": id, "ean": id,
                    "amountInfo": {"amount": amount, "unit": "kpl"},
                })
            })
            .collect();
        serde_json::from_value(serde_json::json!({"id": "b1", "items": items})).unwrap()
    }

    /// `find_item` validated against an *earlier* read. Tool calls run concurrently,
    /// so a 200 that changed nothing is still reachable by interleaving even when the
    /// id was valid when checked.
    #[test]
    fn a_set_amount_that_changed_nothing_is_not_a_success() {
        let unchanged = basket_with_amounts(&[("abc", 2.0)]);
        assert!(confirm_amount(&unchanged, "abc", 5.0).is_err());
        assert!(confirm_amount(&unchanged, "abc", 2.0).is_ok());
    }

    /// K-Ruoka clamps at 999 and the returned cart shows the truth, so a clamped
    /// amount is the API working as measured -- not a discrepancy to reject.
    #[test]
    fn the_999_clamp_is_not_treated_as_a_discrepancy() {
        let clamped = basket_with_amounts(&[("abc", 999.0)]);
        assert!(confirm_amount(&clamped, "abc", 1e9).is_ok());
        assert!(confirm_amount(&clamped, "abc", 998.0).is_err());
    }

    /// Amount 0 is the documented spelling of remove, so absence is the success case
    /// and presence is the failure -- the opposite of every other amount.
    #[test]
    fn amount_zero_succeeds_only_when_the_item_is_gone() {
        assert!(confirm_amount(&basket_with_amounts(&[]), "abc", 0.0).is_ok());
        assert!(confirm_amount(&basket_with_amounts(&[("abc", 1.0)]), "abc", 0.0).is_err());
    }

    /// An item vanishing under a non-zero set is a concurrent removal, not a success.
    #[test]
    fn a_vanished_item_fails_a_non_zero_set() {
        assert!(confirm_amount(&basket_with_amounts(&[]), "abc", 3.0).is_err());
    }

    #[test]
    fn a_remove_that_left_the_item_behind_is_not_a_success() {
        assert!(confirm_absent(&basket_with(&["abc"]), "abc").is_err());
        assert!(confirm_absent(&basket_with(&["def"]), "abc").is_ok());
    }

    /// The unit a caller omits must come from the item, not a constant. Defaulting
    /// to "kpl" would convert a kg item to pieces -- corruption, not a no-op.
    #[test]
    fn omitted_unit_is_inherited_from_the_item() {
        let basket: Basket = serde_json::from_value(serde_json::json!({
            "id": "b1",
            "items": [{"id": "x", "ean": "x", "amountInfo": {"amount": 1.5, "unit": "kg"}}],
        }))
        .unwrap();
        let item = find_item(&basket, "x").unwrap();
        assert_eq!(item.amount_info.unit, "kg");
    }

    /// `productDetails.attributes` is the marker for "K-Ruoka knows this EAN".
    #[test]
    fn phantom_products_are_recognisable() {
        let basket: Basket = serde_json::from_value(serde_json::json!({
            "id": "b1",
            "items": [
                {"id": "real", "ean": "real",
                 "productDetails": {"attributes": {"ean": "real"}, "availability": {}}},
                // What ADD-ITEM returns for an EAN K-Ruoka has no record of.
                {"id": "phantom", "ean": "phantom", "pricing": null,
                 "productDetails": {"availability": {}}},
            ],
        }))
        .unwrap();
        assert!(find_item(&basket, "real").unwrap().is_known_product());
        assert!(!find_item(&basket, "phantom").unwrap().is_known_product());
    }

    /// Deliberately not keyed off `pricing`: a real product that is out of stock
    /// could plausibly have none, and rejecting a valid add is worse than the bug.
    #[test]
    fn a_real_product_without_pricing_is_still_known() {
        let basket: Basket = serde_json::from_value(serde_json::json!({
            "id": "b1",
            "items": [{"id": "x", "ean": "x", "pricing": null,
                       "productDetails": {"attributes": {}, "availability": {}}}],
        }))
        .unwrap();
        assert!(find_item(&basket, "x").unwrap().is_known_product());
    }

    #[test]
    fn invalid_store_error_names_the_store_the_caller_passed() {
        let raw = ApiError::Api {
            status: 422,
            message: r#"{"name":"InvalidStoreIdError","message":"Invalid store ID undefined"}"#
                .into(),
        };
        match clarify_store_error(raw, "ZZZZ9") {
            ApiError::Api { message, .. } => {
                assert!(message.contains("ZZZZ9"), "{message}");
                assert!(!message.contains("undefined"), "{message}");
            }
            other => panic!("expected an Api error, got {other:?}"),
        }
    }

    /// Other errors must pass through untouched -- notably AuthExpired, which the
    /// caller needs to see as itself.
    #[test]
    fn clarify_store_error_leaves_other_errors_alone() {
        assert!(matches!(
            clarify_store_error(ApiError::AuthExpired, "N137"),
            ApiError::AuthExpired
        ));
    }
}
