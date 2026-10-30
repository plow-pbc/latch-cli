//! Paying with a saved card, end to end through the host's fill path: a
//! card saved with no sites, on a shop it was never tied to, into the
//! card forms checkouts have (the shop's own fields, with and without
//! hints; a provider's hosted fields, each its own frame on another site,
//! under the shop's own Pay button; a number in four boxes and the expiry
//! as two lists), and what an agent can read of the number afterwards.

use serde_json::{Value, json};

use super::{AMEX, CVV, Harness, NUMBER, find, item, latch, site, test_state};
use crate::browser::host::Browsers;
use crate::failure::Failure;
use crate::vault::host::Vault;
use crate::vault::item::CARD;
use crate::vault::store::Backend;

conformance!(
    a_card_with_no_sites_pays_on_any_shop,
    a_card_fills_a_providers_hosted_fields,
    a_card_fills_split_boxes_and_expiry_lists,
    a_card_saved_with_spaces_fills_by_its_digits,
    a_hosted_checkout_takes_a_number_however_it_was_saved,
    a_two_digit_year_field_gets_two_digits,
    an_amex_number_in_four_boxes_of_four_says_it_does_not_split,
    a_field_that_clears_itself_says_what_happened_and_what_was_filled,
    a_card_pays_after_a_script_on_the_shop,
    a_page_that_shows_the_number_back_shows_the_agent_only_its_last_four,
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
    let (mut h, _, local, processor, _keep) = shop(engine).await;
    // Kept with spaces, as an older latch did; the provider's fields format
    // the number and expiry as they are typed.
    let card = add_as_stored(&h, "Spaced card", " 4242 4242 4242 4242");
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
    let suggestion = observed["fill"]["suggestions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["item"] == card.as_str())
        .unwrap()
        .clone();
    assert_eq!(suggestion["press"], Value::Null);
    assert_eq!(
        suggestion["frames"],
        json!([
            "https://www.merchant.example",
            "https://js.processor.example"
        ])
    );
    let paid = h
        .pay(
            json!({"item": card, "suggested": suggestion["id"], "snapshot": observed["snapshot"],
                    "press": find(&observed, "button", "Pay $10.00")}),
        )
        .await
        .unwrap();
    // In the order the provider's frames came, which varies.
    let mut fields = filled(&paid);
    fields.sort();
    assert_eq!(fields, ["cardholder name", "code", "expiry", "number"]);
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
    let (mut h, _, local, _, _keep) = shop(engine).await;
    // Kept with spaces, as an older latch did: its digits split.
    let card = add_as_stored(&h, "Spaced card", " 4242 4242 4242 4242");
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

/// A script the agent ran on the shop earlier in the session (one the
/// gatekeeper let through, as page work) does not keep the card out: a
/// card fills under the same protections as a password.
async fn a_card_pays_after_a_script_on_the_shop(engine: &str) {
    let (mut h, card, local, _, _keep) = shop(engine).await;
    h.call("open", json!({"url": format!("{local}/checkout.html")}))
        .await
        .unwrap();
    h.call("eval", json!({"script": "window.scrollTo(0, 0); 1"}))
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
    let checkout = h
        .call("open", json!({"url": format!("{local}/card-echo.html")}))
        .await
        .unwrap();
    // While a value latch filled is in a field, no script runs, as for a
    // password: here the expiry, filled with no press.
    let (filled, _) = h
        .gated_fill(
            json!({"item": card, "fields": [["expiry", [find(&checkout, "textbox", "Expiry")]]]}),
        )
        .await;
    assert_eq!(filled.unwrap()["submitted"], false);
    let refused = h.call("eval", json!({"script": "1"})).await.unwrap_err();
    assert_eq!(refused.code, "secret_on_page");

    // That script never ran: the card still pays on the shop.
    let args = h.suggested(&card).await;
    h.pay(args).await.unwrap();
    // Once the page was submitted and its fields are empty, scripts run.
    h.call("eval", json!({"script": "document.title"}))
        .await
        .unwrap();
    let page = h.text().await;
    println!("the shop's confirmation, as the agent reads it: {page:?}");
    assert!(page.contains("Paid $10.00 with card •••• 4242"), "{page}");
    // The last four digits on their own are no card number.
    assert!(page.contains("Visa ending in 4242"), "{page}");

    // Pictures are taken once the card was filled and paid.
    h.call("screenshot", json!({})).await.unwrap();

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

async fn a_card_saved_with_spaces_fills_by_its_digits(engine: &str) {
    let (mut h, _, local, _, _keep) = shop(engine).await;
    // As an older latch kept it, into a number field of 16 characters.
    let old = add_as_stored(&h, "Old card", " 4242 4242 4242 4242");
    h.call(
        "open",
        json!({"url": format!("{local}/checkout.html?max16")}),
    )
    .await
    .unwrap();
    let args = h.suggested(&old).await;
    h.pay(args).await.unwrap();
    let page = h.text().await;
    assert!(page.contains(PAID), "{page}");
    assert!(!h.saw_the_card(), "the card reached a result or the audit");
    h.call("close", json!({})).await.ok();
}

/// A card as an older latch may have kept it: the number as it was
/// typed, spaces and all.
fn add_as_stored(h: &Harness, title: &str, number: &str) -> String {
    h.browsers
        .vault
        .with(|s| {
            s.add_as_stored(item(
                CARD,
                title,
                &[],
                &[
                    ("cardholder name", "Ada Lovelace"),
                    ("number", number),
                    ("expiry month", "04"),
                    ("expiry year", "2031"),
                    ("code", CVV),
                ],
            ))
        })
        .unwrap()
        .item
}

/// The fields a paid fill's trail says went in, in order, each once.
fn filled(paid: &Value) -> Vec<String> {
    let mut fields: Vec<String> = Vec::new();
    for field in paid["trail"][0]["fields"].as_array().unwrap() {
        let name = field["field"].as_str().unwrap().to_string();
        if field["outcome"] == "filled" && !fields.contains(&name) {
            fields.push(name);
        }
    }
    fields
}

async fn a_hosted_checkout_takes_a_number_however_it_was_saved(engine: &str) {
    let (mut h, plain, local, _, _keep) = shop(engine).await;
    let spaced = add_as_stored(&h, "Spaced card", " 4242 4242 4242 4242");
    let amex = add_as_stored(&h, "Amex", "3782 8224 6310 005");
    for card in [&spaced, &plain, &amex] {
        h.call("open", json!({"url": format!("{local}/card-hosted.html")}))
            .await
            .unwrap();
        let args = h.suggested(card).await;
        let paid = h.pay(args).await.unwrap();
        assert_eq!(
            filled(&paid),
            ["number", "expiry", "code", "cardholder name"],
            "{card}"
        );
        let page = h.text().await;
        println!("{card} on the hosted checkout: {page}");
        assert!(page.contains(PAID), "{card}: {page}");
    }
    assert!(!h.saw_the_card(), "the card reached a result or the audit");
    assert!(!h.shown.join("\n").replace(' ', "").contains(AMEX));
    h.call("close", json!({})).await.ok();
}

async fn a_two_digit_year_field_gets_two_digits(engine: &str) {
    let (mut h, card, local, _, _keep) = shop(engine).await;
    let split = h
        .call(
            "open",
            json!({"url": format!("{local}/card-split.html?year2")}),
        )
        .await
        .unwrap();
    let boxes: Vec<String> = (1..=4)
        .map(|n| find(&split, "textbox", &format!("Card number {n}")))
        .collect();
    let fields = json!([
        ["cardholder name", [find(&split, "textbox", "Name on card")]],
        ["number", boxes],
        ["expiry month", [find(&split, "combobox", "Month")]],
        ["expiry year", [find(&split, "textbox", "Year")]],
        ["cvv", [find(&split, "textbox", "CVC")]],
    ]);
    let press = find(&split, "button", "Pay $10.00");
    let paid = h
        .pay(json!({"item": card, "fields": fields, "press": press}))
        .await
        .unwrap();
    assert_eq!(
        filled(&paid),
        [
            "cardholder name",
            "number",
            "expiry month",
            "expiry year",
            "cvv"
        ]
    );
    let page = h.text().await;
    assert!(page.contains(PAID), "{page}");
    h.call("close", json!({})).await.ok();
}

async fn an_amex_number_in_four_boxes_of_four_says_it_does_not_split(engine: &str) {
    let (mut h, _, local, _, _keep) = shop(engine).await;
    let amex = add_as_stored(&h, "Amex", AMEX);
    let split = h
        .call("open", json!({"url": format!("{local}/card-split.html")}))
        .await
        .unwrap();
    let boxes: Vec<String> = (1..=4)
        .map(|n| find(&split, "textbox", &format!("Card number {n}")))
        .collect();
    let press = find(&split, "button", "Pay $10.00");
    let refused = h
        .pay(json!({"item": amex, "fields": [["number", boxes]], "press": press}))
        .await
        .unwrap_err();
    println!(
        "an Amex in four boxes: {} ({})",
        refused.code, refused.message
    );
    assert!(
        refused.message.contains(
            "the number (15 characters) does not split into those 4 boxes; nothing was filled"
        ),
        "{}",
        refused.message
    );
    assert!(!h.text().await.contains("payment:"));
    h.call("close", json!({})).await.ok();
}

async fn a_field_that_clears_itself_says_what_happened_and_what_was_filled(engine: &str) {
    let (mut h, card, local, _, _keep) = shop(engine).await;
    let page = h
        .call(
            "open",
            json!({"url": format!("{local}/card-clearing.html")}),
        )
        .await
        .unwrap();
    let fields = json!([
        ["cardholder name", [find(&page, "textbox", "Name on card")]],
        ["number", [find(&page, "textbox", "Card number")]],
        ["expiry", [find(&page, "textbox", "Expiry")]],
        ["cvv", [find(&page, "textbox", "CVV")]],
    ]);
    let press = find(&page, "button", "Pay");
    let refused = h
        .pay(json!({"item": card, "fields": fields, "press": press}))
        .await
        .unwrap_err();
    println!(
        "a field that clears itself: {} ({})",
        refused.code, refused.message
    );
    assert!(
        refused.message.contains(
            "the page did not keep the number latch typed: it cleared it. This stage was not submitted; filled before it, and cleared again: cardholder name; not filled: expiry, cvv. tell the owner what the page did; latch can't fill it there"
        ),
        "{}",
        refused.message
    );
    assert!(!h.text().await.contains("payment:"));
    h.call("close", json!({})).await.ok();
}
