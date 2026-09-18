import assert from 'node:assert/strict';
import { Response } from 'miniflare';

// Public ingress/Storage seams. Only GitHub HTTP and the acknowledgement of an
// actual D1 write are controlled; installation and admission handlers are real.
export async function installationRecovery({ ingress, http, storage, id, creation, eventually, observe, loseAuditAck }) {
  const account = 820;
  let current = 8201;
  let mode = 'available';
  const pages = [];
  observe(request => {
    const url = new URL(request.url);
    if (url.pathname.startsWith('/app/installations/')) {
      if (url.pathname.endsWith('/access_tokens')) return Response.json({ token: 'fixture', expires_at: '2099-01-01T00:00:00Z' });
      if (mode === 'unavailable' || url.pathname !== `/app/installations/${current}`) return Response.json({}, { status: 404 });
      return Response.json({ id: current, account: { id: account, login: 'renamed', type: 'Organization' }, suspended_at: null });
    }
    if (url.pathname === '/installation/repositories') {
      const page = Number(url.searchParams.get('page') || 1);
      pages.push(page);
      // A second-page failure must never turn the first page into complete scope.
      if (mode === 'incomplete' && page === 2) return Response.json({}, { status: 503 });
      const repoIds = page === 1 ? Array.from({ length: 100 }, (_, n) => n + 10) : [110];
      return Response.json({ total_count: 101, repositories: repoIds.map(id => ({ id, full_name: `acme/repo-${id}`, private: true })) });
    }
    assert.fail(`unexpected installation HTTP: ${request.method} ${url.pathname}`);
  });
  try {
    const onboard = installation_id => ({ installation_id, actor_user_id: 7, account_id: account,
      account_login: 'stale-webhook-name', account_type: 'Organization', selected_repos: 'all', installed_at: '2026-01-01T00:00:00Z' });
    const event = (installation, handler, body) => http(`${ingress}/Installation/${installation}/${handler}`, body);
    const status = () => http(`${ingress}/AccountInstallationV1/${account}/status`);
    const input = { ...creation(), account_id: account, installation_id: current, admin: { account_id: account, user_id: 7 },
      max_uses: 3, repos: [{ repo_id: 10, repo_full_name: 'acme/repo-10' }, { repo_id: 110, repo_full_name: 'acme/repo-110' }] };
    const call = (handler, body) => http(`${ingress}/InvitationLinkV1/${input.link_id}/${handler}`, body);
    const attempt = requester_id => ({ version: 1, link_id: input.link_id, operation_id: id(), requester_id });
    await event(current, 'onboard', onboard(current));
    await call('create', input);
    mode = 'unavailable';
    const unavailableAttempt = attempt(91);
    const rejected = await call('admit', unavailableAttempt);
    assert.deepEqual(rejected.result, { kind: 'rejected', reason: 'installation_unavailable' });
    mode = 'incomplete';
    const retry = attempt(91);
    const response = await fetch(`${ingress}/InvitationLinkV1/${input.link_id}/admit`, {
      method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(retry), signal: AbortSignal.timeout(25_000),
    });
    assert.equal(response.status, 503, await response.text());
    assert.ok(pages.includes(2), 'observation must traverse beyond the first page');
    assert.equal((await status()).observation.kind, 'unknown');
    assert.equal((await call('link_status', { link_id: input.link_id, admin: input.admin })).uses, 0);
    mode = 'available';
    assert.deepEqual(await call('admit', unavailableAttempt), rejected, 'definitive rejection is retained');
    const accepted = await call('admit', retry);
    assert.equal(accepted.result.kind, 'accepted', 'incomplete observation must not bind a rejection');
    console.log('PASS Worker/D1 unavailable installation and incomplete paginated scope recover without consuming a use');

    await eventually(() => storage('audit', account), page => page.events.some(row => row.target_id === '8201' && row.event_type === 'installation.created'));
    current = 8202;
    const lost = loseAuditAck(String(account));
    await event(current, 'onboard', onboard(current));
    await eventually(lost.count, count => count > 0);
    const committed = (await storage('audit', account)).events;
    const created = committed.filter(row => row.target_id === String(current) && row.event_type === 'installation.created');
    assert.equal(created.length, 1, 'audit really committed before acknowledgement loss');
    lost.release();
    // Attach to the exact interrupted invocation: releasing the projector key
    // after terminal failure is not successful recovery.
    assert.ok(lost.invocation(), 'faulted invocation identity must be observed');
    await http(`${ingress}/restate/invocation/${lost.invocation()}/attach`, undefined, 'GET');
    assert.deepEqual((await storage('audit', account)).events.filter(row => row.target_id === String(current) && row.event_type === 'installation.created'), created);
    const scope = Array.from({ length: 101 }, (_, n) => n + 10);
    const assertReplacement = async () => {
      const replacement = await status();
      assert.equal(replacement.account.installation_id, current);
      assert.equal(replacement.account.account_login, 'renamed');
      assert.deepEqual(replacement.observation.repo_ids, scope);
      const projected = await eventually(() => storage('installation', current), row => row?.selected_repos?.length === 101);
      assert.deepEqual(projected.selected_repos, scope);
      assert.equal(projected.uninstalled_at, null);
      assert.equal(projected.account_id, account);
    };
    await assertReplacement();
    // Before the old identity receives its uninstall tombstone, old events must
    // reach account-level convergence and still leave the replacement intact.
    await event(8201, 'onboard', onboard(8201));
    await assertReplacement();
    await event(8201, 'repos_changed', { installation_id: 8201, selected_repos: [] });
    await assertReplacement();
    for (let duplicate = 0; duplicate < 2; duplicate++) {
      await event(8201, 'uninstall', { installation_id: 8201, uninstalled_at: '2026-02-01T00:00:00Z' });
      await event(8201, 'onboard', onboard(8201));
      await event(8201, 'repos_changed', { installation_id: 8201, selected_repos: [] });
    }
    await assertReplacement();
    assert.ok((await storage('installation', 8201)).uninstalled_at, 'replacement retires old D1 installation');
    assert.deepEqual(await call('admit', retry), accepted);
    assert.equal((await call('admit', attempt(92))).result.kind, 'accepted');
    const link = await call('link_status', { link_id: input.link_id, admin: input.admin });
    assert.equal(link.uses, 2);
    assert.equal(link.creation.installation_id, 8201);
    assert.deepEqual(link.creation.repos, input.repos);
    console.log('PASS Worker/D1 replacement, delayed duplicate old events, committed audit acknowledgement loss and replay');
  } finally {
    observe(null);
    loseAuditAck(null);
  }
}
