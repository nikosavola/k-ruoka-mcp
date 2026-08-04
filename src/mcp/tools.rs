//! The cart tool surface.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::model::{ContentBlock, Implementation, IntoContents, ServerCapabilities, ServerInfo};
use rmcp::{
    ServerHandler, handler::server::router::tool::ToolRouter, schemars, tool, tool_handler,
    tool_router,
};
use serde::Deserialize;

use crate::browser::KrApi;
use crate::browser::basket::Cart;
use crate::browser::catalog::Catalog;
use crate::browser::offers::Offers;
use crate::browser::session::ApiError;
use crate::login_flow::{LoginFlow, LoginProgress};
use crate::types::{
    CartView, DEFAULT_UNIT, PersonalOffersView, ProductSearchView, StoreSearchView,
};

/// Used on argument structs where the caller must always supply a store id.
const STORE_ID_DESC: &str = "K-Ruoka store id, e.g. \"N137\" for K-Citymarket Helsinki \
                             Ruoholahti. A cart belongs to a store. Use search_stores to \
                             find one.";

/// Used on argument structs where the store id may be omitted when a default has been set.
const STORE_ID_OPT_DESC: &str = "K-Ruoka store id, e.g. \"N137\" for K-Citymarket Helsinki \
                                  Ruoholahti. A cart belongs to a store. Use search_stores to \
                                  find one. May be omitted if a default store was set with \
                                  set_default_store.";

/// Used by tools whose only argument is a store id that may fall back to the default
/// (`get_cart`, `clear_cart`).
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct StoreArg {
    #[schemars(description = STORE_ID_OPT_DESC)]
    pub store_id: Option<String>,
}

/// Used by `set_default_store`, where the store id is always required.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SetDefaultStoreArg {
    #[schemars(description = STORE_ID_DESC)]
    pub store_id: String,
}

#[derive(Debug, serde::Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DefaultStoreStatus {
    pub default_store: String,
}

const LIMIT_DESC: &str = "How many results to return. Defaults to 10, capped at 50.";

/// Matches the `login` subcommand's own default, so the printed instructions and this
/// tool agree without the caller having to think about it.
const DEFAULT_DEBUG_PORT: u16 = 9222;

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct StartLoginArg {
    #[schemars(
        description = "Chrome remote-debugging port, for reaching the browser on a \
                              headless host. Defaults to 9222. Only change it if that port \
                              is taken."
    )]
    pub port: Option<u16>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SearchProductsArg {
    #[schemars(description = STORE_ID_OPT_DESC)]
    pub store_id: Option<String>,
    #[schemars(
        description = "What to search for, in Finnish -- the catalogue is Finnish, so \
                              \"maito\" finds far more than \"milk\". Free text, e.g. \
                              \"pirkka banaani\" or \"kaurajuoma\"."
    )]
    pub query: String,
    #[schemars(description = LIMIT_DESC)]
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SearchStoresArg {
    #[schemars(
        description = "Place or store name, e.g. \"Ruoholahti\" or \"K-Citymarket \
                              Tampere\"."
    )]
    pub query: String,
    #[schemars(description = LIMIT_DESC)]
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct AddArg {
    #[schemars(description = STORE_ID_OPT_DESC)]
    pub store_id: Option<String>,
    #[schemars(description = "Product EAN barcode. Use search_products to find one.")]
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
    #[schemars(description = STORE_ID_OPT_DESC)]
    pub store_id: Option<String>,
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
    #[schemars(description = STORE_ID_OPT_DESC)]
    pub store_id: Option<String>,
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

/// Read a previously saved default store id from `path`.
///
/// Returns `None` if the file does not exist, is unreadable, or is empty --
/// any of which means "no persisted value".
fn read_default_store(path: &std::path::Path) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Write `store_id` to `path`, creating parent directories if needed.
///
/// Failures are logged to stderr but do not abort the tool call: the value is
/// already live in memory, and a write failure (e.g. read-only filesystem)
/// should not make `set_default_store` appear to fail to the model.
async fn write_default_store(path: &std::path::Path, store_id: &str) {
    if let Some(parent) = path.parent()
        && let Err(e) = tokio::fs::create_dir_all(parent).await
    {
        eprintln!(
            "k-ruoka-mcp: could not create directory {}: {e}",
            parent.display()
        );
        return;
    }
    if let Err(e) = tokio::fs::write(path, store_id).await {
        eprintln!(
            "k-ruoka-mcp: could not save default store to {}: {e}",
            path.display()
        );
    }
}

#[derive(Clone)]
pub struct CartServer {
    api: Arc<dyn KrApi>,
    /// `None` when nothing can drive an interactive login, which is the case for the
    /// tests and would be the case for any other embedding. The login tools then say
    /// so rather than being absent, since a missing tool is harder to explain than one
    /// that tells you why it cannot help.
    login: Option<Arc<dyn LoginFlow>>,
    /// Shared across all clones so a `set_default_store` call persists for the life
    /// of the server, regardless of which clone handles the next tool call.
    default_store: Arc<Mutex<Option<String>>>,
    /// Where to persist the default store between restarts. `None` in test/embedded
    /// contexts that have no real profile directory.
    store_path: Option<Arc<PathBuf>>,
    /// Read by the `#[tool_handler]`-generated `call_tool`/`list_tools`, which
    /// dead-code analysis does not see through.
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
}

impl CartServer {
    pub fn new(api: Arc<dyn KrApi>) -> Self {
        Self {
            api,
            login: None,
            default_store: Arc::new(Mutex::new(None)),
            store_path: None,
            tool_router: Self::tool_router(),
        }
    }

    pub fn with_login(api: Arc<dyn KrApi>, login: Arc<dyn LoginFlow>) -> Self {
        Self {
            api,
            login: Some(login),
            default_store: Arc::new(Mutex::new(None)),
            store_path: None,
            tool_router: Self::tool_router(),
        }
    }

    /// Attach a persistence file for the default store and load any previously saved value.
    ///
    /// `set_default_store` will write to this file so the value survives restarts. On
    /// construction the file is read (if it exists) and used as the initial default.
    /// `K_RUOKA_DEFAULT_STORE` is read as a fallback when no file exists yet.
    pub fn with_default_store_path(self, path: PathBuf) -> Self {
        // File takes precedence; env var is a bootstrap fallback for first-run.
        let initial = read_default_store(&path)
            .or_else(|| {
                std::env::var("K_RUOKA_DEFAULT_STORE")
                    .ok()
                    .map(|s| s.trim().to_string())
            })
            .filter(|s| !s.is_empty());
        if let Some(store) = initial {
            *self.default_store.lock().unwrap() = Some(store);
        }
        Self {
            store_path: Some(Arc::new(path)),
            ..self
        }
    }

    fn login_flow(&self) -> Result<&Arc<dyn LoginFlow>, ToolFailure> {
        self.login.as_ref().ok_or_else(|| {
            ToolFailure(
                "This server cannot drive an interactive login. Run `k-ruoka-mcp login` \
                 in a terminal on the machine hosting it instead."
                    .to_string(),
            )
        })
    }

    /// Resolve a store id: use the explicitly-provided one if present, otherwise fall
    /// back to the session default, or fail with a clear instruction.
    fn resolve_store(&self, provided: Option<String>) -> Result<String, ToolFailure> {
        provided
            .or_else(|| self.default_store.lock().unwrap().clone())
            .ok_or_else(|| {
                ToolFailure(
                    "No store_id provided and no default store has been set. \
                    Call set_default_store first, or pass store_id explicitly."
                        .to_string(),
                )
            })
    }

    fn cart(&self) -> Cart<'_> {
        Cart::new(&*self.api)
    }

    fn catalog(&self) -> Catalog<'_> {
        Catalog::new(&*self.api)
    }

    fn offers(&self) -> Offers<'_> {
        Offers::new(&*self.api)
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
/// only `ErrorData` (rmcp's own) short-circuits into a protocol error. Hence a type of
/// our own. Not an intra-doc link: with `--no-deps`, the rmcp pages it would point at
/// are never generated, so even a fully-qualified path would resolve to a dead link.
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
        annotations(title = "Start login", read_only_hint = false, idempotent_hint = true),
        description = "Open a browser for the user to sign in to K-Plussa by hand, and \
                       return the instructions to give them. Use this when auth_status says \
                       the session is not signed in. Relay the returned `instructions` \
                       VERBATIM: they differ between a desktop and a headless host, and \
                       only the running server knows which it is. Then poll login_status. \
                       This never sees the user's credentials, and it takes over the \
                       browser, so the cart tools will not work until the login finishes \
                       or is cancelled."
    )]
    async fn start_login(
        &self,
        Parameters(arg): Parameters<StartLoginArg>,
    ) -> Result<Json<LoginProgress>, ToolFailure> {
        let progress = self
            .login_flow()?
            .start(arg.port.unwrap_or(DEFAULT_DEBUG_PORT))
            .await
            .map_err(to_tool_failure)?;
        Ok(Json(progress))
    }

    #[tool(
        annotations(title = "Login status", read_only_hint = true, idempotent_hint = true),
        description = "How the login started by start_login is going: `waiting`, \
                       `signedIn`, `failed`, or `notStarted`. Poll this every 10 to 20 \
                       seconds while the user signs in; they may need a couple of minutes \
                       for a password manager and MFA."
    )]
    async fn login_status(&self) -> Result<Json<LoginProgress>, ToolFailure> {
        let progress = self.login_flow()?.status().await.map_err(to_tool_failure)?;
        Ok(Json(progress))
    }

    #[tool(
        annotations(title = "Cancel login", idempotent_hint = true),
        description = "Give up on a login in progress and close its browser, so the cart \
                       tools work again. Any previously stored session is left untouched."
    )]
    async fn cancel_login(&self) -> Result<Json<LoginProgress>, ToolFailure> {
        let progress = self.login_flow()?.cancel().await.map_err(to_tool_failure)?;
        Ok(Json(progress))
    }

    #[tool(
        // A local state write, not a cart mutation.
        annotations(
            title = "Set default store",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true
        ),
        description = "Set a default store so other tools can omit store_id. Once set, \
                       any tool that takes a store_id will use this value when store_id \
                       is not explicitly provided. The value is persisted to the profile \
                       directory and restored on restart. Use search_stores to find a \
                       store_id."
    )]
    async fn set_default_store(
        &self,
        Parameters(SetDefaultStoreArg { store_id }): Parameters<SetDefaultStoreArg>,
    ) -> Result<Json<DefaultStoreStatus>, ToolFailure> {
        *self.default_store.lock().unwrap() = Some(store_id.clone());
        if let Some(path) = &self.store_path {
            write_default_store(path, &store_id).await;
        }
        Ok(Json(DefaultStoreStatus {
            default_store: store_id,
        }))
    }

    #[tool(
        annotations(
            title = "Search products",
            read_only_hint = true,
            idempotent_hint = true
        ),
        description = "Find products by name and get their EAN barcodes. Read-only. This is \
                       how you get the `ean` that add_to_cart needs, so call it first when \
                       the user names a product rather than a barcode. Results are specific \
                       to the store: price and availability differ between them."
    )]
    async fn search_products(
        &self,
        Parameters(arg): Parameters<SearchProductsArg>,
    ) -> Result<Json<ProductSearchView>, ToolFailure> {
        let store_id = self.resolve_store(arg.store_id)?;
        let found = self
            .catalog()
            .search_products(&store_id, &arg.query, arg.limit)
            .await
            .map_err(to_tool_failure)?;
        Ok(Json(found.into()))
    }

    #[tool(
        annotations(
            title = "Personal offers",
            read_only_hint = true,
            idempotent_hint = true
        ),
        description = "The account's personalised OmaPlussa-edut offers at a store: what \
                       is on personal offer right now. Read-only. Every offer seen so \
                       far already sat on the account's Plussa card, so redeeming one \
                       was just buying a listed product -- pass an EAN whose \
                       isAvailable is true to add_to_cart, same check search_products \
                       needs. Check priceUnit: a price is often for several items \
                       (e.g. \"3 kpl\"), not one. Time-limited: call this fresh each \
                       time rather than caching the result. An anonymous session \
                       returns an empty list rather than an error -- check \
                       auth_status if that is not what you expected."
    )]
    async fn get_personal_offers(
        &self,
        Parameters(arg): Parameters<StoreArg>,
    ) -> Result<Json<PersonalOffersView>, ToolFailure> {
        let store_id = self.resolve_store(arg.store_id)?;
        let offers = self
            .offers()
            .personal_offers(&store_id)
            .await
            .map_err(to_tool_failure)?;
        Ok(Json(PersonalOffersView {
            store_id,
            offers: offers.offers.into_iter().map(Into::into).collect(),
        }))
    }

    #[tool(
        annotations(title = "Search stores", read_only_hint = true, idempotent_hint = true),
        description = "Find K-Ruoka stores by name or place, and get the `store_id` every \
                       other tool needs. Read-only. Check `isWebStore`: a store without an \
                       online cart cannot be used by the other tools."
    )]
    async fn search_stores(
        &self,
        Parameters(arg): Parameters<SearchStoresArg>,
    ) -> Result<Json<StoreSearchView>, ToolFailure> {
        let found = self
            .catalog()
            .search_stores(&arg.query, arg.limit)
            .await
            .map_err(to_tool_failure)?;
        Ok(Json(found.into()))
    }

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
        let store_id = self.resolve_store(store_id)?;
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
        let store_id = self.resolve_store(arg.store_id)?;
        let basket = self
            .cart()
            .add(
                &store_id,
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
        let store_id = self.resolve_store(arg.store_id)?;
        let basket = self
            .cart()
            .set_amount(&store_id, &arg.item_id, arg.quantity, arg.unit.as_deref())
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
        let store_id = self.resolve_store(arg.store_id)?;
        let basket = self
            .cart()
            .remove(&store_id, &arg.item_id)
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
        let store_id = self.resolve_store(store_id)?;
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
        let store_id = store_id
            .or_else(|| self.default_store.lock().unwrap().clone())
            .unwrap_or_else(|| crate::login::DEFAULT_PROBE_STORE.to_string());
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
             Every tool that operates on a store accepts a `store_id` (e.g. \"N137\"); a \
             cart belongs to a store. Use `search_stores` to find one. Call \
             `set_default_store` once to avoid repeating it on every subsequent call -- \
             after that, tools will use the default when store_id is omitted. Products are \
             added by EAN barcode, which `search_products` returns -- search in Finnish, \
             since the catalogue is Finnish. `update_cart_item` and `remove_from_cart` \
             instead take a basket `itemId`, which only exists once an item is in the cart \
             and is NOT the EAN -- get it from `get_cart`.\n\n\
             If `auth_status` says the session is not signed in, the cart reachable is an \
             anonymous one rather than the user's. Call `start_login` and relay its \
             instructions verbatim, then poll `login_status`. Credentials are never \
             automated and this server never sees them.\n\n\
             Checkout is deliberately not supported: nothing here can spend money.",
            )
    }
}
