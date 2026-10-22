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

  // Hidden from the page's readers: `aria-hidden` or inert, itself or by an
  // ancestor (checkouts carry such copies of their fields).
  const unread = (el) => !!el.closest('[aria-hidden="true"], [inert]');

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
    const all = [...root.querySelectorAll(INTERACTIVE)]
      .filter((el) => (rendered(el) || fileInput(el)) && !unread(el));
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
  const SUBMIT_BUTTONS = 'button:not([type]), button[type=submit], input[type=submit], input[type=image]';
  // Whether something, such as a modal or a banner, covers `el` so a click
  // could not reach it now. Off-screen elements are never covered: a click
  // scrolls them into view first. A link that wraps onto several lines is
  // tried at the middle of each line, not of the box around them.
  const covered = (el) => {
    if (!rendered(el) || offscreen(el.getBoundingClientRect())) return false;
    const reachesAt = (rect) => {
      const x = Math.min(Math.max(rect.left + rect.width / 2, 0), innerWidth - 1);
      const y = Math.min(Math.max(rect.top + rect.height / 2, 0), innerHeight - 1);
      const hit = document.elementFromPoint(x, y);
      return !hit || hit === el || el.contains(hit) || hit.contains(el)
        || (el.labels && [...el.labels].some((l) => l.contains(hit)));
    };
    const lines = [...el.getClientRects()].filter((r) => r.width > 0 && r.height > 0 && !offscreen(r));
    return !(lines.length ? lines : [el.getBoundingClientRect()]).some(reachesAt);
  };

  // What a fill needs to know of one element, from the page's own markup:
  // its tag and input type, its `autocomplete` attribute, its form (an
  // index into the document's forms), whether it is a submit button or a
  // button or link at all, its role and name, and whether a value could go
  // into it now. Never its value, and never a guess from its words.
  const controlOf = (el) => {
    const form = el.form || el.closest('form');
    const r = role(el);
    return {
      // Editable first: a textarea made contenteditable takes its value as
      // text too, which its name and the page's text show.
      tag: el.isContentEditable ? 'editable' : el.localName,
      type: el.localName === 'input' ? (el.getAttribute('type') || 'text').toLowerCase() : '',
      autocomplete: el.getAttribute('autocomplete') || '',
      form: form ? [...document.forms].indexOf(form) : -1,
      submit: !!form && el.matches(SUBMIT_BUTTONS),
      clickable: r === 'button' || r === 'link',
      role: r,
      name: name(el),
      hidden: !rendered(el) || unread(el),
      disabled: !!(el.disabled || el.getAttribute('aria-disabled') === 'true'),
      readonly: !!el.readOnly,
      covered: covered(el),
    };
  };

  // Every element of snapshot `snap`, as a fill sees it.
  const controls = (snap) => ({
    origin: self.origin === 'null' ? 'opaque' : self.origin,
    controls: (snaps.get(snap) || []).map((el) => (el.isConnected ? controlOf(el) : { gone: true })),
  });

  // The page's rendered fields as one mark, which changes when the page
  // moves to another stage of a sign-in, and whether one asks for a
  // password or a code.
  const stageMark = () => {
    const shown = [...document.querySelectorAll('input:not([type=hidden]), textarea, select')]
      .filter((el) => rendered(el) && !unread(el));
    const tokens = (el) => (el.getAttribute('autocomplete') || '').toLowerCase().split(/\s+/);
    return {
      mark: shown.map((el) => [el.localName, (el.getAttribute('type') || '').toLowerCase(),
        el.getAttribute('autocomplete') || '', el.name || el.id || ''].join('/')).join('|'),
      asks: shown.some((el) => (el.getAttribute('type') || '').toLowerCase() === 'password'
        || tokens(el).includes('one-time-code')),
    };
  };

  // Fields whose markup marks them secret, which a note never fills.
  const SECRET_TOKENS = ['current-password', 'new-password', 'one-time-code', 'cc-number', 'cc-csc'];
  const markedSecret = (el) => (el.getAttribute('type') || '').toLowerCase() === 'password'
    || (el.getAttribute('autocomplete') || '').toLowerCase().split(/\s+/).some((t) => SECRET_TOKENS.includes(t));

  // Where a value would go, and what the field says about its format.
  const fillTarget = (snap, index) => {
    const el = element(snap, index);
    if (!el) return { gone: true };
    return {
      ...controlOf(el),
      origin: self.origin === 'null' ? 'opaque' : self.origin,
      token,
      kind: el.localName === 'select' ? 'select' : textLike(el) ? 'text' : 'other',
      max_length: el.maxLength > 0 ? el.maxLength : null,
      placeholder: el.getAttribute('placeholder') || '',
      page_secret: markedSecret(el),
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
    // Checked again here: the page can make a field editable after the
    // fill was checked, e.g. from an earlier field's input handler.
    if (el.isContentEditable) return { refused: 'the field became an editable region' };
    if (!setValue(el, value)) return { refused: 'no option matches' };
    if (value) filled.add(el); else filled.delete(el);
    if (el.localName === 'select' || current(el) === value) return { held: true };
    // A single-line field drops line breaks, as it does from a paste: it
    // holds the value less them, which is what a person would have got.
    const unbroken = value.replace(/[\r\n]/g, '');
    if (el.localName === 'input' && unbroken !== value && current(el) === unbroken) {
      return { held: true, dropped_breaks: true };
    }
    return { held: false };
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
  // What Enter in `el` acts on: its form's submit control, else the form;
  // and the element whose surroundings are read for it.
  const enterOn = (el) => {
    const form = el.form || el.closest('form');
    const submit = form && [...form.querySelectorAll(SUBMITS)].find(rendered);
    const actsOn = submit ? { role: role(submit), name: name(submit) }
      : form ? { role: 'form', name: clean(form.getAttribute('aria-label') || form.getAttribute('name') || '') }
        : null;
    return { actsOn, around: submit || form || el };
  };
  // The part of the page an action is judged in: the dialog, section,
  // article or main part `around` is in, else its form; and its lines.
  const regionOf = (around) => around.closest('dialog, [role=dialog], section, article, main')
    || around.closest('form') || document.body;
  const linesOf = (region) => (region ? region.innerText : '').split('\n').map((line) => clean(line, 160)).filter(Boolean).slice(0, 20);
  const review = (snap, index, enter) => {
    const el = element(snap, index);
    if (!el) return null;
    const { role: r, name: n, row } = describe(el);
    let actsOn = null;
    let around = el;
    if (enter) ({ actsOn, around } = enterOn(el));
    const lines = linesOf(regionOf(around));
    return { uid: uid(el), element: { role: r, name: n, row }, acts_on: actsOn, lines };
  };

  // --- What a plan's decision sees ----------------------------------------
  // What was written into this document since a step began: `mark(step)`
  // starts the count, and every element text was put into after it (added,
  // or set again, even to the same text) is kept until the next mark.
  let marked = null;
  const written = new Set();
  const note = (records) => {
    for (const record of records) {
      if (record.type === 'characterData') {
        if (record.target.parentElement) written.add(record.target.parentElement);
      } else {
        for (const added of record.addedNodes) {
          const el = added.nodeType === 1 ? added : added.parentElement;
          if (el) written.add(el);
        }
      }
    }
  };
  let watcher = null;
  const mark = (step) => {
    if (!watcher) {
      watcher = new MutationObserver(note);
      watcher.observe(document, { subtree: true, childList: true, characterData: true });
    }
    watcher.takeRecords();
    marked = step;
    written.clear();
  };

  // The lines of text on screen now, top to bottom: the text of each block
  // (a paragraph, a list item, a cell) that shows in the viewport, one line
  // per block, so "Total: $13.74" stays one line though its parts are
  // separate elements. With `step`, also whether each line was written
  // since that step began (`fresh`): all of them on a document the step
  // did not begin on; none when `begin` starts the step here.
  const SKIP_TEXT = new Set(['SCRIPT', 'STYLE', 'NOSCRIPT', 'TEMPLATE', 'OPTION', 'TEXTAREA', 'SELECT']);
  const screenView = (max, step, begin) => {
    if (step != null && begin) mark(step);
    if (watcher) note(watcher.takeRecords());
    const other = step != null && marked !== step;
    const blocks = new Map();
    const blockOf = (el) => {
      if (blocks.has(el)) return blocks.get(el);
      const display = getComputedStyle(el).display;
      const found = (display.startsWith('inline') || display === 'contents') && el.parentElement
        ? blockOf(el.parentElement) : el;
      blocks.set(el, found);
      return found;
    };
    const lines = [];
    const fresh = [];
    let block = null;
    let line = '';
    let rewritten = false;
    const push = () => {
      const text = clean(line, 160);
      if (text && lines[lines.length - 1] !== text) {
        lines.push(text);
        fresh.push(other || rewritten);
      } else if (text && rewritten) {
        fresh[fresh.length - 1] = true;
      }
      line = '';
      rewritten = false;
    };
    const root = document.body || document.documentElement;
    const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
    for (let node = walker.nextNode(); node && lines.length < max; node = walker.nextNode()) {
      const parent = node.parentElement;
      if (!parent || SKIP_TEXT.has(parent.tagName) || !node.textContent.trim() || !rendered(parent)) continue;
      const range = document.createRange();
      range.selectNodeContents(node);
      const rect = range.getBoundingClientRect();
      if ((rect.width === 0 && rect.height === 0) || rect.bottom < 0 || rect.top > innerHeight) continue;
      const at = blockOf(parent);
      if (at !== block) { push(); block = at; }
      line += ' ' + node.textContent;
      if (step != null && !other && written.size && [...written].some((el) => el.contains(node))) rewritten = true;
    }
    push();
    return { lines: lines.slice(0, max), fresh: fresh.slice(0, max) };
  };

  // For each element of snapshot `snap`: what kind of input it takes, a
  // dropdown's options, its form (an index into the document's forms) and
  // whether something else, such as a modal or a banner, covers it so a
  // click could not reach it now. Off-screen elements are never covered:
  // a click scrolls them into view first. A link that wraps onto several
  // lines is tried at the middle of each line, not of the box around them,
  // which may fall between its lines.
  const viewOf = (snap) => (snaps.get(snap) || []).map(view);
  const view = (el) => {
    if (!el.isConnected) return { gone: true };
    const item = {
      kind: el.localName === 'select' ? 'select' : fileInput(el) ? 'file' : textLike(el) ? 'text' : 'other',
    };
    if (el.localName === 'select') item.options = [...el.options].slice(0, 50).map((o) => clean(o.label, 80));
    const form = el.form || el.closest('form');
    if (form) item.form = [...document.forms].indexOf(form);
    if (covered(el)) item.covered = true;
    return item;
  };

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
    screenView,
    viewOf,
    scroll,
    review,
    wall,
    read,
    locate,
    elementAt: (snap, index) => element(snap, index),
    isFileInput: (snap, index) => { const el = element(snap, index); return el ? fileInput(el) : null; },
    describeAt: (snap, index) => { const el = element(snap, index); return el ? describe(el) : null; },
    // Whether the element now has the focus: a hidden or disabled one
    // cannot take it, and a key would go to whatever had it before.
    focus: (snap, index) => {
      const el = element(snap, index);
      if (!el) return null;
      el.focus();
      return el.getRootNode().activeElement === el;
    },
    leadsTo: (snap, index, key) => { const el = element(snap, index); return el ? leadsTo(el, key) : null; },
    leadsToFocused: (key) => (document.activeElement ? leadsTo(document.activeElement, key) : null),
    selected: (snap, index) => { const el = element(snap, index); return el ? [...el.selectedOptions].map((o) => o.label) : null; },
    text: (max) => {
      const body = document.body ? document.body.innerText : '';
      return { text: body.length > max ? body.slice(0, max) : body, truncated: body.length > max };
    },
    controls,
    stageMark,
    fillTarget,
    fill,
    holding,
    clearFilled,
  };
})();
