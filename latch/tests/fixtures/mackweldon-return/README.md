# An agent's Mack Weldon return, as it ran

`run.json` is one live run of an agent returning an order through Mack
Weldon's returns portal (a Redo portal at `/apps/redo/returns-portal/`),
on the Studio host's Chrome for Testing, read from that host's own audit
and browser journal: every call in order, with when it started, the wait
before it, how long it took, its outcome, each plan step's op, ref and
target (role and name), the gatekeeper's verdict and timings, the risk
model's latency, and the eval scripts the agent sent.

Left out or replaced:
- the order id in paths is a fake (`0123456789abcdef01234567`);
- typed text was never recorded, only its length;
- gate prompts and the owner's messages;
- the screenshots each step took, which show the owner's name, address and
  order: they are kept on the Studio, not here, under the names
  `screenshot` gives.

`tests/browser-site/return-portal.html` rebuilds the portal's pages from
this run, the live portal's item page (read view-only) and its public
bundle: the same sequence of views, and its checkboxes and radios drawn as
the portal draws them (the selection card takes the click; the input inside
it has no name and sits 0x0 and transparent in a span that takes no
clicks). Names, items and the order are fakes.

Replayed once on those pages (both engines, before this replay was cut to
the batched flow the browser tests keep): as the agent drove it, a call per
step with an observe after nearly each and scripts for the controls, 79
calls in 64 s (Chrome for Testing) and 61 s (Camoufox) of host time;
batched, each page's steps in one `do`, 25 calls in 48 s and 45 s. On the
live run each call also cost the agent about 2.9 s of its own turn.
