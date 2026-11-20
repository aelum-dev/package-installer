//! aelum-package-installer-helper — the root half of every package-manager
//! operation. pkexec authenticates this program (the polkit action names it
//! through its exec.path annotation); it is the only place pacman and
//! pacman-key run with privileges, so one action and one rule cover every
//! verb. The verbs that
//! run no pacman are `firmware-update` (fwupdmgr — the fwupd daemon treats
//! uid 0 as always trusted, so the update needs no auth agent the desktop may
//! not run, and the app's wheel rule is what stands between a person and the
//! policy), `update-mirrors` (reflector) and `fix-keyring` (pacman-key).
//!
//! This is the crate's second binary, not a script, and that is the point:
//! pkexec hands the program it runs as root the *caller's* environment, and
//! a shell script inside that environment is run by an interpreter that
//! obeys `BASH_ENV` and imported shell functions, resolves every bare
//! command — its own `#!/usr/bin/env` line first of all — through the
//! caller's `PATH`, and passes `LD_*` on to what it execs. One
//! attacker-chosen variable there is code running as root through a
//! legitimately authorized action. So this program names no interpreter,
//! builds pacman's argv itself, execve's both programs by absolute path,
//! and gives them an environment that is empty apart from a fixed `PATH`
//! and the locale variables. It is the rule rEFInd's compiled pkexec helper
//! follows: never exec a bare name as root.
//!
//! `--noconfirm` everywhere: the app's dialog owns the confirmation, and a
//! conflict must exit non-zero with pacman's real output rather than wait
//! for a TTY that does not exist.
//! No `--noprogressbar`: measured against pacman 7.1, pacman draws bars and
//! transaction counters only on a TTY — through this app's pipe it prints
//! plain lines either way — and the flag would strip the ( i/n) counters a
//! TTY run does carry. progress.rs parses both shapes.
//! `stdbuf -oL`: with stdout a pipe, glibc buffers pacman's output in 4 KiB
//! blocks, and a download's lines stay in that buffer until it ends — the
//! app watched "Retrieving packages..." for a minute with 700 MiB moving.
//! Line-buffered, every line reaches the app as pacman prints it.

use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Command;

/// The database lock, under the same pacman root the model reads its
/// databases from — one path, never a second copy of it.
const DB_LOCK: &str = "/var/lib/pacman/db.lck";

/// The mirror list, the same file the app reads its country default from —
/// one path, never a second copy of it (pinned against `model::MIRRORLIST`
/// by a test in `txn.rs`).
const MIRRORLIST: &str = "/etc/pacman.d/mirrorlist";

/// Where a mirror update writes before the real file is touched: same
/// directory (so the rename below is atomic) and named so a listing says what
/// it is if a run is ever killed between the two steps.
const MIRRORLIST_TMP: &str = "/etc/pacman.d/mirrorlist.aelum-tmp";

/// pacman's own keyring manager: the keyring repair runs its two documented
/// stages rather than reimplementing what pacman verifies signatures against.
const PACMAN_KEY: &str = "/usr/bin/pacman-key";

/// The fixed PATH these programs are handed. Absolute-path exec leaves no
/// lookup to chance, and this is the PATH a script's `#! /usr/bin/env` line
/// resolves through — the caller's never is.
const ENV_PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";

/// The only caller variables these programs are handed: they choose the
/// message language, nothing else. Everything else is dropped by `env_clear`
/// — `LD_*`, `IFS`, shell variables, `TMPDIR` (a package's scriptlets mktemp
/// through it) — and `PATH` is the fixed `ENV_PATH` alone.
const ENV_KEEP: [&str; 5] = ["LANG", "LC_ALL", "LC_CTYPE", "LC_MESSAGES", "LANGUAGE"];

fn main() {
    let mut argv = std::env::args_os().skip(1);
    let Some(verb) = argv.next() else { usage() };
    let rest: Vec<OsString> = argv.collect();
    // The verbs that run no pacman: the mirror update, the firmware write and
    // the keyring repair. They are dispatched before the pacman_args match so
    // none clears a database lock it has no business touching — reflector
    // fetches a JSON file and writes the mirror list, fwupd talks to its own
    // daemon, pacman-key writes under the GPG directory pacman names in its
    // config. None opens a pacman database.
    if verb == "update-mirrors" {
        update_mirrors(&rest);
    }
    if verb == "firmware-update" {
        firmware_update(&rest);
    }
    if verb == "fix-keyring" {
        fix_keyring(&rest);
    }
    // The verbs are named here before anything is touched, so an unknown one
    // is refused instead of clearing a lock on a call that runs no pacman.
    let Some(flags) = pacman_args(&verb.to_string_lossy(), &rest) else {
        usage()
    };
    clear_stale_lock();
    exec_pacman(&flags);
}

/// How the arguments a verb takes reach pacman.
#[derive(PartialEq, Eq, Clone, Copy)]
enum Take {
    /// names follow with no separator, as the dispatched script had them.
    Loose,
    /// names arrive behind a `--` so none can read as a pacman option.
    Dashed,
    /// the verb takes no names at all.
    None,
}

/// Which arguments a verb of the helper runs: pacman's flags, then whatever
/// arguments the verb takes. A pure function so every mapping is testable
/// without root.
fn pacman_args(verb: &str, rest: &[OsString]) -> Option<Vec<OsString>> {
    let (flags, take): (&[&str], Take) = match verb {
        "refresh" => (&["-Sy"], Take::Loose),
        "install" => (&["-S", "--needed"], Take::Dashed),
        "install-asdeps" => (&["-S", "--needed", "--asdeps"], Take::Dashed),
        "remove" => (&["-Rns"], Take::Dashed),
        "upgrade" => (&["-Syu"], Take::None),
        "file" => (&["-U"], Take::Dashed),
        "file-asdeps" => (&["-U", "--asdeps"], Take::Dashed),
        _ => return None,
    };
    let mut argv: Vec<OsString> = flags.iter().map(|flag| (*flag).into()).collect();
    argv.push("--noconfirm".into());
    if take == Take::Dashed {
        argv.push("--".into());
    }
    if take != Take::None {
        argv.extend_from_slice(rest);
    }
    Some(argv)
}

/// The flags the mirror update runs reflector with: the newest twenty https
/// mirrors of one country — no country means the installer's worldwide
/// location, every mirror — listed by the mirror-status' own rate, saved to
/// the temp file `update_mirrors` then moves into place. The country is
/// accepted only as a bare 2-letter ISO code, the shape the app sends:
/// nothing else on argv can read as a reflector option. (Measured against
/// reflector 2023: `--country` matches the status database by name or code,
/// case-insensitively — the code is what survives every country's spelling.)
fn reflector_args(rest: &[OsString]) -> Option<Vec<OsString>> {
    let country = match rest {
        [] => None,
        [one] if is_country_code(one) => Some(one.clone()),
        _ => return None,
    };
    let mut argv: Vec<OsString> = ["--latest", "20", "--protocol", "https"]
        .iter()
        .map(|flag| (*flag).into())
        .collect();
    if let Some(country) = country {
        argv.push("--country".into());
        argv.push(country);
    }
    argv.extend(
        ["--sort", "rate", "--save"]
            .iter()
            .map(|flag| (*flag).into()),
    );
    argv.push(MIRRORLIST_TMP.into());
    Some(argv)
}

fn is_country_code(value: &OsStr) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 2 && bytes.iter().all(|b| b.is_ascii_alphabetic())
}

/// The mirror update — a verb that cannot exec, because the file it asked for
/// has to be moved into place first: reflector opens its
/// `--save` file truncating, so a run killed mid-write would otherwise leave
/// the system with a mirror list it cannot sync through: it writes beside the
/// real file and this program moves it in with `rename(2)` — atomic within
/// the directory — and only after reflector exited 0. (reflector itself
/// refuses to save an empty selection, measured: `error: no mirrors found`
/// before it ever opens the file; the rename is the guard against the rarer
/// kill.) reflector's output is written at its exit, and the app watches a
/// spinner, not lines — so no stdbuf here: that works on C stdio, not on
/// Python's own buffering.
fn update_mirrors(rest: &[OsString]) -> ! {
    let Some(flags) = reflector_args(rest) else {
        usage()
    };
    let mut cmd = Command::new("/usr/bin/reflector");
    cmd.args(&flags)
        .env_clear()
        .env("PATH", ENV_PATH)
        .envs(locale_env());
    let status = match cmd.spawn().and_then(|mut child| child.wait()) {
        Ok(status) => status,
        Err(err) => {
            eprintln!("aelum-package-installer-helper: cannot run reflector: {err}");
            std::process::exit(127);
        }
    };
    // A signal's exit (no code) is not a success either.
    if status.code() != Some(0) {
        std::process::exit(status.code().unwrap_or(1));
    }
    if let Err(err) = std::fs::rename(MIRRORLIST_TMP, MIRRORLIST) {
        eprintln!(
            "aelum-package-installer-helper: cannot move the new mirror list into place: {err}"
        );
        std::process::exit(1);
    }
    std::process::exit(0);
}

/// The flags the firmware write runs fwupdmgr with: the one fixed command,
/// and no argument — the fwupd daemon answers for which devices take an
/// update (the app asked it the same question when the page filled), and an
/// id on argv could only mean a write nobody confirmed. `--assume-yes`
/// answers fwupdmgr's own prompts; the final "Restart now?" is fwupdmgr's
/// doing on a TTY, and read from the closed pipe here it takes its default
/// (no): a staged update waits for a person to reboot.
fn fwupd_args(rest: &[OsString]) -> Option<Vec<OsString>> {
    match rest {
        [] => Some(vec!["update".into(), "--assume-yes".into()]),
        _ => None,
    }
}

/// The firmware write — the second exec verb: fwupd's own progress lines
/// stream into the caller's log, and pkexec reports fwupdmgr's exit status.
/// stdbuf for the same measured reason as pacman: through a pipe its stdio
/// output arrives at the end without line buffering.
fn firmware_update(rest: &[OsString]) -> ! {
    let Some(flags) = fwupd_args(rest) else {
        usage()
    };
    let mut cmd = Command::new("/usr/bin/stdbuf");
    cmd.arg("-oL")
        .arg("/usr/bin/fwupdmgr")
        .args(&flags)
        .env_clear()
        .env("PATH", ENV_PATH)
        .envs(locale_env());
    let err = cmd.exec();
    eprintln!("aelum-package-installer-helper: cannot run fwupdmgr: {err}");
    std::process::exit(127)
}

/// The two pacman-key stages the keyring repair runs, in this order.
const KEYRING_STAGES: [&str; 2] = ["--init", "--populate"];

/// The repair's stages, and the verb's whole argument surface: pacman-key's
/// flags are the helper's to name, so one on argv could mean a keyring
/// operation nobody confirmed. The order is load-bearing — `--populate`
/// refuses to run on a keyring that holds no secret key, and `--init` is what
/// creates one. Neither stage asks a keyserver: the key files come from
/// `/usr/share/pacman/keyrings`, so a repair works offline.
fn keyring_stages(rest: &[OsString]) -> Option<&'static [&'static str]> {
    match rest {
        [] => Some(&KEYRING_STAGES),
        _ => None,
    }
}

/// The keyring repair — the third verb that runs no pacman, and the second
/// that cannot exec: `--init` has to finish before `--populate` starts. stdbuf
/// for the same measured reason as pacman and fwupdmgr: gpg writes through C
/// stdio, and through a pipe that buffers until the run ends.
fn fix_keyring(rest: &[OsString]) -> ! {
    let Some(stages) = keyring_stages(rest) else {
        usage()
    };
    let (last, first) = stages
        .split_last()
        .expect("the keyring repair names at least one stage");
    for stage in first {
        let mut cmd = keyring_command(stage);
        let status = match cmd.spawn().and_then(|mut child| child.wait()) {
            Ok(status) => status,
            Err(err) => {
                eprintln!("aelum-package-installer-helper: cannot run pacman-key: {err}");
                std::process::exit(127);
            }
        };
        // A signal's exit (no code) is not a success either: a repair that
        // never finished must not report the keyring as repaired.
        if status.code() != Some(0) {
            std::process::exit(status.code().unwrap_or(1));
        }
    }
    let err = keyring_command(last).exec();
    eprintln!("aelum-package-installer-helper: cannot run pacman-key: {err}");
    std::process::exit(127)
}

/// One stage of the keyring repair, in the same cleared environment as every
/// other program this helper runs: pacman-key is a shell script, and pkexec
/// hands it the caller's environment — which is how `BASH_ENV`, an `LD_*` or a
/// chosen `PATH` would otherwise choose what runs as root.
fn keyring_command(stage: &str) -> Command {
    let mut cmd = Command::new("/usr/bin/stdbuf");
    cmd.arg("-oL")
        .arg(PACMAN_KEY)
        .arg(stage)
        .env_clear()
        .env("PATH", ENV_PATH)
        .envs(locale_env());
    cmd
}

/// The caller variables the programs this helper runs are handed: they choose
/// the message language, nothing else.
fn locale_env() -> impl Iterator<Item = (&'static str, OsString)> {
    ENV_KEEP
        .iter()
        .filter_map(|key| std::env::var_os(key).map(|value| (*key, value)))
}

/// Whether any pacman is running right now. Its `comm` is the whole test:
/// the app runs one transaction at a time, so a live pacman is a live
/// transaction, whoever started it.
fn pacman_running() -> bool {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return false;
    };
    for entry in entries.flatten() {
        // Only PID directories: /proc holds plenty of names that never are.
        if !entry
            .file_name()
            .to_string_lossy()
            .bytes()
            .all(|byte| byte.is_ascii_digit())
        {
            continue;
        }
        // A process that exits mid-scan simply fails its read, which is
        // nobody's pacman.
        if let Ok(comm) = std::fs::read_to_string(entry.path().join("comm"))
            && comm.trim() == "pacman"
        {
            return true;
        }
    }
    false
}

/// pacman's DB lock is an empty file it opens with O_EXCL — measured on
/// pacman 7.1: mode 0000, zero bytes, no PID written — and alpm has no
/// dead-lock check at all. A lock left by a killed or crashed pacman
/// therefore answers every later transaction "unable to lock database",
/// whatever verb it asked for, forever. So every verb below clears one
/// first — but only with no live pacman anywhere: a lock nothing is holding
/// is nobody's, while a live one is a transaction that must not be run
/// twice at once and is left for pacman to refuse honestly.
fn clear_stale_lock() {
    if !Path::new(DB_LOCK).exists() {
        return;
    }
    if pacman_running() {
        println!(":: another pacman is running — leaving its database lock");
        return;
    }
    println!(":: removing stale database lock: {DB_LOCK}");
    // If a pacman took the lock between the scan and here, this delete loses
    // the race; pacman then reports the taken database either way, which is
    // the honest refusal the check aims at.
    let _ = std::fs::remove_file(DB_LOCK);
}

fn usage() -> ! {
    eprintln!(
        "usage: aelum-package-installer-helper \
         {{refresh|install|install-asdeps|remove|upgrade|file|file-asdeps|update-mirrors|firmware-update|fix-keyring}} \
         [args...]"
    );
    std::process::exit(2)
}

/// Run pacman, replacing this process — the same exec the script used, so
/// pkexec reports pacman's exit status and no parent is left behind.
fn exec_pacman(flags: &[OsString]) -> ! {
    let mut cmd = Command::new("/usr/bin/stdbuf");
    cmd.arg("-oL")
        .arg("/usr/bin/pacman")
        .args(flags)
        .env_clear()
        .env("PATH", ENV_PATH)
        .envs(locale_env());
    let err = cmd.exec();
    eprintln!("aelum-package-installer-helper: cannot run pacman: {err}");
    std::process::exit(127)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(verb: &str, rest: &[&str]) -> Vec<String> {
        let rest: Vec<OsString> = rest.iter().map(|name| (*name).into()).collect();
        pacman_args(verb, &rest)
            .expect("a known verb")
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn every_verb_runs_pacmans_command_unchanged() {
        // The dispatched script's seven mappings, flag for flag: this binary
        // replaced that script and must not quietly change an invocation.
        assert_eq!(argv("refresh", &[]), ["-Sy", "--noconfirm"]);
        assert_eq!(
            argv("install", &["htop"]),
            ["-S", "--needed", "--noconfirm", "--", "htop"]
        );
        // The asdeps verbs carry the marking makepkg's -s uses: what the AUR
        // flow brings that the user did not name is a dependency, never an
        // Explicit package that outlives every -Rns and reads as installed.
        assert_eq!(
            argv("install-asdeps", &["cmake"]),
            ["-S", "--needed", "--asdeps", "--noconfirm", "--", "cmake"]
        );
        assert_eq!(
            argv("remove", &["a", "b"]),
            ["-Rns", "--noconfirm", "--", "a", "b"]
        );
        assert_eq!(argv("upgrade", &[]), ["-Syu", "--noconfirm"]);
        assert_eq!(
            argv("file", &["/t/x.pkg.tar.zst"]),
            ["-U", "--noconfirm", "--", "/t/x.pkg.tar.zst"]
        );
        assert_eq!(
            argv("file-asdeps", &["/t/d.pkg.tar.zst"]),
            ["-U", "--asdeps", "--noconfirm", "--", "/t/d.pkg.tar.zst"]
        );
    }

    #[test]
    fn no_verb_reaches_pacman_without_noconfirm() {
        // The dialog owns the confirmation; a verb that lost the flag would
        // hang waiting for input on a pipe that has none.
        for verb in [
            "refresh",
            "install",
            "install-asdeps",
            "remove",
            "upgrade",
            "file",
            "file-asdeps",
        ] {
            assert!(
                argv(verb, &[]).iter().any(|arg| arg == "--noconfirm"),
                "{verb} would prompt"
            );
        }
    }

    #[test]
    fn an_unknown_verb_gets_no_pacman_argv() {
        // Including words shaped like pacman options: the verb is the only
        // thing here that may choose anything, and it chooses from seven.
        assert!(pacman_args("-Sy", &[]).is_none());
        assert!(pacman_args("rm", &[]).is_none());
        assert!(pacman_args("", &[]).is_none());
    }

    fn firmware_argv(rest: &[&str]) -> Option<Vec<String>> {
        let rest: Vec<OsString> = rest.iter().map(|name| (*name).into()).collect();
        fwupd_args(&rest).map(|args| {
            args.iter()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect()
        })
    }

    #[test]
    fn the_firmware_write_is_one_fixed_command() {
        // The one argv fwupdmgr ever sees: no device id, no flag nobody
        // asked for, and the prompts answered.
        assert_eq!(
            firmware_argv(&[]),
            Some(vec!["update".to_string(), "--assume-yes".to_string()])
        );
        // A device id on argv would be a write the confirmation never
        // named — refused like an unknown verb.
        assert_eq!(firmware_argv(&["f742a6af"]), None);
        assert_eq!(firmware_argv(&["--force"]), None);
    }

    fn stages(rest: &[&str]) -> Option<Vec<String>> {
        let rest: Vec<OsString> = rest.iter().map(|name| (*name).into()).collect();
        keyring_stages(&rest)
            .map(|stages| stages.iter().map(|stage| (*stage).to_string()).collect())
    }

    #[test]
    fn the_keyring_repair_is_two_stages_in_one_order() {
        // --init before --populate: --populate refuses to run on a keyring
        // holding no secret key to sign with, and --init is what creates one.
        assert_eq!(
            stages(&[]),
            Some(vec!["--init".into(), "--populate".into()])
        );
        // A pacman-key flag on argv would be a keyring operation the
        // confirmation never named — refused like an unknown verb.
        assert_eq!(stages(&["--populate"]), None);
        assert_eq!(stages(&["--keyserver"]), None);
        assert_eq!(stages(&["archlinux"]), None);
    }

    fn reflector_argv(rest: &[&str]) -> Vec<String> {
        let rest: Vec<OsString> = rest.iter().map(|name| (*name).into()).collect();
        reflector_args(&rest)
            .expect("a valid mirror update")
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn the_mirror_update_sorts_the_newest_https_mirrors_by_rate() {
        // The one fixed command of the feature, and the country lands as the
        // plain ISO code the app sends (reflector matches it case-free).
        assert_eq!(
            reflector_argv(&["ua"]),
            [
                "--latest",
                "20",
                "--protocol",
                "https",
                "--country",
                "ua",
                "--sort",
                "rate",
                "--save",
                "/etc/pacman.d/mirrorlist.aelum-tmp"
            ]
        );
        // No country means the installer's worldwide location: no --country
        // at all, every mirror still rate-sorted.
        let worldwide = reflector_argv(&[]);
        assert!(
            !worldwide.iter().any(|arg| arg == "--country"),
            "{worldwide:?}"
        );
        let tail: Vec<String> = worldwide.iter().rev().take(4).cloned().rev().collect();
        assert_eq!(
            tail,
            [
                "--sort",
                "rate",
                "--save",
                "/etc/pacman.d/mirrorlist.aelum-tmp"
            ],
            "{worldwide:?}"
        );
    }

    #[test]
    fn only_a_bare_country_code_reaches_the_reflector_argv() {
        // Anything else reads as an option, a second country, or a name the
        // app never sends: the verb names reflector's flags, argv adds at
        // most two letters.
        for bad in [
            "--country",
            "-c",
            "u",
            "usa",
            "united states",
            "ua --sort age",
            "",
        ] {
            let bad: Vec<OsString> = vec![bad.into()];
            assert!(
                reflector_args(&bad).is_none(),
                "{bad:?} is not a country code"
            );
        }
        let two: Vec<OsString> = vec!["ua".into(), "de".into()];
        assert!(reflector_args(&two).is_none(), "one country, one argument");
    }

    #[test]
    fn names_arrive_behind_the_double_dash() {
        // A package name like "-S" must stay a name and never become a
        // pacman option.
        assert_eq!(
            argv("install", &["-S"]),
            ["-S", "--needed", "--noconfirm", "--", "-S"]
        );
    }

    #[test]
    fn upgrade_takes_no_names() {
        // The script dropped "$@" for this verb; a stray name here would
        // widen the transaction nobody asked for.
        assert_eq!(argv("upgrade", &["linux"]), ["-Syu", "--noconfirm"]);
    }

    #[test]
    fn the_handed_environment_is_the_allowlist_alone() {
        // A regression here reopens the hole this binary exists to close:
        // only variables that choose pacman's language survive, and the
        // ones that can change what executes — or where — never do.
        assert_eq!(
            ENV_KEEP,
            ["LANG", "LC_ALL", "LC_CTYPE", "LC_MESSAGES", "LANGUAGE"]
        );
        for danger in [
            "LD_PRELOAD",
            "LD_LIBRARY_PATH",
            "BASH_ENV",
            "ENV",
            "PATH",
            "IFS",
            "TMPDIR",
        ] {
            assert!(
                !ENV_KEEP.contains(&danger),
                "{danger} must never reach pacman or a package's scriptlets"
            );
        }
    }
}
