// Fake mode for Activity: the answers the Mac's `FakeLatchClient` gives that
// fake.js doesn't (a gatekeeper request), and the stills the Mac's
// Snapshots stage (App/Snapshots.swift), named in the address with
// `still=`: reading, unreachable, read-failure, stopped, agent,
// pi-kitchen-offline, browser, browser-gone; and two of the feed moving,
// which the Mac's stills can't show: live (a request arrives a moment
// after the page opens) and window (the stream opens on the newest few
// records, the rest two at a time a page back). Outside a still, the page follows
// fake.js's stream as it would latch's.

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

const failure = (code, message) => ({ error: { code, message } });
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
  ? async (args) => { throw failure("no_such_shot", `record ${args[2]}'s screenshot ${args[3]} is no longer kept`); }
  : null;

/** A request scout makes on studio a moment after the page opens: today's,
 * refused, so Home's counts and Recent change as it arrives. */
function arriving() {
  return {
    type: "record",
    cursor: "f".repeat(64),
    record: {
      id: `11ve${"0".repeat(60)}`, time: new Date().toISOString(),
      agent: "a74f38668570a45ddc4b762ba0465d74", caller: "9f4d06180a5fa0824b2bc4fc4f056b0395e90914cedf7709621e6559047263f2",
      caller_name: "scout", op: "exec", decision: "deny", outcome: "gatekeeper_denied", command: "latch exec cat Taxes/2026.pdf",
      details: {
        args: { argv: ["cat", "Taxes/2026.pdf"], network: false, secret_env: [], workdir: "/Users/me", writes: [] },
        gate: { verdict: "owner_said_no", source: "rules", p: { COVERED: 0.02, FORBIDDEN: 0.97, NOT_COVERED: 0.01 } },
        started: null,
      },
    },
  };
}

/** How many records studio's stream opens on in the `window` still, and
 * how many each earlier page has: Home reaches back two pages for its
 * five requests and offers Load Earlier for the rest. */
const WINDOW = 3;
const PAGE = 2;

/** How the feed reaches latch in a still that moves (merged over latch.js's
 * `follow` and `latchLines`); null to use those. */
export function io(host) {
  if (host !== "studio") return null;
  const audit = each("studioAudit");
  if (still === "live") {
    return {
      follow(id, args, line) {
        queueMicrotask(() => audit.forEach(line));
        setTimeout(() => line(arriving()), 1200);
      },
    };
  }
  if (still === "window") {
    const kept = audit.filter((line) => line.type === "record");
    const newest = kept.slice(-WINDOW);
    const last = newest.at(-1).cursor;
    return {
      follow(id, args, line) {
        queueMicrotask(() => [
          { type: "hello", schema: 1 },
          { type: "snapshot", cursor: kept.at(-WINDOW - 1).cursor, has_older: true },
          ...newest,
          { type: "caught_up", cursor: last, has_older: true },
        ].forEach(line));
      },
      // `audit --before ID`: the page of records before it.
      async lines(args) {
        const before = args[args.indexOf("--before") + 1];
        const at = Math.max(kept.findIndex((line) => line.record.id === before), 0);
        const from = Math.max(at - PAGE, 0);
        return [{ type: "snapshot", cursor: "0".repeat(64), has_older: from > 0 }, ...kept.slice(from, at),
          { type: "caught_up", cursor: kept[Math.max(at - 1, 0)].cursor, has_older: from > 0 }];
      },
    };
  }
  return null;
}
