// The selected host's settings, and only that host's, as the Mac app's
// (Main/HostSettingsView.swift, and the start, stop, keep-awake and plugin
// parts of App/AppModel.swift): its state (and, for this PC, start,
// restart and stop), its version (another host's as it says), keep-awake,
// what it refuses agents, and its plugins (on and off only here, for this
// PC). The header names the host.

import { el, icon, button } from "../dom.js";
import { latch } from "../latch.js";
import { hostState, seen } from "../main.js";
import { errorText, details, code, spoken, unreachable } from "./rules.js";

/** A start, stop or restart of this PC's host while it runs, and how it
 * failed; a plugin being turned on or off, and why that failed; a
 * keep-awake save. Kept for the window, as the Mac's AppModel keeps them,
 * so Home can show a start too (`startHosting`). */
function state(app) {
  return (app.pages.settings ??= {
    action: null, actionError: null,
    starting: false, startError: null,
    change: null, changeError: null,
    savingKeepAwake: false, keepAwakeError: null,
  });
}

/** This PC's status read again (as the Mac does after every change here),
 * the host's rules (what it refuses agents), and another host's own
 * status (its version and plugins), asked only of one Plow doesn't count
 * offline. */
export async function enter(app, host) {
  const reads = [refreshStatus(app)];
  if (host.thisPC || host.online !== false) reads.push(app.read(host, "rules", ["rules", "show", ...app.on(host)]));
  if (!host.thisPC && host.online !== false) reads.push(app.read(host, "status", ["status", "--host", host.name]));
  await Promise.all(reads);
}

/** `status --local` into the window's own, which the sidebar and header
 * draw from too. */
async function refreshStatus(app) {
  try {
    app.local = await latch(["status", "--local"]);
    app.localError = null;
  } catch (failure) {
    app.localError = failure;
  }
  app.draw({ page: false });
}

/** `status`, after a start: Plow's hosts, this PC among them once it hosts. */
async function refreshAccount(app) {
  try {
    app.account = await latch(["status"]);
    app.accountError = null;
  } catch (failure) {
    app.accountError = failure;
  }
  app.draw({ page: false });
}

/** Draws the page again where the person was. */
function redraw(app) {
  if (app.place !== "settings") return;
  const top = document.querySelector(".content .page")?.scrollTop ?? 0;
  app.drawPage();
  const page = document.querySelector(".content .page");
  if (page) page.scrollTop = top;
}

// ── starting and stopping this PC's host ──────────────────────────────────

/** Starts this PC's host the way `setup --status` says (its `host` step's
 * words after `latch`: `serve -d` with the last serve's flags). Nothing to
 * do when it isn't needed; a host latch says can't start now throws, with
 * latch's reason as detail. */
async function startHost() {
  const setup = await latch(["setup", "--status"]);
  if (!Array.isArray(setup?.steps)) throw unreachable("Latch returned an unreadable response.");
  const step = setup.steps.find((s) => s.step === "host");
  if (!step) return;
  if (step.state === "needed" && step.args) await latch(step.args);
  else if (step.state === "blocked") throw unreachable("Latch can't start on this PC right now.", step.reason ?? null);
}

/** Start Latch: starts this PC's host, then reads back what latch and
 * Plow say. Home's own start can be this too. */
export async function startHosting(app) {
  const s = state(app);
  if (s.starting) return;
  s.starting = true;
  s.startError = null;
  redraw(app);
  try {
    await startHost();
  } catch (error) {
    s.startError = error;
  }
  await refreshStatus(app);
  await refreshAccount(app);
  s.starting = false;
  redraw(app);
}

/** A stop or restart while it runs; then this PC's status, read again. */
async function hostAction(app, action, run) {
  const s = state(app);
  s.action = action;
  s.actionError = null;
  redraw(app);
  try {
    await run();
  } catch (error) {
    s.actionError = error;
  }
  await refreshStatus(app);
  s.action = null;
  redraw(app);
}

async function stop(app) {
  if (!(await app.confirm("Stop Latch on this PC?", "Your agents can't reach this PC until you start it again.", "Stop"))) return;
  await hostAction(app, "stopping", () => latch(["stop"]));
}

/** Stops this PC's host and starts it again as it last ran. */
function restart(app) {
  return hostAction(app, "restarting", async () => {
    await latch(["stop"]);
    await startHost();
  });
}

// ── keep-awake and plugins ────────────────────────────────────────────────

/** "Keep this PC awake while plugged in": latch saves it (no host or
 * restart needed), then the switch shows what latch reads back. One save
 * at a time; a click meanwhile is dropped. */
async function setKeepAwake(app, on) {
  const s = state(app);
  if (s.savingKeepAwake) return;
  s.savingKeepAwake = true;
  try {
    await latch(["settings", "set", "--keep-awake", on ? "on" : "off"]);
    s.keepAwakeError = null;
  } catch (error) {
    s.keepAwakeError = error;
  }
  s.savingKeepAwake = false;
  await refreshStatus(app);
  redraw(app);
}

/** Turns a plugin on this PC's host on or off, one at a time, then reads
 * this PC's status, its plugins among it, again; a refusal in the app's
 * words. */
async function setPlugin(app, name, on) {
  const s = state(app);
  if (s.change || !app.hosts().some((h) => h.thisPC)) return;
  s.change = { plugin: name, on };
  s.changeError = null;
  redraw(app);
  let failed = null;
  try {
    await latch(["plugins", on ? "enable" : "disable", name]);
  } catch (error) {
    failed = error;
  }
  await refreshStatus(app);
  if (failed) s.changeError = unreachable(refusal(failed, name, on), details(failed));
  s.change = null;
  redraw(app);
}

const pluginTitle = (name) => name[0].toUpperCase() + name.slice(1);

/** Under a plugin's name: on, off, or what it needs first. */
function pluginState(plugin) {
  if (plugin.state === "enabled") return "On";
  if (plugin.state === "needs_setup") return "Needs setting up";
  return "Off";
}

/** Why turning a plugin on or off failed. */
function refusal(error, plugin, on) {
  switch (code(error)) {
    case "google_not_connected": return "Connect a Google account first, in Connections.";
    case "needs_setup": return "Needs setting up.";
    default: return `Couldn't turn ${pluginTitle(plugin)} ${on ? "on" : "off"}.`;
  }
}

/** What the host does about keep-awake, in words; null when the switch
 * says it all. Left out on Windows, where latch can't hold the PC awake
 * and says `unsupported`: the Mac's own sentences for running on battery,
 * macOS refusing, and power it can't read. */
function keepAwakeSaid(keepAwake) {
  if (!keepAwake?.enabled || keepAwake.held == null) return null;
  if (keepAwake.held) return "Latch is keeping this PC awake while it's plugged in.";
  switch (keepAwake.reason) {
    case "not_serving": return "Latch isn't running, so this PC sleeps as usual.";
    case "unsupported": return "This PC can't be kept awake by Latch.";
    default: return null;
  }
}

// ── drawing ───────────────────────────────────────────────────────────────

/** This PC's state shows as it changes. */
export const followsStatus = true;

export function render(app, host) {
  const offline = !host.thisPC && host.online === false;
  return el("div", { class: "settings-page" },
    hostSection(app, host),
    host.thisPC ? sleepSection(app) : null,
    offline
      ? group(null, [el("div", { class: "line quiet" }, `${host.name} is offline. Its running programs and plugins show here once it's back online.`)])
      : [execSection(app, host), pluginsSection(app, host)]);
}

function group(title, rows, note) {
  return el("section", { class: "group" }, title ? el("h3", {}, title) : null,
    rows.flat().filter(Boolean).length ? el("div", { class: "card" }, rows) : null,
    note ? el("p", { class: "group-note" }, note) : null);
}

function pair(label, value, { mono = false } = {}) {
  return el("div", { class: "line" }, el("span", {}, label),
    el("span", { class: `value selectable${mono ? " mono" : ""}` }, value));
}

const ring = () => el("span", { class: "ring", "aria-hidden": "true" });

/** The host's state; this PC's starts, restarts and stops here. */
function hostSection(app, host) {
  const s = state(app);
  const now = hostState(host);
  const rows = [el("div", { class: "line" }, el("span", {}, "State"),
    el("span", { class: "value state" }, el("i", { class: `dot${now.up ? " is-up" : now.problem ? " is-problem" : ""}`, "aria-hidden": "true" }), stateText(app, host, now)))];
  if (host.thisPC) {
    const status = app.local;
    const busy = s.action != null;
    const running = status?.this_machine?.hosting?.running === true;
    rows.push(el("div", { class: "line buttons" },
      busy ? el("span", { class: "doing" }, ring(), s.action === "restarting" ? "Restarting…" : "Stopping…") : el("span"),
      running ? [
        el("button", { title: "Stop Latch and start it again as it last ran", disabled: busy, onclick: () => restart(app) }, "Restart"),
        el("button", { disabled: busy, onclick: () => stop(app) }, "Stop…"),
      ] : el("button", { disabled: busy || s.starting, onclick: () => startHosting(app) }, s.starting ? "Starting…" : "Start Latch")));
    const failed = s.actionError ?? s.startError;
    if (failed) rows.push(el("div", { class: "line bad" }, errorText(failed)));
    if (status) {
      if (status.version) rows.push(pair("Version", status.version));
      rows.push(el("details", { class: "line expander" },
        el("summary", {}, el("span", {}, "Details"), icon("chevron", "chevron")),
        pair("Key", status.this_machine?.key ?? "—", { mono: true }),
        pair("State folder", status.state_dir ?? "—", { mono: true })));
    }
  } else {
    if (host.online !== false) {
      const read = app.summaries[host.name]?.status;
      rows.push(el("div", { class: "line" }, el("span", {}, "Version"),
        !read ? ring()
          : read.error ? el("span", { class: "value" }, `${host.name} didn't say`)
            : el("span", { class: "value selectable" }, read.value?.version ?? "—")));
    }
    rows.push(el("div", { class: "line quiet" }, `Start, stop and update ${host.name} on ${host.name} itself.`));
  }
  return group(host.thisPC ? "This PC's host" : "Host", rows);
}

function stateText(app, host, now) {
  if (host.thisPC) {
    const serve = app.local?.this_machine?.hosting;
    if (serve?.running) return `${now.title}${serve.managed ? " · in the background" : serve.attached ? " · in a terminal" : ""}`;
    return now.title;
  }
  return seen(host) ? `${now.title} · ${seen(host)}` : now.title;
}

/** Keep-awake: the choice latch reads back (`status --local`), never the
 * click, and below it what the host makes of it. Only once latch has said
 * the choice can it change. */
function sleepSection(app) {
  const s = state(app);
  const status = app.local;
  const keepAwake = status?.this_machine?.hosting?.keep_awake ?? null;
  const toggle = toggleSwitch("Keep this PC awake while plugged in", keepAwake?.enabled ?? false, {
    disabled: keepAwake == null,
    onchange: (on) => setKeepAwake(app, on),
  });
  let said = null;
  if (s.keepAwakeError) said = el("div", { class: "line bad" }, errorText(s.keepAwakeError));
  else if (status && keepAwake == null) said = el("div", { class: "line quiet" }, "Latch couldn't read this PC's settings.");
  else if (keepAwakeSaid(keepAwake)) said = el("div", { class: "line quiet" }, keepAwakeSaid(keepAwake));
  return group("Sleep", [
    el("label", { class: "line" },
      el("span", { class: "text" }, el("span", {}, "Keep this PC awake while plugged in"),
        el("small", {}, "The display still sleeps, and closing the lid still sleeps the PC.")),
      toggle),
    said,
  ]);
}

/** Whether the host refuses agents exec (`serve --deny-exec`), as its
 * rules say. */
function execSection(app, host) {
  const deny = app.summaries[host.name]?.rules?.value?.deny_exec;
  if (!deny) return null;
  let words;
  if (deny.every_agent === true) words = "No";
  else if (!deny.agent_uids?.length) words = "Yes, judged by your rules";
  else {
    const agents = app.summaries[""]?.agents?.value?.agents ?? [];
    const names = deny.agent_uids.map((uid) => agents.find((a) => a.uid === uid)?.name ?? `Agent ${uid.slice(0, 8)}`);
    words = `Yes, except ${names.join(", ")}`;
  }
  return group("Running programs", [pair("Agents may run programs", words)]);
}

/** The host's plugins: on and off here for this PC, a change at a time;
 * another host's are turned on there. Google's accounts are the
 * account's, in Connections. */
function pluginsSection(app, host) {
  const s = state(app);
  let plugins;
  if (host.thisPC) plugins = app.local ? { value: app.local.plugins ?? [] } : null;
  else {
    const read = app.summaries[host.name]?.status;
    plugins = read && (read.error ? read : { value: read.value?.plugins ?? [] });
  }
  let rows;
  if (!plugins) rows = [el("div", { class: "line center" }, ring())];
  else if (plugins.error) rows = [el("div", { class: "line quiet" }, `${host.name} couldn't list its plugins.`)];
  else {
    rows = plugins.value.map((plugin) => {
      const on = plugin.state === "enabled";
      const under = host.thisPC ? pluginState(plugin) : on ? null : `Turn this on from ${host.name}.`;
      return el("div", { class: "line" },
        el("span", { class: "text" }, el("span", {}, pluginTitle(plugin.name)), under ? el("small", {}, under) : null),
        plugin.name === "google" ? button("Manage Accounts…", () => app.go("connections")) : null,
        host.thisPC
          ? toggleSwitch(pluginTitle(plugin.name), on, { disabled: s.change != null, onchange: (want) => setPlugin(app, plugin.name, want) })
          : el("span", { class: "value" }, on ? "On" : "Off"));
    });
    if (host.thisPC && s.changeError) rows.push(el("div", { class: "line bad" }, errorText(s.changeError, { host: spoken(host) })));
  }
  const note = host.thisPC && s.change ? `Turning ${pluginTitle(s.change.plugin)} ${s.change.on ? "on" : "off"}…` : null;
  return group("Plugins", rows, note);
}

/** Windows' ToggleSwitch, On or Off before it: what latch says, until a
 * change is read back. */
function toggleSwitch(label, on, { disabled = false, onchange }) {
  const words = el("span", { class: "state" }, on ? "On" : "Off");
  const input = el("input", {
    type: "checkbox", role: "switch", "aria-label": label, disabled,
    onchange: (event) => {
      words.textContent = event.target.checked ? "On" : "Off";
      onchange(event.target.checked);
    },
  });
  input.checked = on;
  return el("span", { class: "switch" }, words, input, el("span", { class: "track", "aria-hidden": "true" }));
}
