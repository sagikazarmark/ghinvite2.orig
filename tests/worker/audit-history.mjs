// Follow the production seek cursor: fixtures must not treat a 25-event page as
// complete account history once independent scenarios add more audit events.
export async function auditHistory(storage, account) {
  const events = [];
  let position = [account, null, 'latest', null, null];
  for (;;) {
    const page = await storage('audit-page', position);
    events.push(...page.events);
    if (!page.has_older) return events;
    const last = page.events.at(-1);
    if (!last) throw new Error('audit page advertised an older cursor without events');
    position = [account, null, 'before', last.id, last.occurred_at];
  }
}
