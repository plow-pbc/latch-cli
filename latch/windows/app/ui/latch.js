// How the window reaches latch: `latch.exe`, through the app (src/main.rs),
// or in fake mode the Mac app's snapshot fixtures (fake.js), so a still of
// this window and one of the Mac's show the same made-up hosts.

const tauri = window.__TAURI__;
let fake = null;

/** Picks where answers come from: fixtures outside the app, or in a debug
 * build started with `LATCH_APP_FAKE`; latch otherwise. */
export async function connect() {
  if (!tauri || (await tauri.core.invoke("fake_mode"))) fake = await import("./fake.js");
  return fake != null;
}

/** One latch command's answer, or its failure (thrown). */
export function latch(args, input) {
  return fake ? fake.run(args, input) : tauri.core.invoke("run", { args, input });
}

/** Every JSON line one command printed, as `audit`'s pages have many. */
export function latchLines(args) {
  return fake ? fake.lines(args) : tauri.core.invoke("run", { args, lines: true });
}

/** What one command printed, verbatim (`--pretty`'s indented JSON). */
export function latchText(args) {
  if (!fake) return tauri.core.invoke("run", { args, text: true });
  return fake.run(args.filter((arg) => arg !== "--pretty")).then((value) => JSON.stringify(value, null, 2));
}

/** A command that writes a file, its `{file}` argument a temp path: that
 * file as a data URL (`kind` its extension: png, vcf…). */
export function latchFile(args, kind) {
  return fake ? fake.file(args, kind) : tauri.core.invoke("run_file", { args, kind });
}

/** Opens a web link in the default browser. */
export function openURL(url) {
  return fake ? Promise.resolve(window.open(url, "_blank")) : tauri.core.invoke("open_url", { url });
}

/** The native Open dialog: the path picked, or null if cancelled. */
export function pickFile(title, name, extensions) {
  return fake ? fake.pick(title, name, extensions) : tauri.core.invoke("pick_file", { title, name, extensions });
}

const followers = new Map();
if (tauri) {
  tauri.event.listen("latch-line", ({ payload }) => followers.get(payload.id)?.line(payload.line));
  tauri.event.listen("latch-end", ({ payload }) => followers.get(payload.id)?.end(payload.failure));
}

/** Follows a streaming command: `line` for each JSON line, `end` once. */
export function follow(id, args, line, end) {
  followers.set(id, { line, end });
  if (fake) return fake.stream(args, (value) => followers.get(id)?.line(value));
  return tauri.core.invoke("stream", { id, args }).catch((failure) => end(failure));
}

export function unfollow(id) {
  followers.delete(id);
  if (!fake) tauri.core.invoke("stop", { id });
}

/** What a latch failure says, for people. */
export function said(failure) {
  const error = failure?.error ?? failure;
  return error?.message ?? String(failure);
}
