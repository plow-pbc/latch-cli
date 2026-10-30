// Building the window's elements, and its icons: line drawings in the
// spirit of Windows' own, where the Mac app uses SF Symbols.

export function el(tag, attrs = {}, ...children) {
  const node = document.createElement(tag);
  for (const [key, value] of Object.entries(attrs)) {
    if (key === "class") node.className = value;
    else if (key.startsWith("on")) node.addEventListener(key.slice(2), value);
    else if (value !== false && value != null) node.setAttribute(key, value === true ? "" : value);
  }
  for (const child of children.flat()) {
    if (child != null && child !== false) node.append(child.nodeType ? child : String(child));
  }
  return node;
}

export function button(label, onclick, { primary = false, disabled = false } = {}) {
  return el("button", { class: primary ? "primary" : "", onclick, disabled }, label);
}

const ICONS = {
  agents: '<circle cx="7" cy="7" r="2.6"/><path d="M2.5 16c.6-2.6 2.4-4 4.5-4s3.9 1.4 4.5 4"/><circle cx="13.6" cy="7.6" r="2.1"/><path d="M13.4 11.9c1.9 0 3.4 1.2 4.1 3.6"/>',
  connections: '<path d="M8.6 11.4l2.8-2.8"/><path d="M7.2 9.6l-1.7 1.7a2.6 2.6 0 0 0 3.7 3.7l1.7-1.7"/><path d="M12.8 10.4l1.7-1.7a2.6 2.6 0 0 0-3.7-3.7L9.1 6.7"/>',
  pc: '<rect x="3" y="4.5" width="14" height="9" rx="1.2"/><path d="M1.5 15.8h17"/>',
  server: '<rect x="3" y="3.5" width="14" height="5.2" rx="1"/><rect x="3" y="11.3" width="14" height="5.2" rx="1"/><path d="M6 6.1h.01M6 13.9h.01"/>',
  chevron: '<path d="M8 5l5 5-5 5"/>',
  warning: '<path d="M10 3.5l7.2 12.6H2.8z"/><path d="M10 8.4v3.6M10 14.1v.01"/>',
  pause: '<circle cx="10" cy="10" r="7"/><path d="M8.4 7.5v5M11.6 7.5v5"/>',
  update: '<circle cx="10" cy="10" r="7"/><path d="M10 6.4v6.2M7.4 10.2l2.6 2.6 2.6-2.6"/>',
  offline: '<path d="M6.2 15h7.4a3 3 0 0 0 .6-5.9 4.6 4.6 0 0 0-8.3-1.2A3.6 3.6 0 0 0 6.2 15z"/><path d="M3 3l14 14"/>',
  search: '<circle cx="8.8" cy="8.8" r="5"/><path d="M12.6 12.6l4 4"/>',
  wifiOff: '<path d="M3 3l14 14M10 15.5h.01M7.4 12.6a3.7 3.7 0 0 1 4.4-.4M4.6 9.8a7.7 7.7 0 0 1 3.4-1.9M12.5 8.1a7.7 7.7 0 0 1 2.9 1.7M2 7a11.6 11.6 0 0 1 3.3-2.2M9.3 4.3A11.6 11.6 0 0 1 18 7"/>',
  inspector: '<rect x="2.5" y="3.5" width="15" height="13" rx="1.6"/><path d="M12.5 3.5v13"/>',
  settings: '<circle cx="10" cy="10" r="2.6"/><path d="M10 2.5v2M10 15.5v2M2.5 10h2M15.5 10h2M4.7 4.7l1.4 1.4M13.9 13.9l1.4 1.4M4.7 15.3l1.4-1.4M13.9 6.1l1.4-1.4"/>',
};

export function icon(name, className = "icon") {
  const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
  svg.setAttribute("viewBox", "0 0 20 20");
  svg.setAttribute("class", className);
  svg.setAttribute("aria-hidden", "true");
  svg.innerHTML = ICONS[name] ?? "";
  return svg;
}

/** A time as a clock shows it: "1:57 AM". */
export function clock(date) {
  return date.toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
}

export function sameDay(a, b) {
  return a.getFullYear() === b.getFullYear() && a.getMonth() === b.getMonth() && a.getDate() === b.getDate();
}
