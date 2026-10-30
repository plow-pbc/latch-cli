// Plow Latch for Windows: the setup flow and the host's home, each step a
// latch command, as in the Mac app (gui/Sources/PlowLatchUI/App/Onboarding.swift).
// The window keeps nothing of its own but which steps were passed.
import { connect, latch, follow, unfollow, said } from "./latch.js";
import { el, button } from "./dom.js";

// Outside the app (fake mode in a browser) there's no QR code to draw.
const invoke = window.__TAURI__?.core.invoke ?? (() => Promise.reject(new Error("no app")));

const MARK = "M64.2891 6.03791C103.801 -2.33144 140.954 15.1702 152.051 56.9783L152.059 56.9705C163.194 98.9657 146.53 157.832 93.5537 167.899C82.3869 170.025 67.326 171.372 53.1455 170.259V396.001L4 395.954V84.8739C4.57639 47.1219 27.183 13.9013 64.2891 6.03791ZM135.946 194.909V332.057C136.211 340.271 142.121 344.787 149.122 346.5L149.13 346.492C150.843 346.912 153.701 347.192 155.469 347.192H164.098V395.945H153.709C120.777 396.934 91.746 371.249 87.1982 338.986C87.1671 338.41 87.1278 337.827 87.0889 337.251C86.5126 328.827 86.3027 320.379 86.3027 311.939V194.909H135.946ZM396.003 328.515C396.003 344.125 391.035 355.422 387.889 361.23C367.751 398.383 319.82 406.581 286.584 381.441C261.104 401.224 224.177 400.905 200.13 379.386C190.162 370.463 177.688 354.425 177.688 332.93V194.909H226.459C226.459 231.447 226.451 293.231 226.459 323.867C226.49 329.325 226.747 334.557 230.049 339.142C237.026 348.936 251.137 349.637 258.714 339.944C262.833 333.949 261.868 329.255 262.062 320.037V194.909H311.612C311.612 228.504 311.597 298.183 311.612 324.568C311.628 327.223 311.714 329.909 312.212 332.377C314.463 344.6 330.216 349.723 339.95 341.759C344.038 338.8 347.169 333.84 347.185 329.114C347.27 287.188 347.192 41.4011 347.216 4.06819H396.003V328.515ZM197.996 22.5194C236.8 -9.91499 297.01 1.78684 321.571 45.6815C340.884 78.9335 332.536 123.257 303.389 148.069H303.381L303.271 148.163C269 178.48 214.395 174.851 185.785 138.711C157.51 103.902 163.055 50.7341 197.996 22.5194ZM83.1182 51.6688C68.011 51.5365 58.6272 60.6846 54.7959 74.5428C53.6434 78.6847 53.1611 82.9903 53.1533 87.2957V123.249C61.1118 124.759 69.7794 125.507 77.0137 124.674V124.681C95.9831 122.782 107.617 107.662 107.695 85.7225C107.765 66.4379 98.8794 51.8011 83.1182 51.6688ZM284.582 95.1746C291.972 70.7904 269.958 46.6475 245.335 49.7459L245.202 49.7615C217.55 53.4598 202.98 88.5723 222.16 110.053C242.142 132.895 276.125 123.49 284.582 95.1746Z";
const STEPS = ["welcome", "login", "privacy", "rules", "access", "start", "done"];

const screen = document.getElementById("screen");
const dots = document.getElementById("steps");
const actions = document.getElementById("actions");

const state = {
  step: remembered() ?? "welcome",
  login: { phase: "idle" },
  rules: null,
  rulesError: null,
  starting: false,
  startError: null,
  status: null,
};

// ── latch ────────────────────────────────────────────────────────────────


// ── drawing ──────────────────────────────────────────────────────────────


function mark() {
  const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
  svg.setAttribute("viewBox", "0 0 400 400");
  svg.setAttribute("class", "mark");
  svg.setAttribute("aria-label", "Plow");
  const path = document.createElementNS("http://www.w3.org/2000/svg", "path");
  path.setAttribute("d", MARK);
  svg.append(path);
  return svg;
}


function status(kind, text) {
  return el("p", { class: "status" }, el("span", { class: `dot ${kind}` }), text);
}

function draw() {
  const index = STEPS.indexOf(state.step);
  dots.replaceChildren(
    ...STEPS.map((_, i) => el("li", { class: i < index ? "done" : i === index ? "now" : "" })),
  );
  const [body, buttons] = SCREENS[state.step]();
  const typing = document.activeElement?.matches("input, textarea") && screen.contains(document.activeElement);
  screen.replaceChildren(...body);
  actions.replaceChildren(...buttons);
  // The way on is one key away: Enter takes the primary action.
  if (!typing) actions.querySelector(".primary:not(:disabled)")?.focus();
}

function go(step) {
  if (step === "home") return openMain();
  state.step = step;
  remember(step);
  draw();
  ENTER[step]?.();
}

function next() {
  go(STEPS[STEPS.indexOf(state.step) + 1] ?? "home");
}

function back() {
  const index = STEPS.indexOf(state.step);
  // A login left waiting would still log this PC in when its text comes.
  if (state.step === "login" && state.login.phase !== "verified") {
    unfollow("login");
    state.login = { phase: "idle" };
  }
  if (index > 0) go(STEPS[index - 1]);
}

function remembered() {
  try {
    return localStorage.getItem("step");
  } catch {
    return null;
  }
}

function remember(step) {
  try {
    localStorage.setItem("step", step);
  } catch {}
}

// ── the screens ──────────────────────────────────────────────────────────

const SCREENS = {
  welcome: () => [
    [
      mark(),
      el("div", { class: "center" },
        el("h1", {}, "Welcome to Latch"),
        el("p", { class: "lead", style: "margin: 0 auto 0" },
          "Put your agent to work. Your agent can do everyday work on this PC. Anything riskier needs your OK."),
      ),
    ],
    [button("Get Started", next, { primary: true })],
  ],

  login: () => {
    const login = state.login;
    const body = [el("h1", {}, "Log in to Plow")];
    const buttons = [button("Back", back)];
    if (login.phase === "verified") {
      body.push(el("p", { class: "lead" }, `This PC is named ${login.name}.`), status("on", "Logged in to Plow."));
      buttons.push(button("Continue", next, { primary: true }));
    } else if (login.phase === "nameTaken") {
      body.push(
        el("p", { class: "lead" },
          "Another machine in your Plow account already has this PC's name. Choose a new name for this PC, or replace that machine."),
        el("label", { class: "small muted", for: "name" }, "A new name"),
        el("input", { type: "text", id: "name", value: login.existing ?? "" }),
      );
      buttons.push(
        button(`Replace ${login.existing}`, () => startLogin(login.existing, true)),
        button("Use This Name", () => startLogin(document.getElementById("name").value.trim()), { primary: true }),
      );
    } else if (login.code) {
      const code = login.code;
      const card = el("div", { class: "code-card" },
        el("div", { class: "qr", id: "qr", role: "img", "aria-label": "QR code with the login text" }),
        el("div", {},
          el("p", { class: "small muted", style: "margin:0" }, "Text this code"),
          el("p", { class: "code" }, code.code),
          el("p", { class: "to", style: "margin:0" }, `to ${code.send_to_display}`),
        ),
      );
      body.push(
        el("p", { class: "lead" }, "Scan with your phone's camera, or text the code below."),
        card,
        login.phase === "expired"
          ? status("off", "This code expired.")
          : status("wait", remaining(code.expires_at) ? `Waiting for your text. Code expires in ${remaining(code.expires_at)}.` : "Waiting for your text."),
        el("p", { class: "small muted" }, "Keep this code private. The phone that texts it gets access to this PC."),
      );
      invoke("qr", { text: code.smsto }).then((svg) => {
        const qr = document.getElementById("qr");
        if (qr) qr.innerHTML = svg;
      }, () => {});
      if (login.phase === "expired") buttons.push(button("Get a New Code", () => startLogin(), { primary: true }));
    } else if (login.phase === "failed") {
      body.push(el("p", { class: "problem" }, login.message));
      buttons.push(button("Try Again", () => startLogin(), { primary: true }));
    } else {
      body.push(el("p", { class: "lead" }, "Scan this code to prepare a login text."), status("wait", "Getting a code…"));
    }
    return [body, buttons];
  },

  privacy: () => [
    [
      el("h1", {}, "You decide what your agents do on this PC."),
      el("p", { class: "lead" },
        "Their requests are checked before they run, and every one is on the record. The connection is encrypted end to end."),
      el("ul", { class: "points" },
        el("li", {}, el("b", {}, "Checked before it runs."),
          el("span", { class: "muted" },
            "Commands, file changes, Google calls and browser plans are checked, and anything risky needs your OK. If the check can't run, nothing runs.")),
        el("li", {}, el("b", {}, "On the record."),
          el("span", { class: "muted" }, "Every request, allowed or refused, goes in a sealed log on this PC.")),
        el("li", {}, el("b", {}, "Encrypted end to end."),
          el("span", { class: "muted" }, "The connection between your agents and this PC is encrypted end to end.")),
      ),
    ],
    [button("Back", back), button("Continue", next, { primary: true })],
  ],

  rules: () => {
    const body = [
      el("h1", {}, "Meet the Gatekeeper"),
      el("p", { class: "lead" },
        "Your agents can look things up and create or edit ordinary files on their own. Anything riskier, like sending, buying, deleting or giving a program internet access, needs your OK in your rules or your Plow chat."),
    ];
    if (state.rules === null && !state.rulesError) {
      body.push(status("wait", "Reading your rules…"));
    } else if (state.rules === null) {
      body.push(el("p", { class: "problem" }, state.rulesError));
    } else {
      if (state.rulesError) body.push(el("p", { class: "problem" }, state.rulesError));
      body.push(
        el("label", { class: "small muted", for: "rules" }, "Your rules for agents on this PC"),
        el("textarea", { id: "rules", placeholder: "Allow my agent to read my email and keep my calendar. Never let it share my documents." },
          state.rules.text),
        el("p", { class: "small muted" }, "Save rules replaces your current rules on this PC."),
      );
    }
    return [body, [button("Back", back), button("Skip", next), button("Save Rules", saveRules, { primary: true, disabled: state.rules === null })]];
  },

  access: () => [
    [
      el("h1", {}, "Connections"),
      el("p", { class: "lead" }, "Let your agents use your Google account, judged by your rules."),
      el("div", { class: "rows" },
        el("div", { class: "row" }, el("span", {}, "Gmail and Google Calendar"), el("span", {}, "Coming to Windows")),
      ),
      el("p", { class: "note" },
        "On Windows this first version runs the Gatekeeper, your rules, the vault and the record. Running programs, files, Google and the browser come once their sandbox does."),
    ],
    [button("Back", back), button("Continue", next, { primary: true })],
  ],

  start: () => {
    const hosting = state.status?.this_machine?.hosting;
    const body = [
      el("h1", {}, "Start Latch"),
      el("p", { class: "lead" }, "Your PC stays reachable while it's awake and Latch is running."),
    ];
    if (hosting?.running) body.push(status("on", "Latch is running on this PC."));
    else if (state.starting) body.push(status("wait", "Starting Latch…"));
    if (state.startError) body.push(el("p", { class: "problem" }, state.startError));
    const buttons = [button("Back", back)];
    buttons.push(hosting?.running
      ? button("Continue", next, { primary: true })
      : button("Start Latch", startHost, { primary: true, disabled: state.starting }));
    return [body, buttons];
  },

  done: () => [
    [
      mark(),
      el("div", { class: "center" },
        el("h1", {}, "Your agents can now use this PC."),
        el("p", { class: "lead", style: "margin: 0 auto" }, "You can review or change your setup in Latch."),
      ),
    ],
    [button("Open Latch", next, { primary: true })],
  ],

};

// What each step does as it opens.
const ENTER = {
  login: () => {
    if (state.login.phase === "idle" || state.login.phase === "failed") startLogin();
  },
  rules: async () => {
    try {
      const shown = await latch(["rules", "show"]);
      state.rules = { text: shown?.every_agent?.text ?? "" };
      state.rulesError = null;
    } catch (failure) {
      state.rulesError = said(failure);
    }
    draw();
  },
  start: () => followStatus(),
};

// ── what the buttons do ──────────────────────────────────────────────────

let ticking = null;

async function startLogin(name, replace = false) {
  unfollow("login");
  state.login = { phase: "starting" };
  draw();
  const args = ["login", "--events"];
  if (name) args.push("--name", name);
  if (replace) args.push("--replace");
  clearInterval(ticking);
  ticking = setInterval(() => state.login.code && state.step === "login" && state.login.phase === "waiting" && draw(), 1000);
  follow("login", args, (line) => {
    if (line.type === "code") state.login = { phase: "waiting", code: line };
    else if (line.type === "verified") state.login = { phase: "verified", name: line.peer?.name };
    else if (line.type === "expired") state.login = { ...state.login, phase: "expired" };
    else if (line.type === "error" && line.error?.code === "conflict") {
      state.login = { phase: "nameTaken", existing: line.error.existing_name };
    } else if (line.type === "error") state.login = { phase: "failed", message: said(line.error) };
    if (state.step === "login") draw();
  }, (failure) => {
    if (failure && !["verified", "nameTaken", "failed"].includes(state.login.phase)) {
      state.login = { phase: "failed", message: said(failure) };
      if (state.step === "login") draw();
    }
  });
}

/** How long until `expiresAt`, as m:ss. latch's timestamps are RFC 3339 with
 *  up to nanoseconds, and the date parser takes at most milliseconds. */
function remaining(expiresAt) {
  const at = Date.parse(String(expiresAt).replace(/(\.\d{3})\d+/, "$1"));
  if (Number.isNaN(at)) return null;
  const seconds = Math.max(0, Math.round((at - Date.now()) / 1000));
  return `${Math.floor(seconds / 60)}:${String(seconds % 60).padStart(2, "0")}`;
}

async function saveRules() {
  const text = document.getElementById("rules").value;
  // Kept, so a save latch refuses leaves the draft to fix.
  state.rules = { text };
  state.rulesError = null;
  try {
    await latch(["rules", "set", "--file", "-"], text);
    next();
  } catch (failure) {
    state.rulesError = said(failure);
    draw();
  }
}

async function startHost() {
  state.starting = true;
  state.startError = null;
  draw();
  try {
    await latch(["serve", "-d"]);
  } catch (failure) {
    state.startError = said(failure);
  }
  state.starting = false;
  draw();
}

async function stopHost() {
  try {
    await latch(["stop"]);
  } catch (failure) {
    state.startError = said(failure);
  }
  draw();
}

/** Keeps `state.status` as latch reports it, live, until the window closes. */
let followingStatus = false;

function followStatus() {
  if (followingStatus) return;
  followingStatus = true;
  follow("status", ["status", "--local", "--follow"], (line) => {
    if (line.type === "status") {
      state.status = line.status;
      if (state.step === "start") draw();
    }
  }, () => (followingStatus = false));
}

/** Opens where setup stands: a PC that isn't logged in goes back to login,
 *  whatever step was passed before, as the Mac app's `Progress.destination`. */
async function boot() {
  const fake = await connect();
  // Fake mode can open setup at a step (`?step=login`), for its stills.
  const asked = new URLSearchParams(location.search).get("step");
  if (fake && STEPS.includes(asked)) {
    state.step = asked;
    draw();
    ENTER[state.step]?.();
    return;
  }
  try {
    const status = await latch(["status", "--local"]);
    const past = state.step === "home" || STEPS.indexOf(state.step) > STEPS.indexOf("login");
    if (!status?.login && past) state.step = "login";
  } catch {}
  // Fake mode is for the main window's stills: it opens there.
  if (state.step === "home" || fake) return openMain();
  draw();
  ENTER[state.step]?.();
}

/** Setup is done: the window becomes the main window, as the Mac app opens
 *  its own (Main/MainWindow.swift). Setting up again comes back here. */
async function openMain() {
  state.step = "home";
  remember("home");
  unfollow("status");
  followingStatus = false;
  for (const id of ["screen", "bar"]) document.getElementById(id).hidden = true;
  const root = document.getElementById("app");
  root.hidden = false;
  const main = await import("./main.js");
  main.app.setUpAgain = () => {
    root.hidden = true;
    for (const id of ["screen", "bar"]) document.getElementById(id).hidden = false;
    go("welcome");
  };
  // Logged out: setup again, from logging in (the Mac's `logout`).
  main.app.loggedOut = () => {
    root.hidden = true;
    for (const id of ["screen", "bar"]) document.getElementById(id).hidden = false;
    go("login");
  };
  // Home's Start Latch starts it as Host Settings does (the Mac's
  // `startHosting`: setup's host step, then status read again).
  const settings = await import("./pages/settings.js");
  main.app.start = () => settings.startHosting(main.app);
  await main.open(root);
}

boot();
