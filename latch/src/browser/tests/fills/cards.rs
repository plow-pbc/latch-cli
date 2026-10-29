//! Paying with a saved card, end to end through the host's fill path: a
//! card saved with no sites, on a shop it was never tied to, into the
//! card forms checkouts have (the shop's own fields, with and without
//! hints; a provider's hosted fields, each its own frame on another site,
//! under the shop's own Pay button; a number in four boxes and the expiry
//! as two lists), and what an agent can read of the number afterwards.

use serde_json::{Map, Value, json};

use super::{
    CALLER, CVV, Harness, NUMBER, find, item, latch, seen, seen_failure, site, test_state,
};
use crate::browser::host::Browsers;
use crate::failure::Failure;
use crate::vault::host::Vault;
use crate::vault::item::CARD;
use crate::vault::store::Backend;

conformance!(
    a_card_with_no_sites_pays_on_any_shop,
    a_card_fills_a_providers_hosted_fields,
    a_card_fills_split_boxes_and_expiry_lists,
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
    /// which the agent then gives with `do --confirm`, through the host.
    async fn pay(&mut self, args: Value) -> (Result<Value, Failure>, Map<String, Value>) {
        let (stop, _) = self.gated_fill(args).await;
        let stop = stop.expect_err("a card fill stops for a confirmation first");
        assert_eq!(stop.code, "confirm_needed", "{}", stop.message);
        println!("confirmation: {}", stop.details["confirmation"]);
        let mut audit = Map::new();
        let done = super::super::host_call(
            &self.browsers,
            CALLER.into(),
            "do",
            &self.id,
            &json!({"request": format!("r-pay-{}", self.shown.len()), "plan": {"steps": []},
                    "confirm": stop.details["confirmation"]["id"]}),
            None,
            &mut audit,
        )
        .await
        .map(|(done, _)| done);
        self.shown.push(seen(&Value::Object(audit.clone())));
        match &done {
            Ok(value) => self.shown.push(seen(value)),
            Err(failure) => self.shown.push(seen_failure(failure)),
        }
        (done, audit)
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
    h.call("open", json!({"url": format!("{local}/card-form.html")}))
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
    let (paid, audit) = h.pay(args).await;
    println!(
        "paid: {}; audit {}",
        paid.unwrap()["trail"],
        Value::Object(audit)
    );
    let page = h.text().await;
    assert!(page.contains(PAID), "{page}");

    // The same form with no hints: nothing is suggested, and the agent's
    // own mapping pays.
    h.call(
        "open",
        json!({"url": format!("{local}/card-form.html?bare")}),
    )
    .await
    .unwrap();
    let bare = h.call("observe", json!({})).await.unwrap();
    assert_eq!(bare["fill"]["suggestions"], json!([]), "{}", bare["fill"]);
    let fields = json!([
        ["cardholder name", [find(&bare, "textbox", "Name on card")]],
        ["number", [find(&bare, "textbox", "Card number")]],
        ["expiry", [find(&bare, "textbox", "Expiry")]],
        ["cvv", [find(&bare, "textbox", "CVC")]],
    ]);
    let press = find(&bare, "button", "Pay $10.00");
    let (paid, _) = h
        .pay(json!({"item": card, "fields": fields, "press": press}))
        .await;
    paid.unwrap();
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
    let (paid, audit) = h
        .pay(
            json!({"item": card, "suggested": suggestion["id"], "snapshot": observed["snapshot"],
                    "press": find(&observed, "button", "Pay $10.00")}),
        )
        .await;
    println!(
        "paid: {:?}; audit {}",
        paid.map(|p| p["trail"].clone()),
        Value::Object(audit)
    );
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
    let (paid, audit) = h
        .pay(json!({"item": card, "fields": fields, "press": press}))
        .await;
    println!(
        "paid: {:?}; audit {}",
        paid.map(|p| p["trail"].clone()),
        Value::Object(audit)
    );
    let page = h.text().await;
    assert!(page.contains(PAID), "{page}");
    assert!(!h.saw_the_card(), "the card reached a result or the audit");
    h.call("close", json!({})).await.ok();
}
