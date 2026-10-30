// Home for the selected host, as the Mac app's (Main/HomeView.swift): what
// needs the person (this PC's own host only), today's requests and the
// latest five, and the host's rules, vault and plugins. Today and Recent
// read the host's activity feed, the one Activity shows (activity.js),
// which runs while either screen shows it.

import { el, icon, button, clock, sameDay } from "../dom.js";
import { said } from "../latch.js";
import * as records from "../records.js";
import * as errors from "../errors.js";
import { feed, watch, unwatch, reachBack, caller, openActivity, openRecord } from "./activity.js";

/** What needs you follows this PC's status as it changes. */
export const followsStatus = true;

export async function enter(app, host) {
  if (!host) return;
  const reads = [watch(app, host)];
  if (host.thisPC || host.online !== false) {
    reads.push(app.read(host, "rules", ["rules", "show", ...app.on(host)]));
    reads.push(app.read(host, "vault", ["vault", "list", "--all", ...app.on(host)]));
  }
  if (!host.thisPC && host.online !== false) reads.push(app.read(host, "status", ["status", "--host", host.name]));
  await Promise.all(reads);
}

/** Another place shows: the feed stops after its grace, unless Activity
 * (which shares it) shows next. */
export function leave(app) {
  unwatch(app);
}

export function render(app, host) {
  const viewing = app.viewerOnly() ? viewer(app) : null;
  if (!host) {
    return [viewing, el("div", { class: "empty" }, icon("server", "icon big"), el("h3", {}, "No hosts yet"),
      el("p", {}, "To add a host, install and start Latch on another computer, or start hosting on this PC."))];
  }
  if (!host.thisPC && host.online === false) return [viewing, offline(host)];
  return [
    viewing,
    host.thisPC ? needs(app) : null,
    ...activity(app, host),
    onHost(app, host),
  ];
}

/** Logged in with nothing hosted here: say so, and offer to start. */
function viewer(app) {
  const name = app.local?.this_machine.name ?? "";
  return section(null, el("div", { class: "line need" }, icon("pc", "icon warn"),
    el("span", { class: "text" }, el("b", {}, `This PC (${name}) isn't hosting.`),
      el("small", {}, "You're viewing your hosts. Your agents' work runs on them, not here.")),
    button("Start hosting on this PC…", () => app.askToHost())));
}

function section(title, ...rows) {
  return el("section", { class: "group" }, title ? el("h3", {}, title) : null, el("div", { class: "card" }, ...rows));
}

/** A row that opens another screen, with the chevron that says so. */
function goRow(label, value, onclick, { tone } = {}) {
  return el("button", { class: "line go", onclick },
    el("span", {}, label), el("span", { class: `value${tone ? ` ${tone}` : ""}` }, value), icon("chevron", "chevron"));
}

function line(label, value) {
  return el("div", { class: "line" }, el("span", {}, label), el("span", { class: "value" }, value));
}

function need(symbol, text, detail, action) {
  return el("div", { class: "line need" }, icon(symbol, "icon warn"),
    el("span", { class: "text" }, text, detail ? el("small", {}, detail) : null), action ?? null);
}

/** Home's own Start Latch: whether it is starting, and how its last try
 * failed, shown on its row. */
const startedHere = (app) => (app.pages.home ??= { starting: false, error: null });

async function start(app) {
  const here = startedHere(app);
  if (here.starting || !app.start) return;
  here.starting = true;
  here.error = null;
  app.drawPage();
  here.error = (await app.start()) ?? null;
  here.starting = false;
  app.drawPage();
}

function needs(app) {
  const rows = [];
  if (app.localError) rows.push(need("warning", "Couldn't check this PC.", said(app.localError)));
  const status = app.local;
  if (status) {
    const hosting = status.this_machine.hosting;
    if (hosting.blocked) rows.push(need("warning", "Latch can't run on this PC right now.", said(hosting.blocked)));
    else if (hosting.state === "stopped" || hosting.state === "error") {
      rows.push(need("pause", "Latch isn't running on this PC, so your agents can't reach it.",
        startedHere(app).error ? errors.errorText(startedHere(app).error, { tone: "bad" })
          : status.last_error ? said(status.last_error) : "Start it to let them in again.",
        button(startedHere(app).starting ? "Starting…" : "Start Latch", () => start(app), { disabled: startedHere(app).starting })));
    }
    if (status.update?.available) {
      const build = status.update.build ? ` (build ${status.update.build})` : "";
      rows.push(need("update", `A new version of Latch is available${build}.`, "Install the update from a terminal."));
    }
  }
  if (!rows.length) rows.push(el("div", { class: "line quiet" }, "Nothing needs you right now."));
  return section("Needs you", ...rows);
}

/** Today's requests and the newest five, the owner's own reads left out
 * (`HomeActivity`). */
function summary(list) {
  const now = new Date();
  const out = { today: 0, refused: 0, agents: new Set(), recent: [] };
  for (const record of [...list].reverse()) {
    const when = records.time(record);
    const earlier = !sameDay(when, now) && when < now;
    if (earlier && out.recent.length >= 5) break;
    if (records.isBackground(record)) continue;
    if (out.recent.length < 5) out.recent.push(record);
    if (earlier) continue;
    out.today += 1;
    if (records.isRefused(record)) out.refused += 1;
    if (record.agent) out.agents.add(record.caller_name ?? `Agent ${record.agent.slice(0, 8)}`);
  }
  return out;
}

function activity(app, host) {
  const shown = feed(app, host);
  const list = shown.records;
  const calls = summary(list);
  if (!calls.recent.length && shown.error) {
    const none = errors.code(shown.error) === "no_host_here";
    return [section("Activity", el("div", { class: "line" }, none ? "This PC keeps no activity yet." : "Couldn't read the activity."),
      none ? null : el("div", { class: "line quiet" }, errors.errorText(shown.error, { host: errors.spoken(host), titled: true })))];
  }
  if (!calls.recent.length && !shown.caughtUp) return [section("Today", el("div", { class: "line quiet" }, "Reading…"))];
  // The newest page may be all connections and reads: reach back for five
  // requests, or to the start.
  reachBack(app, host);
  // Only the newest records are held: if the oldest held is from today,
  // there were more today than these.
  const more = shown.hasOlder && list[0] && sameDay(records.time(list[0]), new Date()) ? "+" : "";
  const agents = [...calls.agents].sort();
  return [
    section("Today",
      goRow("Requests", `${calls.today}${more}`, () => openActivity(app, host)),
      goRow("Refused", `${calls.refused}${more}`, () => openActivity(app, host, { filter: "Refused" }), { tone: calls.refused ? "bad" : "" }),
      line("Agents that called", agents.length ? agents.join(", ") : "None")),
    section("Recent activity",
      calls.recent.length ? null : el("div", { class: "line quiet" }, shown.loadingOlder ? "Looking further back…" : "No requests yet."),
      calls.recent.map((record) => recent(app, record, host, () => openRecord(app, record, host))),
      // Home reaches back on its own only so far; the rest is a click away.
      calls.recent.length < 5 && shown.hasOlder && !shown.loadingOlder && list[0]
        ? el("div", { class: "line" }, button("Load Earlier", () => shown.loadOlder()))
        : null),
  ];
}

function recent(app, record, host, onclick) {
  const who = caller(app, host, record);
  const status = records.status(record);
  return el("button", { class: "line go recent", onclick },
    el("span", { class: "mono" }, record.command ?? "—"),
    el("span", { class: "value" }, `${who.who} · ${who.kind}`),
    el("span", { class: "state" }, el("i", { class: `pip ${status.tone}` }), status.title),
    el("span", { class: "time" }, clock(records.time(record))),
    icon("chevron", "chevron"));
}

function onHost(app, host) {
  const summary = app.summaries[host.name] ?? {};
  const show = (read, words) => (read ? (read.error ? "Couldn't check" : words(read.value)) : "Checking…");
  const plugins = host.thisPC ? { value: app.local?.plugins ?? [] } : summary.status && (summary.status.error ? summary.status : { value: summary.status.value?.plugins ?? [] });
  const google = plugins?.value?.some((p) => p.name === "google" && p.state === "enabled");
  return section(`On ${host.name}`,
    goRow("Rules", show(summary.rules, rulesWords), () => app.go("rules")),
    goRow("Vault", show(summary.vault, vaultWords), () => app.go("vault")),
    goRow("Plugins", show(plugins, pluginWords), () => app.go("settings")),
    google ? goRow("Google accounts", "In Connections", () => app.go("connections")) : null,
    host.thisPC ? el("div", { class: "line" }, button("Set up again…", () => app.setUpAgain?.())) : null);
}

function rulesWords(rules) {
  const every = rules.every_agent ? "Set for every agent" : "None for every agent";
  const own = rules.agents?.length ?? 0;
  if (own === 0) return every;
  return `${every} · ${own === 1 ? "1 agent" : `${own} agents`} with ${own === 1 ? "its" : "their"} own rules`;
}

function vaultWords(vault) {
  const n = vault.items?.length ?? 0;
  return n === 0 ? "No items" : n === 1 ? "1 item" : `${n} items`;
}

function pluginWords(list) {
  const name = (p) => p.name[0].toUpperCase() + p.name.slice(1);
  const on = list.filter((p) => p.state === "enabled").map(name);
  const off = list.filter((p) => p.state !== "enabled").map(name);
  return [on.length ? `${on.join(", ")}: on` : null, off.length ? `${off.join(", ")}: off` : null].filter(Boolean).join(" · ");
}

function offline(host) {
  let seen = "";
  if (host.lastSeen) {
    seen = sameDay(host.lastSeen, new Date())
      ? `Plow last saw it today at ${clock(host.lastSeen)}. `
      : `Plow last saw it ${host.lastSeen.toLocaleDateString([], { month: "long", day: "numeric" })} at ${clock(host.lastSeen)}. `;
  }
  return el("div", { class: "empty" }, icon("wifiOff", "icon big"), el("h3", {}, `${host.name} is offline`),
    el("p", {}, `${seen}Start Latch on ${host.name} to reach it again.`));
}
