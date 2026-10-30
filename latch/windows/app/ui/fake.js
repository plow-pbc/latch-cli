// Fake mode: latch's answers from the Mac app's snapshot fixtures
// (gui/Sources/PlowLatchUI/App/Snapshots.swift, `Fixtures`; copied by
// ../fixtures.py), routed as its `FakeLatchClient` routes them. Nothing runs
// and nothing changes: a save or a removal answers as if it worked.

const fixtures = await (await fetch(new URL("fake/fixtures.json", import.meta.url))).json();
const one = (name) => JSON.parse(fixtures[name]);
const each = (name) => fixtures[name].split("\n").filter(Boolean).map((line) => JSON.parse(line));

/** The host a command names with `--host`, if any. */
const on = (args) => {
  const at = args.indexOf("--host");
  return at < 0 ? null : args[at + 1];
};

/** This Mac's fixture ("studio"), or pi-kitchen's for another host. */
const per = (args, studio, kitchen) => (on(args) && on(args) !== "studio" ? kitchen : studio);

const answers = [
  [["status", "--local"], () => one("serving")],
  [["status", "--host"], () => one("piStatus")],
  [["status"], () => one("studioHosts")],
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

const failed = (code, message) => ({ error: { code, message } });

export async function run(args) {
  for (const [prefix, answer] of answers) {
    if (prefix.every((word, i) => args[i] === word || (word === "--host" && args.includes("--host")))) {
      return answer(args);
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

/** A stream's lines, then nothing more until it's stopped, as the Mac's
 * fake `statusFollow` and `auditFollow` do. */
export function stream(args, line) {
  queueMicrotask(() => {
    if (args[0] === "status") line({ type: "status", status: one("serving") });
    if (args[0] === "audit") for (const value of each(per(args, "studioAudit", "kitchenAudit"))) line(value);
  });
}
