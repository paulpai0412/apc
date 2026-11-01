# APC Oracle ARM64 container contract

This packages the existing application, not a second app: the Vite UI, authenticated
Rust HMI, native Rust numerical runner, Python/NumPy research code and the real
Windows COM local simulator. The x64 simulator runs under pinned Hangover 11.16
(ARM64 Wine with FEX); registration and tests stay in this service's Wine prefix.
Upstream: https://github.com/AndreRH/hangover/releases/tag/hangover-11.16 . Its
Debian archive SHA-256 is pinned in Dockerfile. Wine/FEX/system package copyright
and license files remain under `/usr/share/doc`; simulator license is in `/app/com`.

## Build and release

Only GitHub Actions builds. `APC ARM64 container` uses the native
`ubuntu-24.04-arm` runner, tests both Rust crates, the frontend and Python, then
runs the real HMI and COM acceptance with no network, no capabilities and a
read-only root filesystem. A failing build or acceptance does not publish an
image. The push job publishes `ghcr.io/paulpai0412/apc:sha-<source SHA>`, pulls its
immutable digest and repeats acceptance before producing `image-contract.json`
and `container-evidence` as workflow artifacts. PR checks build/test without
publishing. There is no deploy job, SSH credential or Oracle source build.

A job's temporary `GITHUB_TOKEN` gets package write scope only to publish this
repository's image. No new stored registry credential is configured. Publishing
never changes package visibility: a new GHCR package defaults private. A public
source repository does not prove anonymous image pull is available. Before VM
startup, the host owner must verify the exact digest can be pulled with existing
authorized registry access. If not, obtain a specifically approved pull method;
do not make the package public or copy a build token onto the VM.

## VM handoff (host owner only)

Use this Compose file in the approved APC rootless Docker account, never the
Teams runtime. No host changes are performed by this repository. Confirm native
ARM64, a 4 KiB host page size (FEX requirement), sufficient free disk/RAM, the
approved account/data location and usable registry access. Set `APC_IMAGE` to the
**tested** `ghcr.io/paulpai0412/apc@sha256:...` from `image-contract.json`. Do not
substitute a tag. Then:

```sh
docker compose -f deploy/docker/compose.yaml config --quiet
docker compose -f deploy/docker/compose.yaml pull
docker compose -f deploy/docker/compose.yaml up -d --no-build --wait --wait-timeout 300
docker compose -f deploy/docker/compose.yaml ps
```

Only host loopback `127.0.0.1:18082` is bound. The Compose network is internal;
there is no external OPC, historian, supervisor, plant simulator or controller
host. Real device controls have no configured profiles, OPC is disabled and the
HMI's research-mode Runtime installation/control gate remains in force. There is
no auto-arm, Engage or automatic deployment. Public access/firewall/IAM changes
are outside this contract.

The HMI retains production `Secure`, HttpOnly, SameSite=Strict session cookies,
CSRF/RBAC and mandatory first password change. It uses the existing fresh-database
bootstrap convention; change that password immediately through an approved local
access path. HTTP on an arbitrary hostname is not a supported authenticated
browser deployment. Host-loopback/SSH forwarding does not by itself establish
HTTPS on a remote hostname. A future HTTPS endpoint requires its own approved
certificate/access configuration. No test bypass account is enabled in this
image. The CI API client explicitly forwards its own cookie on container
loopback; that check does not claim browser TLS validation.

## What is functional and what is absent

With acceptance passing, real login, CSV import/mapping/cleaning, saved drafts,
model identification, immutable studies, fixed-model search and local numerical
simulations use the actual HMI/Python/Rust pipeline. The COM check is never
substituted with a mock or a successful no-op. The container smoke compares 32
actual closed-loop scans through direct and COM transports and asserts real COM
read/write counters. This is packaging acceptance, **not** the full three-month
`validation/verify_autoresearch.py` qualification or factory commissioning.

Runtime's overview, equipment/3D and historical trend UI remain in the same
application. This isolated deployment has no commissioned equipment, real-time
telemetry or QuestDB history. Empty/disconnected state is expected; it does not
fabricate live equipment, old curves, connected status or accepted control.
Users can import research history and use isolated research/twin artifacts.
Live historian/OPC capabilities need a separately commissioned deployment, not a
change to this container's defaults.

## Persistence, health and recovery

`apc_apc-data` is the sole persistent volume, mounted at `/data` and owned by
container UID/GID 10001. It contains authentication/audit SQLite data, Runtime
registries, research datasets/studies/runs, reports and the dedicated Wine prefix.
The container runs as this non-root user with no capabilities and no-new-privileges.
Its root filesystem is read-only; only the data volume and bounded `/tmp` tmpfs
are writable. Resource limits are 6 GiB RAM / 2 CPUs / 512 processes. No Docker
socket or host directories are mounted.

Readiness requires the real simulator's COM tests to have passed on this start
and an actual HMI `/api/auth/me` response. It does not mean an OPC connection or
plant readiness. A simulator/architecture failure prevents HMI startup; inspect
`docker compose ... logs --tail=100`. Do not disable the check to get green status.

For an upgrade, retain the previous tested digest and back up the volume while
stopped (including SQLite side files; do not copy a live database). Keep that
backup private. Pull and start the new digest with `--no-build`, check health,
login, stored drafts/datasets and disconnected Runtime state. If validation fails,
stop the service, restore the pre-upgrade data backup if a schema changed, set the
previous digest and start again. Never run `down -v`, prune the data volume or
share its contents. In-memory login sessions intentionally expire on restart;
research partial runs reconcile using existing application logic and are not
silently restarted or deployed. The CI restart smoke confirms stored draft and
account persistence; it is not an upgrade migration qualification.
