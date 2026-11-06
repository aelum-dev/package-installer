# Packages (`aelum-package-installer`)

A package manager for the Aelum desktop: GTK4/libadwaita, one Rust crate.
Five pages over one in-memory model of pacman's on-disk state:

- **Installed** — the packages the user chose (`%REASON%`, the `-Qe` view),
  each removable with its orphaned dependencies; the confirmation shows what
  would break and what would be freed. A package with a newer version in its
  repo shows the new version and an Update button.
- **Browse** — every package in the configured sync repositories (including
  `[aelum]`, tagged), searched instantly, installed with one click.
- **Aelum** — the `[aelum]` repository's apps this system does not have yet.
- **AUR** — the Arch User Repository, searched when you type. A row says what
  that name is on this system (not installed / installed / older than the
  AUR's version); installing one brings its repository dependencies through
  the same polkit action, builds the PKGBUILD here with `makepkg`, and
  installs the result with `pacman -U` — one log dialog for the whole
  request. Nothing is fetched from the AUR unasked, so the tab has no badge.
- **Updates** — every package with a newer version in its repo, per-package
  Update and Update All. The databases are refreshed at every start (quiet
  on success, a log dialog on failure; the Refresh button spins while the
  check runs), and the tab carries a badge when updates are waiting.

Opening a `.pkg.tar.zst` installs it through the same flow. Every package
transaction runs as root through `aelum-package-installer-helper` under one
polkit action (`dev.aelum.package.manage`, granted to the local active wheel
session — the `aelum-snapshot-restore` pattern), and its output is always
shown: a failed operation never goes silent. An AUR build is the one part
that runs as you, in `~/.cache/aelum/package-installer/aur`, because
`makepkg` refuses to run as root.

The model is read directly from `/var/lib/pacman` (sync DBs + local DB are
alpm's documented formats); the AUR is read over its RPC with `curl`, and
`%PROVIDES%` is read from both databases so a dependency answered under
another name is not asked for again. Version comparison is a tested
rpmvercmp in `src/vercmp.rs`.

## Distribution

Shipped through the **online `[aelum]` repository** — that is its purpose:
proving new apps can reach installed users through the repo.

```bash
sudo pacman -S aelum-package-installer   # on any Aelum install (repo is pre-trusted)
```

It is listed in `live-iso/packages.x86_64` too: the ISO build resolves `[aelum]`,
bakes the result into the offline mirror, and fresh installs ship it preinstalled.
Updates still come from the repo.

## Commands

```bash
cargo test                                # db parsing, vercmp, model, txn argv, AUR
cargo test -- --ignored                   # real host + real AUR: pacman -Qqe/-Qu, the
                                          # base-devel metapackage, the live RPC format
cargo clippy --all-targets                # must stay at 0 warnings
cargo run                                 # GUI (reads the host's /var/lib/pacman)
./scripts/build-pkg.sh [--no-sign]        # Arch package + checksum + GPG .sig → dist/
../repo/publish-repo.sh dist/<pkg>        # publish to [aelum]
./scripts/bump-version.sh X.Y.Z [--changes "..."]   # release: bump, commit, publish, prune
```

The real-install checks belong to the QEMU guest (`make -C docker test-boot`,
install the package from disk) — see the workspace `AGENTS.md`.

## License

GPL-3.0 (`LICENSE`).
