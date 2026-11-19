import assert from 'node:assert/strict';
import { createHash, createHmac } from 'node:crypto';
import { fileURLToPath } from 'node:url';
import { Response } from 'miniflare';
import { installedWebApp, signinApp } from './app-config.mjs';
import { orgApp } from './github-world.mjs';

const account=860, installation=8602, user=8, team=50;

// GitHub (octoemu): `org-fixture` (860, owner 7) with the team `devs` (50),
// alice (8), the organization App installed as 8602.
export const seed = () => ({
  users:[{id:7,login:'owner'},{id:user,login:'alice'}],
  orgs:[{id:account,login:'org-fixture',created_at:'2020-01-01T00:00:00Z'}],
  teams:[{id:team,org:'org-fixture',slug:'devs',name:'Developers'}],
  apps:[orgApp],
  installations:[{id:installation,app_id:456,account:'org-fixture',repository_selection:'all',created_at:'2026-01-01T00:00:00Z'}],
  memberships:[{org:'org-fixture',user:'owner',role:'admin',state:'active'}],
});

const forwarded = [];
let loseAcknowledgement = true;
export function organizationWebhookWorker(ingress) {
  forwarded.length = 0;
  loseAcknowledgement = true;
  return {
    name: 'org-webhook-web', modules: true,
    scriptPath: fileURLToPath(new URL('./worker.mjs', import.meta.url)),
    modulesRules: [{ type: 'CompiledWasm', include: ['**/*.wasm'] }],
    compatibilityDate: '2024-09-23', compatibilityFlags: ['nodejs_compat'],
    kvNamespaces: ['SESSIONS', 'AUTHZ_CACHE'],
    ratelimits: { RL_IP: { namespace_id: '1001', simple: { limit: 30, period: 60 } }, RL_USER: { namespace_id: '1002', simple: { limit: 10, period: 60 } }, AUTHZ_RL: { namespace_id: '1003', simple: { limit: 10, period: 60 } } },
    d1Databases: { DB: 'journey-shared' },
    bindings: {
      P1_HOST: 'journey.test', P1_KEYRING: 'kekring1;name=p1;current=1;hwm=1;k1=BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwc.9fdab21f330c89b7',
      RESTATE_INGRESS_URL: 'https://ingress.fixture.test', RESTATE_INGRESS_TOKEN: 'synthetic-worker-ingress-key',
      GHINVITE_GITHUB_INSTALL_URL: 'https://github.com/apps/dummy/installations/new',
      ...signinApp,
      ...installedWebApp, P1_APP_STAGE: 'repo-and-org', GH_ORG_DELIVERY_ENABLED: 'true',
      GH_ORG_APP_ID: '456', GH_ORG_CLIENT_ID: 'Iv.fixture-org',
      GH_ORG_PRIVATE_KEYS: installedWebApp.GH_REPO_PRIVATE_KEYS,
      GH_ORG_WEBHOOK_SECRETS: 'org-current;org-previous',
    },
    async outboundService(request) {
      const url = new URL(request.url);
      assert.equal(url.origin, 'https://ingress.fixture.test');
      assert.equal(request.headers.get('authorization'), 'Bearer synthetic-worker-ingress-key');
      assert.match(url.pathname, /^\/OrganizationDelivery\/[^/]+\/on_webhook\/send$/);
      const input = Buffer.from(await request.arrayBuffer());
      forwarded.push(JSON.parse(input));
      const response = await fetch(`${ingress}${url.pathname}`, { method: request.method, headers: [...request.headers].filter(([name]) => name !== 'content-length'), body: input, signal: AbortSignal.timeout(25_000) });
      const body = await response.arrayBuffer();
      if (response.ok && loseAcknowledgement) {
        loseAcknowledgement = false;
        return new Response(null, { status: 502 });
      }
      return new Response(body, { status: response.status, headers: response.headers });
    },
  };
}

// Real HMAC ingress, D1 exact routing, signed Restate dispatch and fresh owner
// observation. GitHub is octoemu (alice accepts there); webhook claims never
// settle a target.
export async function organizationWebhooks({ mf, ingress, http, id, db, eventually, github }) {
  const membershipReads=async()=>(await github.requests('GET','/orgs/org-fixture/memberships/alice')).length;
  const posts=async()=>(await github.requests('POST','/orgs/org-fixture/invitations')).length;
  try {
    for(const [uid,login] of [[7,'owner'],[user,'alice']]) await db.prepare('INSERT INTO users(user_id,login,avatar_url,last_seen_at) VALUES (?, ?, NULL, ?)').bind(uid,login,new Date().toISOString()).run();
    await http(`${ingress}/Installation/org:${installation}/onboard`,{operation_id:id(),verified:{app:'org',account:{installation_id:installation,account_id:account,account_login:'org-fixture',account_type:'Organization',selected_repos:[],installed_at:'2026-01-01T00:00:00Z',uninstalled_at:null},suspended_at:null,permissions:{members:'write',metadata:'read'}}});
    const link=id(),secret='3a'.repeat(32);
    const creation={link_id:link,secret_sha256:secret,account_id:account,installation_id:installation,admin:{account_id:account,user_id:7},description:'Authenticated organization webhook',max_uses:10,approval_required:false,targets:{organization:{org_id:account,org_login:'org-fixture',grant:'member'},teams:[{team_id:team,team_slug:'devs',grant:'member'}],repos:[]}};
    assert.deepEqual(await http(`${ingress}/InvitationLink/${link}/create`,creation),{Ok:{link_id:link,secret_generation:1}});
    const admission=await http(`${ingress}/InvitationLink/${link}/admit`,{scope_evidence:[],secret_sha256:secret,secret_generation:1,link_id:link,operation_id:id(),requester_id:user,justification:null});
    assert.equal(admission.result.kind,'accepted');
    const request=admission.result.request_id,key=`${request}:${account}:1`;
    const status=()=>http(`${ingress}/OrganizationDelivery/${key}/status`);
    await eventually(status,value=>value?.create.outcome.kind==='created');
    await eventually(()=>db.prepare("SELECT id FROM deliveries WHERE id=?").bind(`org:${key}`).first(),Boolean);
    const web=await mf.getWorker('org-webhook-web');
    const send=async(event,payload,secret='org-current')=>{
      const body=JSON.stringify(payload);
      return web.fetch('https://journey.test/webhooks/org',{method:'POST',headers:{'content-type':'application/json','x-github-event':event,'x-github-delivery':id(),'x-hub-signature-256':`sha256=${createHmac('sha256',secret).update(body).digest('hex')}`},body});
    };
    const payload={action:'member_added',installation:{id:installation,app_id:456},organization:{id:account},membership:{user:{id:user}},sender:{id:999}};
    assert.equal((await send('organization',payload,'fixture-repo-webhook-current')).status,401);
    assert.equal(forwarded.length,0);
    const upstream=(await github.api('GET','/orgs/org-fixture/invitations',undefined,{installation})).find(value=>value.login==='alice').id;
    const wrong={...payload,membership:{user:{id:upstream}},sender:{id:user}};
    assert.equal((await send('organization',wrong)).status,202);
    assert.equal(forwarded.length,0,'invitation and sender IDs cannot replace member identity');
    const wrongHash=createHash('sha256').update(JSON.stringify(wrong)).digest('hex');
    const noMatch=await db.prepare('SELECT receipt FROM organization_webhook_receipts WHERE payload_sha256=?').bind(wrongHash).first();
    assert.equal(noMatch.receipt,null);
    const readsBefore=await membershipReads();
    assert.equal((await send('organization',payload)).status,502,'lost ingress acknowledgement remains retryable');
    assert.equal((await send('organization',payload,'org-previous')).status,202);
    assert.equal(forwarded.length,2);
    assert.deepEqual(forwarded[0],forwarded[1],'retry retains exact original binding and received_at');
    assert.equal(forwarded[0].identity.request_id,request);
    assert.equal(forwarded[0].identity.delivery_generation,1);
    assert.equal(forwarded[0].requester_id,user);
    await eventually(membershipReads,value=>value>readsBefore);
    assert.equal((await status()).settlement,null,'member_added is only a wake while invitation remains pending');
    // alice accepts on GitHub: an active member, and of the team.
    await github.emulator.act('alice','orgs/org-fixture/accept');
    assert.equal((await send('membership',{action:'added',scope:'team',installation:{id:installation,app_id:456},organization:{id:account},member:{id:user},team:{id:team},sender:{id:999}})).status,202);
    const settled=await eventually(status,value=>value?.settlement?.state==='accepted');
    assert.equal(settled.teams[team].settlement.state,'accepted');
    assert.equal(forwarded.at(-1).source,'team_membership_added');
    assert.equal(forwarded.at(-1).team_id,team);
    assert.equal(await posts(),1,'webhook retries do not write GitHub');
    await eventually(async()=>JSON.parse((await db.prepare('SELECT receipt FROM delivery_outcomes WHERE delivery_id=?').bind(`org:${key}`).first()).receipt),value=>value.settlement?.state==='accepted');
    console.log('PASS authenticated organization Worker webhooks: HMAC app separation, numeric member/team routing, exact lost-ack replay in D1, payload cannot settle, fresh reads settle org and attached team, one original POST');
  } finally { /* GitHub-side state needs no restoring */ }
}
