// Fake mode for Activity: the answers the Mac's `FakeLatchClient` gives that
// fake.js doesn't (a gatekeeper request), and the stills the Mac's
// Snapshots stage (App/Snapshots.swift), named in the address with
// `still=`: reading, unreachable, read-failure, stopped, agent,
// pi-kitchen-offline, browser, browser-gone. Outside a still, the page
// follows fake.js's stream as it would latch's.

const fixtures = await (await fetch(new URL("fixtures.json", import.meta.url))).json();
const one = (name) => JSON.parse(fixtures[name]);
const each = (name) => fixtures[name].split("\n").filter(Boolean).map((line) => JSON.parse(line));

const still = new URLSearchParams(location.search).get("still");

export const answers = [
  // `audit judge ID`: what the gatekeeper was sent, as the Mac's fake has it.
  [["audit", "judge"], (args) => ({ id: args[2], layer: "rules", request: { model: "typesafe/jev-1.14" }, answer: null })],
];

// pi-kitchen counted offline by Plow (`laptopHostsPiOffline`).
if (still === "pi-kitchen-offline") {
  answers.push([["status"], (args) => {
    if (args.includes("--local")) return one("serving");
    if (args.includes("--host")) return one("piStatus");
    return one("laptopHostsPiOffline");
  }]);
}

const failure = (code, message) => ({ code, message });
const unreachable = failure("host_unreachable", 'could not connect to host "pi-kitchen"; nothing was sent');
const stopped = failure("host_stopped", 'host "pi-kitchen" stopped serving');
const busy = failure("state_busy", "another latch is changing /Users/me/.latch; try again");

/** What a still shows of `host`'s feed instead of following it, as the
 * Mac's `ActivityFeed.stage` leaves it: its lines and error. Undefined:
 * follow. */
export function feed(host) {
  const audit = host === "studio" ? each("studioAudit") : host === "pi-kitchen" ? each("kitchenAudit") : [];
  switch (still) {
    case "reading": return host === "studio" ? { lines: [] } : undefined;
    case "unreachable": return host === "studio" ? { lines: [], error: unreachable } : undefined;
    case "read-failure": return host === "studio" ? { lines: [], error: busy } : undefined;
    case "stopped": return { lines: audit, error: host === "pi-kitchen" ? stopped : null };
    // A browser plan and a standalone screenshot, after the rest.
    case "browser": case "browser-gone": return { lines: host === "studio" ? [...audit, ...each("browserAudit")] : audit };
    default: return undefined;
  }
}

/** The agent a still narrows Activity to: scout, on studio. */
export const agent = still === "agent" ? { host: "studio", agent: "a74f38668570a45ddc4b762ba0465d74" } : null;

/** `audit shot`, when the host no longer keeps a plan's screenshots. */
export const shot = still === "browser-gone"
  ? async (args) => { throw { error: failure("no_such_shot", `record ${args[2]}'s screenshot ${args[3]} is no longer kept`) }; }
  : null;
