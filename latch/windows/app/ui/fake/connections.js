// Fake mode for Connections: the Mac's stills of it (App/Snapshots.swift,
// `settingsApp` and its `Fixtures.setup(on: [], google:)`), picked by the
// address:
//   ?google=none|one|two|three|some|all|unknown|pending   Google's accounts
//     (none when another of these is given without it)
//   ?devices=fail|pending        `latch devices` failing, or still reading
//   ?removeDevice=fail           a removal of a device already gone
//   ?signin=adding|first|timed-out   the sign-in sheet, staged with a link
// `setup --status` is answered here only when one of these is in the
// address; otherwise this gives undefined and Host Settings' own answer
// (fake/settings.js) is tried next. A prefix ending in `undefined`
// matches only that command with nothing after it (`setup --status`, not
// `setup --status --check-access`).

const fixtures = await (await fetch(new URL("fixtures.json", import.meta.url))).json();
const asked = new URLSearchParams(location.search);

const failed = (code, message) => ({ error: { code, message } });

/** `setup --status`'s Google step with these accounts connected in Plow
 * (the plugin off here). */
const THREE = ["alex@example.com", "alex.work@example.com", "family@example.com"];
const connected = (accounts, needsSignIn = []) => JSON.stringify({
  step: "google", state: "needed",
  reason: `Google is connected in Plow (${accounts.join(", ")}); the Google connector is off.`,
  args: ["plugins", "enable", "google"], enabled: false, ready: false, accounts,
  ...(needsSignIn.length ? { needs_sign_in: needsSignIn } : {}),
});
const GOOGLE = {
  none: fixtures.googleNotConnected,
  one: connected(["ada@example.com"]),
  two: connected(["ada@example.com", "ada.work@example.com"]),
  three: connected(THREE),
  some: connected(THREE, ["family@example.com"]),
  all: connected(THREE, THREE),
  unknown: fixtures.googleUnreachable,
};

/** `latch setup --status` on a logged-in PC whose host is stopped, with
 * every connector off and Google's step as given. */
function setup(google) {
  const steps = [{ step: "login", state: "done", reason: "Logged in as studio.", args: null }];
  for (const name of ["google", "messages", "contacts"]) {
    steps.push(name === "google" ? JSON.parse(google) : {
      step: name, state: "needed", reason: `The ${name[0].toUpperCase() + name.slice(1)} connector is off.`,
      args: ["plugins", "enable", name], enabled: false, ready: false,
    });
  }
  steps.push({ step: "host", state: "needed", reason: "latch is not running.", args: ["serve", "-d", "--again"] });
  return { steps };
}

const google = asked.get("google") ?? "none";
const devices = asked.get("devices");
const mine = ["google", "devices", "signin", "removeDevice"].some((name) => asked.has(name));

export const answers = [
  [["setup", "--status", undefined], () => {
    if (!mine) return undefined;
    return google === "pending" ? new Promise(() => {}) : setup(GOOGLE[google] ?? GOOGLE.none);
  }],
  [["devices", "remove"], (args) => {
    if (asked.get("removeDevice") === "fail") throw failed("not_found", `no device named "${args[2]}" in this account`);
    return {};
  }],
  ...(devices ? [[["devices", undefined], () => {
    if (devices === "pending") return new Promise(() => {});
    throw failed("plow_unreachable", "could not reach Plow");
  }]] : []),
];

// The sign-in sheet's stills: the page stages it, as the Mac's do.
const SHEETS = {
  adding: { adding: true, failure: null },
  first: { adding: false, failure: null },
  "timed-out": {
    adding: true,
    failure: "No new Google account showed up in time. An account that's already connected isn't added again.",
  },
};
const sheet = SHEETS[asked.get("signin")];
if (sheet) {
  import("../pages/connections.js").then((page) => page.stage({
    ...sheet, link: sheet.failure ? null : "http://127.0.0.1:18934/v1/connectors/gmail/connect/EXAMPLE_ONLY",
  }));
}
