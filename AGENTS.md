# AGENTS.md — working notes for coding agents

A package manager for the Aelum desktop: GTK4 + libadwaita, one Rust crate.
Binary `package-installer`, app ID `aelum.packageinstaller.Packages`, Arch
package `aelum-package-installer`. It reads pacman's on-disk state directly and
shells out only for transactions.

**Distribution channel: the online `[aelum]` repository** — published with
`../repo/publish-repo.sh`, installed on installed systems with `pacman -S`.
Deliberately **not** in `live-iso/packages.x86_64` yet: the app is the test of
whether the repo can carry new apps to users, so it must not be baked into an
ISO while that is being measured.

## Read these first

- **`../package-installer-plan.md`** — the plan of record: the page design, the
  decisions already taken, and every measured fact below with its evidence. It
  lives at the workspace root and is untracked by design (the root is not a git
  repo); if it is missing you are in a fresh clone of this repo alone.
- `../AGENTS.md` → the `package-installer/` section, for how this repo fits the
  distro (polkit pattern, the repo channel, the QEMU test guest).
- `../repo/REPO.md` — the `[aelum]` repo: publisher runbook, R2/pacman caveats.

## Layout, and the one hard rule

- `src/db.rs` — the formats: sync DBs, the local DB, `.PKGINFO`. **No GTK.**
- `src/model.rs` — the merged model (sync × local), `Status`/`Reason`
  classification, search, `updatable`, reverse-deps and orphans. **No GTK.**
- `src/i18n.rs` — locale selection + the embedded catalogs (the player's
  contract: `LC_ALL`→`LC_MESSAGES`→`LANG` → BCP-47 → supported list →
  en-US; per-key fallback; `{placeholder}` interpolation). **No GTK.**
- `src/vercmp.rs` — rpmvercmp + epoch, hand-written. **No GTK, no alpm.**
- `src/txn.rs` — the only place a subprocess runs, and the only place root is
  asked for.
- `src/ui/` — thin: `app.rs` is the shared state (`Ui`) and the transaction
  gate, `window.rs` the four-page `ViewStack`, one module per page,
  `dialog`/`txnlog`/`fileopen` the dialogs.

The three pure modules hold every decision and carry the tests; a page builds
rows from what `Model` returns and never classifies anything itself. That is
what makes the data layer testable without root, a display, or a package
installed — keep it that way.

## Measured facts about pacman's on-disk state

Each of these was paid for in a debugging session. Re-verify against the pacman
release you target before changing the code that encodes them.

- **A sync DB is compressed with whatever the mirror served.** Arch publishes
  `core.db.tar.gz` and pacman stores it verbatim, so `/var/lib/pacman/sync/core.db`
  is **gzip**; `repo-add` writes gzip too. `db::unpack_db` sniffs the magic bytes
  (gzip + zstd are read, xz/bzip2 name themselves). A zstd-only reader parsed
  *no* repository and the app said "Everything up to date" on a system with 6
  updates pending — the failure was invisible because the parse error was dropped.
- **`%REASON%` is written only for dependencies** (pacman 7.1 / alpm 16: the
  numeric enum, `1` = dep). An explicit package has **no `%REASON%` section at
  all**, so a missing section means explicit; an unparseable value fails closed
  to dependency. Verified 1118/1118 against `pacman -Qqe`.
- **`pacman -Sy` sends a conditional request**: when the mirror answers
  *not-modified* the `.db` files are not rewritten, so their mtimes cannot prove
  when a check last ran. The app records successful check times itself
  (`Ui::last_check`) and the freshness line shows the newer of that and the mtimes.
- **An alpm hook `Depends` value is stored verbatim** (pacman `hook.c` does no
  word splitting): one dependency per line, or the hook never runs.
- `filesystem` owns `/etc/polkit-1/rules.d` at **750**; `install -D` records a
  created directory at 755 and pacman warns on every transaction.
- **Nothing may read as empty by accident.** `Model::load` collects unreadable
  databases in `Model::db_errors`; the UI turns them into a toast and an honest
  empty state. An empty list must mean "there is nothing", never "I could not read it".
- `.PKGINFO` is the first member of the package archive — `read_pkginfo` streams,
  it does not mmap. Package files are routinely hundreds of MB.

## The one polkit action

`dev.aelum.package.manage` + `/usr/bin/aelum-package-installer-helper`
(`{refresh|install|remove|upgrade|file}` → `pacman -Sy|-S -Rns|-Syu|-U`, all
`--noconfirm`), the `aelum-snapshot-restore` shape: the policy's `exec.path`
annotation authenticates the *program*, and `data/polkit/aelum-package-manage.rules`
grants it to a local, active, `wheel` session. There is **no raw-pacman
fallback**: every privileged operation resolves to that one action, and a host
without the helper is told so instead of getting a pkexec error in a dialog.
`txn.rs::the_helper_is_annotated_by_the_policy` reads the policy file and pins
the annotation — an invariant, not a unit test of a string.

## UI rules the app holds to

- **Tab order Installed · Aelum · Browse · Updates.** Aelum lists the
  `[aelum]` repo's packages that are not installed yet — the distribution
  channel as a page; installing one moves it to Installed. Updates is last
  because it is the page you go to when something is waiting, and its
  `needs-attention` badge is the only update signal — the Installed page has
  no banner, a count there was the same fact twice.
- **Two dots, one rule.** The Updates badge marks pending updates, the Aelum
  badge marks apps the channel carries that the system lacks; both are
  `needs-attention` on the `ViewStackPage`, both are computed in
  `refresh_pages`, and visiting a page is the acknowledgement that clears
  its dot (window.rs). A third page may add a third dot the same way.
- **A successful refresh is quiet.** Startup and manual Refresh spin the header
  button, refresh the pages, and (manual only) toast; only a failure opens the
  log dialog. A modal log would cover the very thing that says a check is running.
- **The app lists its own package** (`model::SELF_PACKAGE`) like any other row —
  it needs its updates too, and a self-update while running is ordinary (Linux
  replaces the binary under the process). Only its **Uninstall is insensitive**,
  decided by `Entry::can_remove` and explained by a tooltip: removing it would
  leave a system with no package manager and nothing inside it to bring it back.
  The entry is never filtered out of `search`/`updatable`/`distro_apps`, so
  removing a dependency of the package manager still reports the breakage.
- `MAX_ROWS` (300) caps rendered rows — a full sync DB is ~15k entries and the
  pages rebuild on every keystroke.
- Every `ListBox` sits in a `ScrolledWindow`: an unscrolled list grows the
  window instead of the scrollbar.
- **A Remove click answers on the spot.** `Model` builds a dependents index
  (`name → installed packages that need it`) at load, and `reverse_deps` /
  `orphans_after_removal` answer from it. Scanning every installed package per
  question measured **~2 s per click** on a 1258-package system — the
  confirmation dialog appeared three seconds after the press; with the index the
  same 1258 questions take 15 ms. The ignored `removal_questions_answer_on_a_ui_thread_budget`
  test holds that budget.
- **The orphan list is pacman's `-Rns` set, not "everything orphaned".** It walks
  the target's dependency tree and takes a dependency only when nothing outside
  the removal needs it; a package that merely *depends on* the target is a
  breakage (`reverse_deps`), and an already-orphaned leftover elsewhere on the
  system is nobody's business here.
- **The confirmation asks one question and adds at most two facts.** It asks in
  words — "Are you sure you want to remove this package?" for removal, "Do you
  want to install/update &lt;name&gt; &lt;version&gt;?" for installs — because a
  bare fact beside a button does not read as a confirmation. Then only: what
  still needs the package (three names maximum, then a count) and how many
  unneeded dependencies go along. Sizes, "this runs as root", and the full
  orphan list are not there — the transaction log names every package it touches.
- Model loads are generation-numbered (`Ui::load_seq`): the startup load and the
  post-refresh load overlap, and only the newest may install its result.

## Stack decisions

| Crate | Pinned | Why |
|---|---|---|
| `gtk` (`package = "gtk4"`) | 0.11.5, `gnome_50` | the player's pin, same system libraries |
| `libadwaita` | 0.9.2, `v1_10` + `gtk_v4_24` | matches the system libraries |
| `tar`, `zstd` | 0.4 / 0.13 | sync DB + `.PKGINFO` reading, no libalpm |
| `flate2` | 1.1, default (pure-Rust) backend | gzip DBs; no new system dependency in the build image |
| `futures` | 0.3 | worker → main-thread channels (glib 0.22 removed `MainContext::channel`) |

No libalpm, no `alpm` crate, no `pacman` subprocess for reads: the on-disk
formats are stable and parsing them is what makes search instant. Version
comparison is `vercmp.rs` — pacman 7 ships no `vercmp` helper and linking a
library for two strings is overkill.

GTK/libadwaita traps already hit: `gtk::Image::from_icon_name` takes `&str`, not
`Option`; `ViewStack::visible_child_name()` returns `Option<GString>`; glib
dropped `escape_text`, so `ui::markup_escape` handles it — `ActionRow`
title/subtitle are Pango markup and package descriptions carry bare `&`
(firefox's does).

## Commands

```bash
cargo test                                 # the data layer + txn invariants, no root needed
cargo test -- --ignored                    # real Arch host: compares with pacman -Qqe and -Quq
cargo clippy --all-targets                 # must stay at 0 warnings
cargo run                                  # GUI. Reads work from a source tree; transactions
                                           # need the packaged helper (see below)
./scripts/build-pkg.sh [--no-sign]         # Arch package + sha256 + GPG .asc → dist/
./scripts/bump-version.sh X.Y.Z [--changes "…"]   # bump all version files, test+clippy gate,
                                                 # commit, push, build, publish, prune the old pkg
../repo/publish-repo.sh dist/<pkg>.pkg.tar.zst    # publish one artifact by hand
```

- `build-pkg.sh` tars **`git ls-files`**: uncommitted code literally cannot ship,
  and it fails fast when `PKGBUILD` pkgver drifts from `Cargo.toml` (Cargo.toml
  is the source of truth).
- `bump-version.sh` needs a clean tracked tree and **does not tag**. Tag by hand
  after it runs, matching video-player:
  `git tag -a vX.Y.Z -m "aelum-package-installer X.Y.Z" && git push origin vX.Y.Z`.
- AppStream edits are worth validating: `appstreamcli validate data/*.metainfo.xml`
  (a `<ul>` must sit inside `<description>`; `<summary>` is not valid in `<release>`).
- Run from source and the packaged helper is absent, so `auto_check` skips itself
  and every transaction refuses with a toast — that is the designed behaviour, not
  a bug to fix. Dogfooding needs `sudo pacman -U dist/…`.
- The published DB is edge-cached for ~4 h (see `../repo/REPO.md`), so a fresh
  publish is not instantly visible to `pacman -Sy`.

## Conventions

- Comments state *why*, especially about pacman and GTK; keep them to one clause
  and preserve them when moving code. Never narrate the request that prompted a
  change — the history of asks belongs to git.
- Commit messages: one conventional subject (≤72 chars), at most two or three
  body lines for the *why*. Measured data goes in the code comment or the plan.
- A claim about classification is settled against pacman, not by reading its
  source: `-Qqe` / `-Qqd` / `-Quq` are the oracles and the ignored tests hold them.
- **i18n is wired (M4, 2026-11-05):** every UI string is an `i18n::t()`/
  `i18n::t_fmt()` call keyed in `data/translations/en-US.json`; all 28
  supported locales carry a full catalog, embedded at compile time (no data
  dir, no network). The completeness tests in `i18n.rs` fail if a catalog
  misses a key or loses a `{placeholder}`, if the desktop entry's `Name[]`
  disagrees with `app.name`, or if `GenericName[]`/`Comment[]` coverage
  drops — so adding a string means: key in en-US + the call site + all 27
  translations, and the tests say so. The window direction follows `is_rtl`
  (`apply_rtl` walks the tree — GTK direction is not inherited).
- GPL-3.0-only, declared in `Cargo.toml`, the PKGBUILD and `LICENSE` (the
  polkit rule carries its own SPDX id; the license ships in the package under
  `/usr/share/licenses/aelum-package-installer/`).
