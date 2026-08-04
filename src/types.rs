//! Serde shapes for the basket API.
//!
//! Every field here was observed on a live response, not read
//! from documentation -- there isn't any. Structs are therefore deliberately
//! permissive: unknown fields are ignored and almost everything is `default`ed,
//! so a K-Ruoka deploy that adds or drops a field degrades the output rather
//! than failing the call.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The raw basket, as returned by `/kr-api/basket/active` and by every
/// successful `PATCH /kr-api/basket/by-id/{id}`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Basket {
    pub id: String,
    #[serde(default)]
    pub items: Vec<BasketItem>,
    #[serde(default)]
    pub price_summary: PriceSummary,
    pub store: Option<Store>,
    #[serde(default)]
    pub user_info: UserInfo,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BasketItem {
    /// The basket's own item id, and what `SET-ITEM-AMOUNT` / `REMOVE-ITEM` take.
    ///
    /// For ordinary products this is observably equal to the EAN, but the
    /// frontend keys items by `(localStoreId, ean)`, so that equality is not
    /// something to depend on for store-local products. Always resolve it from a
    /// `get_cart` read.
    pub id: String,
    #[serde(default)]
    pub ean: String,
    #[serde(default)]
    pub name: LocalizedName,
    #[serde(default)]
    pub amount_info: AmountInfo,
    #[serde(default)]
    pub pricing: Option<Pricing>,
    #[serde(default)]
    pub allow_substitutes: bool,
    #[serde(default)]
    pub product_details: ProductDetails,
}

impl BasketItem {
    /// Whether K-Ruoka actually has a product record for this EAN.
    ///
    /// `ADD-ITEM` accepts *any* EAN and cheerfully puts an item named "Tuntematon
    /// tuote" / "Unknown product" in the basket, so a typo'd barcode silently
    /// pollutes the cart while reporting success.
    ///
    /// The discriminator is the presence of `productDetails.attributes`, not
    /// `pricing`: `attributes` means "we have a record of this product", while
    /// `availability` means "you can get it here". A real product that is simply
    /// out of stock still has `attributes`, so keying off `pricing == null` would
    /// risk rejecting valid adds. Observed live -- two real EANs carried
    /// `[attributes, availability, category, soldBy]`, the phantom only
    /// `[availability]`.
    pub fn is_known_product(&self) -> bool {
        self.product_details.attributes.is_some()
    }
}

/// Only the one key we need. The full blob is several KB of nutrition data,
/// images and category trees.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ProductDetails {
    #[serde(default)]
    pub attributes: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct LocalizedName {
    #[serde(default)]
    pub finnish: Option<String>,
    #[serde(default)]
    pub english: Option<String>,
    #[serde(default)]
    pub swedish: Option<String>,
}

impl LocalizedName {
    pub fn best(&self) -> String {
        self.finnish
            .clone()
            .or_else(|| self.english.clone())
            .or_else(|| self.swedish.clone())
            .unwrap_or_default()
    }
}

/// K-Ruoka amounts always carry a unit. `kpl` ("pieces") is overwhelmingly the
/// common one, including for goods that are *priced* by weight.
pub const DEFAULT_UNIT: &str = "kpl";

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AmountInfo {
    pub amount: f64,
    pub unit: String,
}

impl Default for AmountInfo {
    fn default() -> Self {
        Self {
            amount: 0.0,
            unit: DEFAULT_UNIT.into(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Pricing {
    #[serde(default)]
    pub price: Option<f64>,
    #[serde(default)]
    pub unit: Option<String>,
    /// True for weight-priced goods, where the charge is settled at picking.
    #[serde(default)]
    pub is_approximate: bool,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PriceSummary {
    #[serde(default)]
    pub items_sub_total: f64,
    #[serde(default)]
    pub grand_total: f64,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
pub struct Store {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
}

/// Empty strings throughout on an anonymous session; populated once logged in.
/// This is the only reliable signal of whether the basket belongs to an account,
/// because an anonymous caller gets a perfectly valid basket rather than a 401.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserInfo {
    #[serde(default)]
    pub first_name: String,
    #[serde(default)]
    pub last_name: String,
    #[serde(default)]
    pub email: String,
}

impl UserInfo {
    /// The signed-in account, or `None` when the session is anonymous.
    ///
    /// The only reliable signal: an anonymous caller gets a perfectly valid basket
    /// rather than a 401, so a successful call proves nothing.
    pub fn display(&self) -> Option<String> {
        if self.email.is_empty() && self.first_name.is_empty() && self.last_name.is_empty() {
            return None;
        }
        let name = format!("{} {}", self.first_name, self.last_name)
            .trim()
            .to_string();
        Some(match (name.is_empty(), self.email.is_empty()) {
            (false, false) => format!("{name} <{}>", self.email),
            (false, true) => name,
            (true, _) => self.email.clone(),
        })
    }
}

/// A cart mutation. `PATCH /kr-api/basket/by-id/{id}` always takes an array of
/// these, even for a single change.
///
/// Names match the literals in K-Ruoka's own bundle (`pendingEvents.push(...)`).
/// Two further events exist there -- `SET-ITEM-ALLOW-SUBSTITUTES` and
/// `SET-ITEM-MESSAGE-TO-STORE` -- deliberately not modelled, as they are outside
/// the tool surface.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "SCREAMING-KEBAB-CASE")]
pub enum BasketEvent {
    AddItem {
        item: NewItem,
    },
    #[serde(rename_all = "camelCase")]
    SetItemAmount {
        item_id: String,
        value: AmountInfo,
    },
    #[serde(rename_all = "camelCase")]
    RemoveItem {
        item_id: String,
    },
    ClearItems,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NewItem {
    pub ean: String,
    /// Only set for store-local products; omitted entirely otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_store_id: Option<String>,
    pub allow_substitutes: bool,
    pub amount_info: AmountInfo,
}

// ---------------------------------------------------------------------------
// What the MCP tools actually return.
//
// The raw basket carries a large `productDetails` blob per item (nutrition,
// images, category trees). Returning that verbatim would bury the few fields a
// caller needs in several KB of noise per item, so tools return this view.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CartView {
    pub basket_id: String,
    pub store: Store,
    /// `null` when the session is anonymous -- the cart is then a throwaway
    /// basket, not the account's.
    pub account: Option<String>,
    pub items: Vec<CartItemView>,
    pub totals: PriceSummary,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CartItemView {
    /// Pass this as `item_id` to `update_cart_item` / `remove_from_cart`.
    pub item_id: String,
    pub ean: String,
    pub name: String,
    pub amount: f64,
    pub unit: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub price: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub price_unit: Option<String>,
    /// Weight-priced item: the final charge is settled when the order is picked.
    pub price_is_approximate: bool,
}

impl From<Basket> for CartView {
    fn from(b: Basket) -> Self {
        Self {
            basket_id: b.id,
            store: b.store.unwrap_or_default(),
            account: b.user_info.display(),
            items: b
                .items
                .into_iter()
                .map(|i| CartItemView {
                    item_id: i.id,
                    ean: i.ean,
                    name: i.name.best(),
                    amount: i.amount_info.amount,
                    unit: i.amount_info.unit,
                    price: i.pricing.as_ref().and_then(|p| p.price),
                    price_unit: i.pricing.as_ref().and_then(|p| p.unit.clone()),
                    price_is_approximate: i.pricing.as_ref().is_some_and(|p| p.is_approximate),
                })
                .collect(),
            totals: b.price_summary,
        }
    }
}

// --- Product search -----------------------------------------------------------------

/// `POST /kr-api/v2/product-search/{term}`. Only the fields the tool surfaces are
/// modelled; the response also carries aggregations, brands and suggestions.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProductSearchResponse {
    #[serde(default)]
    pub result: Vec<ProductHit>,
    #[serde(default)]
    pub total_hits: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProductHit {
    pub product: SearchProduct,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchProduct {
    #[serde(default)]
    pub ean: String,
    #[serde(default)]
    pub localized_name: LocalizedName,
    #[serde(default)]
    pub brand: Option<Brand>,
    /// Whether it can be bought at this store right now, as opposed to merely existing
    /// in the catalogue. Surfaced because an unavailable hit is still a valid EAN and
    /// `add_to_cart` would accept it.
    #[serde(default)]
    pub is_available: bool,
    /// Price lives under `mobilescan`, not at the top level, and is absent often enough
    /// that it has to be optional.
    #[serde(default)]
    pub mobilescan: Option<MobileScan>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Brand {
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MobileScan {
    #[serde(default)]
    pub pricing: Option<MobileScanPricing>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MobileScanPricing {
    #[serde(default)]
    pub normal: Option<NormalPrice>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NormalPrice {
    #[serde(default)]
    pub price: Option<f64>,
    #[serde(default)]
    pub unit: Option<String>,
    /// Comparison price, e.g. per kg, which is what makes weight-priced items
    /// comparable at all.
    #[serde(default)]
    pub unit_price: Option<UnitPrice>,
    #[serde(default)]
    pub is_approximate: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UnitPrice {
    #[serde(default)]
    pub value: Option<f64>,
    #[serde(default)]
    pub unit: Option<String>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProductSearchView {
    /// How many the catalogue holds in total, which is usually far more than `results`.
    pub total_hits: u64,
    pub results: Vec<ProductView>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProductView {
    /// Pass this as `ean` to `add_to_cart`.
    pub ean: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub brand: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub price: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub price_unit: Option<String>,
    /// e.g. "1.69 EUR/kg". Absent when K-Ruoka gives no comparison price.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comparison_price: Option<String>,
    /// Weight-priced: the final charge is settled when the order is picked.
    pub price_is_approximate: bool,
    /// `false` means the EAN is real but not buyable at this store right now.
    pub is_available: bool,
}

impl From<ProductSearchResponse> for ProductSearchView {
    fn from(r: ProductSearchResponse) -> Self {
        Self {
            total_hits: r.total_hits,
            results: r.result.into_iter().map(|h| h.product.into()).collect(),
        }
    }
}

impl From<SearchProduct> for ProductView {
    fn from(p: SearchProduct) -> Self {
        let normal = p
            .mobilescan
            .as_ref()
            .and_then(|m| m.pricing.as_ref())
            .and_then(|p| p.normal.as_ref());
        Self {
            ean: p.ean,
            name: p.localized_name.best(),
            brand: p.brand.and_then(|b| b.name),
            price: normal.and_then(|n| n.price),
            price_unit: normal.and_then(|n| n.unit.clone()),
            comparison_price: normal.and_then(|n| n.unit_price.as_ref()).and_then(|u| {
                match (u.value, u.unit.as_deref()) {
                    (Some(value), Some(unit)) => Some(format!("{value} EUR/{unit}")),
                    _ => None,
                }
            }),
            price_is_approximate: normal.is_some_and(|n| n.is_approximate),
            is_available: p.is_available,
        }
    }
}

// --- Store search -------------------------------------------------------------------

/// `POST /kr-api/stores/search`, which takes its query in a JSON body rather than the
/// path or query string like product search does.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoreSearchResponse {
    #[serde(default)]
    pub results: Vec<StoreHit>,
    #[serde(default)]
    pub total_hits: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoreHit {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub location: Option<String>,
    #[serde(default)]
    pub chain_name: Option<String>,
    /// A store that is not a web store has no cart to operate on, so this decides
    /// whether an id is usable here at all.
    #[serde(default)]
    pub is_web_store: bool,
    #[serde(default)]
    pub has_pickup: bool,
    #[serde(default)]
    pub has_home_delivery: bool,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct StoreSearchView {
    pub total_hits: u64,
    pub results: Vec<StoreView>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct StoreView {
    /// Pass this as `store_id` to every other tool.
    pub store_id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chain: Option<String>,
    /// `false` means this store has no online cart, so the other tools cannot use it.
    pub is_web_store: bool,
    pub has_pickup: bool,
    pub has_home_delivery: bool,
}

impl From<StoreSearchResponse> for StoreSearchView {
    fn from(r: StoreSearchResponse) -> Self {
        Self {
            total_hits: r.total_hits,
            results: r
                .results
                .into_iter()
                .map(|s| StoreView {
                    store_id: s.id,
                    name: s.name,
                    location: s.location,
                    chain: s.chain_name,
                    is_web_store: s.is_web_store,
                    has_pickup: s.has_pickup,
                    has_home_delivery: s.has_home_delivery,
                })
                .collect(),
        }
    }
}

// --- Personal offers ------------------------------------------------------------------

/// `POST /kr-api/tos-offers` -- OmaPlussa-edut, the account's personalised offers.
/// "tos" is K-Ruoka's own term, seen throughout the frontend (`tosOffers`,
/// `isTargetingOffersAllowed`); not modelled here since nothing surfaces it.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PersonalOffersResponse {
    #[serde(default)]
    pub offers: Vec<PersonalOffer>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PersonalOffer {
    #[serde(default)]
    pub localized_title: LocalizedName,
    #[serde(default)]
    pub pricing: Option<PersonalOfferPricing>,
    #[serde(default)]
    pub normal_pricing: Option<PersonalOfferNormalPricing>,
    #[serde(default)]
    pub products: Vec<PersonalOfferProduct>,
    /// How many redemptions are left on the Plussa card. Distinct from `valid_until`:
    /// an offer can still have time left but no redemptions, or the reverse.
    #[serde(default)]
    pub remaining_quantity: Option<u32>,
    #[serde(default)]
    pub valid_until: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PersonalOfferPricing {
    #[serde(default)]
    pub price: Option<f64>,
    #[serde(default)]
    pub discount_percentage: Option<String>,
    /// What `price` buys -- often more than one item. A live "-57-65 %" coffee offer
    /// priced at 10.00 EUR turned out to mean 10.00 EUR for three (`unit.fi` was
    /// "3 kpl"), not one; every offer observed carried this field.
    #[serde(default)]
    pub unit: Option<OfferUnit>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct OfferUnit {
    #[serde(default)]
    pub fi: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PersonalOfferNormalPricing {
    #[serde(default)]
    pub price: Option<f64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PersonalOfferProduct {
    pub product: OfferProduct,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OfferProduct {
    #[serde(default)]
    pub ean: String,
    #[serde(default)]
    pub localized_name: LocalizedName,
    #[serde(default)]
    pub is_available: bool,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PersonalOffersView {
    /// The store the offers are scoped to. Measured against two real stores: the sets
    /// differ (one was a strict superset of the other) rather than being identical
    /// everywhere, so this is worth echoing back rather than assuming the caller
    /// remembers which store a cached-looking result came from.
    pub store_id: String,
    pub offers: Vec<PersonalOfferView>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PersonalOfferView {
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub price: Option<f64>,
    /// What `price` buys, e.g. "3 kpl" for a buy-three bundle. Read this before
    /// treating `price` as a per-item price -- it usually is not for a bundle offer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub price_unit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub normal_price: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub discount_percentage: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remaining_quantity: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub valid_until: Option<String>,
    /// Any one of these that is available redeems the offer -- pass an available EAN
    /// to `add_to_cart`. `is_available` still has to be checked here, same as
    /// `search_products`: an offer can list a product this store does not stock.
    /// Every offer observed so far was already loaded onto the account's Plussa card
    /// with no separate activation call; not verified as a universal K-Ruoka guarantee.
    pub products: Vec<PersonalOfferProductView>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PersonalOfferProductView {
    /// Pass this as `ean` to `add_to_cart`.
    pub ean: String,
    pub name: String,
    /// `false` means the EAN is real but not buyable at this store right now.
    pub is_available: bool,
}

impl From<PersonalOffer> for PersonalOfferView {
    fn from(o: PersonalOffer) -> Self {
        Self {
            title: o.localized_title.best(),
            price: o.pricing.as_ref().and_then(|p| p.price),
            price_unit: o
                .pricing
                .as_ref()
                .and_then(|p| p.unit.as_ref())
                .and_then(|u| u.fi.clone()),
            normal_price: o.normal_pricing.as_ref().and_then(|p| p.price),
            discount_percentage: o.pricing.and_then(|p| p.discount_percentage),
            remaining_quantity: o.remaining_quantity,
            valid_until: o.valid_until,
            products: o.products.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<PersonalOfferProduct> for PersonalOfferProductView {
    fn from(p: PersonalOfferProduct) -> Self {
        Self {
            ean: p.product.ean,
            name: p.product.localized_name.best(),
            is_available: p.product.is_available,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verbatim from a live `PATCH /kr-api/basket/by-id/...`, trimmed of the
    /// `productDetails` blob.
    const LIVE: &str = r#"{
      "schemaVersion": 5,
      "id": "c0fa67a6-4b56-4dc6-9a7e-506c2b29b7cf",
      "name": "Ostoskori",
      "userInfo": {"firstName":"","lastName":"","email":"","phoneNumber":""},
      "items": [{
        "allowSubstitutes": true,
        "amountInfo": {"amount": 1, "unit": "kpl"},
        "ean": "2000818700008",
        "id": "2000818700008",
        "name": {"english":"Pirkka banana","finnish":"Pirkka banaani","swedish":"Pirkka banan"},
        "pricing": {"isApproximate": true, "price": 0.3, "unit": "kg"},
        "productDetails": {"attributes": {"ean": "2000818700008"}}
      }],
      "priceSummary": {"grandTotal": 0.3, "itemsSubTotal": 0.3,
                       "plussaSavings": {"total": 0, "type": "POTENTIAL"}},
      "store": {"id": "N137", "name": "K-Citymarket Helsinki Ruoholahti"}
    }"#;

    #[test]
    fn parses_a_live_basket_into_a_view() {
        let view: CartView = serde_json::from_str::<Basket>(LIVE).unwrap().into();
        assert_eq!(view.store.id, "N137");
        assert_eq!(view.totals.grand_total, 0.3);
        assert_eq!(view.items.len(), 1);

        let item = &view.items[0];
        assert_eq!(item.item_id, "2000818700008");
        assert_eq!(item.name, "Pirkka banaani");
        assert_eq!(item.amount, 1.0);
        assert_eq!(item.unit, "kpl");
        assert!(item.price_is_approximate);
    }

    /// An anonymous session returns a valid basket with a blank `userInfo`, so
    /// "the call worked" must not be read as "we are logged in".
    #[test]
    fn anonymous_basket_reports_no_account() {
        let view: CartView = serde_json::from_str::<Basket>(LIVE).unwrap().into();
        assert_eq!(view.account, None);
    }

    #[test]
    fn logged_in_basket_reports_the_account() {
        let json = LIVE.replace(
            r#""firstName":"","lastName":"","email":""#,
            r#""firstName":"Niko","lastName":"Savola","email":"n@example.com"#,
        );
        let view: CartView = serde_json::from_str::<Basket>(&json).unwrap().into();
        assert_eq!(view.account.as_deref(), Some("Niko Savola <n@example.com>"));
    }

    /// The wire format is dictated by K-Ruoka's bundle, so pin it exactly.
    #[test]
    fn events_serialise_to_the_shapes_the_bundle_expects() {
        let add = BasketEvent::AddItem {
            item: NewItem {
                ean: "2000818700008".into(),
                local_store_id: None,
                allow_substitutes: true,
                amount_info: AmountInfo {
                    amount: 2.0,
                    unit: "kpl".into(),
                },
            },
        };
        assert_eq!(
            serde_json::to_value(&add).unwrap(),
            serde_json::json!({
                "type": "ADD-ITEM",
                "item": {
                    "ean": "2000818700008",
                    "allowSubstitutes": true,
                    "amountInfo": {"amount": 2.0, "unit": "kpl"}
                }
            })
        );

        let set = BasketEvent::SetItemAmount {
            item_id: "x".into(),
            value: AmountInfo {
                amount: 3.0,
                unit: "kpl".into(),
            },
        };
        assert_eq!(
            serde_json::to_value(&set).unwrap(),
            serde_json::json!({
                "type": "SET-ITEM-AMOUNT",
                "itemId": "x",
                "value": {"amount": 3.0, "unit": "kpl"}
            })
        );

        assert_eq!(
            serde_json::to_value(BasketEvent::RemoveItem {
                item_id: "x".into()
            })
            .unwrap(),
            serde_json::json!({"type": "REMOVE-ITEM", "itemId": "x"})
        );
        assert_eq!(
            serde_json::to_value(BasketEvent::ClearItems).unwrap(),
            serde_json::json!({"type": "CLEAR-ITEMS"})
        );
    }

    /// `localStoreId` is conditional in the bundle -- present only for
    /// store-local products, and it must be absent rather than null otherwise.
    #[test]
    fn local_store_id_is_omitted_when_absent() {
        let item = NewItem {
            ean: "1".into(),
            local_store_id: None,
            allow_substitutes: false,
            amount_info: AmountInfo::default(),
        };
        let v = serde_json::to_value(&item).unwrap();
        assert!(!v.as_object().unwrap().contains_key("localStoreId"));

        let item = NewItem {
            local_store_id: Some("N137".into()),
            ..item
        };
        assert_eq!(serde_json::to_value(&item).unwrap()["localStoreId"], "N137");
    }
}
