// Fake mode's answers for Rules: `rules try` from the Mac's snapshot
// fixtures (App/Snapshots.swift, `Fixtures.tries`, built there in code),
// picked by what was tried; and, for a still (`?still=NAME`, named after
// the Mac's), what latch said when the Mac drew it:
//   16-rules-*                            rules as the 16 stills read them (scout's own on studio, helper refused exec on pi-kitchen), sidecar offline
//   23-error-rules-pi-kitchen-unreachable pi-kitchen's rules can't be read: it's out of reach
//   23-error-rules-unknown-code           studio's rules can't be read: a code the app has no sentence for
//   rules-stale                           a save over rules changed elsewhere meanwhile
//   rules-too-long                        a save latch refuses as too long
//   rules-try-usage                       a try latch can't run as typed
// The 16 stills also stage what the person had done, as the Mac's do
// (`trial.stage`, `rulesDrafts[key] = …`, `openRules`): the request and
// who it's tried as (Try runs it), a draft, the agent's rules asked for.

const fixtures = await (await fetch(new URL("fixtures.json", import.meta.url))).json();
const one = (name) => JSON.parse(fixtures[name]);
const still = new URLSearchParams(location.search).get("still") ?? "";

/** The host a command names with `--host`; null for this PC. */
const on = (args) => {
  const at = args.indexOf("--host");
  return at < 0 ? null : args[at + 1];
};
const kitchen = (args) => on(args) != null && on(args) !== "studio";
const failed = (code, message) => ({ error: { code, message } });

const HELPER = "3971582e59baad83250141c8dcc95855";
const SCOUT = "ba3c0b3ad1eba7a9a1eb04b67c9db0dd";

/** Rules' own state (pages/rules.js `state`) with a still's staged in it. */
function staged({ request = "", caller = "new", drafts = {}, focus = null }) {
  return {
    drafts, errors: {}, saving: new Set(), focus,
    trial: { request, caller, draft: "", useDraft: false, trying: false, results: {} },
  };
}
const studioRules = JSON.parse(fixtures.rulesStudio).every_agent;
const STAGED = {
  "16-rules-studio-scout-try-allowed": () => staged({
    request: "exec --workdir ~/Projects/latch-demo -- make test", caller: SCOUT, focus: { host: "studio", agent: SCOUT } }),
  "16-rules-studio-try-you-said-no": () => staged({ request: "exec -- cat Finance/2026.csv", caller: SCOUT }),
  "16-rules-studio-unsaved-draft": () => staged({
    drafts: { "studio\n": { text: `${studioRules.text}Never send email without asking me.`, base: studioRules.revision } } }),
  "16-rules-pi-kitchen-helper-exec-refused": () => staged({
    request: "exec -- ls", caller: HELPER, focus: { host: "pi-kitchen", agent: HELPER } }),
};
if (STAGED[still]) (await import("../main.js")).app.pages.rules = STAGED[still]();

const TRIES = {
  "allow-owner": {"as":"owner","gate":{"caller":"owner_device","cost":1.1e-05,"latency_ms":268,"model":"typesafe/jev-1.14-20261014","model_request":"req_nzfeeqvhlqnslxsf","p":{"COVERED":0.9,"FORBIDDEN":0.08,"NOT_COVERED":0.02},"rule":"refused when forbidden >= within instructions","rules":{"agent":null,"host":"ea4ac6c6bd9184d7479f8312f4ebca18db1804f21bba03212ac8f0dc80f35c7c"},"source":"owner_request","verdict":"allow"},"host":"studio","request":{"action":{"argv":["ls","Projects"],"network":false,"operation":"exec","secret_env":[],"working_folder":"/Users/me","writes":[]},"op":"exec"},"verdict":"allow"},
  "allow-scout": {"as":{"agent_name":"scout","agent_uid":"ba3c0b3ad1eba7a9a1eb04b67c9db0dd"},"gate":{"caller":"agent","cost":1e-05,"latency_ms":230,"model":"typesafe/jev-1.14-20261014","model_request":"req_xcxovwucwqgsvcis","p":{"COVERED":0.56,"FORBIDDEN":0.01,"NOT_COVERED":0.43},"rule":"allowed when within instructions >= 0.50","rules":{"agent":"a33189e9c51bcecdd839cfc226d392a7ce6c00eb700b08ce25fa325e2c29f780","host":"ea4ac6c6bd9184d7479f8312f4ebca18db1804f21bba03212ac8f0dc80f35c7c"},"source":"rules","verdict":"allow"},"host":"studio","request":{"action":{"argv":["make","test"],"network":false,"operation":"exec","secret_env":[],"working_folder":"/Users/me/Projects/latch-demo","writes":[]},"op":"exec"},"verdict":"allow"},
  "not-covered": {"as":{"agent_name":"scout","agent_uid":"ba3c0b3ad1eba7a9a1eb04b67c9db0dd"},"gate":{"caller":"agent","cost":1e-05,"latency_ms":251,"model":"typesafe/jev-1.14-20261014","model_request":"req_ikrytewoewrfscag","p":{"COVERED":0.0,"FORBIDDEN":0.0,"NOT_COVERED":1.0},"rule":"allowed when within instructions >= 0.50","rules":{"agent":"a33189e9c51bcecdd839cfc226d392a7ce6c00eb700b08ce25fa325e2c29f780","host":"ea4ac6c6bd9184d7479f8312f4ebca18db1804f21bba03212ac8f0dc80f35c7c"},"source":"rules","verdict":"not_covered"},"host":"studio","request":{"action":{"argv":["curl","--version"],"network":false,"operation":"exec","secret_env":[],"working_folder":"/Users/me","writes":[]},"op":"exec"},"verdict":"not_covered"},
  "said-no": {"as":{"agent_name":"scout","agent_uid":"ba3c0b3ad1eba7a9a1eb04b67c9db0dd"},"gate":{"caller":"agent","cost":1e-05,"latency_ms":213,"model":"typesafe/jev-1.14-20261014","model_request":"req_dvpscpwflpqagaja","p":{"COVERED":0.0,"FORBIDDEN":0.99,"NOT_COVERED":0.01},"rule":"allowed when within instructions >= 0.50","rules":{"agent":"a33189e9c51bcecdd839cfc226d392a7ce6c00eb700b08ce25fa325e2c29f780","host":"ea4ac6c6bd9184d7479f8312f4ebca18db1804f21bba03212ac8f0dc80f35c7c"},"source":"rules","verdict":"owner_said_no"},"host":"studio","request":{"action":{"argv":["cat","Finance/2026.csv"],"network":false,"operation":"exec","secret_env":[],"working_folder":"/Users/me","writes":[]},"op":"exec"},"verdict":"owner_said_no"},
  "defaults": {"as":"new_agent","gate":{"caller":"new_agent","cost":1.1e-05,"decided_by":"rules","latency_ms":197,"layers":{"rules":{"cost":1.1e-05,"model":"typesafe/jev-1.14-20261014","model_request":"req_acdevaoxexjochck","p":{"COVERED":0.05,"FORBIDDEN":0.03,"NOT_COVERED":0.92},"prompt":"7f3a7dc2c0d23b04","says":"neither"}},"model":"typesafe/jev-1.14-20261014","p":{"COVERED":0.05,"FORBIDDEN":0.03,"NOT_COVERED":0.92},"rule":"refused when a layer forbids; else allowed when a layer is within instructions >= 0.50","rules":null,"source":"defaults","verdict":"not_covered"},"host":"pi-kitchen","request":{"action":{"argv":["rm","recipes/old.txt"],"network":false,"operation":"exec","secret_env":[],"working_folder":"/Users/me","writes":[]},"op":"exec"},"verdict":"not_covered"},
  "host-refused": {"as":{"agent_name":"helper","agent_uid":"3971582e59baad83250141c8dcc95855"},"gate":null,"host":"pi-kitchen","refused":{"code":"exec_denied","message":"You turned exec off for this agent on this host when you started it with `latch serve --deny-exec=3971582e59baad83250141c8dcc95855`. Start it again without that flag to turn exec back on."},"request":{"action":{"argv":["ls"],"network":false,"operation":"exec","secret_env":[],"working_folder":"/Users/me","writes":[]},"op":"exec"},"verdict":"host_refused"},
  "new-agent": {"as":"new_agent","gate":{"caller":"new_agent","cost":1e-05,"latency_ms":212,"model":"typesafe/jev-1.14-20261014","model_request":"req_nnlzesajgklnyqbg","p":{"COVERED":0.58,"FORBIDDEN":0.01,"NOT_COVERED":0.41},"rule":"allowed when within instructions >= 0.50","rules":{"agent":null,"host":"ea4ac6c6bd9184d7479f8312f4ebca18db1804f21bba03212ac8f0dc80f35c7c"},"source":"rules","verdict":"allow"},"host":"studio","request":{"action":{"argv":["ls","Projects"],"network":false,"operation":"exec","secret_env":[],"working_folder":"/Users/me","writes":[]},"op":"exec"},"verdict":"allow"},
  "draft": {"as":{"agent_name":"helper","agent_uid":"3971582e59baad83250141c8dcc95855"},"gate":{"caller":"agent","cost":1e-05,"latency_ms":242,"model":"typesafe/jev-1.14-20261014","model_request":"req_hplzksgpinmlheze","p":{"COVERED":0.21,"FORBIDDEN":0.0,"NOT_COVERED":0.79},"rule":"allowed when within instructions >= 0.50","rules":{"agent":null,"host":"7d8f456802c7a988d09fc874e41d23543e9b8e2d810ab5c7d110757f193a90ff"},"source":"rules","verdict":"not_covered"},"host":"studio","request":{"action":{"argv":["touch","Projects/x"],"network":false,"operation":"exec","secret_env":[],"working_folder":"/Users/me","writes":[]},"op":"exec"},"verdict":"not_covered"},
};

/** The verdict a try gets: the stills' own requests their own, the rest
 * as the Mac's fake latch answers every try (not covered). */
function tried(args) {
  const words = args.slice(args.indexOf("--") + 1).join(" ");
  const agent = args.includes("--agent") ? args[args.indexOf("--agent") + 1] : null;
  if (kitchen(args) && agent === HELPER && words.startsWith("exec")) return TRIES["host-refused"];
  if (words.includes("make test")) return TRIES["allow-scout"];
  if (words.includes("Finance/")) return TRIES["said-no"];
  if (args.includes("--draft")) return TRIES.draft;
  if (args.includes("--new-agent")) return kitchen(args) ? TRIES.defaults : TRIES["new-agent"];
  if (!agent) return TRIES["allow-owner"];
  return TRIES["not-covered"];
}

/** `latch status`, sidecar offline, as the 16 stills' account. */
function sidecarOffline() {
  return JSON.parse(fixtures.studioHosts.replace(
    '"self":false,"online":true,"last_seen_at":"2026-10-22T22:06:33.384336Z"', '"self":false,"online":false,"last_seen_at":null'));
}

const rulesShow = {
  "23-error-rules-pi-kitchen-unreachable": (args) => {
    if (kitchen(args)) throw failed("host_unreachable", 'could not connect to host "pi-kitchen"; nothing was sent');
    return one("studioRules");
  },
  "23-error-rules-unknown-code": (args) => {
    if (!kitchen(args)) throw failed("state_busy", "another latch is changing /Users/me/.latch; try again");
    return one("piRules");
  },
};

const saving = {
  "rules-stale": () => { throw failed("stale_revision", "the rules changed since revision was read; read them again with `latch rules show`"); },
  "rules-too-long": () => { throw failed("rules_too_long", "rules for every agent and this agent's own are 9214 bytes together; the limit is 8192"); },
};

export const answers = [
  [["rules", "try"], (args) => {
    if (still === "rules-try-usage") throw failed("usage", "unexpected argument '--frobnicate' found");
    return tried(args);
  }],
  ...(still.startsWith("16-rules-") ? [
    [["rules", "show"], (args) => one(kitchen(args) ? "rulesKitchen" : "rulesStudio")],
    [["status"], (args) => (args[1] === "--local" ? one("serving") : args[1] === "--host" ? one("piStatus") : sidecarOffline())],
  ] : []),
  ...(rulesShow[still] ? [[["rules", "show"], rulesShow[still]]] : []),
  ...(saving[still] ? [[["rules", "set"], saving[still]], [["rules", "clear"], saving[still]]] : []),
];
