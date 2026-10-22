# Running a latch host in Docker

Use an image containing latch, non-setuid bubblewrap, CA certificates, and the
programs your host needs.
Install these while building the image, then run latch
as a non-root user. The state and intended write destinations must be writable by that user.
The normal latch login still applies.

From the repository root, with an existing host state directory and home workspace:

```sh
docker run --rm --name latch-host \
  --user 1000:1000 --cap-drop=ALL \
  --security-opt=no-new-privileges=true \
  --security-opt=systempaths=unconfined \
  --security-opt="seccomp=$PWD/latch/docker/seccomp.json" \
  -e HOME=/home/latch -v "$PWD/host-state:/state" -v "$PWD/work:/home/latch/work" \
  latch-image latch --state-dir /state serve --foreground
```

Exec and file need no shared-folder setup. Both read the container user's
home and system files. Mount workspaces under that home; declare exec writes
with repeated `--write PATH`, and use `--network` for IP networking (off by
default). File put writes only its judged destination. Quote leading `~` to
expand it on the host. Mount only the state and folders this host needs.
The remote judge and sealed audit see argv, workdir, writes and network;
keep credentials out of argv.
Docker's `systempaths=unconfined` is required to mount private procfs in a nested
user namespace; with Docker's default proc masks the kernel refuses it. Keep
the capability drop, no-new-privileges and seccomp policy above. The worker
never reuses the container's process view: exec gets read-only `/proc` for its
own PID namespace; file and Google workers have no `/proc`. All workers get a minimal
`/dev` and their permitted writable folders. Exec and file workers see the
container's system folders (`/usr`, `/bin`, `/sbin`, `/lib*`, `/etc`, `/opt`,
`/var/lib`). Exec reads home except latch state, writes only its declared paths,
existing plain tool-cache directories and temp, and has no IP network without
`--network`. Missing caches and cache aliases are skipped; latch creates none.
On Linux 7.1+ with Landlock ABI 9+ enabled, pathname Unix sockets are
restricted to private temp and abstract sockets to this worker's domain.
On older kernels (including Ubuntu 24.04's 6.8 and Raspberry Pi 6.12), or
with Landlock disabled, seccomp blocks named Unix sockets everywhere,
including private temp, and datagram socketpairs. Connected AF_UNIX stream
and sequenced-packet socketpairs remain available offline for Python asyncio,
Node subprocesses and Rust process spawning.
Exec and IP networking still work; programs requiring named Unix sockets do not
work on those kernels. Host daemon sockets stay blocked in both modes.
Exec gets a private empty `/tmp`, `/run` and `TMPDIR`; workdir defaults to the
first write folder, otherwise home. Writes must stay inside home, outside the
hidden places, and cannot grant home itself.

Hidden paths are `~/.latch` and this host's state directory (macOS also hides
`~/Library` except `~/Library/Caches`). Empty read-only mounts cover them even
inside a workspace. Other home files, including credential dotfiles and other
state folders, are readable; their use is judged. The running latch is mounted
read-only; folders above protected mounts cannot be renamed. Google workers
get only explicit runtime mounts.

Exec, file and Google workers cannot create further user namespaces after
bubblewrap sets up their sandbox. Browser workers retain nested namespaces
for the engine's own sandbox.

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
      - systempaths=unconfined
      - seccomp=./latch/docker/seccomp.json
    environment:
      HOME: /home/latch
    volumes:
      - ./host-state:/state
      - ./work:/home/latch/work
    command: [latch, --state-dir, /state, serve, --foreground]
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
The runtime must permit private procfs in nested user namespaces. No Kubernetes
deployment was validated for this POC.

## Setup refusals

Latch probes the worker sandbox before registering. Missing bubblewrap calls for
install.sh. Namespace or mount failures can come from the runtime seccomp policy,
AppArmor, or a host that disables unprivileged user namespaces. Check the cause
printed by bubblewrap and the host's policy. Do not use `--privileged`, add
`SYS_ADMIN`, or disable seccomp as a workaround. There is no unsandboxed host mode.
