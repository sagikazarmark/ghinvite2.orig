// Test-only observer around the unchanged captured production Worker.
import worker from './worker.mjs';

export default {
  async fetch(request, env, ctx) {
    if (!request.headers.has('x-test-observe-d1')) return worker.fetch(request, env, ctx);
    const statements = [];
    const observe = (statement, sql, values = []) => new Proxy(statement, {
      get(target, property) {
        if (property === 'constructor') return target.constructor;
        if (property === 'bind') return (...args) => observe(target.bind(...args), sql, args);
        if (['all', 'first', 'run', 'raw'].includes(property)) return async (...args) => {
          const result = await target[property](...args);
          if (!sql.startsWith('EXPLAIN ')) {
            const kind = sql === 'SELECT * FROM installations WHERE app=?1 AND account_login=?2 AND uninstalled_at IS NULL'
              ? `installation_login:${values[0]}`
              : sql === 'SELECT * FROM installations WHERE app=?1 AND account_id=?2 AND uninstalled_at IS NULL'
                ? `installation_authority:${values[0]}`
                : sql.startsWith('SELECT COUNT(*) AS pending') ? 'pending_badge'
                  : sql.startsWith('WITH page AS MATERIALIZED') ? 'queue_page' : 'unexpected';
            statements.push({ kind, rows_read: result?.meta?.rows_read ?? 0 });
          }
          return result;
        };
        const value = target[property];
        return typeof value === 'function' ? value.bind(target) : value;
      },
    });
    const db = new Proxy(env.DB, {
      get(target, property) {
        if (property === 'constructor') return target.constructor;
        if (property === 'prepare') return sql => observe(target.prepare(sql), sql);
        const value = target[property];
        return typeof value === 'function' ? value.bind(target) : value;
      },
    });
    const response = await worker.fetch(request, { ...env, DB: db }, ctx);
    response.headers.set('x-test-d1-classes', JSON.stringify(statements));
    return response;
  },
};
