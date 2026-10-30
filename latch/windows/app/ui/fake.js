// Fake mode: latch's answers from the Mac app's snapshot fixtures
// (gui/Sources/PlowLatchUI/App/Snapshots.swift, `Fixtures`; copied by
// ../fixtures.py), routed as its `FakeLatchClient` routes them. Nothing runs
// and nothing changes: a save or a removal answers as if it worked.

const fixtures = await (await fetch(new URL("fake/fixtures.json", import.meta.url))).json();
const one = (name) => JSON.parse(fixtures[name]);
const each = (name) => fixtures[name].split("\n").filter(Boolean).map((line) => JSON.parse(line));

const failed = (code, message) => ({ error: { code, message } });

/** The host a command names with `--host`, if any. */
const on = (args) => {
  const at = args.indexOf("--host");
  return at < 0 ? null : args[at + 1];
};

/** This Mac's fixture ("studio"), or pi-kitchen's for another host. */
const per = (args, studio, kitchen) => (on(args) && on(args) !== "studio" ? kitchen : studio);

// The address can stage this PC and the account as the Mac's stills do:
// status=stopped|servingAttached|otherVersion|laptop|failed, update=1,
// account=studioHosts|laptopHosts|laptopHostsPiOffline|noHosts|unreachable.
const asked = new URLSearchParams(location.search);

function localStatus() {
  if (asked.get("status") === "failed") throw failed("io_error", "latch couldn't read its state folder");
  const status = one(asked.get("status") ?? "serving");
  if (asked.get("update")) status.update = { available: true, build: 139, checked_at: "2026-10-22T22:00:00Z" };
  return status;
}

function accountStatus() {
  if (asked.get("account") === "unreachable") {
    throw failed("plow_unreachable", "could not reach Plow at http://127.0.0.1:9: connection refused");
  }
  return one(asked.get("account") ?? "studioHosts");
}

const answers = [
  [["status", "--local"], localStatus],
  [["status", "--host"], () => one("piStatus")],
  [["status"], accountStatus],
  [["rules", "show"], (args) => one(per(args, "studioRules", "piRules"))],
  [["vault", "list"], (args) => one(per(args, "studioVault", "piVault"))],
  [["agents", "catalog"], () => one("catalog")],
  [["agents", "start-by-text"], () => one("startByText")],
  [["agents"], (args) => one(on(args) ? per(args, "agentsStudio", "agentsKitchen") : "agents")],
  [["devices"], () => one("devices")],
  [["update", "--check"], () => one("updateCheck")],
];

// Each page's own answers (fake/<page>.js, exporting `answers` in the shape
// above), tried first.
for (const name of ["activity", "rules", "vault", "settings", "agents", "connections", "app-settings"]) {
  try {
    answers.unshift(...(await import(`./fake/${name}.js`)).answers);
  } catch {}
}

export async function run(args, input) {
  for (const [prefix, answer] of answers) {
    if (prefix.every((word, i) => args[i] === word || (word === "--host" && args.includes("--host")))) {
      // An answer that gives undefined isn't this still's: the next is tried.
      const value = await answer(args, input);
      if (value !== undefined) return value;
    }
  }
  // Writes (rules set, vault remove, stop…) answer as if they worked.
  if (["set", "clear", "remove", "edit", "add", "stop", "enable", "disable"].some((word) => args.includes(word))) return {};
  throw failed("not_in_fixtures", `fake mode has no answer for latch ${args.join(" ")}`);
}

export async function lines(args) {
  if (args[0] === "audit") return each(per(args, "studioAudit", "kitchenAudit"));
  return [await run(args)];
}

/** A file a command would write: a small grey PNG (a screenshot's place),
 * or a contact card. */
export async function file(args, kind) {
  if (kind === "vcf") return `data:text/vcard;base64,${btoa("BEGIN:VCARD\nVERSION:3.0\nFN:scout\nEND:VCARD\n")}`;
  return "data:image/svg+xml;base64," + btoa('<svg xmlns="http://www.w3.org/2000/svg" width="640" height="400"><rect width="640" height="400" fill="#d9d9d9"/><text x="320" y="205" font-size="22" text-anchor="middle" fill="#666">screenshot</text></svg>');
}

/** The Open dialog's answer: the Mac's import fixture's file. */
export async function pick() {
  return "C:\\Users\\me\\Downloads\\passwords.csv";
}

/** A stream's lines, then nothing more until it's stopped, as the Mac's
 * fake `statusFollow` and `auditFollow` do. */
export function stream(args, line) {
  queueMicrotask(() => {
    // Whatever `status --local` answers, a page's own fake included.
    if (args[0] === "status") run(["status", "--local"]).then((status) => line({ type: "status", status }));
    if (args[0] === "audit") for (const value of each(per(args, "studioAudit", "kitchenAudit"))) line(value);
  });
}
