# Panamax

![image](https://user-images.githubusercontent.com/1247908/132435079-b703bd5d-c139-4a73-818f-51746353b3ea.png)

Panamax mirrors `rustup` toolchains and a slim copy of crates.io, so you can keep building Rust, and rebuild infrastructure, when the internet goes down.

- **rustup:** toolchains are synced ahead of time with `panamax sync`.
- **crates.io baseline:** `panamax sync` keeps every crate, but not every version. Per crate it keeps the newest version of each of the newest 3 semver lines, the newest that builds on each pinned toolchain, and everything those depend on. That's about 745k of 3.5M versions. Baseline versions are never deleted.
- **Everything else:** `panamax serve` fetches any other version the first time `cargo` asks for it while crates.io is reachable, and keeps it until it goes unused for 180 days.
- **Offline:** the index served to `cargo` only lists versions on disk, so `cargo` only ever resolves versions that will download.

This is a fork of [panamax-rs/panamax](https://github.com/panamax-rs/panamax), which is no longer actively maintained.

## Installation

This fork is not published on crates.io. Install it from git:

```
$ cargo install --locked --git https://github.com/mvdschee/panamax
```

Alternatively, you can clone this repository and `cargo build` or `cargo run` within it.

## Docker

Panamax is available as a docker image on GHCR, so you can run:

```
$ docker run --rm -it -v /path/to/mirror/:/mirror --user $(id -u) ghcr.io/mvdschee/panamax init /mirror
(Modify /path/to/mirror/mirror.toml as needed)
$ docker run --rm -it -v /path/to/mirror/:/mirror --user $(id -u) ghcr.io/mvdschee/panamax sync /mirror
(Once synced, serve the mirror)
$ docker run --rm -it -v /path/to/mirror/:/mirror --user $(id -u) -p8080:8080 ghcr.io/mvdschee/panamax serve /mirror
```

Alternatively, you can run panamax in a bare-metal environment like below.

### Init

In Panamax, mirrors consist of self-contained directories. To create a mirror directory `my-mirror`:

```
$ panamax init my-mirror
Successfully created mirror base at `my-mirror`.
```

Use `--ignore-rustup` to skip the rustup half and only run the crate cache.

### Modify mirror.toml

Within the directory, you'll find a `mirror.toml` file with the rustup settings: which channels, platforms and pinned versions to keep. The `pinned_rust_versions` toolchains also decide which crate versions count as "builds on this toolchain" for the baseline. Under `[crates]`, `sync = false` turns off the baseline for a cache-only mirror.

### Sync

`panamax sync` downloads the rustup toolchains, then updates the crates baseline:

```
$ panamax sync my-mirror
Syncing Rustup repositories...
[1/5] Syncing rustup-init files... ██████████████████████████████████████████████████████████████ 27/27 [00:00:06]
[2/5] Syncing latest stable...     ████████████████████████████████████████████████████████████ 602/602 [00:09:02]
[3/5] Syncing latest beta...       ████████████████████████████████████████████████████████████ 524/524 [00:07:29]
[4/5] Syncing latest nightly...    ████████████████████████████████████████████████████████████ 546/546 [00:08:56]
[5/5] Cleaning old files...        ████████████████████████████████████████████████████████████ 546/546 [00:00:00]
Syncing Rustup repositories complete!
Sync complete.
```

The crates baseline uses a shallow checkout of the crates.io git index (`crates.io-index/`, about 400 MB of git data). Computing it takes about 1 GB of memory. It downloads kept versions that are missing, then deletes files that are outside the baseline and haven't been used for 180 days.

- `--dry-run` reports what would be downloaded and deleted, and changes nothing except the index checkout.
- A run refuses to delete more than 10% of all crate files unless you pass `--allow-mass-delete`.

Run it in a nightly cronjob.

## Server

```
$ panamax serve my-mirror
Running HTTP on [::]:8080
```

This serves the rustup mirror and the crate cache. The index page explains how to set up a client.

Pass `--offline` to never contact crates.io and serve only what is cached, for example to test that your projects still build without internet.

The server speaks plain HTTP. For HTTPS, put it behind a reverse proxy. The download URL in `index/config.json` is built from the request's `Host` and `X-Forwarded-Proto` headers, so the proxy must pass both.

## Configuring `rustup` and `cargo`

Once you have a mirror server set up and running, it's time to tell your Rust components to use it.

### Setting environment variables

In order to ensure `rustup` knows where to look for the Rust components, we need to set some environment variables. Assuming the mirror is hosted at http://panamax.internal/:

```
export RUSTUP_DIST_SERVER=http://panamax.internal
export RUSTUP_UPDATE_ROOT=http://panamax.internal/rustup
```

These need to be set whenever `rustup` is used, so these should be added to your `.bashrc` file (or equivalent).

### Installing `rustup`

If you already have `rustup` installed, this step isn't necessary, however if you don't have access to https://rustup.rs, the mirror also contains the `rustup-init` files needed to install `rustup`.

Assuming the mirror is hosted at http://panamax.internal/, you will find the `rustup-init` files at http://panamax.internal/rustup/dist/. The `rustup-init` file you want depends on your architecture. Assuming you're running desktop Linux on a 64-bit machine:

```
wget http://panamax.internal/rustup/dist/x86_64-unknown-linux-gnu/rustup-init
chmod +x rustup-init
./rustup-init
```

This will let you install `rustup` the similarly following the steps from https://rustup.rs. This will also let you use `rustup` to keep your Rust installation updated in the future.

### Configuring `cargo`

`Cargo` also needs to be configured to point to the mirror. This can be done by adding the following lines to `~/.cargo/config.toml` (creating the file if it doesn't exist):

```
[source.panamax]
registry = "sparse+http://panamax.internal/index/"
[source.crates-io]
replace-with = "panamax"
```

`Cargo` should now be pointing to the correct location to use the mirror.

### Testing configuration

You've now set up a Rust mirror! In order to make sure everything is set up properly, you can run a simple test:

```
$ cargo install ripgrep
```

This will install the grep-like `rg` tool (which is a great tool - props to burntsushi!). If `cargo` successfully downloads and builds everything, you have yourself a working mirror. Congratulations!


### Proxies

If panamax itself has to reach the internet through a proxy, set `HTTPS_PROXY` (and `HTTP_PROXY`) to something like `http://your.proxy:1234` before running `sync` or `serve`.

## Development

```
$ make check            # clippy, fmt and tests
$ make local            # serve a crate cache in .local/mirror on port 8282
$ make local OFFLINE=1  # the same, serving only what is cached
$ make local-check      # fetch the crates in Cargo.lock through it, with a fresh cargo home
$ make dev              # make local, restarting on code changes (needs watchexec)
$ make release          # tag the version in Cargo.toml and push it, which publishes the image
```

## License

Licensed under the terms of the MIT license and the Apache License (Version 2.0)

See [LICENSE-MIT](LICENSE-MIT) and [LICENSE-APACHE](LICENSE-APACHE) for details.

### Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any
additional terms or conditions.
