// A host's vault, as the Mac app's (Main/VaultView.swift): its items, never
// their secrets; an item opens in the inspector (`vault show`), and items
// are added, changed, removed and imported in sheets, each on the host it
// was opened for. A secret is typed fresh and never read back: latch never
// sends one, and a field it marks hidden shows only that it's saved.

import { el, icon, button } from "../dom.js";
import { latch, pickFile } from "../latch.js";
import * as errors from "../errors.js";

export const search = "Search items";

/** What the page keeps: the item selected (on the host it was picked on),
 * what `vault show` said of items (by host, then item), the shows asked for
 * while the inspector shows, and why the last removal failed. */
function kept(app) {
  return (app.pages.vault ??= { selected: null, shows: {}, asked: new Set(), removeError: null, focus: false });
}

// ── kinds ─────────────────────────────────────────────────────────────────

const entry = (label, secret = false) => ({ label, secret });

/** latch's item kinds and the fields a person fills in for each, by latch's
 * labels, with those latch holds secret (`VaultKind`). */
const KINDS = {
  login: { title: "Login", symbol: "key", entries: [entry("username"), entry("password", true), entry("totp", true)] },
  card: {
    title: "Card", symbol: "card",
    entries: [entry("cardholder name"), entry("brand"), entry("number", true), entry("expiry month"), entry("expiry year"), entry("code", true)],
  },
  identity: {
    title: "Identity", symbol: "identity",
    entries: ["title", "first name", "middle name", "last name", "email", "phone", "company", "username",
      "address1", "address2", "address3", "city", "state", "postal code", "country", "date of birth"].map((label) => entry(label))
      .concat([entry("ssn", true), entry("passport number", true), entry("license number")]),
  },
  note: { title: "Note", symbol: "note", entries: [] },
};

const kindTitle = (kind) => KINDS[kind]?.title ?? kind;

/** A field's label in words. */
function fieldTitle(label) {
  const words = {
    totp: "Authenticator key", number: "Card number", code: "Security code", address1: "Address",
    address2: "Address line 2", address3: "Address line 3", ssn: "Social Security number",
  };
  return words[label] ?? label.charAt(0).toUpperCase() + label.slice(1);
}

/** "ada · shop.example", "Note", or "Card · Fills on any site". */
function context(item) {
  switch (item.kind) {
    case "login": return [item.username, item.sites.join(", ")].filter(Boolean).join(" · ");
    case "note": return kindTitle(item.kind);
    default: return `${kindTitle(item.kind)} · ${item.sites.length ? `Fills on ${item.sites.join(", ")}` : "Fills on any site"}`;
  }
}

// ── errors, in errors.js's words ─────────────────────────────────────────

const UNREADABLE = "Latch returned an unreadable response.";
const ITEM_CHANGED_ELSEWHERE = "This item changed since you opened it. Nothing was saved; look at it again, then make your change.";
const CHANGED_SINCE_PREVIEW = "The vault or the export changed since this preview, so nothing was imported. Here it is again: check the rows, then import.";

const { spoken, unreachable } = errors;

/** An error as the app says it (errors.js), naming `host`, in this page's
 * look: Details behind a chevron. */
function errorText(error, host, { titled = false } = {}) {
  return errors.errorText(error, { host: host ? spoken(host) : null, titled, moreClass: "more", chevron: true });
}

// ── icons the shared set doesn't have ─────────────────────────────────────

const GLYPHS = {
  key: '<circle cx="7" cy="7.2" r="3.6"/><path d="M9.6 9.8l6.6 6.6M13.4 13.6l1.8-1.8M15.2 15.4l1.4-1.4"/><circle cx="6" cy="6.2" r=".6"/>',
  card: '<rect x="2.5" y="4.5" width="15" height="11" rx="1.6"/><path d="M2.5 8.2h15M5.5 12.4h3"/>',
  identity: '<rect x="2.5" y="4" width="15" height="12" rx="1.6"/><circle cx="7.2" cy="8.8" r="1.6"/><path d="M4.7 13.2c.5-1.3 1.3-1.9 2.5-1.9s2 .6 2.5 1.9M11.6 8.4h3.4M11.6 11.4h3.4"/>',
  note: '<rect x="3.5" y="3" width="13" height="14" rx="1.6"/><path d="M6.5 7h7M6.5 10h7M6.5 13h4"/>',
  unknown: '<rect x="3" y="3" width="14" height="14" rx="1.6" stroke-dasharray="2.4 2"/><path d="M8.2 8a1.9 1.9 0 1 1 2.6 1.8c-.5.2-.8.6-.8 1.1v.4M10 13.6v.01"/>',
  shield: '<path d="M10 2.8l5.8 2.2v4.4c0 3.6-2.5 6.3-5.8 7.6-3.3-1.3-5.8-4-5.8-7.6V5z"/><rect x="7.6" y="9" width="4.8" height="3.6" rx=".6"/><path d="M8.6 9V7.9a1.4 1.4 0 0 1 2.8 0V9"/>',
  plus: '<path d="M10 4v12M4 10h12"/>',
  import: '<path d="M10 3v9M6.5 8.5L10 12l3.5-3.5"/><path d="M3.5 12.5v3a1 1 0 0 0 1 1h11a1 1 0 0 0 1-1v-3"/>',
  vault: '<path d="M5.5 3h9M4.2 5.2h11.6"/><rect x="3" y="7.4" width="14" height="9.6" rx="1.6"/><rect x="8" y="11.6" width="4" height="3" rx=".5"/><path d="M8.8 11.6v-1a1.2 1.2 0 0 1 2.4 0v1"/>',
  lockRect: '<rect x="2.5" y="4" width="15" height="12" rx="1.6"/><rect x="7.8" y="9.4" width="4.4" height="3.6" rx=".5"/><path d="M8.7 9.4V8.3a1.3 1.3 0 0 1 2.6 0v1.1"/>',
  done: '<circle cx="10" cy="10" r="7"/><path d="M6.8 10.2l2.2 2.2 4.2-4.6"/>',
  pencil: '<path d="M12.8 3.8l3.4 3.4L7.4 16H4v-3.4z"/><path d="M11 5.6l3.4 3.4"/>',
  trash: '<path d="M3.5 5.5h13M8 5.5V4h4v1.5M5.2 5.5l.8 11h8l.8-11"/>',
};

/** A glyph of this page's (the Mac's SF Symbols, drawn as dom.js's are), or
 * one of the shared set. */
function glyph(name, className = "icon") {
  const svg = icon(name, className);
  if (GLYPHS[name]) svg.innerHTML = GLYPHS[name];
  return svg;
}

// ── reading ───────────────────────────────────────────────────────────────

const offline = (host) => !host.thisPC && host.online === false;

/** `vault list --all`, into the host's summary (Home reads the same). */
function refresh(app, host) {
  return app.read(host, "vault", ["vault", "list", "--all", ...app.on(host)]);
}

/** Read when the Vault shows, unless it's held; another host Plow counts
 * offline isn't asked. */
export async function enter(app, host) {
  if (offline(host)) return;
  if (!app.summaries[host.name]?.vault?.value) await refresh(app, host);
}

/** Another place shows: its shows are read again when it's back, as the
 * Mac's inspector asks each time it appears. */
export function leave(app) {
  kept(app).asked.clear();
  closeMenu();
}

/** Reads an item's non-secret values; if it changed since the vault was
 * listed, the vault is read again, so the item listed is the one shown. */
async function showItem(app, host, id) {
  let show;
  try {
    const answer = await latch(["vault", "show", id, ...app.on(host)]);
    if (!answer?.item) throw unreachable(UNREADABLE);
    show = { value: answer };
  } catch (error) {
    show = { error };
  }
  (kept(app).shows[host.name] ??= {})[id] = show;
  const listed = app.summaries[host.name]?.vault?.value?.items?.find((item) => item.item === id);
  if (show.value && listed && listed.revision < show.value.item.revision) await refresh(app, host);
}

/** What `vault show` said of an item at the revision listed; null while
 * that revision is read (a show of an older one is not its values). */
function showOf(app, host, item) {
  const show = kept(app).shows[host.name]?.[item.item];
  if (show?.value && show.value.item.revision !== item.revision) return null;
  return show ?? null;
}

/** The items the Vault lists: those its search matches. */
function matching(app, items) {
  const query = (app.search.vault ?? "").trim().toLowerCase();
  if (!query) return items;
  return items.filter((item) =>
    [item.title, item.kind, item.username ?? "", ...item.sites].some((word) => word.toLowerCase().includes(query)));
}

/** The item the inspector shows: the one selected on this host, while the
 * Vault lists it. */
function inspected(app, host) {
  const { selected } = kept(app);
  const items = app.summaries[host.name]?.vault?.value?.items;
  if (!selected || selected.host !== host.name || !items) return null;
  const item = items.find((listed) => listed.item === selected.id);
  return item && matching(app, [item]).length ? item : null;
}

/** Puts `children` in `node` in place of what it held, as el() takes them
 * (nested lists, nothing for null). */
function fill(node, ...children) {
  node.replaceChildren(...children.flat(Infinity).filter((child) => child != null && child !== false));
}

/** Draws the page again where it was scrolled to, if it still shows. */
function redraw(app) {
  if (app.place !== "vault") return;
  const state = kept(app);
  const top = document.querySelector(".content .page")?.scrollTop ?? 0;
  state.redrawing = true;
  app.drawPage();
  state.redrawing = false;
  const page = document.querySelector(".content .page");
  if (page) page.scrollTop = top;
}

// ── the page ──────────────────────────────────────────────────────────────

export function render(app, host) {
  // A closed inspector asks for nothing; opened again, it asks again.
  if (!app.inspecting.vault) kept(app).asked.clear();
  return el("div", { class: "vault" },
    commands(app, host),
    offline(host)
      ? unavailable("wifiOff", `${host.name} is offline`, `Its vault is kept on ${host.name}. Start Latch there to see and change it.`)
      : [removeError(app, host), items(app, host)]);
}

/** New Item and Import…, the Mac's bar buttons: they work once the host's
 * vault has been read. */
function commands(app, host) {
  const can = Boolean(app.summaries[host.name]?.vault?.value);
  const command = (symbol, label, help, action) =>
    el("button", { class: "command", title: help, disabled: !can, onclick: action }, glyph(symbol), label);
  return el("div", { class: "commands", role: "toolbar", "aria-label": "Vault" },
    command("plus", "New Item", "Add a login, card, identity or note", () => newItem(app, host)),
    command("import", "Import…", "Import another password manager's export", () => chooseImport(app, host)));
}

/** The Mac's ContentUnavailableView: a symbol, a title, words, an action. */
function unavailable(symbol, title, words, action, className = "") {
  return el("div", { class: `empty ${className}`.trim() }, glyph(symbol, "icon big"), el("h3", {}, title),
    typeof words === "string" ? el("p", {}, words) : words, action ? el("div", { class: "action" }, action) : null);
}

/** Why the last removal failed, above the list, until it's dismissed: room
 * for latch's reason under Details, which a dialog wouldn't have. */
function removeError(app, host) {
  const error = kept(app).removeError;
  if (!error) return null;
  return el("div", { class: "infobar", role: "alert" }, glyph("warning", "icon warn"),
    el("div", { class: "text" }, el("strong", {}, "Couldn't remove the item"), errorText(error, host, { titled: true })),
    button("OK", () => { kept(app).removeError = null; redraw(app); }));
}

function items(app, host) {
  const read = app.summaries[host.name]?.vault;
  if (!read) return el("div", { class: "reading" }, el("span", { class: "ring", "aria-hidden": "true" }), `Reading the vault on ${host.name}…`);
  if (read.error) {
    return unavailable("warning", `Couldn't read the vault on ${host.name}`, errorText(read.error, host, { titled: true }),
      button("Try Again", async () => { await refresh(app, host); redraw(app); }));
  }
  const all = read.value.items ?? [];
  if (!all.length) {
    return unavailable("vault", `No items on ${host.name}`,
      "Agents fill logins and cards from here without ever seeing them. Add an item, or import another password manager's export.",
      button("New Item…", () => newItem(app, host)));
  }
  const shown = matching(app, all);
  if (!shown.length) return unavailable("search", `No Results for “${app.search.vault}”`, "Check the spelling or try a new search.");
  return list(app, host, shown);
}

function list(app, host, shown) {
  const state = kept(app);
  const selected = state.selected?.host === host.name ? state.selected.id : null;
  const box = el("div", {
    class: "list", role: "listbox", tabindex: "0", "aria-label": `Items on ${host.name}`,
    onkeydown: (event) => keys(event, app, host, shown),
    onfocusin: () => { state.focus = true; },
    onfocusout: () => { if (!state.redrawing) state.focus = false; },
  }, shown.map((item) => row(app, host, item, item.item === selected)));
  // Picked in the list, the list keeps the keyboard focus across redraws.
  if (state.focus || state.reveal) {
    const reveal = state.reveal;
    state.reveal = false;
    queueMicrotask(() => {
      if (!box.isConnected) return;
      if (state.focus) box.focus({ preventScroll: true });
      if (reveal) box.querySelector(".entry.selected")?.scrollIntoView({ block: "nearest" });
    });
  }
  return box;
}

function row(app, host, item, selected) {
  const kind = KINDS[item.kind];
  return el("div", {
    class: `entry${selected ? " selected" : ""}`, role: "option", "aria-selected": String(selected), "data-item": item.item,
    onclick: () => select(app, host, item),
    ondblclick: () => beginEdit(app, host, item),
    oncontextmenu: (event) => {
      event.preventDefault();
      select(app, host, item);
      openMenu(event.clientX, event.clientY, [
        ["Edit…", "pencil", () => beginEdit(app, host, item)],
        ["Remove…", "trash", () => askRemove(app, host, item), true],
      ]);
    },
  },
  glyph(kind?.symbol ?? "unknown", "icon kind"),
  el("span", { class: "text" }, el("span", { class: "title" }, item.title), el("small", {}, context(item))),
  item.totp ? el("span", { class: "twofa", title: "Its authenticator code is filled at sign-in" }, glyph("shield"), "2FA") : null);
}

function select(app, host, item, { reveal = false } = {}) {
  const state = kept(app);
  state.selected = { host: host.name, id: item.item };
  state.focus = true;
  state.reveal = reveal;
  redraw(app);
}

/** The arrows move the selection; Enter opens Edit, Delete asks to remove. */
function keys(event, app, host, shown) {
  const state = kept(app);
  const at = state.selected?.host === host.name ? shown.findIndex((item) => item.item === state.selected.id) : -1;
  const to = (index) => {
    event.preventDefault();
    select(app, host, shown[Math.max(0, Math.min(shown.length - 1, index))], { reveal: true });
  };
  switch (event.key) {
    case "ArrowDown": return to(at < 0 ? 0 : at + 1);
    case "ArrowUp": return to(at < 0 ? 0 : at - 1);
    case "Home": return to(0);
    case "End": return to(shown.length - 1);
    case "Enter": if (at >= 0) { event.preventDefault(); beginEdit(app, host, shown[at]); } return;
    case "Delete": if (at >= 0) { event.preventDefault(); askRemove(app, host, shown[at]); } return;
  }
}

// ── the context menu (Edit…, Remove…) ─────────────────────────────────────

let menuClose = null;

function closeMenu() {
  menuClose?.();
}

function openMenu(x, y, entries) {
  closeMenu();
  const root = document.querySelector(".app:not([hidden])") ?? document.body;
  const close = () => {
    menu.remove();
    removeEventListener("pointerdown", outside, true);
    removeEventListener("blur", close);
    removeEventListener("resize", close);
    menuClose = null;
  };
  const outside = (event) => { if (!menu.contains(event.target)) close(); };
  const menu = el("div", {
    class: "vault-menu", role: "menu",
    onkeydown: (event) => {
      const items = [...menu.querySelectorAll("button")];
      const at = items.indexOf(document.activeElement);
      if (event.key === "Escape") { event.preventDefault(); event.stopPropagation(); close(); }
      if (event.key === "ArrowDown" || event.key === "ArrowUp") {
        event.preventDefault();
        items[(at + (event.key === "ArrowDown" ? 1 : items.length - 1)) % items.length].focus();
      }
    },
  }, entries.map(([label, symbol, action, danger]) =>
    el("button", { role: "menuitem", class: danger ? "item danger" : "item", onclick: () => { close(); action(); } }, glyph(symbol), label)));
  root.append(menu);
  const { width, height } = menu.getBoundingClientRect();
  menu.style.left = `${Math.min(x, innerWidth - width - 4)}px`;
  menu.style.top = `${Math.min(y, innerHeight - height - 4)}px`;
  addEventListener("pointerdown", outside, true);
  addEventListener("blur", close);
  addEventListener("resize", close);
  menuClose = close;
  menu.querySelector("button")?.focus();
}

// ── the inspector ─────────────────────────────────────────────────────────

/** The selected item: its fields (secret ones only as saved, never shown),
 * where it fills, and Edit and Remove. */
export function inspector(app, host) {
  const item = inspected(app, host);
  if (!item) {
    return el("div", { class: "vault-inspector" }, unavailable("lockRect", "No Selection",
      "Select an item to see what it holds. Secret values are never shown."));
  }
  ask(app, host, item);
  const show = showOf(app, host, item);
  const kind = KINDS[item.kind];
  const fields = shownFields(item);
  const value = (label) => (show?.value ? show.value.values?.[label] ?? "—" : show?.error ? "—" : "…");
  const section = (title, ...rows) => el("section", { class: "group" }, title ? el("h3", {}, title) : null, el("div", { class: "card" }, ...rows));
  const line = (label, shown, { quiet = false, mono = false } = {}) =>
    el("div", { class: "line" }, el("span", { class: mono ? "mono" : "" }, label), el("span", { class: `value${quiet ? " quiet" : ""}` }, shown));
  return el("div", { class: "vault-inspector" },
    el("div", { class: "item" }, glyph(kind?.symbol ?? "unknown", "icon kind"),
      el("div", {}, el("h2", {}, item.title), el("p", {}, kindTitle(item.kind)))),
    section("Fields",
      fields.length ? null : el("div", { class: "line quiet" }, "No fields"),
      show?.error ? el("div", { class: "line failed" }, glyph("warning"), errorText(show.error, null)) : null,
      fields.map((field) => (field.hidden
        ? line(fieldTitle(field.label), "Saved, never shown", { quiet: true })
        : el("div", { class: "line" }, el("span", {}, fieldTitle(field.label)), el("span", { class: "value shown" }, value(field.label)))))),
    item.kind !== "note"
      ? section(item.kind === "login" ? "Fills on" : "Fills only on",
        item.sites.length ? null : el("div", { class: "line quiet" }, "Any site"),
        item.sites.map((site) => el("div", { class: "line" }, el("span", { class: "mono" }, site))),
        (item.other_top_sites ?? []).map((site) => line(site, "also inside", { quiet: true, mono: true })))
      : null,
    item.kind === "login" ? section(null, line("Two-factor", item.totp ? "Code filled at sign-in" : "None")) : null,
    section(null, el("div", { class: "line buttons" },
      el("button", { class: "remove", "aria-label": `Remove ${item.title}…`, onclick: () => askRemove(app, host, item) }, "Remove…"),
      button("Edit…", () => beginEdit(app, host, item)))),
    el("p", { class: "foot" }, `Kept on ${host.name}. Latch never shows its secret values.`));
}

/** Reads the item's values once per revision while the inspector shows. */
function ask(app, host, item) {
  const state = kept(app);
  const key = `${host.name}/${item.item}@${item.revision}`;
  if (state.asked.has(key)) return;
  state.asked.add(key);
  showItem(app, host, item.item).then(() => redraw(app));
}

/** The fields a person fills in, and any of the item's own: not those latch
 * derives from them (a card's `expiry`). */
function shownFields(item) {
  const kind = KINDS[item.kind];
  if (!kind) return item.fields;
  const labels = new Set([...kind.entries.map((e) => e.label), "notes"]);
  return item.fields.filter((field) => labels.has(field.label) || field.custom === true);
}

// ── changes ───────────────────────────────────────────────────────────────

/** A change to a host's vault, then the vault read again, whether it worked
 * or not. An item changed elsewhere is said in the app's words (`stale`),
 * or left as latch's error when null. */
async function change(app, host, run, stale = ITEM_CHANGED_ELSEWHERE) {
  try {
    const answer = await run();
    await refresh(app, host);
    return answer;
  } catch (error) {
    await refresh(app, host);
    if (stale && error?.error?.code === "stale_revision") throw unreachable(stale);
    throw error;
  }
}

function askRemove(app, host, item) {
  app.confirm(`Remove “${item.title}” from ${host.name}?`, "Agents can no longer fill it. This can't be undone.", "Remove")
    .then((yes) => yes && remove(app, host, item));
}

/** Removes an item at the revision the person saw; a refusal is kept for
 * the page to say. */
async function remove(app, host, item) {
  const state = kept(app);
  try {
    await change(app, host, () => latch(["vault", "remove", item.item, "--revision", String(item.revision), ...app.on(host)]));
    state.removeError = null;
    if (state.selected?.host === host.name && state.selected.id === item.item) state.selected = null;
    delete state.shows[host.name]?.[item.item];
  } catch (error) {
    state.removeError = error;
  }
  redraw(app);
}

// ── New Item and Edit ─────────────────────────────────────────────────────

/** Sites as typed: separated by commas or spaces. */
const split = (text) => text.split(/[\s,]+/).filter(Boolean);

/** The sheet's state (`VaultEditor`): an item new, being changed (from what
 * `vault show` said), or whose values couldn't be read. Values latch marks
 * hidden are never put in a field, whatever an answer holds. */
function editor(host, { kind = "login", show = null, unreadable = null } = {}) {
  const item = show?.item ?? unreadable?.item;
  const hidden = new Set((show?.item.fields ?? []).filter((field) => field.hidden).map((field) => field.label));
  const values = {};
  for (const [label, value] of Object.entries(show?.values ?? {})) if (!hidden.has(label)) values[label] = value;
  return {
    host, original: show, unreadable,
    kind: item ? (KINDS[item.kind] ? item.kind : "note") : kind,
    title: item?.title ?? "",
    sites: show ? show.item.sites.join(", ") : "",
    otherTopSites: show ? (show.item.other_top_sites ?? []).join(", ") : "",
    values, saving: false, error: null,
  };
}

const isNew = (editing) => !editing.original;

/** Whether a secret field holds a saved value the sheet won't show. */
const saved = (editing, label) => editing.original?.item.fields.some((field) => field.label === label && field.hidden) === true;

const secret = (editing, label, fallback) => editing.original?.item.fields.find((field) => field.label === label)?.hidden ?? fallback;

/** What stops a save, in words; null when it can be saved. */
function problem(editing) {
  if (!editing.title.trim()) return "Give it a title.";
  if (editing.kind === "login" && !split(editing.sites).length) return "A login needs its website, or it can never be filled.";
  return null;
}

/** What `vault add` or `vault edit` is given: for an edit, only the values
 * that changed, and a secret only when one was typed. */
function draft(editing) {
  const values = {};
  for (const label of [...KINDS[editing.kind].entries.map((e) => e.label), "notes"]) {
    const typed = editing.values[label] ?? "";
    if (isNew(editing) || secret(editing, label, false)) {
      if (typed) values[label] = typed;
    } else if (typed !== (editing.original.values[label] ?? "")) {
      values[label] = typed;
    }
  }
  return {
    kind: editing.kind, title: editing.title.trim(),
    sites: editing.kind === "note" ? [] : split(editing.sites),
    otherTopSites: editing.kind === "login" ? split(editing.otherTopSites) : [],
    values,
  };
}

/** `vault add --kind K --title T [--site S…] [--other-top-site S…] --stdin`:
 * every value on stdin as one JSON object, never in the arguments. */
async function addItem(app, host, given) {
  const args = ["vault", "add", "--kind", given.kind, "--title", given.title,
    ...given.sites.flatMap((site) => ["--site", site]), ...given.otherTopSites.flatMap((site) => ["--other-top-site", site]),
    "--stdin", ...app.on(host)];
  const values = Object.fromEntries(Object.entries(given.values).filter(([, value]) => value !== ""));
  const answer = await latch(args, JSON.stringify(values));
  if (!answer?.item) throw unreachable(UNREADABLE);
  return answer.item;
}

/** `vault edit ITEM --revision R --title T …sites --stdin`: the values that
 * changed on stdin (an empty one clears it). */
async function editItem(app, host, id, revision, given) {
  const args = ["vault", "edit", id, "--revision", String(revision), "--title", given.title,
    ...(given.sites.length ? given.sites.flatMap((site) => ["--site", site]) : ["--no-sites"])];
  if (given.kind === "login") {
    args.push(...(given.otherTopSites.length ? given.otherTopSites.flatMap((site) => ["--other-top-site", site]) : ["--no-other-top-sites"]));
  }
  args.push("--stdin", ...app.on(host));
  const answer = await latch(args, JSON.stringify(given.values));
  if (!answer?.item) throw unreachable(UNREADABLE);
  return answer.item;
}

function newItem(app, host) {
  editSheet(app, editor(host));
}

/** Opens an item's Edit sheet, from the item as it is now. */
async function beginEdit(app, host, item) {
  await showItem(app, host, item.item);
  redraw(app);
  const show = kept(app).shows[host.name]?.[item.item];
  if (show?.value) editSheet(app, editor(host, { show: show.value }));
  else if (show?.error) editSheet(app, editor(host, { unreadable: { item, error: show.error } }));
}

/** New Item, or an item's Edit: its kind's fields, grouped. */
function editSheet(app, editing) {
  const host = editing.host;
  const body = el("fieldset", { class: "scroll" });
  const foot = el("div", { class: "foot" });
  const box = el("div", { class: "vault-sheet editor" },
    el("header", {},
      el("h2", {}, isNew(editing) ? "New Item" : `Edit “${editing.original?.item.title ?? editing.title}”`),
      el("p", {}, `In ${host.name}'s vault`)),
    body, foot);
  const close = app.sheet(box, { width: 480 });

  const save = async () => {
    if (problem(editing) || editing.saving) return;
    editing.saving = true;
    body.disabled = true;
    drawFoot();
    try {
      const given = draft(editing);
      if (editing.original) {
        const { item, revision } = editing.original.item;
        await change(app, host, () => editItem(app, host, item, revision, given));
        await showItem(app, host, item);
      } else {
        const item = await change(app, host, () => addItem(app, host, given));
        kept(app).selected = { host: host.name, id: item.item };
      }
      editing.error = null;
      close();
      redraw(app);
    } catch (error) {
      editing.error = error;
    } finally {
      editing.saving = false;
      body.disabled = false;
    }
    drawFoot();
  };

  // A refused save is said here, beside Save, never below the fold.
  function drawFoot() {
    const blocked = problem(editing);
    fill(foot,
      editing.error
        ? el("div", { class: "said bad" }, glyph("warning"), errorText(editing.error, host))
        : el("p", { class: "said" }, `Secrets go straight to ${host.name}'s vault and are never shown again.`),
      el("div", { class: "buttons" },
        button("Cancel", () => close()),
        editing.unreadable ? null : el("button", {
          class: "primary", disabled: editing.saving || blocked != null, title: blocked ?? false, onclick: save,
        }, editing.saving ? "Saving…" : isNew(editing) ? "Add" : "Save")));
  }

  const input = (label, read, write, { placeholder = "", password = false, help = null } = {}) =>
    el("label", { class: "line field", title: help ?? false }, el("span", {}, label), el("input", {
      type: password ? "password" : "text", value: read(), placeholder, "aria-label": label,
      autocomplete: password ? "new-password" : "off", spellcheck: "false",
      oninput: (event) => { write(event.target.value); drawFoot(); },
      // Return saves, as the Mac's default button does.
      onkeydown: (event) => { if (event.key === "Enter") { event.preventDefault(); save(); } },
    }));

  function drawBody() {
    box.classList.toggle("tall", editing.kind === "identity");
    if (editing.unreadable) {
      const { item, error } = editing.unreadable;
      fill(body, el("div", { class: "card" }, el("div", { class: "line failed" }, glyph("warning"),
        el("div", {}, el("p", { class: "headline" }, `Couldn't read “${item.title}”`), errorText(error, host, { titled: true })))));
      return;
    }
    const kind = KINDS[editing.kind];
    const value = (label) => () => editing.values[label] ?? "";
    const set = (label) => (typed) => { editing.values[label] = typed; };
    const picker = el("select", {
      "aria-label": "Kind",
      onchange: (event) => { editing.kind = event.target.value; drawBody(); drawFoot(); body.querySelector("select")?.focus(); },
    }, Object.entries(KINDS).map(([id, { title }]) => el("option", { value: id, selected: id === editing.kind }, title)));
    const siteNote = {
      login: "Latch fills it only on this website (separate several with commas).",
      card: "Leave empty to let it fill on any site you send an agent to.",
      identity: "Leave empty to let it fill on any site you send an agent to.",
      note: "A note isn't tied to a site.",
    }[editing.kind];
    const fields = kind.entries.map((field) => (secret(editing, field.label, field.secret)
      ? input(fieldTitle(field.label), value(field.label), set(field.label), {
        password: true, placeholder: saved(editing, field.label) ? "Saved · type to replace" : field.label === "totp" ? "Optional" : "",
      })
      : input(fieldTitle(field.label), value(field.label), set(field.label))));
    fill(body,
      el("div", { class: "card" },
        isNew(editing) ? el("label", { class: "line field" }, el("span", {}, "Kind"), picker) : null,
        input("Title", () => editing.title, (typed) => { editing.title = typed; }, { placeholder: editing.kind === "login" ? "Shop" : "Required" }),
        editing.kind === "login" ? [
          input("Website", () => editing.sites, (typed) => { editing.sites = typed; }, { placeholder: "shop.example" }),
          input("Also inside", () => editing.otherTopSites, (typed) => { editing.otherTopSites = typed; },
            { placeholder: "Optional", help: "Other sites that show this sign-in page inside their own" }),
        ] : null,
        editing.kind === "card" || editing.kind === "identity"
          ? input("Fills only on", () => editing.sites, (typed) => { editing.sites = typed; }, { placeholder: "Any site" })
          : null),
      el("p", { class: "hint" }, siteNote),
      fields.length ? [el("h3", {}, "Fields"), el("div", { class: "card" }, fields)] : null,
      el("h3", {}, editing.kind === "note" ? "Note" : "Notes"),
      el("textarea", {
        class: editing.kind === "note" ? "notes long" : "notes", "aria-label": "Notes", spellcheck: "false", autocomplete: "off",
        oninput: (event) => { editing.values.notes = event.target.value; drawFoot(); },
      }, editing.values.notes ?? ""));
  }

  drawBody();
  drawFoot();
  (body.querySelector("input, select") ?? foot.querySelector("button"))?.focus();
}

// ── Import ────────────────────────────────────────────────────────────────

/** Import…: the Open dialog, for an export to import into this host. */
async function chooseImport(app, host) {
  const file = await pickFile(`Choose another password manager's export to import into ${host.name}'s vault.`,
    "Password exports", ["csv", "1pux", "json"]);
  if (file) importSheet(app, host, file);
}

/** Importing an export (`VaultImport`): what it holds (`vault import
 * --preview`, no secret shown), the rows picked, and what was saved. */
function importSheet(app, host, file) {
  const importing = { preview: null, picked: new Set(), running: false, result: null, notice: null };
  const content = el("div", { class: "scroll" });
  const foot = el("div", { class: "foot" });
  const box = el("div", { class: "vault-sheet import" },
    el("header", {}, el("h2", {}, `Import into ${host.name}'s vault`), el("p", {}, file.split(/[\\/]/).pop())),
    content, foot);
  const close = app.sheet(box, { width: 560 });
  const on = app.on(host);

  /** The preview, its rows picked but for those the vault already holds. */
  const load = async () => {
    try {
      const answer = await latch(["vault", "import", file, "--preview", ...on]);
      if (!answer?.preview) throw unreachable(UNREADABLE);
      importing.preview = { value: answer.preview };
      importing.picked = new Set(answer.preview.items.filter((row) => !row.duplicate).map((row) => row.pick));
    } catch (error) {
      importing.preview = { error };
    }
  };

  /** Imports the picked rows. If the vault or the export changed since the
   * preview, nothing is saved: the preview is read again, the picks that
   * still exist kept, for the person to check and import again. */
  const run = async () => {
    const expect = importing.preview?.value?.expect;
    if (!expect || !importing.picked.size || importing.running) return;
    importing.running = true;
    draw();
    try {
      const picks = [...importing.picked].sort((a, b) => a - b).join(",");
      const result = await change(app, host, async () => {
        const answer = await latch(["vault", "import", file, "--pick", picks, "--expect", expect, ...on]);
        if (typeof answer?.saved !== "number") throw unreachable(UNREADABLE);
        return answer;
      }, null);
      importing.result = { value: result };
    } catch (error) {
      if (error?.error?.code === "stale_revision") {
        const before = importing.picked;
        await load();
        const fresh = importing.preview?.value;
        if (fresh) importing.picked = new Set(fresh.items.map((row) => row.pick).filter((pick) => before.has(pick)));
        importing.notice = CHANGED_SINCE_PREVIEW;
      } else {
        importing.result = { error };
      }
    }
    importing.running = false;
    draw();
    redraw(app);
  };

  const checkable = () => importing.preview?.value?.expect != null;

  function drawFoot() {
    const preview = importing.preview?.value;
    fill(foot,
      preview && !importing.result
        ? el("p", { class: "said" }, `${importing.picked.size} of ${preview.items.length} picked · from ${preview.source}`)
        : el("p", { class: "said" }),
      el("div", { class: "buttons" }, importing.result
        ? el("button", { class: "primary", onclick: () => close() }, "Done")
        : [button("Cancel", () => close()), el("button", {
          class: "primary", disabled: importing.running || !importing.picked.size || !checkable(), onclick: run,
        }, importing.running ? "Importing…" : "Import")]));
  }

  function drawContent() {
    const { preview, result } = importing;
    let shown;
    if (!preview) {
      shown = el("div", { class: "reading" }, el("span", { class: "ring", "aria-hidden": "true" }), "Reading the export…");
    } else if (preview.error) {
      shown = unavailable("warning", "Couldn't read this export", errorText(preview.error, host, { titled: true }));
    } else if (result?.value) {
      const failed = result.value.failed ?? [];
      shown = unavailable(failed.length ? "warning" : "done", savedWords(result.value),
        failed.length ? el("p", { class: "lines" }, failed.map((row) => `${row.title}: ${row.reason}`).join("\n")) : null);
    } else if (result?.error) {
      shown = unavailable("warning", "Couldn't confirm the import", errorText(result.error, host, { titled: true }));
    } else {
      shown = rows(preview.value);
    }
    fill(content, shown);
  }

  function rows(preview) {
    const warn = (words) => el("div", { class: "warn-line" }, glyph("warning"), el("span", {}, words));
    return el("fieldset", { class: "import-list", disabled: importing.running },
      importing.notice ? warn(importing.notice) : null,
      preview.expect == null ? warn(`${host.name}'s Latch can't check an import against its preview. Update Latch there, then import.`) : null,
      el("div", { class: "picks" }, preview.items.map((row) => el("label", { class: "pick" },
        el("input", {
          type: "checkbox", checked: importing.picked.has(row.pick),
          onchange: (event) => { importing.picked[event.target.checked ? "add" : "delete"](row.pick); drawFoot(); },
        }),
        el("span", { class: "text" }, el("span", {}, row.title), el("small", {}, rowDetail(row)))))),
      el("p", { class: "hint" }, `Only the rows you pick are saved. Their secrets go straight to ${host.name} and are never shown.`),
      preview.skipped?.length ? [el("h3", {}, "Not imported"), el("div", { class: "skipped" }, preview.skipped.map((row) =>
        el("div", { class: "skip" }, el("span", {}, row.title), el("span", { class: "value" }, row.reason))))] : null);
  }

  function draw() {
    drawContent();
    drawFoot();
  }

  draw();
  load().then(draw);
}

/** "Login · ada · shop.example/login · already in the vault". */
function rowDetail(row) {
  const words = [kindTitle(row.kind)];
  if (row.username) words.push(row.username);
  if (row.url) words.push(row.url);
  if (row.duplicate) words.push("already in the vault");
  if (row.changed?.length) words.push(`updates ${row.changed.join(", ")}`);
  words.push(...(row.warnings ?? []));
  return words.join(" · ");
}

function savedWords(result) {
  const parts = [`${result.saved} saved`];
  if (result.updated > 0) parts.push(`${result.updated} updated`);
  if (result.duplicates > 0) parts.push(`${result.duplicates} already there`);
  if (result.failed?.length) parts.push(`${result.failed.length} failed`);
  return parts.join(" · ");
}
