# Packages (`aelum-package-installer`)

A package manager for the Aelum desktop: GTK4/libadwaita, one Rust crate.
Three pages over one in-memory model of pacman's on-disk state:

- **Installed** — the packages the user chose (`%REASON%`, the `-Qe` view),
  each removable with its orphaned dependencies; the confirmation shows what
  would break and what would be freed. An update banner + Update All when a
  sync repo carries something newer.
- **Updates** — every package with a newer version in its repo, per-package
  Update and Update All. The databases are refreshed at every start (quiet
  on success, a log dialog on failure; the Refresh button spins while the
  check runs), and the tab carries a badge when updates are waiting.
- **Browse** — every package in the configured sync repositories (including
  `[aelum]`, tagged), searched instantly, installed with one click.

Opening a `.pkg.tar.zst` installs it through the same flow. Every operation
runs as root through `aelum-package-installer-helper` under one polkit action
(`dev.aelum.package.manage`, granted to the local active wheel session — the
`aelum-snapshot-restore` pattern), and its output is always shown: a failed
operation never goes silent.

The model is read directly from `/var/lib/pacman` (sync DBs + local DB are
alpm's documented formats); the only subprocesses are transactions. Version
comparison is a tested rpmvercmp in `src/vercmp.rs`.

## Distribution

Shipped through the **online `[aelum]` repository** — that is its purpose:
proving new apps can reach installed users through the repo.

```bash
sudo pacman -S aelum-package-installer   # on any Aelum install (repo is pre-trusted)
```

It is deliberately **not** in the ISO/offline mirror yet; see
`../package-installer-plan.md` for the plan and the measured caveats.

## Commands

```bash
cargo test                                # db parsing, vercmp, model, txn argv
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
