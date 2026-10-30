// Plow Latch for Windows: the setup flow and the host's home, each step a
// latch command, as in the Mac app (gui/Sources/PlowLatchUI/App/Onboarding.swift
// and Onboarding/*.swift, drawn as there). The window keeps nothing of its
// own but which step it's on: what's set up is always latch's answer.
import { connect, latch, follow, unfollow, openURL } from "./latch.js";
import { el } from "./dom.js";
import { decks, tryArgs, errorText, sentence, glyph, THIS_PC, unreachable, code } from "./pages/rules.js";
import { startHost, startHosting } from "./pages/settings.js";
import { accountsOf, working, addsAccount, noneWorks, RECONNECT, TOOLS_MISSING, signInProblem } from "./google.js";

const invoke = window.__TAURI__?.core.invoke ?? (() => Promise.reject(new Error("no app")));

const MARK = "M64.2891 6.03791C103.801 -2.33144 140.954 15.1702 152.051 56.9783L152.059 56.9705C163.194 98.9657 146.53 157.832 93.5537 167.899C82.3869 170.025 67.326 171.372 53.1455 170.259V396.001L4 395.954V84.8739C4.57639 47.1219 27.183 13.9013 64.2891 6.03791ZM135.946 194.909V332.057C136.211 340.271 142.121 344.787 149.122 346.5L149.13 346.492C150.843 346.912 153.701 347.192 155.469 347.192H164.098V395.945H153.709C120.777 396.934 91.746 371.249 87.1982 338.986C87.1671 338.41 87.1278 337.827 87.0889 337.251C86.5126 328.827 86.3027 320.379 86.3027 311.939V194.909H135.946ZM396.003 328.515C396.003 344.125 391.035 355.422 387.889 361.23C367.751 398.383 319.82 406.581 286.584 381.441C261.104 401.224 224.177 400.905 200.13 379.386C190.162 370.463 177.688 354.425 177.688 332.93V194.909H226.459C226.459 231.447 226.451 293.231 226.459 323.867C226.49 329.325 226.747 334.557 230.049 339.142C237.026 348.936 251.137 349.637 258.714 339.944C262.833 333.949 261.868 329.255 262.062 320.037V194.909H311.612C311.612 228.504 311.597 298.183 311.612 324.568C311.628 327.223 311.714 329.909 312.212 332.377C314.463 344.6 330.216 349.723 339.95 341.759C344.038 338.8 347.169 333.84 347.185 329.114C347.27 287.188 347.192 41.4011 347.216 4.06819H396.003V328.515ZM197.996 22.5194C236.8 -9.91499 297.01 1.78684 321.571 45.6815C340.884 78.9335 332.536 123.257 303.389 148.069H303.381L303.271 148.163C269 178.48 214.395 174.851 185.785 138.711C157.51 103.902 163.055 50.7341 197.996 22.5194ZM83.1182 51.6688C68.011 51.5365 58.6272 60.6846 54.7959 74.5428C53.6434 78.6847 53.1611 82.9903 53.1533 87.2957V123.249C61.1118 124.759 69.7794 125.507 77.0137 124.674V124.681C95.9831 122.782 107.617 107.662 107.695 85.7225C107.765 66.4379 98.8794 51.8011 83.1182 51.6688ZM284.582 95.1746C291.972 70.7904 269.958 46.6475 245.335 49.7459L245.202 49.7615C217.55 53.4598 202.98 88.5723 222.16 110.053C242.142 132.895 276.125 123.49 284.582 95.1746Z";
const STEPS = ["welcome", "login", "privacy", "rules", "access", "start", "done"];
/** The screens a person may skip (`Step.skippable`). */
const SKIPPABLE = ["rules", "access"];
/** One footer dot for each screen from Log in to Start Latch. */
const DOTTED = ["login", "privacy", "rules", "access", "start"];

const screen = document.getElementById("screen");
const bar = document.getElementById("bar");
const dots = document.getElementById("steps");
const actions = document.getElementById("actions");
// Back and Skip for now, left of the dots.
const leading = el("div", { id: "leading" });
bar.prepend(leading);

/** The flow's state, as the Mac's `Onboarding` keeps it. */
const state = {
  step: remembered() ?? "welcome",
  /** A still of fake mode: nothing runs on its own (the Mac's `app.still`). */
  still: false,
  /** Set up again: screens show what's set up now, not first-run defaults. */
  rerun: false,
  /** `status --local`, followed. */
  status: null,
  // Log in to Plow.
  login: { phase: "starting" },
  loginName: "",
  qr: null,
  // Set your rules.
  savedRules: null,
  draftRules: "",
  /** What loadRules last put in the box: while the box still holds it, the
   * person has not edited it. */
  filledRules: null,
  rulesError: null,
  savingRules: false,
  rulesUnreadable: false,
  deck: 0,
  previews: {},
  reviewing: false,
  reviewed: null,
  review: 0,
  macLit: false,
  open: new Set(),
  // Connect Google.
  google: "checking",
  googleWhy: null,
  accounts: { accounts: [], needsSignIn: [] },
  googleWaitEnded: null,
  googleLink: null,
  lookingAtGoogle: false,
  // Start Latch.
  runInBackground: true,
  startAtLogin: true,
  loginItemOffered: false,
  starting: false,
  startError: null,
  savingKeepAwake: false,
  keepAwakeError: null,
  // Done: what latch says is set up.
  rulesSaved: null,
  setup: null,
};

/** Fake mode's own: a still's staging, a made-up QR code, the login item. */
let fakeSetup = null;

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

// ── icons and drawings ───────────────────────────────────────────────────

const ICONS = {
  check: '<path d="M4.5 10.4l3.6 3.6 7.4-8"/>',
  list: '<path d="M7.5 5.5h9M7.5 10h9M7.5 14.5h9"/><path d="M4 5.5h.01M4 10h.01M4 14.5h.01" stroke-width="2"/>',
  lock: '<rect x="5" y="9" width="10" height="8" rx="1.6"/><path d="M7.2 9V6.6a2.8 2.8 0 0 1 5.6 0V9"/>',
  refresh: '<path d="M15.6 10.6a5.6 5.6 0 1 1-1.7-4.6"/><path d="M14.2 3.4v3h-3"/>',
  clock: '<circle cx="10" cy="10" r="6.6"/><path d="M10 6.4V10l-2.3 1.5"/>',
  cup: '<path d="M4.5 7.5h9.5V11a4 4 0 0 1-4 4h-1.5a4 4 0 0 1-4-4z"/><path d="M14 8.6h.8a1.9 1.9 0 0 1 0 3.8H14M3 17.2h14"/>',
  rules: '<path d="M4 5.5h12M4 8.5h8.5M4 11.5h12M4 14.5h8.5"/>',
  google: '<circle cx="10" cy="10" r="7"/><path d="M12.8 8.1A3.2 3.2 0 1 0 13.2 10.6H10"/>',
  envelope: '<rect x="2.8" y="4.8" width="14.4" height="10.4" rx="1.4"/><path d="M3.3 5.6L10 11l6.7-5.4"/>',
  arrow: '<path d="M4 10h12M11.2 5.2L16 10l-4.8 4.8"/>',
};

function icon(name, className = "icon") {
  const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
  svg.setAttribute("viewBox", "0 0 20 20");
  svg.setAttribute("class", className);
  svg.setAttribute("aria-hidden", "true");
  svg.innerHTML = ICONS[name] ?? "";
  return svg;
}

function svg(viewBox, className, inner, label) {
  const node = document.createElementNS("http://www.w3.org/2000/svg", "svg");
  node.setAttribute("viewBox", viewBox);
  node.setAttribute("class", className);
  if (label) node.setAttribute("aria-label", label);
  else node.setAttribute("aria-hidden", "true");
  node.innerHTML = inner;
  return node;
}

/** The Plow mark, filled with the accent. */
function mark(className = "mark") {
  return svg("0 0 400 400", className, `<path d="${MARK}"/>`, "Plow");
}

/** The old app's little host (a 120 × 86 drawing), whose glow lights up
 * when a request gets through (`MacDrawing`). */
function hostDrawing(lit) {
  return svg("0 0 120 86", `host-drawing${lit ? " lit" : ""}`, `
    <defs>
      <radialGradient id="host-glow"><stop offset="0" style="stop-color: var(--volt-fill); stop-opacity: .55"/><stop offset="1" style="stop-color: var(--volt-fill); stop-opacity: 0"/></radialGradient>
      <linearGradient id="host-front" x1="0" y1="0" x2="0" y2="1"><stop offset="0" style="stop-color: var(--host-front-a)"/><stop offset="1" style="stop-color: var(--host-front-b)"/></linearGradient>
      <linearGradient id="host-top" x1="0" y1="0" x2="0" y2="1"><stop offset="0" style="stop-color: var(--host-top-a)"/><stop offset="1" style="stop-color: var(--host-top-b)"/></linearGradient>
      <radialGradient id="host-light"><stop offset="0" stop-color="#EBFFB0"/><stop offset=".35" style="stop-color: var(--volt-fill); stop-opacity: .8"/><stop offset="1" style="stop-color: var(--volt-fill); stop-opacity: 0"/></radialGradient>
    </defs>
    <ellipse class="glow" cx="60" cy="77" rx="54" ry="6" fill="url(#host-glow)"/>
    <ellipse cx="60" cy="75.5" rx="50" ry="2.6" fill="#000" fill-opacity=".55"/>
    <path class="body" d="M8.5 36H111.5V65Q111.5 74 102 74H18Q8.5 74 8.5 65Z" fill="url(#host-front)"/>
    <path class="body" d="M17 23Q18 18 26 18H94Q102 18 103 23L111 32Q113 36.5 107 36.5H13Q7 36.5 9 32Z" fill="url(#host-top)"/>
    <path class="seam" d="M13 36H107"/>
    <rect class="part" x="23" y="53" width="3.6" height="10" rx="1.8"/>
    <rect class="part" x="31" y="53" width="3.6" height="10" rx="1.8"/>
    <circle class="part" cx="96" cy="58" r="2.3"/>
    <circle class="glow" cx="86" cy="58" r="5" fill="url(#host-light)"/>
    <circle class="pin" cx="86" cy="58" r=".9"/>`);
}

function spinner(className = "spinner") {
  return el("span", { class: className, "aria-hidden": "true" });
}

// ── drawing ──────────────────────────────────────────────────────────────

let drawn = null;
let reveal = false;

/** Draws the screen and its footer again, the field being typed in kept
 * with its selection, and the screen's scroll. */
function draw() {
  const active = document.activeElement;
  const kept = active?.id && screen.contains(active) ? active.id : null;
  const selection = kept && typeof active.selectionStart === "number" ? [active.selectionStart, active.selectionEnd, active.scrollTop] : null;
  const top = screen.scrollTop;
  const fresh = drawn !== state.step;
  drawn = state.step;
  screen.className = `step-${state.step}${fresh ? " entering" : ""}`;
  screen.replaceChildren(...[SCREENS[state.step]()].flat().filter(Boolean));
  if (!fresh) screen.scrollTop = top;
  drawFooter();
  // A lane just opened is brought into view: the window is shorter than
  // the Mac's.
  if (reveal) screen.querySelector(".lane .detail")?.scrollIntoView({ block: "nearest" });
  reveal = false;
  const again = kept && document.getElementById(kept);
  if (again && !again.disabled) {
    again.focus({ preventScroll: true });
    if (selection) {
      again.setSelectionRange(selection[0], selection[1]);
      again.scrollTop = selection[2];
    }
  }
}

// The way on is one key away: Enter takes the primary action, as the Mac's
// default button, unless a field or a button has the key.
document.addEventListener("keydown", (event) => {
  if (event.key !== "Enter" || bar.hidden || event.target.closest?.("input, textarea, button, select, summary")) return;
  event.preventDefault();
  primary();
});

/** Back or Skip for now, the progress dots, and the step's primary action
 * (`OnboardingFooter`). Welcome has its button alone, on the screen. */
function drawFooter() {
  const step = state.step;
  bar.className = step === "welcome" ? "bare" : "";
  const quiet = (label, onclick) => el("button", { class: "quiet", onclick, disabled: busy() }, label);
  leading.replaceChildren(
    ...(["login", "privacy", "rules", "access", "start"].includes(step) ? [quiet("Back", back)] : []),
    // Once the step is set up, Continue is the way on.
    ...(SKIPPABLE.includes(step) && !stepDone() ? [quiet("Skip for now", skip)] : []),
  );
  const current = DOTTED.indexOf(step) < 0 ? DOTTED.length : DOTTED.indexOf(step);
  dots.hidden = step === "welcome";
  dots.className = step === "done" ? "gone" : "";
  dots.setAttribute("aria-label", `Step ${Math.min(current + 1, DOTTED.length)} of ${DOTTED.length}`);
  dots.replaceChildren(...DOTTED.map((_, i) => el("li", { class: i === current ? "now" : i < current ? "done" : "" })));
  actions.replaceChildren(el("button", { class: "primary", onclick: primary, disabled: !primaryEnabled() },
    el("span", {}, primaryTitle()), icon("arrow", "arrow")));
}

// ── moving between screens ───────────────────────────────────────────────

function go(step) {
  if (step === "home") return openMain();
  stopWork();
  state.step = step;
  remember(step);
  draw();
  if (!state.still) ENTER[step]?.();
}

/** Continue: the next screen. */
function next() {
  go(STEPS[STEPS.indexOf(state.step) + 1] ?? "home");
}

/** While Continue saves the rules or starts Latch, Back and Skip wait. */
const busy = () => state.savingRules || state.starting;

function skip() {
  if (!SKIPPABLE.includes(state.step) || busy()) return;
  next();
}

function back() {
  const index = STEPS.indexOf(state.step);
  if (busy() || index <= 0) return;
  go(STEPS[index - 1]);
}

/** Done's Open Latch: setup is finished; the main window takes over. */
function finish() {
  go("home");
}

/** Stops what a screen started: leaving it, or closing the window. */
function stopWork() {
  unfollow("login");
  clearInterval(ticking);
  // A review stopped part way is not judged: its rows are unchecked again.
  state.review += 1;
  clearTimeout(reviewTimer);
  state.reviewing = false;
  for (const [id, preview] of Object.entries(state.previews)) if (preview === "checking") delete state.previews[id];
  state.reviewed = null;
  unfollow("google");
  state.googleLink = null;
  if (state.google === "waiting") state.google = "notConnected";
}

/** The footer's main button on each screen: its title, whether it can be
 * pressed now, and what it does. On Windows Grant access is Google's
 * alone: Contacts & Messages and Full Disk Access are a Mac's. */
function primaryTitle() {
  switch (state.step) {
    case "welcome": return "Get started";
    case "rules": return state.savingRules ? "Saving…" : rulesChanged() ? "Save rules" : "Continue";
    case "start": return state.starting ? "Starting…" : state.runInBackground && !ready() ? "Start Latch" : "Continue";
    case "done": return "Open Latch";
    default: return "Continue";
  }
}

function primaryEnabled() {
  switch (state.step) {
    case "login": return state.login.phase === "verified";
    case "rules": return !state.savingRules && !state.rulesUnreadable;
    // Continue never skips Google: until it's on, Skip for now does that.
    case "access": return state.google === "connected";
    case "start": return !state.starting;
    default: return true;
  }
}

function primary() {
  if (!primaryEnabled()) return;
  switch (state.step) {
    case "rules": return saveRules();
    case "start": return startLatch();
    case "done": return finish();
    default: return next();
  }
}

/** A step already set up shows Continue alone, not Skip for now. */
function stepDone() {
  switch (state.step) {
    case "rules": return Boolean((state.savedRules ?? "").trim()) && !rulesChanged() && !state.rulesUnreadable;
    case "access": return state.google === "connected";
    default: return false;
  }
}

const ready = () => state.status?.this_machine?.hosting?.ready === true;

// ── the screens ──────────────────────────────────────────────────────────

const SCREENS = {
  welcome: () => [
    mark(),
    el("div", { class: "center" },
      // As the Mac's welcome (Onboarding/Screens.swift): its two lines, the
      // second in volt; "Welcome to Latch" is its window's title.
      el("h1", {}, "Put your agent to work.", el("br"), el("span", { class: "volt" }, "Keep control.")),
      el("p", { class: "lead", style: "margin: 0 auto 0" },
        "Your agent can do everyday work on this PC. Anything riskier needs your OK."),
    ),
  ],

  login: () => {
    const login = state.login;
    const verified = login.phase === "verified";
    return el("div", { class: "login" },
      el("h1", {}, "Log in to Plow"),
      el("p", { class: "lead" }, verified ? `This PC is named ${login.name} in Plow.` : "Scan with your phone's camera, or text the code below."),
      hero(login),
      loginCaption(login));
  },

  privacy: () => el("div", { class: "privacy" },
    el("p", { class: "logged-in" }, icon("check"), "Logged in to Plow."),
    header("You decide what your agents do on this PC.",
      "Their requests are checked before they run, and every one is on the record. The connection is encrypted end to end."),
    el("div", { class: "statements" },
      statement("check", "Checked before it runs.",
        "Commands, file changes, Google calls and browser plans are checked, and anything risky needs your OK. If the check can't run, nothing runs."),
      statement("list", "On the record.", "Every request, allowed or refused, goes in a sealed log on this PC."),
      statement("lock", "Encrypted end to end.", "The connection between your agents and this PC is encrypted end to end."))),

  rules: () => gatekeeper(),
  access: () => grantAccess(),
  start: () => startScreen(),
  done: () => doneScreen(),
};

/** A screen's title and lead, left-aligned (`ScreenHeader`). */
function header(title, lead) {
  return el("div", { class: "header" }, el("h1", {}, title), lead ? el("p", { class: "lead" }, lead) : null);
}

function statement(symbol, title, detail) {
  return el("div", { class: "statement" }, el("span", { class: "tile" }, icon(symbol)),
    el("div", {}, el("strong", {}, title), el("p", {}, detail)));
}

// ── Log in to Plow ───────────────────────────────────────────────────────

/** The white QR card, which shrinks and rounds into the check once logged
 * in; an exclamation mark when the login stopped short. */
function hero(login) {
  const label = {
    starting: "Getting a code", waiting: "Scan this code to prepare a login text.", expired: "Expired login code",
    failed: "Couldn't get a login code", nameTaken: "This PC's name is taken", verified: "Logged in",
  }[login.phase];
  const card = el("div", { class: `hero ${login.phase}`, role: "img", "aria-label": label });
  if (login.phase === "verified") {
    card.append(svg("0 0 40 40", "check", '<path d="M10 21l7 7 13-16"/>'));
  } else if (login.code) {
    const qr = el("div", { class: "qr" });
    if (state.qr?.text === login.code.smsto) qr.innerHTML = state.qr.svg;
    else drawQR(login.code.smsto);
    card.append(qr);
  } else if (login.phase === "failed" || login.phase === "nameTaken") {
    card.append(el("span", { class: "bang" }, "!"));
  } else {
    card.append(spinner("spinner large"));
  }
  return card;
}

/** The QR code of the login text, drawn by the app (or fake mode's). */
async function drawQR(text) {
  // Asked from inside a draw: it draws again once that one is done.
  await null;
  try {
    const drawing = fakeSetup ? fakeSetup.qr(text) : await invoke("qr", { text });
    state.qr = { text, svg: drawing };
  } catch {
    // Asked once: the card stays white rather than asking at every redraw.
    state.qr = { text, svg: "" };
  }
  if (state.step === "login") draw();
}

function loginCaption(login) {
  switch (login.phase) {
    case "starting":
      return el("p", { class: "muted" }, "Getting a code…");
    case "waiting":
    case "expired": {
      const code = login.code;
      const expired = login.phase === "expired";
      const left = remaining(code.expires_at);
      return el("div", { class: "caption" },
        el("div", { class: `code-table${expired ? " faded" : ""}` },
          el("div", { class: "code-row" }, el("span", {}, "To"), el("span", { class: "to" }, code.send_to_display)),
          el("div", { class: "code-row" }, el("span", {}, "Text"), el("span", { class: "code" }, code.code))),
        expired
          ? el("p", { class: "muted" }, "This code expired. ", el("button", { class: "link", onclick: () => startLogin() }, "Get a new code."))
          : el("p", { class: "muted waiting" }, spinner("spinner mini"),
            left ? `Waiting for your text. Code expires in ${left}.` : "Waiting for your text."),
        // Open in Messages is a Mac's: Messages opens with the text filled in.
        el("p", { class: "callout muted" }, "Keep this code private. The phone that texts it gets access to this PC."));
    }
    case "failed":
      return el("div", { class: "caption" },
        errorText(login.error, { titled: true, tone: "clay" }),
        el("button", { class: "link", onclick: () => startLogin() }, "Try again"));
    case "nameTaken":
      return nameTaken(login);
    default:
      return null;
  }
}

/** Plow has another machine by this PC's name: pick another, or replace it. */
function nameTaken(login) {
  const existing = login.existing;
  const use = el("button", { class: "sys", disabled: !state.loginName.trim(), onclick: useAnotherName }, "Use this name");
  const field = el("input", {
    type: "text", id: "login-name", placeholder: "New name", "aria-label": "New name", spellcheck: "false",
    oninput: (event) => {
      state.loginName = event.target.value;
      use.disabled = !state.loginName.trim();
    },
    onkeydown: (event) => { if (event.key === "Enter") useAnotherName(); },
  });
  field.value = state.loginName;
  const seen = login.lastSeen
    ? ` It was last seen ${new Date(login.lastSeen).toLocaleString([], { dateStyle: "medium", timeStyle: "short" })}.` : "";
  return el("div", { class: "name-taken" },
    el("p", {}, `Another machine in your Plow account already has this PC's name${existing ? `, “${existing}”` : ""}. Choose a new name for this PC, or replace that machine.`),
    el("div", { class: "name-row" }, field, use),
    existing ? el("div", { class: "replace" },
      el("button", { class: "sys", onclick: replaceMachine }, `Replace ${existing}`),
      el("p", { class: "callout muted" }, `${existing} is signed out of Plow and leaves your account, and this PC takes its name.${seen}`)) : null);
}

let ticking = null;

/** Runs `latch login --events` until it ends or the screen goes. `name`,
 * when given, is the name this PC takes in Plow; `replace` takes it from
 * the machine that has it, only ever on the person's choice. */
function startLogin(name, replace = false) {
  unfollow("login");
  state.login = { phase: "starting" };
  if (state.step === "login") draw();
  const args = ["login", "--events"];
  if (name) args.push("--name", name);
  if (replace) args.push("--replace");
  clearInterval(ticking);
  // The countdown to the code's expiry.
  ticking = setInterval(() => state.step === "login" && state.login.phase === "waiting" && state.login.code?.expires_at && draw(), 1000);
  follow("login", args, (line) => {
    const was = state.login;
    if (line.type === "code") state.login = { phase: "waiting", code: line };
    else if (line.type === "verified") {
      state.login = { phase: "verified", name: line.peer?.name };
      refreshStatus();
    } else if (line.type === "expired") {
      state.login = was.phase === "waiting" ? { phase: "expired", code: was.code } : { phase: "failed", error: unreachable("This code expired.") };
    } else if (line.type === "error" && line.error?.code === "conflict") {
      state.login = { phase: "nameTaken", existing: line.error.existing_name ?? null, lastSeen: line.error.last_seen_at ?? null };
    } else if (line.type === "error") state.login = { phase: "failed", error: { error: line.error } };
    else return;
    if (state.step === "login") draw();
  }, (failure) => {
    if (!failure || ["verified", "expired", "nameTaken", "failed"].includes(state.login.phase)) return;
    state.login = { phase: "failed", error: failure };
    if (state.step === "login") draw();
  });
}

/** "Use this name": logs in again under the name typed. */
function useAnotherName() {
  const name = state.loginName.trim();
  if (name) startLogin(name);
}

/** "Replace ‹name›": that machine leaves the account, signed out of Plow,
 * and this PC takes its name. */
function replaceMachine() {
  if (state.login.phase === "nameTaken" && state.login.existing) startLogin(state.login.existing, true);
}

/** How long until `expiresAt`, as m:ss. latch's timestamps are RFC 3339 with
 *  up to nanoseconds, and the date parser takes at most milliseconds. */
function remaining(expiresAt) {
  if (!expiresAt) return null;
  const at = Date.parse(String(expiresAt).replace(/(\.\d{3})\d+/, "$1"));
  if (Number.isNaN(at)) return null;
  const seconds = Math.max(0, Math.round((at - Date.now()) / 1000));
  return `${Math.floor(seconds / 60)}:${String(seconds % 60).padStart(2, "0")}`;
}

// ── Meet the Gatekeeper ──────────────────────────────────────────────────

const DECKS = decks();
const deck = () => DECKS[state.deck];
/** A lane's geometry, the Mac's: the host 118 wide at the right, the beam
 * 168 from the right edge, a lane 46 tall with a 36 tall pill. */
const HOST_WIDTH = 118;
const BEAM_X = 168;
const LANE = 46;
const DETAIL = 102;

/** The rules Continue leaves in place: the box's, or the saved ones it keeps. */
function previewedRules() {
  const draft = state.draftRules.trim();
  return draft || (state.savedRules ?? "").trim();
}

function rulesChanged() {
  const draft = state.draftRules.trim();
  return Boolean(draft) && draft !== (state.savedRules ?? "").trim();
}

/** Whose rules the box holds, when this PC already has some. */
function rulesNote() {
  if (!(state.savedRules ?? "").trim()) return null;
  return rulesChanged() ? "Save rules replaces your current rules on this PC." : "These are your current rules on this PC.";
}

function gatekeeper() {
  const field = el("textarea", {
    id: "rules-draft", "aria-label": "Your rules for agents on this PC", disabled: state.rulesUnreadable, spellcheck: "true",
    oninput: (event) => {
      state.draftRules = event.target.value;
      draftChanged();
    },
  });
  field.value = state.draftRules;
  const note = rulesNote();
  return el("div", { class: "gatekeeper" },
    el("h1", {}, "Meet the Gatekeeper"),
    el("p", { class: "lead" },
      "Your agents can look things up and create or edit ordinary files on their own, and they see what they find. Anything riskier, like sending, buying, deleting or giving a program internet access, needs your OK in your rules or your Plow chat."),
    el("p", { class: "question" }, "What may your agents do on this PC?"),
    field,
    el("p", { class: "decks" }, el("span", { class: "muted" }, "Start with:"),
      DECKS.map((one, i) => el("button", {
        class: `deck-link${state.deck === i && state.draftRules === one.rules ? " on" : ""}`,
        disabled: state.rulesUnreadable, onclick: () => choose(i),
      }, one.name))),
    note ? el("p", { class: "rules-note" }, note) : null,
    state.rulesError ? errorText(state.rulesError, { host: THIS_PC, tone: "clay callout" }) : null,
    state.rulesUnreadable ? el("p", { class: "unreadable" },
      el("span", {}, "Your current rules couldn't be read, so none can be saved over them."),
      el("button", { class: "sys", onclick: loadRules }, "Try Again")) : null,
    lanes());
}

/** The lanes, the beam and the host: each example request crossing the
 * glowing boundary. An allowed one passes the beam; a refused one stops. */
function lanes() {
  const examples = deck().examples;
  const height = examples.length * LANE;
  return el("div", { class: "deck", style: `--lanes: ${height}px` },
    el("div", { class: "host", style: `top: ${height / 2}px` }, hostDrawing(state.macLit)),
    el("div", { class: "lanes" }, examples.map(lane)),
    el("div", { class: `beam${state.reviewing ? " scanning" : ""}`, style: `height: ${height + 24}px` }, el("i")));
}

const drawnLooks = {};

function lane(example) {
  const preview = state.previews[example.title];
  const look = !preview ? "idle" : preview === "checking" ? "checking" : preview === "noAnswer" || preview.error ? "unavailable"
    : preview.verdict.verdict === "allow" ? "allowed" : "refused";
  // A verdict that just arrived flares at the beam; a refused pill bumps.
  const arrived = drawnLooks[example.title] && drawnLooks[example.title] !== look && (look === "allowed" || look === "refused");
  drawnLooks[example.title] = look;
  const open = state.open.has(example.title);
  const end = look === "allowed" ? el("span", { class: "mark-ok" }, "✓")
    : look === "refused" ? el("span", { class: "mark-no" }, "✕")
      : look === "checking" ? el("span", { class: "shimmer" }, "Checking…") : null;
  return el("div", { class: `lane ${look}${arrived ? " arrived" : ""}` },
    el("div", { class: "track" },
      el("button", {
        class: "pill", "aria-expanded": String(open), "aria-label": `${example.title}: ${verdictWord(preview)}`,
        title: open ? "Hides the request and decision." : "Shows the request and decision.",
        onclick: () => {
          if (open) state.open.delete(example.title);
          else {
            state.open.add(example.title);
            reveal = true;
          }
          draw();
        },
      }, glyph(example.symbol), el("span", { class: "title" }, example.title), end),
      el("i", { class: "flare", "aria-hidden": "true" })),
    open ? detail(example, preview) : null);
}

function verdictWord(preview) {
  if (!preview) return "Not checked";
  if (preview === "checking") return "Checking…";
  if (preview === "noAnswer") return "Didn't answer";
  if (preview.error) return "Couldn't check";
  switch (preview.verdict.verdict) {
    case "allow": return "Allowed";
    case "owner_said_no": return "You said no";
    case "host_refused": return "Refused by this PC";
    default: return "Needs your OK";
  }
}

/** A sentence's first sentence, for a single line. */
const first = (said) => {
  const end = said.indexOf(". ");
  return end < 0 ? said : `${said.slice(0, end)}.`;
};

/** Why it couldn't be checked, or why this PC refuses it, in the app's
 * words: one sentence, with no step to take from here. */
function reason(example, preview) {
  if (preview === "noAnswer") return "The gatekeeper took longer than 15 seconds.";
  if (preview?.error) {
    return sentence(preview.error, { host: THIS_PC, place: { tryRequest: example.request.words?.[0] ?? null } }) ?? "Latch couldn't do that.";
  }
  const refused = preview?.verdict?.refused;
  if (!refused) return null;
  const op = preview.verdict.request?.op;
  const said = sentence({ error: refused }, { host: THIS_PC, place: { tryRequest: ["google", "messages", "contacts"].includes(op) ? op : null } });
  return said ? first(said) : null;
}

function detail(example, preview) {
  const why = reason(example, preview);
  const latchSaid = preview?.error ?? (preview?.verdict?.refused ? { error: preview.verdict.refused } : null);
  return el("div", { class: "detail" },
    el("p", {}, example.detail),
    el("p", { class: "decision" }, el("strong", {}, "Decision: "), verdictWord(preview) + (why ? `. ${why}` : "")),
    latchSaid ? errorText(latchSaid, { said: "", tone: "quiet" }) : null);
}

/** Reads the saved rules into the box, or on a first run the first
 * default's, so it's never empty on arrival; then reviews the examples. */
async function loadRules() {
  try {
    const shown = await latch(["rules", "show"]);
    const saved = shown?.every_agent?.text ?? null;
    // The box follows the saved rules unless the person changed it since
    // it was filled: rules saved elsewhere meanwhile (the main window's
    // Rules, before "Set up again") show here, as on the Mac, whose setup
    // starts afresh each time.
    const followed = !state.draftRules || state.draftRules === state.filledRules;
    state.savedRules = saved;
    if (followed) state.draftRules = state.filledRules = saved?.trim() ? saved : DECKS[0].rules;
    const match = DECKS.findIndex((one) => one.rules === state.draftRules.trim());
    if (match >= 0) state.deck = match;
    state.rulesError = null;
    state.rulesUnreadable = false;
  } catch (error) {
    state.rulesError = error;
    state.rulesUnreadable = true;
  }
  if (state.step === "rules") startReview();
}

/** "Start with": a default's rules in the box, its requests judged at once. */
function choose(index) {
  state.deck = index;
  state.open = new Set();
  state.draftRules = DECKS[index].rules;
  startReview();
}

/** The box changed: the examples show Checking… at once and are judged a
 * second after the person stops typing. */
function draftChanged() {
  if (previewedRules() === state.reviewed) return draw();
  startReview(1000);
}

let reviewTimer = null;

/** Judges each example against the draft as a new agent would be judged
 * (`rules try --new-agent --draft -`), all at once, each verdict shown as
 * it comes, or no answer after 15 s. Nothing is saved. */
function startReview(delay = 0) {
  const draft = previewedRules();
  state.reviewed = draft;
  const current = ++state.review;
  const examples = deck().examples;
  for (const example of examples) state.previews[example.title] = "checking";
  state.reviewing = true;
  state.macLit = false;
  clearTimeout(reviewTimer);
  draw();
  reviewTimer = setTimeout(async () => {
    await Promise.all(examples.map(async (example) => {
      const preview = await judge(example, draft);
      if (current !== state.review) return;
      state.previews[example.title] = preview;
      if (state.step === "rules") draw();
    }));
    if (current !== state.review) return;
    state.reviewing = false;
    if (state.step === "rules") draw();
    // An allowed request reaches the host: it lights up briefly.
    if (!examples.some((example) => state.previews[example.title]?.verdict?.verdict === "allow")) return;
    await sleep(420);
    if (current !== state.review) return;
    state.macLit = true;
    if (state.step === "rules") draw();
    await sleep(260);
    state.macLit = false;
    if (current === state.review && state.step === "rules") draw();
  }, delay);
}

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

/** One example's verdict, or no answer once 15 s pass first. */
function judge(example, draft) {
  const asking = latch(tryArgs(example.request, [], { newAgent: true, draft }), draft).then(
    (verdict) => (typeof verdict?.verdict === "string" ? { verdict } : { error: unreachable("Latch returned an unreadable response.") }),
    (error) => ({ error }));
  return Promise.race([asking, sleep(15000).then(() => "noAnswer")]);
}

/** Continue: saves the rules only if the person changed them. An empty box
 * never clears rules that are saved. */
async function saveRules() {
  if (state.rulesUnreadable) return;
  if (!rulesChanged()) return next();
  const draft = state.draftRules.trim();
  state.savingRules = true;
  draw();
  try {
    await latch(["rules", "set", "--file", "-"], draft);
    state.savedRules = state.filledRules = state.draftRules = draft;
    state.rulesError = null;
    state.savingRules = false;
    next();
  } catch (error) {
    state.rulesError = error;
    state.savingRules = false;
    draw();
  }
}

// ── Grant access: Google ─────────────────────────────────────────────────

const NOT_LOGGED_IN = "This PC isn't logged in to Plow. Go back to log in, then try again.";
const COULD_NOT_CHECK = "Latch couldn't check Google just now. Try again.";

/** From latch's enable, which lists the accounts that work and the rest apart. */
function accountsEnabled(enabled) {
  const again = enabled?.needs_sign_in ?? [];
  const working = enabled?.accounts ?? [];
  return { accounts: [...working, ...again.filter((a) => !working.includes(a))], needsSignIn: again };
}

/** latch's refusal, said plainly: decided on its code, never shown. */
function googleProblem(error) {
  switch (code(error)) {
    case "plow_unreachable": return ["unreachable", null];
    case "not_logged_in": return ["failed", NOT_LOGGED_IN];
    case "plow_error": return ["failed", "Plow couldn't answer just now. Try again in a moment."];
    case "gog_not_configured": return ["failed", TOOLS_MISSING];
    // No connected account works until the owner signs in again.
    case "google_needs_sign_in": return ["notConnected", null];
    default: return ["failed", COULD_NOT_CHECK];
  }
}

function setGoogle([google, why]) {
  state.google = google;
  state.googleWhy = why;
}

/** What latch says of Google now: on arrival, on every return to the
 * window, and on Try Again. An account connected in Plow turns Google on
 * here, with no press. */
async function checkGoogle() {
  if (state.lookingAtGoogle) return;
  state.lookingAtGoogle = true;
  if (state.google !== "waiting" && state.google !== "connected") setGoogle(["checking", null]);
  if (state.step === "access") draw();
  const before = working(state.accounts);
  const seen = await lookAtGoogle();
  state.lookingAtGoogle = false;
  if (seen) setGoogle(seen);
  // A sign-in finished after the wait ended: its timeout no longer holds.
  if (working(state.accounts).some((a) => !before.includes(a))) state.googleWaitEnded = null;
  if (state.step === "access") draw();
}

/** One look: `setup --status` reads what Plow says without changing
 * anything; an account there and Google off here runs `plugins enable
 * google`. Null when the screen went meanwhile. */
async function lookAtGoogle() {
  let setup;
  try {
    setup = await latch(["setup", "--status"]);
  } catch (error) {
    return state.step === "access" ? googleProblem(error) : null;
  }
  if (state.step !== "access") return null;
  const steps = Array.isArray(setup?.steps) ? setup.steps : [];
  if (steps.find((s) => s.step === "login")?.state !== "done") return ["failed", NOT_LOGGED_IN];
  const step = steps.find((s) => s.step === "google");
  if (!step) return ["failed", COULD_NOT_CHECK];
  const accounts = accountsOf(step);
  if (!accounts) return ["unreachable", null];
  state.accounts = accounts;
  if (step.state === "done") return ["connected", null];
  // While latch waits for a sign-in, turning Google on is its to do.
  if (state.google === "waiting") return ["waiting", null];
  if (!accounts.accounts.length) return ["notConnected", null];
  try {
    const enabled = await latch(["plugins", "enable", "google"]);
    if (Array.isArray(enabled?.accounts)) state.accounts = accountsEnabled(enabled);
    return ["connected", null];
  } catch (error) {
    return state.step === "access" ? googleProblem(error) : null;
  }
}

/** Try Again, or the window back in front: a look, unless one is under
 * way or latch is waiting for a sign-in. */
function recheckGoogle() {
  if (state.step !== "access" || state.still || state.lookingAtGoogle || state.google === "waiting") return;
  checkGoogle();
}

/** Connect Google…, Add Another Account… or Sign In Again…: latch gets a
 * sign-in link from Plow, which opens in the browser, and waits for the
 * account, then turns Google on; only ever on a click. */
function connectGoogle() {
  const adding = addsAccount(state.accounts);
  state.googleWaitEnded = null;
  setGoogle(["waiting", null]);
  let opened = false;
  let answered = false;
  draw();
  follow("google", ["plugins", "enable", "google", "--events", ...(adding ? ["--add-account"] : [])], (line) => {
    if (line.type === "link" && line.url) {
      // A credential until used: opened, never shown.
      state.googleLink = line.url;
      if (!opened) {
        opened = true;
        openURL(line.url).catch(() => {});
      }
    } else if (line.type === "result") {
      answered = true;
      state.googleLink = null;
      state.accounts = accountsEnabled(line.result);
      setGoogle(["connected", null]);
      unfollow("google");
    } else return;
    if (state.step === "access") draw();
  }, (failure) => {
    if (answered || state.step !== "access") return;
    state.googleLink = null;
    if (!failure) {
      // latch stopped before it answered: what's there now.
      setGoogle(["checking", null]);
      checkGoogle();
      return;
    }
    if (code(failure) === "sign_in_timeout") {
      // Adding, the accounts already there still work.
      setGoogle([adding ? "connected" : "notConnected", null]);
      state.googleWaitEnded = signInProblem(failure, adding);
    } else setGoogle(googleProblem(failure));
    draw();
  });
}

function grantAccess() {
  const google = state.google;
  const accounts = state.accounts;
  const canSignIn = google === "notConnected" || google === "connected";
  const stateWords = {
    checking: "Checking…", notConnected: null, connected: null, waiting: "Waiting for sign-in",
    unreachable: "Couldn't reach Plow", failed: "Couldn't check",
  }[google] ?? (!accounts.accounts.length ? "Not connected" : noneWorks(accounts) ? "Needs sign-in" : "Connected");
  const tone = { connected: "volt", notConnected: "amber", waiting: "amber", unreachable: "amber", failed: "clay", checking: "muted" }[google];
  const problem = (why) => [el("div", { class: "row clay" }, why),
    el("div", { class: "row end" }, el("button", { class: "sys", onclick: () => checkGoogle() }, "Try Again"))];
  const title = noneWorks(accounts) ? null : addsAccount(accounts) ? "Add Another Account…" : "Connect Google…";
  return el("div", { class: "access" },
    header("Grant access", "The Plow Gatekeeper will monitor how your agents use these permissions."),
    el("div", { class: "form" },
      el("div", { class: "panel" },
        el("div", { class: "row" }, el("span", { class: "grow" }, "Google"), el("span", { class: tone }, stateWords)),
        accounts.accounts.map((account) => {
          const needs = accounts.needsSignIn.includes(account);
          return el("div", { class: "row account" }, icon("envelope"),
            el("span", { class: "grow" }, el("span", {}, account),
              needs && !noneWorks(accounts) ? el("small", {}, RECONNECT) : null),
            needs ? el("span", { class: "muted" }, "Needs sign-in") : null,
            needs && noneWorks(accounts) ? el("button", { class: "sys", disabled: !canSignIn, onclick: connectGoogle }, "Sign In Again…") : null);
        }),
        state.googleWaitEnded ? el("div", { class: "row muted" }, state.googleWaitEnded) : null,
        google === "waiting" ? [
          el("div", { class: "row" }, spinner(), el("span", {}, "Finish signing in with Google in your browser. This screen updates when you're done.")),
          el("div", { class: "row end" }, el("button", {
            class: "sys", disabled: !state.googleLink, onclick: () => state.googleLink && openURL(state.googleLink).catch(() => {}),
          }, "Open the Sign-In Page Again")),
        ] : null,
        google === "unreachable"
          ? problem("Latch couldn't get an answer from Plow, so it can't tell whether Google is connected. Check your internet connection, then try again.")
          : null,
        google === "failed" ? problem(state.googleWhy) : null,
        canSignIn && title ? el("div", { class: "row end" }, el("button", { class: "sys", onclick: connectGoogle }, title)) : null),
      el("p", { class: "form-note" },
        "Gmail and Google Calendar, for every host where the Google plugin is on. You sign in with Google in your browser.")),
    stepDone() ? null : el("p", { class: "later" }, "You can finish this later in Settings."));
}

// ── Start Latch ──────────────────────────────────────────────────────────

function startScreen() {
  const hosting = state.status?.this_machine?.hosting;
  const keepAwake = hosting?.keep_awake ?? null;
  return el("div", { class: "start" },
    mark("mark small"),
    header("Start Latch", "Choose when Latch runs on this PC."),
    el("div", { class: "panel" },
      toggle("Run Latch now", "refresh", state.runInBackground, (on) => {
        state.runInBackground = on;
        // Starting at login needs a host this PC started in the background.
        if (!on) state.startAtLogin = false;
        draw();
      }),
      state.loginItemOffered ? toggle("Start when you log in", "clock", state.startAtLogin, (on) => {
        state.startAtLogin = on;
        draw();
      }, { disabled: !state.runInBackground, dim: !state.runInBackground }) : null,
      toggle("Keep this PC awake while plugged in", "cup", keepAwake?.enabled ?? false, (on) => setKeepAwake(on),
        { disabled: keepAwake == null })),
    keepAwakeSaid(keepAwake),
    state.loginItemOffered && !state.runInBackground
      ? el("p", { class: "muted" }, "Latch can start when you log in only if it runs now.") : null,
    hosting?.blocked ? errorText({ error: hosting.blocked }, { tone: "clay callout" })
      : hosting?.ready ? el("p", { class: "muted" }, "Latch is already running on this PC.") : null,
    state.startError ? errorText(state.startError, { tone: "clay callout" }) : null,
    el("p", { class: "muted" }, "Your PC stays reachable while it's awake and Latch is running."));
}

function toggle(title, symbol, on, change, { disabled = false, dim = false } = {}) {
  const input = el("input", { type: "checkbox", role: "switch", "aria-label": title, disabled, onchange: (event) => change(event.target.checked) });
  input.checked = on;
  return el("label", { class: `row toggle${dim ? " dim" : ""}` }, icon(symbol), el("span", { class: "grow" }, title),
    el("span", { class: "switch" }, input, el("i", { "aria-hidden": "true" })));
}

/** What the host does about keep-awake, and a failed save; with Run Latch
 * now on, a host not running yet is about to be (`KeepAwakeSwitch.Said`).
 * latch on Windows can't hold a PC awake: it says `unsupported`. The Mac's
 * own sentences for its battery, macOS refusing and power it can't read
 * are left out. */
function keepAwakeSaid(keepAwake) {
  if (state.keepAwakeError) return errorText(state.keepAwakeError, { tone: "clay" });
  if (state.status && keepAwake == null) return el("p", { class: "muted" }, "Latch couldn't read this PC's settings.");
  if (!keepAwake?.enabled || keepAwake.held == null) return null;
  if (keepAwake.held) return el("p", { class: "muted" }, "Latch is keeping this PC awake while it's plugged in.");
  const said = {
    not_serving: state.runInBackground ? "Once Latch is running, it keeps this PC awake while it's plugged in." : "Latch isn't running, so this PC sleeps as usual.",
    unsupported: "This PC can't be kept awake by Latch.",
  }[keepAwake.reason];
  return said ? el("p", { class: "muted" }, said) : null;
}

/** latch saves the choice (no host or restart needed); the switch then
 * shows what latch reads back. One save at a time. */
async function setKeepAwake(on) {
  if (state.savingKeepAwake) return;
  state.savingKeepAwake = true;
  try {
    await latch(["settings", "set", "--keep-awake", on ? "on" : "off"]);
    state.keepAwakeError = null;
  } catch (error) {
    state.keepAwakeError = error;
  }
  state.savingKeepAwake = false;
  await refreshStatus();
}

/** Whether Latch starts when this user logs in, from the app (a Run entry
 * under the user's registry key), or fake mode's. */
const loginItem = () => (fakeSetup ? fakeSetup.loginItem() : invoke("login_item"));
const setLoginItem = (on) => (fakeSetup ? fakeSetup.setLoginItem(on) : invoke("set_login_item", { on }));

/** Starts the host in the background and sets the login item, as the
 * switches say. */
async function startLatch() {
  state.starting = true;
  state.startError = null;
  draw();
  try {
    if (state.runInBackground && !ready()) await startHost();
    if (state.loginItemOffered) await setLoginItem(state.runInBackground && state.startAtLogin);
    await refreshStatus();
    state.starting = false;
    next();
  } catch (error) {
    state.startError = error;
    await refreshStatus();
    state.starting = false;
    draw();
  }
}

// ── Done ─────────────────────────────────────────────────────────────────

/** Done's list of what's set up, all from latch: its status, its rules and
 * its setup steps (`Summary.rows`). Contacts & Messages is a Mac's. */
function summary() {
  const loggedIn = state.status?.login != null;
  const serving = ready();
  const failed = state.status == null;
  const google = state.setup ? state.setup.steps?.find((s) => s.step === "google")?.state === "done" : null;
  const rules = state.rulesSaved ?? (state.savedRules != null ? Boolean(state.savedRules.trim()) : null);
  return [
    ["check", "Plow", failed ? "Couldn't check" : loggedIn ? "Logged in" : "Not logged in", loggedIn],
    ["rules", "Your rules", rules == null ? "Couldn't check" : rules ? "Saved" : "Not set up", rules === true],
    ["google", "Google", google == null ? "Couldn't check" : google ? "Connected" : "Not set up", google === true],
    ["refresh", "Latch", failed ? "Couldn't check" : serving ? "Running" : "Not running", serving],
  ];
}

function doneScreen() {
  return el("div", { class: "done" },
    el("span", { class: "done-check" }, icon("check")),
    el("h1", {}, "You're all set"),
    el("div", { class: "panel" }, summary().map(([symbol, title, words, set]) =>
      el("div", { class: "row" }, icon(symbol), el("span", { class: "grow" }, title), el("span", { class: set ? "volt" : "muted" }, words)))),
    el("p", { class: "muted center" }, ready() ? "Your agents can now use this PC." : "You can review or change your setup in Latch."));
}

// ── latch, as each screen opens ──────────────────────────────────────────

async function refreshStatus() {
  try {
    state.status = await latch(["status", "--local"]);
  } catch {}
  if (["start", "done"].includes(state.step)) draw();
}

/** Keeps `state.status` as latch reports it, live, until the window closes. */
let followingStatus = false;

function followStatus() {
  if (followingStatus) return;
  followingStatus = true;
  follow("status", ["status", "--local", "--follow"], (line) => {
    if (line.type !== "status") return;
    state.status = line.status;
    if (["start", "done"].includes(state.step)) draw();
  }, () => (followingStatus = false));
}

const ENTER = {
  login: () => startLogin(),
  rules: () => loadRules(),
  access: async () => {
    await refreshStatus();
    await checkGoogle();
  },
  start: async () => {
    followStatus();
    try {
      const on = await loginItem();
      state.loginItemOffered = true;
      // A rerun shows the login item as it is; a first run offers it on.
      if (state.rerun) state.startAtLogin = Boolean(on) && state.runInBackground;
    } catch {
      state.loginItemOffered = false;
      state.startAtLogin = false;
    }
    if (state.step === "start") draw();
  },
  done: async () => {
    await refreshStatus();
    try {
      state.rulesSaved = (await latch(["rules", "show"]))?.every_agent != null;
    } catch {
      state.rulesSaved = null;
    }
    try {
      state.setup = await latch(["setup", "--status"]);
    } catch {
      state.setup = null;
    }
    if (state.step === "done") draw();
  },
};

// Back from the browser, or from anywhere: Google is looked at again.
window.addEventListener("focus", () => recheckGoogle());

/** Opens where setup stands: a PC that isn't logged in goes back to login,
 *  whatever step was passed before, as the Mac app's `Progress.destination`. */
async function boot() {
  const fake = await connect();
  // Fake mode can open setup at a step (`?step=login`), and stage one of
  // the Mac's stills there (`&still=02-login-waiting`), for its stills.
  const asked = new URLSearchParams(location.search).get("step");
  if (fake && STEPS.includes(asked)) {
    fakeSetup = await import("./fake/setup.js");
    const staged = fakeSetup.staged(DECKS);
    state.step = asked;
    if (staged) Object.assign(state, staged, { still: true });
    reveal = state.open.size > 0;
    draw();
    if (!state.still) ENTER[state.step]?.();
    return;
  }
  try {
    const status = await latch(["status", "--local"]);
    state.status = status;
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
    // Every screen again, each showing what's set up now.
    state.rerun = true;
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
  main.app.start = () => startHosting(main.app);
  await main.open(root);
}

boot();
