// The main window, as the Mac app's (gui/Sources/PlowLatchUI/Main): a
// sidebar of the account's pages and its hosts, and for the host picked,
// its screens under a bar of tabs, with an inspector beside the screens
// that have one (Activity, Vault). Every page reads latch and keeps
// nothing: what this module holds is only what the pages last read.
//
// A page is a module in pages/ with `render(app, host)` (its nodes), and
// optionally `enter(app, host)` (reads when it shows), `leave(app)` (when
// another place shows: a stream it follows ends), `inspector(app, host)`
// (the pane's nodes, or null) and `search` (the bar's placeholder).
// latch.js's latchFile() shows a file latch writes, and pickFile() asks
// for one.

import { latch, latchLines, follow, said } from "./latch.js";
import { el, icon, clock, sameDay } from "./dom.js";
import * as home from "./pages/home.js";
import * as activity from "./pages/activity.js";
import * as rules from "./pages/rules.js";
import * as vault from "./pages/vault.js";
import * as settings from "./pages/settings.js";
import * as agents from "./pages/agents.js";
import * as connections from "./pages/connections.js";
import * as appSettings from "./pages/app-settings.js";

const SCREENS = [
  ["home", "Home", home],
  ["activity", "Activity", activity],
  ["rules", "Rules", rules],
  ["vault", "Vault", vault],
  ["settings", "Host Settings", settings],
];
const ACCOUNT = [
  ["agents", "Agents", "agents", agents],
  ["connections", "Connections", "connections", connections],
  // The Mac app's Settings window (⌘,): here a page, from the sidebar's foot.
  ["app-settings", "Settings", "settings", appSettings],
];

export const app = {
  /** `status --local`, followed. */
  local: null,
  localError: null,
  /** `status`: the account and its hosts. */
  account: null,
  accountError: null,
  hostName: null,
  /** A host screen ("home"…) or an account page ("agents"…). */
  place: "home",
  /** Per host: what its pages read, each `{ value }` or `{ error }`. */
  summaries: {},
  /** Per place: whether its inspector shows, and what its search holds. */
  inspecting: { activity: true, vault: true },
  search: {},
  /** Anything a page keeps while it shows (a selection, a draft…), by page. */
  pages: {},
  draw,
  drawPage,
  go,
  host,
  hosts,
  on,
  read,
  sheet,
  confirm,
};

let root;

/** Opens the main window in `into`, sized as the Mac's. In fake mode the
 * address can name what shows: `?place=vault&host=pi-kitchen&inspect=0`. */
export async function open(into) {
  root = into;
  const window_ = window.__TAURI__?.window?.getCurrentWindow?.();
  const { LogicalSize } = window.__TAURI__?.dpi ?? {};
  if (window_ && LogicalSize) {
    await window_.setMinSize(new LogicalSize(760, 480));
    await window_.setSize(new LogicalSize(1120, 700));
    await window_.center();
  }
  const asked = new URLSearchParams(location.search);
  follow("main-status", ["status", "--local", "--follow"], (line) => {
    if (line.type !== "status") return;
    app.local = line.status;
    app.localError = null;
    draw({ page: false });
  }, (failure) => {
    app.localError = failure;
    draw({ page: false });
  });
  await readAccount();
  setInterval(readAccount, 30_000);
  if (asked.get("host")) app.hostName = asked.get("host");
  if (asked.get("inspect")) app.inspecting[asked.get("place")] = asked.get("inspect") !== "0";
  if (asked.get("search")) app.search[asked.get("place")] = asked.get("search");
  go(asked.get("place") ?? app.place);
}

async function readAccount() {
  try {
    app.account = await latch(["status"]);
    app.accountError = null;
  } catch (failure) {
    app.accountError = failure;
  }
  app.hostName ??= hosts()[0]?.name ?? null;
  draw({ page: false });
}

/** The arguments that point a command at `host`: none for this PC. */
function on(host) {
  return host?.thisPC ? [] : ["--host", host.name];
}

/** Reads `args` into `app.summaries[host][key]` as `{ value }` or `{ error }`;
 * `host` null keeps it under the account. */
async function read(host, key, args, { lines = false } = {}) {
  const summary = (app.summaries[host?.name ?? ""] ??= {});
  try {
    summary[key] = { value: await (lines ? latchLines(args) : latch(args)) };
  } catch (error) {
    summary[key] = { error };
  }
  return summary[key];
}

// ── hosts ─────────────────────────────────────────────────────────────────

/** The account's hosts, this PC first, then by name (`HostEntry.list`). */
export function hosts() {
  const listed = (app.account?.hosts ?? []).map((h) => ({
    name: h.name, key: h.key, thisPC: h.self, online: h.online,
    lastSeen: h.last_seen_at ? new Date(h.last_seen_at) : null,
  }));
  const local = app.local;
  if (local?.login && (local.this_machine.hosting.running || !app.account) && !listed.some((h) => h.thisPC)) {
    const name = local.this_machine.name;
    listed.splice(0, listed.length, ...listed.filter((h) => h.name !== name));
    listed.push({ name, key: local.this_machine.key, thisPC: true, online: null, lastSeen: null });
  }
  return listed.sort((a, b) => (b.thisPC - a.thisPC) || a.name.localeCompare(b.name));
}

function host() {
  return hosts().find((h) => h.name === app.hostName) ?? null;
}

/** A host's state in a word or two (`HostState`). */
export function hostState(entry) {
  if (entry.thisPC) {
    const serve = app.local?.this_machine.hosting;
    if (!serve) return { title: "Checking…" };
    if (serve.blocked) return { title: "Can't be used", problem: true };
    switch (serve.state) {
      case "serving": return serve.ready ? { title: "Serving", up: true } : { title: "Not ready" };
      case "starting": return { title: "Starting…" };
      case "error": return { title: "Stopped after an error", problem: true };
      default: return { title: "Stopped" };
    }
  }
  if (entry.online === true) return { title: "Online", up: true };
  if (entry.online === false) return { title: "Offline" };
  return { title: "Checking…" };
}

/** When Plow last heard from another host: "seen 7:06 PM". */
export function seen(entry) {
  if (!entry.lastSeen) return null;
  if (sameDay(entry.lastSeen, new Date())) return `seen ${clock(entry.lastSeen)}`;
  const day = entry.lastSeen.toLocaleDateString([], { month: "short", day: "numeric" });
  return `seen ${day}, ${clock(entry.lastSeen)}`;
}

export function subtitle(entry) {
  const state = hostState(entry).title;
  if (entry.thisPC) return `This PC · ${state}`;
  return seen(entry) ? `${state} · ${seen(entry)}` : state;
}

// ── drawing ───────────────────────────────────────────────────────────────

const isAccount = (place) => ACCOUNT.some(([id]) => id === place);
const pageOf = (place) => (SCREENS.find(([id]) => id === place) ?? ACCOUNT.find(([id]) => id === place))?.at(-1);

function go(place) {
  // Even the same place again (another host's): what it follows ends.
  pageOf(app.place)?.leave?.(app);
  app.place = place;
  draw();
  const page = pageOf(place);
  const entry = host();
  if (page?.enter && (entry || isAccount(place))) page.enter(app, entry).then(() => drawPage());
}

function draw({ page = true } = {}) {
  if (!root) return;
  const entry = host();
  const [settingsId, settingsTitle, settingsIcon] = ACCOUNT.at(-1);
  const sidebar = el("nav", { class: "sidebar", "aria-label": "Account and hosts" },
    el("h2", {}, "Your account"),
    ACCOUNT.slice(0, -1).map(([id, title, symbol]) => row(icon(symbol), title, null, app.place === id, () => go(id))),
    el("h2", {}, "Hosts"),
    hosts().map((h) => row(icon(h.thisPC ? "pc" : "server"), h.name, subtitle(h),
      !isAccount(app.place) && app.hostName === h.name, () => { app.hostName = h.name; go(isAccount(app.place) ? "home" : app.place); })),
    hosts().length ? null : el("p", { class: "quiet" }, app.accountError ? "Not listed" : "Checking…"),
    app.accountError ? el("p", { class: "notice" }, icon("offline"), "Plow can't be reached") : null,
    el("div", { class: "foot" },
      row(icon(settingsIcon), settingsTitle, null, app.place === settingsId, () => go(settingsId)),
      el("div", { class: "who" }, app.local?.login ? `Logged in as ${app.local.this_machine.name}` : "")),
  );
  // Swapped in place: a page being typed in keeps its focus while the
  // status and hosts around it change.
  const old = root.querySelector(".sidebar");
  if (old) old.replaceWith(sidebar);
  else root.prepend(sidebar);
  const content = root.querySelector(".content");
  if (!content) root.append(el("section", { class: "content" }, header(entry), body(entry)));
  else if (page) content.replaceChildren(header(entry), body(entry));
  else content.querySelector(".head").replaceWith(header(entry));
}

/** Draws the page (and its inspector) again, the header and sidebar kept:
 * what a page calls when what it shows changes. */
function drawPage() {
  const shown = root?.querySelector(".content .body");
  if (shown) shown.replaceWith(body(host()));
  else draw();
}

function row(glyph, title, detail, selected, onclick) {
  return el("button", { class: `nav${selected ? " selected" : ""}`, onclick, "aria-current": selected ? "page" : false },
    glyph, el("span", { class: "label" }, el("span", {}, title), detail ? el("small", {}, detail) : null));
}

function header(entry) {
  const page = pageOf(app.place);
  const tools = el("div", { class: "tools" }, searchField(page), inspectorToggle(page));
  if (isAccount(app.place)) {
    return el("header", { class: "head" }, el("div", { class: "title" }, ACCOUNT.find(([id]) => id === app.place)[1]), tools);
  }
  return el("header", { class: "head" },
    el("div", { class: "title" }, entry?.name ?? "Latch"),
    entry ? el("div", { class: "subtitle" }, subtitle(entry)) : null,
    el("div", { class: "tabs", role: "tablist" },
      SCREENS.map(([id, title]) => el("button", {
        role: "tab", "aria-selected": String(app.place === id), class: app.place === id ? "on" : "",
        onclick: () => go(id), disabled: !entry,
      }, title))),
    tools,
  );
}

/** The bar's search field, kept across redraws so typing in it goes on. */
function searchField(page) {
  if (!page?.search) return null;
  const kept = root.querySelector(`.head input.search[data-place="${app.place}"]`);
  if (kept) return kept.parentElement;
  const input = el("input", {
    class: "search", type: "search", placeholder: page.search, "data-place": app.place,
    value: app.search[app.place] ?? "", "aria-label": page.search,
    oninput: (event) => { app.search[app.place] = event.target.value; drawPage(); },
  });
  return el("label", { class: "search-box" }, icon("search"), input);
}

function inspectorToggle(page) {
  if (!page?.inspector) return null;
  const shown = app.inspecting[app.place];
  return el("button", {
    class: `inspect${shown ? " on" : ""}`, title: shown ? "Hide Inspector" : "Show Inspector",
    "aria-pressed": String(Boolean(shown)),
    onclick: () => { app.inspecting[app.place] = !shown; draw(); },
  }, icon("inspector"));
}

function body(entry) {
  const page = pageOf(app.place);
  const view = el("div", { class: "page" });
  if (!entry && !isAccount(app.place)) view.append(el("p", { class: "quiet center" }, app.accountError ? said(app.accountError) : "Checking your hosts…"));
  else view.append(...[page.render(app, entry)].flat().filter(Boolean));
  const pane = page?.inspector && app.inspecting[app.place] && (entry || isAccount(app.place)) ? page.inspector(app, entry) : null;
  return el("div", { class: `body${pane ? " inspected" : ""}` }, view,
    pane ? el("aside", { class: "inspector", "aria-label": "Inspector" }, ...[pane].flat().filter(Boolean)) : null);
}

// ── sheets ────────────────────────────────────────────────────────────────

/** Shows `content` in a sheet over the window (the Mac's sheets); returns
 * its close. Escape closes it, as Cancel does. */
function sheet(content, { width = 480 } = {}) {
  const dialog = el("dialog", { class: "sheet", style: `width: ${width}px` }, ...[content].flat());
  const close = () => { dialog.close(); dialog.remove(); };
  dialog.addEventListener("cancel", (event) => { event.preventDefault(); close(); });
  root.append(dialog);
  dialog.showModal();
  return close;
}

/** Asks before something that can't be undone: resolves to whether it may. */
function confirm(title, message, action, { destructive = true } = {}) {
  return new Promise((resolve) => {
    let close;
    const answer = (yes) => { close(); resolve(yes); };
    close = sheet([
      el("h2", {}, title),
      message ? el("p", {}, message) : null,
      el("div", { class: "actions" },
        el("button", { onclick: () => answer(false) }, "Cancel"),
        el("button", { class: destructive ? "destructive" : "primary", onclick: () => answer(true) }, action)),
    ], { width: 420 });
  });
}
