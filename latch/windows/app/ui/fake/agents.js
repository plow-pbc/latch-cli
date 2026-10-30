// Fake mode for Agents: the Mac's stills of it (App/Snapshots.swift), each
// picked by the address, as fake.js's own answers stand for the default
// (scout and helper, studio's and pi-kitchen's standing):
//   ?agents=trio     Aspen, grok-bot and Willow, sidecar offline (15-agents-*)
//   ?agents=hub      5 agents on 8 hosts (15-agents-hub-5x8, App/Synthetic.swift)
//   ?agents=empty    none yet; =pending, still listing; =failed, Plow unreachable
//   ?remove=fail     a removal Plow may or may not have done
//   ?start=fail|life|none   New Agent: Plow unreachable, Life Assistant
//                    picked (20-new-agent-text-community), no kinds offered
// A prefix ending in `undefined` matches only that command with nothing
// after it (`agents`, not `agents catalog`).

const fixtures = await (await fetch(new URL("fixtures.json", import.meta.url))).json();
const one = (name) => JSON.parse(fixtures[name]);
const asked = new URLSearchParams(location.search);
const scene = asked.get("agents");

const failed = (code, message) => ({ error: { code, message } });
const host = (args) => args[args.indexOf("--host") + 1];

// ── 15-agents-*: three agents, sidecar offline ──
const sidecarOffline = () => {
  const account = one("studioHosts");
  for (const entry of account.hosts) if (entry.name === "sidecar") Object.assign(entry, { online: false, last_seen_at: null });
  return account;
};

// ── 15-agents-hub-5x8: the synthetic latch's hub ──
const NAMES = ["studio", "pi-kitchen", "sidecar", "build-server-in-the-garage-closet", "nas", "mini", "travel-air", "lab-linux"];
const AGENTS = ["scout", "helper", "courier", "ledger", "sentinel"];
const hex = (n, salt, width) => "0".repeat(width - 12) + salt.toString(16).padStart(4, "0") + n.toString(16).padStart(8, "0");
const hubAccount = () => ({
  account: { owner_uid: "usr_synthetic", display_name: "Morgan" },
  hosts: NAMES.map((name, index) => ({
    name, key: hex(index, 0x6b55, 64), self: index === 0,
    ...(name === "sidecar" ? { online: false, last_seen_at: null }
      : name === "nas" ? { online: null, last_seen_at: null }
        : { online: true, last_seen_at: `2026-10-23T09:00:0${index}Z` }),
  })),
  default_host: "studio", default_host_reason: null, api_url: "http://127.0.0.1:9",
});
const hubAgents = (withStanding) => ({
  agents: AGENTS.map((name, index) => ({
    cloud_kind: null, created_at: "2026-10-23T05:37:45", failure_code: null, kind: "own", last_seen_at: "2026-10-23T05:37:45",
    line: { name: null, number: `+1555000${String(index + 2).padStart(4, "0")}` }, name,
    standing: withStanding ? { exec_blocked: index % 3 === 2, own_rules: index % 2 === 0 } : null,
    status: null, uid: hex(index, 0xa9e7, 32),
  })),
});

const LISTS = {
  trio: () => one("trio"),
  hub: () => hubAgents(false),
  empty: () => ({ agents: [], host: null }),
  pending: () => new Promise(() => {}),
  failed: () => { throw failed("plow_unreachable", "could not reach Plow at http://127.0.0.1:9: connection refused"); },
};
const STANDINGS = {
  trio: (args) => one(host(args) === "pi-kitchen" ? "trioKitchen" : "trioStudio"),
  hub: () => hubAgents(true),
};
const ACCOUNTS = { trio: sidecarOffline, hub: hubAccount };

const start = () => {
  const text = one("startByText");
  switch (asked.get("start")) {
    case "fail": throw failed("plow_unreachable", "could not reach Plow");
    case "life": return { ...text, default: "exe:life" };
    case "none": return { ...text, default: null, kinds: [] };
    default: return text;
  }
};

export const answers = [
  [["agents", "remove"], () => {
    if (asked.get("remove") === "fail") throw failed("outcome_unknown", "Plow may or may not have removed Willow");
    return {};
  }],
  ...(asked.has("start") ? [[["agents", "start-by-text"], start]] : []),
  ...(LISTS[scene] ? [[["agents", undefined], LISTS[scene]]] : []),
  ...(STANDINGS[scene] ? [[["agents", "--host"], STANDINGS[scene]]] : []),
  ...(ACCOUNTS[scene] ? [[["status", undefined], ACCOUNTS[scene]]] : []),
];
