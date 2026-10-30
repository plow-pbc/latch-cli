// Fake mode for the Vault page: the answers the Mac's Vault stills are made
// from (Snapshots.swift: `vaultItems`, `vaultShows`, `remoteVaultItems`,
// `remoteVaultShow`, `importPreview`, `importResult`). A window opened on
// the Vault (`?place=vault`) lists what those stills list; opened anywhere
// else, `vault list` answers as fake.js does, so Home's stills keep theirs.
// `&vault=` stages a still's state:
//   empty, read-failed, remove-failed, show-failed (`vault show` refused:
//   the inspector's and Edit's unreadable item), changed-elsewhere (an edit
//   refused as stale), changed-since-preview (an import refused as stale),
//   offline (pi-kitchen offline, as `17-vault-remote-offline`).

const fixtures = await (await fetch(new URL("fixtures.json", import.meta.url))).json();
const one = (name) => JSON.parse(fixtures[name]);
const asked = new URLSearchParams(location.search);
const onVault = asked.get("place") === "vault";
const stage = asked.get("vault");

const on = (args) => {
  const at = args.indexOf("--host");
  return at < 0 ? null : args[at + 1];
};
const remote = (args) => on(args) != null && on(args) !== "studio";
const failed = (code, message) => ({ error: { code, message } });
const busy = () => failed("state_busy", "another latch is changing /Users/me/.latch; try again");

// `vault show`'s values on studio, by item (the Mac's `vaultShows`); each
// item itself is as `vault list` lists it.
const showValues = {
  v1: { username: "ada" },
  v2: { notes: "Gate code is in the binder." },
  v3: { brand: "Visa", "cardholder name": "Ada L", expiry: "2031-04", "expiry month": "04", "expiry year": "2031" },
  v4: { address1: "1 St", city: "London", company: "Co", country: "UK", "date of birth": "1815-12-10", email: "ada@example.com", "first name": "Ada", "last name": "Lovelace", "license number": "L1", "middle name": "K", phone: "555", "postal code": "N1", state: "LDN", title: "Dr", username: "ada2" },
  v6: { username: "ada@example.com" },
};

/** What `vault list` lists on the host `args` name. */
function list(args) {
  if (!onVault) return one(remote(args) ? "piVault" : "studioVault");
  if (stage === "empty" || stage === "offline") return { items: [] };
  if (stage === "read-failed" && !remote(args)) throw busy();
  return one(remote(args) ? "remoteVaultItems" : "vaultItems");
}

/** `vault show`: the stills' values, or, off the Vault's fixtures, the
 * item as listed with its username (the only value those lists name). */
function show(args) {
  const id = args[2];
  if (stage === "show-failed") throw busy();
  if (onVault && remote(args)) return one("remoteVaultShow");
  const item = list(args).items.find((listed) => listed.item === id);
  if (!item) throw failed("no_such_item", `no item ${id}`);
  const values = onVault && showValues[id] ? structuredClone(showValues[id]) : item.username ? { username: item.username } : {};
  return { item, values };
}

/** `vault add`'s answer: the item as the arguments describe it. */
function added(args) {
  const all = (flag) => args.flatMap((word, i) => (args[i - 1] === flag ? [word] : []));
  const kind = all("--kind")[0];
  return { item: { item: "v7", revision: 1, kind, title: all("--title")[0], sites: all("--site"), other_top_sites: all("--other-top-site"), fields: [], totp: false } };
}

const offlineHosts = () => JSON.parse(fixtures.studioHosts.replace(
  '"self":false,"online":true,"last_seen_at":"2026-10-22T22:06:30.037004Z"',
  '"self":false,"online":false,"last_seen_at":null'));

export const answers = [
  [["vault", "list"], list],
  [["vault", "show"], show],
  [["vault", "add"], added],
  [["vault", "edit"], (args) => {
    if (stage === "changed-elsewhere") throw failed("stale_revision", `item ${args[2]} changed since revision ${args[4]}`);
    return { item: show(args).item };
  }],
  [["vault", "remove"], () => {
    if (stage === "remove-failed") throw busy();
    return {};
  }],
  [["vault", "import"], (args) => {
    if (args.includes("--preview")) return one("importPreview");
    if (stage === "changed-since-preview") throw failed("stale_revision", "the vault or the export changed since the preview");
    return one("importResult");
  }],
  // pi-kitchen offline, as Plow lists it; this PC's own status as fake.js's.
  ...(stage === "offline" ? [[["status"], (args) => {
    if (args[1] === "--local") return one("serving");
    if (args[1] === "--host") return one("piStatus");
    return offlineHosts();
  }]] : []),
];
