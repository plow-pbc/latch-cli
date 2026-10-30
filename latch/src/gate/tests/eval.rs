//! Repeated synthetic cases through the real host judgment, without
//! executing their actions. Run with `just gatekeeper eval OUT`.

use serde::Deserialize;

use super::*;

#[derive(Deserialize)]
struct Cases {
    cases: Vec<Case>,
}

#[derive(Deserialize)]
struct Case {
    id: String,
    host_rules: Option<String>,
    agent_rules: Option<String>,
    chat: Option<Vec<Message>>,
    action: Box<RawValue>,
}

#[derive(Deserialize)]
struct Message {
    from: Sender,
    text: String,
    minutes_ago: i64,
    name: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Sender {
    Owner,
    Member,
    Agent,
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn live_eval_cases() {
    let input =
        std::env::var_os("LATCH_EVAL_CASES").expect("LATCH_EVAL_CASES names the cases file");
    let out = std::env::var_os("LATCH_EVAL_OUT").expect("LATCH_EVAL_OUT names the result file");
    let cases: Cases = serde_json::from_slice(&std::fs::read(input).unwrap()).unwrap();
    let repeats: usize = std::env::var("LATCH_EVAL_REPEATS").map_or(10, |r| r.parse().unwrap());
    assert!(!cases.cases.is_empty() && repeats > 0);
    let provider = Gatekeeper::provider_for_host(&crate::model::logged_in_for_tests()).unwrap();
    let slots = Arc::new(tokio::sync::Semaphore::new(6));
    let mut tasks = tokio::task::JoinSet::new();
    for case in cases.cases {
        let case = Arc::new(case);
        for repeat in 1..=repeats {
            let (case, provider, slots) = (case.clone(), provider.clone(), slots.clone());
            tasks.spawn(async move {
                let _held = slots.acquire().await.unwrap();
                let world = match &case.chat {
                    None => World::default(),
                    Some(messages) => World::with_chat(
                        "chat_a",
                        messages
                            .iter()
                            .enumerate()
                            .map(|(i, m)| {
                                let from = match m.from {
                                    Sender::Owner => owner(),
                                    Sender::Member => member(m.name.as_deref().unwrap_or("Alex")),
                                    Sender::Agent => agent_self(),
                                };
                                message(&format!("m{i}"), from, &m.text, m.minutes_ago)
                            })
                            .collect(),
                    ),
                };
                let rules = Rules {
                    host: case.host_rules.clone(),
                    agent: case.agent_rules.clone(),
                };
                let keeper = Gatekeeper::new(plow(world).0, provider, Box::new(rules));
                let judgment = keeper.judge(AGENT_CALLER, &case.action).await;
                json!({"case": case.id, "repeat": repeat, "gate": judgment.gate})
            });
        }
    }
    let mut rows = Vec::new();
    while let Some(row) = tasks.join_next().await {
        rows.push(row.unwrap());
    }
    let meta = json!({"path": "Gatekeeper::judge", "model": MODEL, "repeats": repeats,
                      "rules_prompt": RULES_LAYER.id(), "chat_prompt": CHAT_LAYER.id()});
    std::fs::write(
        out,
        serde_json::to_vec(&json!({"meta": meta, "rows": rows})).unwrap(),
    )
    .unwrap();
}
