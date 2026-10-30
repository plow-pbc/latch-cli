// Your account › Agents, as the Mac app's (Main/AgentsView.swift and
// Main/NewAgentSheet.swift): the account's agents in a list, the one picked
// in full beside it (who it is, how to reach it, and where it stands on
// each host), its ⋯ menu (Copy, Remove…), and New Agent, the sheet that
// says what to text Plow to start a cloud agent and waits for it.
//
// What the account pages share (latch's errors in the app's words, the
// progress ring, a QR code, a few glyphs) is exported from here for
// Connections and Settings.

import { el, icon, button } from "../dom.js";
import { latch, latchFile } from "../latch.js";
import { hostState } from "../main.js";
import * as rulesPage from "./rules.js";
import { openActivity } from "./activity.js";

/** What the page keeps (the Mac's `AppModel` agents part): the account's
 * agents and why they couldn't be listed, each host's standing by host
 * name, the catalog's kind names, the agent picked and a removal's refusal. */
function kept(app) {
  return (app.pages.agents ??= {
    agents: null, error: null, standings: {}, reading: new Set(), kinds: null, selected: null, removeError: null,
  });
}

/** Draws the page again, if it's still the one showing. */
function redraw(app) {
  if (app.place === "agents") app.drawPage();
}

let activated = false;

/** Reads only what isn't held yet, as the Mac's `enter(.account(.agents))`:
 * the catalog (only for kind names; a failure isn't shown), the list, then
 * each reachable host's standing. */
export async function enter(app) {
  if (!activated) {
    activated = true;
    // The app came to the front: an agent added or removed elsewhere shows
    // (the Mac's `activated()`).
    window.addEventListener("focus", () => app.place === "agents" && refresh(app, { kinds: true }));
  }
  const page = kept(app);
  const kinds = page.kinds ? null : readKinds(app);
  if (!page.agents) {
    await readList(app);
    redraw(app);
  }
  await readStandings(app, { missing: true });
  await kinds;
}

export function leave() {
  closeMenu();
}

/** Everything again: the list and every standing (and the kinds). */
async function refresh(app, { kinds = false } = {}) {
  const names = kinds ? readKinds(app) : null;
  await readList(app);
  redraw(app);
  await readStandings(app, { missing: false });
  await names;
}

async function readKinds(app) {
  try {
    const catalog = await latch(["agents", "catalog"]);
    kept(app).kinds = Object.fromEntries([...catalog.kinds].reverse().map((kind) => [kind.id, kind.name]));
    redraw(app);
  } catch {}
}

/** `latch agents`: the account's agents, without their standing. A failure
 * keeps the agents held, as the Mac's does. */
async function readList(app) {
  const page = kept(app);
  try {
    const list = await latch(["agents"]);
    page.agents = list.agents;
    page.error = null;
    // Rules names the agents too.
    (app.summaries[""] ??= {}).agents = { value: list };
  } catch (error) {
    page.error = error;
  }
}

/** Each reachable host's standing (`latch agents --host NAME`, this PC's
 * too), all at once; with `missing`, only those not held yet. */
function readStandings(app, { missing }) {
  const page = kept(app);
  return Promise.all(reachable(app)
    .filter((host) => !page.reading.has(host.name) && !(missing && page.standings[host.name]?.value))
    .map((host) => readStanding(app, host)));
}

async function readStanding(app, host) {
  const page = kept(app);
  page.reading.add(host.name);
  try {
    const list = await latch(["agents", "--host", host.name]);
    const byAgent = new Map();
    for (const agent of list.agents) if (agent.standing && !byAgent.has(agent.uid)) byAgent.set(agent.uid, agent.standing);
    page.standings[host.name] = { value: byAgent };
  } catch (error) {
    page.standings[host.name] = { error };
  }
  page.reading.delete(host.name);
  redraw(app);
}

/** This PC, and the hosts Plow doesn't count offline. */
const reachable = (app) => app.hosts().filter((host) => host.thisPC || host.online !== false);

/** A host listed after the page was entered is asked too, once (the Mac
 * keys its reads by the hosts it can ask). */
function readNewHosts(app) {
  const page = kept(app);
  for (const host of reachable(app)) {
    if (!(host.name in page.standings) && !page.reading.has(host.name)) readStanding(app, host);
  }
}

// ── the page ──────────────────────────────────────────────────────────────

export function render(app) {
  const page = kept(app);
  // A photo read after the page drew draws it again.
  photoApp = app;
  return el("div", { class: "agents-page" },
    // The page's own button, above it: the window's header is the same on
    // every page.
    el("div", { class: "ag-bar" },
      el("button", { class: "ag-new", title: "Start a cloud agent by text", onclick: () => newAgent(app) }, glyph("plus"), "New Agent")),
    content(app, page));
}

function content(app, page) {
  if (page.agents) {
    if (!page.agents.length) {
      return el("div", { class: "empty" }, icon("agents", "icon big"), el("h3", {}, "No agents yet"),
        el("p", {}, "Agents you create, run by Plow or by you, show up here."));
    }
    readNewHosts(app);
    const shown = page.agents.find((agent) => agent.uid === page.selected) ?? page.agents[0];
    return el("div", { class: "ag-split" },
      page.removeError ? removeFailed(app, page) : null,
      el("div", { class: "ag-panes" }, list(app, page, shown), detail(app, shown)));
  }
  if (page.error) {
    return el("div", { class: "empty" }, icon("warning", "icon big"), el("h3", {}, "Couldn't list your agents"),
      errorText(page.error, { titled: true }), el("p", {}, button("Try Again", () => refresh(app))));
  }
  return el("div", { class: "ag-waiting" }, ring(), el("span", {}, "Listing your agents…"));
}

/** Said above the list and the agent, where latch's reason can sit under
 * Details (a dialog has no room for it). */
function removeFailed(app, page) {
  return el("div", { class: "ag-infobar", role: "alert" },
    icon("warning", "icon"),
    el("div", { class: "ag-infotext" }, el("strong", {}, "Couldn't remove the agent"), errorText(page.removeError, { titled: true })),
    button("OK", () => { page.removeError = null; redraw(app); }));
}

/** Every agent, by name and number; one picked shows beside it. Up and
 * Down move the pick; a right-click opens its menu. */
function list(app, page, shown) {
  const pick = (uid) => {
    page.selected = uid;
    redraw(app);
    document.querySelector(".agents-page .ag-row.selected")?.focus();
  };
  const move = (event) => {
    const step = { ArrowDown: 1, ArrowUp: -1 }[event.key];
    if (!step) return;
    event.preventDefault();
    const at = page.agents.findIndex((agent) => agent.uid === shown.uid);
    const next = page.agents[at + step];
    if (next) pick(next.uid);
  };
  return el("div", { class: "ag-list", role: "listbox", "aria-label": "Agents", onkeydown: move },
    page.agents.map((agent) => {
      const picked = agent.uid === shown.uid;
      const number = phone(agent);
      return el("button", {
        class: `line ag-row${picked ? " selected" : ""}`, role: "option", "aria-selected": String(picked),
        tabindex: picked ? "0" : "-1", "aria-label": [name(agent), number].filter(Boolean).join(", "),
        onclick: () => pick(agent.uid),
        oncontextmenu: (event) => { event.preventDefault(); menu(event, menuItems(app, agent)); },
      }, monogram(agent, 32), el("span", { class: "ag-who" }, el("span", {}, name(agent)), number ? el("small", {}, number) : null));
    }));
}

/** One agent in full: who it is, how to reach it, and where it stands on
 * each host. */
function detail(app, agent) {
  const page = kept(app);
  const who = name(agent);
  const number = phone(agent);
  const made = created(agent);
  return el("div", { class: "ag-detail" },
    el("div", { class: "ag-head" },
      monogram(agent, 56),
      el("div", { class: "ag-title" }, el("h2", {}, who), el("span", {}, kindName(agent, page.kinds))),
      el("button", {
        class: "ag-more", title: "More", "aria-label": `More for ${who}`, "aria-haspopup": "menu",
        onclick: (event) => menu(event.currentTarget, menuItems(app, agent)),
      }, glyph("more"))),
    el("section", { class: "group" }, el("div", { class: "card" },
      number ? row("Phone", el("span", { class: "value ag-number" }, number)) : null,
      row("Handle", el("span", { class: "value" }, agent.name)),
      made ? row("Created", el("span", { class: "value" }, made)) : null)),
    el("section", { class: "group" }, el("h3", {}, "On your hosts"),
      el("div", { class: "card" }, app.hosts().map((host) => hostRow(app, agent, host)))));
}

const row = (label, value) => el("div", { class: "line" }, el("span", {}, label), value);

/** An agent on one host: whether the host is up, the agent's standing
 * there, and its rules and its requests there. */
function hostRow(app, agent, host) {
  const offline = isOffline(host);
  const state = hostState(host);
  const read = kept(app).standings[host.name];
  const who = name(agent);
  const words = !offline && read?.error
    ? errorText(read.error, { host: spoken(host) })
    : el("small", {}, standing(agent, host, read));
  const action = offline
    ? el("button", {
      class: "subtle", title: `${who}'s requests to ${host.name} that were read before it went offline`,
      onclick: () => openActivity(app, host, { agent: agent.uid }),
    }, "Activity")
    : el("button", {
      class: "subtle ag-menu-button", title: `${who}'s rules and requests on ${host.name}`, "aria-haspopup": "menu",
      onclick: (event) => menu(event.currentTarget, [
        { label: `Rules on ${host.name}`, run: () => openRules(app, host, agent.uid) },
        { label: `Activity on ${host.name}`, run: () => openActivity(app, host, { agent: agent.uid }) },
      ]),
    }, "Rules & Activity", glyph("down"));
  return el("div", { class: `line ag-host${offline ? " offline" : ""}` },
    el("span", { class: "ag-on" },
      el("i", { class: `ag-dot${state.up ? " up" : state.problem ? " problem" : ""}`, role: "img", "aria-label": state.title }),
      el("span", { class: "ag-where" }, el("span", {}, host.thisPC ? `${host.name} (this PC)` : host.name), words)),
    action);
}

/** The ⋯ menu's items, which a row's context menu shares. */
function menuItems(app, agent) {
  return [
    agent.line?.number && isPhone(agent.line.number) ? { label: "Copy Phone Number", run: () => copy(agent.line.number) } : null,
    { label: "Copy Handle", run: () => copy(agent.name) },
    "-",
    { label: `Remove ${name(agent)}…`, danger: true, run: () => remove(app, agent) },
  ];
}

/** Removes an agent by its uid (never its name, which another agent may
 * share), once the person has confirmed it; then the account's agents are
 * read again, removed or not. A refusal is said above the list. */
async function remove(app, agent) {
  const yes = await app.confirm(`Remove ${name(agent)}?`,
    "Its credential stops working at once, its phone line is freed and its chats are closed."
      + (agent.kind === "cloud" ? "" : " The program you run for it keeps running until you stop it."),
    "Remove");
  if (!yes) return;
  const page = kept(app);
  let failure = null;
  try {
    await latch(["agents", "remove", agent.uid, "--yes"]);
  } catch (error) {
    failure = error;
  }
  // Gone even if the list can't be read again just now.
  if (!failure && page.agents) page.agents = page.agents.filter((listed) => listed.uid !== agent.uid);
  await readList(app);
  page.removeError = failure;
  redraw(app);
}

/** An agent's rules on a host: that host's Rules, its editor brought into
 * view (Rules' own `openRules`). */
function openRules(app, host, uid) {
  if (rulesPage.openRules) return rulesPage.openRules(app, host, uid);
  app.hostName = host.name;
  app.go("rules");
}

function copy(text) {
  navigator.clipboard?.writeText(text).catch(() => {});
}

// ── an agent in words ─────────────────────────────────────────────────────

/** The name people know it by: its line's name in Plow, else its handle,
 * else its number. */
function name(agent) {
  const named = agent.line?.name?.trim();
  if (named) return named;
  if (agent.name) return agent.name;
  return phone(agent) ?? "";
}

/** Its line, as people write it. */
function phone(agent) {
  const number = agent.line?.number;
  return number ? phoneNumber(number) ?? number : null;
}

/** Who runs it: for a cloud agent, its kind as Plow's catalog names it. */
function kindName(agent, kinds) {
  if (agent.kind !== "cloud") return "Your own agent";
  return ["Cloud agent", agent.cloud_kind && kinds?.[agent.cloud_kind]].filter(Boolean).join(" · ");
}

/** When Plow made it (Plow writes it in UTC, with no zone). */
function created(agent) {
  const text = agent.created_at;
  if (!text) return null;
  const date = timestamp(text.endsWith("Z") || text.includes("+") ? text : `${text}Z`);
  return date ? date.toLocaleDateString([], { month: "short", day: "numeric", year: "numeric" }) : null;
}

/** latch's timestamps, with up to nanoseconds: to the millisecond. */
export function timestamp(text) {
  const date = new Date(text.replace(/(\.\d{3})\d+/, "$1"));
  return Number.isNaN(date.getTime()) ? null : date;
}

/** The host is known to be offline, so its standing there can't be read. */
const isOffline = (host) => !host.thisPC && host.online === false;

/** How a sentence names a host. */
const spoken = (host) => (host.thisPC ? THIS_PC : host.name);

/** Its standing on a host, in words: offline, not read yet, or which rules
 * judge it there and whether it may run programs. */
function standing(agent, host, read) {
  if (isOffline(host)) return "Offline, so its rules there can't be checked";
  if (!read) return "Checking…";
  if (read.error) return sentence(read.error, { host: spoken(host) }) ?? FALLBACK;
  const own = read.value.get(agent.uid);
  const blocked = own?.exec_blocked === true;
  const rules = own?.own_rules === true ? "Shared rules + own rules" : blocked ? "Shared rules" : "Shared rules only";
  return blocked ? `${rules} · can't run programs` : rules;
}

const isPhone = (text) => /^\+\d{8,15}$/.test(text);

/** A number in E.164 (`+16505550123`) as people write it (the Mac's
 * Latch/PhoneNumber.swift): a North American number nationally where this
 * PC's region is in it, else internationally; nil when it isn't E.164, and
 * any other country's as given. */
export function phoneNumber(text) {
  if (!isPhone(text)) return null;
  const nanp = /^\+1(\d{3})(\d{3})(\d{4})$/.exec(text);
  if (!nanp) return text;
  const [, area, exchange, line] = nanp;
  // A number that isn't in service anywhere (555's own area code, an
  // exchange of 0 or 1) has no country to write it as.
  const real = /^[2-9]\d\d$/.test(area) && area !== "555" && /^[2-9]\d\d$/.test(exchange);
  return real && NANP_REGIONS.has(region()) ? `(${area}) ${exchange}-${line}` : `+1 ${area}-${exchange}-${line}`;
}

const NANP_REGIONS = new Set(["US", "CA", "PR", "GU", "VI", "AS", "MP"]);

function region() {
  try {
    return new Intl.Locale(navigator.language).maximize().region ?? "US";
  } catch {
    return "US";
  }
}

// ── an agent's picture ────────────────────────────────────────────────────

/** Its monogram's tints, the same each time (the Mac's blue, green, orange,
 * pink, purple, teal, indigo and brown, in Windows' own shades). */
const TINTS = ["#0078D4", "#107C10", "#CA5010", "#E3008C", "#8764B8", "#038387", "#5C2E91", "#8E562E"];

/** Each persona's photo, by its card's URL: read once a session, when an
 * agent with that card is first shown. One that couldn't be read isn't
 * asked again; its agent keeps its monogram. */
const photos = new Map();
const asked = new Set();
let photoApp = null;

/** An agent's persona photo, from its line's contact card, or its initial
 * on a tint of its own; with a badge for who runs it. */
function monogram(agent, size) {
  const card = agent.line?.card_url;
  if (card) loadPhoto(agent, card);
  const photo = card ? photos.get(card) : null;
  const initial = [...name(agent)].find((char) => /\p{L}/u.test(char));
  const tint = TINTS[[...agent.uid].reduce((sum, char) => sum + char.codePointAt(0), 0) % TINTS.length];
  return el("span", { class: "ag-avatar", style: `--size: ${size}px; --tint: ${tint}`, "aria-hidden": "true" },
    photo ? el("img", { src: photo, alt: "" }) : initial ? el("span", { class: "ag-initial" }, initial.toUpperCase()) : glyph("person"),
    el("span", { class: "ag-badge" }, glyph(agent.kind === "cloud" ? "cloud" : "home")));
}

function loadPhoto(agent, card) {
  if (asked.has(card)) return;
  asked.add(card);
  latchFile(["agents", "card", agent.uid, "--output", "{file}"], "vcf").then((file) => {
    const photo = cardPhoto(file);
    if (!photo) return;
    photos.set(card, photo);
    if (photoApp) redraw(photoApp);
  }, () => {});
}

/** A contact card's PHOTO, as an image's data URL (the Mac's
 * App/AgentPhoto.swift): only a photo the card holds; one it links to
 * isn't fetched. `card` is the vCard as a data URL. */
export function cardPhoto(card) {
  let text;
  try {
    const bytes = Uint8Array.from(atob(card.slice(card.indexOf(",") + 1)), (char) => char.charCodeAt(0));
    text = new TextDecoder().decode(bytes);
  } catch {
    return null;
  }
  // Long values are folded onto lines that start with a space.
  for (const line of text.replace(/\r?\n[ \t]/g, "").split(/\r?\n/)) {
    const colon = line.indexOf(":");
    if (colon < 0) continue;
    const [property, ...params] = line.slice(0, colon).split(";");
    if (property.split(".").pop().toUpperCase() !== "PHOTO") continue;
    const value = line.slice(colon + 1).trim();
    if (/^data:image\//i.test(value)) return value;
    if (!params.some((param) => /^(encoding=)?(b|base64)$/i.test(param))) continue;
    const type = params.map((param) => /^(?:type=)?(?:image\/)?(jpe?g|png|gif|webp|bmp)$/i.exec(param)?.[1]).find(Boolean) ?? "jpeg";
    return `data:image/${type.toLowerCase().replace("jpg", "jpeg")};base64,${value.replace(/\s/g, "")}`;
  }
  return null;
}

// ── its menu ──────────────────────────────────────────────────────────────

let shownMenu = null;

/** A menu (a Windows menu flyout) under `at`, a button, or at a right
 * click's point; it goes on a pick, Escape, or a click elsewhere. */
export function menu(at, items) {
  closeMenu();
  const back = at instanceof Event ? null : at;
  const box = el("div", { class: "ag-flyout", role: "menu" },
    items.filter(Boolean).map((item) => (item === "-"
      ? el("hr", { role: "separator" })
      : el("button", {
        class: `line${item.danger ? " danger" : ""}`, role: "menuitem",
        onclick: () => { closeMenu(); item.run(); },
      }, item.label))));
  (document.querySelector(".app:not([hidden])") ?? document.body).append(box);
  const width = box.offsetWidth, height = box.offsetHeight;
  let left, top;
  if (back) {
    const rect = back.getBoundingClientRect();
    left = rect.right - width;
    top = rect.bottom + 4;
    if (top + height > innerHeight - 8) top = rect.top - height - 4;
  } else {
    left = at.clientX;
    top = at.clientY;
  }
  box.style.left = `${Math.max(8, Math.min(left, innerWidth - width - 8))}px`;
  box.style.top = `${Math.max(8, Math.min(top, innerHeight - height - 8))}px`;
  const entries = () => [...box.querySelectorAll("[role=menuitem]")];
  box.addEventListener("keydown", (event) => {
    const step = { ArrowDown: 1, ArrowUp: -1 }[event.key];
    if (step) {
      event.preventDefault();
      const all = entries();
      all[(all.indexOf(document.activeElement) + step + all.length) % all.length]?.focus();
    } else if (event.key === "Escape" || event.key === "Tab") {
      event.preventDefault();
      closeMenu();
      back?.focus();
    }
  });
  const outside = (event) => { if (!box.contains(event.target)) closeMenu(); };
  setTimeout(() => document.addEventListener("pointerdown", outside, true));
  shownMenu = { box, outside };
  entries()[0]?.focus();
}

function closeMenu() {
  if (!shownMenu) return;
  document.removeEventListener("pointerdown", shownMenu.outside, true);
  shownMenu.box.remove();
  shownMenu = null;
}

// ── New Agent ─────────────────────────────────────────────────────────────

/** How long to wait before each read of the account's agents: every 3
 * seconds for the first minute, while a text is likely on its way, then
 * every 10 seconds for nine more. Plow has no stream to follow. */
const WAITS = [...Array(20).fill(3_000), ...Array(54).fill(10_000)];

/** The sheet: what to text Plow's number to start a cloud agent (Plow's
 * own choice picked), and the wait for the agent Plow sets up. */
function newAgent(app) {
  const flow = { start: null, kind: null, closed: false };
  const body = el("div", { class: "na-body" });
  const wait = el("span", { class: "na-wait" });
  const done = el("button", { class: "primary", autofocus: true, onclick: () => flow.close() }, "Done");
  const foot = el("div", { class: "na-foot" }, wait, done);
  flow.draw = () => {
    body.replaceChildren(sheetBody(app, flow));
    // Shown while a kind is picked: the wait for Plow's text back.
    wait.replaceChildren(...(pickedKind(flow) ? [ring(true), "It shows up in Agents once Plow sets it up."] : []));
  };
  flow.draw();
  flow.close = app.sheet([
    el("div", { class: "na-head" }, el("h2", {}, "New Agent"), el("p", {}, "Start a cloud agent by texting Plow from your phone.")),
    body, foot,
  ], { width: 560 });
  const dialog = body.closest("dialog");
  dialog.classList.add("ag-sheet", "na-sheet");
  // However it closes (Done, Escape), the wait ends with it.
  dialog.addEventListener("close", () => (flow.closed = true));
  // Return is Done, wherever it isn't a button's or the picker's own.
  dialog.addEventListener("keydown", (event) => {
    if (event.key === "Enter" && !event.target.closest("button, select")) flow.close();
  });
  load(flow).then(() => watch(app, flow));
}

async function load(flow) {
  let start;
  try {
    start = { value: await latch(["agents", "start-by-text"]) };
  } catch (error) {
    start = { error };
  }
  if (flow.closed) return;
  flow.start = start;
  if (flow.kind == null && start.value) flow.kind = start.value.default ?? start.value.kinds[0]?.id ?? null;
  flow.draw();
}

function pickedKind(flow) {
  const kinds = flow.start?.value?.kinds ?? [];
  return kinds.find((kind) => kind.id === flow.kind) ?? kinds[0] ?? null;
}

/** Reads the account's agents until one appears that wasn't there when
 * the sheet opened; then the sheet closes on Agents with it picked. */
async function watch(app, flow) {
  const page = kept(app);
  if (!page.agents) {
    await readList(app);
    redraw(app);
  }
  // Without a first list, every agent would look new.
  if (flow.closed || !page.agents) return;
  const known = new Set(page.agents.map((agent) => agent.uid));
  for (const wait of WAITS) {
    await new Promise((resolve) => setTimeout(resolve, wait));
    if (flow.closed) return;
    await readList(app);
    if (flow.closed) return;
    redraw(app);
    const added = page.agents?.find((agent) => !known.has(agent.uid));
    if (added) {
      page.selected = added.uid;
      // Its standings first, then the sheet goes.
      await readStandings(app, { missing: false });
      flow.close();
      redraw(app);
      return;
    }
  }
}

function sheetBody(app, flow) {
  if (!flow.start) return el("div", { class: "na-center" }, ring());
  if (flow.start.error) {
    return el("div", { class: "empty na-failed" }, icon("warning", "icon big"), el("h3", {}, "Couldn't read how to start an agent"),
      errorText(flow.start.error, { titled: true }), el("p", {}, button("Try Again", () => load(flow))));
  }
  const start = flow.start.value;
  const picked = pickedKind(flow);
  if (!picked) return el("div", { class: "empty" }, glyph("cloud", "icon big"), el("h3", {}, "Plow offers no cloud agents right now"));
  const number = phoneNumber(start.number) ?? start.number;
  const code = el("div", { class: "na-code", role: "img", "aria-label": `QR code: text “${picked.phrase}” to ${number}` });
  qrCode(picked.smsto).then((svg) => (code.innerHTML = svg));
  return el("div", { class: "na-text" }, code,
    el("div", { class: "na-info" },
      start.kinds.length > 1
        ? el("label", { class: "na-kind" }, el("span", {}, "Agent"),
          el("span", { class: "na-select" },
            el("select", { onchange: (event) => { flow.kind = event.target.value; flow.draw(); } },
              start.kinds.map((kind) => el("option", { value: kind.id, selected: kind.id === picked.id }, kind.name))),
            glyph("down")))
        : null,
      copyable("Text", number, start.number),
      copyable("Message", picked.phrase, picked.phrase),
      el("p", { class: "na-hint" },
        "Scan the code with your phone's camera, or text this from your phone. Plow sets the agent up on a number of its own and texts you back.")));
}

/** A value to read or copy, under its title. */
function copyable(title, value, copied) {
  return el("div", { class: "na-copy" }, el("small", {}, title),
    el("div", {}, el("span", { class: "mono" }, value),
      el("button", { "aria-label": `Copy ${title.toLowerCase()}`, onclick: () => copy(copied) }, "Copy")));
}

// ── shared by the account pages ───────────────────────────────────────────

/** Windows' progress ring. */
export function ring(small = false) {
  return el("span", { class: `ag-ring${small ? " small" : ""}`, role: "progressbar", "aria-label": "Working" });
}

const codes = new Map();

/** `text` as a QR code, in SVG, from the app (`qr`); fake mode draws a
 * stand-in of the same look, which no camera reads. */
export function qrCode(text) {
  if (!codes.has(text)) {
    const invoke = window.__TAURI__?.core.invoke;
    codes.set(text, invoke ? invoke("qr", { text }).catch(() => "") : Promise.resolve(standInCode(text)));
  }
  return codes.get(text);
}

function standInCode(text) {
  const side = 29;
  let seed = [...text].reduce((hash, char) => (hash * 31 + char.codePointAt(0)) >>> 0, 7);
  const random = () => ((seed = (seed * 1103515245 + 12345) >>> 0) >>> 16) & 1;
  const finder = (x, y) => [[0, 0], [side - 7, 0], [0, side - 7]].some(([fx, fy]) => x >= fx - 1 && x <= fx + 7 && y >= fy - 1 && y <= fy + 7);
  let path = "";
  for (const [fx, fy] of [[0, 0], [side - 7, 0], [0, side - 7]]) {
    path += `M${fx} ${fy}h7v7h-7zM${fx + 1} ${fy + 1}v5h5v-5zM${fx + 2} ${fy + 2}h3v3h-3z`;
  }
  for (let y = 0; y < side; y++) for (let x = 0; x < side; x++) if (!finder(x, y) && random()) path += `M${x} ${y}h1v1h-1z`;
  return `<svg xmlns="http://www.w3.org/2000/svg" viewBox="-2 -2 ${side + 4} ${side + 4}"><path fill-rule="evenodd" fill="#000" d="${path}"/></svg>`;
}

/** Glyphs the shared set (dom.js) doesn't have yet, drawn the same way. */
const GLYPHS = {
  plus: '<path d="M10 4v12M4 10h12"/>',
  more: '<path d="M4.6 10h.01M10 10h.01M15.4 10h.01" stroke-width="2.4"/>',
  down: '<path d="M6 8l4 4 4-4"/>',
  cloud: '<path class="solid" d="M6.2 15.5h7.6a3.2 3.2 0 0 0 .5-6.4 4.7 4.7 0 0 0-8.7-1.1A3.8 3.8 0 0 0 6.2 15.5z"/>',
  home: '<path class="solid" d="M10 3.2l7 6V17h-4.6v-4.6H7.6V17H3V9.2z"/>',
  person: '<circle class="solid" cx="10" cy="7" r="3.2"/><path class="solid" d="M3.8 17.5c.6-3.6 3.1-5.6 6.2-5.6s5.6 2 6.2 5.6z"/>',
  mail: '<rect x="2.5" y="4.5" width="15" height="11" rx="1.5"/><path d="M3 5.5l7 5.5 7-5.5"/>',
};

export function glyph(name, className = "icon") {
  const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
  svg.setAttribute("viewBox", "0 0 20 20");
  svg.setAttribute("class", `${className} ag-glyph`);
  svg.setAttribute("aria-hidden", "true");
  svg.innerHTML = GLYPHS[name] ?? "";
  return svg;
}

// latch's errors in the app's own words (the Mac's App/ErrorCopy.swift):
// each code the app knows gets a sentence of its own; latch's message is
// only ever shown under Details.

/** Where nothing else says what failed. */
export const FALLBACK = "Latch couldn't do that.";
/** How the sentences name this PC's own host. */
export const THIS_PC = "this PC";

const code = (failure) => failure?.error?.code ?? null;

/** latch's own words for a failure, for Details. */
function details(failure) {
  if (typeof failure === "string") return failure || null;
  return failure?.error?.message ?? failure?.message ?? null;
}

/** The app's sentence for a failure, naming `host` (its name, or
 * `THIS_PC`) where it matters, `place` "devices" for a device's removal;
 * null when the app has none. */
export function sentence(failure, { host = null, place = null } = {}) {
  const name = host ?? "the host";
  const Name = host === THIS_PC ? "This PC" : host ?? "The host";
  const said = code(failure);
  if (place === "devices" && said === "not_found") return "That device isn't in your account any more.";
  switch (said) {
    // The window's own failures: latch didn't run, or didn't answer as latch.
    case "no_binary": return "Latch couldn't start.";
    case "latch_failed": case "io": return "Latch couldn't complete this step.";
    case "host_unreachable": return `Latch couldn't reach ${name}, so nothing changed. Try again.`;
    case "host_offline": return `${Name} is offline, so nothing changed. Start Latch there, then try again.`;
    case "host_stopped": return `Latch stopped on ${name}. Try again once it's running.`;
    case "record_too_large": return `Activity from ${name} stops at a record too large to send here. See the rest on ${name} itself.`;
    case "host_outdated":
      return `${Name}'s Latch can't check an import against its preview, so nothing was imported. Update Latch there, then import again.`;
    case "serve_required": return "Latch on this PC isn't answering. Start or restart it in Host Settings, then try again.";
    case "other_version": return "Another version of Latch is already running on this PC. Stop it, then start this one.";
    case "not_logged_in": return "This PC isn't logged in to Plow. Log in, then try again.";
    case "plow_unreachable": return "Plow can't be reached right now. Check your internet connection, then try again.";
    case "plow_error": return "Plow couldn't answer just now. Try again in a moment.";
    case "plow_denied": return "Plow didn't accept this PC's login for that.";
    case "gatekeeper_unavailable": return "The Gatekeeper couldn't judge this. Try again in a moment.";
    case "bad_name": return "Give the agent a name without control characters, such as tabs or line breaks.";
    case "line_taken": return "That phone line already has an agent. Choose a free line.";
    case "no_such_line": return "That phone line couldn't be found. Choose another line.";
    case "unknown_cloud_kind": return "That cloud agent isn't available. Choose another cloud agent.";
    case "invalid_request": return "Plow didn't accept these details. Check them, then try again.";
    case "no_such_agent": return "That agent isn't in your account any more.";
    case "unknown_host": return `${Name} isn't one of your hosts any more.`;
    case "own_machine": return "This is the PC you're using. Log out in Settings to remove it.";
    case "not_configured": return "Latch is missing information it needs to log in.";
    case "nothing_to_start_again": return "Latch has no saved start settings. Try starting Latch again.";
    default: return null;
  }
}

/** An error as the app says it: its sentence (none under a title that
 * already says what failed, when the app has no sentence of its own),
 * then latch's own words under a collapsed Details. */
export function errorText(failure, { host = null, titled = false, said = null, place = null } = {}) {
  const words = said ?? sentence(failure, { host, place }) ?? (titled ? null : FALLBACK);
  const more = details(failure);
  return el("div", { class: "ag-error" },
    words ? el("span", {}, words) : null,
    more ? el("details", {}, el("summary", {}, "Details"), el("div", { class: "ag-details" }, more)) : null);
}
