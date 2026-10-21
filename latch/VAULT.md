# The vault

One vault per latch install, on the host. Agents never see a value: no
command returns one.

## Items

Four kinds, as the Latch app had: **login** (username, password, an
optional authenticator key), **card**, **identity** and **note**. Values
are kept in Bitwarden's item shape, so field names and custom fields carry
over. Each item has a title, its sites (a login needs one; a card or
identity may list the only sites it fills on) and a revision: an edit or
removal against an older revision is refused.

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
the rows you pick; nothing is picked by default. The export stays on the
host only while the command runs: if you quit, the connection drops, or
you pick nothing within 15 minutes, it is dropped and nothing is saved.

## From your phone: a link (prototype)

`latch vault link` opens a one-time request on the host and prints a link
(and a QR code on a terminal) that adds the item from a phone, away from
your machines. The value goes from the phone straight to the host, never
through Plow and never to an agent.

```sh
latch vault link --host my-mac --kind login --title Shop --site shop.example \
  --field username=ada@shop.example --page http://localhost:8731/
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

## Storage

`vault.json` in the host's state directory, written only by `latch serve`,
atomically. Items are sealed with AES-256-GCM under a random data key; the
data key is sealed under the custody key in `vault.key` (0600) beside it.
`exec` and file workers can read neither (the state directory is denied to
them).
