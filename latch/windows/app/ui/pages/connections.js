// Your account › Connections, as the Mac app's (Main/ConnectionsView.swift):
// the Google accounts connected in Plow, the same whatever host is picked,
// with the one sign-in they offer (latch's `plugins enable google --events`,
// followed while its sheet shows), and the account's logged-in devices,
// each of which can be removed.

import { el, button } from "../dom.js";
import { latch, follow, unfollow, openURL } from "../latch.js";
import { errorText, glyph, ring, timestamp } from "./agents.js";
import { unreachable } from "../errors.js";
import { accountsOf, needs, addsAccount, noneWorks, RECONNECT, signInProblem } from "../google.js";

/** What the page keeps: what `setup --status` said of Google (undefined
 * until it has said; "unknown" when Plow couldn't say), `latch devices`,
 * and why a removal failed. */
function kept(app) {
  return (app.pages.connections ??= { google: undefined, devices: null, removeError: null });
}

function redraw(app) {
  if (app.place === "connections") app.drawPage();
}

let activated = false;

/** Reads what isn't held yet: Google's accounts, then the devices. */
export async function enter(app) {
  shownApp = app;
  if (!activated) {
    activated = true;
    // The app came to the front: a Google account connected in the browser
    // shows (the Mac's `activated()`).
    window.addEventListener("focus", async () => {
      if (app.place !== "connections") return;
      await readGoogle(app);
      redraw(app);
      await readDevices(app);
      redraw(app);
    });
  }
  const page = kept(app);
  if (page.google === undefined) {
    await readGoogle(app);
    redraw(app);
  }
  if (!page.devices?.value) await readDevices(app);
  if (staged) {
    showSignIn(app, staged);
    staged = null;
  }
}

/** Another place shows: a sign-in still waiting stops. */
export function leave() {
  endSignIn();
}

async function readGoogle(app) {
  let setup = null;
  try {
    setup = await latch(["setup", "--status"]);
  } catch {}
  kept(app).google = accountsOf(setup?.steps?.find((step) => step.step === "google")) ?? "unknown";
}

async function readDevices(app) {
  try {
    kept(app).devices = { value: (await latch(["devices"])).devices };
  } catch (error) {
    kept(app).devices = { error };
  }
}

// ── Google's accounts (the Mac's `GoogleAccounts`) ────────────────────────

function stateOf(google) {
  if (!google.accounts.length) return "Not connected";
  return noneWorks(google) ? "Needs sign-in" : "Connected";
}

/** The section's own button, while some or no account works. */
function buttonTitle(google) {
  if (noneWorks(google)) return null;
  return addsAccount(google) ? "Add Another Account…" : "Connect Google…";
}

/** Some work, so latch turns Google on without waiting for this one: the
 * owner reconnects it in Plow, as latch tells agents. */

// ── the page ──────────────────────────────────────────────────────────────

export function render(app) {
  const page = kept(app);
  return el("div", { class: "connections-page" }, google(app, page), devices(app, page));
}

/** Google: its state, every account connected in Plow, and the one
 * sign-in it offers. */
function google(app, page) {
  const said = page.google;
  const accounts = said && said !== "unknown" ? said : { accounts: [], needsSignIn: [] };
  const canSignIn = said !== undefined && said !== "unknown";
  const state = said === undefined ? "Checking…" : said === "unknown" ? "Couldn't reach Plow" : stateOf(accounts);
  const signIn = () => connectGoogle(app, addsAccount(accounts));
  const title = buttonTitle(accounts);
  return el("section", { class: "group" },
    el("div", { class: "card" },
      el("div", { class: "line" }, el("span", {}, "Google"), el("span", { class: "value" }, state)),
      accounts.accounts.map((account) => {
        const again = needs(accounts, account);
        return el("div", { class: "line cx-account" },
          el("span", { class: "cx-who" }, glyph("mail"),
            el("span", { class: "cx-name" }, el("span", {}, account),
              again && !noneWorks(accounts) ? el("small", {}, RECONNECT) : null)),
          again ? el("span", { class: "value" }, "Needs sign-in") : null,
          again && noneWorks(accounts) ? el("button", { onclick: signIn, disabled: !canSignIn }, "Sign In Again…") : null);
      }),
      said === undefined ? el("div", { class: "line cx-center" }, ring(true)) : null,
      said === "unknown"
        ? el("div", { class: "line" }, el("span", { class: "cx-quiet" }, "Plow couldn't say which Google accounts are connected."),
          button("Check Again", async () => { await readGoogle(app); redraw(app); }))
        : null,
      canSignIn && title ? el("div", { class: "line" }, el("span"), button(title, signIn)) : null),
    el("p", { class: "cx-foot" }, "Gmail and Google Calendar, for every host where the Google plugin is on. You sign in with Google in your browser."));
}

/** The account's logged-in machines that don't host. */
function devices(app, page) {
  const rows = [];
  if (!page.devices) rows.push(el("div", { class: "line cx-center" }, ring(true)));
  else if (page.devices.error) rows.push(el("div", { class: "line cx-quiet" }, "Plow couldn't list your logged-in devices."));
  else {
    const hostKeys = new Set(app.hosts().map((host) => host.key).filter(Boolean));
    const list = page.devices.value.filter((device) => !hostKeys.has(device.key));
    if (!list.length) rows.push(el("div", { class: "line cx-quiet" }, "None"));
    for (const device of list) rows.push(deviceRow(app, device));
  }
  if (page.removeError) rows.push(el("div", { class: "line cx-bad" }, errorText(page.removeError, { place: "devices" })));
  return el("section", { class: "group" }, el("h3", {}, "Logged-in devices"),
    el("div", { class: "card" }, rows),
    el("p", { class: "cx-foot" }, "Logged-in devices can send requests as you but don't host."));
}

function deviceRow(app, device) {
  const thisPC = device.key === app.local?.this_machine?.key;
  return el("div", { class: "line" },
    el("span", { class: "cx-name" }, el("span", {}, thisPC ? `${device.name} (this PC)` : device.name), el("small", {}, seen(device))),
    el("button", {
      "aria-label": `Remove ${device.name}…`, disabled: thisPC,
      title: thisPC ? "Log out this PC in Settings instead" : `Remove ${device.name} from your account`,
      onclick: () => removeDevice(app, device),
    }, "Remove…"));
}

function seen(device) {
  const state = device.online === true ? "Online" : "Offline";
  const when = device.last_seen_at && timestamp(device.last_seen_at);
  return when ? `${state} · seen ${when.toLocaleString([], { dateStyle: "medium", timeStyle: "short" })}` : state;
}

/** Removes a machine from the account; it's logged out there. A refusal
 * is said under the list. */
async function removeDevice(app, device) {
  const yes = await app.confirm(`Remove “${device.name}” from your account?`,
    `${device.name} is logged out of Plow and can no longer send requests as you.`, "Remove");
  if (!yes) return;
  let failure = null;
  try {
    // latch removes by name: only while that name is still this very
    // machine (its key), in the account logged in now.
    const now = (await latch(["devices"])).devices.find((one) => one.name === device.name);
    if (now?.key !== device.key) throw unreachable(`“${device.name}” isn't the machine shown anymore. The list is up to date now; check it and try again.`);
    await latch(["devices", "remove", device.name, "--yes"]);
  } catch (error) {
    failure = error;
  }
  await readDevices(app);
  kept(app).removeError = failure;
  redraw(app);
}

// ── signing in with Google ────────────────────────────────────────────────

const STREAM = "google-sign-in";
const STOPPED = "Signing in stopped before Google answered. Try again.";

/** The sign-in under way while its sheet shows: connecting a first account
 * or adding another, the newest link latch gave (kept only to open again),
 * and why it ended without an account. */
let signIn = null;
let staged = null;
let shownApp = null;

/** "Connect Google…", "Add Another Account…" or "Sign In Again…": latch
 * gets a sign-in link from Plow (opened in the browser) and waits for the
 * new account, then turns Google on here; the sheet goes once it's on. */
function connectGoogle(app, adding) {
  showSignIn(app, { adding, failure: null, link: null });
  start(app, signIn);
}

function showSignIn(app, sheet) {
  endSignIn();
  const flow = { ...sheet, opened: Boolean(sheet.link) };
  const box = el("div", { class: "cx-signin" });
  flow.draw = () => box.replaceChildren(...signInSheet(app, flow));
  flow.draw();
  signIn = flow;
  flow.close = app.sheet(box, { width: 420 });
  const dialog = box.closest("dialog");
  dialog.classList.add("ag-sheet", "cx-sheet");
  // However it goes (Cancel, Escape), latch stops waiting.
  dialog.addEventListener("close", () => signIn === flow && endSignIn());
}

function start(app, flow) {
  const args = ["plugins", "enable", "google", "--events", ...(flow.adding ? ["--add-account"] : [])];
  follow(STREAM, args, (line) => {
    if (signIn !== flow) return;
    if (line.type === "link" && line.url) {
      flow.link = line.url;
      if (!flow.opened) {
        flow.opened = true;
        open(line.url);
      }
      flow.draw();
    } else if (line.type === "result") {
      // Google is on: the sheet goes, and the page says who's connected.
      signIn = null;
      unfollow(STREAM);
      flow.close();
      readGoogle(app).then(() => redraw(app));
    }
  }, (failure) => {
    if (signIn !== flow) return;
    // Over without an account: no link to open again.
    flow.link = null;
    flow.failure = failure ? signInProblem(failure, flow.adding) : STOPPED;
    flow.draw();
  });
}

/** Cancel, Close, or the sheet going: latch stops waiting. */
function endSignIn() {
  const flow = signIn;
  if (!flow) return;
  signIn = null;
  unfollow(STREAM);
  flow.close?.();
}

/** While latch waits for the sign-in in the browser; it goes by itself
 * once Google is on. The link is a credential until used: it's opened in
 * the browser, never shown. */
function signInSheet(app, flow) {
  const nodes = [el("h2", {}, flow.adding ? "Add a Google Account" : "Connect a Google Account")];
  if (flow.failure) {
    nodes.push(el("p", { class: "cx-said" }, flow.failure));
  } else {
    nodes.push(el("div", { class: "cx-wait" }, ring(true),
      el("span", {}, "Finish signing in with Google in your browser. This closes by itself when you're done.")));
    if (flow.adding) nodes.push(el("p", {}, "Choose an account that isn't connected yet."));
  }
  nodes.push(el("div", { class: "actions" },
    flow.failure ? null : el("button", { disabled: !flow.link, onclick: () => open(flow.link) }, "Open the Sign-In Page Again"),
    el("span", { class: "cx-gap" }),
    flow.failure
      ? [button("Close", endSignIn), el("button", { class: "primary", autofocus: true, onclick: () => retry(app, flow) }, "Try Again")]
      : button("Cancel", endSignIn)));
  return nodes;
}

/** Try Again: the same sheet, a new wait. */
function retry(app, flow) {
  unfollow(STREAM);
  Object.assign(flow, { failure: null, link: null, opened: false });
  flow.draw();
  start(app, flow);
}

/** Opens a sign-in link in the default browser; a failure leaves the
 * sheet's Open Again to try. */
function open(url) {
  if (url) openURL(url).catch(() => {});
}

/** For fake mode's stills (the Mac's `stage(googleSignIn:link:)`): the
 * sign-in sheet as it shows at a point, opened once Connections shows. */
export function stage(sheet) {
  if (shownApp?.place === "connections") showSignIn(shownApp, sheet);
  else staged = sheet;
}
