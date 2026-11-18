//! The owner's OK by a reaction in Swarm. When this host's settings turn it
//! on (`swarm_approvals`), an agent's action that the owner's rules and
//! chat left uncovered is posted, through Plow, to the owner's Swarm
//! approval room as one short line. Its reference binds that line to the
//! exact action: the sha256 of the host, the agent, the line's fields and
//! the action's digest, which Plow's function computes itself from what it
//! posts and what it is asked to confirm. A line whose text is not this
//! action's has a reference the gate never confirms. The owner's ✅ on the
//! line, there, within 30 minutes, allows that action once: Plow checks the
//! reaction, and latch takes its answer from Plow only, never from the agent.
//!
//! Only an action the line names whole is asked: filling a saved login, which
//! goes only onto the login's saved sites (what, which site, which item, by
//! its id). A card or an identity may fill into a frame of another site,
//! which no line names.
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
    if action["kind"] != "vault_fill" || action["item"]["kind"] != "login" {
        return None;
    }
    let site = action["site"].as_str()?;
    let item = action["item"]["title"].as_str()?;
    let id = action["item"]["id"].as_str()?;
    Some(json!({"verb": "fill login", "site": site, "item": format!("{item} ({id})")}))
}

/// The digest of `action`, as the judge was shown it.
pub fn digest(action: &Value) -> String {
    sha256(&action.to_string())
}

/// The reference of the line `summary` for an action of `digest` by agent
/// `agent` on host `host`: what the owner's ✅ on that line allows, and
/// nothing else. Plow's function computes the same from the same fields.
pub fn reference(host: &str, agent: &str, summary: &Value, digest: &str) -> String {
    let fields = [
        &json!(host),
        &json!(agent),
        &summary["verb"],
        &summary["site"],
        &summary["item"],
        &json!(digest),
    ];
    sha256(&serde_json::to_string(&fields).expect("fields serialize"))
}

/// How an approval call went.
pub enum Asked {
    /// The owner's ✅ allows the action, this once: the request's and the
    /// reaction's event ids.
    Approved { prompt: String, reaction: String },
    /// The request is posted, waiting for the owner: its event id.
    Waiting { prompt: String },
}

/// Whether the owner's ✅ already allows the line `summary` for an action
/// of `digest`; if not, the request posted for it (Plow posts each line
/// once while it is live).
pub async fn ask(
    plow: &Plow,
    host: &str,
    agent: &str,
    summary: &Value,
    digest: &str,
) -> Result<Asked, Failure> {
    let body = |op: &str| {
        json!({"op": op, "host": host, "agent": agent, "verb": summary["verb"],
               "site": summary["site"], "item": summary["item"], "digest": digest})
    };
    let confirmed = plow.approval(&body("confirm")).await?;
    let id = |answer: &Value, key: &str| answer[key].as_str().unwrap_or_default().to_string();
    if confirmed["approved"] == true {
        return Ok(Asked::Approved {
            prompt: id(&confirmed, "prompt"),
            reaction: id(&confirmed, "reaction"),
        });
    }
    let asked = plow.approval(&body("ask")).await?;
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
                          "item": {"id": "v12", "revision": 3, "kind": "login", "title": "AWS root"},
                          "fields": ["password"]});
        assert_eq!(
            summary(&fill).unwrap(),
            json!({"verb": "fill login", "site": "amazon.com", "item": "AWS root (v12)"})
        );
        for blind in [
            json!({"kind": "open_site", "url": "http://localhost:2375/containers/json"}),
            json!({"operation": "exec", "argv": ["rm", "-rf", "/"]}),
            json!({"kind": "run_script", "site": "amazon.com", "script": "1"}),
            json!({"kind": "upload_file", "site": "amazon.com", "file": {"name": "a", "size": 1}}),
            json!({"kind": "browser_plan", "page": {"url": "https://a.example/", "title": ""}, "steps": []}),
            json!({"operation": "file_put", "path": "/x"}),
            json!({"kind": "vault_fill", "site": null, "item": {"id": "v1", "kind": "login", "title": "x"}}),
            json!({"kind": "vault_fill", "site": "shop.example", "item": {"id": "v2", "kind": "card", "title": "Visa"}}),
            json!({"kind": "vault_fill", "site": "shop.example", "item": {"id": "v3", "kind": "identity", "title": "Me"}}),
            json!({"kind": "vault_fill", "site": "amazon.com", "item": {"kind": "login", "title": "no id"}}),
        ] {
            assert_eq!(summary(&blind), None, "{blind}");
        }
    }

    #[test]
    fn a_reference_binds_the_line_to_the_exact_action_its_agent_and_its_host() {
        let fill = |item: &str| {
            json!({"kind": "vault_fill", "site": "amazon.com",
                   "item": {"kind": "login", "title": item}, "fields": ["password"]})
        };
        let line = |item: &str| json!({"verb": "fill login", "site": "amazon.com", "item": item});
        let root = digest(&fill("AWS root"));
        let one = reference("mba", "agent-a", &line("AWS root"), &root);
        assert_eq!(one, reference("mba", "agent-a", &line("AWS root"), &root));
        for other in [
            reference(
                "mba",
                "agent-a",
                &line("AWS root"),
                &digest(&fill("AWS billing")),
            ),
            reference("mba", "agent-a", &line("Test"), &root),
            reference("mba", "agent-b", &line("AWS root"), &root),
            reference("mbp", "agent-a", &line("AWS root"), &root),
        ] {
            assert_ne!(one, other);
        }
        // The vector Plow's function is tested against (test_handler.py):
        // both sides must hash the same fields to the same reference.
        assert_eq!(
            reference("mba", "agt_1", &line("AWS root"), &"d".repeat(64)),
            "722286032ea8ea14a0ccb96c6e241649369442041e5e36f66bcd563e17b64421"
        );
    }
}
