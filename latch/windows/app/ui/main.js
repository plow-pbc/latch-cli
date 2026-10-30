// The main window, as the Mac app's (gui/Sources/PlowLatchUI/Main): a
// sidebar of the account's pages and its hosts, and for the host picked,
// its screens under a bar of tabs, with an inspector beside the screens
// that have one (Activity, Vault). Every page reads latch and keeps
// nothing: what this module holds is only what the pages last read.
//
// A page is a module in pages/ with `render(app, host)` (its nodes), and
// optionally `enter(app, host)` (reads when it shows), `leave(app)` (when
// another place shows: a stream it follows ends), `inspector(app, host)`
// (the pane's nodes, or null), `search` (the bar's placeholder),
// `followsStatus` (drawn again when this PC's status changes) and
// `forget(app)` (another account logged in: what it keeps for the last ends).
// latch.js's latchFile() shows a file latch writes, and pickFile() asks
// for one.

import { latch, latchLines, follow, said } from "./latch.js";
import { el, icon, clock, sameDay } from "./dom.js";
import * as home from "./pages/home.js";
import * as activity from "./pages/activity.js";

const SCREENS = [
  ["home", "Home", home],
  ["activity", "Activity", activity],
];
const ACCOUNT = [
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
  readAccount,
  host,
  hosts,
  on,
  read,
  sheet,
  confirm,
  viewerOnly,
  askToHost,
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
  // Set up again opens the window again: what runs for it is started once.
  if (!opened) {
    opened = true;
    followLocal();
    setInterval(readAccount, 30_000);
    document.addEventListener("keydown", shortcut);
  }
  await readAccount();
  if (asked.get("host")) app.hostName = asked.get("host");
  if (asked.get("inspect")) app.inspecting[asked.get("place")] = asked.get("inspect") !== "0";
  if (asked.get("search")) app.search[asked.get("place")] = asked.get("search");
  go(asked.get("place") ?? app.place);
}

let opened = false;

function followLocal() {
  follow("main-status", ["status", "--local", "--follow"], (line) => {
    if (line.type !== "status") return;
    app.local = line.status;
    app.localError = null;
    draw({ page: false });
    if (pageOf(app.place)?.followsStatus) drawPage();
  }, (failure) => {
    app.localError = failure;
    draw({ page: false });
    if (pageOf(app.place)?.followsStatus) drawPage();
  });
}

/** `status`: the account and its hosts, read again every 30 seconds. */
async function readAccount() {
  const was = { account: JSON.stringify(app.account), error: Boolean(app.accountError), host: host() };
  let account = app.account;
  try {
    account = await latch(["status"]);
    app.accountError = null;
  } catch (failure) {
    app.accountError = failure;
  }
  const owner = (status) => status?.account?.owner_uid ?? null;
  if (app.account && account && owner(account) !== owner(app.account)) {
    forgetAccount();
    app.account = account;
    app.hostName = hosts()[0]?.name ?? null;
    return go("home");
  }
  app.account = account;
  app.hostName ??= hosts()[0]?.name ?? null;
  // Nothing new: nothing is drawn again, so a search being typed keeps
  // its focus.
  if (JSON.stringify(account) === was.account && Boolean(app.accountError) === was.error) return;
  const now = host();
  // A host that came online: its page reads what it couldn't while it was
  // offline, as opening it then would.
  if (was.host?.online === false && now?.online === true && now.name === was.host.name) go(app.place);
  else draw({ page: false });
}

/** Another Plow account than the one read before (logged out, then in as
 * someone else): nothing read for the last one stays, so nothing acts on
 * its hosts, devices or agents by mistake. */
function forgetAccount() {
  pageOf(app.place)?.leave?.(app);
  for (const [, , page] of SCREENS) page.forget?.(app);
  for (const entry of ACCOUNT) entry.at(-1).forget?.(app);
  Object.assign(app, { summaries: {}, pages: {}, search: {}, hostName: null, place: "home" });
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

/** Logged in, Plow answered, and this PC isn't one of the hosts: the
 * person is viewing their hosts from here (`AppModel.viewerOnly`). */
function viewerOnly() {
  return Boolean(app.local?.login && app.account && !hosts().some((h) => h.thisPC));
}

/** Asks, then starts hosting on this PC (the Mac's `.startHosting`). */
async function askToHost() {
  const yes = await confirm("Start hosting on this PC?",
    "Latch runs in the background, and your agents can reach this PC, judged by your rules.", "Start Hosting", { destructive: false });
  if (yes) await app.start?.();
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

/** The Mac app's menu commands, as Windows' Ctrl keys (Main/WindowCommands.swift):
 * Ctrl+1–5 a host's screens, Ctrl+F search, Ctrl+N a new vault item,
 * Ctrl+Alt+I the inspector, Ctrl+Alt+S the sidebar. Rules saves itself with
 * Ctrl+S. Nothing while a sheet is up. */
function shortcut(event) {
  if (!event.ctrlKey || event.metaKey || root.hidden || root.querySelector("dialog[open]")) return;
  const key = event.key.toLowerCase();
  const screen = SCREENS[Number(event.key) - 1];
  if (!event.altKey && screen && host()) go(screen[0]);
  else if (!event.altKey && key === "f") root.querySelector(".head input.search")?.focus();
  else if (!event.altKey && key === "n" && app.place === "vault") {
    [...root.querySelectorAll(".page button")].find((button) => button.textContent.trim() === "New Item")?.click();
  } else if (event.altKey && key === "i" && pageOf(app.place)?.inspector) {
    app.inspecting[app.place] = !app.inspecting[app.place];
    draw();
  } else if (event.altKey && key === "s") {
    root.classList.toggle("no-sidebar");
  } else return;
  event.preventDefault();
}

function draw({ page = true } = {}) {
  if (!root) return;
  const entry = host();
  // The account's pages, and the app's Settings at the sidebar's foot.
  const settingsPage = ACCOUNT.find(([id]) => id === "app-settings");
  const account = ACCOUNT.filter((entry) => entry !== settingsPage);
  const sidebar = el("nav", { class: "sidebar", "aria-label": "Account and hosts" },
    account.length ? el("h2", {}, "Your account") : null,
    account.map(([id, title, symbol]) => row(icon(symbol), title, null, app.place === id, () => go(id))),
    el("h2", {}, "Hosts"),
    hosts().map((h) => row(icon(h.thisPC ? "pc" : "server"), h.name, subtitle(h),
      !isAccount(app.place) && app.hostName === h.name, () => { app.hostName = h.name; go(isAccount(app.place) ? "home" : app.place); })),
    hosts().length ? null : el("p", { class: "quiet" }, viewerOnly() ? "No hosts yet" : app.accountError ? "Not listed" : "Checking…"),
    app.accountError ? el("p", { class: "notice" }, icon("offline"), "Plow can't be reached") : null,
    viewerOnly() ? el("button", { class: "host-here", title: "This PC isn't hosting; you're viewing your hosts", onclick: askToHost },
      "Start Hosting on This PC…") : null,
    el("div", { class: "foot" },
      settingsPage && row(icon(settingsPage[2]), settingsPage[1], null, app.place === "app-settings", () => go("app-settings")),
      el("div", { class: "who" }, app.local?.login
        ? `Logged in as ${app.local.this_machine.name}${viewerOnly() ? " · viewing only" : ""}` : "")),
  );
  // Swapped in place: a page being typed in keeps its focus while the
  // status and hosts around it change.
  const old = root.querySelector(".sidebar");
  if (old) old.replaceWith(sidebar);
  else root.prepend(sidebar);
  const content = root.querySelector(".content");
  if (!content) root.append(el("section", { class: "content" }, header(entry), body(entry)));
  else keepingFocus(() => {
    if (page) content.replaceChildren(header(entry), body(entry));
    else content.querySelector(".head").replaceWith(header(entry));
  });
}

/** Runs `change`, then gives focus back to what had it, if it is still
 * shown: the search field moves into each new header, and moving it would
 * end typing in it, every 30 seconds when the hosts are read again. */
function keepingFocus(change) {
  const focused = document.activeElement;
  const range = typeof focused?.selectionStart === "number" ? [focused.selectionStart, focused.selectionEnd] : null;
  change();
  if (focused && focused !== document.body && focused !== document.activeElement && focused.isConnected) {
    focused.focus({ preventScroll: true });
    if (range) focused.setSelectionRange(...range);
  }
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
    return el("header", { class: "head" }, el("div", { class: "title" }, ACCOUNT.find(([id]) => id === app.place)[1]),
      app.place === "app-settings" ? null : el("div", { class: "subtitle" }, "Your account"), tools);
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
  // Home speaks for a viewer with no host yet; other screens need a host.
  if (!entry && !isAccount(app.place) && !(app.place === "home" && viewerOnly())) {
    view.append(el("p", { class: "quiet center" }, app.accountError ? said(app.accountError) : "Checking your hosts…"));
  }
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
