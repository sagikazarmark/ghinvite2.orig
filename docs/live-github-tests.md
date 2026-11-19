# Live GitHub tests

octoemu's fidelity suite (`libs/octoauth/crates/octoemu-conformance`) runs
the same scenarios against the emulator and against real GitHub. CI runs the
emulator side on every push. This guide sets up the GitHub side: the
accounts, Apps, organizations and the env file the live run reads. The
scenarios themselves are described in the
[octoemu README](../libs/octoauth/crates/octoemu/README.md#fidelity-suite-octoemu-conformance).

The live run is not a ghinvite deployment. It uses two throwaway GitHub Apps
on a throwaway organization, and nothing in it touches ghinvite's own App.

## What you end up with

| Thing | What it is | Needed for |
|---|---|---|
| **Operator** | Your own GitHub account, signed in to `gh`. It owns the organizations and both Apps. | everything |
| **Fidelity org** | A new organization on the **Free** plan with one private repository and one team. | everything |
| **Fidelity App** | A GitHub App owned by the fidelity org and installed on it: repository Administration write, organization Members write, Metadata read; Member, Organization and Membership events; an active webhook. | everything |
| **Invitee** | A second GitHub account that is **not** a member of the fidelity org, and its classic personal access token. | everything |
| **Sign-in App** | A second GitHub App with no permissions. The suite uses its OAuth client. | everything |
| User token | A user access token (`ghu_…`) of the sign-in App for the invitee. | optional: `user-permissionless` |
| Policy org | An organization where only owners may invite outside collaborators, with a repository and the fidelity App installed. Needs **GitHub Enterprise Cloud** (a trial works). | optional: `outside-collaborator-policy` |
| No-seat org | An organization on a **paid** plan with every seat filled, with the fidelity App installed. Costs a seat a month. | optional: `org-no-seat` |

Scenarios whose optional part is missing are skipped with a reason, not
failed. A first run needs only the rows without "optional".

All of it lives in one directory, `.octoemu-live/` at the repository root:
the env file `live.env`, the App's private key and any scratch files below.
The root `.gitignore` ignores that directory, and the suite refuses an env
file or a key inside the repository that git does not ignore, so neither can
be committed by accident. Never paste the values into chat, issues or PR
descriptions.

## 0. Before you start

You need `gh`, `jq`, `curl`, a browser and the repository's dev shell
(`devenv shell`, for the Rust toolchain). The invitee needs its own email
address for sign-up.

Sign `gh` in as the operator and check it:

```sh
gh auth status
# Listing an organization's installations needs the admin:org scope:
gh auth refresh -h github.com -s admin:org
```

Pick the names once and keep the shell open; every step below uses these
variables. Organization and App names are global on GitHub, so choose
unused ones.

```sh
cd /path/to/ghinvite2.orig           # the repository root
LIVE="$PWD/.octoemu-live"
ORG=yourname-fidelity                 # the fidelity org (new)
REPO=sandbox
TEAM=sandbox-team
INVITEE=yourname-invitee              # the invitee's login (new account)
umask 077 && mkdir -p "$LIVE"
git check-ignore -q "$LIVE/live.env" && echo "ignored: good"
```

## 1. The fidelity org

GitHub has no API for creating an organization on github.com, so this one
step is in the browser:

1. Open <https://github.com/account/organizations/new?plan=free> and create
   `$ORG` on the **Free** plan, owned by your personal account.
2. Leave the default member settings. Don't invite anyone.

Check it:

```sh
gh api "/orgs/$ORG" --jq '{login, plan: .plan.name}'   # plan: "free"
```

## 2. The repository and the team

```sh
gh repo create "$ORG/$REPO" --private --add-readme \
  --description "octoemu live fidelity sandbox"
gh api -X POST "/orgs/$ORG/teams" -f name="$TEAM" -f privacy=closed --jq .slug
```

The team's slug must equal `$TEAM`. If GitHub changed it (for example
because of capital letters), set `TEAM` to the printed slug.

## 3. The fidelity App

Register it from a manifest, so the permissions and events come out exactly
right. GitHub's manifest flow is a form POST from a browser, then one API
call:

```sh
# Set this to true if you will also do the policy or no-seat org (step 7):
# only a public App can be installed outside the org that owns it.
FIDELITY_PUBLIC=false

jq -n --arg org "$ORG" --argjson public "$FIDELITY_PUBLIC" '{
  name: "\($org)-app",
  url: "https://github.com/\($org)",
  description: "octoemu live fidelity suite (throwaway)",
  public: $public,
  redirect_url: "https://example.com/octoemu-fidelity",
  hook_attributes: {url: "https://example.com/octoemu-fidelity/webhook", active: true},
  default_permissions: {administration: "write", members: "write", metadata: "read"},
  default_events: ["member", "organization", "membership"]
}' > "$LIVE/fidelity-manifest.json"

cat > "$LIVE/register-fidelity-app.html" <<EOF
<form method="post" action="https://github.com/organizations/$ORG/settings/apps/new">
  <input type="hidden" name="manifest" value="$(jq -c . "$LIVE/fidelity-manifest.json" | jq -Rr @html)">
  <button type="submit">Register $ORG-app</button>
</form>
EOF
xdg-open "$LIVE/register-fidelity-app.html"     # macOS: open …
```

Click **Register**, then **Create GitHub App** on GitHub's page. GitHub
redirects to `https://example.com/octoemu-fidelity?code=…`. That page
doesn't exist, which is fine: copy the `code` from the address bar. It
expires after an hour.

```sh
CODE=paste-the-code-here
gh api -X POST "/app-manifests/$CODE/conversions" > "$LIVE/fidelity-app.json"
jq -r .pem "$LIVE/fidelity-app.json" > "$LIVE/fidelity-app.pem"
APP_ID=$(jq -r .id "$LIVE/fidelity-app.json")
APP_SLUG=$(jq -r .slug "$LIVE/fidelity-app.json")
APP_CLIENT_ID=$(jq -r .client_id "$LIVE/fidelity-app.json")
APP_WEBHOOK_SECRET=$(jq -r .webhook_secret "$LIVE/fidelity-app.json")
echo "$APP_ID $APP_SLUG"
```

The webhook goes to `example.com` on purpose. The deliveries fail, but
GitHub still records them, and `member-edited-changes` reads the payload
back through the App's deliveries API.

### Install it on the fidelity org

```sh
xdg-open "https://github.com/apps/$APP_SLUG/installations/new"
```

Choose `$ORG`, then **All repositories**, then **Install**. Then:

```sh
INSTALLATION_ID=$(gh api "/orgs/$ORG/installations" \
  --jq ".installations[] | select(.app_slug == \"$APP_SLUG\") | .id")
echo "$INSTALLATION_ID"
```

If that answers 403, the ID is also the last part of the URL GitHub shows
after installing: `…/organizations/$ORG/settings/installations/<ID>`.

## 4. The invitee

In a private browser window (so you stay signed in as the operator
elsewhere):

1. Sign up a new GitHub account `$INVITEE` and verify its email.
2. Don't join the fidelity org, and don't accept invitations by hand. The
   scenarios send and accept invitations themselves and clean them up.
3. Still signed in as the invitee, create a **classic** personal access
   token with the `repo`, `user` and `admin:org` scopes. This link
   pre-fills them:
   <https://github.com/settings/tokens/new?scopes=repo,user,admin:org&description=octoemu-fidelity>.
   Pick an expiry that covers your test period.

Check it from your terminal. The token goes in the env file only:

```sh
read -rs INVITEE_PAT && export INVITEE_PAT      # paste, then Enter
GH_TOKEN="$INVITEE_PAT" gh api /user --jq .login                       # = $INVITEE
GH_TOKEN="$INVITEE_PAT" gh api -i /user | grep -i '^x-oauth-scopes'    # admin:org, repo, user
gh api "/orgs/$ORG/members/$INVITEE" --silent 2>/dev/null \
  && echo "PROBLEM: the invitee is a member" || echo "not a member: good"
```

## 5. The sign-in App

This is a second App with no permissions. The suite only uses its OAuth
client. Register it the same way:

```sh
jq -n --arg org "$ORG" '{
  name: "\($org)-signin",
  url: "https://github.com/\($org)",
  description: "octoemu live fidelity suite: sign-in client (throwaway)",
  public: true,
  redirect_url: "https://example.com/octoemu-fidelity",
  callback_urls: ["http://127.0.0.1:8787/callback"],
  hook_attributes: {url: "https://example.com/octoemu-fidelity/webhook", active: false},
  default_permissions: {}
}' > "$LIVE/signin-manifest.json"

cat > "$LIVE/register-signin-app.html" <<EOF
<form method="post" action="https://github.com/organizations/$ORG/settings/apps/new">
  <input type="hidden" name="manifest" value="$(jq -c . "$LIVE/signin-manifest.json" | jq -Rr @html)">
  <button type="submit">Register $ORG-signin</button>
</form>
EOF
xdg-open "$LIVE/register-signin-app.html"
```

It's public so the invitee, who isn't in the org, can authorize it in step
6. With no permissions, that's harmless. Create it, copy the new `code`,
then:

```sh
CODE=paste-the-new-code-here
gh api -X POST "/app-manifests/$CODE/conversions" > "$LIVE/signin-app.json"
SIGNIN_SLUG=$(jq -r .slug "$LIVE/signin-app.json")
SIGNIN_CLIENT_ID=$(jq -r .client_id "$LIVE/signin-app.json")
SIGNIN_CLIENT_SECRET=$(jq -r .client_secret "$LIVE/signin-app.json")
```

It never needs installing. If you'll do step 6, open
`https://github.com/organizations/$ORG/settings/apps/$SIGNIN_SLUG`, tick
**Enable Device Flow** and save.

## 6. Optional: the invitee's user token

`user-permissionless` needs a user access token that only GitHub's consent
screen issues. The device flow gets one without a web server:

```sh
curl -s -X POST https://github.com/login/device/code \
  -H 'Accept: application/json' -d client_id="$SIGNIN_CLIENT_ID" > "$LIVE/device.json"
jq -r '"Open \(.verification_uri) as the invitee and enter \(.user_code)"' "$LIVE/device.json"
```

In the invitee's private window, open the link, enter the code and click
**Authorize**. Then:

```sh
USER_TOKEN=$(curl -s -X POST https://github.com/login/oauth/access_token \
  -H 'Accept: application/json' \
  -d client_id="$SIGNIN_CLIENT_ID" \
  -d device_code="$(jq -r .device_code "$LIVE/device.json")" \
  -d grant_type=urn:ietf:params:oauth:grant-type:device_code | jq -r .access_token)
GH_TOKEN="$USER_TOKEN" gh api /user --jq .login     # = $INVITEE
```

The token expires after 8 hours, so get it shortly before a run.

## 7. Optional: the policy and no-seat orgs

Both need the fidelity App to be public. Either register it with
`FIDELITY_PUBLIC=true`, or switch it in the App's settings: **Advanced** →
**Make public**.

**Policy org** (`outside-collaborator-policy`). Only GitHub Enterprise Cloud
can limit outside-collaborator invitations to owners
([GitHub docs](https://docs.github.com/en/enterprise-cloud@latest/organizations/managing-organization-settings/setting-permissions-for-adding-outside-collaborators)).

1. Create an organization `$POLICY_ORG` on an Enterprise Cloud trial.
2. Under **Settings → Member privileges**, untick **Allow repository
   administrators to invite outside collaborators**.
3. Create a repository, install the fidelity App on the org, and note the
   installation ID:

```sh
POLICY_ORG=yourname-fidelity-restricted
POLICY_REPO=vault
gh repo create "$POLICY_ORG/$POLICY_REPO" --private --add-readme
xdg-open "https://github.com/apps/$APP_SLUG/installations/new"   # choose $POLICY_ORG
POLICY_INSTALLATION_ID=$(gh api "/orgs/$POLICY_ORG/installations" \
  --jq ".installations[] | select(.app_slug == \"$APP_SLUG\") | .id")
```

**No-seat org** (`org-no-seat`):

1. Create an organization `$NO_SEAT_ORG` on the **Team** plan with exactly
   one seat. The operator fills it.
2. Install the fidelity App on it.

The scenario's invitation is refused for want of a seat, so you aren't
charged for another one.

```sh
NO_SEAT_ORG=yourname-fidelity-full
xdg-open "https://github.com/apps/$APP_SLUG/installations/new"   # choose $NO_SEAT_ORG
NO_SEAT_INSTALLATION_ID=$(gh api "/orgs/$NO_SEAT_ORG/installations" \
  --jq ".installations[] | select(.app_slug == \"$APP_SLUG\") | .id")
```

## 8. Write the env file

```sh
{
  echo "OCTOEMU_LIVE_APP_ID=$APP_ID"
  echo "OCTOEMU_LIVE_APP_CLIENT_ID=$APP_CLIENT_ID"
  echo "OCTOEMU_LIVE_APP_PRIVATE_KEY_PATH=fidelity-app.pem"
  echo "OCTOEMU_LIVE_APP_WEBHOOK_SECRET=$APP_WEBHOOK_SECRET"
  echo "OCTOEMU_LIVE_INSTALLATION_ID=$INSTALLATION_ID"
  echo "OCTOEMU_LIVE_ORG=$ORG"
  echo "OCTOEMU_LIVE_REPO=$REPO"
  echo "OCTOEMU_LIVE_TEAM=$TEAM"
  echo "OCTOEMU_LIVE_INVITEE_LOGIN=$INVITEE"
  echo "OCTOEMU_LIVE_INVITEE_PAT=$INVITEE_PAT"
  echo "OCTOEMU_LIVE_SIGNIN_CLIENT_ID=$SIGNIN_CLIENT_ID"
  echo "OCTOEMU_LIVE_SIGNIN_CLIENT_SECRET=$SIGNIN_CLIENT_SECRET"
  [ -n "${USER_TOKEN:-}" ] && echo "OCTOEMU_LIVE_USER_TOKEN=$USER_TOKEN"
  [ -n "${POLICY_INSTALLATION_ID:-}" ] && printf '%s\n' \
    "OCTOEMU_LIVE_POLICY_INSTALLATION_ID=$POLICY_INSTALLATION_ID" \
    "OCTOEMU_LIVE_POLICY_ORG=$POLICY_ORG" "OCTOEMU_LIVE_POLICY_REPO=$POLICY_REPO"
  [ -n "${NO_SEAT_INSTALLATION_ID:-}" ] && printf '%s\n' \
    "OCTOEMU_LIVE_NO_SEAT_INSTALLATION_ID=$NO_SEAT_INSTALLATION_ID" \
    "OCTOEMU_LIVE_NO_SEAT_ORG=$NO_SEAT_ORG"
  true
} > "$LIVE/live.env"
rm -f "$LIVE"/*-app.json "$LIVE"/*.html "$LIVE"/*-manifest.json "$LIVE/device.json"
git status --short .octoemu-live    # prints nothing: the directory is ignored
```

The key path is relative to `live.env`. The full format, with every key, is
in the octoemu README under **The live env file**. Later edits (a new user
token, a reinstalled App's new installation ID) go straight into
`$LIVE/live.env`.

## 9. Run it

From `libs/octoauth`, in the dev shell. The suite finds
`.octoemu-live/live.env` itself; set `OCTOEMU_LIVE_ENV` to use another
file.

```sh
cd libs/octoauth
# Smoke test: one harmless scenario.
OCTOEMU_CONFORMANCE_ONLY=jwt-error-matrix \
  cargo test -p octoemu-conformance --test live --locked -- --ignored --nocapture

# First real run: record GitHub's answers as the snapshots.
OCTOEMU_CONFORMANCE=record \
  cargo test -p octoemu-conformance --test live --locked -- --ignored --nocapture

# Later runs: compare GitHub against the recorded snapshots.
cargo test -p octoemu-conformance --test live --locked -- --ignored --nocapture
```

A record run against api.github.com rewrites
`crates/octoemu-conformance/snapshots/*.json` with `"source": "live"`.
Snapshots are normalised: IDs, names, tokens and timestamps become
placeholders. Check `git diff` for anything that looks like a real value
before you commit them in a PR. Then run the emulator side
(`cargo test -p octoemu-conformance --test emulator`). Each difference it
reports is either an emulator bug to fix or a `fidelity.toml` entry to
correct.

`OCTOEMU_CONFORMANCE_REPORT=/some/path.json` also writes the run's report
as JSON.

### Destructive and slow scenarios

These are off by default:

```sh
OCTOEMU_LIVE_DESTRUCTIVE=1 OCTOEMU_CONFORMANCE=record \
  cargo test -p octoemu-conformance --test live --locked -- --ignored --nocapture
```

Run them only on the throwaway org, after a normal run, and at most once a
day:

- `repo-invitation-limit` uses up the repository's 50 invitations for the
  day.
- `org-invitation-limit` uses up the organization's invitation budget.
- `secondary-limit-shape` runs last but one and leaves the App rate-limited
  for a while.
- `token-revocation-uninstall` runs last. The App uninstalls itself.
  Reinstall it (step 3) and put the new installation ID in `live.env`.

`OCTOEMU_LIVE_SLOW=1` lets scenarios wait in real time, up to 15 minutes.
`expired-invitation` needs 7 days, so it stays emulator-only. The live run
skips it either way.

## 10. Optional: run it from GitHub Actions

`.github/workflows/fidelity-live.yml` (manual only) builds the env file
from repository secrets on the runner and runs the same test. Whether to
keep these credentials as repository secrets at all is an open decision;
local runs need none of this.

```sh
R=sagikazarmark/ghinvite2.orig
# Every key except the key path becomes a secret of the same name …
grep -v '^OCTOEMU_LIVE_APP_PRIVATE_KEY_PATH=' "$LIVE/live.env" | gh secret set --repo "$R" -f -
# … and the private key goes in whole.
gh secret set OCTOEMU_LIVE_APP_PRIVATE_KEY --repo "$R" < "$LIVE/fidelity-app.pem"

gh workflow run fidelity-live.yml --repo "$R" -f mode=record -f destructive=false -f slow=false
gh run watch --repo "$R" "$(gh run list --repo "$R" --workflow fidelity-live.yml --limit 1 --json databaseId --jq '.[0].databaseId')"
gh run download --repo "$R" <run-id>       # the report and the recorded snapshots
```

Remember that a reinstall changes the installation ID, so update
`OCTOEMU_LIVE_INSTALLATION_ID` there too. Remove the secrets with
`gh secret delete <NAME> --repo "$R"`.

## 11. Tearing down

Do this when you're finished, or to rotate a leaked value:

1. **Invitee's token.** As the invitee, delete it at
   <https://github.com/settings/tokens>. For the user token, revoke the
   sign-in App at <https://github.com/settings/apps/authorizations>.
2. **The Apps.** Delete both under
   `https://github.com/organizations/$ORG/settings/apps/<slug>/advanced`.
   This also invalidates the private key, the client secrets and every
   token. To rotate without deleting, generate a new private key or client
   secret on the App's settings page instead.
3. **The organizations.** Delete them under **Settings → Danger zone**.
   Cancel the paid plan or the trial first.
4. **Local files.** `rm -rf "$LIVE"`, and the repository secrets if you
   set any.
