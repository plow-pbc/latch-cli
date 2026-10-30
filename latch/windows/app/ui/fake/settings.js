// Fake mode's answers for Host Settings: this PC as latch on Windows says
// it (its plugins Google alone: Messages and Contacts are a Mac's), another
// host's own status with its plugins (`Fixtures.status(piStatus, plugins:
// piPlugins)`), and a start (`setup --status`, then `serve -d`). For a
// still (`?still=NAME`, named after the Mac's), what latch said when the
// Mac drew it:
//   19-settings-this-mac-serving              serving, Google off
//   19-settings-this-mac-keep-awake-on-battery keep-awake on: on Windows latch says `unsupported`
//   19-settings-this-mac-stopped-update       stopped, a newer build found
//   19-settings-this-mac-blocked              another latch version holds the state folder
//   19-settings-this-mac-plugins              Google on
//   19-settings-remote-online                 (with &host=pi-kitchen)
//   19-settings-remote-offline                pi-kitchen offline (with &host=pi-kitchen)
//   19-settings-viewer-only                   this PC hosts nothing (with &host=pi-kitchen)
//   settings-start-blocked                    stopped, and latch says it can't start now
//   settings-stop-fails                       a stop latch refuses
//   settings-plugin-refused                   Google can't be turned on: no account connected
//   settings-remote-unreachable               pi-kitchen online in Plow, but out of reach (with &host=pi-kitchen)

const fixtures = await (await fetch(new URL("fixtures.json", import.meta.url))).json();
const one = (name) => JSON.parse(fixtures[name]);
const still = new URLSearchParams(location.search).get("still") ?? "";
const failed = (code, message) => ({ error: { code, message } });

const on = (args) => {
  const at = args.indexOf("--host");
  return at < 0 ? null : args[at + 1];
};
const kitchen = (args) => on(args) != null && on(args) !== "studio";

const google = (state) => ({
  name: "google", state,
  reason: state === "enabled" ? "enabled; agents can use it once latch is running" : "its owner has not enabled it on this host",
  enable: state === "enabled" ? null : "latch plugins enable google",
});

/** `status --local` on this PC, as each still has it. */
function local() {
  // Not one of Host Settings' stills: fake.js's own answer (its `status=`).
  if (!still) return undefined;
  const status = one({
    "19-settings-this-mac-stopped-update": "stopped",
    "19-settings-this-mac-blocked": "otherVersion",
    "19-settings-viewer-only": "laptop",
    "settings-start-blocked": "stopped",
  }[still] ?? "serving");
  status.plugins = [google(still === "19-settings-this-mac-plugins" ? "enabled" : "disabled")];
  if (still === "19-settings-this-mac-keep-awake-on-battery") {
    status.this_machine.hosting.keep_awake = { enabled: true, held: false, reason: "unsupported" };
  }
  return status;
}

/** `latch status`: the account's hosts, as each still has them. */
function account() {
  if (still === "19-settings-viewer-only") return one("laptopHosts");
  if (still === "19-settings-remote-offline") {
    return JSON.parse(fixtures.studioHosts.replace(
      '"self":false,"online":true,"last_seen_at":"2026-10-22T22:06:30.037004Z"', '"self":false,"online":false,"last_seen_at":null'));
  }
  return one("studioHosts");
}

const setup = () => ({
  steps: [still === "settings-start-blocked"
    ? { step: "host", state: "blocked", reason: "a host from another latch version (pid 4242) holds this state folder; stop it with `taskkill /PID 4242`, then start latch again", args: null }
    : { step: "host", state: "needed", reason: "latch is not serving on this machine", args: ["serve", "-d"] }],
});

export const answers = [
  [["status", "--local"], local],
  [["status", "--host"], () => {
    if (still === "settings-remote-unreachable") throw failed("host_unreachable", 'could not connect to host "pi-kitchen"; nothing was sent');
    return { ...one("piStatus"), plugins: JSON.parse(fixtures.piPlugins) };
  }],
  ...(still === "settings-remote-unreachable" ? [[["rules", "show"], (args) => {
    if (kitchen(args)) throw failed("host_unreachable", 'could not connect to host "pi-kitchen"; nothing was sent');
    return one("studioRules");
  }]] : []),
  ...(still.startsWith("19-settings-") ? [
    [["status"], account],
    [["rules", "show"], (args) => one(kitchen(args) ? "rulesKitchen" : "rulesStudio")],
  ] : []),
  [["setup", "--status"], setup],
  [["serve"], () => ({})],
  [["stop"], () => {
    if (still === "settings-stop-fails") throw failed("state_busy", "another latch is changing C:\\Users\\me\\AppData\\Local\\Plow\\Latch; try again");
    return {};
  }],
  [["plugins", "enable"], () => {
    if (still === "settings-plugin-refused") {
      throw failed("google_not_connected", "Google is not connected in Plow: connect it at http://localhost:3000/connections, then run `latch plugins enable google` again");
    }
    return { accounts: ["testuser@plow.test"] };
  }],
];
