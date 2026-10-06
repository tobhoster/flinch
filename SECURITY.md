# Security

FLINCH decides which media gets deleted, and it holds your Plex token and your
*arr API keys. Please report problems privately.

## Reporting a vulnerability

Use **[Report a vulnerability](https://github.com/tobhoster/flinch/security/advisories/new)**
(GitHub's private vulnerability reporting), not a public issue. Say what an
attacker needs (where on the network, which settings) and what they get. You
will get an answer within a week. Fixes ship in a new release, with credit if
you want it.

## Supported versions

Only the latest release gets fixes.

## How FLINCH protects itself

- **The UI needs a login; automations need the API key.** `flinch-web`
  refuses every API request without a session from logging in
  (`FLINCH_WEB_USERNAME`, `FLINCH_WEB_PASSWORD`) or the API key
  (`FLINCH_WEB_TOKEN`, as `X-Api-Key` or `Authorization: Bearer`), and refuses
  everything when neither is set. Both grant everything the UI can do,
  including turning off dry run, so treat them like an *arr API key. Serve
  the UI over HTTPS, and never publish it to the internet. Upgrading from the
  token alone: see
  [Upgrading from the access token](deploy/README.md#upgrading-from-the-access-token).
- **Sessions are guarded.** The session cookie is `HttpOnly`,
  `SameSite=Strict`, and `Secure` behind an HTTPS ingress, and no cache may
  keep an answer behind the login. A login, a logout and every write made with
  the cookie need the `X-Flinch-Request: 1` header, which a page on another
  site cannot add, so no other site can guess the password through your
  browser or log you out. Five failed logins in a row pause logging in, for up
  to 15 minutes. Sessions live in memory: restarting `flinch-web` ends them
  all, and clears the pause.
- **The Plex URL and token are one credential.** They always come from the same
  place, the Secret or Settings, and a changed URL needs the token again, so a
  saved token is never sent to a new address. It is never sent back to the
  browser.
- **Keys stay with their host.** The daemon follows no HTTP redirects, and the
  Plex token travels in a header, never in a URL.
- **State is private.** Files on the state volume are readable only by the pods'
  user; `settings.json` can hold the Plex token.
- **Least privilege.** Both pods run as a non-root user with a read-only root
  filesystem, no capabilities, no privilege escalation, the runtime's seccomp
  profile and no Kubernetes API token.
- **FLINCH never deletes a file itself** and never writes to Radarr or Sonarr.
  Deletions happen in Maintainerr, on Maintainerr's schedule, and anything
  FLINCH cannot match or measure is kept.

Out of scope: someone who can already read the Secret or the state volume, run
code in the pods, or reach Maintainerr and the *arr apps directly.

## Checked on every change

Nothing reaches `main` without a pull request that passes the tests, CodeQL,
and the [Security workflow](.github/workflows/security.yml), which also runs
every week:

- gitleaks over the whole history;
- cargo-deny: RustSec advisories, licenses and sources of every crate;
- npm audit of the UI's packages, and their registry signatures;
- actionlint and zizmor over the workflows, whose actions are pinned by SHA;
- Trivy over the image, the Dockerfile and the manifests, then the image run
  as the manifests run it, checking the login, the API key and the security
  headers.

Release tags cannot be moved or deleted, and each release image carries a
build provenance attestation and an SBOM.
