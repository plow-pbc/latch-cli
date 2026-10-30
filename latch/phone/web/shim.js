// The phone page's shim. It reads the link after `#` (which never reaches
// the server that served this file), dials the latch host through its
// relay, and shows the form the host sends in a sandboxed frame. The form
// talks to the host only through this file.
import init, { connect } from "./latch_phone.js";

// Opening another link in the same tab changes only what follows `#`,
// which does not reload the page: start over for the new link.
window.addEventListener("hashchange", () => location.reload());

const status = document.getElementById("status");
const frame = document.getElementById("page");
const link = new URLSearchParams(location.hash.slice(1));
const code = link.get("c");
// An agent's link names its request instead of carrying a code: the owner
// signs in to Plow here, and Plow vouches for this page's key for it.
const requestId = link.get("l");
const plow = link.get("p") || "https://api.plow.co";
const hostKey = link.get("h");

function say(text, bad) {
  status.textContent = text;
  status.className = bad ? "bad" : "";
}

// The computer's name, as it gives it over the connection. Nothing here
// checks that the computer is the owner's: the name and the website are
// what the person checks.
function connected(name, before) {
  status.replaceChildren("Connecting to a computer named ", Object.assign(document.createElement("b"), { textContent: name }), ". ",
    before === name ? "This phone has reached it under this name before." : "This phone has not reached it before.");
  status.className = "";
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

// Signs the owner in to Plow with a code Plow texts them, and has Plow
// vouch for this page's `key`, for this request only. No login is kept.
function signIn(key) {
  const form = document.getElementById("signin");
  const phone = document.getElementById("phone");
  const otp = document.getElementById("otp");
  const note = document.getElementById("signin-note");
  const tell = (text, bad) => { note.textContent = text; note.className = bad ? "note bad" : "note"; };
  const post = (path, body) => fetch(plow + path, {
    method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body),
  });
  say("Sign in to Plow to see what this request is for.");
  form.hidden = false;
  phone.focus();
  document.getElementById("send").addEventListener("click", async () => {
    if (!phone.reportValidity()) return;
    tell("Texting a code…");
    try {
      await post("/v1/auth/otp/request", { phone: phone.value });
    } catch {
      return tell("Could not reach Plow; try again.", true);
    }
    document.getElementById("entry").hidden = false;
    tell("If that number is on a Plow account, a code is on its way. It lasts 5 minutes.");
    otp.focus();
  });
  return new Promise((resolve) => {
    form.addEventListener("submit", async (event) => {
      event.preventDefault();
      tell("Signing in…");
      let answer;
      try {
        answer = await post("/v1/latch/phone-grants", { phone: phone.value, code: otp.value.trim(), key, request: requestId });
      } catch {
        return tell("Could not reach Plow; try again.", true);
      }
      if (!answer.ok) return tell("That code didn't work. Check it, or text yourself a new one.", true);
      form.hidden = true;
      otp.value = "";
      resolve();
    });
  });
}

async function main() {
  if ((!code && !requestId) || !hostKey || !link.get("r")) {
    return say("This link is incomplete. Open the whole link from latch.", true);
  }
  await init();
  const host = await connect(hostKey, link.get("r"));
  const which = code ? { code } : { link: requestId };
  const call = async (op, args) => JSON.parse(await host.call(JSON.stringify({ op, args: { ...which, ...args } })));
  if (!code) await signIn(host.key());

  const opened = await call("page", {});
  if (opened.error) return say(opened.error.message, true);
  const request = opened.ok.request;
  connected(request.host, remember(request.host));
  document.getElementById("what").textContent = request.kind === "login" ? "Your password" : "What you type here";
  for (const el of document.querySelectorAll(".host")) el.textContent = request.host;
  document.getElementById("privacy").hidden = false;

  window.addEventListener("message", async (event) => {
    if (event.source !== frame.contentWindow) return;
    const message = event.data;
    if (message.type === "ready") {
      frame.contentWindow.postMessage({ type: "request", request }, "*");
    }
    if (message.type === "submit") {
      let answer;
      try {
        answer = await call("submit", { values: message.values, title: message.title, sites: message.sites });
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
