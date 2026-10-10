# Deploying FLINCH

FLINCH runs as two pods that share one volume:

- `flinch-arrd`, the daemon. Each cycle it reads the inventory from Radarr and
  Sonarr, the watch evidence from Plex and Tautulli, and the optional signals
  from Seerr, Prowlarr and SABnzbd. It forecasts each disk, plans, writes the
  plan to `eviction-plan.json`, and syncs it to Maintainerr when dry run is
  off: selected items join a Maintainerr collection, and Maintainerr deletes
  them on its own schedule. Both are addressed by Plex ratingKey, so an item
  whose Plex GUID join failed is left alone. It has no Service or Ingress: it
  only calls out.
- `flinch-web`, the UI and its JSON API on port 7911. It reads the state the
  daemon writes and writes `settings.json` when you save Settings. People log
  in with `FLINCH_WEB_USERNAME` and `FLINCH_WEB_PASSWORD`; automations send the
  API key in `FLINCH_WEB_TOKEN`.

| Path | What it is |
| --- | --- |
| `deploy/kustomization.yaml` | what you configure: namespace, image, URLs |
| `deploy/base/` | the manifests: state volume, ConfigMap, both Deployments, Service |
| `deploy/ingress/` | optional: an HTTPS Ingress for the UI |
| `deploy/Dockerfile` | the image |

## Try the demo first

The demo fills a state directory with made-up titles and serves the UI on
<http://localhost:7911>. No cluster and no *arr apps are needed.

With the published image:

```bash
docker run --rm -p 127.0.0.1:7911:7911 -e FLINCH_STATE_DIR=/tmp/demo \
  -e FLINCH_WEB_USERNAME=demo -e FLINCH_WEB_PASSWORD=demo \
  ghcr.io/tobhoster/flinch:0.3.0 sh -c 'flinch-demo && flinch-web'
```

Log in as `demo` with the password `demo`.

From source:

```bash
cd frontend && npm ci && npm run build && cd ..
FLINCH_STATE_DIR=/tmp/flinch-demo cargo run -p flinch-web --bin flinch-demo
FLINCH_STATE_DIR=/tmp/flinch-demo FLINCH_WEB_DIR=frontend/dist \
  FLINCH_WEB_USERNAME=demo FLINCH_WEB_PASSWORD=demo cargo run -p flinch-web --bin flinch-web
```

## Requirements

- Radarr and Sonarr. They supply the inventory; their API keys are required.
- Maintainerr 3.10 or newer. FLINCH hands evictions to it. Nothing is handed
  to an older version.
- Plex. It supplies item-level watch state and the ratingKey Maintainerr
  needs. Without Plex every item is held. With the native executor FLINCH
  also keeps its own Leaving Soon collection in Plex and promotes it to each
  section's recommended and home hubs (owner and shared users); that needs
  the server owner's token, since Plex answers 200 to a managed user's
  collection writes and ignores them. Every write is read back, so a wrong
  token shows as "write not applied" in the daemon log, never as a silent
  success. Plex cannot create an empty collection: it appears with its first
  warned item.
- Tautulli, optional. It keeps every play with the user who played it, so
  FLINCH sees plays Plex no longer reports.
- Seerr, Prowlarr and SABnzbd, optional. Seerr's requests and watchlists,
  Prowlarr's seeders and SABnzbd's retention feed the
  [regret](../docs/how-it-works.md#regret) of each item.
- A Kubernetes cluster and `kubectl` (kustomize is built in).
- A storage class that supports ReadWriteMany (NFS, Longhorn, CephFS, ...).
  With ReadWriteOnce only, both pods must run on the same node; see
  `deploy/base/flinch-state.yaml`.

## The image

`ghcr.io/tobhoster/flinch` is built by this repository's CI for amd64 and
arm64, with a build provenance attestation and an SBOM. One image carries the
binaries (`flinch-arrd`, `flinch-fit`, `flinch-web`, `flinch-demo`) and the
built UI; the two Deployments differ only in `command:`. It is about 50 MB
(Rust and Alpine; no GPU, no Python) and runs as uid 1000.

| Tag | What it is |
| --- | --- |
| `0.3.0` | a release; `deploy/kustomization.yaml` pins one |
| `0.1`, `latest` | the newest release of that line, or overall |
| `edge` | the main branch |
| `sha-<commit>` | one build of main |

### Build your own

From the repository root:

```bash
docker build -t registry.example.com/flinch:0.3.0 -f deploy/Dockerfile .
docker push registry.example.com/flinch:0.3.0
```

Then point `images:` in `deploy/kustomization.yaml` at it. The manifests use
`imagePullPolicy: Always`, so a tag you push again (like `latest`) is never
served stale by a node that already holds it.

## Configure

Edit `deploy/kustomization.yaml`:

- `namespace:` is `media` by default. Use the namespace Radarr, Sonarr and
  Maintainerr run in.
- `images:` pins the published release. To run your own build, set `newName`
  and `newTag` to what you pushed.
- URLs. The daemon reaches `http://radarr:7878`, `http://sonarr:8989` and
  `http://maintainerr:6246`, which works when those services are in the same
  namespace. To change them, uncomment the `patches:` block in the same file
  and edit it.
- The Ingress, if you want one: see [Reach the UI](#reach-the-ui).

To keep these edits out of git, put them in an overlay instead; see
[Keeping your own values out of git](#keeping-your-own-values-out-of-git).

Then create the Secret in the same namespace. Every key goes into this one
Secret, `flinch-secrets`. Write the keys to a file only you can read, not on a
command line, where they would stay in your shell history:

```bash
install -m 600 /dev/null flinch.env                              # readable only by you
echo "FLINCH_WEB_TOKEN=$(openssl rand -hex 32)" >> flinch.env   # the API key, for automations
$EDITOR flinch.env
kubectl -n media create secret generic flinch-secrets --from-env-file=flinch.env
rm flinch.env
```

with one `KEY=value` per line, and no quotes around a value: `--from-env-file`
keeps them as part of it, so a quoted password would include the quotes:

```
FLINCH_WEB_USERNAME=<your choice>
FLINCH_WEB_PASSWORD=<a strong password>
FLINCH_WEB_TOKEN=<generated above>
RADARR_API_KEY=<radarr api key>
SONARR_API_KEY=<sonarr api key>
MAINTAINERR_API_KEY=<maintainerr api key>
SEERR_URL=http://seerr:5055
SEERR_API_KEY=<seerr api key>
PROWLARR_URL=http://prowlarr:9696
PROWLARR_API_KEY=<prowlarr api key>
SABNZBD_URL=http://sabnzbd:8080
SABNZBD_API_KEY=<sabnzbd api key>
TMDB_API_KEY=<tmdb v3 api key or v4 read token>
PLEX_URL=http://plex:32400
PLEX_TOKEN=<plex token>
TAUTULLI_URL=http://tautulli:8181
TAUTULLI_API_KEY=<tautulli api key>
```

| Key | Needed | Notes |
| --- | --- | --- |
| `FLINCH_WEB_USERNAME`, `FLINCH_WEB_PASSWORD` | yes, for the UI | the UI's login. The username matches exactly, case included; spaces around either value are ignored. Without both, the UI says how to set them |
| `FLINCH_WEB_TOKEN` | no | the API key for automations (Home Assistant, n8n), sent as `X-Api-Key` or `Authorization: Bearer`: any long random string. With neither it nor a login, the API refuses everything |
| `FLINCH_WEB_OIDC_ISSUER`, `FLINCH_WEB_OIDC_CLIENT_ID`, `FLINCH_WEB_OIDC_REDIRECT_URL` | no | single sign-on through your OpenID Connect provider, beside or instead of the password login: the provider's issuer exactly as its discovery document states it, FLINCH's client id there, and `https://<flinch host>/api/oidc/callback`. All three or none. See [Single sign-on](#single-sign-on-openid-connect) |
| `FLINCH_WEB_OIDC_CLIENT_SECRET` | no | the client's secret; leave it out for a public client (PKCE alone) |
| `FLINCH_WEB_OIDC_ALLOWED_EMAILS`, `FLINCH_WEB_OIDC_ALLOWED_GROUPS`, `FLINCH_WEB_OIDC_ALLOWED_SUBJECTS` | with SSO | who may sign in, comma-separated; any one match is enough. Emails count only when the provider marks them verified. With all three empty every sign-in is refused |
| `FLINCH_WEB_OIDC_NAME`, `FLINCH_WEB_OIDC_GROUPS_CLAIM` | no | the button's label ("Sign in with Authelia"; default "single sign-on") and the claim holding groups (default `groups`) |
| `RADARR_API_KEY` | yes | Radarr > Settings > General |
| `SONARR_API_KEY` | yes | Sonarr > Settings > General |
| `MAINTAINERR_API_KEY` | no | Maintainerr checks no key today; FLINCH sends one when it is set |
| `PLEX_URL`, `PLEX_TOKEN` | no | or set both in the UI (Settings > Plex), which then wins over the Secret; the pair always comes from one place |
| `JELLYFIN_API_KEY` | no | a Jellyfin or Emby admin API key (Dashboard > API Keys). Set the server in Settings > Jellyfin / Emby with key variable `FLINCH_JELLYFIN_KEY`, or type the key there instead. Every user's played state is read; Leaving Soon and collections stay in Plex |
| `TAUTULLI_URL`, `TAUTULLI_API_KEY` | no | without them FLINCH reads Tautulli's address from Maintainerr, but Maintainerr returns the key masked, so set them here to use Tautulli |
| `SEERR_URL`, `SEERR_API_KEY` | no | Overseerr or Jellyseerr (Settings > General): requests and watchlists raise an item's regret; with the native executor, a deleted title's request is cleared so it can be requested again |
| `PROWLARR_URL`, `PROWLARR_API_KEY` | no | Prowlarr (Settings > General): seeders, for how hard a title is to download again |
| `SABNZBD_URL`, `SABNZBD_API_KEY` | no | SABnzbd (Config > General): the servers' retention |
| `TMDB_API_KEY` | no | a TMDB API key (themoviedb.org > Settings > API) for streaming availability: switch it on in Settings > Streaming with your region and the TMDB ids of the services you subscribe to. A title on one of them is cheaper to lose. Data by JustWatch; flinch-arrd must reach `api.themoviedb.org` |
| `DISCORD_WEBHOOK_URL`, `NOTIFY_URL`, `NTFY_TOKEN` | no | notification secrets, seen by flinch-arrd as `FLINCH_DISCORD_URL`, `FLINCH_NOTIFY_URL` and `FLINCH_NTFY_TOKEN`: name those variables in Settings > Notifications. See [Notifications](#notifications) |
| `FLINCH_WEB_LINK_SECRET` | with household links | 32+ random characters (`openssl rand -hex 32`), mapped into both flinch-arrd (signs links) and flinch-web (verifies them). Without it Settings > Household makes no links. See [Household](#household-keep-links-removal-requests-requester-messages) |
| `QBITTORRENT_PASSWORD`, `TRANSMISSION_PASSWORD` | no | torrent client passwords, seen by flinch-arrd as `FLINCH_QBIT_PASSWORD` and `FLINCH_TRANSMISSION_PASSWORD`: name those variables in Settings > Torrents. See [Torrents](#torrents) |
| `TRACEARR_API_KEY` | no | a Tracearr public API key (generated in Tracearr's Settings, `trr_pub_…`), seen by flinch-arrd as `FLINCH_TRACEARR_KEY`: add a Tracearr source in Settings > Watch sources with that variable. See [Watch sources](#watch-sources-tracearr-and-trakt) |
| `TRAKT_CLIENT_ID`, `TRAKT_TOKEN_<NAME>` | no | a Trakt application's client id (mapped to `FLINCH_TRAKT_CLIENT_ID`) and one OAuth access token per household member: add a variable per member to the flinch-arrd manifest the same way, e.g. `FLINCH_TRAKT_ANN` from `TRAKT_TOKEN_ANN`. See [Watch sources](#watch-sources-tracearr-and-trakt) |

Leave out the keys you do not use. The Radarr and Sonarr keys are required
(the daemon exits without them), and the UI stays locked without the login.
The daemon also takes the optional three as `--seerr-url`, `--seerr-key`,
`--prowlarr-url`, `--prowlarr-key`, `--sabnzbd-url` and `--sabnzbd-key`. What
a missing one changes: see [Regret](../docs/how-it-works.md#regret).

### Notifications

Optional, off until you add a channel in Settings > Notifications: Discord,
ntfy, Apprise API or a JSON webhook, each told about the kinds you tick —
titles entering Leaving Soon (with a Keep link), deletions, problems seen
three runs in a row, and a daily digest. What is sent and when: see
[Notifications](../docs/how-it-works.md#notifications-telling-the-household-once).

1. Put each URL that holds a secret in `flinch-secrets` (a Discord webhook
   URL always does): `DISCORD_WEBHOOK_URL`, or `NOTIFY_URL` for any other
   channel, and `NTFY_TOKEN` for an ntfy access token (`tk_…`). The
   flinch-arrd manifest maps them to `FLINCH_DISCORD_URL`,
   `FLINCH_NOTIFY_URL` and `FLINCH_NTFY_TOKEN`; for more channels add more
   variables the same way. Restart flinch-arrd after changing the Secret.
2. In Settings > Notifications add a channel, name its URL variable (or type
   a URL without a secret, such as an internal ntfy topic — that one is stored
   in `settings.json` and shown on the page), tick its events, and set the
   FLINCH address the Keep links open (e.g. `https://flinch.example.com`).
   Save.
3. Press **Send test**. flinch-web holds no notification secret, so the
   daemon posts the test (within a few seconds, also mid-run) and the page
   shows each channel's answer. A channel named *not set* means the variable
   is missing from flinch-arrd's environment.

URLs by kind: Discord `https://discord.com/api/webhooks/<id>/<token>`; ntfy
the topic URL, `https://ntfy.sh/<topic>` (FLINCH publishes JSON to the server
root); Apprise the notify endpoint of a saved configuration,
`http://apprise:8000/notify/<key>`; a webhook any URL that takes a JSON POST.
Each channel gets at most the hourly limit (default 12 messages); the rest
wait for a later run. Notifications are sent in a dry run too: only real
hand-overs and deletions produce Leaving Soon and deletion messages.

### Household: keep links, removal requests, requester messages

Optional, off until you switch it on in Settings > Household. What each
switch does: see
[The household](../docs/how-it-works.md#the-household-keep-links-removal-requests-requester-messages-off-by-default).

1. Add `FLINCH_WEB_LINK_SECRET` to `flinch-secrets` (`openssl rand -hex 32`);
   both manifests map it. Restart flinch-arrd and flinch-web. Rotating it
   kills every link already sent.
2. Set the FLINCH address in Settings > Notifications: links are
   `<address>/r/<token>`, and `/r/` must be reachable by the household
   (it needs no login; keep the rest of FLINCH behind yours).
3. Settings > Household: turn on the links; optionally *Tell requesters* and
   the newsletter, the ntfy server and Apprise API for per-person addresses,
   and recipients (a Discord id to mention, an ntfy topic, an Apprise URL
   variable, or an email). Secrets in an address go in an environment
   variable of flinch-arrd, added the same way as the notification ones; for
   email, an Apprise `mailtos://user:password@smtp.example.com?from=…` URL in
   the variable you name. Tick *Newsletter* on the channels that should get it.

Removal requests wait in the Overview's *Removal requests* card until you
approve or deny them. With the native executor, each Plex Leaving Soon
shelf's summary lists its titles with keep links (in a dry run the summary
write is printed, not sent).

### Torrents

Optional, off until you add a client in Settings > Torrents. FLINCH then
keeps an item while one of its torrents is still seeding toward its goal, and
counts an item's size only when deleting it frees the bytes. What it reads
and decides: see [Torrents](../docs/how-it-works.md#torrents-seed-goals-and-hardlinks).

1. If the client asks for a password, put it in `flinch-secrets` as
   `QBITTORRENT_PASSWORD` or `TRANSMISSION_PASSWORD` (the manifest maps them
   to `FLINCH_QBIT_PASSWORD` and `FLINCH_TRANSMISSION_PASSWORD`; add more
   variables the same way for more clients). Restart flinch-arrd.
2. In Settings > Torrents add the client: qBittorrent's Web UI address
   (`http://qbittorrent:8080`), or Transmission's (`http://transmission:9091`,
   which gets `/transmission/rpc`; give the full RPC URL if yours differs),
   the username, and the password's variable name. A qBittorrent that skips
   authentication for flinch-arrd's address needs neither. Save.
3. Set the seed goal FLINCH adds to each client's own share limits: a
   torrent has met its goal at the client's limit, at the minimum ratio
   (default 1.0), or after the minimum days (default 14). Set both to 0 to
   rely on the clients' limits alone. Optionally set a **Desired ratio**
   above the goal: an item whose torrent is below it is not kept, only taken
   last, once nothing else on its disk fills the target (0, the default, is
   off).
4. For the hardlink check, flinch-arrd must see both the torrents' files and
   the library. Mount the share that holds both (one mount, so a hardlink
   reads as one file) read-only into the flinch-arrd container, and map each
   path the clients and the *arrs report to where it is mounted under
   Settings > Torrents > Path map, e.g. `/data/torrents → /mnt/media/torrents`
   and `/data/media → /mnt/media/media`. Do not mount over `/data`, which is
   the daemon's scratch space. Without the mounts every torrent-held item
   reads as *unverified* and is kept, unless the native executor removes its
   torrents with it.

Settings > Torrents > Status reads back each client and what its torrents
kept on the last run. FLINCH writes to a client only to remove a torrent
after the native executor deleted its item (Remove after delete, on by
default; never with Maintainerr), only once the torrent met its goal, never
one that also holds an item that stays, and not at all in a dry run.

### Watch sources (Tracearr and Trakt)

Optional, none by default, in Settings > Watch sources. Each source's plays
join the library by TMDB, TVDB or IMDb id and count like Tautulli streams;
FLINCH only reads them. What they decide: see
[Integrations](../docs/how-it-works.md#integrations-by-identity--never-by-title).

1. **Tracearr.** Create a public API key in Tracearr, put it in
   `flinch-secrets` as `TRACEARR_API_KEY` and map it to `FLINCH_TRACEARR_KEY`
   in the flinch-arrd manifest. Add a Tracearr source with its address
   (`http://tracearr:3000`) and token variable `FLINCH_TRACEARR_KEY`. Every
   user's whole history is read each run. If you prune Tracearr's history by
   hand, set **History kept** to the days you keep, so older silence never
   counts as "never played".
2. **Trakt.** Create an application at <https://trakt.tv/oauth/applications>
   (redirect URI `urn:ietf:wg:oauth:2.0:oob`). For each household member run
   Trakt's device flow once — `POST https://api.trakt.tv/oauth/device/code`
   with the client id, open the shown URL and enter the code, then poll
   `POST https://api.trakt.tv/oauth/device/token` — and store the
   `access_token` in `flinch-secrets` (e.g. `TRAKT_TOKEN_ANN`), mapped to
   `FLINCH_TRAKT_ANN`; the client id goes in as `TRAKT_CLIENT_ID` →
   `FLINCH_TRAKT_CLIENT_ID`. Add one Trakt source per member with its name,
   token variable and client id variable; leave the URL blank. flinch-arrd
   needs HTTPS out to `api.trakt.tv`. FLINCH does not refresh tokens: when one
   expires, run the device flow again.
3. Restart flinch-arrd after changing the Secret, and save.

Settings > Watch sources > Status reads back each source: complete or not,
its plays, the items they joined, and (Tracearr) the items nobody played. A
source that fails holds never-played reclaim off until it reads in full.

### Rules

Optional, none by default, in Settings > Rules; nothing goes into the Secret.
A rule names a scope (kind, root folder, disk, *arr tag, Plex section id, Seerr
requester, theme, genre, quality, size, days on disk, played, days since the
last play, P(watch)) and an effect: **Keep**, **Keep until** N days after the
item was added, last played or requested, **Keep the newest seasons** (N) of a
continuing show, **Keep the first season**, **Prefer evict** or **Must
evict**. The two retention effects let the other seasons of the show go
around the kept ones, which a plain Keep would block.
Rules decide only whether an item may or must go, never its regret. What they
can and cannot override: see
[Rules](../docs/how-it-works.md#rules-hard-constraints-never-a-score).

1. Add a rule, set its conditions and effect. **YAML** shows the whole list as
   `settings.json` will hold it (under `rules`), for review or a copy.
2. Press **Preview changes**. flinch-web plans the daemon's last inputs
   (`state/plan-inputs.json`) under the saved rules and under the draft, and
   lists what would leave, what would stay, each rule's items and bytes, items
   kept only for a missing fact, and conflicts. Before the daemon's first run
   with this version there are no inputs to preview.
3. Save. A changed rule list saves only once that exact list was previewed.

Settings > Rules then reads `status.json` `rules` back: per rule its items and
bytes, items forced, kept for a missing fact, and the first 50 conflicts.

**Ignored viewers** (same page) lists names whose plays count as no play: a
guest, a kid's profile, your own test plays. A name matches a Plex account,
a Tautulli user, a Jellyfin/Emby user, a Tracearr username or a Trakt source's
name. Evidence health stays as read, so it never makes a partial record look
complete; Plex's own item state of the token's account still counts.

**Inflow actions** (same page, off by default) acts on the inflow advice you
approve, only while a disk is over its target: tick shows whose future
seasons Sonarr may stop fetching, and import lists (listed after the first
run with it on) whose automatic add goes off until no disk is over target.
It writes with the Radarr and Sonarr keys already configured; a dry run only
prints each write. What it changed is kept in `state/inflow-actions.json`.

### Quality actions and upgrade churn

Optional, in Settings > Quality; nothing new goes into the Secret. With
**Downgrades** on, an item advised a downgrade moves to the compact quality
profile and Radarr or Sonarr is asked to search, at most `max_per_day` items
in 24 hours (default 3) and only when Prowlarr lists a release at least 30%
smaller (needs `PROWLARR_URL`/`PROWLARR_API_KEY`). The compact profile is the
one FLINCH's quality sync manages; without it, name the profile in each app
under **Compact profile**, and make sure it does not list the qualities you
want to shed (a Recyclarr "WEB-1080p" profile does not list 2160p). A dry run
prints the moves; `state/quality-actions.json` records each live one and
whether a smaller file landed.

**Upgrade churn** is on by default and only flags items grabbed more than the
limit in 30 days (default 5). **On churn** can instead unmonitor such an item,
or turn upgrades off on its quality profile (every item on that profile; a
profile the quality sync manages is left alone). See
[Quality advice](../docs/how-it-works.md#quality-advice-keep-downgrade-or-evict).

### Duplicates

Optional, in Settings > Duplicates; nothing new goes into the Secret. **Finder**
lists movies held in several copies on the Overview, each with the copy to
keep, and folders under the Radarr and Sonarr roots that no item owns. Choose
a copy there, then confirm it. With **Remove** on, a confirmed choice removes
the other copies: a Plex version through Plex, which needs Plex's Settings >
Library > "Allow media deletion" and the server owner's token, a second
Radarr's file through that Radarr. A dry run prints each removal. Choices live
in `state/dupes.json`, outcomes in `state/dupes-acted.json`. See
[Duplicates](../docs/how-it-works.md#duplicates-one-copy-is-enough-off-by-default).

### Archive tier (move instead of delete)

Optional, in Settings > Archive, and off by default; nothing new goes into the
Secret (it uses `RADARR_API_KEY`/`SONARR_API_KEY`).

1. Mount the archive disk into Radarr and Sonarr and add the archive folders
   (e.g. `/archive/movies`, `/archive/tv`) as **root folders** there. FLINCH
   measures the disk through the apps' disk report (or, with
   `FLINCH_LIBRARY_PREFIX`, its own mount, see
   [Disks the *arrs do not report](#disks-the-arrs-do-not-report)); a root it
   cannot measure archives nothing and is named in Settings > Archive.
2. Add the same folders to the matching Plex or Jellyfin library, so an
   archived item stays playable.
3. Enter the roots in Settings > Archive, as the *arrs see them, and switch the
   tier on. A blank root keeps that app from archiving.

The archive disk's own forecast sets how much it may take. Planned moves wait
for the grace runs, then at most **Moves per run** are sent through the *arrs'
editor with `moveFiles: true` and read back at the new path. A dry run prints
each move. See
[The archive tier](../docs/how-it-works.md#the-archive-tier-move-instead-of-delete-off-by-default).

### Quality profiles (TRaSH sync)

Optional, in Settings > Quality profiles (TRaSH), and off by default; nothing
new goes into the Secret (it uses `RADARR_API_KEY`/`SONARR_API_KEY`). It does
Recyclarr's job: custom formats, quality profiles with their scores, and
quality sizes from [TRaSH-Guides](https://trash-guides.info) at a pinned
commit. Switch on **Preview the TRaSH sync**, save, and the next run lists
every change on the **Quality profiles** tab; select changes and press
**Apply**, and the run after applies them (with dry run on it only prints
them). **Apply automatically** applies every change on the schedule instead.

Each app's config is JSON in Recyclarr's field names, starting from FLINCH's
preset (Radarr *Remux 2160p (Combined)* capped at WEB 2160p and *WEB 1080p*
as the compact profile; Sonarr *WEB-2160p (Combined)* and *WEB-1080p*; sizes
with `preferred_ratio` 0.2), for example a score override:

```json
"custom_formats": [{ "trash_id": "e7718d7a3ce595f289bfee26adc178f5", "score": 0, "profiles": [] }]
```

For a relative change, `"adjust_score": 20` moves the guide's score instead,
and a profile's `"score_multiplier": 0.5` scales all of its guide scores.
`"language": { "prefer": "english", "fallback": true }` adds TRaSH's language
formats to every profile of the instance. Each profile and size change on the
tab shows its GiB estimate (items on the profile × cached Prowlarr release
sizes) and the disk forecast before and after.

FLINCH adopts a custom format or profile of the same name (one Recyclarr made,
say), deletes only custom formats it created and no synced profile uses
(**Offer to delete custom formats FLINCH did not create** widens that), and
deletes a profile only with **Offer to delete unused profiles** on, when no
movie or series uses it and you select it. If Recyclarr keeps syncing the same profiles, leave
this off: the two would undo each other. The first preview downloads about
550 small files from `api.github.com` and `raw.githubusercontent.com`, once
per guide commit, so flinch-arrd needs HTTPS out to both while it is on.

An instance with `"source": "pcd"` reads a Profilarr Compliant Database
instead (default: the Dictionarry database, MIT per its `pcd.json`; one
without a declared license is refused), about 370 SQL files from the same two
hosts, once per pinned commit pair. See
[Quality profiles](../docs/how-it-works.md#quality-profiles-the-trash-sync-off-by-default).

### Deleting without Maintainerr (native executor)

Optional. Settings > Executor > Deletes: **FLINCH (native)** makes FLINCH
delete through Radarr and Sonarr itself; Maintainerr is then not needed and
FLINCH writes nothing to it. The default stays Maintainerr. What it does: see
[The native executor](../docs/how-it-works.md#the-native-executor-settings--executor).

1. Configure Plex (Settings > Plex, or `PLEX_URL`/`PLEX_TOKEN`) with the
   server owner's token: FLINCH keeps a Leaving Soon collection in each
   library and reads each item again right before deleting it. Without Plex
   nothing is deleted. The collection is titled as Settings > Collections >
   Leaving Soon says; if Maintainerr already has a Plex collection with that
   title, rename or remove it first.
2. Settings > Executor: the Leaving Soon window (14 days), what deleting a
   movie means (its file and unmonitor, or the whole Radarr entry, optionally
   with Radarr's import exclusion), Seerr cleanup (needs `SEERR_URL` and a
   `SEERR_API_KEY` allowed to manage requests) and deletes per run (10). The
   per-run cap under Schedule bounds new deletes and announcements too.
3. Optional: Settings > Torrents > Remove after delete, so a deleted item's
   torrents go with it once seeded.
4. Run in dry run first: the log prints every Radarr, Sonarr, Plex, Seerr and
   torrent write as `[dry-run] would …`, and the Overview's Executor card
   shows what would go.

Switching from Maintainerr leaves its collections as they are: empty them in
Maintainerr, or what FLINCH handed there earlier still leaves on Maintainerr's
schedule. The Executor card lists FLINCH's deletes of the last 30 days with a
Restore button (monitor and search again).

### Taste embeddings (EmbeddingGemma 2)

Optional. For titles nobody has played yet, P(watch) can lean on how readily
the household plays similar titles. "Similar" comes from an embedding of each
movie's and show's catalogue description, made by
[EmbeddingGemma 2](https://ai.google.dev/gemma/docs/embeddinggemma/model_card_2)
inside `flinch-arrd` itself: a pure-Rust port of its text encoder on
[candle](https://github.com/huggingface/candle), on the CPU, with no model
server to run. Two smaller encoders fit a tighter memory budget (see
*Models* below). Off, FLINCH runs as before.

The description is built from content metadata only: title, year, genres,
certification, runtime, original language, studio or network, series type,
Radarr collection, overview, and from Plex (when configured) tagline,
countries, directors, writers and the cast Plex lists. Nothing about your
household's viewing, ratings, requests, dates or tags goes in.

Switch it on in Settings > Taste embeddings:

| Field | Default | Notes |
| --- | --- | --- |
| Embed titles | off | embed new and changed titles each cycle |
| Model | EmbeddingGemma 2 | `embeddinggemma-2`, `bge-small-en-v1.5` or `all-minilm-l6-v2`; switching embeds every title again |
| Dimensions | 256 | EmbeddingGemma 2 only: 128, 256, 512 or 768, how much of the 768-d vector is kept (Matryoshka truncation); changing it embeds every title again. The other models always keep their 384 and ignore it |
| Daily budget | 500 | at most this many titles (1–20000) embedded per UTC day, on-disk titles first |
| Posters | off | EmbeddingGemma 2 only: describe each title by its poster too (see below); switching it on or off embeds every title again |
| Taste half-life | 0 (off) | `taste.half_life_days`, 7–3650: an outcome counts half after this many days (see [Taste](../docs/how-it-works.md#taste-embeddinggemma-2)); applied at the next daily refit |

- **Models.** Pick by memory:

  | Model | Weights | Vector | Resident while embedding | Peak while loading | 16 texts × 256 tokens |
  | --- | --- | --- | --- | --- | --- |
  | EmbeddingGemma 2 (`google/embeddinggemma-2`, Apache 2.0) | ~580 MB | 128–768 | ~650 MB peak (below) | — | not measured (5 texts ≈ 3 s, below) |
  | bge-small (`BAAI/bge-small-en-v1.5`, MIT) | ~130 MB | 384 | ~200 MB | ~270 MB | ~3.2 s |
  | MiniLM (`sentence-transformers/all-MiniLM-L6-v2`, Apache 2.0) | ~90 MB | 384 | ~110 MB | ~190 MB | ~1.6 s |

  Measured on the 20-thread test machine. While a small model loads, its
  memory-mapped file (clean page cache the kernel can drop) and the float32
  copy candle makes of it are both resident; the mapping is released once
  loaded. So only MiniLM stays under 150 MB while embedding; bge-small needs
  about 200 MB. EmbeddingGemma 2 describes titles best. The two BERT
  encoders run on candle's BERT port with float32 weights, four texts per
  forward pass, each text cut to 256 tokens (a long overview loses its end);
  bge uses CLS pooling, MiniLM mean pooling, as their sentence-transformers
  configs say. Their first cycle downloads `config.json`, `tokenizer.json`
  and `model.safetensors` of a pinned revision into
  `/state/models/<model>-<revision>/` (offline: copy those three there).
  Search uses whichever model the vectors were made with.

- **Weights.** The first cycle with embedding on downloads the pinned
  revision of `google/embeddinggemma-2` (Apache 2.0, no token needed) from
  Hugging Face: its tokenizer and only the text tensors of the checkpoint
  (one 542 MB byte range of the 1.5 GB file), into
  `/state/models/embeddinggemma-2-<revision>/`. The daemon needs HTTPS to
  `huggingface.co` and its file CDN that once, and follows their redirects for
  this download only. Offline? Copy `tokenizer.json` and `text.safetensors`
  there yourself.
- **CPU and memory.** Float32 math on bfloat16 weights that stay
  memory-mapped, one layer at a time (float16 is never used: the model card
  says it breaks EmbeddingGemma). A cycle embeds for at most two minutes and
  the plan waits for it; on a 20-thread test machine five descriptions took
  about 3 seconds. The first cycle (download plus embedding) peaked at about
  650 MB resident, mostly the mapped weights, which is why the manifests give
  `flinch-arrd` a 1 Gi limit.
- **Posters (optional).** With Posters on, each title's description also
  holds its poster, as the model's interleaved image input: a port of
  EmbeddingGemma 2's vision tower (Gemma 4's) on candle turns the poster into
  up to 280 soft tokens that the text encoder reads where the description
  says `Poster: <|image|>`, right after the title. The poster is the one
  Radarr or Sonarr lists upstream (`images` → `poster` → `remoteUrl`, a TMDB
  or TheTVDB CDN address), fetched with no credential or cookie, at most
  8 MB, JPEG, PNG or WebP; the *arr's own copy is never used, since reaching
  it takes an API key. The first cycle with posters on downloads the vision
  tensors as well: two more byte ranges of the checkpoint (335 MB) into
  `vision.safetensors`, plus `processor_config.json`. A poster the CDN
  refuses or that does not decode leaves the title described by text alone
  until its poster URL changes; a network error or CDN fault leaves it for a
  later cycle. The vectors match the reference ONNX export (Pillow
  preprocessing) to a cosine of 1.000000 for a PNG poster and 0.99985 to
  0.99996 for JPEGs, where the two JPEG decoders round a few pixels
  differently. Cost, measured on the test machine with a 2:3 poster (260
  soft tokens): the vision tower took 12.2 s on 2 CPUs, 7.4 s on 4 and 4.9 s
  on 8, and the text pass with the poster another 0.6–1.3 s, so a cycle's two
  minutes embed roughly 9 to 22 titles; the daily budget still applies. Peak
  resident memory was about 830 MB: some 580 MB of it the mapped weights,
  which the kernel can drop and read again, and about 250 MB working memory.
  That fits under the manifests' 1 Gi limit only because the mapped part can
  be dropped; it was not run under that limit, so give `flinch-arrd` 1.5 Gi
  with posters on.
- **Off again.** Switching it off stops embedding; the vectors already made
  stay in use. The header lists **Embeddings** while it is on; hover it for
  coverage and the budget left today, or the last error.
- **Inflow advice.** The Overview's *Coming in, likely unwatched* card lists
  monitored shows that read cold or were abandoned, and cold Seerr requests,
  with the suggested action (e.g. unmonitor future seasons in Sonarr). FLINCH
  only reads Sonarr, Radarr and Seerr for it; acting on it is up to you. The
  abandoned rule needs no vectors; the cold rules list nothing until titles
  are embedded.
- **Search by meaning.** With titles embedded, the Series and Movies search
  box's *Meaning* mode finds titles by what they are about. `flinch-web` runs
  the same encoder on the weights the daemon downloaded (it never fetches
  them itself): about 0.3 s of one core per query, and about 550 MB resident
  from the first search on (700 MB at peak), so the manifests give
  `flinch-web` a 1 Gi memory and one-core CPU limit. Until the weights and
  vectors exist the search says so and the *Title* mode works as before.

## Install

```bash
kubectl kustomize deploy/     # review what will be created
kubectl apply -k deploy/
kubectl -n media get pods
```

This creates the claim `flinch-state-rwx`, the ConfigMap `flinch-watch-state`,
the Deployments `flinch-arrd` and `flinch-web`, and the Service `flinch-web`.

## Reach the UI

A port-forward is the simplest, and it only listens on your machine:

```bash
kubectl -n media port-forward svc/flinch-web 7911:7911
```

Open <http://localhost:7911> and log in with `FLINCH_WEB_USERNAME` and
`FLINCH_WEB_PASSWORD`. The session lasts 30 days from its last use, until you
press **Log out**, or until `flinch-web` restarts: sessions live in its memory
only. To change the login, update the Secret and restart `flinch-web`
(`kubectl -n media rollout restart deploy/flinch-web`), which also logs every
browser out.

To publish the UI through your ingress controller, turn on the `ingress`
component in `deploy/kustomization.yaml` and set its host (the commented
`patches:` example) and certificate (`deploy/ingress/flinch-web-ingress.yaml`).
It is HTTPS only, because the browser sends its session cookie with every
request; `flinch-web` marks the cookie `Secure` when the ingress reports HTTPS
in `X-Forwarded-Proto`, as ingress-nginx and Traefik do. For single sign-on,
set up [OpenID Connect](#single-sign-on-openid-connect) below, or put your
proxy's forward-auth annotation (Authelia, Authentik, oauth2-proxy) in front
of the login. Keep it off the internet: whoever gets past it can schedule
deletions.

### Single sign-on (OpenID Connect)

Optional, off until the issuer, client id and redirect URL are in the
Secret. The login page then shows **Sign in with <name>** above the password
form (or alone, without `FLINCH_WEB_USERNAME`/`FLINCH_WEB_PASSWORD`), and a
sign-in gets the same 30-day session a password login does. The API key is
unchanged.

1. **Register FLINCH at your provider** (Authelia, Authentik, Keycloak,
   Kanidm, Pocket ID …) as a confidential client with the authorization code
   flow: redirect URI `https://<flinch host>/api/oidc/callback`, scopes
   `openid email profile` (plus `groups` when you allow by group), PKCE with
   `S256`, and `client_secret_basic` (or `client_secret_post`) at the token
   endpoint. For Authelia, a client under `identity_providers.oidc.clients`
   with `public: false`, `require_pkce: true`, `pkce_challenge_method: S256`,
   `redirect_uris` and `scopes` as above, and
   `token_endpoint_auth_method: client_secret_basic`
   ([Authelia's client reference](https://www.authelia.com/configuration/identity-providers/openid-connect/clients/)).
2. **Add the keys** to `flinch-secrets` (the manifests pass each one to
   `flinch-web` when present):
   ```
   FLINCH_WEB_OIDC_ISSUER=https://auth.example.org
   FLINCH_WEB_OIDC_CLIENT_ID=flinch
   FLINCH_WEB_OIDC_CLIENT_SECRET=<the client secret>
   FLINCH_WEB_OIDC_REDIRECT_URL=https://flinch.example.org/api/oidc/callback
   FLINCH_WEB_OIDC_ALLOWED_GROUPS=admins
   FLINCH_WEB_OIDC_NAME=Authelia
   ```
   Copy the issuer exactly from the provider's
   `/.well-known/openid-configuration` (`"issuer"`), trailing slash and all:
   Authentik's ends in `/application/o/<slug>/`. FLINCH refuses a provider
   whose discovery names another issuer.
3. **Restart `flinch-web`.** Its log says
   `single sign-on with <issuer> set; … allowed`, or why it stayed off.

How it behaves:

- **Nobody is allowed by default.** An account gets in when its `sub` is in
  `_ALLOWED_SUBJECTS`, its verified email in `_ALLOWED_EMAILS` (any case), or
  one of its groups in `_ALLOWED_GROUPS`. Email and groups are read from the
  ID token and the provider's userinfo endpoint, so a provider that keeps
  them out of the ID token (Authelia's default) works. A refused account
  lands back on the login page with a message; the log says it was refused,
  never who.
- **Checked like the specification says.** `state` and `nonce` are fresh per
  sign-in, single use and valid ten minutes; PKCE (`S256`) binds the code to
  this sign-in. The ID token's signature is checked against the provider's
  published keys (RS, PS, ES and EdDSA; never `none` or a shared secret), and
  its issuer, audience, expiry and nonce too. No code, token, subject or
  email is ever logged.
- **Cookies.** The session cookie stays `SameSite=Strict`. The provider's
  redirect back is a navigation from another site, which carries no `Strict`
  cookie, so the sign-in's own cookie (`flinch_oidc`, ten minutes, path
  `/api/oidc/` only) is `SameSite=Lax`; it ties the callback to the browser
  that started it, so nobody can sign you in as them with a link. The
  callback answers with a page that moves on to the UI by itself, so the
  first load already carries the new session cookie. Both are `Secure`
  behind HTTPS.
- **Reaching the provider.** `flinch-web` calls its discovery, token, keys
  and userinfo endpoints over HTTPS (plain HTTP only on localhost), checks
  certificates against the public CAs built into the image (a private CA is
  not supported), follows no redirect, and gives up after 10 seconds.
  Discovery is reused for ten minutes; keys are read for every sign-in, so a
  rotated key is never missed.
- **Signing out** of FLINCH ends its session only, not the one at the
  provider.

### Upgrading from the access token

Up to 0.1.1, one token in `FLINCH_WEB_TOKEN` opened both the UI and the API.
From 0.2.0 on:

- Automations keep working unchanged: the token is now the API key.
  `Authorization: Bearer <FLINCH_WEB_TOKEN>` is still accepted everywhere, and
  `X-Api-Key: <FLINCH_WEB_TOKEN>` now is too.
- The UI asks for a username and password, and deletes the token the browser
  kept. Until both are in the Secret and `flinch-web` runs the new release's
  manifests, it shows **No login set**.

Add the login to the Secret you have from a file only you can read
(`kubectl create secret` fails on a Secret that exists):

```bash
install -m 600 /dev/null login.yaml    # readable only by you
$EDITOR login.yaml
kubectl -n media patch secret flinch-secrets --type merge --patch-file login.yaml
rm login.yaml
```

with:

```yaml
stringData:
  FLINCH_WEB_USERNAME: '<your choice>'
  FLINCH_WEB_PASSWORD: '<a strong password>'
```

Keep the single quotes, so YAML takes each value as written; a `'` inside one
is written `''`. Then update your checkout to the new release
(`git fetch --tags && git checkout v0.3.0`) and apply its manifests:
`kubectl apply -k deploy/`, or your overlay with its `newTag` moved. They pass
the two new keys to `flinch-web` and restart it. Moving `newTag` in an overlay
of an older checkout is not enough: its `../base` is still the old one, which
never passes the login, so the UI keeps saying **No login set**. If
`flinch-web` already runs the new release's manifests, restart it instead:
`kubectl -n media rollout restart deploy/flinch-web`.

## First run

Dry run is on by default (Settings > Planner > Dry run). A fresh install
writes its plan to `eviction-plan.json` and sends nothing to Maintainerr. Each
write FLINCH would send is printed instead.

The first cycle starts when the daemon starts. The next one follows after the
Scan interval (Settings > Schedule, one hour by default), or when you press
**Trigger run** in the UI.

Open the UI and read the Overview. Settings shows the state of each connection
(Radarr, Sonarr, Plex, Tautulli, Maintainerr). Then read the daemon log:

```bash
kubectl -n media logs deploy/flinch-arrd        # add -f to follow
```

The per-action lines are shaped for grep. In a dry run:

```
[dry-run] would exclude ratingKey 4511 (mediaId 4502)
[dry-run] would add ratingKey 812 (mediaId 812) to collection 3
```

With dry run off, each action ends with its outcome:

```
[flinch-arrd] protect sonarr-11-s2 (ratingKey 4511): done, verified
[flinch-arrd] schedule radarr-7 (ratingKey 812, 41.2 GiB) into collection 3: failed, retried next cycle: …
```

On the volume, `eviction-plan.json` holds the last cycle's forecast and plan
(see [The plan](../docs/how-it-works.md#the-plan-least-total-regret)) and
`status.json` its `sync` block.

Rules the daemon obeys (all under test): pinned items (favorites, keep
collections, the keep tag, your own Maintainerr exclusions) are never
selected; missing watch state fails closed; a Maintainerr **201 with
`{"code":0, "result":"Failed - no metadata"}` is a failure**, never a silent
success; nothing is handed to a Maintainerr older than 3.10, or to a
collection that is inactive, of the wrong type, bound to another Plex
library, or whose *arr action frees nothing or is one Maintainerr refuses for
its type; in a dry run, every write is only printed. An item nobody finished
goes only to a Leaving Soon collection that is active, shown in Plex and has
a window of at least one day; while none is named or validates, it waits
instead of going to a delete collection. FLINCH releases only exclusions it
created, and only for an item proven gone: no file in Radarr/Sonarr and
absent from a complete Plex listing.

## Maintainerr setup

FLINCH fills Maintainerr collections; Maintainerr deletes (unless you choose
the [native executor](#deleting-without-maintainerr-native-executor)). The
collection names are in Settings > Collections. FLINCH writes exclusions
only for pinned items and items someone is partway through, so your other
Maintainerr rule groups still act on everything else.

1. Make the two delete collections, Movies (`Watched Movies Cleanup` by
   default) and Seasons (`Watched Seasons Cleanup`), manual-membership
   collections with a delete action. If they also keep their own rules,
   Maintainerr will delete what those rules select regardless of FLINCH's
   plan. A season collection needs "Unmonitor and delete existing episodes"
   ("Unmonitor and delete season", or deleting the show if empty, work too).
   Never "Unmonitor and delete all": Maintainerr refuses it for seasons, so
   nothing would ever leave. FLINCH reports it and hands that collection
   nothing.
2. Create the Leaving Soon collections (`Leaving Soon` by default): one
   Maintainerr rule group per library with that same title, of media type
   *movie* for the movie library and *season* for TV. Turn **Use rules** off:
   FLINCH adds the items, and with a delete action anything the group's own
   rules selected would be deleted by Maintainerr on its own. Give each an
   *arr action that deletes files ("Delete" for movies, one of the season
   actions above for TV), "Take action after days" set to the household's
   warning window (14 is a good start), "Show on Plex home" on and "Keep in
   Maintainerr only" off. A collection with the action "Do nothing" never
   deletes: Maintainerr skips it. Turn on the overlay if the leave date should
   show on the poster. Until both validate, the status names what is wrong and
   unwatched evictions wait. A blank title holds never-played reclaim off, so
   nothing nobody played is selected.
3. If Seerr is configured in Maintainerr, turn on **Force delete Seerr
   request** on every collection FLINCH uses. Otherwise a removed title's
   Seerr request stays until Seerr's availability sync notices, and it cannot
   be requested again at once. FLINCH warns about each such collection but
   still hands items to it.

## Radarr and Sonarr setup

FLINCH writes to Radarr or Sonarr only for what you switch on (the quality
sync's applies, quality actions, the native executor). One setting in each keeps a file
deleted outside FLINCH from being downloaded again: turn on **Unmonitor
Deleted Movies** in Radarr and **Unmonitor Deleted Episodes** in Sonarr
(Settings > Media Management). FLINCH lists such deletions on the Overview,
with whether each one will download again.

### Disks the *arrs do not report

The Overview's Storage card lists any root folder as **Not governed** when its
app reports no mount for it. Sonarr leaves NFS mounts out of its disk report,
so TV libraries on their own shares show up here, and nothing on them is
evicted. Give FLINCH the same shares, read-only, at the app's own paths under
a prefix, and set `FLINCH_LIBRARY_PREFIX` to it; FLINCH then measures them
itself. In your overlay's `flinch-arrd` patch:

```yaml
          containers:
            - name: flinch-arrd
              env:
                - { name: FLINCH_LIBRARY_PREFIX, value: "/library" }
              volumeMounts:
                # Sonarr has media-tv-a at /data/media/tv: the same path, under /library.
                - { name: lib-tv-a, mountPath: /library/data/media/tv, readOnly: true }
          volumes:
            - name: lib-tv-a
              persistentVolumeClaim: { claimName: media-tv-a, readOnly: true }
```

A share mounted at the wrong path is refused: FLINCH compares its reading with
the free space the app measured at the root, and keeps the root ungoverned
when they disagree. A share two roots live on (movies and anime on one disk)
counts once.

### Several Radarr and Sonarr instances

Beside the default `RADARR_URL`/`SONARR_URL` pair, FLINCH reads up to eight
extra instances per app (an HD and a 4K Radarr, an anime Sonarr). Add them in
either of two ways; both are read every cycle, no restart needed. A broken
extra instance is logged and skipped; the default pair always stays. See
[how it works](../docs/how-it-works.md#several-radarr-and-sonarr-instances)
for the card ids and what each instance keeps to itself.

**Settings → Instances** (stored in `settings.json` as `instances`):

| Field | Meaning |
|---|---|
| `app` | `radarr` or `sonarr` |
| `name` | 1–24 of `a-z`, `0-9`, `_` (no `-`), unique per app; ids become `radarr@<name>-…` |
| `url` | `http://` or `https://` URL the daemon reaches the instance at |
| `key_env` | The **name** of an environment variable holding the API key (`A-Z`, `0-9`, `_`, up to 64), never the key itself |
| `archive_root` | Archive root folder; blank means this instance never archives |
| `compact_profile` | Quality profile to downgrade into; blank means none |
| `public_url` | Link the browser opens; blank means a sibling host of FLINCH named `<app>-<name>` (`radarr-4k`) |

The key stays in the daemon's environment: add the variable named in
`key_env` to the `flinch-arrd` Deployment (and CronJob), from the secret:

```yaml
              env:
                - { name: RADARR_4K_API_KEY, valueFrom: { secretKeyRef: { name: flinch-secrets, key: RADARR_4K_API_KEY } } }
```

**Numbered environment variables**, on `flinch-arrd`, for `N` = 1, 2, …:

| Variable | Meaning |
|---|---|
| `RADARR_<N>_URL` | Required. The instance's URL |
| `RADARR_<N>_API_KEY` | Required. Its API key (from the secret) |
| `RADARR_<N>_NAME` | Optional name; without it the number is the name (`radarr@2`) |
| `RADARR_<N>_ARCHIVE_ROOT` | Optional archive root; unset means never archives |
| `RADARR_<N>_COMPACT_PROFILE` | Optional compact profile |
| `RADARR_<N>_PUBLIC_URL` | Optional browser link |

`SONARR_<N>_URL`, `SONARR_<N>_API_KEY`, `SONARR_<N>_NAME` and the rest work
the same for Sonarr. For example, a 4K Radarr and an anime Sonarr:

```yaml
          containers:
            - name: flinch-arrd
              env:
                - { name: RADARR_2_URL, value: "http://radarr-4k:7878" }
                - { name: RADARR_2_NAME, value: "4k" }
                - { name: RADARR_2_COMPACT_PROFILE, value: "HD-1080p" }
                - { name: RADARR_2_API_KEY, valueFrom: { secretKeyRef: { name: flinch-secrets, key: RADARR_4K_API_KEY } } }
                - { name: SONARR_2_URL, value: "http://sonarr-anime:8989" }
                - { name: SONARR_2_NAME, value: "anime" }
                - { name: SONARR_2_API_KEY, valueFrom: { secretKeyRef: { name: flinch-secrets, key: SONARR_ANIME_API_KEY } } }
```

Every instance must be readable or the cycle fails. Inflow's import-list
toggles cover the default pair only. Renaming an instance changes its card
ids, so its items look new to FLINCH. For the TRaSH sync, give each extra
instance an entry under `instances.extra` keyed `radarr@<name>`; without one
it is not synced.

## Turn off dry run

Before you do:

1. Run at least one cycle in dry run and read the log and
   `eviction-plan.json`: every write FLINCH would send is printed, with the
   Plex ratingKey it targets.
2. Finish the [Maintainerr setup](#maintainerr-setup) (or choose the
   [native executor](#deleting-without-maintainerr-native-executor)) and the
   [Radarr and Sonarr setup](#radarr-and-sonarr-setup).
3. If Tautulli is connected, turn **Keep History** on for every user and for
   every library FLINCH manages (Tautulli > Users / Libraries > edit). Where it
   is off, the log says `tautulli keeps no history for …` and never-played
   reclaim stays held, because Tautulli's silence then proves nothing.
4. Mark anything the household must never lose with the keep tag (Settings >
   Rules > Keep tag, `flinch-keep` by default): as a tag in Radarr/Sonarr, a
   label on the movie or show in Plex, or by putting it in a Plex collection
   with that name (a collection can also hold single seasons). The log prints
   `plex keep markers ('flinch-keep'): N item(s) kept`, and the Overview lists
   them under Pinned.
5. Optional: preview the quality sync on the Quality profiles tab, and apply
   the changes you want while still in dry run to read the requests it would
   send in the log.

Then turn off Settings > Planner > Dry run and press **Save settings**. It
applies on the next run.

`FLINCH_DRY_RUN=1` on the daemon forces a dry run, whatever the setting says.

Upgrading from a release with the Enforcement switch: it is gone, along with
`--enforce` and `FLINCH_ENFORCE`. An existing `settings.json` loads with dry
run on, so turn it off again once the new plan looks right.

## State on the volume

Everything the daemon remembers between cycles is a small JSON file on the
`flinch-state-rwx` volume, mounted at `/state`. Every file is replaced through
a temp file and a rename, so the UI never reads a half-written file, even over
the NFS share behind a ReadWriteMany volume, and is readable only by the pods'
user (mode 600): `settings.json` can hold the Plex token and the play files
hold the household's viewing. Every reader fails safe: a missing or corrupt
file reads as "nothing yet".

| File | What it holds | If lost |
| --- | --- | --- |
| `settings.json` | settings from the UI | defaults; an unparseable or out-of-range file keeps the last good settings in force |
| `status.json`, `items.json`, `history.json` | what the UI shows | rebuilt next cycle |
| `eviction-plan.json` | the last cycle's forecast per disk and its plan: method, items, sizes, regret and reasons, and the moves to the archive | nothing; written again next cycle |
| `plan-inputs.json` | the last cycle's planner inputs before any rule: candidates, forecasts, and the facts rules ask about (paths, tags, requests, themes), for the rules preview | the preview waits for the next cycle |
| `inflow-actions.json` | the approved shows whose future seasons FLINCH unmonitored, and the import lists it holds off, with when | FLINCH no longer knows which lists it switched off and never switches them back on (do it in the *arr); an approved show is unmonitored again, which changes nothing |
| `arr-imports.json` | Radarr and Sonarr imports of the last 30 days, the arrival rate's input (read again after 6 hours) | read again from the *arrs on the next cycle |
| `releases.json` | per item, the last Prowlarr search: seeders, the newest usenet post's age and the smallest whole release (each kept 7 days) | searched again, 20 items per cycle; until then re-download cost uses size only and no downgrade is acted on |
| `evictions.json` | bytes handed to Maintainerr (or moved to the archive) that a recycle bin or an unfinished copy may still hold or that are held, and each hand-over for 120 days (to tell FLINCH's deletions from others) | evictions in flight or held go uncredited, and FLINCH's own recent deletions may be listed as ones it did not make |
| `protected.json`, `scheduled.json` | exclusions and collection members FLINCH created | FLINCH forgets it owns them and leaves them alone |
| `candidates.json` | grace-run streaks | every candidate re-earns its grace window |
| `archive-streaks.json` | grace-run streaks of planned moves to the archive | every planned move re-earns its grace window |
| `operator-keeps.json` | the cards your own Maintainerr exclusions keep, as last read | rewritten by the next cycle that reads Maintainerr; an outage before then plans without them (nothing is synced during it) |
| `playback.json`, `tautulli.json` | raw plays, the daily fit's input | fitting waits for new history |
| `fit.json` | the last daily fit: the candidate judged (recalibrated priors or full fit), its out-of-fold AUC, Brier and ECE beside the priors', its parameters, and why it was or was not adopted | refitted on the next cycle |
| `hazard.json` | the adopted P(watch) hazard, present only while a fit clears the gate | the priors run until a fit is adopted again |
| `episode-guids.json` | episode `plex://` GUIDs of resolved shows, for plays recorded before a library migration (refreshed at most daily, only while such plays exist) | re-read from Plex on the next cycle that needs it |
| `arr-history.json` | when each title was on disk, and the files removed in the last 30 days, from the Radarr and Sonarr import and delete history (refreshed daily) | read again from the *arrs on the next cycle |
| `embeddings.json` | the taste vectors: model, dimensions and text recipe they were made with, per movie and show the hash of its description and its vector, and how many titles were embedded today (UTC) | every title is embedded again, within the daily budget |
| `themes.json` | each movie's and show's theme (clusters of the taste vectors) and the theme names, with when they were computed and a fingerprint of the vectors (recomputed daily or when a vector changes) | recomputed on the next cycle |
| `notify.json` | per notification channel the events it was sent (kept 120 days), its posts of the last hour, and each problem's run of cycles | events still current are sent once more; problem runs start over |
| `notify-test.json`, `notify-test-result.json` | a Send test the UI asked for, and the daemon's answer | nothing; press Send test again |
| `requests.json` | the household's keeps and removal requests (written by flinch-web only): who, when, until when, and your decisions (settled ones kept 90 days) | every requested keep ends and the removal queue is empty: items are judged as before; links already sent still work |
| `quality-actions.json` | every move to the compact profile (kept a year): when, from which profile, and whether a smaller file landed | FLINCH forgets what it moved: an item advised a downgrade may be moved again, and the day's cap starts over |
| `arr-grabs.json` | Radarr and Sonarr grabs and imports of the last 30 days, for upgrade churn (read again after 6 hours) | read again from the *arrs on the next cycle |
| `upgrade-guard.json` | the churn steps taken (unmonitor, upgrades off), each kept 90 days | a step you undid may be taken once more |
| `trash.json` | the quality sync's preview per app, its last apply, and the custom formats and profiles FLINCH created | the preview is rebuilt next run; FLINCH forgets which formats it created, so it never offers to delete them |
| `trash-apply.json` | a quality-sync apply the UI queued, until the daemon takes it | nothing; apply again |
| `trash-guide-<commit>.json` | the TRaSH-Guides files at the pinned commit | downloaded again on the next preview |
| `trash-pcd-<commit>-<schema commit>.json` | a Profilarr Compliant Database read at its pinned commits, with its declared license | downloaded and replayed again on the next preview |
| `native.json` | with the native executor: the Leaving Soon shelf (each item's announcement, window end and Plex collection) and the deletes of the last 30 days with what undoes them | the shelf's windows start over (announced again, so nothing is deleted unwarned); recent deletes can no longer be restored from the UI |
| `dupes.json` | the duplicate copies you chose to keep in the UI, confirmed or not | your choices are gone; nothing is removed until you choose and confirm again |
| `dupes-acted.json` | duplicate copies removed (or tried) in the last 30 days | a failed removal may be tried once more; the kept copy is checked first every time |
| `restore/<item id>` | an undo the UI queued, until the daemon takes it | nothing; press Restore again |
| `models/embeddinggemma-2-<revision>/` | EmbeddingGemma 2's tokenizer and text weights (about 580 MB), downloaded the first time embedding runs; with Posters on also `vision.safetensors` (335 MB) and `processor_config.json` | downloaded again the next time embedding runs |
| `models/bge-small-en-v1.5-<revision>/`, `models/all-minilm-l6-v2-<revision>/` | the chosen small model's `config.json`, `tokenizer.json` and `model.safetensors` (about 130 / 90 MB) | downloaded again the next time embedding runs |

`protected.json` and `scheduled.json` record only the exclusions and
collection members FLINCH created and verified by reading them back, so runs
are idempotent and FLINCH never removes your own rows. A failed or unverified
write (409 while Maintainerr is busy, a timeout, a refusal) is not recorded
and is retried next run. A `protected.json` from before the sync existed (a
bare list of card ids) reads as empty: none of those exclusions ever landed.

The ConfigMap `flinch-watch-state` is an optional media-server watch export
the daemon merges (`examples/watch-state.json` is the shape). `{}` means none:
Plex and Tautulli supply the evidence, and anything without evidence is held.

## Running as a CronJob

The Deployment loops on the Scan interval. For a fixed schedule instead, run
the same image with `--once` from a CronJob and remove the Deployment. With
`--once`, the Scan interval and **Trigger run** have no effect; the schedule
decides. In your overlay (next section), add a file `flinch-arrd-cronjob.yaml`:

```yaml
apiVersion: batch/v1
kind: CronJob
metadata: { name: flinch-arrd }
spec:
  schedule: "0 3 * * *"
  # One classifier at a time, as with the Deployment's Recreate strategy.
  concurrencyPolicy: Forbid
  jobTemplate:
    spec:
      template:
        spec:
          restartPolicy: OnFailure
          automountServiceAccountToken: false
          securityContext:
            runAsNonRoot: true
            runAsUser: 1000
            runAsGroup: 1000
            fsGroup: 1000
            seccompProfile: { type: RuntimeDefault }
          containers:
            - name: flinch-arrd
              image: flinch
              imagePullPolicy: Always
              command: ["/usr/local/bin/flinch-arrd"]
              args: ["--once"]
              # The same env as deploy/base/flinch-arrd.yaml.
              env:
                - { name: RADARR_URL, value: "http://radarr:7878" }
                - { name: SONARR_URL, value: "http://sonarr:8989" }
                - { name: MAINTAINERR_URL, value: "http://maintainerr:6246" }
                - { name: FLINCH_WATCH_STATE, value: "/cfg/watch-state.json" }
                - { name: FLINCH_STATE_DIR, value: "/state" }
                - { name: RADARR_API_KEY, valueFrom: { secretKeyRef: { name: flinch-secrets, key: RADARR_API_KEY } } }
                - { name: SONARR_API_KEY, valueFrom: { secretKeyRef: { name: flinch-secrets, key: SONARR_API_KEY } } }
                - { name: MAINTAINERR_API_KEY, valueFrom: { secretKeyRef: { name: flinch-secrets, key: MAINTAINERR_API_KEY, optional: true } } }
                - { name: FLINCH_TAUTULLI_URL, valueFrom: { secretKeyRef: { name: flinch-secrets, key: TAUTULLI_URL, optional: true } } }
                - { name: FLINCH_TAUTULLI_KEY, valueFrom: { secretKeyRef: { name: flinch-secrets, key: TAUTULLI_API_KEY, optional: true } } }
                - { name: FLINCH_PLEX_URL, valueFrom: { secretKeyRef: { name: flinch-secrets, key: PLEX_URL, optional: true } } }
                - { name: FLINCH_PLEX_TOKEN, valueFrom: { secretKeyRef: { name: flinch-secrets, key: PLEX_TOKEN, optional: true } } }
                - { name: SEERR_URL, valueFrom: { secretKeyRef: { name: flinch-secrets, key: SEERR_URL, optional: true } } }
                - { name: SEERR_API_KEY, valueFrom: { secretKeyRef: { name: flinch-secrets, key: SEERR_API_KEY, optional: true } } }
                - { name: PROWLARR_URL, valueFrom: { secretKeyRef: { name: flinch-secrets, key: PROWLARR_URL, optional: true } } }
                - { name: PROWLARR_API_KEY, valueFrom: { secretKeyRef: { name: flinch-secrets, key: PROWLARR_API_KEY, optional: true } } }
                - { name: SABNZBD_URL, valueFrom: { secretKeyRef: { name: flinch-secrets, key: SABNZBD_URL, optional: true } } }
                - { name: SABNZBD_API_KEY, valueFrom: { secretKeyRef: { name: flinch-secrets, key: SABNZBD_API_KEY, optional: true } } }
                - { name: TMDB_API_KEY, valueFrom: { secretKeyRef: { name: flinch-secrets, key: TMDB_API_KEY, optional: true } } }
              securityContext:
                allowPrivilegeEscalation: false
                readOnlyRootFilesystem: true
                capabilities: { drop: [ALL] }
              volumeMounts:
                - { name: watch, mountPath: /cfg, readOnly: true }
                - { name: state, mountPath: /state }
                - { name: data, mountPath: /data }
          volumes:
            - { name: watch, configMap: { name: flinch-watch-state } }
            - { name: state, persistentVolumeClaim: { claimName: flinch-state-rwx } }
            - { name: data, emptyDir: {} }
```

and in the overlay's `kustomization.yaml`:

```yaml
resources: ["../base", flinch-arrd-cronjob.yaml]
patches:
  - patch: |
      $patch: delete
      apiVersion: apps/v1
      kind: Deployment
      metadata: { name: flinch-arrd }
```

With `--once`, a failed cycle exits non-zero, so the Job shows the failure.

## Keeping your own values out of git

`deploy/local/` is gitignored. Put your namespace, image, URLs and host in
an overlay there and leave the tracked files as they are:

```yaml
# deploy/local/kustomization.yaml
apiVersion: kustomize.config.k8s.io/v1beta1
kind: Kustomization
namespace: media
resources: ["../base"]
components: ["../ingress"]   # only if you publish the UI through an Ingress
images:
  - { name: flinch, newName: ghcr.io/tobhoster/flinch, newTag: "0.3.0" }
patches:
  - target: { kind: Ingress, name: flinch-web }
    patch: |
      - { op: replace, path: /spec/rules/0/host, value: flinch.your-domain.example }
      - { op: replace, path: /spec/tls/0/hosts/0, value: flinch.your-domain.example }
```

The overlay points at `../base`, not `..`: kustomize refuses a base directory
that contains the overlay. Add further `patches:` for the URLs (the example in
`deploy/kustomization.yaml`), a storage class, the certificate, or
`imagePullSecrets` if you run your own build from a registry that needs a
login. Then:

```bash
kubectl diff -k deploy/local
kubectl apply -k deploy/local
```

## Security

- **A login for people, an API key for machines.** `flinch-web` refuses every
  API request without a session from logging in (`FLINCH_WEB_USERNAME`,
  `FLINCH_WEB_PASSWORD`, or [single sign-on](#single-sign-on-openid-connect))
  or the API key (`FLINCH_WEB_TOKEN`, as `X-Api-Key` or
  `Authorization: Bearer`), and everything when none is set. Either one can
  change Settings, including dry run, so treat both like the *arr API
  keys, and allow single sign-on only to the accounts that should.
- **Sessions.** The session cookie is `HttpOnly` and `SameSite=Strict`, and
  `Secure` behind an HTTPS ingress. A session lasts 30 days from its last use,
  every login starts a new one, **Log out** ends it, and a restart of
  `flinch-web` ends them all. No cache may keep an answer behind the login
  (`Cache-Control: no-store`).
- **Logging in, logging out and writing with the cookie need the UI's
  header.** Each needs `X-Flinch-Request: 1`, which the UI sends and a page on
  another site cannot add. So a site you visit cannot guess the password
  through your browser, hold the pause below, log you out, or change Settings.
- **Failed logins pause logging in.** After five in a row, logins are refused
  for 30 seconds, doubling with each further failure up to 15 minutes, and a
  success resets the count. The count is shared by everyone, since behind an
  ingress every request comes from the same address: someone who can reach
  the UI can keep you from logging in, but not your open sessions or the API
  key. Restarting `flinch-web` clears the pause, and ends every session:
  `kubectl -n media rollout restart deploy/flinch-web`.
- **HTTPS off the machine.** Through an Ingress, serve it over HTTPS only
  (`deploy/ingress`), and keep it off the internet.
- **The Plex URL and token are one credential.** They always come from the
  same place, the Secret or Settings, and a changed URL needs the token again,
  so a saved token is never sent to a new address. Settings stores it in
  `settings.json` on the state volume, readable only by the pods' user; it is
  never sent back to the browser.
- **Keys stay with their host.** The daemon follows no HTTP redirects, so an
  API key is never forwarded to another host.
- **Least privilege.** Both pods run as uid 1000 with a read-only root
  filesystem, no capabilities, no privilege escalation, the runtime's seccomp
  profile and no Kubernetes API token. The daemon has no Service or Ingress:
  it only calls out.
- **Report problems privately:** see [SECURITY.md](../SECURITY.md).
