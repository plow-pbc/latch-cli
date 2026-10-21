// camoufox-js reads its WebGL table through better-sqlite3, a native addon
// whose install script downloads a prebuilt binary nothing checks. This
// stands in for it with Node's own SQLite, which has the same `prepare`,
// `all` and `get`.
import { DatabaseSync } from "node:sqlite";

export default class Database extends DatabaseSync {}
