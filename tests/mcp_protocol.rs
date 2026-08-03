//! MCP protocol integration tests, against a fake K-Ruoka.
//!
//! These run the real `CartServer` over a real in-process MCP connection (see
//! `support::connect`), so the schemas, argument deserialisation, routing and error
//! mapping are all exercised. What is faked is only the layer below the cart logic:
//! the browser and the live site.
//!
//! That trade buys three things the live suite (`tests/live_e2e.rs`) cannot have:
//! they run in milliseconds with no network or Chrome, they are deterministic, and
//! they can reach states an anonymous session never reaches -- a signed-in account,
//! an expired session, a Cloudflare block.
//!
//! Where a test asserts on `api.events()` or `api.mutations()`, it is checking the
//! request actually sent to K-Ruoka. That matters more than the returned cart: the
//! unit-inheritance bug in `update_cart_item` produced a perfectly plausible cart
//! and a wrong request.

mod support;

use serde_json::json;
use support::{
    BANANA, Failure, LOOSE_MINCE, MockApi, MockLogin, PHANTOM, STORE, call_tool, connect,
    connect_with_login, connect_with_store_path, try_call_tool,
};

// ---------------------------------------------------------------------------
// Protocol surface
// ---------------------------------------------------------------------------

#[tokio::test]
async fn advertises_itself_and_its_tools() -> anyhow::Result<()> {
    let (client, _) = connect(MockApi::new()).await?;

    let info = client.peer_info().expect("server info after initialize");
    // rmcp defaults this to its own crate name; clients display it.
    assert_eq!(info.server_info.as_ref().unwrap().name, "k-ruoka-mcp");
    assert!(
        info.capabilities.tools.is_some(),
        "tools capability missing"
    );
    assert!(
        info.instructions
            .as_deref()
            .is_some_and(|i| i.contains("itemId")),
        "instructions should warn that itemId is not an EAN"
    );

    let names: Vec<_> = client
        .list_all_tools()
        .await?
        .into_iter()
        .map(|t| t.name.to_string())
        .collect();
    let mut sorted = names.clone();
    sorted.sort();
    assert_eq!(
        sorted,
        [
            "add_to_cart",
            "auth_status",
            "cancel_login",
            "clear_cart",
            "get_cart",
            "login_status",
            "remove_from_cart",
            "search_products",
            "search_stores",
            "set_default_store",
            "start_login",
            "update_cart_item"
        ]
    );
    Ok(())
}

/// The generated schemas are the contract a calling model reads. Pin the parts that
/// would silently change behaviour: which arguments are required, and which are not.
#[tokio::test]
async fn tool_schemas_mark_the_right_arguments_required() -> anyhow::Result<()> {
    let (client, _) = connect(MockApi::new()).await?;
    let tools = client.list_all_tools().await?;
    let schema_of = |name: &str| {
        tools
            .iter()
            .find(|t| t.name == name)
            .map(|t| serde_json::to_value(&t.input_schema).unwrap())
            .unwrap_or_else(|| panic!("no tool {name}"))
    };

    let required = |v: &serde_json::Value| -> Vec<String> {
        let mut r: Vec<String> = v["required"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|s| s.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        r.sort();
        r
    };

    assert_eq!(required(&schema_of("get_cart")), [] as [&str; 0]);
    // quantity/unit/local_store_id/allow_substitutes all default; store_id falls back to default.
    assert_eq!(required(&schema_of("add_to_cart")), ["ean"]);
    // quantity is NOT optional here: there is no sensible default for "set to".
    assert_eq!(
        required(&schema_of("update_cart_item")),
        ["item_id", "quantity"]
    );
    assert_eq!(required(&schema_of("remove_from_cart")), ["item_id"]);
    // set_default_store always requires a store_id.
    assert_eq!(required(&schema_of("set_default_store")), ["store_id"]);

    // The itemId-is-not-an-EAN warning has to be on the tools that take one, since
    // that is the mistake a caller is most likely to make.
    for tool in ["update_cart_item", "remove_from_cart"] {
        let schema = schema_of(tool);
        let desc = schema["properties"]["item_id"]["description"]
            .as_str()
            .unwrap_or_default();
        assert!(desc.contains("NOT the EAN"), "{tool}: {desc}");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Login, driven through MCP
// ---------------------------------------------------------------------------

/// The instructions are the payload: they differ between a desktop and a headless host,
/// so the caller has to relay what the server produced rather than inventing steps.
#[tokio::test]
async fn start_login_returns_instructions_to_relay() -> anyhow::Result<()> {
    let (client, _api, login) = connect_with_login(MockApi::new(), MockLogin::new()).await?;

    let progress = call_tool(&client, "start_login", json!({}))
        .await
        .unwrap_or_else(|e| panic!("start_login failed: {e}"));

    assert_eq!(progress["state"], "waiting");
    assert!(
        progress["instructions"]
            .as_str()
            .is_some_and(|i| i.contains("pick the tab")),
        "{progress}"
    );
    // The subcommand's own default, so its printed steps and the tool agree.
    assert_eq!(login.calls(), ["start:9222"]);
    Ok(())
}

#[tokio::test]
async fn start_login_passes_an_explicit_port_through() -> anyhow::Result<()> {
    let (client, _api, login) = connect_with_login(MockApi::new(), MockLogin::new()).await?;
    call_tool(&client, "start_login", json!({"port": 9333}))
        .await
        .unwrap();
    assert_eq!(login.calls(), ["start:9333"]);
    Ok(())
}

#[tokio::test]
async fn login_status_reports_the_account_once_signed_in() -> anyhow::Result<()> {
    let (client, _api, _login) = connect_with_login(
        MockApi::new(),
        MockLogin::new().reporting("signedIn", Some("Niko Savola <a@b.c>")),
    )
    .await?;

    let progress = call_tool(&client, "login_status", json!({})).await.unwrap();
    assert_eq!(progress["state"], "signedIn");
    assert_eq!(progress["account"], "Niko Savola <a@b.c>");
    Ok(())
}

#[tokio::test]
async fn cancel_login_is_routed() -> anyhow::Result<()> {
    let (client, _api, login) = connect_with_login(MockApi::new(), MockLogin::new()).await?;
    let progress = call_tool(&client, "cancel_login", json!({})).await.unwrap();
    assert_eq!(progress["state"], "notStarted");
    assert_eq!(login.calls(), ["cancel"]);
    Ok(())
}

/// An embedding with no way to drive a browser still exposes the tools, and says why it
/// cannot help rather than failing opaquely -- a missing tool is harder to explain.
#[tokio::test]
async fn the_login_tools_explain_themselves_when_unavailable() -> anyhow::Result<()> {
    let (client, _) = connect(MockApi::new()).await?;
    for tool in ["start_login", "login_status", "cancel_login"] {
        let err = call_tool(&client, tool, json!({}))
            .await
            .expect_err("should refuse without a login flow");
        assert!(err.contains("k-ruoka-mcp login"), "{tool}: {err}");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Search
// ---------------------------------------------------------------------------

/// The whole point of this tool is handing an `ean` to `add_to_cart`, so that field
/// existing and being the real barcode is the assertion that matters.
#[tokio::test]
async fn search_products_returns_eans_and_prices() -> anyhow::Result<()> {
    let (client, api) = connect(MockApi::new()).await?;

    let found = call_tool(
        &client,
        "search_products",
        json!({"store_id": STORE, "query": "banaani"}),
    )
    .await
    .unwrap_or_else(|e| panic!("search_products failed: {e}"));

    assert_eq!(found["totalHits"], 169);
    let first = &found["results"][0];
    assert_eq!(first["ean"], BANANA);
    assert_eq!(first["name"], "Pirkka banaani");
    assert_eq!(first["brand"], "Pirkka");
    assert_eq!(first["price"], 0.3);
    assert_eq!(first["priceUnit"], "kpl");
    // Price lives under `mobilescan.pricing.normal`, and the comparison price is what
    // makes a weight-priced item comparable at all.
    assert_eq!(first["comparisonPrice"], "1.69 EUR/kg");
    assert_eq!(first["priceIsApproximate"], true);
    assert_eq!(first["isAvailable"], true);

    // A row with no price and no brand must still come back, with `isAvailable` telling
    // the caller the EAN is real but not buyable here.
    let second = &found["results"][1];
    assert_eq!(second["ean"], LOOSE_MINCE);
    assert_eq!(second["isAvailable"], false);
    assert!(second.get("price").is_none(), "{second}");
    assert!(second.get("brand").is_none(), "{second}");

    // Read-only: searching must not touch the cart.
    assert!(api.mutations().is_empty(), "{:?}", api.mutations());
    Ok(())
}

/// The search term is interpolated into a URL path, so it has to be escaped. Without
/// that, `&limit=` in a query would silently override the real paging parameter.
#[tokio::test]
async fn a_search_term_cannot_rewrite_the_request_url() -> anyhow::Result<()> {
    let (client, api) = connect(MockApi::new()).await?;
    call_tool(
        &client,
        "search_products",
        json!({"store_id": STORE, "query": "maito & keksit?/x"}),
    )
    .await
    .unwrap();

    let path = &api.calls()[0].path;
    assert!(path.contains("maito%20%26%20keksit%3F%2Fx"), "{path}");
    // Exactly one `limit=`, i.e. ours.
    assert_eq!(path.matches("limit=").count(), 1, "{path}");
    Ok(())
}

#[tokio::test]
async fn an_empty_search_query_is_refused_before_any_request() -> anyhow::Result<()> {
    for tool in ["search_products", "search_stores"] {
        let (client, api) = connect(MockApi::new()).await?;
        let err = call_tool(&client, tool, json!({"store_id": STORE, "query": "   "}))
            .await
            .expect_err("a blank query should be rejected");
        assert!(err.contains("must not be empty"), "{tool}: {err}");
        assert!(api.calls().is_empty(), "{tool}: {:?}", api.calls());
    }
    Ok(())
}

/// `store_id` is what every other tool needs, and a store with no online cart is
/// useless to them -- so both have to be visible in the result.
#[tokio::test]
async fn search_stores_returns_ids_and_flags_non_web_stores() -> anyhow::Result<()> {
    let (client, api) = connect(MockApi::new()).await?;

    let found = call_tool(&client, "search_stores", json!({"query": "Ruoholahti"}))
        .await
        .unwrap_or_else(|e| panic!("search_stores failed: {e}"));

    let first = &found["results"][0];
    assert_eq!(first["storeId"], STORE);
    assert_eq!(first["chain"], "K-Citymarket");
    assert_eq!(first["isWebStore"], true);
    assert_eq!(first["hasPickup"], true);

    let second = &found["results"][1];
    assert_eq!(second["storeId"], "K815");
    assert_eq!(second["isWebStore"], false);

    // The term goes in a JSON body here, not the path -- unlike product search.
    assert_eq!(api.calls()[0].path, "/kr-api/stores/search");
    assert_eq!(api.calls()[0].body.as_ref().unwrap()["query"], "Ruoholahti");
    assert!(api.mutations().is_empty(), "{:?}", api.mutations());
    Ok(())
}

/// A model asking for hundreds of results would swamp its own context, and the caller
/// cannot see the cap from the outside, so it is enforced rather than documented.
#[tokio::test]
async fn the_result_limit_is_clamped() -> anyhow::Result<()> {
    let (client, api) = connect(MockApi::new()).await?;
    call_tool(
        &client,
        "search_products",
        json!({"store_id": STORE, "query": "maito", "limit": 9999}),
    )
    .await
    .unwrap();
    assert!(
        api.calls()[0].path.contains("limit=50"),
        "{:?}",
        api.calls()
    );

    let (client, api) = connect(MockApi::new()).await?;
    call_tool(
        &client,
        "search_stores",
        json!({"query": "x", "limit": 9999}),
    )
    .await
    .unwrap();
    assert_eq!(api.calls()[0].body.as_ref().unwrap()["limit"], 50);
    Ok(())
}

// ---------------------------------------------------------------------------
// Reads
// ---------------------------------------------------------------------------

#[tokio::test]
async fn get_cart_summarises_the_basket() -> anyhow::Result<()> {
    let (client, api) = connect(MockApi::new().with_item(BANANA, 2.0, "kpl")).await?;

    let cart = call_tool(&client, "get_cart", json!({"store_id": STORE}))
        .await
        .unwrap_or_else(|e| panic!("get_cart failed: {e}"));
    assert_eq!(cart["store"]["id"], STORE);
    assert_eq!(cart["items"].as_array().unwrap().len(), 1);
    assert_eq!(cart["items"][0]["itemId"], BANANA);
    assert_eq!(cart["items"][0]["amount"], 2.0);
    assert_eq!(cart["items"][0]["name"], "Pirkka banaani");
    // The multi-KB productDetails blob must not be forwarded to the caller.
    assert!(cart["items"][0].get("productDetails").is_none());

    // Read-only: no mutation may be sent.
    assert!(api.mutations().is_empty(), "{:?}", api.mutations());
    Ok(())
}

/// A per-piece item must still carry `priceIsApproximate: false` rather than omitting
/// the field: the field is declared `required` in the output schema, so a client that
/// validates structured output against it rejects the whole cart if the field is
/// missing on any item.
#[tokio::test]
async fn get_cart_reports_price_is_approximate_false_for_a_per_piece_item() -> anyhow::Result<()> {
    let (client, _api) = connect(MockApi::new().with_item(BANANA, 2.0, "kpl")).await?;

    let cart = call_tool(&client, "get_cart", json!({"store_id": STORE}))
        .await
        .unwrap_or_else(|e| panic!("get_cart failed: {e}"));
    assert_eq!(
        cart["items"][0].get("priceIsApproximate"),
        Some(&json!(false)),
        "priceIsApproximate must be present and false, not omitted: {}",
        cart["items"][0]
    );
    Ok(())
}

/// An anonymous session gets a valid basket rather than a 401, so "the call worked"
/// is not evidence of being signed in.
#[tokio::test]
async fn anonymous_session_reports_no_account() -> anyhow::Result<()> {
    let (client, _) = connect(MockApi::new()).await?;

    let cart = call_tool(&client, "get_cart", json!({"store_id": STORE}))
        .await
        .unwrap();
    assert_eq!(cart["account"], serde_json::Value::Null);

    let status = call_tool(&client, "auth_status", json!({"store_id": STORE}))
        .await
        .unwrap();
    assert_eq!(status["loggedIn"], false);
    assert!(
        status["detail"].as_str().unwrap().contains("login"),
        "should tell the caller what to do: {}",
        status["detail"]
    );
    Ok(())
}

/// The signed-in branch. Unreachable in the live suite, which has no login.
#[tokio::test]
async fn signed_in_session_reports_the_account() -> anyhow::Result<()> {
    let (client, _) =
        connect(MockApi::new().signed_in_as("Niko", "Savola", "niko@example.com")).await?;

    let status = call_tool(&client, "auth_status", json!({"store_id": STORE}))
        .await
        .unwrap();
    assert_eq!(status["loggedIn"], true);
    assert_eq!(status["account"], "Niko Savola <niko@example.com>");

    let cart = call_tool(&client, "get_cart", json!({"store_id": STORE}))
        .await
        .unwrap();
    assert_eq!(cart["account"], "Niko Savola <niko@example.com>");
    Ok(())
}

// ---------------------------------------------------------------------------
// Mutations -- asserted on the request sent, not just the cart returned
// ---------------------------------------------------------------------------

#[tokio::test]
async fn add_to_cart_sends_one_add_item_event() -> anyhow::Result<()> {
    let (client, api) = connect(MockApi::new()).await?;

    let cart = call_tool(
        &client,
        "add_to_cart",
        json!({"store_id": STORE, "ean": BANANA, "quantity": 3}),
    )
    .await
    .unwrap();
    assert_eq!(cart["items"][0]["amount"], 3.0);

    let mutations = api.mutations();
    assert_eq!(mutations.len(), 1, "{mutations:?}");
    let events = mutations[0].body.clone().unwrap();
    // Always an array, even for a single change.
    assert!(events.is_array(), "{events}");
    assert_eq!(
        events[0],
        json!({
            "type": "ADD-ITEM",
            "item": {
                "ean": BANANA,
                "allowSubstitutes": true,
                "amountInfo": {"amount": 3.0, "unit": "kpl"},
            }
        }),
        "event shape must match K-Ruoka's bundle"
    );
    Ok(())
}

#[tokio::test]
async fn local_store_id_is_omitted_unless_given() -> anyhow::Result<()> {
    let (client, api) = connect(MockApi::new()).await?;
    call_tool(
        &client,
        "add_to_cart",
        json!({"store_id": STORE, "ean": BANANA}),
    )
    .await
    .unwrap();
    let item = api.mutations()[0].body.clone().unwrap()[0]["item"].clone();
    assert!(
        item.as_object().unwrap().get("localStoreId").is_none(),
        "must be absent, not null: {item}"
    );

    let (client, api) = connect(MockApi::new()).await?;
    call_tool(
        &client,
        "add_to_cart",
        json!({"store_id": STORE, "ean": BANANA, "local_store_id": "N137"}),
    )
    .await
    .unwrap();
    assert_eq!(
        api.mutations()[0].body.clone().unwrap()[0]["item"]["localStoreId"],
        "N137"
    );
    Ok(())
}

/// The regression test for the bug this seam was worth adding for.
///
/// `update_cart_item` used to default `unit` to "kpl" while setting
/// `{amount, unit}` on an item that already had one, silently converting a
/// weight-sold item to pieces. The returned cart looked entirely plausible; only
/// the outgoing request was wrong, which is why this asserts on the request.
#[tokio::test]
async fn update_cart_item_keeps_the_items_existing_unit() -> anyhow::Result<()> {
    let (client, api) = connect(MockApi::new().with_item(LOOSE_MINCE, 1.5, "kg")).await?;

    let cart = call_tool(
        &client,
        "update_cart_item",
        json!({"store_id": STORE, "item_id": LOOSE_MINCE, "quantity": 2}),
    )
    .await
    .unwrap();

    let event = api.mutations()[0].body.clone().unwrap()[0].clone();
    assert_eq!(
        event,
        json!({
            "type": "SET-ITEM-AMOUNT",
            "itemId": LOOSE_MINCE,
            "value": {"amount": 2.0, "unit": "kg"},
        }),
        "unit must be inherited from the item, not defaulted to kpl"
    );
    assert_eq!(cart["items"][0]["unit"], "kg");
    Ok(())
}

#[tokio::test]
async fn update_cart_item_honours_an_explicit_unit() -> anyhow::Result<()> {
    let (client, api) = connect(MockApi::new().with_item(LOOSE_MINCE, 1.5, "kg")).await?;
    call_tool(
        &client,
        "update_cart_item",
        json!({"store_id": STORE, "item_id": LOOSE_MINCE, "quantity": 4, "unit": "kpl"}),
    )
    .await
    .unwrap();
    assert_eq!(
        api.mutations()[0].body.clone().unwrap()[0]["value"]["unit"],
        "kpl"
    );
    Ok(())
}

#[tokio::test]
async fn quantity_zero_removes_and_clear_empties() -> anyhow::Result<()> {
    let (client, api) = connect(MockApi::new().with_item(BANANA, 2.0, "kpl")).await?;

    let cart = call_tool(
        &client,
        "update_cart_item",
        json!({"store_id": STORE, "item_id": BANANA, "quantity": 0}),
    )
    .await
    .unwrap();
    assert!(cart["items"].as_array().unwrap().is_empty());

    call_tool(
        &client,
        "add_to_cart",
        json!({"store_id": STORE, "ean": BANANA}),
    )
    .await
    .unwrap();
    let cart = call_tool(&client, "clear_cart", json!({"store_id": STORE}))
        .await
        .unwrap();
    assert!(cart["items"].as_array().unwrap().is_empty());
    assert_eq!(cart["totals"]["grandTotal"], 0.0);
    assert!(
        api.events().contains(&"CLEAR-ITEMS".to_string()),
        "{:?}",
        api.events()
    );
    Ok(())
}

#[tokio::test]
async fn remove_from_cart_sends_remove_item() -> anyhow::Result<()> {
    let (client, api) = connect(MockApi::new().with_item(BANANA, 1.0, "kpl")).await?;
    let cart = call_tool(
        &client,
        "remove_from_cart",
        json!({"store_id": STORE, "item_id": BANANA}),
    )
    .await
    .unwrap();
    assert!(cart["items"].as_array().unwrap().is_empty());
    assert_eq!(api.events(), ["REMOVE-ITEM"]);
    Ok(())
}

// ---------------------------------------------------------------------------
// Guards against K-Ruoka's unhelpful 200s
// ---------------------------------------------------------------------------

/// A non-positive quantity must be refused *before* a request goes out. K-Ruoka
/// would accept it, add nothing, and return 200 -- a success that achieved nothing.
#[tokio::test]
async fn non_positive_add_quantity_is_refused_without_a_request() -> anyhow::Result<()> {
    for quantity in [0.0, -5.0] {
        let (client, api) = connect(MockApi::new()).await?;
        let err = call_tool(
            &client,
            "add_to_cart",
            json!({"store_id": STORE, "ean": BANANA, "quantity": quantity}),
        )
        .await
        .expect_err("should be rejected");
        assert!(err.contains("greater than 0"), "{err}");
        assert!(
            api.calls().is_empty(),
            "nothing should have been sent: {:?}",
            api.calls()
        );
    }
    Ok(())
}

#[tokio::test]
async fn negative_update_quantity_is_refused() -> anyhow::Result<()> {
    let (client, api) = connect(MockApi::new().with_item(BANANA, 2.0, "kpl")).await?;
    let err = call_tool(
        &client,
        "update_cart_item",
        json!({"store_id": STORE, "item_id": BANANA, "quantity": -3}),
    )
    .await
    .expect_err("should be rejected");
    assert!(err.contains("cannot be negative"), "{err}");
    assert!(
        err.contains("Use 0 to remove"),
        "should say what to do: {err}"
    );
    assert!(api.mutations().is_empty(), "{:?}", api.mutations());
    // The item survives.
    assert_eq!(api.items(), [(BANANA.to_string(), 2.0, "kpl".to_string())]);
    Ok(())
}

/// An unknown EAN is inserted by K-Ruoka as a placeholder. It must be rolled back
/// and reported, not left in the cart under a success.
#[tokio::test]
async fn unknown_ean_is_rejected_and_rolled_back() -> anyhow::Result<()> {
    let (client, api) = connect(MockApi::new()).await?;

    let err = call_tool(
        &client,
        "add_to_cart",
        json!({"store_id": STORE, "ean": PHANTOM}),
    )
    .await
    .expect_err("an unknown EAN should not report success");
    assert!(err.contains("no product with EAN"), "{err}");
    assert!(err.contains("nothing was added"), "{err}");

    // The rollback is the point: added, then removed again.
    assert_eq!(api.events(), ["ADD-ITEM", "REMOVE-ITEM"]);
    assert!(api.items().is_empty(), "cart left dirty: {:?}", api.items());
    Ok(())
}

/// The rollback must be verified by *result*, not by status.
///
/// `REMOVE-ITEM` is one of the calls K-Ruoka answers 200 to while changing nothing,
/// and a phantom placeholder is exactly the off-the-tested-path case where that is
/// most plausible. Checking only `is_ok()` reported "nothing was added" while leaving
/// "Tuntematon tuote" in a real user's cart -- the precise outcome the phantom check
/// exists to prevent.
#[tokio::test]
async fn a_rollback_that_silently_did_nothing_is_reported_not_claimed() -> anyhow::Result<()> {
    let (client, api) = connect(MockApi::new().deaf_to("REMOVE-ITEM")).await?;

    let err = call_tool(
        &client,
        "add_to_cart",
        json!({"store_id": STORE, "ean": PHANTOM}),
    )
    .await
    .expect_err("an unknown EAN should not report success");

    assert!(err.contains("no product with EAN"), "{err}");
    // The distinguishing assertion: it must NOT claim the cart is clean.
    assert!(
        !err.contains("nothing was added"),
        "claimed a clean rollback that did not happen: {err}"
    );
    assert!(
        err.contains("could not be removed"),
        "must say the rollback failed: {err}"
    );
    assert!(
        err.contains(PHANTOM),
        "must name the item id to clean up by hand: {err}"
    );
    // And the phantom really is still there, which is why the message matters.
    assert_eq!(api.events(), ["ADD-ITEM", "REMOVE-ITEM"]);
    assert_eq!(api.items().len(), 1, "{:?}", api.items());
    Ok(())
}

/// `find_item` validates against an earlier read, and tool calls run concurrently, so
/// a 200-that-changed-nothing is still reachable by interleaving even when the id was
/// valid at check time. The response carries the basket, so verify against it.
#[tokio::test]
async fn a_set_amount_that_silently_did_nothing_is_not_a_success() -> anyhow::Result<()> {
    let (client, api) = connect(
        MockApi::new()
            .with_item(BANANA, 2.0, "kpl")
            .deaf_to("SET-ITEM-AMOUNT"),
    )
    .await?;

    let err = call_tool(
        &client,
        "update_cart_item",
        json!({"store_id": STORE, "item_id": BANANA, "quantity": 5}),
    )
    .await
    .expect_err("a no-op must not report success");
    assert!(err.contains(BANANA), "must name the item: {err}");
    assert!(err.contains('5'), "must name what was asked for: {err}");
    assert_eq!(api.events(), ["SET-ITEM-AMOUNT"]);
    Ok(())
}

#[tokio::test]
async fn a_remove_that_silently_did_nothing_is_not_a_success() -> anyhow::Result<()> {
    let (client, api) = connect(
        MockApi::new()
            .with_item(BANANA, 1.0, "kpl")
            .deaf_to("REMOVE-ITEM"),
    )
    .await?;

    let err = call_tool(
        &client,
        "remove_from_cart",
        json!({"store_id": STORE, "item_id": BANANA}),
    )
    .await
    .expect_err("a no-op must not report success");
    assert!(err.contains(BANANA), "must name the item: {err}");
    assert!(err.contains("still in the cart"), "{err}");
    assert_eq!(api.events(), ["REMOVE-ITEM"]);
    Ok(())
}

/// An empty EAN would match any returned item carrying no `ean` of its own, so the
/// phantom check would pass on someone else's line and report a false success.
#[tokio::test]
async fn an_empty_ean_is_refused_before_any_request() -> anyhow::Result<()> {
    let (client, api) = connect(MockApi::new()).await?;
    let err = call_tool(
        &client,
        "add_to_cart",
        json!({"store_id": STORE, "ean": ""}),
    )
    .await
    .expect_err("an empty EAN should be rejected");
    assert!(err.contains("must not be empty"), "{err}");
    assert!(api.calls().is_empty(), "{:?}", api.calls());
    Ok(())
}

/// An item id that is not in the cart. K-Ruoka answers 200 with the cart unchanged,
/// so without a guard this reads as success.
#[tokio::test]
async fn unknown_item_id_fails_loudly_and_lists_the_valid_ones() -> anyhow::Result<()> {
    let (client, api) = connect(MockApi::new().with_item(BANANA, 1.0, "kpl")).await?;

    let err = call_tool(
        &client,
        "remove_from_cart",
        json!({"store_id": STORE, "item_id": "not-a-real-item"}),
    )
    .await
    .expect_err("should be rejected");
    assert!(err.contains("not-a-real-item"), "{err}");
    assert!(err.contains(BANANA), "should list the valid ids: {err}");
    assert!(api.mutations().is_empty(), "{:?}", api.mutations());

    let (client, _) = connect(MockApi::new()).await?;
    let err = call_tool(
        &client,
        "remove_from_cart",
        json!({"store_id": STORE, "item_id": BANANA}),
    )
    .await
    .expect_err("should be rejected");
    assert!(err.contains("cart is empty"), "{err}");
    Ok(())
}

// ---------------------------------------------------------------------------
// Error mapping -- states the live suite cannot produce
// ---------------------------------------------------------------------------

/// An expired session must produce an actionable instruction, not a generic
/// failure, and must never suggest that the stored profile was touched.
#[tokio::test]
async fn expired_session_tells_the_caller_to_log_in_again() -> anyhow::Result<()> {
    let (client, _) = connect(MockApi::new().failing_with("auth")).await?;

    let err = call_tool(&client, "get_cart", json!({"store_id": STORE}))
        .await
        .expect_err("should fail");
    assert!(err.contains("expired"), "{err}");
    assert!(err.contains("login"), "should name the remedy: {err}");
    assert!(
        err.contains("left untouched"),
        "should reassure about the profile: {err}"
    );

    // auth_status is the diagnostic tool, so it reports rather than erroring.
    let status = call_tool(&client, "auth_status", json!({"store_id": STORE}))
        .await
        .unwrap();
    assert_eq!(status["loggedIn"], false);
    assert!(status["detail"].as_str().unwrap().contains("expired"));
    Ok(())
}

/// A Cloudflare block is a different problem from an expired login and must not be
/// reported as one -- the remedies are unrelated.
#[tokio::test]
async fn cloudflare_block_is_reported_as_itself() -> anyhow::Result<()> {
    let (client, _) = connect(MockApi::new().failing_with("cloudflare")).await?;
    let err = call_tool(&client, "get_cart", json!({"store_id": STORE}))
        .await
        .expect_err("should fail");
    assert!(err.contains("Cloudflare"), "{err}");
    assert!(
        !err.contains("login"),
        "must not be confused with auth expiry: {err}"
    );
    Ok(())
}

/// K-Ruoka's own message is "Invalid store ID undefined" -- it does not echo the id,
/// and is identical for an empty and a nonexistent store.
#[tokio::test]
async fn invalid_store_id_is_named_back_to_the_caller() -> anyhow::Result<()> {
    let (client, _) = connect(MockApi::new().failing_with("invalid-store")).await?;
    let err = call_tool(&client, "get_cart", json!({"store_id": "ZZZZ9"}))
        .await
        .expect_err("should fail");
    assert!(err.contains("ZZZZ9"), "{err}");
    assert!(
        !err.contains("undefined"),
        "K-Ruoka's useless text leaked: {err}"
    );
    Ok(())
}

/// Malformed arguments are rejected by rmcp from the generated schema, before any
/// of our code runs -- so nothing reaches K-Ruoka.
#[tokio::test]
async fn missing_required_arguments_are_rejected_before_any_request() -> anyhow::Result<()> {
    let (client, api) = connect(MockApi::new()).await?;

    // store_id is now optional (falls back to the default); ean is still required.
    call_tool(&client, "add_to_cart", json!({"store_id": STORE}))
        .await
        .expect_err("ean is required");
    call_tool(&client, "get_cart", json!({"store_id": 42}))
        .await
        .expect_err("store_id must be a string");

    assert!(api.calls().is_empty(), "{:?}", api.calls());
    Ok(())
}

// ---------------------------------------------------------------------------
// Retry boundary
// ---------------------------------------------------------------------------

/// The retry-and-relaunch loop lives in `Session`, *below* the `KrApi` seam, so a
/// mock sees exactly one call per request no matter what it returns. That is the
/// right place for it -- relaunching a browser is not cart logic -- but it means the
/// retry *routing* is not testable from here. The policy is unit-tested in
/// `session.rs` (`plan_recovery`), and the navigation-time path has a live
/// live reproducer.
///
/// What this does pin: nothing above the seam retries on its own, so a failure
/// surfaces after a single attempt rather than being multiplied by two layers.
#[tokio::test]
async fn cart_logic_does_not_retry_on_top_of_the_session() -> anyhow::Result<()> {
    for kind in ["auth", "cloudflare", "invalid-store"] {
        let (client, api) = connect(MockApi::new().failing_with(kind)).await?;
        call_tool(&client, "get_cart", json!({"store_id": STORE}))
            .await
            .expect_err("should fail");
        assert_eq!(
            api.calls().len(),
            1,
            "{kind} was retried above the seam: {:?}",
            api.calls()
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Annotations and optional arguments
// ---------------------------------------------------------------------------

/// The read-only / destructive contracts used to live only in prose inside the
/// descriptions, where a client cannot act on them.
#[tokio::test]
async fn tools_declare_their_read_only_and_destructive_hints() -> anyhow::Result<()> {
    let (client, _) = connect(MockApi::new()).await?;
    let tools = client.list_all_tools().await?;
    let ann = |name: &str| {
        tools
            .iter()
            .find(|t| t.name == name)
            .and_then(|t| t.annotations.clone())
            .unwrap_or_else(|| panic!("no annotations on {name}"))
    };

    for read_only in ["get_cart", "auth_status"] {
        assert_eq!(
            ann(read_only).read_only_hint,
            Some(true),
            "{read_only} does not modify anything"
        );
    }
    // The only tool that destroys data. Checkout is out of scope, so this is as far
    // as the damage can go -- but it is still not undoable.
    assert_eq!(ann("clear_cart").destructive_hint, Some(true));
    assert_eq!(ann("clear_cart").read_only_hint, None);
    // Setting a quantity only ever adds or adjusts one line.
    assert_eq!(ann("add_to_cart").destructive_hint, Some(false));
    // A local state write, not a cart mutation.
    assert_eq!(ann("set_default_store").destructive_hint, Some(false));
    Ok(())
}

/// `auth_status` is the tool someone reaches for when their setup is broken, so it
/// must not demand a store id they may not have.
#[tokio::test]
async fn auth_status_works_without_a_store_id() -> anyhow::Result<()> {
    let (client, _) = connect(MockApi::new()).await?;

    let status = call_tool(&client, "auth_status", json!({}))
        .await
        .expect("store_id must be optional");
    assert_eq!(status["loggedIn"], false);

    let tools = client.list_all_tools().await?;
    let schema = tools.iter().find(|t| t.name == "auth_status").unwrap();
    let required = serde_json::to_value(&schema.input_schema).unwrap()["required"].clone();
    assert!(
        required.as_array().is_none_or(|a| a.is_empty()),
        "auth_status should require nothing, got {required}"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Which error channel a failure arrives on
// ---------------------------------------------------------------------------

/// Failures must arrive as tool results with `isError: true`, not as JSON-RPC
/// protocol errors.
///
/// This is the difference between the model reading "the item ids currently in the
/// cart are X and Y" and correcting itself, versus the client treating the call as a
/// transport failure and the text never being seen at all. Every message this server
/// produces is written to be acted on, so every one needs the right channel. It was
/// on the wrong one until this test existed.
#[tokio::test]
async fn failures_reach_the_model_rather_than_the_transport() -> anyhow::Result<()> {
    // A local rejection, made before anything is sent...
    let (client, _) = connect(MockApi::new()).await?;
    match try_call_tool(
        &client,
        "add_to_cart",
        json!({"store_id": STORE, "ean": BANANA, "quantity": 0}),
    )
    .await
    {
        Err(Failure::ToolError(text)) => {
            assert!(text.contains("greater than 0"), "{text}");
            // Must not claim K-Ruoka refused; it was never asked.
            assert!(!text.contains("K-Ruoka API error"), "{text}");
        }
        other => panic!("expected an isError result, got {other:?}"),
    }

    // ...and one that really did come back from K-Ruoka.
    let (client, _) = connect(MockApi::new().failing_with("auth")).await?;
    match try_call_tool(&client, "get_cart", json!({"store_id": STORE})).await {
        Err(Failure::ToolError(text)) => assert!(text.contains("login"), "{text}"),
        other => panic!("expected an isError result, got {other:?}"),
    }
    Ok(())
}

/// The exception the spec calls out: an unknown tool is the client's mistake, not
/// something the model can fix by trying different arguments.
#[tokio::test]
async fn an_unknown_tool_is_a_protocol_error() -> anyhow::Result<()> {
    let (client, _) = connect(MockApi::new()).await?;
    match try_call_tool(&client, "no_such_tool", json!({})).await {
        Err(Failure::Protocol(_)) => Ok(()),
        other => panic!("expected a protocol error, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Default store
// ---------------------------------------------------------------------------

/// Once set, subsequent tool calls may omit store_id and the default is used.
#[tokio::test]
async fn set_default_store_makes_store_id_optional_on_other_tools() -> anyhow::Result<()> {
    let (client, api) = connect(MockApi::new().with_item(BANANA, 2.0, "kpl")).await?;

    let result = call_tool(&client, "set_default_store", json!({"store_id": STORE}))
        .await
        .expect("set_default_store should succeed");
    assert_eq!(result["defaultStore"], STORE);

    // All store-sensitive tools can now omit store_id.
    let cart = call_tool(&client, "get_cart", json!({}))
        .await
        .expect("get_cart should use the default store");
    assert_eq!(cart["store"]["id"], STORE);

    let cart = call_tool(&client, "add_to_cart", json!({"ean": BANANA}))
        .await
        .expect("add_to_cart should use the default store");
    assert_eq!(cart["store"]["id"], STORE);

    call_tool(
        &client,
        "update_cart_item",
        json!({"item_id": BANANA, "quantity": 3}),
    )
    .await
    .expect("update_cart_item should use the default store");

    call_tool(&client, "remove_from_cart", json!({"item_id": BANANA}))
        .await
        .expect("remove_from_cart should use the default store");

    // Add one back and clear.
    call_tool(&client, "add_to_cart", json!({"ean": BANANA}))
        .await
        .unwrap();
    call_tool(&client, "clear_cart", json!({}))
        .await
        .expect("clear_cart should use the default store");

    assert_eq!(api.mutations().len(), 5, "{:?}", api.mutations());
    Ok(())
}

/// An explicit store_id takes precedence over the default.
#[tokio::test]
async fn explicit_store_id_overrides_the_default() -> anyhow::Result<()> {
    let (client, api) = connect(MockApi::new()).await?;

    call_tool(&client, "set_default_store", json!({"store_id": "DEFAULT"}))
        .await
        .unwrap();

    call_tool(&client, "get_cart", json!({"store_id": STORE}))
        .await
        .unwrap();

    // The storeId in the request body must be the explicit one, not the default.
    let calls = api.calls();
    let body = calls[0].body.as_ref().unwrap();
    assert_eq!(body["storeId"], STORE, "explicit store_id not sent");
    assert_ne!(body["storeId"], "DEFAULT", "default leaked into request");
    Ok(())
}

/// `auth_status` also falls back to the default store when one has been set, rather
/// than always probing `DEFAULT_PROBE_STORE`.
#[tokio::test]
async fn auth_status_uses_the_default_store_when_set() -> anyhow::Result<()> {
    let (client, api) = connect(MockApi::new()).await?;

    call_tool(&client, "set_default_store", json!({"store_id": STORE}))
        .await
        .unwrap();

    call_tool(&client, "auth_status", json!({}))
        .await
        .expect("auth_status should use the default store");

    let calls = api.calls();
    let body = calls[0].body.as_ref().unwrap();
    assert_eq!(body["storeId"], STORE, "default store not sent");
    Ok(())
}

/// Without a default set, omitting store_id returns a clear tool error.
#[tokio::test]
async fn omitting_store_id_without_a_default_is_a_tool_error() -> anyhow::Result<()> {
    let (client, api) = connect(MockApi::new()).await?;

    // No API call should be made: the error is detected before the request.
    let err = call_tool(&client, "get_cart", json!({}))
        .await
        .expect_err("should fail without a default store");
    assert!(
        err.contains("set_default_store"),
        "should name the remedy: {err}"
    );
    assert!(api.calls().is_empty(), "{:?}", api.calls());
    Ok(())
}

/// set_default_store with no store_id is a schema error (the field is required).
#[tokio::test]
async fn set_default_store_requires_store_id() -> anyhow::Result<()> {
    let (client, _) = connect(MockApi::new()).await?;
    call_tool(&client, "set_default_store", json!({}))
        .await
        .expect_err("store_id is required for set_default_store");
    Ok(())
}

// ---------------------------------------------------------------------------
// Default store persistence
// ---------------------------------------------------------------------------

/// set_default_store writes to the persistence file so a new server instance
/// loading from the same path starts with the stored value.
#[tokio::test]
async fn set_default_store_persists_to_file() -> anyhow::Result<()> {
    let dir = std::env::temp_dir().join(format!(
        "k-ruoka-mcp-test-persist-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .subsec_nanos()
    ));
    std::fs::create_dir_all(&dir)?;
    let store_path = dir.join("default_store");

    // First server: call set_default_store and verify the file is written.
    let (client, _) = connect_with_store_path(MockApi::new(), store_path.clone()).await?;
    call_tool(&client, "set_default_store", json!({"store_id": STORE}))
        .await
        .expect("set_default_store should succeed");
    drop(client);

    let written = std::fs::read_to_string(&store_path).expect("file should exist after set");
    assert_eq!(written.trim(), STORE);

    // Second server: loads from the same file, the default is already set.
    let (client2, api2) = connect_with_store_path(MockApi::new(), store_path.clone()).await?;
    let cart = call_tool(&client2, "get_cart", json!({}))
        .await
        .expect("get_cart should use the restored default store");
    assert_eq!(cart["store"]["id"], STORE);
    // One call was made (the get_cart), no set_default_store was needed.
    assert_eq!(api2.calls().len(), 1);

    std::fs::remove_dir_all(&dir).ok();
    Ok(())
}

/// When no persistence file exists and no default has been set, omitting store_id
/// still fails with the same error as without persistence.
#[tokio::test]
async fn missing_file_does_not_hide_the_no_default_error() -> anyhow::Result<()> {
    let dir = std::env::temp_dir().join(format!(
        "k-ruoka-mcp-test-no-file-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .subsec_nanos()
    ));
    std::fs::create_dir_all(&dir)?;
    let store_path = dir.join("default_store"); // file does not exist

    let (client, api) = connect_with_store_path(MockApi::new(), store_path).await?;
    let err = call_tool(&client, "get_cart", json!({}))
        .await
        .expect_err("should fail without a default store even with a path configured");
    assert!(
        err.contains("set_default_store"),
        "should name the remedy: {err}"
    );
    assert!(api.calls().is_empty());

    std::fs::remove_dir_all(&dir).ok();
    Ok(())
}
