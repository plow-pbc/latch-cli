// What the audit log's records say, as the Mac app reads them
// (gui/Sources/PlowLatchUI/Latch/Audit.swift, App/ActivityFeed.swift).

/** An `audit` page's records, oldest first, and whether older ones exist. */
export function page(lines) {
  const records = [];
  let hasOlder = false;
  for (const line of lines) {
    if (line.type === "record") records.push(line.record);
    if (line.type === "snapshot" || line.type === "caught_up") hasOlder = line.has_older ?? false;
  }
  return { records, hasOlder };
}

export const time = (record) => new Date(record.time);
const args = (record) => record.details?.args;
export const gate = (record) => args(record)?.gate ?? record.details?.gate;

export const isRefused = (record) => record.decision !== "allow";

/** The owner's own reads: a window's, or a status check. */
function isOwnerRead(record) {
  if (record.agent) return false;
  const command = args(record)?.command;
  switch (record.op) {
    case "status": case "audit": case "audit_shot": case "audit_judge": return true;
    case "rules": return command === "show";
    case "vault": return ["list", "show"].includes(command);
    case "plugins": return command === "list";
    default: return false;
  }
}

/** Not a request anyone made of the host: a connection, or the owner's reads. */
export function isBackground(record) {
  return (record.op === "connect" && record.decision === "allow") || isOwnerRead(record)
    || (record.op === "plugins" && args(record)?.command === "list" && record.outcome === "ok");
}

const words = (text) => text.replaceAll("_", " ");

/** The status in words, and its tone: connected, allowed, not-covered, refused. */
export function status(record) {
  if (record.op === "connect" && record.decision === "allow") return { tone: "connected", title: "Connected" };
  if (record.decision === "allow") {
    return { tone: "allowed", title: record.outcome === "ok" ? "Allowed" : `Allowed · ${words(record.outcome)}` };
  }
  const refused = (why) => ({ tone: "refused", title: `Refused · ${why}` });
  switch (record.outcome) {
    case "exec_denied": return refused("exec is off for this agent");
    case "not_enabled": return refused(`${record.op[0].toUpperCase()}${record.op.slice(1)} is off`);
    case "gatekeeper_unavailable": return refused("judge unavailable");
    case "caller_denied": return refused("caller not allowed");
    case "owner_only": return refused("the owner's only");
    case "gatekeeper_denied":
      return gate(record)?.verdict === "owner_said_no" ? refused("you said no") : { tone: "not-covered", title: "Refused · needed your OK" };
    default: return refused(words(record.outcome));
  }
}

/** Who called, and as what: "scout · agent", "You · on this PC". */
export function caller(record, host) {
  if (record.agent) return { who: record.caller_name ?? `Agent ${record.agent.slice(0, 8)}`, kind: "agent" };
  if (record.caller && record.caller === host.key) return { who: "You", kind: host.thisPC ? "on this PC" : `on ${host.name}` };
  return { who: "You", kind: record.caller_name ?? record.caller?.slice(0, 8) ?? "unknown" };
}
