//! The cart tool surface.

use std::sync::Arc;

use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::model::{ContentBlock, Implementation, IntoContents, ServerCapabilities, ServerInfo};
use rmcp::{
    ServerHandler, handler::server::router::tool::ToolRouter, schemars, tool, tool_handler,
    tool_router,
};
use serde::Deserialize;

use crate::browser::KrApi;
use crate::browser::basket::Cart;
use crate::browser::session::ApiError;
use crate::types::{CartView, DEFAULT_UNIT};

/// Hand-copied onto four argument structs before, in two different wordings.
const STORE_ID_DESC: &str = "K-Ruoka store id, e.g. \"N137\" for K-Citymarket Helsinki \
                             Ruoholahti. A cart belongs to a store. Same id space as the \
                             `ruoka` plugin's get_stores.";

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct StoreArg {
    #[schemars(description = STORE_ID_DESC)]
    pub store_id: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct AddArg {
    #[schemars(description = STORE_ID_DESC)]
    pub store_id: String,
    #[schemars(
        description = "Product EAN barcode. Use the `ruoka` plugin's search_products \
                              to find one -- this server does not search."
    )]
    pub ean: String,
    #[schemars(
        description = "Resulting quantity, not an increment. Defaults to 1. Must be greater \
                              than 0. K-Ruoka caps it at 999."
    )]
    pub quantity: Option<f64>,
    #[schemars(
        description = "Unit for the quantity. Defaults to \"kpl\" (pieces), which is \
                              correct even for items priced by weight. Passed through to \
                              K-Ruoka verbatim and not validated."
    )]
    pub unit: Option<String>,
    #[schemars(description = "Only for store-local products; omit for the common case.")]
    pub local_store_id: Option<String>,
    #[schemars(
        description = "Let the store substitute a similar product if this one is out \
                              of stock. Defaults to true, matching the website."
    )]
    pub allow_substitutes: Option<bool>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct UpdateArg {
    #[schemars(description = STORE_ID_DESC)]
    pub store_id: String,
    #[schemars(
        description = "The basket item id from get_cart's `itemId` -- NOT the EAN. \
                              Call get_cart first to resolve it."
    )]
    pub item_id: String,
    #[schemars(
        description = "New quantity. 0 removes the item. Negative is rejected. K-Ruoka caps \
                              it at 999."
    )]
    pub quantity: f64,
    #[schemars(
        description = "Unit for the quantity. Defaults to the unit the item already has, \
                              which is almost always what you want -- passing the wrong one \
                              converts the item (e.g. 2 kg becomes 2 pieces)."
    )]
    pub unit: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct RemoveArg {
    #[schemars(description = STORE_ID_DESC)]
    pub store_id: String,
    #[schemars(
        description = "The basket item id from get_cart's `itemId` -- NOT the EAN. \
                              Call get_cart first to resolve it."
    )]
    pub item_id: String,
}

/// `store_id` is optional here, unlike everywhere else: any store reports the same
/// `userInfo`, and the caller most likely to reach for this tool is someone whose
/// setup is not working, who may not have a store id to hand.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct AuthArg {
    #[schemars(description = "Optional. Any store works; defaults to a sensible one.")]
    pub store_id: Option<String>,
}

#[derive(Debug, serde::Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AuthStatus {
    pub logged_in: bool,
    /// The signed-in account, when there is one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    pub detail: String,
}

#[derive(Clone)]
pub struct CartServer {
    api: Arc<dyn KrApi>,
    /// Read by the `#[tool_handler]`-generated `call_tool`/`list_tools`, which
    /// dead-code analysis does not see through.
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
}

impl CartServer {
    pub fn new(api: Arc<dyn KrApi>) -> Self {
        Self {
            api,
            tool_router: Self::tool_router(),
        }
    }

    fn cart(&self) -> Cart<'_> {
        Cart::new(&*self.api)
    }
}

/// A tool failure the *model* is meant to read and act on.
///
/// MCP has two error channels, and which one you use decides whether the text ever
/// reaches the model. JSON-RPC protocol errors are for the client's problems --
/// unknown tool, arguments that violate the schema -- and a client may reasonably
/// treat one as a transport failure. Tool *execution* errors are returned as an
/// ordinary result with `isError: true`, precisely so the model can see them and
/// try something else.
///
/// Everything this server produces is the second kind: "run login", "the item ids
/// currently in the cart are X and Y", "quantity must be greater than 0". Those
/// messages exist to be acted on, and as protocol errors they were at risk of being
/// swallowed. Verified on the wire, not assumed -- see `tests/mcp_protocol.rs`.
///
/// The mechanism: rmcp flips `isError` for any error type that converts to content;
/// only [`ErrorData`] short-circuits into a protocol error. Hence a type of our own.
pub struct ToolFailure(String);

impl IntoContents for ToolFailure {
    fn into_contents(self) -> Vec<ContentBlock> {
        vec![ContentBlock::text(self.0)]
    }
}

/// Preserve the distinction that matters to whoever reads it: "re-run login" is
/// actionable, "Cloudflare is blocking us" is a different problem with a different
/// remedy, and neither should read as a generic failure.
fn to_tool_failure(e: ApiError) -> ToolFailure {
    ToolFailure(match e {
        ApiError::AuthExpired => {
            "The K-Plussa session has expired. Run `k-ruoka-mcp login` in a terminal \
             on the machine hosting this server, then retry. The stored profile was left \
             untouched."
                .to_string()
        }
        other => other.to_string(),
    })
}

#[tool_router]
impl CartServer {
    #[tool(
        annotations(title = "Read cart", read_only_hint = true, idempotent_hint = true),
        description = "Read the K-Ruoka shopping cart for a store. Read-only and safe to call \
                       anytime. This is also the ONLY way to learn the `itemId` values that \
                       update_cart_item and remove_from_cart require, so call it first before \
                       either of those."
    )]
    async fn get_cart(
        &self,
        Parameters(StoreArg { store_id }): Parameters<StoreArg>,
    ) -> Result<Json<CartView>, ToolFailure> {
        let basket = self
            .cart()
            .active(&store_id)
            .await
            .map_err(to_tool_failure)?;
        Ok(Json(basket.into()))
    }

    #[tool(
        // Not destructive: setting a quantity only ever adds or adjusts one line.
        annotations(title = "Add to cart", destructive_hint = false, idempotent_hint = true),
        description = "Add a product to the cart by EAN barcode. Returns the updated cart. \
                       `quantity` is the resulting amount, not an increment: calling this \
                       twice with quantity 1 leaves 1 in the cart, not 2. To go from 2 to 3, \
                       pass quantity 3 (or use update_cart_item)."
    )]
    async fn add_to_cart(
        &self,
        Parameters(arg): Parameters<AddArg>,
    ) -> Result<Json<CartView>, ToolFailure> {
        let basket = self
            .cart()
            .add(
                &arg.store_id,
                &arg.ean,
                arg.quantity.unwrap_or(1.0),
                arg.unit.as_deref().unwrap_or(DEFAULT_UNIT),
                arg.local_store_id,
                arg.allow_substitutes.unwrap_or(true),
            )
            .await
            .map_err(to_tool_failure)?;
        Ok(Json(basket.into()))
    }

    #[tool(
        // Destructive: quantity 0 removes the item.
        annotations(title = "Change quantity", idempotent_hint = true),
        description = "Set the quantity of an item already in the cart. Takes the `itemId` \
                       from get_cart, not an EAN. Setting quantity to 0 removes the item."
    )]
    async fn update_cart_item(
        &self,
        Parameters(arg): Parameters<UpdateArg>,
    ) -> Result<Json<CartView>, ToolFailure> {
        let basket = self
            .cart()
            .set_amount(
                &arg.store_id,
                &arg.item_id,
                arg.quantity,
                arg.unit.as_deref(),
            )
            .await
            .map_err(to_tool_failure)?;
        Ok(Json(basket.into()))
    }

    #[tool(
        annotations(title = "Remove from cart", idempotent_hint = true),
        description = "Remove an item from the cart. Takes the `itemId` from get_cart, not \
                       an EAN."
    )]
    async fn remove_from_cart(
        &self,
        Parameters(arg): Parameters<RemoveArg>,
    ) -> Result<Json<CartView>, ToolFailure> {
        let basket = self
            .cart()
            .remove(&arg.store_id, &arg.item_id)
            .await
            .map_err(to_tool_failure)?;
        Ok(Json(basket.into()))
    }

    #[tool(
        // The one genuinely destructive tool here. Checkout is out of scope, so this
        // is as far as the damage can go, but it is still not undoable.
        annotations(title = "Empty the cart", destructive_hint = true, idempotent_hint = true),
        description = "Remove every item from the cart. This cannot be undone -- confirm with \
                       the user before calling it. The cart itself and its settings survive; \
                       only the items go."
    )]
    async fn clear_cart(
        &self,
        Parameters(StoreArg { store_id }): Parameters<StoreArg>,
    ) -> Result<Json<CartView>, ToolFailure> {
        let basket = self
            .cart()
            .clear(&store_id)
            .await
            .map_err(to_tool_failure)?;
        Ok(Json(basket.into()))
    }

    #[tool(
        annotations(title = "Check sign-in", read_only_hint = true, idempotent_hint = true),
        description = "Check whether the stored K-Plussa session is still logged in. Cheap. \
                       Worth calling first if a cart operation behaves unexpectedly, because \
                       an anonymous session still returns a valid -- but wrong, and not the \
                       account's -- cart rather than failing."
    )]
    async fn auth_status(
        &self,
        Parameters(AuthArg { store_id }): Parameters<AuthArg>,
    ) -> Result<Json<AuthStatus>, ToolFailure> {
        let store_id = store_id.unwrap_or_else(|| crate::login::DEFAULT_PROBE_STORE.to_string());
        match self.cart().active(&store_id).await {
            Ok(basket) => {
                let account = basket.user_info.display();
                Ok(Json(match &account {
                    Some(who) => AuthStatus {
                        logged_in: true,
                        account: Some(who.clone()),
                        detail: format!("Signed in as {who}."),
                    },
                    None => AuthStatus {
                        logged_in: false,
                        account: None,
                        detail: "Not signed in. The cart reachable right now is an anonymous \
                                 one, not the account's. Run `k-ruoka-mcp login` on the \
                                 machine hosting this server."
                            .to_string(),
                    },
                }))
            }
            Err(ApiError::AuthExpired) => Ok(Json(AuthStatus {
                logged_in: false,
                account: None,
                detail: "The K-Plussa session has expired. Run `k-ruoka-mcp login` again."
                    .to_string(),
            })),
            Err(e) => Err(to_tool_failure(e)),
        }
    }
}

#[tool_handler]
impl ServerHandler for CartServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            // Without this the server introduces itself as "rmcp" 3.0.1 -- rmcp's
            // `from_build_env` reads CARGO_CRATE_NAME from inside its own crate.
            // This string is what MCP clients display.
            .with_server_info(Implementation::new(
                env!("CARGO_PKG_NAME"),
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(
                "Manages the shopping cart of one K-Ruoka (k-ruoka.fi) account.\n\n\
             Every tool needs a `store_id` (e.g. \"N137\"); a cart belongs to a store. \
             `update_cart_item` and `remove_from_cart` take a basket `itemId`, which only \
             exists once an item is in the cart and is not the EAN -- get it from `get_cart`. \
             This server cannot search for products; use the `ruoka` plugin for that and pass \
             the EAN here. Checkout is deliberately not supported: nothing here can spend \
             money.",
            )
    }
}
