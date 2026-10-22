// Greenleaf Market: a fictional grocery shop, the plan suite's fixture.
// State lives in localStorage; reset.html sets it up for each case.
const CATALOG = [
  { id: 'caesar', name: 'Chicken Caesar Salad', cat: 'salads', price: 11.5, sizes: ['Small', 'Regular', 'Large'] },
  { id: 'greek', name: 'Greek Salad', cat: 'salads', price: 9.75, sizes: ['Small', 'Regular', 'Large'] },
  { id: 'kale', name: 'Kale Power Bowl', cat: 'salads', price: 12.25, sizes: ['Regular'], out: true },
  { id: 'cobb', name: 'Cobb Salad', cat: 'salads', price: 12.0, sizes: ['Regular', 'Large'] },
  { id: 'nicoise', name: 'Salade Niçoise', cat: 'salads', price: 13.5, sizes: ['Regular'] },
  { id: 'quinoa', name: 'Quinoa Crunch Salad', cat: 'salads', price: 10.25, sizes: ['Regular'] },
  { id: 'caprese', name: 'Caprese Salad', cat: 'salads', price: 9.25, sizes: ['Regular'] },
  { id: 'oat1', name: 'Oatly Oat Milk 1L', cat: 'drinks', price: 4.29, sizes: [] },
  { id: 'oat2', name: 'Califia Oat Milk 1.4L', cat: 'drinks', price: 5.49, sizes: [] },
  { id: 'oat3', name: 'Greenleaf Oat Milk 1L', cat: 'drinks', price: 2.99, sizes: [] },
  { id: 'oj', name: 'Fresh Orange Juice 1L', cat: 'drinks', price: 4.99, sizes: [] },
  { id: 'cola', name: 'Cane Sugar Cola 4-pack', cat: 'drinks', price: 6.49, sizes: [] },
  { id: 'rice', name: 'Jasmine Rice 2kg', cat: 'pantry', price: 7.99, sizes: [] },
  { id: 'pasta', name: 'Spaghetti 500g', cat: 'pantry', price: 1.99, sizes: [] },
  { id: 'oats', name: 'Rolled Oats 1kg', cat: 'pantry', price: 3.49, sizes: [] },
];
// The past order the return pages show: two identical white tank lines
// among the rest.
const ORDER = { number: 'A1001', placed: 'October 16, 2026', lines: [
  { line: 1, name: 'White tank (M)', price: 18.0 },
  { line: 2, name: 'White tank (M)', price: 18.0 },
  { line: 3, name: 'Oxford shirt (L)', price: 54.0 },
  { line: 4, name: 'Wool socks', price: 12.0 },
  { line: 5, name: 'Canvas belt', price: 29.0 },
] };
const PER_PAGE = 4;
const money = (n) => `$${n.toFixed(2)}`;
const store = {
  get(k, d) { try { return JSON.parse(localStorage.getItem(k)) ?? d; } catch { return d; } },
  set(k, v) { localStorage.setItem(k, JSON.stringify(v)); },
};
const cart = () => store.get('cart', []);
const user = () => store.get('user', null);
const qs = (k) => new URLSearchParams(location.search).get(k);
const byId = (id) => CATALOG.find((p) => p.id === id);

function addToCart(id, size, qty) {
  // The wrong-item fixture: with `swap` set, the Cobb Salad's button adds a
  // Greek Salad, and the banner says so.
  if (id === 'cobb' && store.get('swap', false)) id = 'greek';
  const p = byId(id);
  if (p.out) return { error: `Sorry, ${p.name} is out of stock. It was not added to your cart.` };
  const items = cart();
  const same = items.find((i) => i.id === id && i.size === (size || ''));
  if (same) same.qty += qty; else items.push({ id, size: size || '', qty });
  store.set('cart', items);
  return { ok: `Added to cart: ${p.name}${size ? ` (${size})` : ''} × ${qty}` };
}

function header() {
  const count = cart().reduce((n, i) => n + i.qty, 0);
  const u = user();
  document.body.insertAdjacentHTML('afterbegin', `
    <header>
      <a href="index.html" class="logo">Greenleaf Market</a>
      <form action="search.html" role="search">
        <input type="search" name="q" aria-label="Search products" placeholder="Search products" value="${qs('q') || ''}">
        <button type="submit">Search</button>
      </form>
      <nav>
        <a href="category.html?c=salads">Salads</a><a href="category.html?c=drinks">Drinks</a><a href="category.html?c=pantry">Pantry</a>
        <a href="cart.html">Cart (${count})</a>
        ${u ? `<a href="account.html">Account</a>` : `<a href="login.html">Sign in</a>`}
      </nav>
    </header>`);
}

function productRow(p) {
  return `<li class="product"><a href="product.html?id=${p.id}">${p.name}</a> <span class="price">${money(p.price)}</span>
    ${p.out ? '<span class="out">Out of stock</span>' : ''}
    <button type="button" onclick="quickAdd('${p.id}', this)">Add to cart</button></li>`;
}

function quickAdd(id, btn) {
  const p = byId(id);
  const r = addToCart(id, p.sizes.length ? p.sizes[Math.min(1, p.sizes.length - 1)] : '', 1);
  flash(r);
}

function flash(r) {
  let box = document.getElementById('flash');
  if (!box) { box = document.createElement('p'); box.id = 'flash'; box.setAttribute('role', 'status'); document.querySelector('main').prepend(box); }
  box.className = r.error ? 'err' : 'ok';
  box.textContent = r.error || r.ok;
  const count = cart().reduce((n, i) => n + i.qty, 0);
  const link = [...document.querySelectorAll('nav a')].find((a) => a.textContent.startsWith('Cart'));
  if (link) link.textContent = `Cart (${count})`;
}

function requireUser(next) {
  if (!user()) { location.replace(`login.html?next=${encodeURIComponent(next)}`); return false; }
  return true;
}

function cookieBanner() {
  if (qs('cookies') !== '1' || store.get('cookies', false)) return;
  document.body.insertAdjacentHTML('beforeend', `
    <div class="overlay" id="cookie-overlay"><div class="modal" role="dialog" aria-label="Cookie consent">
      <p>We use cookies to remember your cart and improve our store.</p>
      <button type="button" id="accept-cookies">Accept all cookies</button> <a href="#" onclick="return false">Cookie settings</a>
    </div></div>`);
  document.getElementById('accept-cookies').onclick = () => { store.set('cookies', true); document.getElementById('cookie-overlay').remove(); };
}
