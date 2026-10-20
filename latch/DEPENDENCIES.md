# latch dependencies

Every crate `latch` depends on directly, and why. The usage numbers are from
the library audit (crates.io and GitHub APIs, pulled 2026-10-14). `rand` is not in
the audit; its row was pulled from the same APIs on 2026-10-15, and the vault's six
(`aes-gcm` to `zip`, named in the browser spec) on 2026-10-19, and `qrcode` on 2026-10-19. Downloads
are all-time / last 90 days. The bar for a new crate: widely used, actively
maintained, from a known owner. Add a row here in the same change that adds a
crate to `Cargo.toml`.

| Crate | Used for | Downloads (all / 90 d) | Stars | Owner | Version (released) | License |
|---|---|---|---|---|---|---|
| `clap` (derive, env) | Command-line parsing, `--help`, `$LATCH_HOST` fallback | 1,228M / 254M | 16.8k | clap-rs org | 4.6.7 (2026-09-14) | MIT/Apache-2.0 |
| `tokio` | Async runtime; iroh and reqwest are built on it | 1,065M / 257M | 33.4k | tokio-rs org | 1.53.3 (2026-10-13) | MIT |
| `iroh` | Peer identity (`SecretKey`, `EndpointId`) and the QUIC transport | 3.3M / 2.1M | 12.7k | n0-computer org | 1.3.0 (2026-09-28) | MIT/Apache-2.0 |
| `jiff` | Time stamps in the audit log and the host's log | 226M / 84M | 3.0k | BurntSushi | 0.2.38 (2026-10-06) | Unlicense/MIT |
| `libc` | `killpg` for `exec` timeouts; signal numbers and names | 1,779M / 410M | 2.6k | rust-lang org | 0.2.190 (2026-10-02) | MIT/Apache-2.0 |
| `seccompiler` (Linux only) | Worker seccomp filters after bubblewrap; pinned | 24.3M / 8.1M | 131 (rust-vmm monorepo) | rust-vmm org | 0.5.0 (2025-03-07) | Apache-2.0/BSD-3 |
| `rand` | Jitter for the heartbeat and its backoff | 1,909M / 512M | 2.1k | rust-random org | 0.10.3 (2026-09-20) | MIT/Apache-2.0 |
| `reqwest` (json, rustls) | HTTPS client for the Plow directory | 801M / 215M | 11.9k | seanmonstar | 0.13.5 (2026-09-08) | MIT/Apache-2.0 |
| `serde` | Plain structs to and from JSON | 1,531M | 10.9k | serde-rs org | 1.0.229 (2026-07-18) | MIT/Apache-2.0 |
| `serde_json` | The JSON format for serde | 1,434M | 5.7k | serde-rs org | 1.0.154 (2026-10-11) | MIT/Apache-2.0 |
| `sha2` | sha256 for the audit log's hash chain | 1,062M / 302M | 2.3k | RustCrypto org | 0.11.1 (2026-10-12) | MIT/Apache-2.0 |
| `tempfile` | Per-call Google folders and throwaway test state | 890M / 208M | 1.5k | Stebalien | 3.27.0 (2026-03-11) | MIT/Apache-2.0 |
| `aes-gcm` | The vault: items and its data key sealed with AES-256-GCM | 182M / 57M | 968 (AEADs monorepo) | RustCrypto org | 0.11.1 (2026-08-21) | Apache-2.0/MIT |
| `csv` | The vault: reading password managers' CSV exports | 269M / 57M | 2.0k | BurntSushi | 1.4.0 (2025-10-17) | Unlicense/MIT |
| `psl` | The vault: registrable domains (the public-suffix list, private suffixes included) | 36M / 7.9M | 38 | addr-rs org | 2.1.239 (2026-10-03); pinned in Cargo.lock, since each release is a new list | MIT/Apache-2.0 |
| `totp-rs` (std) | The vault: authenticator codes (checked against RFC 6238's vectors) | 11M / 3.1M | 277 | constantoine | 6.0.0 (2026-08-06) | MIT |
| `url` | The vault: reading sites and origins | 969M / 219M | 1.6k | servo org | 2.5.8 (2026-01-05) | MIT/Apache-2.0 |
| `zip` (deflate) | The vault: reading 1Password's 1PUX exports | 307M / 87M | 360 | zip-rs org | 8.6.0 (2026-04-25) | MIT |
| `qrcode` (no default features) | `latch vault link`'s QR code in the terminal | 23M / 6.0M | 563 | kennytm | 0.14.1 (2024-07-05); the last release, small and finished rather than abandoned (repository active 2025-08) | MIT/Apache-2.0 |

TLS comes with reqwest and iroh: `rustls` (1,003M / 232M downloads, 7.7k
stars, rustls org) on its `aws-lc-rs` backend (262M / 98M, aws org). iroh is
built without its default `ring` backend, so the binary carries one crypto
library.

Not used, although the audit lists them: `anyhow` and `thiserror`. A failure
is one plain struct (`src/failure.rs`) with an exit kind, a code and a
message, which is all the error contract needs.
