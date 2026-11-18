//! The owner's OK by a reaction in Swarm. When this host's settings turn it
//! on (`swarm_approvals`), an agent's action that the owner's rules and
//! chat left uncovered is posted, through Plow, to the owner's Swarm
//! approval room as one short line, with the reference latch made of the
//! exact action. The owner's ✅ on that line, there, within 30 minutes,
//! allows that action once: Plow checks the reaction, and latch takes its
//! answer from Plow only, never from the agent.
//!
//! Only an action the line names whole is asked: filling a vault item, which
//! goes only onto the item's saved sites (what, which site, which item).
//! Everything else keeps the chat's way. A short line could only say "run a
//! command" of an exec, and only a host of a page to open, hiding its port,
//! path and query: the owner would approve blind.

use serde_json::{Value, json};

use super::sha256;
use crate::failure::Failure;
use crate::plow::Plow;

/// The line the owner sees for `action`, as the fields Plow's template
/// shows: what it does, on which site, with which vault item. `None` for an
/// action no such line names whole.
pub fn summary(action: &Value) -> Option<Value> {
    if action["kind"] != "vault_fill" {
        return None;
    }
    let site = action["site"].as_str()?;
    let item = action["item"]["title"].as_str()?;
    let kind = action["item"]["kind"].as_str().unwrap_or("item");
    Some(json!({"verb": format!("fill {kind}"), "site": site, "item": item}))
}

/// The reference of `action` by agent `agent` on host `host`: what the
/// owner's ✅ allows, and nothing else.
pub fn reference(host: &str, agent: &str, action: &Value) -> String {
    sha256(&json!({"host": host, "agent": agent, "action": action}).to_string())
}

/// How an approval call went.
pub enum Asked {
    /// The owner's ✅ allows the action, this once: the request's and the
    /// reaction's event ids.
    Approved { prompt: String, reaction: String },
    /// The request is posted, waiting for the owner: its event id.
    Waiting { prompt: String },
}

/// Whether the owner's ✅ already allows `reference`; if not, the request
/// posted for it (Plow posts each reference once while it is live).
pub async fn ask(
    plow: &Plow,
    reference: &str,
    host: &str,
    agent: &str,
    summary: &Value,
) -> Result<Asked, Failure> {
    let confirmed = plow
        .approval(&json!({"op": "confirm", "ref": reference}))
        .await?;
    let id = |answer: &Value, key: &str| answer[key].as_str().unwrap_or_default().to_string();
    if confirmed["approved"] == true {
        return Ok(Asked::Approved {
            prompt: id(&confirmed, "prompt"),
            reaction: id(&confirmed, "reaction"),
        });
    }
    let mut body = json!({"op": "ask", "ref": reference, "host": host, "agent": agent});
    for field in ["verb", "site", "item"] {
        body[field] = summary[field].clone();
    }
    let asked = plow.approval(&body).await?;
    Ok(Asked::Waiting {
        prompt: id(&asked, "prompt"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_fill_is_named_whole_by_a_line() {
        let fill = json!({"kind": "vault_fill", "site": "amazon.com",
                          "item": {"kind": "login", "title": "AWS root"}, "fields": ["password"]});
        assert_eq!(
            summary(&fill).unwrap(),
            json!({"verb": "fill login", "site": "amazon.com", "item": "AWS root"})
        );
        for blind in [
            json!({"kind": "open_site", "url": "http://localhost:2375/containers/json"}),
            json!({"operation": "exec", "argv": ["rm", "-rf", "/"]}),
            json!({"kind": "run_script", "site": "amazon.com", "script": "1"}),
            json!({"kind": "upload_file", "site": "amazon.com", "file": {"name": "a", "size": 1}}),
            json!({"kind": "browser_plan", "page": {"url": "https://a.example/", "title": ""}, "steps": []}),
            json!({"operation": "file_put", "path": "/x"}),
            json!({"kind": "vault_fill", "site": null, "item": {"kind": "login", "title": "x"}}),
        ] {
            assert_eq!(summary(&blind), None, "{blind}");
        }
    }

    #[test]
    fn a_reference_names_the_exact_action_its_agent_and_its_host() {
        let fill = |item: &str| {
            json!({"kind": "vault_fill", "site": "amazon.com",
                   "item": {"kind": "login", "title": item}, "fields": ["password"]})
        };
        let one = reference("mba", "agent-a", &fill("AWS root"));
        assert_eq!(one, reference("mba", "agent-a", &fill("AWS root")));
        for other in [
            reference("mba", "agent-a", &fill("AWS billing")),
            reference("mba", "agent-b", &fill("AWS root")),
            reference("mbp", "agent-a", &fill("AWS root")),
        ] {
            assert_ne!(one, other);
        }
    }
}
