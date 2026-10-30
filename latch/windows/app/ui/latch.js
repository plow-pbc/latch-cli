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
