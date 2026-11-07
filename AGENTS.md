# AGENTS.md — working notes for coding agents

A package manager for the Aelum desktop: GTK4 + libadwaita, one Rust crate.
Binary `package-installer`, app ID `aelum.packageinstaller.Packages`, Arch
package `aelum-package-installer`. It reads pacman's on-disk state directly and
shells out only for transactions.

**Distribution channel: the `[aelum]` repository** — published with
`../repo/publish-repo.sh`, installed on installed systems with `pacman -S`. The channel
is proven, so the package is also listed in the installer's
`installdesktop.DesktopPackages`: the ISO build resolves `[aelum]` alongside the mirrors (that list
is what the offline mirror is built from, together with the live profile's), bakes it in, and fresh
installs ship it preinstalled. Updates still come from the repo.

## Read these first

- `../AGENTS.md` → the `package-installer/` section, for how this repo fits the
  distro (polkit pattern, the repo channel, the QEMU test guest).
- `../repo/REPO.md` — the `[aelum]` repo: publisher runbook, R2/pacman caveats.

## Layout, and the one hard rule

- `src/db.rs` — the formats: sync DBs, the local DB, `.PKGINFO`. **No GTK.**
- `src/aur.rs` — the AUR's protocol: the RPC URLs, its reply format, and
  `plan()` (what an install needs, in build order). **No GTK, no network, no
  process** — the caller brings the answers and the model's answers.
- `src/remote.rs` — the commands that run **without** root: curl (the RPC and
  the PKGBUILD snapshot) and makepkg. Holds the sequence one AUR install is.
- `src/model.rs` — the merged model (sync × local), `Status`/`Reason`
  classification, search, `updatable`, reverse-deps and orphans, what an AUR
  answer means for this system (`aur_rows`, `aur_updates`, `foreign_names`),
  and whether a dependency is already answered (`dep_satisfied`/`in_repo`).
  **No GTK.**
- `src/i18n.rs` — locale selection + the embedded catalogs (the player's
  contract: `LC_ALL`→`LC_MESSAGES`→`LANG` → BCP-47 → supported list →
  en-US; per-key fallback; `{placeholder}` interpolation). **No GTK.**
- `src/vercmp.rs` — rpmvercmp + epoch, hand-written. **No GTK, no alpm.**
- `src/progress.rs` — transaction progress: the phase bands per verb, and reading
  pacman's output into honest counts (`Track`). **No GTK.**
- `src/txn.rs` — the only place root is asked for.
- `src/ui/` — thin: `app.rs` is the shared state (`Ui`), the transaction gate
  and the AUR search/job entry points, `window.rs` the five-page `ViewStack`,
  one module per page, `dialog`/`txnlog`/`fileopen` the dialogs.

The pure modules hold every decision and carry the tests; a page builds
rows from what `Model` returns and never classifies anything itself. That is
what makes the data layer testable without root, a display, a package
installed, or the network — keep it that way.

## Measured facts about pacman's on-disk state

Each of these was paid for in a debugging session. Re-verify against the pacman
release you target before changing the code that encodes them.

- **A sync DB is compressed with whatever the mirror served.** Arch publishes
  `core.db.tar.gz` and pacman stores it verbatim, so `/var/lib/pacman/sync/core.db`
  is **gzip**; `repo-add` writes gzip too. `db::unpack_db` sniffs the magic bytes
  (gzip + zstd are read, xz/bzip2 name themselves). A zstd-only reader parsed
  *no* repository and the app said "Everything up to date" on a system with 6
  updates pending — the failure was invisible because the parse error was dropped.
- **A `.db` in the sync directory is not a repository, and a name in two
  repositories is one row.** pacman resolves through pacman.conf: the **first**
  configured repository carrying a name is the candidate for `-S` and `-Qu`, and
  a database whose repository the config does not name is never read. Measured
  with pacman 7 against two throwaway repositories holding the same package:
  `-Sl` with `zz-first.db` on disk and only `zz-second` configured lists only
  `zz-second`; `-Sp` and `-Qu` flip which version they use when the two
  sections are swapped in the file. `Model::load` therefore takes the config
  path as well as the pacman root and merges one row per name in that order
  (`db::read_config_repos`; `Include` is followed, and a config that cannot be
  enumerated — glob include, include cycle — means *do not filter*). The
  failure this holds: `pacstrap -K` resolves names through the live session's
  `[aelum-offline]` and pacman writes that database into the target's sync dir,
  where the installed system's config no longer names it — Browse then showed
  every package twice, once "Install" and once "Installed".
- **`%REASON%` is written only for dependencies** (pacman 7.1 / alpm 16: the
  numeric enum, `1` = dep). An explicit package has **no `%REASON%` section at
  all**, so a missing section means explicit; an unparseable value fails closed
  to dependency. Verified 1118/1118 against `pacman -Qqe`.
- **`pacman -Sy` sends a conditional request**: when the mirror answers
  *not-modified* the `.db` files are not rewritten, so their mtimes cannot prove
  when a check last ran. The app records successful check times itself
  (`Ui::last_check`) and the header's database-date label (`header.fresh`,
  worded about *databases* — a mirror is where the files came from, a database
  is the thing that was refreshed) shows the newer of that and the mtimes (two
  centered lines — words, then date and time; hidden while no databases exist).
- **`db.lck` is an empty file, and alpm never clears one.** `_alpm_handle_lock`
  is `open(<dbpath>/db.lck, O_WRONLY|O_CREAT|O_EXCL, 0000)` — measured on
  pacman 7.1 during a download: mode `0000`, **zero bytes, no PID written** —
  and no dead-lock check survives anywhere in alpm 16 (the old
  `check_lockfile`/`kill(pid, 0)` path is gone). A lock left by a killed or
  crashed pacman therefore fails every later call forever: `error: failed to
  synchronize all databases (unable to lock database)`, exit 1. So the helper
  removes it before **every** verb — after checking `/proc/*/comm` for a live
  `pacman`, because the app's own gate means a lock nothing is holding is
  nobody's, while a live one is a transaction that must not run twice.
  `txn.rs::every_verb_clears_a_stale_db_lock_but_never_a_live_one` pins the
  path against `model::PACMAN_ROOT` and all seven verbs.
- **An alpm hook `Depends` value is stored verbatim** (pacman `hook.c` does no
  word splitting): one dependency per line, or the hook never runs.
- `filesystem` owns `/etc/polkit-1/rules.d` at **750**; `install -D` records a
  created directory at 755 and pacman warns on every transaction.
- **Nothing may read as empty by accident.** `Model::load` collects unreadable
  databases in `Model::db_errors`; the UI turns them into a toast and an honest
  empty state. An empty list must mean "there is nothing", never "I could not read it".
- `.PKGINFO` is the first member of the package archive — `read_pkginfo` streams,
  it does not mmap. Package files are routinely hundreds of MB.
- **`%PROVIDES%` is in both the sync and the local desc files** (measured in
  extra.db: 7zip → `p7zip`; `pacman -Qi` shows the same for installed
  packages). A dependency matched by name alone asks for a package that does
  not exist, and makepkg — which asks alpm — would have built it anyway.
- **`base` and `base-devel` are metapackages, not pacman groups**:
  `pacman -Qg base-devel` finds nothing, while core.db's `base-devel-1-2/desc`
  carries the tool list in `%DEPENDS%`. That entry is what `Model::
  missing_dependencies("base-devel")` reads, and `pacman -Si base-devel`'s
  Depends line is the oracle (`--ignored`).

## Measured facts about pacman's progress output

Captured through the packaged helper on the QEMU test system (pacman 7.1) and held under
`tests/fixtures/`; `progress.rs`'s replay tests feed every file through `Track` and assert the
fraction never goes backwards and ends where the work says it ends. A pacman that rewrites these
lines degrades the bar to phase-creep — which is why the bar must never invent a number.

- **Through a pipe there are no `( i/n)` counters and no progress bars at all** — pacman draws
  those on a TTY only. What *is* there: the `Packages (N) …` header, one bare
  `installing/upgrading/removing X...` line per package, download lines, and `( i/n)` counters on
  the *hook* lines. The counted numerators are those lines against `Packages (N)`; a real
  counter, when one appears, overrides the count.
- **One ` name downloading...` line per download, printed at its start** — through a pipe there
  are no `\r` redraws and no speed lines even on a 1.3 GB download (measured: CR=0). libalpm
  fetches **five at a time**, and no line ever says a single download finished, so Retrieve
  counts **distinct line subjects** (the first token) less the batch in flight: a start proves a
  finished download only once it frees a slot. The count lags the truth by at most the batch —
  never ahead, which is the direction that matters; the in-flight batch earns a bounded credit
  in the fraction, never a whole package.
- **The ` name downloading...` line marks a download's start, and pacman starts up to five at a
  time** (measured through a pipe with timestamped lines): the count lags the truth by the batch —
  never ahead, which is the direction that matters.
- **With stdout a pipe, pacman's own output is block-buffered and arrives at the end** — measured:
  53 s of silence for a ~200 MiB download, the log frozen right after `:: Retrieving packages...`
  (the confirmation prompt flushes the buffer, which is why everything *before* it shows). The
  helper runs pacman under `stdbuf -oL`; with line buffering the same lines arrive as they are
  printed. The `the_helper_is_annotated_by_the_policy` test pins the stdbuf.
- **`--noprogressbar` strips the counters too** (measured): the helper does not use it; the
  parser strips bar and speed columns instead.
- **A pre-transaction hook's `(1/1)` arrives before `:: Processing package changes...`** and
  belongs to neither the package count nor the Hooks band — counting it there moves the bar past
  work that has not happened.
- Per-verb openings: `-Syu` adds `:: Synchronizing package databases...` and `:: Starting full
  system upgrade...` (and ` there is nothing to do` when there is nothing); `-Rns` opens with
  `checking dependencies...` and `:: Do you want to remove these packages? [Y/n]`; `-U` opens
  with `loading packages...`. `--noconfirm` answers the prompts, so they are not phases.

## Measured facts about the AUR

The AUR is someone else's server, and its format is not alpm's. Each of these
was measured against the live RPC (and `cargo test -- --ignored` re-runs that
measurement).

- **A refused request is HTTP 200 with an empty `results` array** and a
  `{"type":"error","error":"Too many package results."}` body — that is what
  `arg=python` and `arg=git` answer. Reading only `results` would show "no
  packages found" for a search the server never ran.
- **`type=info` answers only for the names it has** (measured: one existing
  and one missing name → `resultcount: 1`). A name with no answer has *no
  information*, which is not the same as *no dependencies*.
- **`type=info` takes many names in one request and answers with everything a
  search answer carries.** Measured against the live RPC: 200 `arg[]` values
  (a 4.6 KB URL) answer all 200 packages, each with `Version`, `Description`,
  `URL`, `URLPath`, `NumVotes` and `OutOfDate` — the same fields a search
  row has, which is why `aur::Info::as_hit` lets the update check reuse the
  model's one classification. A batch bound (`aur::INFO_BATCH`, 100) is a
  guard for a pathological system, not a server limit.
- **A name the AUR does not have is simply absent from the reply.** Measured
  on a real host: 30 foreign names (`pacman -Qqm`) yielded 8 answers — the
  rest were locally built or from another source. So the honest number an
  update check reports is how many names it **asked about**, never how many
  it found, and a name with no answer is never shown as an update.
- **A search answer has no dependency lists**; only `info` carries them, and
  the field is spelled `MakeDepends`, not the `.SRCINFO` spelling. So an
  install asks in rounds: a dependency's own dependencies are visible only
  once that dependency has been asked about (`MAX_INFO_ROUNDS` bounds it).
- **Every answer carries `URLPath`** — `/cgit/aur.git/snapshot/<base>.tar.gz`,
  a tarball of `<pkgbase>/PKGBUILD` and its files. That is how a PKGBUILD is
  fetched without git: curl plus the `tar` crate already in use.
- **The RPC's `resultcount` equals the number of results** for a query it
  accepts (`arch` → 3532 in one reply), so there is no paging to implement.
- **`makepkg` refuses to run as root, and `makepkg -s` asks sudo** for a
  terminal this process does not have. So the repository dependencies are
  installed first through the one polkit action, and the build runs as the
  user with plain `makepkg --noconfirm`.
- **curl is a pacman dependency** (`pacman -Qi pacman`: `Depends On: … curl …`),
  so it is on every system this app runs on; no HTTP client crate is needed.
- **An Aelum install has the build tools**: `base-devel` is in the installer's
  own base set, so the
  refusal below is for hosts that are not an Aelum install — it is still the
  right answer there, and it is checked in `verify-installed.sh`.
- **The built archives live *inside* the build directory.** Clearing that tree
  once the package existed — which is what this did first — deleted the files
  being installed, and the real install of qtengine ended in
  `error: '…/aur/qtengine/qtengine-0.2.4-1-x86_64.pkg.tar.zst': could not find
  or read package` for both halves of a split build that had just been built.
  `keep_archives` removes the sources, `$srcdir` and `$pkgdir` and keeps the
  `.pkg.tar.*`.
- **Which files a build produced is asked of makepkg (`--packagelist`), not
  read off the directory.** It names every half of a split build and honours
  `PKGDEST`/`PKGEXT` from the user's makepkg.conf (measured: `PKGDEST=…` puts
  the archives outside the build directory entirely, so a directory scan finds
  nothing). With no PKGBUILD it exits 12 with no output — which is why the
  scan stays as the fallback, and why a path that is not a file is dropped
  before pacman is asked.
- **Which of the names an AUR install brought were build-only is answered by
  the local DB after the install, not by the plan.** makepkg writes a built
  package's runtime dependencies into its `%DEPENDS%` and its build-only ones
  nowhere, so a name no installed package needs — by name or by provision,
  the rule `-Rns` itself respects — was needed by the build alone, and
  `remove` takes it back. A name the system already had was never brought by
  the request and is never touched; the requested package is never a
  candidate whatever the answer says. A split build's dependency halves carry
  their own pkgnames — `.PKGINFO` answers for them, the plan's name is only
  the fallback for an archive that cannot be read. The answer is about the
  **set**: a candidate needed only by *another candidate* is still a leftover
  (measured with qtengine: its makedepend `extra-cmake-modules` names
  `cmake` in its `%DEPENDS%` — asked one name at a time, `cmake` survived
  through its own grave-mate). Candidates are weighed against the packages
  outside them, to a fixed point — what `-Rns` resolves a set to.
- **makepkg marks everything it installs as a dependency** (`/usr/bin/makepkg`:
  `run_pacman -S --asdeps`), and its `--rmdeps` takes back only what its own
  run installed (a `pacman -Qq` diff before/after — which is why step 5 keeps
  its own `brought` list instead of delegating). The AUR flow copies that
  marking: the repository half arrives through `install-asdeps` and built
  dependencies' archives through `file-asdeps`; only the requested package is
  Explicit. The failure this holds: a name installed *by name* is Explicit,
  survives every `-Rns`, and the next plan reads it as satisfied — an earlier
  install's cmake was never a cleanup candidate at all.

## The one polkit action

`dev.aelum.package.manage` + `/usr/bin/aelum-package-installer-helper`
(`{refresh|install|install-asdeps|remove|upgrade|file|file-asdeps}` →
`stdbuf -oL pacman -Sy|-S|-S --asdeps|-Rns|-Syu|-U|-U --asdeps`, all `--noconfirm` —
the stdbuf is what makes the output arrive during the download, not after it),
the `aelum-snapshot-restore` shape. Every verb first drops a leftover
`db.lck` when no pacman is running (see the measured fact above): one left by
a killed transaction — closing the app mid-sync does exactly that — would
otherwise leave every later transaction failing with "unable to lock
database", and the log says which of the two happened.
An AUR install is one request of several
steps — `install-asdeps` (repository dependencies, makepkg's marking), the
build as the user, `file-asdeps` (the built dependencies' archives), `file`
(the requested package, Explicit), `remove` (the build-only dependencies the
finished system does not need) — and `txn::run_wait` runs
the root steps inside the worker so all of it streams into the one log
dialog the request opened. A failed cleanup is a line in the log, not a
failed install: the package is on the system by then. The AUR adds verbs but
no action: the policy's `exec.path`
annotation authenticates the *program*, and `data/polkit/aelum-package-manage.rules`
grants it to a local, active, `wheel` session. There is **no raw-pacman
fallback**: every privileged operation resolves to that one action, and a host
without the helper is told so instead of getting a pkexec error in a dialog.
`txn.rs::the_helper_is_annotated_by_the_policy` reads the policy file and pins
the annotation — an invariant, not a unit test of a string.

## UI rules the app holds to

- **Tab order Installed · Browse · Aelum · AUR · Updates.** Aelum lists the
  `[aelum]` repo's packages that are not installed yet — the distribution
  channel as a page; installing one moves it to Installed. Updates is last
  because it is the page you go to when something is waiting, and its
  `needs-attention` badge is the only update signal — the Installed page has
  no banner, a count there was the same fact twice.
- **The AUR page fetches nothing until asked**: no badge, no startup query.
  This desktop is offline-first, and "N AUR updates waiting" would be a claim
  about a third-party server the user never contacted; the app's only
  automatic network call is `pacman -Sy`, against the user's own mirrors. The
  idle state carries the search note and, below it, the trust warning
  (`aur.warn`: community-published, unreviewed, built on the user's
  machine); the install confirmation repeats it in one line
  (`aur.buildNote`). The
  search entry debounces (400 ms — the AUR rate-limits), answers are
  generation-numbered (`Ui::aur_seq`) so a slow older search cannot answer a
  newer question, and a search that never happened is never rendered as an
  empty list. The answer is kept as the AUR gave it and rows are classified
  when the page draws them (`Model::aur_rows`: not installed / installed /
  older than what the AUR would build), so they say what those names are on
  this system *now* and a search that beat the model load is not misread as
  "nothing matches". An AUR "update" is a rebuild — the row says
  `installed → to build`, because there is no binary to download.
- **The AUR update check is a press, never a badge.** Its button on the AUR
  page takes `Model::foreign_names` (pacman's `-Qqm`: installed names no
  configured repository carries), asks `type=info` for them through
  `remote::aur_updates`, and shows only `Model::aur_updates` — the rows whose
  AUR version is newer. A name a repository carries is already covered by
  Refresh, and an out-of-date flag *without* a newer version is not an update
  (rebuilding would produce the installed version); that row still carries the
  tag. Pressing it clears the search entry — the entry's text belongs to the
  other question — and its empty states name which case they are: nothing
  installed outside the repositories, or asked-about-N-and-nothing-newer with
  the count stated. The button is insensitive while a request is in flight.
- **Two dots, one rule.** The Updates badge marks pending updates, the Aelum
  badge marks apps the channel carries that the system lacks; both are
  `needs-attention` on the `ViewStackPage`, both are computed in
  `refresh_pages`, and visiting a page is the acknowledgement that clears
  its dot (window.rs). A third page may add a third dot the same way.
- **A successful refresh is quiet.** Startup and manual Refresh spin the header
  button, cover the tab content with a "refresh in progress" page, refresh the
  pages, and (manual only) toast; only a failure opens the log dialog. A modal
  log would cover the very thing that says a check is running — and the rows
  behind it are the databases as they were *before* the check, so `Ui::set_refresh_running`
  hides them rather than letting stale rows read as a result.
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
- **Ask instead of taking #1.** A request whose dependency is a virtual with
  several candidates gets one chooser, before the transaction, listing
  `name — repository — size` in pacman's candidate order with pacman's default
  preselected; every question is asked in that one dialog. The chosen providers
  are installed **as dependencies** first (the marking pacman's own provider
  prompt uses — by name they would be Explicit, survive the request's `-Rns`,
  and silently answer the virtual forever after), then the requested names
  Explicit — one job, one log (`txn::run_all`). A single candidate or an
  already-installed provider is never asked about — pacman would not ask either.
- **The bar never claims a number it does not have.** `ui/txnlog.rs` paints
  what `progress::Track` says and decides nothing: a phase label, the band
  fraction, and an "i of n" only when `Track::counts` returns one; un-counted
  phases creep inside their band. The output lives behind a disclosure that
  opens itself on failure. Appends and repaints coalesce into one ~80 ms tick
  (the stored `SourceId` is dropped inside the callback — the GTK trap below),
  and the text view keeps the last 2000 lines, dropping from the front.
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
| `serde_json` | 1 | the i18n catalogs **and** the AUR RPC replies |

No libalpm, no `alpm` crate, no `pacman` subprocess for reads: the on-disk
formats are stable and parsing them is what makes search instant. Version
comparison is `vercmp.rs` — pacman 7 ships no `vercmp` helper and linking a
library for two strings is overkill. No HTTP client crate either: the AUR is
asked with curl, which every pacman system already has.

GTK/libadwaita traps already hit: `gtk::Image::from_icon_name` takes `&str`, not
`Option`; `ViewStack::visible_child_name()` returns `Option<GString>`; glib
dropped `escape_text`, so `ui::markup_escape` handles it — `ActionRow`
title/subtitle are Pango markup and package descriptions carry bare `&`
(firefox's does); `SourceId::remove()` **panics** on an id whose source already
ran (a one-shot `timeout_add_local` is gone when its callback returns `Break`),
and a panic inside a signal trampoline aborts the process — the AUR debounce
dropped the stored id inside its callback for that reason.

## Commands

```bash
cargo test                                 # the data layer + txn invariants, no root needed
cargo test -- --ignored                    # real Arch host + live AUR: pacman -Qqe/-Quq, the
                                           # base-devel entry, the RPC and a real snapshot
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
- **Before cutting a release, read what it actually contains**: `git log
  --no-merges v<previous>..HEAD`, and give every user-visible change its own
  `--changes` bullet. Describing a release from the last commit's message
  describes one commit — 0.5.3 shipped an AUR update check, 28 corrected
  catalogs and a glyph change, and its changelog named only the glyph. The
  `--changes` text becomes the metainfo entry *and* the Release notes, so one
  thin bullet understates the release in both places at once.
- `bump-version.sh` needs a clean tracked tree and **does not tag**. Tag by hand
  after it runs, matching video-player:
  `git tag -a vX.Y.Z -m "aelum-package-installer X.Y.Z" && git push origin vX.Y.Z`.
- Then publish the tag as a **GitHub Release with notes**:
  `gh release create vX.Y.Z --title "aelum-package-installer X.Y.Z" --notes "<the --changes bullets>"`.
  A tag's annotation is invisible on GitHub — the tag list never shows it, and
  descriptions live on Releases. The notes are the same bullets passed to
  `--changes` (they are already the metainfo changelog: one text, reused). The
  Release is the human-facing changelog; the installable artifact still ships
  through the `[aelum]` repo, never GitHub.
- AppStream edits are worth validating: `appstreamcli validate data/*.metainfo.xml`
  (a `<ul>` must sit inside `<description>`; `<summary>` is not valid in `<release>`).
- A wrong changelog in an already **published** package is fixed by editing that
  release's metainfo entry and republishing with `pkgrel` + 1 — the served DB has
  to name a different artifact or nobody receives it: `build-pkg.sh`,
  `publish-repo.sh`, then delete the superseded `-1` from R2. `bump-version.sh`
  reads `pkgrel` and never resets it, so set it back to `1` with the next version
  bump. The tag should name the tree the shipped artifact was built from, so move
  it too: `git tag -f -a vX.Y.Z … && git push -f origin vX.Y.Z`.
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
  body lines for the *why*. Measured data goes in the code comment or the doc
  it belongs to.
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
