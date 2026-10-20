# Running a latch host in Docker

Use an image containing latch, non-setuid bubblewrap, CA certificates, and the
programs your host needs. Install these while building the image, then run latch
as a non-root user. The state and the shared folders must be writable by that user.
The normal latch login still applies.

From the repository root, with an existing host state directory and area:

```sh
docker run --rm --name latch-host \
  --user 1000:1000 --cap-drop=ALL \
  --security-opt=no-new-privileges=true \
  --security-opt="seccomp=$PWD/latch/docker/seccomp.json" \
  -v "$PWD/host-state:/state" -v "$PWD/shared:/shared" \
  latch-image latch --state-dir /state serve --no-dashboard /shared
```

The folders after `serve` are the ones it shares. Mount only the state and the
folders this host needs. Leave Docker's masked/read-only system paths in place.
File and Google workers have no `/proc`. Exec workers reuse the container's
existing `/proc` read-only; this retains Docker's masks but exposes container
process metadata rather than a private process view. All workers get a minimal
`/dev` and their permitted writable folders. Exec sees the container's whole
filesystem read-only, with the shared folders writable on top, a private empty
`/tmp` and `/run`. File and Google workers get only explicit runtime mounts.
State is covered by an empty read-only mount, including when an area contains it;
the running latch is mounted read-only, and folders above either inside a shared
folder are mount points, so they cannot be renamed.

The profile is Moby's published Docker default at commit
`4b8214fc4971e06172938a1f1554ccff6647ee80`, plus four allowance groups:

- `clone` with `CLONE_NEWUSER` (amd64/arm64; excludes new UTS/cgroup namespaces).
- `unshare(CLONE_NEWUSER)`.
- `mount` and `pivot_root`.
- `umount2` with `MNT_DETACH`.

Other default rules remain, including `clone3` returning ENOSYS. This is a pinned
published baseline, not a claim that every Docker version embeds identical rules.
The extra mount syscalls expose more kernel surface; kernel user-namespace and
capability checks still apply. No host capabilities are granted.

## Compose

```yaml
services:
  latch:
    image: latch-image
    user: "1000:1000"
    cap_drop: [ALL]
    security_opt:
      - no-new-privileges:true
      - seccomp=./latch/docker/seccomp.json
    volumes:
      - ./host-state:/state
      - ./shared:/shared
    command: [latch, --state-dir, /state, serve, --no-dashboard, /shared]
```

## AppArmor hosts: unverified

On Ubuntu, Docker's `docker-default` AppArmor profile denies mounts independently
of seccomp. An administrator must load and validate a suitable container profile
before selecting `--security-opt=apparmor=latch-bwrap` (Compose:
`apparmor=latch-bwrap` in `security_opt`). The candidate
[`docker/latch-bwrap.apparmor`](docker/latch-bwrap.apparmor) is **untested on an
AppArmor-enforcing host**; only its syntax was checked during research. It is
derived from Moby's default, replacing the mount denial with mount, pivot and
user-namespace allowances. Its broad mount allowance needs host-side review.
It does not replace the separate bare-host bwrap user-namespace profile installed
by install.sh. AppArmor validation is outside this POC's acceptance.

## Kubernetes: untested

Use a non-root securityContext, `allowPrivilegeEscalation: false`, and
`capabilities: {drop: [ALL]}`. A node administrator must provision an
architecture-matching OCI seccomp profile and select it with
`seccompProfile: {type: Localhost, localhostProfile: profiles/latch-bwrap.json}`.
The Docker JSON uses conditional rules and must be converted for the target
architecture before use with CRI; do not use it as an OCI profile unchanged.
AppArmor nodes also need a loaded and validated Localhost AppArmor profile.
Keep default proc masking. No Kubernetes deployment was validated for this POC.

## Setup refusals

Latch probes the worker sandbox before registering. Missing bubblewrap calls for
install.sh. Namespace or mount failures can come from the runtime seccomp policy,
AppArmor, or a host that disables unprivileged user namespaces. Check the cause
printed by bubblewrap and the host's policy. Do not use `--privileged`, add
`SYS_ADMIN`, or disable seccomp as a workaround. There is no unsandboxed host mode.
