# The vault

One vault per latch install, on the host, shared by every browser engine
and profile. Agents never see a value: `latch browser login`, `fill` and
`fill-code` put values into pages themselves, and no command returns one.

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

Run from an owner device: a latch client state logged in as a `device`,
never the host's own state (on the host's machine, a separate
`--state-dir`). The host refuses agents with exit 4.

```sh
latch vault add --host my-mac --kind login --title Shop --site shop.example \
  --field username=ada@shop.example                    # asks for the password
latch vault add --host my-mac --kind card --title Visa \
  --field "cardholder name=Ada Lovelace" --field "expiry month=4" --field "expiry year=2031"
latch vault list --host my-mac                         # metadata only
latch vault edit --host my-mac v3 --revision 2 --title "Shop (work)" \
  --field username=ada@work.example --ask password      # asks for the new password
latch vault remove --host my-mac v3 --revision 2
latch vault import --host my-mac export.1pux           # shows rows, imports the ones you pick
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
the rows you pick; nothing is picked by default.

## Where each kind fills

Both the field's frame and the top-level page must be HTTPS with a real
host: `http`, IP addresses, local hosts, a page without an address and
opaque (sandboxed) frames are refused (`origin_mismatch`).

- **login:** both share the item's registrable domain (public-suffix list,
  private suffixes included): `shop.example` fills on `login.shop.example`,
  never on `shop.example.evil.net`.
- **card, identity:** any top-level site, or one of the item's listed
  sites; the field's frame may be another site (a payment processor's).
- **note:** any site, into fields the page does not mark secret.

## Filling

`observe` lists the items whose site rule allows them on the page
(titles and handles, never a value).

`login` and `fill-code` pick the form's fields themselves (or take
`--username`, `--password`, `--field`, `--submit`); named fields still go
out through their form's submit button, and with none latch can tell,
nothing is released. latch picks the button only when exactly one fits: a
button after the field, in its form (or on a page without one), whose name
says only that the step ends (Continue, Next, Sign in, Log in, Continue to
sign in), whatever its type; "Sign in via LinkedIn" or "Sign in · Yahoo"
never counts. Otherwise it answers `ambiguous_target` with the candidates,
and the agent names one with `--submit`. A login whose
page asks for the username alone goes on within the same pause: latch
presses its button, waits up to 15 s for the password field, and fills and
submits there, checking that page's origins again. `fill` takes the
agent's mapping, `--field LABEL=eN`, `--field number=e4,e5,e6,e7` for a
value split across boxes; dates and expiries are reshaped to the field's
own format. A fill that includes a secret field needs `--press eN`, the
button that submits it, and stops with `confirm_needed` (exit 4) before any
value is released: the confirmation names the item's title, the button and
the lines around it, and `latch browser do --confirm C` then fills, presses
and cleans up as one step ([BROWSER.md](BROWSER.md)), pressing only while
the button and the lines around it are as confirmed; otherwise it clears
the values and answers `confirm_stale`.

While a fill runs, the session is paused: every other call answers
`session_paused`. Each value is set in one step after the field's element,
document and origin are checked again; latch then presses submit and waits
until the values have left the page (clearing them itself if the page keeps
them). A fill that stops part-way (a covered button, a refused field) is
cleaned up the same way. If latch cannot confirm the values left the page,
the session stays paused, through any later fill too, until a cleanup
confirms the page holds none. No reading is taken while a fill holds the
session, and an element list marks a field holding a filled value as
`filled`, never showing the value. While a filled value is on the page,
`eval` is refused (`secret_on_page`). The result says
`submitted`, not "logged in".

The audit records the item, kind, fields by label, both origins, whether
agent code (`eval`) ran in the session before, and the outcome: never a
value. An agent script that ran before a fill could read the value it puts
on the page: an accepted risk for now, flagged on every fill.

## Storage

`vault.json` in the host's state directory, written only by `latch serve`,
atomically. Items are sealed with AES-256-GCM under a random data key; the
data key is sealed under the custody key in `vault.key` (0600) beside it.
`exec` and file workers can read neither (the state directory is denied to
them). On macOS the browser engine runs as the owner outside latch's
sandbox, so a compromised engine could read both: an accepted risk, which a
Keychain half of the custody key narrows later.
