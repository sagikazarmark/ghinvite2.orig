import { randomBytes } from 'node:crypto';

// Explicit verified-setup fixture; the callback actor cannot establish owner.
export function setupCommand(account) {
  const {actor_user_id, app, ...facts} = account;
  return {operation_id:'01'+randomBytes(12).toString('hex').toUpperCase(), verified:{
    app:'repo',account:{...facts,uninstalled_at:null},suspended_at:null,
    permissions:{administration:'write',metadata:'read'},
  }};
}

// Signed-webhook boundary fixture; setup still cannot establish an installer.
export function signedOwnerCommand(setup, sender = 7) {
  return {delivery_id:'a'.repeat(64),action:'created',verified:setup.verified,
    sender_id:sender,sender_login:`user-${sender}`,updated_at:'2026-01-01T01:00:00Z'};
}
export async function establishInstallingOwner({http, ingress, installationId = 1, accountId = 100, sender = 7}) {
  const status = await http(`${ingress}/AccountInstallation/repo:${accountId}/status`);
  const setup = {verified:{app:'repo',account:status.account,suspended_at:null,
    permissions:{administration:'write',metadata:'read'}}};
  await http(`${ingress}/Installation/repo:${installationId}/onboard_webhook`,signedOwnerCommand(setup,sender));
}

// Canonical read-model fixture for personal-owner Console tests (no installer claim).
export async function seedPersonalInstallation(db, { installationId = 1, accountId = 42, login = 'octocat', repoId = 10 } = {}) {
  const snapshot = {app:'repo',revision:1,account:{installation_id:installationId,account_id:accountId,
    account_login:login,account_type:'User',installed_at:'2026-01-01T00:00:00Z',
    uninstalled_at:null,selected_repos:[repoId]},suspended_at:null,
    permissions:{administration:'write',metadata:'read'},installed_by_user_id:null,installed_by_login:null};
  await db.prepare(`INSERT INTO installations(app,installation_id,account_id,account_login,account_type,installed_at,uninstalled_at,selected_repos,permissions_json,projection_revision,projection_content)
    VALUES ('repo',?,?,?,'User','2026-01-01T00:00:00Z',NULL,?,?,1,?)`)
    .bind(installationId,accountId,login,JSON.stringify([repoId]),JSON.stringify(snapshot.permissions),JSON.stringify(snapshot)).run();
}
