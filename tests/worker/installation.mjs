import { setupCommand } from './installation-fixture.mjs';
import { admitCapability } from './capability.mjs';
import assert from 'node:assert/strict';
import { Response } from 'miniflare';

// Public ingress/Storage seams. Only GitHub HTTP and the acknowledgement of an
// actual D1 write are controlled; installation and admission handlers are real.
export async function installationRecovery({ ingress, http, storage, id, creation, eventually, observe, loseAuditAck }) {
  const account = 820;
  let current = 8201;
  let mode = 'available';
  let suspendedAt = null;
  const pages = [];
  observe(request => {
    const url = new URL(request.url);
    if (url.pathname.startsWith('/app/installations/')) {
      if (url.pathname.endsWith('/access_tokens')) return Response.json({ token: 'fixture', expires_at: '2099-01-01T00:00:00Z' }, {status:201});
      if (mode === 'unavailable' || url.pathname !== `/app/installations/${current}`) return Response.json({}, { status: 404 });
      return Response.json({ id: current, account: { id: account, login: 'renamed', type: 'Organization' }, app_id: 123, created_at: '2026-01-01T00:00:00Z', updated_at: '2026-01-01T00:00:00Z', repository_selection: 'all', permissions: {administration: 'write', metadata: 'read'}, suspended_at: suspendedAt });
    }
    if (url.pathname === '/installation/repositories') {
      const page = Number(url.searchParams.get('page') || 1);
      pages.push(page);
      // A second-page failure must never turn the first page into complete scope.
      if (mode === 'incomplete' && page === 2) return Response.json({}, { status: 503 });
      const repoIds = page === 1 ? Array.from({ length: 100 }, (_, n) => n + 10) : [110];
      return Response.json({ total_count: 101, repositories: repoIds.map(id => ({ id, full_name: `acme/repo-${id}`, private: true })) }, {headers: page === 1 ? {link: `<${url.origin}/installation/repositories?per_page=100&page=2>; rel="next"`} : {}});
    }
    assert.fail(`unexpected installation HTTP: ${request.method} ${url.pathname}`);
  });
  try {
    const onboard = installation_id => (setupCommand({ installation_id, actor_user_id: 7, account_id: account,
      account_login: 'stale-webhook-name', account_type: 'Organization', selected_repos: 'all', installed_at: '2026-01-01T00:00:00Z' }));
    const event = (installation, handler, body) => http(`${ingress}/Installation/repo:${installation}/${handler}`, body);
    const status = () => http(`${ingress}/AccountInstallation/repo:${account}/status`);
    const evidence = () => http(`${ingress}/AccountInstallation/repo:${account}/admission_evidence`, { account_id:account, repo_ids:[10,110] });
    const attachEvidence = async input => {
      const observation = await evidence();
      assert.notEqual(observation.eligibility.kind,'unknown');
      input.scope_evidence = [{app:'repo',observed_at:new Date().toISOString(),available:observation.eligibility.kind==='available',unavailable_target_ids:[]}];
      return input;
    };
    const input = { ...creation(), account_id: account, installation_id: current, admin: { account_id: account, user_id: 7 },
      max_uses: 3, targets: { organization: null, teams: [], repos: [{ repo_id: 10, repo_full_name: 'acme/repo-10', grant: 'read' }, { repo_id: 110, repo_full_name: 'acme/repo-110', grant: 'read' }] } };
    const call = (handler, body) => http(`${ingress}/InvitationLink/${input.link_id}/${handler}`, body);
    const attempt = requester_id => ({ ...admitCapability(input.link_id), link_id: input.link_id, operation_id: id(), requester_id });
    await event(current, 'onboard', onboard(current));
    const initialStatus = await status();
    assert.equal(initialStatus.observation.kind, 'available', JSON.stringify({pages, initialStatus}));
    const webhook = (marker, action, updated_at) => ({delivery_id:marker.repeat(64),action,updated_at,
      verified:onboard(current).verified,sender_id:7,sender_login:'installer'});
    const acceptedPermissions = webhook('a','new_permissions_accepted','2026-01-02T00:00:00Z');
    await event(current,'onboard_webhook',acceptedPermissions);
    const permissions = (await eventually(() => storage('audit',account), page => page.events.some(row=>row.event_type==='installation.permissions_accepted'))).events.filter(row=>row.event_type==='installation.permissions_accepted');
    assert.equal(permissions.length,1);
    assert.equal(Date.parse(permissions[0].occurred_at),Date.parse(acceptedPermissions.updated_at));
    assert.equal(permissions[0].actor_id,7);
    assert.equal(permissions[0].metadata.source.payload_sha256,acceptedPermissions.delivery_id);
    assert.equal(permissions[0].request_id,null,'canonical source facts have no recovery-invocation identity');
    await event(current,'onboard_webhook',acceptedPermissions);
    await event(current,'onboard_webhook',webhook('b','new_permissions_accepted',acceptedPermissions.updated_at));
    assert.deepEqual((await storage('audit',account)).events.filter(row=>row.event_type==='installation.permissions_accepted'),permissions);
    suspendedAt='2026-01-03T00:00:00Z';
    await event(current,'onboard_webhook',webhook('c','suspend','2026-01-03T01:00:00Z'));
    const suspended = (await eventually(() => storage('audit',account), page=>page.events.some(row=>row.event_type==='installation.suspended'))).events.filter(row=>row.event_type==='installation.suspended');
    assert.equal(suspended.length,1);
    assert.equal(Date.parse(suspended[0].occurred_at),Date.parse(suspendedAt));
    assert.equal(suspended[0].metadata.source.suspended_at,suspendedAt);
    await event(current,'onboard_webhook',webhook('d','suspend','2026-01-03T02:00:00Z'));
    assert.deepEqual((await storage('audit',account)).events.filter(row=>row.event_type==='installation.suspended'),suspended);
    suspendedAt=null;
    await event(current,'onboard_webhook',webhook('e','unsuspend','2026-01-04T00:00:00Z'));
    assert.equal((await status()).observation.kind,'available');
    console.log('PASS signed Worker installation permission/suspension V1 source facts, exact replay and semantic dedup with actual D1');
    await call('create', input);
    mode = 'unavailable';
    // Onboarding left a recent available observation, which admits on its own.
    // The account observes the loss the way a webhook would; the rejection
    // below still rests on the caller's explicit live evidence read.
    await http(`${ingress}/AccountInstallation/repo:${account}/refresh`, current);
    const unavailableAttempt = await attachEvidence(attempt(91));
    const rejected = await call('admit', unavailableAttempt);
    assert.deepEqual(rejected.result, { kind: 'rejected', reason: 'scope_unavailable' });
    // Start the incomplete observation from confirmed full scope, independently
    // of the preceding unavailable case's fail-closed projection.
    const scope = Array.from({ length: 101 }, (_, n) => n + 10);
    mode = 'available';
    await http(`${ingress}/AccountInstallation/repo:${account}/refresh`, current);
    await eventually(() => storage('app-installation', ['repo', current]).then(value => value?.account), row => row?.selected_repos?.length === 101);
    assert.deepEqual((await storage('app-installation', ['repo', current]).then(value => value?.account)).selected_repos, scope);
    mode = 'incomplete';
    // Same again: the account has to observe the incomplete pagination before
    // the caller refreshes evidence before requesting admission.
    await http(`${ingress}/AccountInstallation/repo:${account}/refresh`, current);
    pages.length = 0;
    const retry = attempt(91);
    const observation = await evidence();
    assert.equal(observation.eligibility.kind, 'unknown');
    assert.ok(pages.includes(2), 'observation must traverse beyond the first page');
    assert.equal((await status()).observation.kind, 'unknown');
    // Unknown availability clears delivery prerequisites, not link guardrails.
    await eventually(() => storage('app-installation', ['repo', current]).then(value => value?.account), row => row?.selected_repos?.length === 0);
    const unavailableLink = await call('link_status', { link_id: input.link_id, admin: input.admin });
    assert.equal(unavailableLink.uses, 0);
    assert.deepEqual(unavailableLink.creation.targets, input.targets);
    mode = 'available';
    assert.deepEqual(await call('admit', unavailableAttempt), rejected, 'definitive rejection is retained');
    await attachEvidence(retry);
    const accepted = await call('admit', retry);
    assert.equal(accepted.result.kind, 'accepted', 'incomplete observation must not bind a rejection');
    await eventually(() => storage('app-installation', ['repo', current]).then(value => value?.account), row => row?.selected_repos?.length === 101);
    assert.deepEqual((await storage('app-installation', ['repo', current]).then(value => value?.account)).selected_repos, scope);
    console.log('PASS Worker/D1 unavailable installation and incomplete paginated scope recover without consuming a use');

    await eventually(() => storage('audit', account), page => page.events.some(row => row.target_id === 'repo:8201' && row.event_type === 'installation.created'));
    current = 8202;
    const lost = loseAuditAck(String(account));
    await event(current, 'onboard', onboard(current));
    await eventually(lost.count, count => count > 0);
    const committed = (await storage('audit', account)).events;
    const created = committed.filter(row => row.target_id === `repo:${current}` && row.event_type === 'installation.created');
    assert.equal(created.length, 1, 'audit really committed before acknowledgement loss');
    lost.release();
    // Attach to the exact interrupted invocation: releasing the projector key
    // after terminal failure is not successful recovery.
    assert.ok(lost.invocation(), 'faulted invocation identity must be observed');
    await http(`${ingress}/restate/invocation/${lost.invocation()}/attach`, undefined, 'GET');
    assert.deepEqual((await storage('audit', account)).events.filter(row => row.target_id === `repo:${current}` && row.event_type === 'installation.created'), created);
    const assertReplacement = async () => {
      const replacement = await status();
      assert.equal(replacement.account.installation_id, current);
      assert.equal(replacement.account.account_login, 'renamed');
      assert.deepEqual(replacement.observation.repo_ids, scope);
      const projected = await eventually(() => storage('app-installation', ['repo', current]).then(value => value?.account), row => row?.selected_repos?.length === 101);
      assert.deepEqual(projected.selected_repos, scope);
      assert.equal(projected.uninstalled_at, null);
      assert.equal(projected.account_id, account);
    };
    await assertReplacement();
    // Before the old identity receives its uninstall tombstone, old events must
    // reach account-level convergence and still leave the replacement intact.
    await event(8201, 'onboard', onboard(8201));
    await assertReplacement();
    await event(8201, 'repos_changed', { app:'repo', installation_id: 8201, selected_repos: [] });
    await assertReplacement();
    for (let duplicate = 0; duplicate < 2; duplicate++) {
      await event(8201, 'uninstall', { app:'repo', installation_id: 8201, uninstalled_at: '2026-02-01T00:00:00Z' });
      await event(8201, 'onboard', onboard(8201));
      await event(8201, 'repos_changed', { app:'repo', installation_id: 8201, selected_repos: [] });
    }
    await assertReplacement();
    assert.ok((await storage('app-installation', ['repo', 8201]).then(value => value?.account)).uninstalled_at, 'replacement retires old D1 installation');
    assert.deepEqual(await call('admit', retry), accepted);
    assert.equal((await call('admit', attempt(92))).result.kind, 'accepted');
    const link = await call('link_status', { link_id: input.link_id, admin: input.admin });
    assert.equal(link.uses, 2);
    assert.equal(link.creation.installation_id, 8201);
    assert.deepEqual(link.creation.targets, input.targets);
    console.log('PASS Worker/D1 replacement, delayed duplicate old events, committed audit acknowledgement loss and replay');
  } finally {
    observe(null);
    loseAuditAck(null);
  }
}
