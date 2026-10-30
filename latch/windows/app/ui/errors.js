// latch's errors in the app's own words, as the Mac app says them
// (gui/Sources/PlowLatchUI/App/ErrorCopy.swift and ErrorText): each code the
// app knows gets a sentence of its own, naming the host where it matters
// ("this PC" for this PC's own); latch's message is only ever shown under
// Details. Every page says its errors through here.
//
// An error is latch's failure (`{ error: { code, message } }`, as latch.js
// throws it), one the app words itself (`unreachable(sentence, details)`),
// or anything else thrown (its message, as the Mac's `LatchError.from`).

import { el, icon } from "./dom.js";

/** Where nothing else says what failed. */
export const FALLBACK = "Latch couldn't do that.";
/** How the sentences name this PC's own host. */
export const THIS_PC = "this PC";

/** How a host's sentences name it: THIS_PC for this PC's own. */
export const spoken = (host) => (host.thisPC ? THIS_PC : host.name);

/** An error the app words itself (the Mac's `LatchError.unreachable`):
 * its sentence, and what latch or the system said, under Details. */
export function unreachable(sentence, details = null) {
  return { unreachable: sentence, details };
}

const failure = (error) => error?.error ?? null;
/** latch's error code; the app decides on this, never on the message. */
export const code = (error) => failure(error)?.code ?? null;
const title = (name) => name[0].toUpperCase() + name.slice(1);

/** The app's sentence for `error`, naming `host` (its name, or THIS_PC)
 * where it matters; null when it has none. `place`: `{ tryRequest:
 * connector }` on Try a request, "devices" removing a device. */
export function sentence(error, { host = null, place = null } = {}) {
  if (error?.unreachable) return error.unreachable;
  if (typeof error === "string") return error || null;
  const failed = failure(error);
  if (!failed) return error instanceof Error ? error.message : null;
  const name = host ?? "the host";
  const Name = host === THIS_PC ? "This PC" : host ?? "The host";
  if (place && typeof place === "object" && "tryRequest" in place) {
    const said = trying(failed, failed.plugin ?? place.tryRequest, host);
    if (said) return said;
  }
  if (place === "devices" && failed.code === "not_found") return "That device isn't in your account any more.";
  switch (failed.code) {
    // The window's own: latch.exe couldn't be run, or said no JSON.
    case "no_binary": return "Latch couldn't start.";
    case "latch_failed": case "io": return "Latch couldn't complete this step.";
    case "unreadable": return "Latch returned an unreadable response.";
    case "host_unreachable": return `Latch couldn't reach ${name}, so nothing changed. Try again.`;
    case "host_offline": return `${Name} is offline, so nothing changed. Start Latch there, then try again.`;
    case "host_stopped": return `Latch stopped on ${name}. Try again once it's running.`;
    case "record_too_large": return `Activity from ${name} stops at a record too large to send here. See the rest on ${name} itself.`;
    case "host_outdated":
      return `${Name}'s Latch can't check an import against its preview, so nothing was imported. Update Latch there, then import again.`;
    case "serve_required": return "Latch on this PC isn't answering. Start or restart it in Host Settings, then try again.";
    case "other_version": return "Another version of Latch is already running on this PC. Stop it, then start this one.";
    case "not_logged_in": return "This PC isn't logged in to Plow. Log in, then try again.";
    case "plow_unreachable": return "Plow can't be reached right now. Check your internet connection, then try again.";
    case "plow_error": return "Plow couldn't answer just now. Try again in a moment.";
    case "plow_denied": return "Plow didn't accept this PC's login for that.";
    case "gatekeeper_unavailable": return "The Gatekeeper couldn't judge this. Try again in a moment.";
    // Rules.
    case "rules_too_long":
      return "These rules are too long to save. The rules for every agent and any one agent's own rules must fit within 8 KB together. Shorten them, then save again.";
    case "bad_rules": return `Latch can't read the rules saved on ${name}. Check Details.`;
    // The Mac's also names messages and contacts, which Windows doesn't have.
    case "not_previewable": return "Only exec, file and google requests can be tried here.";
    // Vault.
    case "bad_item": return "This item wasn't saved because Latch can't use one of its values. Check Details.";
    case "bad_values": return "Latch couldn't read this item's fields. Nothing was saved.";
    case "stdin_too_big": return "This item's fields are too large to save. Use less text, then try again.";
    case "no_such_item": return `That item isn't in the vault on ${name} any more.`;
    case "bad_import":
      return "Latch can't read this file as a passwords export. It reads exports from Apple Passwords, 1Password, Chrome and Bitwarden; Details says what's wrong with this one.";
    case "file_too_large": return "This file is too large to import. Choose a smaller export file.";
    case "local_file": return "Latch couldn't read this file. Choose it again, or pick another.";
    case "bad_pick": return "The selected rows no longer match this export, so nothing was imported. Choose the file again.";
    case "import_expired": return "The import timed out before anything was saved. Start the import again.";
    // Agents, hosts and devices.
    case "bad_name": return "Give the agent a name without control characters, such as tabs or line breaks.";
    case "line_taken": return "That phone line already has an agent. Choose a free line.";
    case "no_such_line": return "That phone line couldn't be found. Choose another line.";
    case "unknown_cloud_kind": return "That cloud agent isn't available. Choose another cloud agent.";
    case "token_file_exists":
      return "A file with that name is already there, and Latch saves a key only to a new file, so no agent was made. Choose a new name.";
    case "token_file_unwritable": return "Latch couldn't create the key file there, so no agent was made. Choose another folder.";
    case "invalid_request": return "Plow didn't accept these details. Check them, then try again.";
    case "no_such_agent": return "That agent isn't in your account any more.";
    case "unknown_host": return `${Name} isn't one of your hosts any more.`;
    case "own_machine": return "This is the PC you're using. Log out in Settings to remove it.";
    // Logging in and starting.
    case "not_configured": return "Latch is missing information it needs to log in.";
    case "nothing_to_start_again": return "Latch has no saved start settings. Try starting Latch again.";
    default: return null;
  }
}

/** Try a request's own sentences: a request it can't try as typed, and
 * the host's refusals in its verdict. */
function trying(failed, connector, host) {
  const name = host ?? "the host";
  const which = connector ? `The ${title(connector)} connector` : "This connector";
  switch (failed.code) {
    case "usage": case "bad_request": return "Latch can't try this request as typed. Details says what to change.";
    case "exec_denied": return `This agent can't run programs on ${name}.`;
    case "not_enabled": return host === THIS_PC ? `${which} is off on this PC. Turn it on in Host Settings.` : `${which} is off on ${name}. Turn it on there.`;
    case "bad_path": return `Latch can't use that path on ${name}. Check Details.`;
    default: return null;
  }
}

/** The sentence, or the plain one where nothing says what failed. */
export function said(error, options) {
  return sentence(error, options) ?? FALLBACK;
}

/** What goes under Details: latch's own words, never the sentence. */
export function details(error) {
  if (error?.unreachable) return error.details ?? null;
  return failure(error)?.message ?? null;
}

/** A sentence's first sentence, for a single line. */
export function first(text) {
  const end = text.indexOf(". ");
  return end < 0 ? text : `${text.slice(0, end)}.`;
}

/** Activity's host errors, which it retries on its own: the fact alone,
 * without a step to take. */
export function fact(error, host) {
  switch (code(error)) {
    case "host_unreachable": return `Latch couldn't reach ${host}.`;
    case "host_offline": return `${host} is offline.`;
    case "host_stopped": return `Latch stopped on ${host}.`;
    default: return null;
  }
}

/** For a single line: Activity's fact, or the sentence's first sentence. */
export function short(error, host) {
  return fact(error, host) ?? first(said(error, { host }));
}

/** An error as the app says it (the Mac's `ErrorText`): its sentence (none
 * under a `titled` row that already says what failed, when the app has
 * none of its own; `said`, the screen's own words, instead), then latch's
 * own words under a collapsed Details. A page's look can name its classes
 * (`className`, `moreClass`, `tone`) and show a chevron on Details. */
export function errorText(error, {
  host = null, titled = false, said: own = null, place = null,
  tone = "", className = "error-text", moreClass = "said", chevron = false,
} = {}) {
  const words = own ?? sentence(error, { host, place }) ?? (titled ? null : FALLBACK);
  const more = details(error);
  return el("div", { class: `${className}${tone ? ` ${tone}` : ""}` },
    words ? el("span", {}, words) : null,
    more ? el("details", { class: "error-details" },
      el("summary", {}, chevron ? icon("chevron", "chevron") : null, "Details"),
      el("div", { class: moreClass }, more)) : null);
}
