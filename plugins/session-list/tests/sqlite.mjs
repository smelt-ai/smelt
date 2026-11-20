// 测试用的库：用 Bun 自带的 SQLite 照宿主 store_* 的语义做一份（write 一个事务，migrate 按
// user_version 一步一个事务，库比步骤新报错）。
import { Database } from "bun:sqlite";

export function open_db(path) {
  const db = new Database(path);
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
