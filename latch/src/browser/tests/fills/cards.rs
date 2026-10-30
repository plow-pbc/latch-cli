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
    a_page_error_after_pay_names_its_field_and_the_same_fill_goes_again,
    a_page_that_stays_silent_after_pay_is_uncertain_and_not_retried,
    a_required_field_left_empty_is_named_before_pay,
    a_field_the_page_takes_back_stops_the_fill_before_pay,
    a_page_that_changes_as_the_card_goes_in_is_still_paid_and_says_so,
    an_alert_that_passes_as_the_page_moves_on_is_no_page_error,
    agent_code_on_a_shop_keeps_the_card_out,
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
    // A suggestion names its item: `--suggested` alone takes the card.
    let mut args = h.suggested(&card).await;
    args.as_object_mut().unwrap().remove("item");
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
            "the page did not keep the number latch typed: it cleared it. This stage was not submitted; filled, and cleared again: cardholder name; not filled: expiry, cvv. `observe` the page, then fill again with the number mapped to the field the page means"
        ),
        "{}",
        refused.message
    );
    assert!(!h.text().await.contains("payment:"));
    h.call("close", json!({})).await.ok();
}

/// The fussy checkout's page, its fields by name, and a mapping of the
/// card's fields with the code into `code_field`.
async fn fussy(h: &mut Harness, local: &str, query: &str, code_field: &str) -> (Value, Value) {
    let page = h
        .call(
            "open",
            json!({"url": format!("{local}/card-fussy.html{query}")}),
        )
        .await
        .unwrap();
    let fields = json!([
        ["cardholder name", [find(&page, "textbox", "Name on card")]],
        ["number", [find(&page, "textbox", "Card number")]],
        ["expiry", [find(&page, "textbox", "Expiry")]],
        ["cvv", [find(&page, "textbox", code_field)]],
    ]);
    (page, fields)
}

/// Types `text` into the field named `name` on `page`, as an agent's
/// literal step does.
async fn type_into(h: &mut Harness, page: &Value, name: &str, text: &str) {
    let step = format!("type {} \"{text}\"", find(page, "textbox", name));
    h.call(
        "do",
        json!({"request": format!("r-type-{}", h.shown.len()),
               "plan": {"steps": [crate::browser::plan::short_step(&step).unwrap()]}}),
    )
    .await
    .unwrap();
}

async fn a_page_error_after_pay_names_its_field_and_the_same_fill_goes_again(engine: &str) {
    let (mut h, card, local, _, _keep) = shop(engine).await;
    // The code goes into the page's older field, which it doesn't check.
    let (page, fields) = fussy(&mut h, &local, "", "Security code (old)").await;
    type_into(&mut h, &page, "ZIP", "94110").await;
    let press = find(&page, "button", "Pay $10.00");
    let paid = h
        .pay(json!({"item": card, "fields": fields, "press": press}))
        .await
        .unwrap();
    let step = &paid["trail"][0];
    println!("the page refused it: {step}");
    assert_eq!(step["outcome"], "page_error");
    assert_eq!(
        step["page_errors"],
        json!([{"field": "CVC", "text": "Your card's security code is incomplete."}])
    );
    let next = step["next"].as_str().unwrap();
    assert!(
        next.contains("CVC: Your card's security code is incomplete.")
            && next.contains(&format!("fill --item {card}")),
        "{next}"
    );
    assert!(!h.text().await.contains("payment:"));

    // The same fill, the code now in the field the page checks: it goes
    // through on the confirmation already given.
    let fields = json!([
        ["cardholder name", [find(&page, "textbox", "Name on card")]],
        ["number", [find(&page, "textbox", "Card number")]],
        ["expiry", [find(&page, "textbox", "Expiry")]],
        ["cvv", [find(&page, "textbox", "CVC")]],
    ]);
    let (again, _) = h
        .gated_fill(json!({"item": card, "fields": fields, "press": press}))
        .await;
    let again = again.unwrap();
    println!("filled again: {again}");
    assert!(again["confirmed_by"].is_string(), "{again}");
    let page = h.text().await;
    assert!(page.contains(PAID), "{page}");
    assert!(!h.saw_the_card(), "the card reached a result or the audit");
    h.call("close", json!({})).await.ok();
}

async fn a_page_that_stays_silent_after_pay_is_uncertain_and_not_retried(engine: &str) {
    let (mut h, card, local, _, _keep) = shop(engine).await;
    let (page, fields) = fussy(&mut h, &local, "?keep", "CVC").await;
    type_into(&mut h, &page, "ZIP", "94110").await;
    let press = find(&page, "button", "Pay $10.00");
    let paid = h
        .pay(json!({"item": card, "fields": fields, "press": press}))
        .await
        .unwrap();
    let step = &paid["trail"][0];
    println!("the page said nothing: {step}");
    assert_eq!(step["outcome"], "uncertain");
    assert!(step["page_errors"].is_null());
    let next = step["next"].as_str().unwrap();
    assert!(
        next.contains("don't submit again") && !next.contains("fill --item"),
        "{next}"
    );
    // No retry without a new confirmation.
    let (again, _) = h
        .gated_fill(json!({"item": card, "fields": fields, "press": press}))
        .await;
    assert_eq!(again.unwrap_err().code, "confirm_needed");
    h.call("close", json!({})).await.ok();
}

async fn a_required_field_left_empty_is_named_before_pay(engine: &str) {
    let (mut h, card, local, _, _keep) = shop(engine).await;
    let (page, fields) = fussy(&mut h, &local, "?required", "CVC").await;
    let press = find(&page, "button", "Pay $10.00");
    let zip = find(&page, "textbox", "ZIP");
    let refused = h
        .pay(json!({"item": card, "fields": fields, "press": press}))
        .await
        .unwrap_err();
    println!("before pay: {} ({})", refused.code, refused.message);
    assert!(
        refused.message.contains(&format!(
            "the form still needs ZIP ({zip}), which this fill doesn't hold; nothing was pressed"
        )),
        "{}",
        refused.message
    );
    // Nothing was submitted, and the agent is back to fill the ZIP.
    assert!(!h.text().await.contains("security code is incomplete"));
    type_into(&mut h, &page, "ZIP", "94110").await;
    h.pay(json!({"item": card, "fields": fields, "press": press}))
        .await
        .unwrap();
    let page = h.text().await;
    assert!(page.contains(PAID), "{page}");
    h.call("close", json!({})).await.ok();
}

async fn a_field_the_page_takes_back_stops_the_fill_before_pay(engine: &str) {
    let (mut h, card, local, _, _keep) = shop(engine).await;
    let (page, fields) = fussy(&mut h, &local, "?takeback", "CVC").await;
    type_into(&mut h, &page, "ZIP", "94110").await;
    let press = find(&page, "button", "Pay $10.00");
    let refused = h
        .pay(json!({"item": card, "fields": fields, "press": press}))
        .await
        .unwrap_err();
    println!("taken back: {} ({})", refused.code, refused.message);
    assert!(
        refused.message.contains(
            "the page did not keep the number latch typed: it cleared it. This stage was not submitted; filled, and cleared again: cardholder name, expiry, cvv; not filled: none."
        ),
        "{}",
        refused.message
    );
    assert!(!h.text().await.contains("security code is incomplete"));
    h.call("close", json!({})).await.ok();
}

async fn a_page_that_changes_as_the_card_goes_in_is_still_paid_and_says_so(engine: &str) {
    let (mut h, card, local, _, _keep) = shop(engine).await;
    for (query, said) in [
        (
            "?hint",
            "since the confirmation: button \"Pay $10.00\" unchanged; other text around the button changed",
        ),
        (
            "?reprice",
            "since the confirmation: amounts $10.00, $10.00 → $12.00, $12.00; button \"Pay $10.00\" → \"Pay $12.00\"",
        ),
    ] {
        let (page, fields) = fussy(&mut h, &local, query, "CVC").await;
        type_into(&mut h, &page, "ZIP", "94110").await;
        let press = find(&page, "button", "Pay $10.00");
        let paid = h
            .pay(json!({"item": card, "fields": fields, "press": press}))
            .await
            .unwrap();
        let step = &paid["trail"][0];
        println!("{query}: {step}");
        assert_eq!(step["since_confirmation"], said, "{query}");
        let text = h.text().await;
        assert!(text.contains(PAID), "{query}: {text}");
    }
    h.call("close", json!({})).await.ok();
}

/// A payment still processing may flash an alert before its page moves
/// on: that is the page moving on, not an error to fill again for.
async fn an_alert_that_passes_as_the_page_moves_on_is_no_page_error(engine: &str) {
    let (mut h, card, local, _, _keep) = shop(engine).await;
    let (page, fields) = fussy(&mut h, &local, "?flash", "CVC").await;
    type_into(&mut h, &page, "ZIP", "94110").await;
    let press = find(&page, "button", "Pay $10.00");
    let paid = h
        .pay(json!({"item": card, "fields": fields, "press": press}))
        .await
        .unwrap();
    let step = &paid["trail"][0];
    println!("a flashed alert: {step}");
    assert_eq!(step["outcome"], "moved_on");
    assert!(step["next"].is_null(), "{step}");
    let text = h.text().await;
    assert!(text.contains(PAID), "{text}");
    h.call("close", json!({})).await.ok();
}
