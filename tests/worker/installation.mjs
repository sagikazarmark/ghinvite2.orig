import { setupCommand } from './installation-fixture.mjs';
import { admitCapability } from './capability.mjs';
import assert from 'node:assert/strict';
import { repoApp } from './github-world.mjs';

const account = 820;
const repoIds = Array.from({ length: 101 }, (_, n) => n + 10);

// GitHub (octoemu): the organization `renamed` (820) with 101 repositories
// (10…110), the repository App installed on all of them as 8201.
export const seed = () => ({
  users: [{ id: 7, login: 'installer' }, { id: 91, login: 'user-91' }, { id: 92, login: 'user-92' }],
  orgs: [{ id: account, login: 'renamed', created_at: '2020-01-01T00:00:00Z' }],
  repos: repoIds.map(id => ({ id, owner: 'renamed', name: `repo-${id}`, private: true })),
  apps: [repoApp],
  installations: [{ id: 8201, app_id: 123, account: 'renamed', repository_selection: 'all', created_at: '2026-01-01T00:00:00Z' }],
  memberships: [{ org: 'renamed', user: 'installer', role: 'admin', state: 'active' }],
});

// Public ingress/Storage seams. GitHub is octoemu, and only the acknowledgement
// of an actual D1 write is controlled; installation and admission handlers are real.
export async function installationRecovery({ ingress, http, storage, id, creation, eventually, github, loseAuditAck }) {
  let current = 8201;
  const emulator = github.emulator;
  const pages = async () => (await emulator.calls()).filter(call => call.path.split('?')[0] === '/installation/repositories')
    .map(call => Number(new URL(call.path, 'http://x').searchParams.get('page') || 1));
  // GitHub stops answering for the installation (an injected 404), or
  // fails the second page of its repositories (an injected 503).
  const rules = [];
  const mode = async value => {
    for (const rule of rules.splice(0)) await emulator.removeFault(rule);
    if (value === 'unavailable') rules.push(await emulator.fault({ method: 'GET', path: `/app/installations/${current}`, label: 'installation unavailable',
      action: { kind: 'respond', status: 404, body: { message: 'Not Found', documentation_url: 'https://docs.github.com/rest/apps/apps#get-an-installation-for-the-authenticated-app' } } }));
    if (value === 'incomplete') rules.push(await emulator.fault({ method: 'GET', path: '/installation/repositories', query_contains: 'page=2', label: 'incomplete scope',
      action: { kind: 'respond', status: 503, body: { message: 'Service Unavailable' } } }));
  };
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
    assert.equal(initialStatus.observation.kind, 'available', JSON.stringify({pages: await pages(), initialStatus}));
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
    await emulator.admin('POST',`/installations/${current}/suspend`);
    const suspendedAt=(await emulator.state()).world.installations.find(value=>value.id===current).suspended_at;
    await event(current,'onboard_webhook',webhook('c','suspend','2026-01-03T01:00:00Z'));
    const suspended = (await eventually(() => storage('audit',account), page=>page.events.some(row=>row.event_type==='installation.suspended'))).events.filter(row=>row.event_type==='installation.suspended');
    assert.equal(suspended.length,1);
    assert.equal(Date.parse(suspended[0].occurred_at),Date.parse(suspendedAt));
    assert.equal(Date.parse(suspended[0].metadata.source.suspended_at),Date.parse(suspendedAt));
    await event(current,'onboard_webhook',webhook('d','suspend','2026-01-03T02:00:00Z'));
    assert.deepEqual((await storage('audit',account)).events.filter(row=>row.event_type==='installation.suspended'),suspended);
    await emulator.admin('POST',`/installations/${current}/unsuspend`);
    await event(current,'onboard_webhook',webhook('e','unsuspend','2026-01-04T00:00:00Z'));
    assert.equal((await status()).observation.kind,'available');
    console.log('PASS signed Worker installation permission/suspension V1 source facts, exact replay and semantic dedup with actual D1');
    await call('create', input);
    await mode('unavailable');
    // Onboarding left a recent available observation, which admits on its own.
    // The account observes the loss the way a webhook would; the rejection
    // below still rests on the caller's explicit live evidence read.
    await http(`${ingress}/AccountInstallation/repo:${account}/refresh`, current);
    const unavailableAttempt = await attachEvidence(attempt(91));
    const rejected = await call('admit', unavailableAttempt);
    assert.deepEqual(rejected.result, { kind: 'rejected', reason: 'scope_unavailable' });
    // Start the incomplete observation from confirmed full scope, independently
    // of the preceding unavailable case's fail-closed projection.
    const scope = repoIds;
    await mode('available');
    await http(`${ingress}/AccountInstallation/repo:${account}/refresh`, current);
    await eventually(() => storage('app-installation', ['repo', current]).then(value => value?.account), row => row?.selected_repos?.length === 101);
    assert.deepEqual((await storage('app-installation', ['repo', current]).then(value => value?.account)).selected_repos, scope);
    await mode('incomplete');
    // Same again: the account has to observe the incomplete pagination before
    // the caller refreshes evidence before requesting admission.
    await http(`${ingress}/AccountInstallation/repo:${account}/refresh`, current);
    await emulator.clearCalls();
    const retry = attempt(91);
    const observation = await evidence();
    assert.equal(observation.eligibility.kind, 'unknown');
    assert.ok((await pages()).includes(2), 'observation must traverse beyond the first page');
    assert.equal((await status()).observation.kind, 'unknown');
    // Unknown availability clears delivery prerequisites, not link guardrails.
    await eventually(() => storage('app-installation', ['repo', current]).then(value => value?.account), row => row?.selected_repos?.length === 0);
    const unavailableLink = await call('link_status', { link_id: input.link_id, admin: input.admin });
    assert.equal(unavailableLink.uses, 0);
    assert.deepEqual(unavailableLink.creation.targets, input.targets);
    await mode('available');
    assert.deepEqual(await call('admit', unavailableAttempt), rejected, 'definitive rejection is retained');
    await attachEvidence(retry);
    const accepted = await call('admit', retry);
    assert.equal(accepted.result.kind, 'accepted', 'incomplete observation must not bind a rejection');
    await eventually(() => storage('app-installation', ['repo', current]).then(value => value?.account), row => row?.selected_repos?.length === 101);
    assert.deepEqual((await storage('app-installation', ['repo', current]).then(value => value?.account)).selected_repos, scope);
    console.log('PASS Worker/D1 unavailable installation and incomplete paginated scope recover without consuming a use');

    await eventually(() => storage('audit', account), page => page.events.some(row => row.target_id === 'repo:8201' && row.event_type === 'installation.created'));
    // The App is reinstalled on the organization: 8202 replaces 8201.
    await emulator.admin('DELETE', `/installations/${current}`);
    current = 8202;
    await emulator.admin('POST', '/installations', { installation: { id: current, app_id: 123, account: 'renamed', repository_selection: 'all' } }, 201);
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
    await mode('available');
    loseAuditAck(null);
  }
}
