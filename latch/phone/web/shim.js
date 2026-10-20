// The phone page's shim. It reads the link after `#` (which never reaches
// the server that served this file), dials the latch host through its
// relay, and shows the form the host sends in a sandboxed frame. The form
// talks to the host only through this file.
import init, { connect } from "./latch_phone.js";

const status = document.getElementById("status");
const frame = document.getElementById("page");
const link = new URLSearchParams(location.hash.slice(1));
const code = link.get("c");
const hostKey = link.get("h");

function say(text, bad) {
  status.textContent = text;
  status.className = bad ? "bad" : "";
}

// Machines this phone has reached before, by key, so a key it has seen
// under that name before says so.
function remember(name) {
  let seen = {};
  try { seen = JSON.parse(localStorage.getItem("latch-hosts") || "{}"); } catch {}
  const before = seen[hostKey];
  try {
    seen[hostKey] = name;
    localStorage.setItem("latch-hosts", JSON.stringify(seen));
  } catch {}
  return before;
}

async function main() {
  if (!code || !hostKey || !link.get("r")) {
    return say("This link is incomplete. Open the whole link from latch.", true);
  }
  await init();
  const host = await connect(hostKey, link.get("r"));
  const call = async (op, args) => JSON.parse(await host.call(JSON.stringify({ op, args: { code, ...args } })));

  const opened = await call("page", {});
  if (opened.error) return say(opened.error.message, true);
  const request = opened.ok.request;
  const before = remember(request.host);
  const short = hostKey.slice(0, 8) + "…" + hostKey.slice(-4);
  say(`Connected to ${request.host}, key ${short}: ` +
    (before === request.host ? "the same machine as last time." : "first time on this phone."));

  window.addEventListener("message", async (event) => {
    if (event.source !== frame.contentWindow) return;
    const message = event.data;
    if (message.type === "ready") {
      frame.contentWindow.postMessage({ type: "request", request }, "*");
    }
    if (message.type === "submit") {
      let answer;
      try {
        answer = await call("submit", { values: message.values });
      } catch (err) {
        answer = { error: { code: "connection_lost", message: "The connection dropped; check on the machine before trying again." } };
      }
      frame.contentWindow.postMessage(answer.error
        ? { type: "result", ok: false, code: answer.error.code, message: answer.error.message }
        : { type: "result", ok: true, host: request.host }, "*");
    }
  });
  frame.srcdoc = opened.ok.html;
  frame.hidden = false;
}

main().catch((err) => say("Could not reach the machine: " + err, true));
