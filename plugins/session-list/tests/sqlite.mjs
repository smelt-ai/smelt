// 测试用的库：用 Bun 自带的 SQLite 照宿主 store_* 的语义做一份（write 一个事务，migrate 按
// user_version 一步一个事务，库比步骤新报错）。
import { constants, Database } from "bun:sqlite";

// 宿主（rusqlite `Connection::open`）默认带 SQLITE_OPEN_URI，`ATTACH 'file:…?mode=ro'` 才认 URI。
// Linux 上 Bun 自带的 SQLite 编译时没开 USE_URI，不显式带上就把 URI 当普通文件名、挂上一个空库。
const OPEN_FLAGS = constants.SQLITE_OPEN_READWRITE | constants.SQLITE_OPEN_CREATE | constants.SQLITE_OPEN_URI;

export function open_db(path) {
  const db = new Database(path, OPEN_FLAGS);
  const bind = (params = []) => params.map((value) => (typeof value === "boolean" ? Number(value) : value));
  const run = (sql, params) => db.prepare(sql).run(...bind(params)).changes;
  return {
    raw: db,
    async query(sql, params) {
      return db
        .prepare(sql)
        .all(...bind(params))
        .map((row) => ({ ...row }));
    },
    async execute(sql, params) {
      return run(sql, params);
    },
    async write(statements) {
      db.exec("BEGIN");
      try {
        const changed = statements.map(({ sql, params }) => Number(run(sql, params)));
        db.exec("COMMIT");
        return changed;
      } catch (error) {
        db.exec("ROLLBACK");
        throw error;
      }
    },
    async migrate(steps) {
      const { user_version } = db.prepare("PRAGMA user_version").get();
      if (user_version > steps.length) throw new Error("库比插件新");
      for (let ix = user_version; ix < steps.length; ix++) {
        db.exec("BEGIN");
        db.exec(steps[ix]);
        db.exec(`PRAGMA user_version = ${ix + 1}`);
        db.exec("COMMIT");
      }
      return null;
    },
  };
}
