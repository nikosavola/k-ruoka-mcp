//! A fake K-Ruoka, and the plumbing to run the real MCP server against it.
//!
//! The point of the [`KrApi`] seam is that everything above it -- event
//! construction, validation, the phantom-add rollback, error mapping onto MCP --
//! is ordinary logic with no browser in it. `MockApi` stands in for the browser so
//! those paths can be tested in milliseconds, deterministically, and without
//! touching anyone's cart.
//!
//! It also reaches places the live suite cannot: a signed-in account, an expired
//! session, a Cloudflare block. Those need a real login or a real block to observe
//! for real, so a fake is the only way to test the code that handles them.
//!
//! `MockApi` models K-Ruoka's *observed* behaviour rather than sane behaviour. In
//! particular it accepts any EAN and inserts an "Unknown product"
//! placeholder, and it ignores `SET-ITEM-AMOUNT` for an item that is not present.
//! A mock that behaved sensibly would let the guards against those pass vacuously.

#![allow(dead_code)] // Each test binary uses a different subset of this.

use std::sync::{Arc, Mutex};

use k_ruoka_mcp::browser::{ApiError, KrApi};
use serde_json::{Value, json};

pub const STORE: &str = "N137";
pub const STORE_NAME: &str = "K-Citymarket Helsinki Ruoholahti";
pub const BASKET_ID: &str = "c0fa67a6-4b56-4dc6-9a7e-506c2b29b7cf";

/// A real EAN, and the shape K-Ruoka returns for it.
pub const BANANA: &str = "2000818700008";
/// Sold by weight: `amountInfo.unit` is `kg`, not `kpl`. K-Ruoka does have such
/// products even though none turned up in the live probing, and the unit-inheritance
/// bug in `update_cart_item` was invisible without one.
pub const LOOSE_MINCE: &str = "2000111100001";
/// An EAN K-Ruoka has no record of.
pub const PHANTOM: &str = "0000000000000";

/// One recorded request.
#[derive(Debug, Clone)]
pub struct Call {
    pub method: String,
    pub path: String,
    pub body: Option<Value>,
}

impl Call {
    /// The `type` values of the basket events this call carried, if any.
    pub fn event_types(&self) -> Vec<String> {
        self.body
            .as_ref()
            .and_then(|b| b.as_array())
            .map(|events| {
                events
                    .iter()
                    .filter_map(|e| e["type"].as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// What the fake should do next.
#[derive(Debug, Clone, Default)]
pub enum Behaviour {
    #[default]
    Normal,
    /// Every call fails this way. For the branches a live anonymous session cannot
    /// reach.
    Fail(&'static str),
    /// Fail the first `n` calls, then behave normally. For asserting that a retry
    /// happens, and happens only once.
    FailFirst { n: usize, kind: &'static str },
}

#[derive(Debug, Default)]
struct State {
    /// `(item_id, ean, amount, unit)`.
    items: Vec<(String, String, f64, String)>,
    calls: Vec<Call>,
    behaviour: Behaviour,
    signed_in: Option<(&'static str, &'static str, &'static str)>,
    /// Event types to record, answer 200 to, and then ignore. K-Ruoka's signature
    /// move: success with nothing changed.
    deaf_to: Vec<&'static str>,
}

#[derive(Clone, Default)]
pub struct MockApi {
    state: Arc<Mutex<State>>,
}

impl MockApi {
    pub fn new() -> Self {
        Self::default()
    }

    /// Report a signed-in account, the way a real logged-in session would.
    pub fn signed_in_as(
        self,
        first: &'static str,
        last: &'static str,
        email: &'static str,
    ) -> Self {
        self.state.lock().unwrap().signed_in = Some((first, last, email));
        self
    }

    pub fn failing_with(self, kind: &'static str) -> Self {
        self.state.lock().unwrap().behaviour = Behaviour::Fail(kind);
        self
    }

    pub fn failing_first(self, n: usize, kind: &'static str) -> Self {
        self.state.lock().unwrap().behaviour = Behaviour::FailFirst { n, kind };
        self
    }

    /// Accept the given event type, answer 200, and change nothing.
    ///
    /// This is not a hypothetical: K-Ruoka does exactly this for `REMOVE-ITEM` and
    /// `SET-ITEM-AMOUNT` with an item id it does not hold, and "it returned 200"
    /// proving nothing is the recurring failure mode this whole codebase guards
    /// against. Without a fake that can be deaf, a guard that checks
    /// the *result* is indistinguishable from one that checks the status.
    pub fn deaf_to(self, event_type: &'static str) -> Self {
        self.state.lock().unwrap().deaf_to.push(event_type);
        self
    }

    /// Seed an item, bypassing the API, so tests can start from a populated cart.
    pub fn with_item(self, ean: &str, amount: f64, unit: &str) -> Self {
        self.state.lock().unwrap().items.push((
            ean.to_string(),
            ean.to_string(),
            amount,
            unit.to_string(),
        ));
        self
    }

    pub fn calls(&self) -> Vec<Call> {
        self.state.lock().unwrap().calls.clone()
    }

    /// Requests that carried basket events, i.e. the mutations.
    pub fn mutations(&self) -> Vec<Call> {
        self.calls()
            .into_iter()
            .filter(|c| c.method == "PATCH")
            .collect()
    }

    /// Flattened event types across every mutation, in order.
    pub fn events(&self) -> Vec<String> {
        self.mutations()
            .iter()
            .flat_map(|c| c.event_types())
            .collect()
    }

    pub fn items(&self) -> Vec<(String, f64, String)> {
        self.state
            .lock()
            .unwrap()
            .items
            .iter()
            .map(|(id, _, amount, unit)| (id.clone(), *amount, unit.clone()))
            .collect()
    }
}

fn basket_json(state: &State) -> Value {
    let items: Vec<Value> = state
        .items
        .iter()
        .map(|(id, ean, amount, unit)| {
            // The placeholder K-Ruoka inserts for an EAN it does not know: no
            // `productDetails.attributes`, no pricing.
            if ean == PHANTOM {
                json!({
                    "id": id, "ean": ean,
                    "name": {"finnish": "Tuntematon tuote", "english": "Unknown product"},
                    "amountInfo": {"amount": amount, "unit": unit},
                    "pricing": null,
                    "productDetails": {"availability": {}},
                })
            } else {
                json!({
                    "id": id, "ean": ean,
                    "name": {"finnish": "Pirkka banaani", "english": "Pirkka banana"},
                    "amountInfo": {"amount": amount, "unit": unit},
                    // Real weight-priced items carry unit "kg" and isApproximate true;
                    // everything else is priced per piece and must NOT be approximate.
                    "pricing": {"price": 0.29, "unit": "kg", "isApproximate": unit == "kg"},
                    "allowSubstitutes": true,
                    "productDetails": {
                        "attributes": {"ean": ean}, "availability": {},
                        "category": {}, "soldBy": "piece",
                    },
                })
            }
        })
        .collect();

    let total = 0.29 * state.items.iter().map(|(_, _, a, _)| *a).sum::<f64>();
    let (first, last, email) = state.signed_in.unwrap_or(("", "", ""));
    json!({
        "schemaVersion": 5,
        "id": BASKET_ID,
        "name": "Ostoskori",
        "userInfo": {"firstName": first, "lastName": last, "email": email, "phoneNumber": ""},
        "items": items,
        "priceSummary": {
            "itemsSubTotal": total, "grandTotal": total,
            "plussaSavings": {"total": 0, "type": "POTENTIAL"},
        },
        "store": {"id": STORE, "name": STORE_NAME},
        "substitutionDefault": true,
    })
}

fn apply_events(state: &mut State, events: &Value) {
    for event in events.as_array().into_iter().flatten() {
        let event_type = event["type"].as_str().unwrap_or_default();
        if state.deaf_to.contains(&event_type) {
            continue;
        }
        match event_type {
            "ADD-ITEM" => {
                let item = &event["item"];
                let ean = item["ean"].as_str().unwrap_or_default().to_string();
                let amount = item["amountInfo"]["amount"].as_f64().unwrap_or(0.0);
                let unit = item["amountInfo"]["unit"]
                    .as_str()
                    .unwrap_or("kpl")
                    .to_string();
                // Observed: a non-positive amount adds nothing, and re-adding an EAN
                // already present replaces the amount rather than accumulating.
                if amount <= 0.0 {
                    continue;
                }
                // Also observed: K-Ruoka caps the amount at 999.
                let amount = amount.min(999.0);
                match state.items.iter_mut().find(|(_, e, _, _)| *e == ean) {
                    Some(existing) => {
                        existing.2 = amount;
                        existing.3 = unit;
                    }
                    // The item id happens to equal the EAN for ordinary products.
                    None => state.items.push((ean.clone(), ean, amount, unit)),
                }
            }
            "SET-ITEM-AMOUNT" => {
                let id = event["itemId"].as_str().unwrap_or_default();
                let amount = event["value"]["amount"].as_f64().unwrap_or(0.0);
                let unit = event["value"]["unit"].as_str().unwrap_or("kpl").to_string();
                // Observed: an unknown item id is a silent no-op, and 0 or negative
                // removes. Both are faithfully unhelpful here on purpose.
                if let Some(pos) = state.items.iter().position(|(i, _, _, _)| i == id) {
                    if amount <= 0.0 {
                        state.items.remove(pos);
                    } else {
                        state.items[pos].2 = amount;
                        state.items[pos].3 = unit;
                    }
                }
            }
            "REMOVE-ITEM" => {
                let id = event["itemId"].as_str().unwrap_or_default();
                state.items.retain(|(i, _, _, _)| i != id);
            }
            "CLEAR-ITEMS" => state.items.clear(),
            _ => {}
        }
    }
}

#[async_trait::async_trait]
impl KrApi for MockApi {
    async fn call(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
    ) -> Result<Value, ApiError> {
        let mut state = self.state.lock().unwrap();
        state.calls.push(Call {
            method: method.to_string(),
            path: path.to_string(),
            body: body.cloned(),
        });

        let failure = match state.behaviour {
            Behaviour::Normal => None,
            Behaviour::Fail(kind) => Some(kind),
            Behaviour::FailFirst { n, kind } => {
                if state.calls.len() <= n {
                    Some(kind)
                } else {
                    None
                }
            }
        };
        if let Some(kind) = failure {
            return Err(match kind {
                "auth" => ApiError::AuthExpired,
                "cloudflare" => ApiError::Cloudflare {
                    detail: "API response, status 403, cf-mitigated: challenge".into(),
                },
                "invalid-store" => ApiError::Api {
                    status: 422,
                    message:
                        r#"{"name":"InvalidStoreIdError","message":"Invalid store ID undefined"}"#
                            .into(),
                },
                other => ApiError::Api {
                    status: 500,
                    message: format!("mock failure: {other}"),
                },
            });
        }

        // Search: shaped like the live responses, including the awkward bits the view
        // types have to cope with. Price lives under `mobilescan`, not at the top level,
        // and the second hit deliberately has neither a price nor a brand -- a real
        // search returns such rows, and a fake that always supplies them would let a
        // panicking unwrap through.
        if path.starts_with("/kr-api/v2/product-search/") {
            return Ok(json!({
                "totalHits": 169,
                "result": [
                    {"product": {
                        "ean": BANANA,
                        "isAvailable": true,
                        "localizedName": {"finnish": "Pirkka banaani", "english": "Pirkka banana"},
                        "brand": {"name": "Pirkka"},
                        "mobilescan": {"pricing": {"normal": {
                            "price": 0.3, "unit": "kpl", "isApproximate": true,
                            "unitPrice": {"value": 1.69, "unit": "kg"},
                        }}},
                    }},
                    {"product": {
                        "ean": LOOSE_MINCE,
                        "isAvailable": false,
                        "localizedName": {"finnish": "Irtojauheliha"},
                    }},
                ],
            }));
        }
        if path == "/kr-api/stores/search" {
            return Ok(json!({
                "totalHits": 2,
                "results": [
                    {
                        "id": STORE, "name": STORE_NAME, "location": "Helsinki",
                        "chainName": "K-Citymarket", "isWebStore": true,
                        "hasPickup": true, "hasHomeDelivery": true,
                    },
                    // A real store with no online cart, which the tool has to surface
                    // rather than hide: its id is valid but useless to the other tools.
                    {
                        "id": "K815", "name": "K-Market Ruoholahti", "location": "Helsinki",
                        "chainName": "K-Market", "isWebStore": false,
                    },
                ],
            }));
        }

        if path.starts_with("/kr-api/basket/by-id/")
            && let Some(events) = body
        {
            apply_events(&mut state, events);
        }
        Ok(basket_json(&state))
    }
}

/// Run the real `CartServer` against `api`, over a real in-process MCP connection.
///
/// Not a shortcut past the protocol: this is `tokio::io::duplex`, so requests are
/// serialised to JSON-RPC, framed, parsed, routed by rmcp and deserialised into the
/// tool argument structs exactly as they are for a real client over stdio. Calling
/// the Rust methods directly would skip the schemas, the argument deserialisation
/// and the error mapping, which is most of what can break.
pub async fn connect(
    api: MockApi,
) -> anyhow::Result<(rmcp::service::RunningService<rmcp::RoleClient, ()>, MockApi)> {
    use rmcp::ServiceExt;

    let (server_io, client_io) = tokio::io::duplex(1 << 16);
    let handle = api.clone();
    let server = k_ruoka_mcp::mcp::CartServer::new(Arc::new(api));
    tokio::spawn(async move {
        if let Ok(running) = server.serve(server_io).await {
            let _ = running.waiting().await;
        }
    });
    let client = ().serve(client_io).await?;
    Ok((client, handle))
}

/// Like [`connect`], but with a persistence file for the default store at `store_path`.
pub async fn connect_with_store_path(
    api: MockApi,
    store_path: std::path::PathBuf,
) -> anyhow::Result<(rmcp::service::RunningService<rmcp::RoleClient, ()>, MockApi)> {
    use rmcp::ServiceExt;

    let (server_io, client_io) = tokio::io::duplex(1 << 16);
    let handle = api.clone();
    let server =
        k_ruoka_mcp::mcp::CartServer::new(Arc::new(api)).with_default_store_path(store_path);
    tokio::spawn(async move {
        if let Ok(running) = server.serve(server_io).await {
            let _ = running.waiting().await;
        }
    });
    let client = ().serve(client_io).await?;
    Ok((client, handle))
}

/// A scripted [`LoginFlow`], so the login tools can be tested without a browser.
///
/// The real one spawns `k-ruoka-mcp login` and waits for a human, which no test can do.
/// What is worth testing above that seam is the same thing as everywhere else here: that
/// the tool exists, routes, and hands the caller something usable.
#[derive(Clone, Default)]
pub struct MockLogin {
    state: Arc<Mutex<MockLoginState>>,
}

#[derive(Default)]
struct MockLoginState {
    calls: Vec<String>,
    /// What `status` should report next, so a test can walk through the flow.
    next_state: Option<(&'static str, Option<&'static str>)>,
}

impl MockLogin {
    pub fn new() -> Self {
        Self::default()
    }

    /// Make the next `login_status` report this state, and account if any.
    pub fn reporting(self, state: &'static str, account: Option<&'static str>) -> Self {
        self.state.lock().unwrap().next_state = Some((state, account));
        self
    }

    pub fn calls(&self) -> Vec<String> {
        self.state.lock().unwrap().calls.clone()
    }
}

#[async_trait::async_trait]
impl k_ruoka_mcp::login_flow::LoginFlow for MockLogin {
    async fn start(
        &self,
        debug_port: u16,
    ) -> Result<k_ruoka_mcp::login_flow::LoginProgress, ApiError> {
        self.state
            .lock()
            .unwrap()
            .calls
            .push(format!("start:{debug_port}"));
        Ok(serde_json::from_value(json!({
            "state": "waiting",
            "detail": "A browser is open.",
            "instructions": "1. Switch to it and pick the tab titled ...",
        }))
        .unwrap())
    }

    async fn status(&self) -> Result<k_ruoka_mcp::login_flow::LoginProgress, ApiError> {
        let mut state = self.state.lock().unwrap();
        state.calls.push("status".to_string());
        let (name, account) = state.next_state.unwrap_or(("waiting", None));
        Ok(serde_json::from_value(json!({
            "state": name,
            "detail": "scripted",
            "account": account,
        }))
        .unwrap())
    }

    async fn cancel(&self) -> Result<k_ruoka_mcp::login_flow::LoginProgress, ApiError> {
        self.state.lock().unwrap().calls.push("cancel".to_string());
        Ok(serde_json::from_value(json!({
            "state": "notStarted",
            "detail": "Login cancelled.",
        }))
        .unwrap())
    }
}

/// Like [`connect`], with the login tools wired to a scripted flow.
pub async fn connect_with_login(
    api: MockApi,
    login: MockLogin,
) -> anyhow::Result<(
    rmcp::service::RunningService<rmcp::RoleClient, ()>,
    MockApi,
    MockLogin,
)> {
    use rmcp::ServiceExt;

    let (server_io, client_io) = tokio::io::duplex(1 << 16);
    let api_handle = api.clone();
    let login_handle = login.clone();
    let server = k_ruoka_mcp::mcp::CartServer::with_login(Arc::new(api), Arc::new(login));
    tokio::spawn(async move {
        if let Ok(running) = server.serve(server_io).await {
            let _ = running.waiting().await;
        }
    });
    let client = ().serve(client_io).await?;
    Ok((client, api_handle, login_handle))
}

/// How the server reported a failure. MCP has two channels and the difference
/// decides whether the model ever sees the text (see `ToolFailure`).
#[derive(Debug)]
pub enum Failure {
    /// A JSON-RPC error. The client may treat this as a transport failure.
    Protocol(String),
    /// A normal result carrying `isError: true`. The model sees the text.
    ToolError(String),
}

impl Failure {
    pub fn text(&self) -> &str {
        match self {
            Failure::Protocol(t) | Failure::ToolError(t) => t,
        }
    }
}

/// Like [`call_tool`], but keeps the two failure channels distinguishable.
pub async fn try_call_tool(
    client: &rmcp::service::RunningService<rmcp::RoleClient, ()>,
    name: &'static str,
    args: Value,
) -> Result<Value, Failure> {
    use rmcp::model::CallToolRequestParams;

    let mut params = CallToolRequestParams::new(name);
    if let Some(obj) = args.as_object() {
        params = params.with_arguments(obj.clone());
    }
    let result = match client.call_tool(params).await {
        Ok(r) => r,
        Err(e) => return Err(Failure::Protocol(e.to_string())),
    };
    let text = result
        .content
        .iter()
        .filter_map(|c| c.as_text().map(|t| t.text.clone()))
        .collect::<Vec<_>>()
        .join("\n");
    if result.is_error.unwrap_or(false) {
        return Err(Failure::ToolError(text));
    }
    Ok(result.structured_content.unwrap_or(Value::String(text)))
}

/// Call a tool and return its structured result, or the error message.
pub async fn call_tool(
    client: &rmcp::service::RunningService<rmcp::RoleClient, ()>,
    name: &'static str,
    args: Value,
) -> Result<Value, String> {
    try_call_tool(client, name, args)
        .await
        .map_err(|f| f.text().to_string())
}
