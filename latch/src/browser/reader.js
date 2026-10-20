// latch's page reader. It runs in an isolated world of each frame (the
// page's own scripts cannot see it or its globals), defines `__latch` once
// per document, and answers with plain JSON values.
//
// A snapshot keeps the elements it listed, by snapshot id, so a later call
// can find "element 7 of snapshot o3" again in the same document. A
// document that is replaced takes its snapshots with it.
(() => {
  if (globalThis.__latch) return;
  const MAX_TEXT = 100;
  const KEEP = 16;
  const snaps = new Map();

  const INTERACTIVE = [
    'a[href]', 'button', 'input:not([type=hidden])', 'select', 'textarea', 'summary',
    '[contenteditable=""]', '[contenteditable=true]', '[tabindex]:not([tabindex="-1"])', '[onclick]',
    '[role=button]', '[role=link]', '[role=checkbox]', '[role=radio]', '[role=tab]', '[role=menuitem]',
    '[role=option]', '[role=switch]', '[role=combobox]', '[role=textbox]', '[role=searchbox]',
    '[role=slider]', '[role=spinbutton]', '[role=treeitem]',
  ].join(',');
  const ROWS = 'tr, li, [role=row], [role=listitem], [role=gridcell], article, dd';
  const SECRET = /pass|secret|token|otp|one-time|cvv|cvc|\bpin\b/i;

  const clean = (text, max = MAX_TEXT) => {
    const flat = (text || '').replace(/\s+/g, ' ').trim();
    return flat.length > max ? flat.slice(0, max - 1) + '…' : flat;
  };

  const inputRole = (el) => {
    const type = (el.getAttribute('type') || 'text').toLowerCase();
    return {
      checkbox: 'checkbox', radio: 'radio', search: 'searchbox', range: 'slider', number: 'spinbutton',
      button: 'button', submit: 'button', reset: 'button', image: 'button', file: 'button',
    }[type] || 'textbox';
  };

  const role = (el) => {
    const explicit = el.getAttribute('role');
    if (explicit) return explicit.split(' ')[0];
    switch (el.localName) {
      case 'a': return 'link';
      case 'button': case 'summary': return 'button';
      case 'select': return el.multiple ? 'listbox' : 'combobox';
      case 'textarea': return 'textbox';
      case 'input': return inputRole(el);
    }
    return el.isContentEditable ? 'textbox' : 'generic';
  };

  // A field whose value latch never reads back: passwords, and fields
  // marked or named as secrets.
  const secret = (el) => {
    if (el.localName !== 'input' && el.localName !== 'textarea') return false;
    if ((el.getAttribute('type') || '').toLowerCase() === 'password') return true;
    const marks = [el.getAttribute('autocomplete'), el.name, el.id].join(' ');
    return SECRET.test(marks);
  };

  // A label or row's text without the options of a dropdown inside it,
  // which are its choices, not its name.
  const ownText = (el) => {
    if (!el.querySelector('select, textarea')) return el.innerText || el.textContent;
    const copy = el.cloneNode(true);
    copy.querySelectorAll('select, textarea').forEach((control) => control.remove());
    return copy.textContent;
  };

  const byIds = (ids) => ids.split(/\s+/)
    .map((id) => document.getElementById(id))
    .filter(Boolean)
    .map((el) => el.innerText || el.textContent)
    .join(' ');

  const name = (el) => {
    const labelledby = el.getAttribute('aria-labelledby');
    if (labelledby) { const text = clean(byIds(labelledby)); if (text) return text; }
    const label = el.getAttribute('aria-label');
    if (label && label.trim()) return clean(label);
    if (el.labels && el.labels.length) {
      const text = clean([...el.labels].map(ownText).join(' '));
      if (text) return text;
    }
    if (el.localName === 'input') {
      const type = (el.getAttribute('type') || '').toLowerCase();
      if (['button', 'submit', 'reset'].includes(type) && el.value) return clean(el.value);
      if (type === 'image' && el.alt) return clean(el.alt);
    }
    if (!['input', 'select', 'textarea'].includes(el.localName)) {
      const text = clean(el.innerText || el.textContent);
      if (text) return text;
      const img = el.querySelector('img[alt]');
      if (img && img.alt.trim()) return clean(img.alt);
    }
    return clean(el.getAttribute('placeholder') || el.getAttribute('title') || '');
  };

  const value = (el) => {
    if (secret(el)) return undefined;
    if (el.localName === 'select') {
      return clean([...el.selectedOptions].map((o) => o.label).join(', '));
    }
    if (el.localName === 'textarea') return clean(el.value, 200);
    if (el.localName === 'input' && inputRole(el) !== 'button' && !['checkbox', 'radio'].includes(el.type)) {
      return clean(el.value, 200);
    }
    if (el.isContentEditable) return clean(el.innerText, 200);
    return undefined;
  };

  const rendered = (el) => (el.checkVisibility ? el.checkVisibility({ visibilityProperty: true }) : el.getClientRects().length > 0);

  const fileInput = (el) => el.localName === 'input' && el.type === 'file';

  const offscreen = (rect) => rect.bottom < 0 || rect.right < 0 || rect.top > innerHeight || rect.left > innerWidth;

  const describe = (el) => {
    const rect = el.getBoundingClientRect();
    const item = { role: role(el), name: name(el) };
    // A field holding a value latch filled from the vault never shows it.
    if (filled.has(el)) item.filled = true;
    const v = filled.has(el) ? undefined : value(el);
    if (v !== undefined && v !== '') item.value = v;
    if (secret(el)) item.secret = true;
    if (el.localName === 'input' && ['checkbox', 'radio'].includes(el.type)) item.checked = el.checked;
    else if (el.getAttribute('aria-checked')) item.checked = el.getAttribute('aria-checked') === 'true';
    if (el.getAttribute('aria-expanded')) item.expanded = el.getAttribute('aria-expanded') === 'true';
    if (el.disabled || el.getAttribute('aria-disabled') === 'true') item.disabled = true;
    if (offscreen(rect)) item.offscreen = true;
    if (!rendered(el)) item.hidden = true;
    const row = el.parentElement && el.parentElement.closest(ROWS);
    if (row) {
      const text = clean(ownText(row), 160);
      if (text && text !== item.name) item.row = text;
    }
    return item;
  };

  const read = (snap, limit, scopeSnap, scopeIndex) => {
    let root = document;
    if (scopeSnap) {
      const scoped = (snaps.get(scopeSnap) || [])[scopeIndex];
      if (!scoped || !scoped.isConnected) return { stale: true };
      root = scoped;
    }
    // A file input is often hidden behind a styled label; it is listed
    // anyway, since `upload` needs no click on it.
    const all = [...root.querySelectorAll(INTERACTIVE)].filter((el) => rendered(el) || fileInput(el));
    if (root !== document && root.matches && root.matches(INTERACTIVE)) all.unshift(root);
    const kept = all.slice(0, limit);
    snaps.set(snap, kept);
    while (snaps.size > KEEP) snaps.delete(snaps.keys().next().value);
    return {
      origin: self.origin === 'null' ? 'opaque' : self.origin,
      url: location.href,
      elements: kept.map(describe),
      more: all.length > kept.length,
    };
  };

  // Brings element `index` of `snap` into view and reports where to click
  // it, in this frame's viewport, and whether a click there would reach it:
  // its centre, or for a link that wraps onto several lines (whose box's
  // centre may fall between them) the middle of the first line that does.
  const locate = (snap, index) => {
    const el = (snaps.get(snap) || [])[index];
    if (!el || !el.isConnected) return { gone: true };
    if (!rendered(el)) return { hidden: true };
    el.scrollIntoView({ block: 'center', inline: 'center', behavior: 'instant' });
    const rect = el.getBoundingClientRect();
    const reachesAt = (px, py) => {
      const hit = document.elementFromPoint(px, py);
      return !!hit && (hit === el || el.contains(hit) || (el.labels && [...el.labels].some((l) => l.contains(hit))));
    };
    let x = rect.left + rect.width / 2;
    let y = rect.top + rect.height / 2;
    let reaches = reachesAt(x, y);
    for (const line of reaches ? [] : el.getClientRects()) {
      const [lx, ly] = [line.left + line.width / 2, line.top + line.height / 2];
      if (line.width > 0 && line.height > 0 && reachesAt(lx, ly)) {
        [x, y, reaches] = [lx, ly, true];
        break;
      }
    }
    return {
      x, y, width: rect.width, height: rect.height,
      covered: !reaches,
      disabled: !!(el.disabled || el.getAttribute('aria-disabled') === 'true'),
      secret: secret(el),
      kind: el.localName === 'select' ? 'select'
        : (el.localName === 'input' && el.type === 'file') ? 'file'
        : (role(el) === 'textbox' || role(el) === 'searchbox' || role(el) === 'spinbutton') ? 'text' : 'other',
      options: el.localName === 'select' ? [...el.options].map((o) => o.label) : undefined,
    };
  };

  const element = (snap, index) => {
    const el = (snaps.get(snap) || [])[index];
    return el && el.isConnected ? el : null;
  };

  // --- Fills -------------------------------------------------------------
  // A fill is prepared on one document and refused on any other: `token`
  // names this one. `filled` holds the elements a vault value went into,
  // so latch can tell whether one is still on the page, and clear them.
  const token = `${Date.now().toString(36)}-${Math.random().toString(36).slice(2)}`;
  const filled = new Set();

  const textLike = (el) => el.localName === 'textarea' || el.isContentEditable
    || (el.localName === 'input' && inputRole(el) !== 'button'
      && !['checkbox', 'radio', 'file', 'hidden', 'range', 'color'].includes(el.type));
  const formOf = (el) => el.form || el.closest('form') || el.ownerDocument;
  const USERNAME = /user|e-?mail|login|account|identifier/i;
  const CODE = /otp|one.?time|totp|2fa|mfa|verif|passcode|security.?code|auth.?code|\bcode\b/i;
  const marks = (el) => [el.name, el.id, el.getAttribute('autocomplete'), el.getAttribute('placeholder'),
    el.getAttribute('aria-label'), el.labels && [...el.labels].map((l) => l.textContent).join(' ')].join(' ');
  const BUTTONS = 'button, input[type=submit], input[type=image], input[type=button], [role=button]';
  // A name that says only "end this step": it starts so, and every word is
  // one of these ("Continue", "Next", "Sign in", "Continue to Sign In"; not
  // "Sign in via LinkedIn", "Sign in · Yahoo", "Forgot password").
  const STEP_START = /^(sign in|log in|login|log on|continue|next|submit|verify|confirm)\b/;
  const STEP_WORDS = new Set(['continue', 'next', 'sign', 'in', 'log', 'login', 'on', 'submit', 'verify',
    'confirm', 'to', 'and', 'your', 'account', 'code', 'now']);
  const plainStep = (text) => {
    const words = text.toLowerCase().replace(/[^a-z]+/g, ' ').trim().split(' ').filter(Boolean);
    return STEP_START.test(words.join(' ')) && words.every((w) => STEP_WORDS.has(w));
  };
  // The buttons that could end `field`'s step (`submits`, as indexes into
  // `els`): the buttons after the field, in its form or (with none) its
  // document, whose names say only that, whatever their type. Only when
  // exactly one fits is it the `submit`; otherwise that is -1.
  const submitOf = (field, els) => {
    const form = field.form || field.closest('form');
    const at = [...(form || field.ownerDocument).querySelectorAll(BUTTONS)]
      .filter((b) => rendered(b) && els.includes(b) && plainStep(name(b))
        && (field.compareDocumentPosition(b) & Node.DOCUMENT_POSITION_FOLLOWING))
      .map((b) => els.indexOf(b));
    return { submit: at.length === 1 ? at[0] : -1, submits: at };
  };

  // Login candidates among snapshot `snap`'s elements, as indexes into it:
  // each password field with the text or email field before it in its form,
  // and the button that submits it (-1 when latch cannot tell one; the
  // buttons that could, in `submits`).
  const loginFields = (snap) => {
    const els = snaps.get(snap) || [];
    const logins = [];
    for (const [i, el] of els.entries()) {
      if (!(el.localName === 'input' && el.type === 'password' && el.isConnected && rendered(el))) continue;
      const form = formOf(el);
      const before = [...form.querySelectorAll('input')]
        .filter((f) => f !== el && textLike(f) && f.type !== 'password' && rendered(f) && els.includes(f))
        .filter((f) => f.compareDocumentPosition(el) & Node.DOCUMENT_POSITION_FOLLOWING);
      const user = before.length ? before[before.length - 1] : null;
      logins.push({ password: i, username: els.indexOf(user), ...submitOf(el, els) });
    }
    // A page with no password field may be the first step of a two-page
    // login: a username or email field, alone.
    const users = els.map((el, i) => [el, i]).filter(([el]) => el.localName === 'input' && textLike(el)
      && rendered(el) && (el.type === 'email' || USERNAME.test(marks(el))))
      .map(([el, i]) => ({ password: -1, username: i, ...submitOf(el, els) }));
    return { logins, users };
  };

  // The buttons that could end the step of element `index`'s field.
  const submitFor = (snap, index) => {
    const els = snaps.get(snap) || [];
    const el = els[index];
    return el ? submitOf(el, els) : { submit: -1, submits: [] };
  };

  // One-time-code fields: marked as such, or a short numeric box.
  const codeFields = (snap) => {
    const els = snaps.get(snap) || [];
    return els.map((el, i) => [el, i]).filter(([el]) => el.localName === 'input' && textLike(el) && rendered(el)
      && el.type !== 'password' && (el.getAttribute('autocomplete') === 'one-time-code' || CODE.test(marks(el))
        || (el.inputMode === 'numeric' && el.maxLength >= 4 && el.maxLength <= 8)))
      .map(([el, i]) => ({ code: i, ...submitOf(el, els) }));
  };

  // Where a value would go, and what the field says about its format.
  const fillTarget = (snap, index) => {
    const el = element(snap, index);
    if (!el) return { gone: true };
    return {
      origin: self.origin === 'null' ? 'opaque' : self.origin,
      token,
      kind: el.localName === 'select' ? 'select' : textLike(el) ? 'text' : 'other',
      max_length: el.maxLength > 0 ? el.maxLength : null,
      placeholder: el.getAttribute('placeholder') || '',
      page_secret: secret(el),
    };
  };

  const setValue = (el, value) => {
    if (el.localName === 'select') {
      const want = String(value).trim().toLowerCase();
      const number = /^\d+$/.test(want) ? Number(want) : null;
      const options = [...el.options];
      const match = options.find((o) => o.value.toLowerCase() === want || o.label.toLowerCase() === want)
        || (number !== null && options.find((o) => /^\d+$/.test(o.value.trim()) && Number(o.value) === number))
        || (number !== null && options.find((o) => /^\d+$/.test(o.label.trim()) && Number(o.label) === number));
      if (!match) return false;
      el.selectedIndex = options.indexOf(match);
    } else if (el.isContentEditable) {
      el.textContent = value;
    } else {
      // The prototype's own setter, so frameworks that watch `value` see it.
      const setter = Object.getOwnPropertyDescriptor(Object.getPrototypeOf(el), 'value').set;
      setter.call(el, value);
    }
    el.dispatchEvent(new Event('input', { bubbles: true }));
    el.dispatchEvent(new Event('change', { bubbles: true }));
    return true;
  };
  const current = (el) => (el.isContentEditable ? el.textContent : el.value);

  // Checks the element, document and origin, then sets the value in one
  // step. Answers whether the field holds it, never the value.
  const fill = (snap, index, value, origin, expected) => {
    const el = element(snap, index);
    if (!el) return { refused: 'the field is gone' };
    if (expected !== token) return { refused: 'the document was replaced' };
    if ((self.origin === 'null' ? 'opaque' : self.origin) !== origin) return { refused: 'the origin changed' };
    if (!setValue(el, value)) return { refused: 'no option matches' };
    if (value) filled.add(el); else filled.delete(el);
    const held = el.localName === 'select' ? true : current(el) === value;
    return { held };
  };

  // How many filled elements still hold something.
  const holding = () => [...filled].filter((el) => el.isConnected && current(el)).length;
  // Empties every filled field. They stay watched: a page that puts a
  // value back still counts as holding it.
  const clearFilled = () => {
    for (const el of [...filled]) {
      if (!el.isConnected) { filled.delete(el); continue; }
      if (el.localName === 'select') { filled.delete(el); continue; }
      setValue(el, '');
    }
    return holding();
  };

  // Non-secret fields as lines, for a judgement of what the page shows.
  const FIELD_ROLES = ['textbox', 'searchbox', 'combobox', 'spinbutton', 'checkbox', 'radio'];
  const fields = (max) => [...document.querySelectorAll(INTERACTIVE)]
    .filter((el) => rendered(el) && FIELD_ROLES.includes(role(el)) && !secret(el) && !filled.has(el))
    .slice(0, max)
    .map((el) => {
      const item = describe(el);
      const state = 'checked' in item ? (item.checked ? 'checked' : 'not checked')
        : (item.value ? JSON.stringify(item.value) : 'empty');
      return `${item.role} "${item.name}": ${state}`;
    });

  // What a consequential action acts on, as a confirmation is bound to it:
  // the element (for Enter, its form's submit control, else the form), an
  // identity that tells it from every other element in this document, and
  // the lines of the section around it: the dialog, section, article or
  // main part of the page it is in (around its form, so the total or the
  // item a checkout form submits is among them), else its form.
  const uids = new WeakMap();
  let nextUid = 0;
  const uid = (el) => {
    if (!uids.has(el)) uids.set(el, ++nextUid);
    return uids.get(el);
  };
  const SUBMITS = 'button:not([type]), button[type=submit], input[type=submit], input[type=image]';
  const review = (snap, index, enter) => {
    const el = element(snap, index);
    if (!el) return null;
    const { role: r, name: n, row } = describe(el);
    let actsOn = null;
    let around = el;
    if (enter) {
      const form = el.form || el.closest('form');
      const submit = form && [...form.querySelectorAll(SUBMITS)].find(rendered);
      actsOn = submit ? { role: role(submit), name: name(submit) }
        : form ? { role: 'form', name: clean(form.getAttribute('aria-label') || form.getAttribute('name') || '') }
          : null;
      around = submit || form || el;
    }
    const region = around.closest('dialog, [role=dialog], section, article, main')
      || around.closest('form') || document.body;
    const lines = (region ? region.innerText : '').split('\n').map((line) => clean(line, 160)).filter(Boolean).slice(0, 20);
    return { uid: uid(el), element: { role: r, name: n, row }, acts_on: actsOn, lines };
  };

  // --- What a plan's decision sees ----------------------------------------
  // The lines of text on screen now, top to bottom: the text of each block
  // (a paragraph, a list item, a cell) that shows in the viewport, one line
  // per block, so "Total: $13.74" stays one line though its parts are
  // separate elements.
  const SKIP_TEXT = new Set(['SCRIPT', 'STYLE', 'NOSCRIPT', 'TEMPLATE', 'OPTION', 'TEXTAREA', 'SELECT']);
  const screenLines = (max) => {
    const blocks = new Map();
    const blockOf = (el) => {
      if (blocks.has(el)) return blocks.get(el);
      const display = getComputedStyle(el).display;
      const found = (display.startsWith('inline') || display === 'contents') && el.parentElement
        ? blockOf(el.parentElement) : el;
      blocks.set(el, found);
      return found;
    };
    const out = [];
    let block = null;
    let line = '';
    const push = () => {
      const text = clean(line, 160);
      if (text && out[out.length - 1] !== text) out.push(text);
      line = '';
    };
    const root = document.body || document.documentElement;
    const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
    for (let node = walker.nextNode(); node && out.length < max; node = walker.nextNode()) {
      const parent = node.parentElement;
      if (!parent || SKIP_TEXT.has(parent.tagName) || !node.textContent.trim() || !rendered(parent)) continue;
      const range = document.createRange();
      range.selectNodeContents(node);
      const rect = range.getBoundingClientRect();
      if ((rect.width === 0 && rect.height === 0) || rect.bottom < 0 || rect.top > innerHeight) continue;
      const at = blockOf(parent);
      if (at !== block) { push(); block = at; }
      line += ' ' + node.textContent;
    }
    push();
    return out.slice(0, max);
  };

  // For each element of snapshot `snap`: what kind of input it takes, a
  // dropdown's options, its form (an index into the document's forms) and
  // whether something else, such as a modal or a banner, covers it so a
  // click could not reach it now. Off-screen elements are never covered:
  // a click scrolls them into view first. A link that wraps onto several
  // lines is tried at the middle of each line, not of the box around them,
  // which may fall between its lines.
  const viewOf = (snap) => (snaps.get(snap) || []).map((el) => {
    if (!el.isConnected) return { gone: true };
    const item = {
      kind: el.localName === 'select' ? 'select' : fileInput(el) ? 'file' : textLike(el) ? 'text' : 'other',
    };
    if (el.localName === 'select') item.options = [...el.options].slice(0, 50).map((o) => clean(o.label, 80));
    const form = el.form || el.closest('form');
    if (form) item.form = [...document.forms].indexOf(form);
    if (rendered(el) && !offscreen(el.getBoundingClientRect())) {
      const reachesAt = (rect) => {
        const x = Math.min(Math.max(rect.left + rect.width / 2, 0), innerWidth - 1);
        const y = Math.min(Math.max(rect.top + rect.height / 2, 0), innerHeight - 1);
        const hit = document.elementFromPoint(x, y);
        return !hit || hit === el || el.contains(hit) || hit.contains(el)
          || (el.labels && [...el.labels].some((l) => l.contains(hit)));
      };
      const lines = [...el.getClientRects()].filter((r) => r.width > 0 && r.height > 0 && !offscreen(r));
      if (!(lines.length ? lines : [el.getBoundingClientRect()]).some(reachesAt)) item.covered = true;
    }
    return item;
  });

  const scroll = () => ({
    y: Math.round(scrollY),
    max: Math.max(0, Math.round(document.documentElement.scrollHeight - innerHeight)),
  });

  // Whether this frame shows a password field: a two-page login's second page.
  const passwordShown = () => [...document.querySelectorAll('input[type=password]')].some(rendered);

  // What an instruction step stops at, or offers to sign in on: a CAPTCHA's
  // frame, and a password field.
  const CAPTCHA = /\/recaptcha\/|hcaptcha\.com|challenges\.cloudflare\.com/;
  const wall = () => ({ password: passwordShown(), captcha: CAPTCHA.test(location.href) });

  // The page a click on `el`, or the key `key` in it, would load, when that
  // is known before it happens: a link's address, or the address its form
  // sends to (a submit button, or Enter in one of the form's fields).
  // Script on the page can still go anywhere.
  const SUBMIT = 'button:not([type]), button[type=submit], input[type=submit], input[type=image]';
  // Where `form` sends what `button` submits: the button's own formaction,
  // else the form's action.
  const sendsTo = (form, button) => (button && button.hasAttribute('formaction') ? button.formAction : form.action);
  const leadsTo = (el, key) => {
    const link = el.closest('a[href], area[href]');
    if (link) return link.href;
    if (!el.form) return null;
    if (el.matches(SUBMIT) && (key == null || key === 'Enter' || key === 'Space')) {
      return sendsTo(el.form, el);
    }
    // Enter in a field submits its form through the form's default button,
    // the first submit button it owns, as a click on that button would.
    if (key === 'Enter' && el.matches('input')) {
      return sendsTo(el.form, [...el.form.elements].find((owned) => owned.matches(SUBMIT)));
    }
    return null;
  };

  globalThis.__latch = {
    fields,
    screenLines,
    viewOf,
    scroll,
    review,
    passwordShown,
    wall,
    read,
    locate,
    elementAt: (snap, index) => element(snap, index),
    isFileInput: (snap, index) => { const el = element(snap, index); return el ? fileInput(el) : null; },
    describeAt: (snap, index) => { const el = element(snap, index); return el ? describe(el) : null; },
    focus: (snap, index) => { const el = element(snap, index); if (!el) return false; el.focus(); return true; },
    leadsTo: (snap, index, key) => { const el = element(snap, index); return el ? leadsTo(el, key) : null; },
    leadsToFocused: (key) => (document.activeElement ? leadsTo(document.activeElement, key) : null),
    selected: (snap, index) => { const el = element(snap, index); return el ? [...el.selectedOptions].map((o) => o.label) : null; },
    text: (max) => {
      const body = document.body ? document.body.innerText : '';
      return { text: body.length > max ? body.slice(0, max) : body, truncated: body.length > max };
    },
    loginFields,
    codeFields,
    submitFor,
    fillTarget,
    fill,
    holding,
    clearFilled,
  };
})();
