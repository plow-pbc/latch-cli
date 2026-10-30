//! Paying with a saved card, end to end through the host's fill path: a
//! card saved with no sites, on a shop it was never tied to, into the
//! card forms checkouts have (the shop's own fields, with and without
//! hints; a provider's hosted fields, each its own frame on another site,
//! under the shop's own Pay button; a number in four boxes and the expiry
//! as two lists), and what an agent can read of the number afterwards.

use serde_json::{Value, json};

use super::{CVV, Harness, NUMBER, find, item, latch, site, test_state};
use crate::browser::host::Browsers;
use crate::failure::Failure;
use crate::vault::host::Vault;
use crate::vault::item::CARD;
use crate::vault::store::Backend;

conformance!(
    a_card_with_no_sites_pays_on_any_shop,
    a_card_fills_a_providers_hosted_fields,
    a_card_fills_split_boxes_and_expiry_lists,
    agent_code_on_a_shop_keeps_the_card_out,
    a_page_that_shows_the_number_back_shows_the_agent_only_its_last_four,
    a_page_that_takes_the_mask_off_gets_no_picture,
    a_redrawn_number_field_does_not_show_its_value,
);

/// What the shop's `/pay` says when the test card arrived whole.
const PAID: &str = "number ok, expiry ok (04 / 31), cvv ok, name Ada Lovelace";

/// A harness on a shop (`https://www.merchant.example`) with a payment
/// provider beside it (`https://js.processor.example`, its port the
/// second value), and the vault's one card, saved with no sites.
async fn shop(engine: &str) -> (Harness, String, String, u16, impl Sized) {
    let (dir, state, area) = test_state(engine);
    let vault = Vault::new(state.clone());
    let card = vault
        .with(|s| {
            s.add(item(
                CARD,
                "Plow card",
                &[],
                &[
                    ("cardholder name", "Ada Lovelace"),
                    ("number", NUMBER),
                    ("expiry month", "4"),
                    ("expiry year", "2031"),
                    ("code", CVV),
                ],
            ))
        })
        .unwrap();
    let (pages, processor) = (site(), site());
    let local = format!("http://127.0.0.1:{pages}");
    let mut browsers = Browsers::new(state, vault, super::super::HEADLESS);
    browsers.test = Some((
        latch(),
        vec![
            (local.clone(), "https://www.merchant.example".into()),
            (
                format!("http://localhost:{processor}"),
                "https://js.processor.example".into(),
            ),
        ],
    ));
    let h = Harness::start(browsers, engine).await;
    (h, card.item, local, processor, (dir, area))
}

impl Harness {
    /// A card fill as an agent makes one: it stops for a confirmation,
    /// which the agent then gives with `do --confirm`.
    async fn pay(&mut self, args: Value) -> Result<Value, Failure> {
        let (stop, _) = self.gated_fill(args).await;
        let stop = stop.expect_err("a card fill stops for a confirmation first");
        assert_eq!(stop.code, "confirm_needed", "{}", stop.message);
        println!("confirmation: {}", stop.details["confirmation"]);
        let request = format!("r-pay-{}", self.shown.len());
        self.call(
            "do",
            json!({"request": request, "plan": {"steps": []},
                   "confirm": stop.details["confirmation"]["id"]}),
        )
        .await
    }

    /// Whether the card's number or code reached anything the agent or
    /// the audit was shown, however the page spaced the number.
    fn saw_the_card(&self) -> bool {
        let everything = self.shown.join("\n");
        everything.replace([' ', '-'], "").contains(NUMBER) || everything.contains(CVV)
    }
}

async fn a_card_with_no_sites_pays_on_any_shop(engine: &str) {
    let (mut h, card, local, _, _keep) = shop(engine).await;

    // The shop's own form, with its hints: latch lists the card and
    // suggests the whole mapping, Pay included.
    h.call("open", json!({"url": format!("{local}/checkout.html")}))
        .await
        .unwrap();
    let observed = h.call("observe", json!({})).await.unwrap();
    println!("shop's own form: fill {}", observed["fill"]);
    let listed = &observed["fill"]["items"][0];
    assert_eq!(
        (listed["item"].as_str(), listed["kind"].as_str()),
        (Some(card.as_str()), Some("card"))
    );
    let args = h.suggested(&card).await;
    let paid = h.pay(args).await.unwrap();
    println!("paid: {}", paid["trail"]);
    let page = h.text().await;
    assert!(page.contains(PAID), "{page}");

    // The same form with no hints: nothing is suggested, and the agent's
    // own mapping pays.
    h.call(
        "open",
        json!({"url": format!("{local}/checkout.html?bare")}),
    )
    .await
    .unwrap();
    let bare = h.call("observe", json!({})).await.unwrap();
    assert_eq!(bare["fill"]["suggestions"], json!([]), "{}", bare["fill"]);
    let fields = json!([
        ["cardholder name", [find(&bare, "textbox", "Name on card")]],
        ["number", [find(&bare, "textbox", "Card number")]],
        ["expiry", [find(&bare, "textbox", "Expiry")]],
        ["cvv", [find(&bare, "textbox", "CVV")]],
    ]);
    let press = find(&bare, "button", "Pay");
    h.pay(json!({"item": card, "fields": fields, "press": press}))
        .await
        .unwrap();
    let page = h.text().await;
    assert!(page.contains(PAID), "{page}");

    assert!(!h.saw_the_card(), "the card reached a result or the audit");
    h.call("close", json!({})).await.ok();
}

async fn a_card_fills_a_providers_hosted_fields(engine: &str) {
    let (mut h, card, local, processor, _keep) = shop(engine).await;
    h.call(
        "open",
        json!({"url": format!("{local}/card-elements.html?frame={processor}")}),
    )
    .await
    .unwrap();
    // latch suggests the fields across the frames, but no button: Pay is in
    // another document than the fields. The agent names it.
    let observed = h.call("observe", json!({})).await.unwrap();
    println!("hosted fields: fill {}", observed["fill"]);
    let suggestion = &observed["fill"]["suggestions"][0];
    assert_eq!(suggestion["press"], Value::Null);
    let paid = h
        .pay(
            json!({"item": card, "suggested": suggestion["id"], "snapshot": observed["snapshot"],
                    "press": find(&observed, "button", "Pay $10.00")}),
        )
        .await
        .unwrap();
    println!("paid: {}", paid["trail"]);
    h.call("wait", json!({"text": "payment:", "within": 5}))
        .await
        .unwrap();
    let page = h.text().await;
    println!("the provider answered: {page}");
    assert!(page.contains(PAID), "{page}");
    assert!(!h.saw_the_card(), "the card reached a result or the audit");
    h.call("close", json!({})).await.ok();
}

async fn a_card_fills_split_boxes_and_expiry_lists(engine: &str) {
    let (mut h, card, local, _, _keep) = shop(engine).await;
    let checkout = h
        .call("open", json!({"url": format!("{local}/card-split.html")}))
        .await
        .unwrap();
    println!("split form: fill {}", checkout["fill"]);
    let boxes: Vec<String> = (1..=4)
        .map(|n| find(&checkout, "textbox", &format!("Card number {n}")))
        .collect();
    let fields = json!([
        [
            "cardholder name",
            [find(&checkout, "textbox", "Name on card")]
        ],
        ["number", boxes],
        ["expiry month", [find(&checkout, "combobox", "Month")]],
        ["expiry year", [find(&checkout, "combobox", "Year")]],
        ["cvv", [find(&checkout, "textbox", "CVC")]],
    ]);
    let press = find(&checkout, "button", "Pay $10.00");
    let paid = h
        .pay(json!({"item": card, "fields": fields, "press": press}))
        .await
        .unwrap();
    println!("paid: {}", paid["trail"]);
    let page = h.text().await;
    assert!(page.contains(PAID), "{page}");
    assert!(!h.saw_the_card(), "the card reached a result or the audit");
    h.call("close", json!({})).await.ok();
}

async fn agent_code_on_a_shop_keeps_the_card_out(engine: &str) {
    let (mut h, card, local, _, _keep) = shop(engine).await;
    h.call("open", json!({"url": format!("{local}/checkout.html")}))
        .await
        .unwrap();
    // The agent's script stays behind on the shop and copies what is typed.
    let planted = "document.addEventListener('input', (e) => localStorage.setItem('copied-' + e.target.name, e.target.value), true); 1";
    h.call("eval", json!({"script": planted})).await.unwrap();
    let args = h.suggested(&card).await;
    let refused = h.pay(args).await.unwrap_err();
    println!(
        "after the agent's script: {} ({})",
        refused.code, refused.message
    );
    assert_eq!(refused.code, "agent_code_on_site");
    let copied = h
        .call(
            "eval",
            json!({"script": "localStorage.getItem('copied-cc-number')"}),
        )
        .await
        .unwrap();
    assert_eq!(copied["value"], Value::Null, "{copied}");

    // A new session has none of it: the card pays there.
    h.new_session().await;
    h.call("open", json!({"url": format!("{local}/checkout.html")}))
        .await
        .unwrap();
    let args = h.suggested(&card).await;
    h.pay(args).await.unwrap();
    let page = h.text().await;
    assert!(page.contains(PAID), "{page}");
    assert!(!h.saw_the_card(), "the card reached a result or the audit");
    h.call("close", json!({})).await.ok();
}

async fn a_page_that_shows_the_number_back_shows_the_agent_only_its_last_four(engine: &str) {
    let (mut h, card, local, _, _keep) = shop(engine).await;
    h.call("open", json!({"url": format!("{local}/card-echo.html")}))
        .await
        .unwrap();
    let args = h.suggested(&card).await;
    h.pay(args).await.unwrap();
    let page = h.text().await;
    println!("the shop's confirmation, as the agent reads it: {page:?}");
    assert!(page.contains("Paid $10.00 with card •••• 4242"), "{page}");

    // A picture is taken with the number's digits replaced by dots (the
    // page outlines its text, which no colour would hide), and the number
    // put back after.
    h.call("screenshot", json!({})).await.unwrap();
    let page = h.text().await;
    assert!(page.contains("[dots][number]"), "{page}");

    // A plan's own pictures too, and its journal, read back after the
    // session closed, holds no number, though the step's field is named
    // after it.
    let page = h.call("observe", json!({})).await.unwrap();
    let nickname = find(&page, "textbox", "Nickname for card •••• 4242");
    let step = format!("type {nickname} \"Work\"");
    let typed = h
        .call(
            "do",
            json!({"request": "r-type", "plan": {"steps": [
                crate::browser::plan::short_step(&step).unwrap()]}}),
        )
        .await
        .unwrap();
    assert!(
        typed["trail"][0]["screenshot"]["shot"].is_number(),
        "{typed}"
    );

    // No script runs in the session: it could read the number in any form.
    let script = "[...document.getElementById('done').textContent].map((c) => c.charCodeAt(0))";
    let eval = h.call("eval", json!({"script": script})).await.unwrap_err();
    println!("eval: {} ({})", eval.code, eval.message);
    assert_eq!(eval.code, "secret_on_page");
    // Nor one the host let in before the card was filled, which reached
    // the session only once the fill was done.
    let late = crate::browser::host::worker(
        &h.browsers,
        &h.entry,
        "eval",
        &json!({"script": script}),
        None,
        &mut serde_json::Map::new(),
    )
    .await
    .unwrap_err();
    assert_eq!(late.code, "secret_on_page");

    h.call("close", json!({})).await.unwrap();
    let history = h
        .call("do_history", json!({"request": "r-type"}))
        .await
        .unwrap();
    println!("history: {history}");
    assert!(
        !h.saw_the_card(),
        "the page's copy of the number reached the agent"
    );
}

async fn a_page_that_takes_the_mask_off_gets_no_picture(engine: &str) {
    let (mut h, card, local, _, _keep) = shop(engine).await;
    h.call(
        "open",
        json!({"url": format!("{local}/card-echo.html?strip")}),
    )
    .await
    .unwrap();
    let args = h.suggested(&card).await;
    h.pay(args).await.unwrap();
    let shot = h.call("screenshot", json!({})).await.unwrap_err();
    println!("screenshot: {} ({})", shot.code, shot.message);
    assert_eq!(shot.code, "secret_on_page");
    let page = h.call("observe", json!({})).await.unwrap();
    let step = format!("type {} \"04 / 31\"", find(&page, "textbox", "Expiry"));
    let typed = h
        .call(
            "do",
            json!({"request": "r-type", "plan": {"steps": [
                crate::browser::plan::short_step(&step).unwrap()]}}),
        )
        .await
        .unwrap();
    assert_eq!(typed["trail"][0]["screenshot"], json!({"suppressed": true}));
    assert!(!h.saw_the_card(), "the card reached the agent");
    h.call("close", json!({})).await.ok();
}

async fn a_redrawn_number_field_does_not_show_its_value(engine: &str) {
    let (mut h, card, local, _, _keep) = shop(engine).await;
    h.call(
        "open",
        json!({"url": format!("{local}/card-rerender.html")}),
    )
    .await
    .unwrap();
    let args = h.suggested(&card).await;
    h.pay(args).await.unwrap();
    let observed = h.call("observe", json!({})).await.unwrap();
    println!("after the shop redrew its form: {}", observed["elements"]);
    let number = &observed["elements"][0];
    assert_eq!(
        (number["name"].as_str(), &number["secret"], &number["value"]),
        (Some("Card number"), &json!(true), &Value::Null)
    );
    assert!(
        !h.saw_the_card(),
        "the redrawn field's value reached the agent"
    );
    h.call("close", json!({})).await.ok();
}
