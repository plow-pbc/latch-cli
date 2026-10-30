// The Mac app's Settings window (Main/AppSettingsView.swift), here a page
// from the sidebar's foot: this PC's Latch updates, and logging this PC
// out of the account.

import { el, button } from "../dom.js";
import { latch } from "../latch.js";
import { errorText } from "./agents.js";

/** What the page keeps: the last update check (`{ value }` or `{ error }`)
 * while one runs, and why logging out failed. */
function kept(app) {
  return (app.pages["app-settings"] ??= { update: null, checking: false, logOutError: null });
}

function redraw(app) {
  if (app.place === "app-settings") app.drawPage();
}

export function render(app) {
  const page = kept(app);
  const name = app.local?.login ? app.local.this_machine.name : null;
  return el("div", { class: "app-settings-page" },
    el("section", { class: "group" }, el("h3", {}, "Updates"),
      el("div", { class: "card" },
        el("div", { class: "line" }, el("span", {}, "Latch"), el("span", { class: "value" }, updateText(app, page)),
          button(page.checking ? "Checking…" : "Check Now", () => check(app), { disabled: page.checking })),
        page.update?.error ? el("div", { class: "line st-bad" }, "Couldn't check for updates. Try again in a moment.") : null,
        available(app, page)
          ? el("div", { class: "line st-quiet" }, el("span", {}, "Installing it from the app isn't available yet: run ", el("code", {}, "latch update"),
            " in a terminal. It restarts Latch on this PC and reopens this window on the new version."))
          : null)),
    el("section", { class: "group" }, el("h3", {}, "Account"),
      el("div", { class: "card" },
        el("div", { class: "line" }, el("span", {}, "Logged in as"), el("span", { class: "value" }, name ?? "Not logged in"),
          name ? button("Log Out…", () => logOut(app)) : null),
        name && page.logOutError ? el("div", { class: "line st-bad" }, errorText(page.logOutError)) : null),
      el("p", { class: "st-foot" }, "Logging out stops Latch on this PC and takes it out of your Plow account.")));
}

/** `latch update --check`, only on a click. */
async function check(app) {
  const page = kept(app);
  page.checking = true;
  redraw(app);
  try {
    page.update = { value: await latch(["update", "--check"]) };
  } catch (error) {
    page.update = { error };
  }
  page.checking = false;
  redraw(app);
}

function available(app, page) {
  if (page.update?.value) return page.update.value.available;
  return app.local?.update?.available === true;
}

function updateText(app, page) {
  const check = page.update?.value;
  if (check) {
    if (check.available) return `Build ${check.latest?.build ?? "?"} is available`;
    if (check.current == null) return `A local build · latest is ${check.latest?.tag ?? "unknown"}`;
    return `Up to date · build ${check.current}`;
  }
  const update = app.local?.update;
  if (update?.available) return `Build ${update.build ?? "?"} is available`;
  return "Not checked yet";
}

/** `latch logout --yes`, once confirmed: this PC leaves the account, then
 * setup takes over at Log in, once the person has read why Plow still
 * lists it, if it does. */
async function logOut(app) {
  const yes = await app.confirm("Log out this PC?",
    "Latch stops here and this PC leaves your Plow account. Its vault and activity stay, readable after you log in again.",
    "Log Out");
  if (!yes) return;
  const page = kept(app);
  let result;
  try {
    result = await latch(["logout", "--yes"]);
    page.logOutError = null;
  } catch (error) {
    page.logOutError = error;
    redraw(app);
    return;
  }
  const device = result?.device;
  if (device && !device.removed) {
    await notice(app, "Still in your Plow account",
      `This PC is logged out, but Plow couldn't take ${device.name ?? "it"} out of your account. It may still be listed until Plow can be reached.`);
  }
  // Setup takes over at Log in (Settings' own fallback: from the start).
  (app.loggedOut ?? app.setUpAgain)?.();
}

/** An alert with only OK: resolves once it's read. */
function notice(app, title, message) {
  return new Promise((resolve) => {
    const ok = el("button", { class: "primary", autofocus: true }, "OK");
    const close = app.sheet([el("h2", {}, title), el("p", {}, message), el("div", { class: "actions" }, ok)], { width: 420 });
    const dialog = ok.closest("dialog");
    dialog.classList.add("ag-sheet");
    dialog.addEventListener("close", () => resolve());
    ok.addEventListener("click", () => close());
  });
}
