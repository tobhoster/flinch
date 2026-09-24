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

- **The UI needs a token.** `flinch-web` refuses every API request without
  `Authorization: Bearer <FLINCH_WEB_TOKEN>`, and refuses everything when no
  token is set. The token grants everything the UI can do, including turning
  on Enforcement, so treat it like an *arr API key. Serve the UI over HTTPS,
  and never publish it to the internet.
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
