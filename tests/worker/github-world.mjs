// The GitHub side of the admission drivers (GE-09b-2): the octoemu world
// they start from, and the GitHub-side actions they take on it (an invitee
// accepting, a user renaming, a repository selection changing). Everything
// goes through octoemu's native admin API or GitHub's own REST routes with
// an admin-minted installation token, never an inline fake; failures are
// fault rules.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { fixturePublicKey, signinAppSeed } from './octoemu.mjs';

// The workflow Workers' Apps: ghinvite's fixture key, the client IDs the
// Workers sign their JWTs with (GH_REPO_CLIENT_ID, GH_ORG_CLIENT_ID).
export const repoApp = { id: 123, slug: 'p1-repo', client_id: 'Iv.fixture-repo', client_secret: 'fixture-repo-secret',
  public_keys: [fixturePublicKey], permissions: { administration: 'write', metadata: 'read' } };
export const orgApp = { id: 456, slug: 'p1-org', client_id: 'Iv.fixture-org', client_secret: 'fixture-org-secret',
  public_keys: [fixturePublicKey], permissions: { members: 'write', metadata: 'read' } };

// The web Worker's token-broker fixture App (broker_fixture.rs): App 99001,
// metadata only.
export const brokerApp = { id: 99001, slug: 'runtime-broker', client_id: 'Iv-runtime-broker', client_secret: 'runtime-broker-secret',
  public_keys: [fixturePublicKey], permissions: { metadata: 'read' } };

const users = ids => ids.map(id => ({ id, login: `user-${id}` }));

/**
 * The stub's old default world, natively: the organization `acme` (100)
 * owned by `creator` (7) with `acme/api` (10), `acme/web` (11) and
 * `acme/repo-12` (12); `alice` (8) and the requesters `user-91`…`user-99`;
 * the repository App 123 installed as 1 on `acme/api` and `acme/web`; the
 * organization App 456 installed as 2. Also the organization `adopted` (300)
 * with `adopted/api` (30) and `adopted/web` (31), the repository App
 * installed there as 40, and the web Workers' sign-in App.
 */
export function acmeSeed({ extraUsers = [] } = {}) {
  return {
    users: [{ id: 7, login: 'creator' }, { id: 8, login: 'alice' }, ...users([91, 92, 93, 94, 95, 96, 97, 98, 99]), ...extraUsers],
    orgs: [{ id: 100, login: 'acme', created_at: '2020-01-01T00:00:00Z' }, { id: 300, login: 'adopted', created_at: '2020-01-01T00:00:00Z' }],
    repos: [{ id: 10, owner: 'acme', name: 'api', private: true }, { id: 11, owner: 'acme', name: 'web', private: true },
      { id: 12, owner: 'acme', name: 'repo-12', private: true },
      { id: 30, owner: 'adopted', name: 'api', private: true }, { id: 31, owner: 'adopted', name: 'web', private: true }],
    apps: [repoApp, orgApp, signinAppSeed({ id: 1, hosts: ['journey.test', 'browser.test'] })],
    installations: [
      { id: 40, app_id: 123, account: 'adopted', repository_selection: 'all', created_at: '2026-01-01T00:00:00Z' },
      { id: 1, app_id: 123, account: 'acme', repository_selection: 'selected', repositories: ['acme/api', 'acme/web'], created_at: '2026-01-01T00:00:00Z' },
      { id: 2, app_id: 456, account: 'acme', repository_selection: 'all', created_at: '2026-01-01T00:00:00Z' },
    ],
    memberships: [{ org: 'acme', user: 'creator', role: 'admin', state: 'active' }],
  };
}

const tokenId = token => createHash('sha256').update(token).digest().subarray(0, 16).toString('hex');

/** GitHub-side actions on an emulator, as the drivers need them. */
export class GithubWorld {
  constructor(emulator) {
    this.emulator = emulator;
    this.tokens = new Map();
    this.ownTokens = new Set();
    // The installation the test's own API calls use (acme's).
    this.installation = 1;
  }

  /** An installation token minted through the admin API (not the Workers'). */
  async token(installation = this.installation) {
    const cached = this.tokens.get(installation);
    if (cached) return cached;
    const minted = await this.emulator.admin('POST', '/tokens/installation', { installation_id: installation });
    this.ownTokens.add(tokenId(minted.token));
    this.tokens.set(installation, minted.token);
    return minted.token;
  }

  /** A GitHub REST call as the App's installation (the test's own token). */
  async api(method, path, body, { installation = this.installation, expected, retried = false } = {}) {
    const response = await fetch(`${this.emulator.apiBase}${path}`, {
      method, signal: AbortSignal.timeout(30_000),
      headers: { authorization: `Bearer ${await this.token(installation)}`, accept: 'application/vnd.github+json',
        ...(body === undefined ? {} : { 'content-type': 'application/json' }) },
      body: body === undefined ? undefined : JSON.stringify(body),
    });
    const text = await response.text();
    // An expired or replaced token of ours: mint again, once.
    if (response.status === 401 && !retried) { this.tokens.delete(installation); return this.api(method, path, body, { installation, expected, retried: true }); }
    if (expected !== undefined) assert.equal(response.status, expected, `${method} ${path}: ${text}`);
    else assert.ok(response.ok, `${method} ${path}: ${response.status} ${text}`);
    return text ? JSON.parse(text) : null;
  }

  /** The Workers' calls (the test's own API calls left out). */
  async calls(...routes) {
    return (await this.emulator.calls(...routes)).filter(call => !this.ownTokens.has(call.identity?.token_id));
  }
  /** The Workers' calls with this method and path (no query). */
  async requests(method, path) {
    return (await this.calls()).filter(call => call.method === method && (path === undefined || call.path.split('?')[0] === path));
  }

  /** The repository's open invitations, as GitHub lists them. */
  invitations(repo = 'acme/api', installation = this.installation) { return this.api('GET', `/repos/${repo}/invitations?per_page=100`, undefined, { installation }); }

  /**
   * The invitee accepts their invitation (the user-side route, through the
   * admin `act` API), then, with `role`, the App changes their role (204).
   */
  async accept(login, { repo = 'acme/api', id, role, installation = this.installation } = {}) {
    const invitation = id ?? (await this.invitations(repo, installation)).find(value => value.invitee.login === login)?.id;
    assert.ok(invitation, `${login} has an invitation to ${repo}`);
    await this.emulator.act(login, `repository_invitations/${invitation}/accept`);
    if (role) await this.api('PUT', `/repos/${repo}/collaborators/${login}`, { permission: role }, { installation, expected: 204 });
  }

  /**
   * `login` ends up a direct collaborator with `role`: they accept their
   * open invitation, or one the App sends them now, then the App sets the
   * role.
   */
  async grant(login, role = 'push', { repo = 'acme/api' } = {}) {
    if (!(await this.invitations(repo)).some(value => value.invitee.login === login)) {
      await this.api('PUT', `/repos/${repo}/collaborators/${login}`, { permission: role });
    }
    if ((await this.invitations(repo)).some(value => value.invitee.login === login)) await this.accept(login, { repo });
    await this.api('PUT', `/repos/${repo}/collaborators/${login}`, { permission: role }, { expected: 204 });
  }

  /** A pending invitation from someone else (the organization's owner). */
  async foreignInvitation(login, { repo = 'acme/api', role = 'pull', inviter = 'creator', id }) {
    await this.emulator.seed({ repository_invitations: [{ id, repo, invitee: login, inviter, role }] });
  }

  /** The user's open invitations withdrawn and their direct access removed. */
  async forget(login, repos = ['acme/api', 'acme/web']) {
    for (const repo of repos) {
      for (const invitation of await this.invitations(repo)) {
        if (invitation.invitee.login === login) await this.api('DELETE', `/repos/${repo}/invitations/${invitation.id}`, undefined, { expected: 204 });
      }
      await this.api('DELETE', `/repos/${repo}/collaborators/${login}`, undefined, { expected: 204 });
    }
  }

  /**
   * What the stub's `/reset` gave the drivers: no fault rules, an empty
   * call log, and `logins` with no invitations or direct access.
   */
  async reset(...logins) {
    await this.emulator.clearFaults();
    for (const login of logins) await this.forget(login);
    await this.emulator.clearCalls();
  }

  /**
   * GitHub's answer to the App's PUT for `login` on `repo`, as the stub's
   * outcomes staged it: `already_collaborator` (a direct `pull`
   * collaborator, so a PUT for a higher role is a role change: 204),
   * otherwise a fault rule on that PUT.
   */
  async outcome(login, outcome, repo = 'acme/api') {
    const path = `/repos/${repo}/collaborators/${login}`;
    switch (outcome) {
      case 'already_collaborator':
        await this.api('PUT', path, { permission: 'pull' }, { expected: 201 });
        return this.accept(login, { repo });
      case 'terminal_failure':
        return this.emulator.fault({ method: 'PUT', path, label: outcome, action: { kind: 'respond', status: 422,
          body: { message: 'Validation Failed', errors: [{ resource: 'Repository', code: 'custom', message: 'fixture refusal' }], documentation_url: 'https://docs.github.com/rest' } } });
      case 'transient_once':
        return this.emulator.fault({ method: 'PUT', path, label: outcome, times: 1, action: { kind: 'respond', status: 502, text: 'fixture: bad gateway' } });
      case 'created_response_lost':
        return this.emulator.fault({ method: 'PUT', path, label: outcome, action: { kind: 'apply_then_lose', status: 502 } });
      case 'access_lost_once':
        return this.emulator.fault({ method: 'PUT', path, label: outcome, times: 1, action: { kind: 'respond', status: 403,
          body: { message: 'Resource not accessible by integration', documentation_url: 'https://docs.github.com/rest' } } });
      default: throw new Error(`unknown outcome ${outcome}`);
    }
  }

  /** Installation `installation` now selects exactly these repositories. */
  async select(ids, installation = this.installation) {
    const names = { 10: 'acme/api', 11: 'acme/web', 12: 'acme/repo-12' };
    await this.emulator.admin('POST', `/installations/${installation}/repositories`, {
      repository_selection: 'selected', repositories: ids.map(id => names[id] ?? assert.fail(`no repository ${id}`)) });
    this.tokens.delete(installation);
  }

  /** The user changes their login (ID, invitations and access stay). */
  rename(login, to) { return this.emulator.admin('POST', `/users/${encodeURIComponent(login)}/rename`, { login: to }); }

  /** The invitation's 7 days are over. */
  expire(invitation) { return this.emulator.admin('POST', `/repository_invitations/${invitation}/expire`); }

  /**
   * GitHub's (virtual) clock catches up with a Worker clock pinned ahead
   * (`x-test-clock`): the emulator verifies the Workers' App JWTs against
   * its own time, so the two may not drift apart. It never moves back.
   */
  async follow(ms) {
    if (ms === undefined) return;
    const now = Date.parse((await this.emulator.admin('GET', '/clock')).now);
    if (ms > now) await this.emulator.advanceClock(Math.ceil((ms - now) / 1000));
  }

  /** Holds the Workers' next collaborator PUTs until released. */
  async holdPut() {
    const id = await this.emulator.fault({ method: 'PUT', path: '/repos/{owner}/{repo}/collaborators/{username}', label: 'held put', action: { kind: 'hold' } });
    let released = false;
    return {
      entered: this.emulator.waitHeld(id, 1, 10_000),
      release: () => { if (!released) { released = true; return this.emulator.removeFault(id); } },
    };
  }
}
