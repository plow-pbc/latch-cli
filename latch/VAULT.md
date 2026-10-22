# The vault

One vault per latch install, on the host, shared by every browser engine
and profile. `latch browser fill` puts values into pages itself, and no
command returns a password or other secret field's value. A username is no
secret: a page, or the address it goes to next, may show it.

## Items

Four kinds, as the Latch app had: **login** (username, password, an
optional authenticator key), **card**, **identity** and **note**. Values
are kept in Bitwarden's item shape, so field names and custom fields carry
over. Each item has a title, its sites (a login needs one; a card or
identity may list the only sites it fills on) and a revision: an edit or
removal against an older revision is refused. Items are not tied to browser
profiles: any session may use any item, wherever its kind's site rule lets
it fill.

Which fields are **secret** follows the Latch app's classifier, ported with
its truth table (`tests/fixtures/maskClassification.json`): a login's
password and key, a card's number and code (CVV), an identity's SSN and
passport number, hidden custom fields.

## Commands (owner devices only)

With no `--host` (or naming this peer), manage this state folder's vault
locally, with or without `serve`. Use the existing device login; no second
state folder or login is needed. Agents are refused with exit 4. To manage
another host, pass `--host NAME` (or set `LATCH_HOST`); it must be serving,
and a failed remote call never falls back to the local vault.

```sh
latch vault add --kind login --title Shop --site shop.example \
  --field username=owner@shop.example                  # asks for the password
latch vault add --kind card --title Visa \
  --field "cardholder name=the owner" --field "expiry month=4" --field "expiry year=2031"
latch vault list                                      # metadata only
latch vault edit v3 --revision 2 --title "Shop (work)" \
  --field username=owner@work.example --ask password    # asks for the new password
latch vault remove v3 --revision 2
latch vault import export.1pux                         # imports only rows you pick
latch vault list --host other-mac                     # another host's vault
```

Secrets are typed at a prompt without echo, never taken from the command
line or the environment. `--ask LABEL` asks for another field the same way;
`--totp` asks for a login's authenticator key.

`edit` changes an item at the revision `list` shows: `--title`, `--site`
and `--other-top-site` (each list given replaces the old one; `--no-sites` and
`--no-other-top-sites` clear them, though a login keeps a site), `--field
LABEL=VALUE` for a non-secret field (an empty value clears it), and `--ask
LABEL` to type a new secret (`--ask totp` for the authenticator key). The
kind never changes. The answer and the audit name the fields that changed
and the new revision, never a value.

`import` reads 1Password's 1PUX, CSV and "Copy item JSON", Apple Passwords'
CSV, Chrome's CSV and Bitwarden's CSV and unencrypted JSON. It shows every
row (values reduced to whether they are there), marks rows the vault
already holds or that would update a changed password, and imports only
the rows you pick; nothing is picked by default. For a remote import,
the uploaded copy stays on the host only while the command runs: if you
quit, the connection drops, or you pick nothing within 15 minutes, it is
dropped and nothing is saved. A local import reads the file in place;
neither path deletes your original export.

## From your phone: a link (prototype)

`latch vault link` needs a running `serve`, including on this machine: the
phone needs its live receiver. Without one, the command says to start
`latch serve`. A private owner-only local socket creates the request in that
receiver; the CLI can exit and the phone link stays live. It prints a link
(and a QR code on a terminal) that adds the item from a phone, away from
your machines. The value goes from the phone straight to the host, never
through Plow and never to an agent.

```sh
latch vault link --host my-mac --kind login --title Shop --site shop.example \
  --field username=owner@shop.example --page http://localhost:8731/
```

- **The link:** `PAGE#h=HOST_KEY&r=RELAY&c=CODE`. Everything after `#` stays
  in the phone's browser; the server that serves the page never sees it.
  The code is 128 random bits; the host keeps only its sha256, in memory
  (a restart drops open links). It works once, for 15 minutes, and only
  adds a new item.
- **The page** (`phone/`, built with `just phone`) is a small shim: iroh
  built for the browser. It dials the host's key through the host's relay
  (a browser cannot connect directly), on its own protocol name,
  `plow/latch-phone/1`, with a throwaway key. The handshake proves it
  reached the key in the link. The host sends the form itself, which the
  shim shows in a sandboxed frame.
- **What the phone may do:** fetch its link's form and save its link's item.
  The code lets it in; Plow is not asked who the phone is. Secret fields are
  typed on the phone; the owner's non-secret `--field`s come filled in. A
  login's username and password, a card's number and code and a note's
  text are always asked for; `--ask LABEL` asks for any other field the
  kind has (`--kind identity --ask ssn --ask "passport number"`).
- **Audit:** `vault_link` records (`page`, `submit`) name the link and the
  item, never a value.
- **Hosting the page:** `just phone-serve` serves it on this machine, for a
  browser here or on the network. A phone away from home needs it on a
  public HTTPS address; whoever serves it could change what it does, so
  that choice is the owner's.


Both the field's frame and the top-level page must be HTTPS with a real
host: `http`, IP addresses, local hosts, a page without an address and
opaque (sandboxed) frames are refused (`origin_mismatch`). While a filled
secret may be on a page, the browser stops every GET request whose address
carries it as a query parameter, as a form sent by GET would; the press, or
the agent's own click, then fails `get_form`, and nothing was sent. A
username is no secret: an email-first sign-in may carry it to the password
page in the address.

- **login:** both share the item's registrable domain (public-suffix list,
  private suffixes included): `shop.example` fills on `login.shop.example`,
  never on `shop.example.evil.net`.
- **card, identity:** any top-level site, or one of the item's listed
  sites; the field's frame may be another site (a payment processor's).
- **note:** any site, into fields the page does not mark secret.

## Filling

One command fills every kind: `latch browser fill --item vN` with the
agent's mapping, `--field FIELD=eN` for each item field (`username`,
`password`, `otp` for a login's current authenticator code, computed on
the host; a card's `number`, `expiry`, `cvv`, …; an identity's or a note's
fields), `--field number=e4,e5,e6,e7` for a value split across boxes; or
`--suggested S`, a mapping latch suggested. Dates and expiries are
reshaped to the field's own format. A mapping that includes a secret (a
password, a code, a card or identity secret) needs `--press eN`, the button
that submits it (given, or in the suggestion). latch never maps, picks an
item or finds a button on its own.

`observe` shows what a fill could use, never a value: `fill.items`, the
items whose site rule allows them on the page, each with the fields it
holds (present, secret, derived) and no username; `fill.page_offers`, what
the page asks for from its standard hints only (`password`,
`username_only`, `code`, `passkey`, `card`, `address`);
`fill.suggestions`, latch's mappings, each with an id, the map, the press
button, the evidence for each element and each target frame's site; and
`fill.alternatives`, the forms' other buttons and links, unclassified.
Suggestions come only from `autocomplete` tokens, input types and forms:
a login's password field and the single text or email field before it in
its form, a lone field marked `username`, cards by their `cc-*` tokens,
identities by theirs; the press button is the form's single submit button.
None is made on a registration form (`new-password`, or two password
fields), for a code field (`one-time-code` can't tell an authenticator
code from a texted one), or where the map would be ambiguous. `--suggested
S` is recomputed at fill time: a page or item that changed since is
refused `stale_snapshot`, with the fresh suggestions.

Before any value leaves the vault, each mapped element must take its
field (`bad_target`, exit 4, naming the field and why, otherwise): a
password only into a password field or one marked `current-password`,
never `new-password` or a code field; a username never into a password or
search field; a code never into the sign-in's password field; a card's
number and code only into a text, tel or number input whose token, if any,
is theirs; anything else unless the markup contradicts it. Hidden,
`aria-hidden`, inert, disabled, read-only and covered elements are
refused. A weak mismatch (an email into a box marked `tel-national`) fills,
flagged `hint_mismatch` in the result and the audit. These checks reject
what the page's markup contradicts; they can't prove what a page means,
so the mapping supplies the purpose and the site rule bounds who gets the
value.

A login whose mapping is its username alone and a button (an identifier
stage) goes on within the same pause: latch waits up to 15 s for the next
stage, reads it itself, and fills the password there only when exactly
one field takes it, on a form that is no registration, with that form's
single submit button, and the origins pass again. Anything else (a code, a
passkey, a method chooser, two submit buttons) ends the fill after cleanup
with `needs_fields` and the new stage's page, fields and offers; a second
stage on another site is refused `origin_mismatch`.

A fill that releases a card or identity secret stops with
`confirm_needed` (exit 4) before any value is released: the confirmation
names the item, the exact mapping by the page's names, the site of each
frame a value goes into and the press button, and `latch browser do
--confirm C` then fills, presses and cleans up as one step
([BROWSER.md](BROWSER.md)), once, and only while that mapping, those frames,
the item and the reviewed lines are as confirmed; otherwise it clears the
values and answers `confirm_stale`.

While a fill runs, every session on its profile is paused: every other
call answers `session_paused`. Each value is set in one step after the
field's element, document and origin are checked again; latch then presses
submit and waits until the values have left the page (clearing them itself
if the page keeps them). A fill that stops part-way (a covered button, a
refused field) is cleaned up the same way. If latch cannot confirm the
values left the page, every session on the profile stays paused, through
any later fill too, until a cleanup confirms the page holds none or the
session that filled is closed. Closing it never lets its pages send what
they hold: on Chrome for Testing the guard stands until the browser exits;
on Camoufox the browser is killed, ending every session on it. No reading is taken while a fill holds the
browser, and an element list marks a field holding a filled value as
`filled`, never showing the value. While a filled value may be on a page,
in whichever of the session's tabs, `eval` and screenshots are refused
(`secret_on_page`) until it is gone or its tab is closed. The result says
`submitted`, not "logged in" or "paid", with each field's outcome and the
number of stages.

The audit records the item, its revision, the mapping by ref, where it
came from (a suggestion's id, or the agent), the stages, both origins, any
`hint_mismatch`, whether
agent code (`eval`) ever ran on those sites in the profile, in any session
(`agent_code_before_fill`), and the outcome: never a value. An agent script
that ran on the site before a fill could read the value it puts on the
page, through what it left behind (a service worker, stored script): an
accepted risk for now, flagged on every such fill. The sites are kept with
the profile (`agent-code.json`), so the flag outlives the session and the
browser.

## Storage

`vault.json` in the host's state directory is written atomically by serve
and local management. Both take the same exclusive `vault.lock` for each
read/write transaction. Serve reloads its cached store when the file's
identity or modification changes, so local edits are visible to fills
without a restart. The lock is released while an import waits for picks.
Items are sealed with AES-256-GCM under a random data key; the
data key is sealed under the custody key in `vault.key` (0600) beside it.
`exec` and file workers can read neither (the state directory is denied to
them). On macOS the browser engine runs as the owner outside latch's
sandbox, so a compromised engine could read both: an accepted risk, which a
Keychain half of the custody key narrows later.
