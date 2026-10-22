// latch's Camoufox helper: the driver contract (`src/browser/driver.rs`)
// over Playwright, for Camoufox. latch's browser worker runs it as
//
//     node helper.mjs RUNTIME
//
// where RUNTIME is the installed runtime folder (this package, with its
// node_modules), and speaks one JSON line each way per operation on the
// helper's standard streams: `{"id": N, "session": K, "op": "frames", ...}`
// in, and `{"id": N, "ok": VALUE}` or `{"id": N, "error": {"code",
// "message"}}` out, each answer also carrying whether a dialog blocks that
// session (`dialog`) and its active tab (`active`), and `late` if a wait for
// the browser ran out, even one the operation went on without (a page's
// title, a frame's document). The first operation is
// `launch`; `close` closes the browser and ends the helper, as does the
// end of stdin.
//
// One browser serves every session on its profile. `begin` starts session
// K with a window of its own, `end` closes its tabs. Each session's share
// is its windows and every tab opened from them (a page whose opener is no
// session's tab is never adopted), its dialogs and its downloads, found
// from the page that started them.
//
// Playwright drives Camoufox over its own private pipe (`-juggler-pipe`);
// no port is ever opened. latch's page reader runs in Camoufox's isolated
// world, where `frame.evaluate` runs by default; the agent's `eval` runs in
// the page's own world, through Camoufox's `mw:` prefix. Every input is
// the engine's own (trusted) mouse and keyboard.

import fs from "node:fs";
import path from "node:path";
import { createInterface } from "node:readline";
import { pathToFileURL } from "node:url";

// Only answers go to stdout: the libraries' notices go to the engine log.
for (const name of ["log", "info", "warn", "debug"]) console[name] = console.error;
const send = (message) => process.stdout.write(`${JSON.stringify(message)}\n`);

const runtime = process.argv[2];
const load = (name, file) =>
  import(pathToFileURL(path.join(runtime, "node_modules", name, file)).href);
const { launchOptions } = await load("camoufox-js", "dist/index.js");
const { firefox } = await load("playwright-core", "index.mjs");

/// How long a command, and a page load after an input, may take.
const COMMAND = 30_000;
const LOADING = 15_000;
/// How long after an input a navigation may still start.
const NAVIGATION_STARTS = 350;

class Failure extends Error {
  constructor(code, message) {
    super(message);
    this.code = code;
  }
}
const stale = () => new Failure("stale_snapshot", "that element's document was replaced; observe again");
const dialogOpen = () => new Failure("dialog_open", "a dialog is open; answer it with `dialog accept|dismiss`");

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
/// A person's pause between two inputs.
const pause = (min, max) => sleep(min + Math.random() * (max - min));
const clip = (text, max) => (text.length <= max ? text : `${text.slice(0, max - 1)}…`);

let context;
let reader;
/// Each session's share, by the worker's key for the session.
const shares = new Map();
/// The session the current operation acts for.
let current = null;
/// When the current operation must have answered (`within_ms`).
let due = Infinity;
/// A wait for the browser in the current operation ran out.
let ranOut = false;
/// The page the browser started with, until a session takes it.
let spare = null;
/// Pages latch itself is opening: they are not the page's new tabs.
let opening = 0;
/// New pages being taken in as tabs: `settle` waits for them.
let adopting = 0;
/// Popup navigations can start before Playwright exposes their pages.
const openingPages = new Set();
const dialogWaiters = new Set();
const frameIds = new WeakMap();
const framesById = new Map();
let nextFrame = 0;
let nextDocument = 0;
// Filled vault values no GET request may carry as a query parameter
// (`guard`), by session; a session's `stopped` says one was stopped. Line
// breaks count alike however written: a form sends a field's as CRLF.
const oneNewline = (text) => text.replace(/\r\n?/g, "\n");
const guards = () => [...shares.values()];
const guarding = () => guards().some((s) => s.guard?.length);
const guardRoute = (route, request) => {
  if (request.method() === "GET") {
    let params = [];
    try {
      params = [...new URL(request.url()).searchParams.values()].map(oneNewline);
    } catch {}
    const carried = guards().filter((s) => s.guard?.some((v) => params.includes(oneNewline(v))));
    if (carried.length) {
      for (const s of carried) s.stopped = true;
      // Aborted, so no error page keeps the address as the tab's.
      return route.abort("aborted");
    }
  }
  return route.fallback();
};
let routed = false;
/// Requests are routed through the guard while any session has one.
async function routeForGuards() {
  if (guarding() && !routed) await guarded(context.route("**/*", guardRoute));
  if (!guarding() && routed) await guarded(context.unroute("**/*", guardRoute));
  routed = guarding();
}

/// `work`, unless a dialog opens or the deadline passes first. Input that
/// opens a dialog is not finished until the dialog is answered, so it is
/// left to finish then.
async function guarded(work, { ms = COMMAND, onDialog = "fail" } = {}) {
  work.catch(() => {});
  ms = Math.max(0, Math.min(ms, due - Date.now()));
  // Only a dialog in this session's own tabs stops its work.
  const mine = shares.get(current);
  if (mine?.dialog && onDialog === "fail") throw dialogOpen();
  let wake;
  const opened = new Promise((resolve) => {
    wake = () => mine?.dialog && resolve("dialog");
    dialogWaiters.add(wake);
  });
  let timer;
  const late = new Promise((resolve) => {
    timer = setTimeout(() => resolve("late"), ms);
  });
  try {
    const first = await Promise.race([work.then((value) => ({ value })), opened, late]);
    if (first === "late") {
      ranOut = true;
      throw new Failure("timeout", "the browser did not answer in time");
    }
    if (first === "dialog") {
      if (onDialog === "fail") throw dialogOpen();
      return undefined;
    }
    return first.value;
  } finally {
    clearTimeout(timer);
    dialogWaiters.delete(wake);
  }
}

function share() {
  const found = shares.get(current);
  if (!found) throw new Failure("no_such_session", "that session has ended");
  return found;
}

function tab(id) {
  const found = share().tabs.find((t) => t.id === id);
  if (!found) throw new Failure("no_such_tab", `no tab ${id}`);
  return found;
}
const activePage = () => tab(share().active).page;

/// The session whose tab `page` is, and that tab.
function ownerOf(page) {
  for (const [key, each] of shares) {
    const found = each.tabs.find((t) => t.page === page);
    if (found) return { key, share: each, tab: found };
  }
  return null;
}

/// Takes `page` in as a new tab of session `key`.
function adopt(key, page, opener) {
  const known = ownerOf(page);
  if (known) return known.tab.id;
  const owner = shares.get(key);
  const state = {
    id: `t${++owner.nextTab}`,
    page,
    opener,
    started: false,
    loading: false,
  };
  owner.tabs.push(state);
  const main = (request) => request.isNavigationRequest() && request.frame() === page.mainFrame();
  page.on("request", (request) => {
    if (main(request)) {
      state.started = true;
      state.loading = true;
    }
  });
  page.on("requestfailed", (request) => {
    if (main(request)) state.loading = false;
  });
  page.on("load", () => {
    state.loading = false;
  });
  page.on("dialog", (opened) => {
    owner.dialog = { tab: state.id, dialog: opened };
    owner.pending.push({ event: "dialog", tab: state.id, kind: opened.type(), message: clip(opened.message(), 300) });
    for (const wake of dialogWaiters) wake();
  });
  page.on("download", (started) => {
    // A navigation that turned into a download loads nothing.
    state.loading = false;
    download(owner, started);
  });
  page.on("framedetached", (frame) => framesById.delete(frameIds.get(frame)));
  page.on("close", () => {
    const at = owner.tabs.indexOf(state);
    if (at < 0) return;
    owner.tabs.splice(at, 1);
    owner.pending.push({ event: "tab_closed", tab: state.id });
    if (owner.dialog?.tab === state.id) owner.dialog = null;
    if (owner.active === state.id && owner.tabs.length) owner.active = owner.tabs[owner.tabs.length - 1].id;
  });
  return state.id;
}

/// Every download lands in the runtime's downloads folder under its own
/// name (Playwright's guid), and is reported as it starts and as it ends,
/// never opened.
function download(owner, started) {
  const { downloads, pending } = owner;
  const entry = {
    download: `d${downloads.length + 1}`,
    name: started.suggestedFilename() || "download",
    url: started.url(),
    state: "in_progress",
    bytes: 0,
  };
  downloads.push(entry);
  const report = () => pending.push({ event: "download", download: entry.download, name: entry.name, state: entry.state });
  report();
  entry.done = started.path().then(
    (file) => {
      entry.file = file;
      entry.bytes = fs.statSync(file).size;
      entry.state = "completed";
      report();
    },
    () => {
      entry.state = "canceled";
      report();
    },
  );
}

function frameId(frame) {
  let id = frameIds.get(frame);
  if (!id) {
    id = `F${++nextFrame}`;
    frameIds.set(frame, id);
    framesById.set(id, frame);
  }
  return id;
}

function frameOf(id) {
  const frame = framesById.get(id);
  if (!frame || frame.isDetached()) throw stale();
  return frame;
}

/// Runs `work(frame)` in the frame's isolated world; a frame that went
/// away under it, or whose document was replaced, is stale.
async function inFrame(id, work) {
  const frame = frameOf(id);
  try {
    return await guarded(work(frame));
  } catch (error) {
    if (error instanceof Failure) throw error;
    const message = String(error?.message ?? error);
    if (/latch:stale|detached|destroyed|closed|navigat|Cannot find context/i.test(message)) throw stale();
    throw new Failure("engine_error", `latch's page script: ${clip(message, 200)}`);
  }
}

/// The document check that guards every run of latch's script: the frame
/// still holds the document latch listed.
const checked = (document) =>
  `if (globalThis.__latchDocument !== ${JSON.stringify(document)}) throw new Error("latch:stale");\n${reader}\n`;

/// Where `frame`'s viewport starts in the tab's viewport: the content box
/// of each `<iframe>` holding it, added up through its ancestors.
async function offset(frame) {
  const parent = frame.parentFrame();
  if (!parent) return [0, 0];
  const element = await frame.frameElement();
  try {
    const [x, y] = await parent.evaluate(([el]) => {
      const box = el.getBoundingClientRect();
      const style = getComputedStyle(el);
      return [
        box.left + el.clientLeft + Number.parseFloat(style.paddingLeft),
        box.top + el.clientTop + Number.parseFloat(style.paddingTop),
      ];
    }, [element]);
    const [px, py] = await offset(parent);
    return [x + px, y + py];
  } finally {
    element.dispose().catch(() => {});
  }
}

// A window a page opens (`window.open`, with or without features, or a
// `target=_blank` link) becomes a tab that loads in the background: never a
// new window, nor a tab that takes the front.
const BACKGROUND_TABS = {
  "browser.link.open_newwindow": 3,
  "browser.link.open_newwindow.restriction": 0,
  "browser.tabs.loadDivertedInBackground": true,
};

// Every request goes through latch's proxy (`params.proxy`), loopback
// ones too, and WebRTC with it.
const THROUGH_PROXY = {
  "network.proxy.allow_hijacking_localhost": true,
  "media.peerconnection.ice.proxy_only": true,
};

async function launch(params) {
  reader = params.reader;
  // One fingerprint per profile, made on its first launch: a site sees the
  // same device every time, as it would a person's.
  const stored = path.join(params.profile, "camoufox.json");
  let identity;
  try {
    identity = JSON.parse(fs.readFileSync(stored, "utf8"));
  } catch {
    const options = await launchOptions({
      os: params.os,
      headless: params.headless,
      main_world_eval: true,
      exclude_addons: ["UBO"],
      geoip: false,
      env: {},
    });
    identity = { env: options.env, prefs: options.firefoxUserPrefs };
    fs.writeFileSync(stored, JSON.stringify(identity), { mode: 0o600 });
  }
  context = await firefox.launchPersistentContext(params.data, {
    executablePath: params.executable,
    headless: params.headless,
    env: { ...identity.env, ...params.env },
    firefoxUserPrefs: {
      ...identity.prefs,
      ...BACKGROUND_TABS,
      ...THROUGH_PROXY,
      // Firefox can open a native macOS select menu even with -headless.
      "widget.macos.allow-native-select": !params.headless,
    },
    proxy: { server: params.proxy },
    acceptDownloads: true,
    downloadsPath: params.downloads,
    viewport: null,
    // A first launch on a busy machine can take a while.
    timeout: 2 * COMMAND,
    handleSIGINT: false,
    handleSIGTERM: false,
    handleSIGHUP: false,
  });
  context.on("request", (request) => {
    if (!request.isNavigationRequest()) return;
    try {
      request.frame();
    } catch {
      // The popup's frame has no Page until the engine reports it ready.
      openingPages.add(request);
    }
  });
  context.on("requestfinished", (request) => openingPages.delete(request));
  context.on("requestfailed", (request) => openingPages.delete(request));
  context.on("page", async (page) => {
    if (opening > 0) return;
    adopting += 1;
    try {
      const from = await page.opener().catch(() => null);
      const owner = from && ownerOf(from);
      // A page no session's tab opened is no session's.
      if (!owner) return;
      const id = adopt(owner.key, page, owner.tab.id);
      owner.share.pending.push({ event: "new_tab", tab: id, url: page.url(), opener: owner.tab.id });
    } finally {
      adopting -= 1;
    }
  });
  spare = context.pages()[0] ?? null;
  return {};
}

async function title(page) {
  try {
    return await guarded(page.title(), { ms: 2000 });
  } catch {
    return "";
  }
}

const ops = {
  launch,

  /// Starts this session with a window of its own.
  async begin() {
    shares.set(current, { tabs: [], active: null, nextTab: 0, dialog: null, pending: [], downloads: [] });
    let page = spare;
    spare = null;
    if (!page || page.isClosed()) {
      opening += 1;
      try {
        page = await guarded(context.newPage(), { onDialog: "ignore" });
      } catch (error) {
        shares.delete(current);
        throw error;
      } finally {
        opening -= 1;
      }
    }
    share().active = adopt(current, page);
    return {};
  },

  /// Ends this session: closes every tab it has. The others go on.
  async end() {
    const ending = shares.get(current);
    // A session still holding a guard (its cleanup failed) keeps its
    // pages: closing one lets it send what it holds as it goes, in a
    // request the routing may not see. latch kills the browser instead.
    if (ending?.guard?.length) return { guarded: true };
    shares.delete(current);
    for (const t of ending?.tabs ?? []) await t.page.close().catch(() => {});
    await routeForGuards().catch(() => {});
    for (const d of ending?.downloads ?? []) if (d.file) fs.rmSync(d.file, { force: true });
    return {};
  },

  async tabs() {
    const { tabs, active } = share();
    const listed = [];
    for (const t of tabs) {
      listed.push({
        tab: t.id,
        url: t.page.url(),
        title: await title(t.page),
        ...(t.opener ? { opener: t.opener } : {}),
        active: t.id === active,
      });
    }
    return listed;
  },

  async new_tab({ url }) {
    opening += 1;
    let page;
    try {
      page = await guarded(context.newPage(), { onDialog: "ignore" });
    } finally {
      opening -= 1;
    }
    share().active = adopt(current, page);
    if (url) await ops.navigate({ url });
    return share().active;
  },

  async switch_tab({ tab: id }) {
    const t = tab(id);
    share().active = id;
    return {};
  },

  /// Brings this session's active tab, its window and the browser to the
  /// front, for the owner: what only a handoff does.
  async front() {
    await guarded(activePage().bringToFront());
    return {};
  },

  async close_tab({ tab: id }) {
    const t = tab(id);
    if (share().tabs.length === 1) {
      throw new Failure("last_tab", "a session keeps at least one tab; close the session instead");
    }
    await guarded(t.page.close(), { onDialog: "ignore" });
    return {};
  },

  async navigate({ url }) {
    const page = activePage();
    try {
      // A page that opens a dialog while loading never finishes loading.
      await guarded(page.goto(url, { waitUntil: "load", timeout: COMMAND }), { onDialog: "ignore" });
    } catch (error) {
      if (error instanceof Failure) throw error;
      throw new Failure("navigation_failed", clip(String(error?.message ?? error).split("\n")[0], 300));
    }
    return {};
  },

  /// The entry Back would load, read from the tab's own history through
  /// the Navigation API. That API names only entries of the page's own
  /// origin; when the one before is not among them (another origin's, or
  /// none), where Back leads is not known.
  async back_url() {
    const before = await guarded(
      activePage()
        .mainFrame()
        .evaluate(() => {
          const index = navigation.currentEntry?.index ?? 0;
          return index > 0 ? navigation.entries()[index - 1].url : null;
        }),
    );
    if (before === null) {
      throw new Failure("back_unknown", "the page before this one is of another origin, or there is none");
    }
    return before;
  },

  async back() {
    const page = activePage();
    const before = page.url();
    const went = await guarded(page.goBack({ waitUntil: "commit", timeout: COMMAND }));
    return went !== null || page.url() !== before;
  },

  async frames() {
    const listed = [];
    for (const frame of activePage().frames()) {
      if (frame.isDetached()) continue;
      const fresh = JSON.stringify(`d${++nextDocument}`);
      let document;
      try {
        document = await guarded(frame.evaluate(`globalThis.__latchDocument ??= ${fresh}`), { ms: 5000 });
      } catch (error) {
        if (error?.code === "dialog_open") throw error;
        // A frame that cannot be read now (it is going away, or loading)
        // has no document refs can be bound to.
        document = "";
      }
      const parent = frame.parentFrame();
      listed.push({ id: frameId(frame), parent: parent ? frameId(parent) : null, document });
    }
    return listed;
  },

  run({ frame, document, expression }) {
    return inFrame(frame, (f) => f.evaluate(`(() => {\n${checked(document)}return (${expression});\n})()`));
  },

  call({ frame, document, function: declaration, args }) {
    // The arguments travel as Playwright's call arguments, never inside
    // the script's text.
    const body = `${checked(document)}return (${declaration})(...args);`;
    return inFrame(frame, (f) => f.evaluate(new Function("args", body), args));
  },

  async reaches({ frame, x, y }) {
    let current = frameOf(frame);
    for (let parent = current.parentFrame(); parent; parent = current.parentFrame()) {
      const [dx, dy] = await inFrame(frameId(parent), offset);
      const element = await guarded(current.frameElement());
      try {
        const hit = await inFrame(frameId(parent), (f) =>
          f.evaluate(([el, px, py]) => document.elementFromPoint(px, py) === el, [element, x - dx, y - dy]),
        );
        if (!hit) return false;
      } finally {
        element.dispose().catch(() => {});
      }
      current = parent;
    }
    return true;
  },

  frame_offset({ frame }) {
    return inFrame(frame, offset);
  },

  async click({ x, y }) {
    const mouse = activePage().mouse;
    const input = { onDialog: "ignore" };
    await guarded(mouse.move(x, y, { steps: 4 }), input);
    await guarded(mouse.down(), input);
    await pause(40, 90);
    await guarded(mouse.up(), input);
    return {};
  },

  async type_text({ text }) {
    const keyboard = activePage().keyboard;
    for (const ch of text) {
      if (share().dialog) break;
      if (ch === "\n") await ops.press({ key: "Enter" });
      else await guarded(keyboard.type(ch), { onDialog: "ignore" });
      await pause(25, 70);
    }
    return {};
  },

  async select_all() {
    await guarded(activePage().keyboard.press("ControlOrMeta+A"), { onDialog: "ignore" });
    return {};
  },

  async press({ key }) {
    const keyboard = activePage().keyboard;
    await guarded(keyboard.down(key), { onDialog: "ignore" });
    await pause(30, 60);
    await guarded(keyboard.up(key), { onDialog: "ignore" });
    return {};
  },

  async wheel({ x, y, delta_y }) {
    const mouse = activePage().mouse;
    await guarded(mouse.move(x, y), { onDialog: "ignore" });
    await guarded(mouse.wheel(0, delta_y), { onDialog: "ignore" });
    return {};
  },

  /// Waits for what an input started (a navigation, a load) to finish, or
  /// a dialog to open, and returns the events since the last call.
  async settle() {
    await sleep(NAVIGATION_STARTS);
    const mine = share();
    const t = mine.tabs.find((each) => each.id === mine.active);
    const deadline = Math.min(Date.now() + LOADING, due);
    while (t?.started && t.loading && !mine.dialog && Date.now() < deadline) await sleep(100);
    if (t) t.started = false;
    await sleep(150);
    while ((openingPages.size > 0 || adopting > 0) && !mine.dialog && Date.now() < deadline) await sleep(20);
    return mine.pending.splice(0);
  },

  async answer_dialog({ accept, text }) {
    const mine = share();
    if (!mine.dialog) throw new Failure("no_dialog", "no dialog is open");
    const { dialog: open } = mine.dialog;
    mine.dialog = null;
    await (accept ? open.accept(text ?? undefined) : open.dismiss());
    return {};
  },

  async eval({ script }) {
    try {
      // Camoufox runs a script in the page's own world when it starts `mw:`.
      const value = await guarded(activePage().mainFrame().evaluate(`mw:${script}`));
      return value === undefined ? null : value;
    } catch (error) {
      if (error?.code === "dialog_open") {
        throw new Failure("dialog_open", "the script opened a dialog; answer it with `dialog accept|dismiss`");
      }
      if (error instanceof Failure) throw error;
      throw new Failure("eval_failed", clip(String(error?.message ?? error).replace(/^frame\.evaluate: /, ""), 1000));
    }
  },

  async set_files({ frame, document, element, files }) {
    const handle = await inFrame(frame, (f) =>
      f.evaluateHandle(`(() => {\n${checked(document)}return (${element});\n})()`),
    );
    const input = handle.asElement();
    try {
      if (!input) throw stale();
      await guarded(input.setInputFiles(files));
    } finally {
      handle.dispose().catch(() => {});
    }
    return {};
  },

  downloads() {
    return share().downloads.map(({ download: id, name, url, state, bytes }) => ({ download: id, name, url, state, bytes }));
  },

  /// The file of completed download `id`, waiting a while for one still
  /// in progress.
  async download_file({ download: id }) {
    const entry = share().downloads.find((d) => d.download === id);
    if (!entry) throw new Failure("no_such_download", `no download ${id}`);
    await Promise.race([entry.done, sleep(Math.min(LOADING, due - Date.now()))]);
    if (entry.state === "canceled") throw new Failure("download_failed", `download ${id} was canceled`);
    if (entry.state !== "completed") {
      throw new Failure("download_incomplete", `download ${id} is still in progress; ask again later`);
    }
    const { download: name, url, state, bytes } = entry;
    return { download: { download: name, name: entry.name, url, state, bytes }, path: entry.file };
  },

  async screenshot() {
    const page = activePage();
    const png = await guarded(page.screenshot({ type: "png", timeout: COMMAND }));
    const [width, height, scrollX, scrollY] = await guarded(
      page.mainFrame().evaluate("[innerWidth, innerHeight, scrollX, scrollY]"),
    );
    return { png: png.toString("base64"), css_width: width, css_height: height, scroll_x: scrollX, scroll_y: scrollY };
  },

  /// This session's guard: until called again with none, every GET
  /// request, in any tab, whose address has a query parameter equal to one
  /// of `values` is stopped.
  async guard({ values }) {
    share().guard = values;
    await routeForGuards();
    return {};
  },

  /// Whether any session holds a guard: then the browser is killed, not
  /// closed, as closing its pages lets them send what they hold.
  guarded() {
    return guarding();
  },

  guard_stopped() {
    const stopped = Boolean(share().stopped);
    share().stopped = false;
    return stopped;
  },

  async close() {
    await context?.close().catch(() => {});
    return {};
  },
};

async function answer({ id, op, session, within_ms, ...params }) {
  const reply = { id };
  current = session ?? null;
  due = within_ms === undefined ? Infinity : Date.now() + within_ms;
  ranOut = false;
  try {
    const run = ops[op];
    if (!run) throw new Failure("bad_request", `no operation ${op}`);
    const value = await run(params);
    reply.ok = value === undefined ? null : value;
  } catch (error) {
    reply.error =
      error instanceof Failure
        ? { code: error.code, message: error.message }
        : { code: "engine_error", message: clip(String(error?.message ?? error), 300) };
  }
  const mine = shares.get(current);
  reply.dialog = Boolean(mine?.dialog);
  reply.active = mine?.active ?? null;
  if (ranOut) reply.late = true;
  send(reply);
  return op === "close";
}

// One operation at a time, in order.
const lines = createInterface({ input: process.stdin });
for await (const line of lines) {
  let request;
  try {
    request = JSON.parse(line);
  } catch {
    send({ id: null, error: { code: "bad_request", message: "not JSON" } });
    continue;
  }
  if (await answer(request)) process.exit(0);
}
await context?.close().catch(() => {});
process.exit(0);
