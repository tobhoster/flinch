# Deploying FLINCH

FLINCH runs as two pods that share one volume:

- `flinch-arrd`, the daemon. Each cycle it reads the inventory from Radarr and
  Sonarr and the watch evidence from Plex and Tautulli, plans, and syncs the
  plan to Maintainerr. Items it keeps get a Maintainerr exclusion; items it
  evicts join a Maintainerr collection, and Maintainerr deletes them on its own
  schedule. Both are addressed by Plex ratingKey, so an item whose Plex GUID
  join failed is left alone. It has no Service or Ingress: it only calls out.
- `flinch-web`, the UI and its JSON API on port 7911. It reads the state the
  daemon writes and writes `settings.json` when you save Settings. Every API
  call needs the token in `FLINCH_WEB_TOKEN`; the browser asks for it once.

| Path | What it is |
| --- | --- |
| `deploy/kustomization.yaml` | what you configure: namespace, image, URLs |
| `deploy/base/` | the manifests: state volume, ConfigMap, both Deployments, Service |
| `deploy/ingress/` | optional: an HTTPS Ingress for the UI |
| `deploy/Dockerfile` | the image |
| `deploy/recyclarr/recyclarr.flinch.yml` | optional Recyclarr changes for the premium and compact tiers |

## Try the demo first

The demo fills a state directory with made-up titles and serves the UI on
<http://localhost:7911>. No cluster and no *arr apps are needed.

With the published image:

```bash
docker run --rm -p 127.0.0.1:7911:7911 -e FLINCH_STATE_DIR=/tmp/demo -e FLINCH_WEB_TOKEN=demo \
  ghcr.io/tobhoster/flinch:0.1.0 sh -c 'flinch-demo && flinch-web'
```

Unlock the UI with `demo`.

From source:

```bash
cd frontend && npm ci && npm run build && cd ..
FLINCH_STATE_DIR=/tmp/flinch-demo cargo run -p flinch-web --bin flinch-demo
FLINCH_STATE_DIR=/tmp/flinch-demo FLINCH_WEB_DIR=frontend/dist FLINCH_WEB_TOKEN=demo \
  cargo run -p flinch-web --bin flinch-web
```

## Requirements

- Radarr and Sonarr. They supply the inventory; their API keys are required.
- Maintainerr 3.10 or newer. FLINCH hands evictions to it. Nothing is handed
  to an older version.
- Plex. It supplies item-level watch state and the ratingKey Maintainerr
  needs. Without Plex every item is held.
- Tautulli, optional. It keeps every play with the user who played it, so
  FLINCH sees plays Plex no longer reports.
- A Kubernetes cluster and `kubectl` (kustomize is built in).
- A storage class that supports ReadWriteMany (NFS, Longhorn, CephFS, ...).
  With ReadWriteOnce only, both pods must run on the same node; see
  `deploy/base/flinch-state.yaml`.

## The image

`ghcr.io/tobhoster/flinch` is built by this repository's CI for amd64 and
arm64, with a build provenance attestation and an SBOM. One image carries the
binaries (`flinch-arrd`, `flinch-fit`, `flinch-web`, `flinch-demo`) and the
built UI; the two Deployments differ only in `command:`. It is about 35 MB
(Rust and Alpine; no GPU, no Python) and runs as uid 1000.

| Tag | What it is |
| --- | --- |
| `0.1.0` | a release; `deploy/kustomization.yaml` pins one |
| `0.1`, `latest` | the newest release of that line, or overall |
| `edge` | the main branch |
| `sha-<commit>` | one build of main |

### Build your own

From the repository root:

```bash
docker build -t registry.example.com/flinch:0.1.0 -f deploy/Dockerfile .
docker push registry.example.com/flinch:0.1.0
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
echo "FLINCH_WEB_TOKEN=$(openssl rand -hex 32)" >> flinch.env   # the UI's login
$EDITOR flinch.env
kubectl -n media create secret generic flinch-secrets --from-env-file=flinch.env
rm flinch.env
```

with one `KEY=value` per line:

```
FLINCH_WEB_TOKEN=<generated above>
RADARR_API_KEY=<radarr api key>
SONARR_API_KEY=<sonarr api key>
MAINTAINERR_API_KEY=<maintainerr api key>
PLEX_URL=http://plex:32400
PLEX_TOKEN=<plex token>
TAUTULLI_URL=http://tautulli:8181
TAUTULLI_API_KEY=<tautulli api key>
```

| Key | Needed | Notes |
| --- | --- | --- |
| `FLINCH_WEB_TOKEN` | yes, for the UI | the UI's login: any long random string. Without it the UI says how to set it and the API refuses everything |
| `RADARR_API_KEY` | yes | Radarr > Settings > General |
| `SONARR_API_KEY` | yes | Sonarr > Settings > General |
| `MAINTAINERR_API_KEY` | no | Maintainerr checks no key today; FLINCH sends one when it is set |
| `PLEX_URL`, `PLEX_TOKEN` | no | or set both in the UI (Settings > Plex), which then wins over the Secret; the pair always comes from one place |
| `TAUTULLI_URL`, `TAUTULLI_API_KEY` | no | without them FLINCH reads Tautulli's address from Maintainerr, but Maintainerr returns the key masked, so set them here to use Tautulli |

Leave out the keys you do not use. The Radarr and Sonarr keys are required
(the daemon exits without them), and the UI stays locked without the token.

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

Open <http://localhost:7911> and paste the token. To read it back:

```bash
kubectl -n media get secret flinch-secrets -o jsonpath='{.data.FLINCH_WEB_TOKEN}' | base64 -d
```

The browser keeps it until you press **Lock**. To change it, update the Secret
and restart `flinch-web` (`kubectl -n media rollout restart deploy/flinch-web`).

To publish the UI through your ingress controller, turn on the `ingress`
component in `deploy/kustomization.yaml` and set its host (the commented
`patches:` example) and certificate (`deploy/ingress/flinch-web-ingress.yaml`).
It is HTTPS only, because the browser sends the token with every request. For
single sign-on in front of the token, add your proxy's forward-auth annotation
(Authelia, Authentik, oauth2-proxy). Keep it off the internet: whoever gets
past it can schedule deletions.

## First run

Enforcement is off by default (Settings > Safety > Enforcement). A fresh
install only plans and logs: nothing is sent to Maintainerr. Each write FLINCH
would send is printed instead.

The first cycle starts when the daemon starts. The next one follows after the
Scan interval (Settings > Schedule, one hour by default), or when you press
**Trigger run** in the UI.

Open the UI and read the Overview. Settings shows the state of each connection
(Radarr, Sonarr, Plex, Tautulli, Maintainerr). Then read the daemon log:

```bash
kubectl -n media logs deploy/flinch-arrd        # add -f to follow
```

The per-action lines are shaped for grep. With enforcement off:

```
[dry-run] would exclude ratingKey 4511 (mediaId 4502)
[dry-run] would add ratingKey 812 (mediaId 812) to collection 3
```

With enforcement on, each action ends with its outcome:

```
[flinch-arrd] protect sonarr-11-s2 (ratingKey 4511): done, verified
[flinch-arrd] schedule radarr-7 (ratingKey 812, 41.2 GiB) into collection 3: failed, retried next cycle: …
```

On the volume, `plan.json` holds the summary of the last cycle and
`status.json` its `sync` block.

Rules the daemon obeys (all under test): favorites, keep collections and the
newest season are immune; an item you excluded in Maintainerr is a keep;
missing watch state fails closed; a Maintainerr **201 with `{"code":0,
"result":"Failed - no metadata"}` is a failure**, never a silent success;
nothing is handed to a Maintainerr older than 3.10, or to a collection that is
inactive, of the wrong type, bound to another Plex library, or whose *arr
action frees nothing or is one Maintainerr refuses for its type; with
enforcement off, every write is only printed. An item nobody finished goes
only to a Leaving Soon collection that is active, shown in Plex and has a
window of at least one day; while none validates, it waits instead of going
to a delete collection. FLINCH releases only exclusions it created, and only
for an item proven gone: no file in Radarr/Sonarr and absent from a complete
Plex listing.

## Maintainerr setup

FLINCH fills Maintainerr collections; Maintainerr deletes. The collection
names are in Settings > Maintainerr collections.

1. Make the two delete collections, Movies (`Watched Movies Cleanup` by
   default) and Seasons (`Watched Seasons Cleanup`), manual-membership
   collections with a delete action. If they also keep their own rules,
   Maintainerr will delete what those rules select regardless of the 80%
   ceiling. A season collection needs "Unmonitor and delete existing episodes"
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
   unwatched evictions wait. A blank title sends them straight to the delete
   collections instead.
3. If Seerr is configured in Maintainerr, turn on **Force delete Seerr
   request** on every collection FLINCH uses. Otherwise a removed title's
   Seerr request stays until Seerr's availability sync notices, and it cannot
   be requested again at once. FLINCH warns about each such collection but
   still hands items to it.

## Radarr and Sonarr setup

FLINCH never writes to Radarr or Sonarr. One setting in each keeps a file
deleted outside FLINCH from being downloaded again: turn on **Unmonitor
Deleted Movies** in Radarr and **Unmonitor Deleted Episodes** in Sonarr
(Settings > Media Management). FLINCH lists such deletions on the Overview,
with whether each one will download again.

## Turn on enforcement

Before you do:

1. Run at least one cycle with enforcement off and read the log: every write
   FLINCH would send is printed, with the Plex ratingKey it targets.
2. Finish the [Maintainerr setup](#maintainerr-setup) and the
   [Radarr and Sonarr setup](#radarr-and-sonarr-setup).
3. If Tautulli is connected, turn **Keep History** on for every user and for
   every library FLINCH manages (Tautulli > Users / Libraries > edit). Where it
   is off, the log says `tautulli keeps no history for …` and never-played
   reclaim stays held, because Tautulli's silence then proves nothing.
4. Mark anything the household must never lose with the keep tag (Settings >
   Safety > Keep tag, `flinch-keep` by default): as a tag in Radarr/Sonarr, a
   label on the movie or show in Plex, or by putting it in a Plex collection
   with that name (a collection can also hold single seasons). The log prints
   `plex keep markers ('flinch-keep'): N item(s) kept`, and the UI lists them
   under "Kept by you".
5. Optional: if you use Recyclarr, merge `deploy/recyclarr/recyclarr.flinch.yml`
   into your Recyclarr config and run `recyclarr sync --preview` first.

Then turn on Settings > Safety > Enforcement (**Schedule deletions**) and
press **Save settings**. It applies on the next run.

`FLINCH_DRY_RUN=1` on the daemon keeps every write printed only, whatever the
setting says. `FLINCH_ENFORCE=1` does the opposite and turns enforcement on
regardless of the setting; leave it unset.

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
| `capacity.json` | which disks are latched (evicting toward the release mark) | a disk between 75% and 80% stops evicting until it crosses 80% again |
| `evictions.json` | bytes handed to Maintainerr that a recycle bin may still hold or that are held, and each hand-over for 120 days (to tell FLINCH's deletions from others) | evictions in flight or held go uncredited, and FLINCH's own recent deletions may be listed as ones it did not make |
| `protected.json`, `scheduled.json` | exclusions and collection members FLINCH created | FLINCH forgets it owns them and leaves them alone |
| `candidates.json` | grace-run streaks | every candidate re-earns its grace window |
| `operator-keeps.json` | the cards your own Maintainerr exclusions keep, as last read | rewritten by the next cycle that reads Maintainerr; an outage before then plans without them (nothing is synced during it) |
| `playback.json`, `tautulli.json` | raw plays, the daily fit's input | fitting waits for new history |
| `fit.json` | the last daily fit: which model was judged (recalibrated priors or full fit), its out-of-fold scores against the priors, why it was or was not adopted, and the genre play-rates genre taste reads | refitted on the next cycle; no genre taste until then |
| `weights.json` | the adopted model, present only while it beats the priors out of fold | the priors run until a model earns adoption again |
| `benchmark.json` | the last `flinch-fit --against … --write`: an external model scored against FLINCH on the same panel | the Forecast model card shows no comparison until the next run |
| `episode-guids.json` | episode `plex://` GUIDs of resolved shows, for plays recorded before a library migration (refreshed at most daily, only while such plays exist) | re-read from Plex on the next cycle that needs it |
| `arr-history.json` | when each title was on disk, and the files removed in the last 30 days, from the Radarr and Sonarr import and delete history (refreshed daily) | read again from the *arrs on the next cycle |

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
  - { name: flinch, newName: ghcr.io/tobhoster/flinch, newTag: "0.1.0" }
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

- **The token is the login.** `flinch-web` refuses every API request without
  `Authorization: Bearer <FLINCH_WEB_TOKEN>`, and everything when no token is
  set. Anyone who has it can change Settings, including Enforcement, so treat
  it like the *arr API keys.
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
