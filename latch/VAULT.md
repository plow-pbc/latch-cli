# The vault

One vault per latch install, on the host, shared by every browser engine
and session. `latch browser fill` puts values into pages itself, and no
command returns a password or other secret field's value. A username is no
secret: a page, or the address it goes to next, may show it.

## Items

Four kinds, as the Latch app had: **login** (username, password, an
optional authenticator key), **card**, **identity** and **note**. Values
are kept in Bitwarden's item shape, so field names and custom fields carry
over. Each item has a title, its sites (a login needs one; a card or
identity may list the only sites it fills on) and a revision: an edit or
removal against an older revision is refused. Items are not tied to browser
sessions: any session may use any item, wherever its kind's site rule lets
it fill. A card's number is kept as its digits alone, however it was typed
or pasted (spaces and dashes are dropped), and must be 12 to 19 of them; a
fill types those digits, a number an older latch kept with spaces too.

Which fields are **secret** follows the Latch app's classifier, ported with
its truth table (`tests/fixtures/maskClassification.json`): a login's
password and key, a card's number and code (CVV), an identity's SSN and
passport number, hidden custom fields.

## Commands (owner devices only)

With no `--host` (or naming this peer), manage this state folder's vault
locally, with or without `serve`. Use the existing device login; no second
state folder or login is needed. Agents are refused with exit 4, but for
`list` (below). To manage another host, pass `--host NAME` (or set
`LATCH_HOST`); it must be serving, and a failed remote call never falls
back to the local vault.

**An agent's `vault list --host NAME`** is judged like its other work and
answers each item's id, title and saved sites only, never a username or a
value. The owner's list is never judged.

```sh
latch vault add --kind login --title Shop --site shop.example \
  --field username=owner@shop.example                  # asks for the password
latch vault add --kind card --title Visa \
  --field "cardholder name=the owner" --field "expiry month=4" --field "expiry year=2031"
latch vault list                                      # metadata only
latch vault show v3                                   # and its non-secret values
latch vault edit v3 --revision 2 --title "Shop (work)" \
  --field username=owner@work.example --ask password    # asks for the new password
latch vault remove v3 --revision 2
latch vault import export.1pux                         # imports only rows you pick
latch vault list --host other-mac                     # another host's vault
```

Secrets are typed at a prompt without echo, never taken from the command
line or the environment. `--ask LABEL` asks for another field the same way;
`--totp` asks for a login's authenticator key.

Without a terminal (an app, a script), `add` and `edit` take `--stdin`
instead: one JSON object of field values by label, secrets included, read
from stdin and never echoed, even in an error. Nothing is asked for, and a
label given with `--field` too is refused.

```sh
printf '{"password": "…", "totp": "…"}' | latch vault add --kind login \
  --title Shop --site shop.example --field username=owner@shop.example --stdin
printf '{"password": "…"}' | latch vault edit v3 --revision 2 --stdin
```

`show ITEM` returns the item's metadata, as `list` does (each field by
label, marked secret or not), and the values of the fields that are not
secret. A secret value is never shown.

`edit` changes an item at the revision `list` shows: `--title`, `--site`
and `--other-top-site` (each list given replaces the old one; `--no-sites` and
`--no-other-top-sites` clear them, though a login keeps a site), `--field
LABEL=VALUE` for a non-secret field (an empty value clears it), and `--ask
LABEL` to type a new secret (`--ask totp` for the authenticator key). The
kind never changes. The answer and the audit name the fields that changed
and the new revision, never a value.

`import` reads 1Password's 1PUX, CSV and "Copy item JSON", Apple Passwords'
CSV, Chrome's CSV and Bitwarden's CSV and unencrypted JSON. Logins come
from all of them; a 1PUX's credit cards come along as cards, and a Bitwarden
JSON's cards, identities and notes as themselves (1Password's CSV carries no
card details). It shows every
row (values reduced to whether they are there), marks rows the vault
already holds (a login by its title, username, site and secrets; a card
or note only when every field it keeps is the same, custom fields
included, so a renewed card is new) or that would update a changed
password, and imports only the rows you pick,
writing the vault once; nothing is picked by default. Without a prompt,
`--preview` returns that preview as JSON, imports nothing and, like a
rules preview, is not audited; and `--pick
1,3-5` (or `all`) imports those rows of it. That pick reads the vault
again, so pass the preview's `expect` with `--expect`: if what any row
would do to the vault changed since the preview (an item it would update
was edited on another device, or a row it would add is now in the
vault), the import is refused with `stale_revision` (exit 4), the new
token in its `expect`, and nothing is saved. An edit no row touches
doesn't count. For a remote import,
the uploaded copy stays on the host only while the command runs: if you
quit, the connection drops, or you pick nothing within 15 minutes, it is
dropped and nothing is saved. A local import reads the file in place;
neither path deletes your original export.

## From your phone: a link

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
  shim shows in a sandboxed frame. The shim says which computer it
  reached, by the name that computer gives (it does not check the computer
  is the owner's), says that what is typed goes straight there, encrypted,
  and links to how it works (`how.html`).
- **What the phone may do:** fetch its link's form and save its link's item.
  An owner's link lets it in with its code; Plow is not asked who the phone
  is. An agent's link carries no code (see below). Secret fields are
  typed on the phone; the owner's non-secret `--field`s come filled in. A
  login's username and password, a card's number and code and a note's
  text are always asked for; `--ask LABEL` asks for any other field the
  kind has (`--kind identity --ask ssn --ask "passport number"`). The
  website, title and username come filled in too, and the owner can change
  them before saving; the vault's own rules apply to what is saved, and a
  site named with a scheme other than `https://` is refused. The audit's
  `submit` record names every field the save changed from the request
  (`edited`), never a value.
- **Whoever holds an owner's link's code can save any login under any
  site.** The code is the only thing the host checks for an owner's link,
  so the owner should keep it to themselves.
- **An agent may ask for a login:** `latch vault link --kind login` from an
  agent is judged by the host's gatekeeper like its other work, on the
  site, title and username it names, and recorded in the audit; it sends
  the link to the owner. Any other kind, any secret `--field`, and
  `--page` are refused: an agent's link always opens the page its host
  sets (`$LATCH_PHONE_PAGE` where `latch serve` runs, else
  `https://plow.co/latch/phone`), since that page sees what the owner
  types. `latch vault link-state LINK` then says whether the link is
  `pending`, `saved` (with the new item's id) or `expired`; never a value.
- **An agent's link carries no code, and opens only for the owner signed in
  to Plow.** It names the request (`l=`) instead, and the Plow the page
  signs in to (`p=`) when it is not `https://api.plow.co`. The page asks
  for the owner's Plow phone number, Plow texts a code, and the page sends
  it with its own throwaway key and the request to
  `POST /v1/latch/phone-grants`; Plow keeps no login for the page, only a
  grant: this key is the owner, for this request, for 10 minutes. The host
  asks `GET /v1/latch/phone-grants/KEY` with its own login before it shows
  the form or saves, and refuses a phone without a live grant for that
  request in its own account (`phone_not_signed_in`). So holding the link
  lets no one, the agent included, see the form or save anything. Not
  covered: a lookalike sign-in page that relays the owner's code to Plow
  as they type it.
- **Audit:** `vault_link` records (`page`, `submit`) name the link and the
  item, never a value.
- **Hosting the page:** plow.co serves it at `https://plow.co/latch/phone`,
  the page a link opens by default, built with
  `BASE=/latch/phone/ phone/build.sh`. Its policy lets it load only its own
  files and connect only to itself, Plow (to sign the owner in) and the
  iroh relays. Whoever serves it
  could change what it does; the page is this open source code, so a change
  can be seen. `just phone-serve` serves it on this machine instead, for a
  browser here or on the network. The policy names the one Plow the page
  signs in to; a build for a local or dev Plow names that one instead:
  `PLOW_API=http://127.0.0.1:18904 phone/build.sh`.


Both the field's frame and the top-level page must be HTTPS with a real
host: `http`, IP addresses, local hosts, a page without an address and
opaque (sandboxed) frames are refused (`origin_mismatch`). While a filled
secret may be on a page, the browser stops every GET request whose address
carries it as a query parameter (as its field formats it too, "123 456"),
as a form sent by GET would; the press, or
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
agent's mapping; with no `--item`, the login saved for the page's site
(several there: refused `which_item`, listing them). The item is found
before the fill is judged, so the judge sees its kind and sites; an id the
vault lacks is refused `no_such_item`. The mapping is
`--field FIELD=eN` for each item field (`username`,
`password`, `otp` for a login's current authenticator code, computed on
the host; a card's `number`, `expiry`, `cvv`, …; an identity's or a note's
fields), `--field number=e4,e5,e6,e7` for a value split across boxes; or
`--suggested S`, a mapping latch suggested. Dates and expiries are
reshaped to the field's own format. A mapping that includes a login's
secret (a password, a code) needs `--press eN`, the button that submits
it (given, or in the suggestion). A card or an identity takes none
(`--press` is refused, `bad_arguments`): it stays in the page's fields
until the agent presses Pay or Submit itself, an ordinary judged click
(below). latch never maps or finds a
button on its own, and picks an item only as above: the one login saved
for the page's site.

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
identities by theirs; the press button is the form's single submit button
(a card's or an identity's suggestion has none: the agent presses the
page's button itself).
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
refused, a covered one naming what covers it. A value goes in with no
click, so only a layer that keeps a person from the field covers it:
another control, a frame or a dialog that does not hold the field; its
own floating label or a page's fixed banner does not. A press button is
covered by anything on top of it. A weak mismatch (an email into a box marked `tel-national`) fills,
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

While a fill runs, its session is paused: every other call in it answers
`session_paused`. Each value goes in after the field's element,
document and origin are checked again, as a person types it: latch
focuses a text field and types the value with the engine's own key
presses (`isTrusted`), checking before each key that the field still has
the focus and its caret has not gone back; only the head of a value
longer than 64 characters is set in one step. A field that takes no keys,
or only the first ones, gets the value in one step, as does any other
field (a list, a date) and a value with a line break or a tab. A field that
formats what it gets (a code as "123 456") still holds the value: only
spaces, dashes, dots, slashes, parentheses and plus signs may come between
its characters, none lost, added or moved. A value sites format as they
like (a card's number and expiry) is held whatever of those separators
either the field or the stored value puts between its characters; every
other value, a login's username and password above all, never is. A field
that holds anything else ends the fill with `needs_owner`, naming the
field, what the page did with it and what went in before it. latch then presses
submit and waits until the values have left the page (clearing them itself
if the page keeps them; the fill's answer then has a `next`. Kept fields
don't prove the submit failed. For a login it says to check the page,
else retry once on the other browser engine, as a site may drop a sign-in
from one engine and take it from the other; for any other kind, a card's
above all, never to submit again but to check whether it went through and
ask the owner). A fill that stops part-way (a covered button, a
refused field) is cleaned up the same way. If latch cannot confirm the
values left the page, the session stays paused, through
any later fill too, until a cleanup confirms the page holds none or the
session that filled is closed. Closing it never lets its pages send what
they hold: on Chrome for Testing the guard stands until the browser exits;
on Camoufox the browser is killed. No reading is taken while a fill holds the
browser, and an element list marks a field holding a filled value as
`filled`, never showing the value. latch protects a secret until the
form is submitted or the fields are cleared; what a site shows afterwards
is the site's. A page that shows a card's number or an SSN back (a
confirmation that repeats it, a preview as it is typed) shows it to the
agent as it shows it to a person, in answers and in plans' journals. A
script (`eval`) is refused
while a filled value may be in a field, as for a password (below), and
runs once the page was submitted and its fields are empty: from then on
it could read a number the page repeats. The gatekeeper alone is shown a
filled card's number only as its last four digits and an identity's
secret not at all, wherever a page put it.

A card fill presses nothing: the card stays in the page's fields, and the
agent checks the page and presses Pay itself, an ordinary click with its
own risk decision, then reads the page that follows to see what happened.
An identity fill presses nothing either: its values stay the same way,
for the agent's own press of the page's button, and all below holds for
it too.
While the card is in the fields, a screenshot shows exactly the fields
latch filled blank (hidden whole for the capture, and the picture dropped
if the page undid that) (in every frame of the page, a payment provider's
included), putting their own look back once it is taken; plans' pictures
too. latch empties fields still holding the card 5 minutes after the fill
(the fill's answer says so in its `next`), and the values go with the
browser when the session closes. Only the filled fields are blanked: a page
that prints the whole number elsewhere shows it in a picture, and a receipt
the agent downloads (text or PDF) can hold it, as can a text answer.
While a filled value may be on a page, in whichever of the session's tabs,
`eval` is refused (`secret_on_page`) until it is gone or its tab is
closed. The result says
`submitted`, not "logged in" or "paid", with each field's outcome, the
number of stages and its `events`: a sign-in the site refused at either
stage (`http_error`, a 429) shows there, though the page may say only
"something went wrong". Only its status comes back, and only for the
page's own origin or one a value went to: while a value is on the page, a
dialog's message, a new tab's address, another site's name or a
response's `Retry-After` may be what the page or the site made of it.
A login fill that pressed a button also says what the page shows after
the submit (`shows`), for telling a sign-in that bounced from one that
moved on: its path, whether a password field (else a username field) is
showing and empty, and the errors it shows (alerts, and the error
messages of fields it marks invalid), for example `after the submit, the
page (/ax/claim) shows the password field, empty, with no error`. With no
such field found while a frame could not be read, it says the page could
not be read rather than that it shows none. A page none of whose frames it could read, short of such a field,
is said to be unreadable rather than to show none. It
describes, never decides: `submitted` stays true.

The audit records the item, its revision, the mapping by ref, where it
came from (a suggestion's id, or the agent), the stages, both origins, any
`hint_mismatch`, and the outcome: never a value. A script the agent runs
with `eval` could read what is typed or filled on its page, then or later
through what it leaves behind (a listener, a service worker, a stored
script), so the gatekeeper judges each script's whole source: one that
reads form fields, hooks input, keyboard or form events, or sends page
data off the page needs the owner's own OK. A login and a card are filled
under the same protections either way.

## Storage

`vault.json` in the host's state directory is written atomically by serve
and local management. Both take the same exclusive `vault.lock` for each
read/write transaction. Each transaction reopens the current store while
holding the lock, so local edits are visible to fills without a restart.
The lock is released while an import waits for picks.
Each item also carries a random `uid`, the same on every host, and when it
last changed; a removed item leaves a tombstone (its uid and when, no
title or value), so a removal can reach other hosts. None of it shows in
`vault list` or `vault show`. A vault from before (version 1) opens as it
was and is written back in the new layout (version 2) on its next change.
Items are sealed with AES-256-GCM under a random data key; the
data key is sealed under the custody key. A signed macOS latch keeps that
key in the Keychain, with the host's other secrets (its key and Plow token);
any other build keeps it in `vault.key` (0600) beside the vault. `exec` and
file workers can read neither (the state directory is denied to them). On
macOS the browser engine runs as the owner outside latch's sandbox, so a
compromised engine could read `vault.json`, and `vault.key` in a build that
keeps files: a known limit (see [SECURITY.md](../SECURITY.md)). In a signed latch the custody key is in the
Keychain, which only a program signed with latch's keychain group can read.

## Sync between the owner's Macs

A Mac whose latch keeps its vault key in the Keychain keeps one vault with
the owner's other such Macs: an add, edit, removal, import or phone-link
save on one reaches the others. Its `serve` runs a round on start, within
seconds of any change to `vault.json`, and every 10 minutes, so a Mac that
was asleep or stopped catches up as it comes back, and a change made while
no other Mac was online waits until one is. In a round it asks each of the
owner's other hosts Plow lists online for its vault state over iroh,
applies it, then sends its own back. Nothing goes through Plow but the
connection's relay, and Plow stores no item.

The later change to an item wins, and a removal as late as the edit wins;
an item each Mac had added on its own, every field equal, becomes one. An
item new to a Mac gets that Mac's next id. A host whose key is a file (any
other system, or an unsigned build) neither sends nor takes items: asked,
it answers `sync_off` before anything is sent. Each change a vault takes
is audited as `vault` `sync` with counts only, the caller and
`caller_name` naming the host it came from; a round that changes nothing
is not audited.
