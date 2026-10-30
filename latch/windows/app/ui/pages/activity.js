// Activity for the selected host, as the Mac app's (Main/ActivityView.swift,
// Main/ActivityController.swift, App/ActivityFeed.swift): the host's records
// as `latch audit --follow` streams them, in a table newest first unless
// sorted otherwise, under the outcome and agent filters and over a footer
// that counts them; why there are none in their place (offline, reading,
// couldn't read, nothing yet, nothing matching). A record selected opens in
// the inspector (Main/RecordInspector.swift).

import { el, icon } from "../dom.js";
import { latchLines, latchFile, latchText, follow, unfollow, said, faked } from "../latch.js";
import * as records from "../records.js";
import * as errors from "../errors.js";

export const search = "Search requests";
/** Drawn again when this PC's status changes: a feed waiting for this PC to
 * host follows once it does. */
export const followsStatus = true;

const FILTERS = ["All", "Allowed", "Refused"];
const COLUMNS = [["time", "Time"], ["status", "Status"], ["request", "Request"], ["caller", "Caller"]];
/** Newest first: Time sorts descending first, the others ascending. */
const NEWEST_FIRST = { key: "time", ascending: false };
const ROW_HEIGHT = 32;

/** What Activity keeps while the app runs: each host's feed, the filters
 * (kept as hosts change), the selection and the sort. */
function state(app) {
  return (app.pages.activity ??= {
    feeds: {},
    filter: "All",
    connections: false,
    /** The one agent asked for, on one host: `{ host, agent }`. */
    agent: null,
    /** The record selected, on the host it was selected on: `{ host, id }`. */
    selected: null,
    sort: savedSort(),
    /** The host whose feed this screen watches while it shows. */
    watching: null,
    /** The rows last drawn, to keep the list where it was on screen. */
    drawn: null,
    /** The record the inspector shows, and what it read for it. */
    shots: null,
    judge: null,
  });
}

// The sort, kept across launches as the Mac's table keeps it.
function savedSort() {
  try {
    const sort = JSON.parse(localStorage.getItem("activity.sort"));
    if (COLUMNS.some(([key]) => key === sort?.key)) return { key: sort.key, ascending: Boolean(sort.ascending) };
  } catch {}
  return { ...NEWEST_FIRST };
}

function saveSort(sort) {
  try { localStorage.setItem("activity.sort", JSON.stringify(sort)); } catch {}
}

// ── the feed ──────────────────────────────────────────────────────────────

/** How many of the newest records a first read (or one after latch forgot
 * where the last left off) starts with. */
const OPENING_PAGE = 500;
/** The earlier pages Home reads on its own looking for five requests. */
const HOME_PAGES = 2;
/** The least time between two changes to the records. */
const BATCH = 100;
/** The wait before following again after the stream dropped; twice as long
 * each time after, up to a minute. */
const FIRST_RETRY = 2_000;
/** How long the stream keeps running after the last screen showing it goes. */
const GRACE = 30_000;
/** Errors that waiting won't fix; `no_host_here` until this PC hosts. */
const FINAL = new Set(["no_host_here", "owner_only", "usage", "audit_broken", "unknown_host", "not_logged_in", "record_too_large"]);
let streams = 0;

/** One host's audit as `latch audit --follow` streams it
 * (App/ActivityFeed.swift): the newest 500 records, then each record as
 * it's written, kept with no limit, so a selected record never goes from
 * under the person. Home and Activity share it: it runs while either shows
 * it and for a grace after, and picks up after the last record it got when
 * it starts again, every record since, none missed or twice. */
class Feed {
  constructor(host, on, changed) {
    this.host = host;
    this.on = on;
    this.changed = changed;
    /** Oldest first. */
    this.records = [];
    this.ids = new Set();
    /** Records older than the first held exist on the host. */
    this.hasOlder = false;
    /** Every record up to now has arrived; `caughtUps` counts the times. */
    this.caughtUp = false;
    this.caughtUps = 0;
    /** Why the stream stopped (latch's failure), until it runs again. */
    this.error = null;
    this.cursor = null;
    this.running = false;
    this.watchers = 0;
    this.stream = null;
    this.wait = FIRST_RETRY;
    this.loadingOlder = false;
    /** How it reaches latch: latch.js's, or a fake-mode still's. */
    this.io = { follow, lines: latchLines };
  }

  /** A screen showing it appeared: the stream starts if it isn't running. */
  start() {
    clearTimeout(this.release);
    this.release = null;
    this.watchers += 1;
    if (!this.running) this.run();
  }

  /** This PC hosts now: if the stream stopped because it never had, follow
   * again. Does nothing otherwise. */
  retry() {
    if (this.watchers > 0 && !this.running && errors.code(this.error) === "no_host_here") this.run();
  }

  /** A screen showing it went: the last one stops the stream once the
   * grace has passed with none back. */
  stop() {
    this.watchers = Math.max(0, this.watchers - 1);
    if (this.watchers > 0 || !this.running) return;
    clearTimeout(this.release);
    this.release = setTimeout(() => this.watchers === 0 && this.end(), GRACE);
  }

  /** Stops the stream now; the records and where they got to stay. */
  end() {
    clearTimeout(this.release);
    clearTimeout(this.retrying);
    this.release = this.retrying = null;
    this.watchers = 0;
    if (this.stream) unfollow(this.stream);
    this.stream = null;
    this.drop?.();
    this.running = false;
  }

  /** Follows from the cursor (or the newest 500), the lines taken a batch
   * at a time: at most one change every 100 ms, at once on catching up. */
  run() {
    this.running = true;
    const id = `activity-${++streams}`;
    this.stream = id;
    let held = [];
    let timer = null;
    let taken = Date.now();
    const take = () => {
      clearTimeout(timer);
      timer = null;
      taken = Date.now();
      const lines = held;
      held = [];
      if (!lines.length) return;
      this.apply(lines);
      this.wait = FIRST_RETRY;
      this.changed(this);
    };
    this.drop = () => { clearTimeout(timer); held = []; };
    const from = this.cursor ? ["--after", this.cursor] : ["--last", String(OPENING_PAGE)];
    this.io.follow(id, ["audit", "--follow", ...from, ...this.on], (line) => {
      if (this.stream !== id) return;
      held.push(line);
      if (line.type === "caught_up" || Date.now() >= taken + BATCH) take();
      else timer ??= setTimeout(take, taken + BATCH - Date.now());
    }, (failure) => {
      if (this.stream !== id) return;
      take();
      unfollow(id);
      this.stream = null;
      this.ended(failure ?? null);
    });
  }

  /** The stream ended: picked up again after the last record, waiting
   * longer each time, unless waiting won't change it. */
  ended(failure) {
    const code = errors.code(failure);
    if (code === "cursor_expired") {
      // Starts again from the newest records.
      Object.assign(this, { cursor: null, records: [], ids: new Set(), hasOlder: false, caughtUp: false });
      this.changed(this);
      return this.run();
    }
    if (failure) Object.assign(this, { error: failure, caughtUp: false });
    if (failure && FINAL.has(code)) {
      this.running = false;
      return this.changed(this);
    }
    this.changed(this);
    this.retrying = setTimeout(() => { this.retrying = null; this.run(); }, this.wait);
    this.wait = Math.min(this.wait * 2, 60_000);
  }

  /** Lines in the order sent (`ActivityFeed.apply`). */
  apply(lines) {
    const added = [];
    let { cursor, hasOlder, caughtUp } = this;
    let cleared = false;
    const empty = this.records.length === 0;
    for (const line of lines) {
      if (line.type === "snapshot") {
        if (empty && !added.length) hasOlder = line.has_older ?? false;
      } else if (line.type === "record") {
        cursor = line.cursor;
        cleared = true;
        if (!this.ids.has(line.record.id)) {
          this.ids.add(line.record.id);
          added.push(line.record);
        }
      } else if (line.type === "caught_up") {
        cursor = line.cursor;
        if (empty && !added.length) hasOlder = line.has_older ?? false;
        caughtUp = true;
        cleared = true;
      }
    }
    if (caughtUp && !this.caughtUp) this.caughtUps += 1;
    Object.assign(this, { cursor, hasOlder, caughtUp });
    if (cleared) this.error = null;
    for (const record of added) this.records.push(record);
  }

  /** "Load Earlier": the page before the first record held
   * (`audit --before ID`). */
  async loadOlder() {
    const first = this.records[0];
    if (!first || this.loadingOlder) return;
    this.loadingOlder = true;
    this.changed(this);
    try {
      const page = records.page(await this.io.lines(["audit", "--before", first.id, ...this.on]));
      const earlier = page.records.filter((record) => !this.ids.has(record.id) && this.ids.add(record.id));
      this.records = earlier.concat(this.records);
      this.hasOlder = page.hasOlder;
    } catch (failure) {
      this.error = failure;
    } finally {
      this.loadingOlder = false;
      this.changed(this);
    }
  }

  /** Earlier pages until `requests` records aren't connections or the
   * owner's reads, there are no more, or `pages` were read (Home's Recent,
   * which offers the rest a page at a time). */
  async loadOlderUntil(requests, pages) {
    for (let page = 0; page < pages; page += 1) {
      if (!this.hasOlder || this.error || this.records.filter((r) => !records.isBackground(r)).length >= requests) return;
      const held = this.records.length;
      await this.loadOlder();
      if (this.records.length === held) return;
    }
  }

  /** A record opened from elsewhere that the feed doesn't hold (it started
   * again from the newest since): held in its place by time, so Activity
   * lists it and the inspector shows it. */
  hold(record) {
    if (this.ids.has(record.id)) return;
    this.ids.add(record.id);
    const at = this.records.findIndex((held) => records.time(held) > records.time(record));
    this.records.splice(at < 0 ? this.records.length : at, 0, record);
  }
}

/** The screens that show a host's feed, and redraw as it changes. */
const SHOWS_FEED = ["home", "activity"];

/** A host's feed, made the first time it's asked for (`AppModel.feed(for:)`).
 * One that stopped because this PC had never hosted follows again once it
 * does (`AppModel.statusChanged`). */
export function feed(app, host) {
  const s = state(app);
  const shown = (s.feeds[host.name] ??= new Feed(host.name, app.on(host), (changed) => {
    if (SHOWS_FEED.includes(app.place) && s.watching === changed.host) app.drawPage();
  }));
  if (host.thisPC && app.local?.this_machine.hosting.state === "serving") shown.retry();
  return shown;
}

/** Fake mode's stills (fake/activity.js); never in the app. */
let stills;
async function staged() {
  if (!faked()) return null;
  return (stills ??= import("../fake/activity.js").catch(() => null));
}

/** Home or Activity shows `host`'s feed: it runs (`MainWindow`'s
 * `activityFeed`). Going between the two keeps it running. */
export async function watch(app, host) {
  const s = state(app);
  s.watching = null;
  if (!host || (!host.thisPC && host.online === false)) return;
  const still = await staged();
  // Left meanwhile, or another screen already watches it: nothing to do.
  if (!SHOWS_FEED.includes(app.place) || app.hostName !== host.name || s.watching) return;
  const shown = feed(app, host);
  s.watching = host.name;
  if (still?.agent && !s.agent) s.agent = still.agent;
  const io = still?.io?.(host.name);
  if (io) shown.io = { ...shown.io, ...io };
  const staging = still?.feed(host.name);
  if (!staging) return shown.start();
  // As the Mac's `ActivityFeed.stage` leaves it: not following.
  if (!shown.staged) {
    shown.apply(staging.lines);
    shown.error = staging.error ?? null;
    shown.staged = true;
  }
}

/** The screen showing the feed went: it stops after its grace. */
export function unwatch(app) {
  const s = app.pages.activity;
  if (!s?.watching) return;
  s.feeds[s.watching]?.stop();
  s.watching = null;
}

export function enter(app, host) {
  return watch(app, host);
}

/** Another place shows (or another host's Activity). */
export function leave(app) {
  closeMenu();
  unwatch(app);
}

/** A host's Activity, or one agent's there, with every outcome or the one
 * asked for, no search, and no connections or owner's reads
 * (`AppModel.openActivity`): for Home and Agents. */
export function openActivity(app, host, { agent = null, filter = "All" } = {}) {
  const s = state(app);
  app.hostName = host.name;
  s.agent = agent ? { host: host.name, agent } : null;
  s.filter = filter;
  s.connections = false;
  app.search.activity = "";
  app.go("activity");
}

/** One request on a host, selected and in the inspector; filters that
 * would hide it go (`AppModel.openRecord`): for Home's recent rows. Home
 * shows the feed's own records, so the feed holds it; one it no longer
 * holds is held again. */
export function openRecord(app, record, host) {
  const s = state(app);
  if (!visible(app, host, [record]).length) {
    s.agent = null;
    s.filter = "All";
    s.connections = false;
    app.search.activity = "";
  }
  feed(app, host).hold(record);
  s.selected = { host: host.name, id: record.id };
  s.reveal = true;
  app.inspecting.activity = true;
  app.hostName = host.name;
  app.go("activity");
}

/** Home's reach back for its five requests once the feed has caught up
 * (each time it does), a few pages at most. */
export function reachBack(app, host) {
  const shown = feed(app, host);
  if (!shown.caughtUp || shown.reached === shown.caughtUps || shown.staged) return;
  shown.reached = shown.caughtUps;
  shown.loadOlderUntil(5, HOME_PAGES);
}

// ── which rows ───────────────────────────────────────────────────────────

/** The key on records of a host's requests of itself: this PC's own, else
 * Plow's for that host. */
function keyed(app, host) {
  return host.thisPC ? { ...host, key: app.local?.this_machine.key ?? host.key } : host;
}

/** Who called, as Activity and Home say it: "scout · agent", "You · on this PC". */
export const caller = (app, host, record) => records.caller(record, keyed(app, host));

/** What Activity lists of a host's records, newest first: those its filters
 * all match. The agent is matched by uid, never by name. */
function visible(app, host, list) {
  const s = state(app);
  const agent = s.agent?.host === host.name ? s.agent.agent : null;
  const query = (app.search.activity ?? "").trim().toLowerCase();
  const out = [];
  for (let i = list.length - 1; i >= 0; i -= 1) {
    const record = list[i];
    if (!s.connections && records.isBackground(record)) continue;
    if (agent && record.agent !== agent) continue;
    if (s.filter === "Allowed" && record.decision !== "allow") continue;
    if (s.filter === "Refused" && !records.isRefused(record)) continue;
    if (query) {
      const who = caller(app, host, record);
      const words = [record.command ?? "", who.who, who.kind, records.status(record).title];
      if (!words.some((word) => word.toLowerCase().includes(query))) continue;
    }
    out.push(record);
  }
  return out;
}

const timeText = (record) => records.time(record).toLocaleTimeString([], { hour: "numeric", minute: "2-digit", second: "2-digit" });

function text(app, host, record, column) {
  switch (column) {
    case "time": return timeText(record);
    case "status": return records.status(record).title;
    case "request": return record.command ?? "—";
    default: return caller(app, host, record).who;
  }
}

/** The table's order of a host's rows (which come newest first); ties keep
 * the newest first. */
function sorted(app, host, rows) {
  const { key, ascending } = state(app).sort;
  if (key === "time") return ascending ? rows.reverse() : rows;
  const keys = new Map(rows.map((record) => [record, text(app, host, record, key)]));
  const order = (a, b) => keys.get(a).localeCompare(keys.get(b), undefined, { numeric: true, sensitivity: "base" });
  return rows.sort((a, b) => (ascending ? order(a, b) : order(b, a)));
}

/** The record the inspector shows: the one selected on this host, while
 * Activity lists it. */
function inspected(app, host) {
  const s = state(app);
  if (s.selected?.host !== host.name) return null;
  const list = s.feeds[host.name]?.records ?? [];
  const record = list.findLast((r) => r.id === s.selected.id);
  return record && visible(app, host, [record]).length ? record : null;
}

// ── the screen ───────────────────────────────────────────────────────────

export function render(app, host) {
  const s = state(app);
  const offline = !host.thisPC && host.online === false;
  const shown = offline ? null : feed(app, host);
  const rows = shown ? sorted(app, host, visible(app, host, shown.records)) : [];
  keep(app, host, rows);
  return el("div", { class: "activity" },
    heading(app, host, shown),
    rows.length ? table(app, host, rows) : states(app, host, shown),
    shown ? footer(host, shown, rows) : null);
}

/** Activity's heading, then its scope bar: the outcome, the agent asked for
 * (a click removes it) and Show Connections and Reads. */
function heading(app, host, shown) {
  const s = state(app);
  const outcome = el("div", { class: "segmented", role: "radiogroup", "aria-labelledby": "activity-show" },
    FILTERS.map((name) => el("button", {
      class: `seg${s.filter === name ? " on" : ""}`, role: "radio", "aria-checked": String(s.filter === name), "data-filter": name,
      onclick: () => { s.filter = name; app.drawPage(); },
    }, name)));
  let chip = null;
  if (s.agent?.host === host.name) {
    const uid = s.agent.agent;
    const named = shown?.records.findLast((r) => r.agent === uid && r.caller_name)?.caller_name;
    const name = agentNames(app).get(uid) ?? named ?? "Unnamed agent";
    chip = el("button", {
      class: "chip", title: "Show every agent's requests", "aria-label": `Remove filter: agent ${name}`,
      onclick: () => { s.agent = null; app.drawPage(); },
    }, glyph("dismiss"), `Agent: ${name}`);
  }
  return el("div", { class: "heading" },
    el("div", { class: "titled" },
      el("h2", {}, "Activity"),
      el("button", {
        class: `toggle${s.connections ? " on" : ""}`, "aria-pressed": String(s.connections),
        "aria-label": "Show Connections and Reads",
        title: "Also show when callers connected, and your own reads (rules, vault, status, activity)",
        onclick: () => { s.connections = !s.connections; app.drawPage(); },
      }, glyph("connections"))),
    el("div", { class: "scope" }, el("span", { class: "show" }, el("span", { id: "activity-show" }, "Show"), outcome), chip));
}

/** The account's agents by uid, if the Agents page has read them. */
function agentNames(app) {
  const list = app.summaries[""]?.agents?.value?.agents ?? [];
  return new Map(list.map((agent) => [agent.uid, agent.name]));
}

function table(app, host, rows) {
  const s = state(app);
  const selected = s.selected?.host === host.name ? s.selected.id : null;
  const header = el("div", { class: "tr th", role: "row" },
    COLUMNS.map(([key, title]) => {
      const on = s.sort.key === key;
      return el("div", { role: "columnheader", "aria-sort": on ? (s.sort.ascending ? "ascending" : "descending") : "none" },
        el("button", {
          class: `sort${on ? " on" : ""}`,
          onclick: () => {
            // Again: the other way; another column: its own first way.
            s.sort = on ? { key, ascending: !s.sort.ascending } : { key, ascending: key !== "time" };
            saveSort(s.sort);
            app.drawPage();
          },
        }, title, on ? icon("chevron", `chevron ${s.sort.ascending ? "up" : "down"}`) : null));
    }));
  const body = rows.map((record) => row(app, host, record, record.id === selected));
  return el("div", {
    class: "table", role: "grid", tabindex: "0", "aria-label": "Requests", "aria-rowcount": String(rows.length + 1),
    onkeydown: (event) => keys(event, app, host, rows),
  }, header, body);
}

function row(app, host, record, selected) {
  const status = records.status(record);
  const who = caller(app, host, record);
  return el("div", {
    class: `tr${selected ? " selected" : ""}`, role: "row", "aria-selected": String(selected), "data-id": record.id,
    onclick: () => select(app, host, record.id),
    // A double-click inspects the row: the inspector opens if it's closed.
    ondblclick: () => {
      if (app.inspecting.activity) return;
      app.inspecting.activity = true;
      app.draw();
    },
    oncontextmenu: (event) => {
      event.preventDefault();
      rowMenu(app, host, record, event.clientX, event.clientY);
    },
  },
    el("div", { role: "gridcell", class: "when" }, timeText(record)),
    el("div", { role: "gridcell", class: "verdict", title: status.title }, el("i", { class: `pip ${status.tone}` }), el("span", {}, status.title)),
    el("div", { role: "gridcell", class: "asked mono", title: record.command ?? "A connection" }, middle(record.command ?? "—")),
    el("div", { role: "gridcell", class: "who" }, `${who.who} `, el("span", {}, `· ${who.kind}`)));
}

/** A request cut short in its middle, as the Mac's table cuts it: the
 * start gives way, the end stays. */
function middle(text) {
  if (text.length <= 18) return text;
  const cut = text.length - 12;
  return [el("span", { class: "start" }, text.slice(0, cut)), el("span", { class: "tail" }, text.slice(cut))];
}

function select(app, host, id, { reveal = false } = {}) {
  const s = state(app);
  s.selected = id ? { host: host.name, id } : null;
  s.reveal = reveal;
  app.drawPage();
}

/** The table's keys: the arrows, Home and End move the selection; Ctrl+C
 * copies the request; the menu key opens the row's menu. */
function keys(event, app, host, rows) {
  const s = state(app);
  const at = rows.findIndex((r) => s.selected?.host === host.name && r.id === s.selected.id);
  const page = Math.max(1, Math.floor((event.currentTarget.clientHeight - ROW_HEIGHT) / ROW_HEIGHT) - 1);
  const to = {
    ArrowDown: at < 0 ? 0 : at + 1, ArrowUp: at < 0 ? 0 : at - 1,
    PageDown: at + page, PageUp: Math.max(0, at - page), Home: 0, End: rows.length - 1,
  }[event.key];
  if (to !== undefined) {
    event.preventDefault();
    const next = rows[Math.min(Math.max(to, 0), rows.length - 1)];
    if (next && next.id !== s.selected?.id) select(app, host, next.id, { reveal: true });
    return;
  }
  const record = rows[at];
  if (!record) return;
  if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "c") {
    event.preventDefault();
    copy(record);
  } else if (event.key === "ContextMenu" || (event.shiftKey && event.key === "F10")) {
    event.preventDefault();
    const box = event.currentTarget.querySelector(`[data-id="${record.id}"]`)?.getBoundingClientRect();
    rowMenu(app, host, record, box ? box.left + 24 : 0, box ? box.bottom : 0);
  }
}

/** Edit › Copy: the request, or its status for a connection. */
function copy(record) {
  navigator.clipboard?.writeText(record.command ?? records.status(record).title).catch(() => {});
}

// ── the row's menu ───────────────────────────────────────────────────────

let openMenu = null;

function closeMenu() {
  openMenu?.();
  openMenu = null;
}

/** Copy Request, and Show Only an agent's Requests while not narrowed to it. */
function rowMenu(app, host, record, x, y) {
  const s = state(app);
  const items = [["Copy Request", () => copy(record)]];
  if (record.agent && !(s.agent?.host === host.name && s.agent.agent === record.agent)) {
    items.push(null, [`Show Only ${record.caller_name ?? "This Agent"}’s Requests`, () => {
      s.agent = { host: host.name, agent: record.agent };
      app.drawPage();
    }]);
  }
  closeMenu();
  const root = document.querySelector(".app") ?? document.body;
  const box = el("div", { class: "activity-menu", role: "menu" },
    items.map((item) => (item ? el("button", { class: "item", role: "menuitem", onclick: () => { closeMenu(); item[1](); } }, item[0]) : el("hr"))));
  root.append(box);
  const { width, height } = box.getBoundingClientRect();
  box.style.left = `${Math.min(x, innerWidth - width - 4)}px`;
  box.style.top = `${Math.min(y, innerHeight - height - 4)}px`;
  box.querySelector("button")?.focus();
  const away = (event) => { if (!box.contains(event.target)) closeMenu(); };
  const escape = (event) => { if (event.key === "Escape") closeMenu(); };
  addEventListener("pointerdown", away, true);
  addEventListener("keydown", escape, true);
  addEventListener("blur", closeMenu);
  openMenu = () => {
    box.remove();
    removeEventListener("pointerdown", away, true);
    removeEventListener("keydown", escape, true);
    removeEventListener("blur", closeMenu);
  };
}

// ── keeping the list in place ────────────────────────────────────────────

/** A redraw builds the list anew: it keeps its place on screen and the
 * keyboard, as the Mac's table does. Records arriving on top of those shown
 * keep those shown where they are, unless the top is in view; another
 * order, filter or host shows the selected record. The inspector keeps its
 * scroll while it shows the same record. */
function keep(app, host, rows) {
  const s = state(app);
  const old = document.querySelector(".activity .table");
  const pane = document.querySelector(".content .inspector");
  const top = old?.scrollTop ?? 0;
  const focused = old != null && old.contains(document.activeElement);
  const paneTop = pane?.scrollTop ?? 0;
  const ids = rows.map((r) => r.id);
  const before = s.drawn;
  const selected = s.selected?.host === host.name ? s.selected.id : null;
  const inspecting = selected;
  const sameRecord = before?.inspecting === inspecting && before?.host === host.name;
  s.drawn = { host: host.name, ids, inspecting };
  let to = top;
  let reveal = s.reveal;
  s.reveal = false;
  if (before?.host === host.name && before.ids.length) {
    const added = ids.length - before.ids.length;
    const onTop = added > 0 && ids.slice(added).every((id, i) => id === before.ids[i]);
    if (onTop) to = top > 0 ? top + added * ROW_HEIGHT : 0;
    else if (ids.length !== before.ids.length || ids.some((id, i) => id !== before.ids[i])) reveal = true;
  } else {
    to = 0;
    reveal = true;
  }
  queueMicrotask(() => {
    const list = document.querySelector(".activity .table");
    if (list) {
      list.scrollTop = to;
      if (reveal && selected) list.querySelector(`[data-id="${selected}"]`)?.scrollIntoView({ block: "nearest" });
      if (focused) list.focus({ preventScroll: true });
    }
    const inspector = document.querySelector(".content .inspector");
    if (inspector && sameRecord) inspector.scrollTop = paneTop;
  });
}

// ── why there are no rows ────────────────────────────────────────────────

function states(app, host, shown) {
  if (!host.thisPC && host.online === false) {
    return empty("wifiOff", `${host.name} is offline`, `Its activity is kept on ${host.name}. Start Latch there to see it.`);
  }
  if (!shown.records.length && shown.error) return unavailable(shown.error, host);
  if (!shown.records.length && !shown.caughtUp) {
    return el("div", { class: "state" }, el("div", { class: "ring", role: "progressbar", "aria-label": "Reading" }),
      el("p", {}, `Reading ${host.thisPC ? "this PC's" : `${host.name}'s`} activity…`));
  }
  if (!shown.records.length) return empty("tray", "No activity yet", `Every request to ${host.name}, allowed or refused, shows up here.`);
  return empty("filter", "No matching requests in the loaded history", "Try another filter.");
}

function empty(symbol, title, ...lines) {
  return el("div", { class: "state" }, symbol === "wifiOff" || symbol === "warning" ? icon(symbol, "icon big") : glyph(symbol, "icon big"),
    el("h3", {}, title), ...lines.filter(Boolean).map((line) => (typeof line === "string" ? el("p", {}, line) : line)));
}

/** Why a host's activity can't be shown, in the app's words. */
function unavailable(error, host) {
  switch (errors.code(error)) {
    case "no_host_here":
      return empty("tray", "No activity on this PC", "This PC has never hosted, so it keeps no activity. Pick one of your hosts to see its own.");
    case "host_offline": case "host_stopped": case "host_unreachable": {
      const fact = errors.code(error) === "host_unreachable" ? null : errors.fact(error, errors.spoken(host));
      const said = [fact, "Latch tries again on its own while this screen is open."].filter(Boolean).join("\n");
      return empty("wifiOff", `Can't reach ${host.name}`, errors.errorText(error, { said }));
    }
    default:
      return empty("warning", "Couldn't read the activity", errors.errorText(error, { host: errors.spoken(host), titled: true }));
  }
}

/** From when the records loaded go back, while there are older ones, and
 * how many of those loaded match. */
function footer(host, shown, rows) {
  const since = shown.hasOlder && shown.records[0]
    ? `Showing since ${records.time(shown.records[0]).toLocaleString([], { dateStyle: "medium", timeStyle: "short" })}` : "";
  const n = rows.length;
  const m = shown.records.length;
  const right = shown.error
    ? el("span", { class: "trouble" }, icon("warning"), errors.short(shown.error, errors.spoken(host)))
    : `${n} ${n === 1 ? "match" : "matches"} · ${m} ${m === 1 ? "record" : "records"} loaded${shown.caughtUp ? "" : " · reading…"}`;
  return el("div", { class: "footer" }, el("span", {}, since), el("span", { class: "counts" }, right));
}

// ── the inspector (Main/RecordInspector.swift) ───────────────────────────

export function inspector(app, host) {
  const record = inspected(app, host);
  if (!record) {
    return el("div", { class: "activity-none" }, glyph("inspect", "icon big"), el("h3", {}, "No Selection"),
      el("p", {}, "Select a request to see who asked, why it was allowed or refused, and what ran."));
  }
  return recordInspector(app, host, record);
}

const shotsOf = (record) => {
  const args = record.details?.args;
  if (record.op !== "browser" || !args) return [];
  return [...(args.step_records ?? []).map((step) => step?.screenshot), args.screenshot].map((shot) => shot?.shot).filter(Boolean);
};
const isScreenshot = (record) => record.op === "browser" && record.details?.args?.command === "screenshot";
const keptRequests = (gate) => [gate.kept_request, gate.layers?.rules?.kept_request, gate.layers?.chat?.kept_request].filter(Boolean);

const GONE = "Kept on the host for 7 days, then removed.";
const STANDALONE = "Saved on the caller's machine; the host doesn't keep it.";
const shotTitle = (id) => (id === "final" ? "Last page" : `After action ${id}`);

/** One audit record in full: who asked, what it could touch, why the
 * gatekeeper allowed or refused it, and what ran. */
function recordInspector(app, host, record) {
  const s = state(app);
  // Read when the record is opened, and again only for another.
  if (s.shots?.id !== record.id) {
    s.judge = { id: record.id, failure: null, token: null };
    s.shots = { id: record.id, shots: shotsOf(record).map((id) => ({ id, state: "loading" })) };
    loadShots(app, host, s.shots);
  }
  const who = caller(app, host, record);
  const status = records.status(record);
  const args = record.details?.args;
  const gate = records.gate(record);
  const sections = [
    section(null,
      el("div", { class: "line" }, el("span", { class: "state" }, el("i", { class: `pip ${status.tone}` }), status.title)),
      record.command ? el("div", { class: "line command mono" }, record.command) : null),
    section("Request",
      labeled("Caller", `${who.who} · ${who.kind}`),
      labeled("Time", records.time(record).toLocaleString([], { dateStyle: "medium", timeStyle: "medium" })),
      labeled("Operation", record.op),
      labeled("Outcome", record.outcome, { mono: true })),
  ];
  if (s.shots.shots.length) sections.push(shotsSection(app, host, s.shots));
  else if (isScreenshot(record)) sections.push(section("Screenshot", el("div", { class: "line quiet" }, STANDALONE)));
  if (record.op === "exec" && args) {
    const writes = args.writes ?? [];
    const secrets = (args.secret_env ?? []).map((secret) => secret?.name).filter(Boolean);
    sections.push(section("What it could touch",
      labeled("Folder", args.workdir ?? "—", { mono: true }),
      labeled("Writes", writes.length ? writes.join("\n") : "None", { mono: true }),
      labeled("Network", args.network === true ? "Yes" : "No"),
      labeled("Secrets", secrets.length ? secrets.join(", ") : "None")));
  }
  const fields = otherFields(record);
  if (fields.length) {
    sections.push(section(record.op === "exec" ? "More" : "Details",
      fields.map(([key, value]) => labeled(label(key), valueText(key, value), { mono: true }))));
  }
  if (gate) {
    sections.push(why(gate, who.who));
    const kept = keptRequests(gate);
    if (kept.length) {
      sections.push(section(null,
        el("div", { class: "line" }, el("button", { onclick: () => openJudged(app, host, record, kept) }, "Gatekeeper request")),
        s.judge.failure ? el("div", { class: "line quiet" }, s.judge.failure) : null));
    }
  }
  if (args && (args.exit_code != null || args.duration_ms != null)) {
    sections.push(section("Ran",
      args.exit_code != null ? labeled("Exit code", String(args.exit_code)) : null,
      args.duration_ms != null ? labeled("Took", `${args.duration_ms} ms`) : null,
      args.stderr_bytes != null ? labeled("Error output", `${args.stderr_bytes} bytes`) : null,
      args.timed_out === true ? labeled("Timed out", "Yes") : null));
  }
  sections.push(section("Record", el("div", { class: "line id mono" }, record.id)));
  return el("div", { class: "activity-record" }, sections.filter(Boolean));
}

function section(title, ...rows) {
  return el("section", { class: "group" }, title ? el("h3", {}, title) : null, el("div", { class: "card" }, ...rows));
}

function labeled(name, value, { mono = false } = {}) {
  return el("div", { class: "line" }, el("span", {}, name), el("span", { class: `value${mono ? " mono" : ""}` }, value));
}

/** The gatekeeper's judgment: what it read, which rules, how sure. */
function why(gate, who) {
  const rows = [];
  if (gate.source) rows.push(labeled("Read", SOURCES[gate.source] ?? gate.source));
  if (gate.rules) {
    const used = [gate.rules.host ? "For every agent" : null, gate.rules.agent ? `${who}'s own` : null].filter(Boolean).join(" + ");
    rows.push(labeled("Rules used", used || "None"));
  }
  if (gate.verdict) rows.push(labeled("Verdict", VERDICTS[gate.verdict] ?? gate.verdict));
  if (gate.p) {
    rows.push(likelihood("Covered", gate.p.COVERED, "ok"), likelihood("Not covered", gate.p.NOT_COVERED, "warn"),
      likelihood("Forbidden", gate.p.FORBIDDEN, "bad"));
  }
  if (gate.model != null || gate.latency_ms != null || gate.cost != null || gate.rule != null) {
    const cost = gate.cost != null
      ? gate.cost.toLocaleString("en-US", { style: "currency", currency: "USD", minimumSignificantDigits: 2, maximumSignificantDigits: 2 }) : null;
    rows.push(el("details", { class: "more" }, el("summary", {}, "Details"),
      gate.model != null ? labeled("Model", gate.model, { mono: true }) : null,
      gate.latency_ms != null ? labeled("Took", `${gate.latency_ms} ms`) : null,
      cost ? labeled("Cost", cost) : null,
      gate.rule != null ? labeled("Rule", gate.rule) : null));
  }
  return rows.filter(Boolean).length ? section("Why", rows) : null;
}

const SOURCES = {
  owner_request: "Your own request",
  defaults: "Defaults only (no rules, no chat)",
  rules: "Defaults and rules (no chat)",
  chat_and_rules: "Defaults, rules and your chat with the agent",
  chat: "Defaults and your chat with the agent",
};
const VERDICTS = { allow: "Allowed", not_covered: "Needed your OK", owner_said_no: "You said no" };

function likelihood(title, value, tint) {
  if (typeof value !== "number") return null;
  const percent = value.toLocaleString([], { style: "percent", maximumFractionDigits: 0 });
  return el("div", { class: "line likely" },
    el("span", { class: "said" }, el("span", {}, title), el("span", { class: "value" }, percent)),
    el("span", { class: `meter ${tint}`, "aria-hidden": "true" }, el("i", { style: `width: ${Math.round(Math.min(Math.max(value, 0), 1) * 100)}%` })));
}

/** Everything else the record holds, by key: each op's own args, and any
 * detail beyond args, the judgment and the start time. Exec's fields shown
 * above aren't repeated. */
function otherFields(record) {
  const detail = record.details ?? {};
  const fields = [];
  const byKey = (a, b) => (a[0] < b[0] ? -1 : a[0] > b[0] ? 1 : 0);
  const args = detail.args;
  if (args && typeof args === "object" && !Array.isArray(args)) {
    // A browser's judgment is in its args; Why shows it.
    const shownAbove = record.op === "exec"
      ? ["argv", "workdir", "writes", "network", "secret_env", "exit_code", "duration_ms", "stderr_bytes", "timed_out"] : ["gate"];
    fields.push(...Object.entries(args).filter(([key, value]) => !shownAbove.includes(key) && value !== null).sort(byKey));
  }
  fields.push(...Object.entries(detail).filter(([key, value]) => !["args", "gate", "started"].includes(key) && value !== null).sort(byKey));
  return fields;
}

function label(key) {
  if (key === "sha256") return "SHA-256";
  if (key === "url") return "URL";
  const words = key.replaceAll("_", " ");
  return words[0].toUpperCase() + words.slice(1);
}

function valueText(key, value) {
  if ((key.endsWith("size") || key.endsWith("bytes")) && typeof value === "number") return bytes(value);
  if (key.endsWith("_ms") && typeof value === "number") return `${Math.trunc(value)} ms`;
  return words(value);
}

/** Any JSON value in words, one line per item: scalars as they are, an
 * object as "key: value" pairs (`JSONValue.text`). */
function words(value) {
  if (value === null || value === undefined) return "—";
  if (typeof value === "boolean") return value ? "Yes" : "No";
  if (typeof value === "number") return String(value);
  if (typeof value === "string") return value;
  if (Array.isArray(value)) return value.length ? value.map(words).join("\n") : "None";
  return Object.keys(value).sort().map((key) => `${key.replaceAll("_", " ")}: ${inline(value[key])}`).join(", ");
}

function inline(value) {
  if (Array.isArray(value)) return value.map(inline).join(", ");
  if (value && typeof value === "object") return `{${words(value)}}`;
  return words(value);
}

/** A size as Windows' own pages and the Mac's file sizes say it: "13 KB". */
function bytes(n) {
  if (n < 1000) return `${n} ${n === 1 ? "byte" : "bytes"}`;
  const units = [["KB", 0], ["MB", 1], ["GB", 2], ["TB", 2]];
  let size = n;
  for (const [unit, digits] of units) {
    size /= 1000;
    if (size < 1000 || unit === "TB") return `${size.toLocaleString([], { maximumFractionDigits: digits })} ${unit}`;
  }
  return `${n} bytes`;
}

// ── screenshots and the gatekeeper's request ─────────────────────────────

/** Reads each screenshot, in the order taken, held in memory only. */
async function loadShots(app, host, held) {
  const still = await staged();
  for (const shot of held.shots) {
    if (state(app).shots !== held) return;
    const args = ["audit", "shot", held.id, shot.id, "--output", "{file}", ...app.on(host)];
    try {
      shot.src = await (still?.shot ?? latchFile)(args, "png");
      shot.state = "loaded";
    } catch (failure) {
      shot.state = errors.code(failure) === "no_such_shot" ? "gone" : "failed";
      shot.error = failure;
    }
    if (state(app).shots === held && app.place === "activity") app.drawPage();
  }
}

/** A browser plan's screenshots as they arrive; one opens larger. */
function shotsSection(app, host, held) {
  const tiles = held.shots.map((shot) => {
    const title = shotTitle(shot.id);
    let face;
    if (shot.state === "loading") face = el("div", { class: "ring small", role: "progressbar", "aria-label": "Loading" });
    else if (shot.state === "loaded") face = el("img", { src: shot.src, alt: "" });
    else face = glyph(shot.state === "gone" ? "gone" : "warningSmall", "icon");
    const value = { loading: "Loading", loaded: "Screenshot", gone: GONE, failed: shot.error && errors.short(shot.error, host.name) }[shot.state];
    const loaded = shot.state === "loaded";
    return el(loaded ? "button" : "div", {
      class: "tile", "aria-label": `${title}, ${value}`, title: loaded ? "Open larger" : null,
      onclick: loaded ? () => openShot(app, shot, title) : null,
    }, el("span", { class: "face" }, face), el("small", {}, title));
  });
  return section("Screenshots",
    el("div", { class: "line shots" }, el("div", { class: "tiles" }, tiles),
      held.shots.some((shot) => shot.state === "gone") ? el("small", {}, GONE) : null,
      held.shots.filter((shot) => shot.state === "failed").map((shot) => el("small", {}, `${shotTitle(shot.id)}: ${errors.short(shot.error, host.name)}`))));
}

/** A screenshot at its own size, where the Mac opens it in Quick Look. */
function openShot(app, shot, title) {
  let close;
  close = app.sheet([
    el("h2", {}, title),
    el("div", { class: "activity-shot" }, el("img", { src: shot.src, alt: title })),
    el("div", { class: "actions" }, el("button", { onclick: () => close() }, "Close")),
  ], { width: 760 });
}

/** What the gatekeeper sent and got back for this record: each kept call,
 * as latch prints it; an agent's two layers as one JSON list. */
async function openJudged(app, host, record, ids) {
  const s = state(app);
  const token = Symbol(record.id);
  s.judge.token = token;
  const calls = [];
  try {
    for (const id of ids) {
      // latch's `--pretty` answer is many lines, which the app's `run` can't
      // hand over whole; the same JSON, indented here.
      calls.push((await latchText(["--pretty", "audit", "judge", id, ...app.on(host)])).trim());
    }
  } catch (failure) {
    if (s.judge.token !== token) return;
    s.judge.failure = said(failure);
    return app.drawPage();
  }
  // Another record opened meanwhile: this one's no longer shown.
  if (s.judge.token !== token || s.shots?.id !== record.id) return;
  if (s.judge.failure) {
    s.judge.failure = null;
    app.drawPage();
  }
  const text = calls.length === 1 ? calls[0] : `[\n${calls.join(",\n")}\n]`;
  let close;
  close = app.sheet([
    el("h2", {}, "Gatekeeper request"),
    el("pre", { class: "activity-judged mono", tabindex: "0" }, text),
    el("div", { class: "actions" }, el("button", { onclick: () => close() }, "Close")),
  ], { width: 640 });
}

// ── glyphs dom.js doesn't draw yet ───────────────────────────────────────

const GLYPHS = {
  connections: '<circle cx="4.5" cy="6" r="1.8"/><circle cx="15.5" cy="6" r="1.8"/><circle cx="10" cy="15" r="1.8"/><path d="M6.3 6h7.4" stroke-dasharray="1.2 1.6"/><path d="M5.5 7.6l3.5 5.8M14.5 7.6l-3.5 5.8"/>',
  dismiss: '<circle cx="10" cy="10" r="7"/><path d="M7.4 7.4l5.2 5.2M12.6 7.4l-5.2 5.2"/>',
  tray: '<path d="M3 11.5l2-6.5h10l2 6.5v4H3z"/><path d="M3 11.5h4.2a2.8 2.8 0 0 0 5.6 0H17"/>',
  filter: '<circle cx="10" cy="10" r="7.5"/><path d="M6 7.6h8M7.4 10.2h5.2M8.8 12.8h2.4"/>',
  inspect: '<path d="M5 2.8h6.5L15 6.3v4.2"/><path d="M5 2.8v14.4h5"/><path d="M11.5 2.8v3.5H15"/><circle cx="13.4" cy="13.6" r="2.6"/><path d="M15.3 15.5l2 2"/>',
  gone: '<circle cx="9" cy="10" r="6"/><path d="M9 6.8V10l2 1.4"/><path d="M14.6 14.6l3 3M17.6 14.6l-3 3"/>',
  warningSmall: '<path d="M10 3.5l7.2 12.6H2.8z"/><path d="M10 8.4v3.6M10 14.1v.01"/>',
};

function glyph(name, className = "icon") {
  const svg = icon("", className);
  svg.innerHTML = GLYPHS[name] ?? "";
  return svg;
}
