// Google's accounts as latch reports them, and why a sign-in ended without
// one (the Mac's `GoogleAccounts` and `GoogleSignIn`): for setup's Access
// step and the Connections page alike.

import { code } from "./errors.js";

/** From `setup --status`'s Google step, which lists every account, those
 * needing a sign-in again among them; null when Plow didn't say. */
export const accountsOf = (step) =>
  (Array.isArray(step?.accounts) ? { accounts: step.accounts, needsSignIn: step.needs_sign_in ?? [] } : null);

/** Whether `account` waits for its owner to sign in again. */
export const needs = (google, account) => google.needsSignIn.includes(account);
/** The accounts that work. */
export const working = (google) => google.accounts.filter((account) => !needs(google, account));
/** One account works, so a sign-in adds another (`--add-account`);
 * otherwise it connects the first, or signs in again with them all. */
export const addsAccount = (google) => working(google).length > 0;
/** None of them works: each one's row offers to sign in again. */
export const noneWorks = (google) => google.accounts.length > 0 && !addsAccount(google);

/** Under an account that needs a sign-in while another works. */
export const RECONNECT = "Reconnect it on Plow's Connections page.";
export const TOOLS_MISSING = "Latch's Google tools are missing from this PC. Install Latch again, then try again.";

/** Why a wait for a sign-in ended with no new account: decided on latch's
 * code, never shown (`GoogleSignIn.problem`). */
export function signInProblem(error, adding) {
  switch (code(error)) {
    case "sign_in_timeout":
      return adding
        ? "No new Google account showed up in time. An account that's already connected isn't added again."
        : "No Google account showed up in time. If you finished signing in, try again.";
    case "plow_unreachable": return "Plow can't be reached right now. Check your internet connection, then try again.";
    case "not_logged_in": return "This PC isn't logged in to Plow.";
    case "gog_not_configured": return TOOLS_MISSING;
    default: return "Couldn't connect Google just now. Try again.";
  }
}
