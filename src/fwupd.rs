//! The firmware service's protocol: what `fwupdmgr` answers, in what shape.
//!
//! `fwupd` is a daemon the user's session talks to over D-Bus; `fwupdmgr` is
//! the client whose answers this app reads. Like the AUR's RPC, its output is
//! someone else's format, so every fact here is pinned against the fwupd
//! release the distro ships (2.1.10, `extra`) — re-verify when that moves.
//!
//! **fwupd 2.x emits `UpdateState` as an integer, not 1.9's string**
//! (`fwupd_device_add_json` adds it with `add_integer`; the strings exist only
//! in the human dump). The enum (`fwupd-enums.rs` @ 2.1.10):
//! 0 Unknown (never emitted — the writer only adds values > 0), 1 Pending,
//! 2 Success, 3 Failed, 4 NeedsReboot, 5 FailedTransient.
//!
//! **`get-updates --json` answers with the devices that have work**, not with
//! every device: a device the remotes do not support is skipped, and a device
//! whose upgrade query fails (nothing to do, not updatable) is skipped too
//! (`fu_util_get_updates_as_json`). So an empty `Devices` is fwupd's own "no
//! updates", not this app failing to read — and the page never counts devices
//! it cannot see.
//!
//! **`--json` disables all interactive prompts** (its own help text), which is
//! why the unprivileged reads carry it. The privileged counterpart belongs to
//! the packaged helper: `fwupdmgr update --assume-yes` answers every prompt
//! *without* hiding the human output the log dialog streams, and with no TTY
//! the final "Restart now?" prompt reads EOF, takes its default (no), and the
//! command exits 0 — the update stages and the machine reboots when its
//! owner says so.

/// The client the app runs; its presence is what "fwupd is installed" means.
pub const BINARY: &str = "/usr/bin/fwupdmgr";

/// The package that puts the daemon and this client on the system.
pub const PACKAGE: &str = "fwupd";

/// Metadata refresh before a check: `get-updates --json` deliberately skips
/// the "are the remotes old" refresh, so a check that wants today's answer
/// asks for it itself. No prompts, no output worth reading.
pub const REFRESH_ARGS: [&str; 2] = ["refresh", "--json"];

/// The check itself: every device the configured remotes carry a release for.
pub const GET_UPDATES_ARGS: [&str; 2] = ["get-updates", "--json"];

/// Machine-readable "restart pending": exit 0 iff some device is waiting for
/// a reboot, exit 2 (`FWUPD_ERROR_NOTHING_TO_DO` mapped, fu-util-common.h) if
/// not. `--json` keeps its failure silent, so the code is the whole answer.
pub const CHECK_REBOOT_ARGS: [&str; 2] = ["check-reboot-needed", "--json"];

/// What fwupd says about a device's update situation (integer values above).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpdateState {
    Unknown,
    /// Written, waiting to run (an offline/staged update).
    Pending,
    /// Applied.
    Success,
    /// An attempt failed.
    Failed,
    /// Applied, and the device's new firmware starts at the next reboot.
    NeedsReboot,
    /// Failed for a reason that goes away, e.g. AC power required.
    FailedTransient,
}

impl UpdateState {
    fn from_u64(value: u64) -> UpdateState {
        match value {
            1 => UpdateState::Pending,
            2 => UpdateState::Success,
            3 => UpdateState::Failed,
            4 => UpdateState::NeedsReboot,
            5 => UpdateState::FailedTransient,
            _ => UpdateState::Unknown,
        }
    }

    /// The state that means "finish this with a restart".
    pub fn needs_reboot(self) -> bool {
        matches!(self, UpdateState::Pending | UpdateState::NeedsReboot)
    }
}

/// The release a device can move to — the first entry of `Releases`, which is
/// the same one `fwupdmgr update` takes (it applies the first release the
/// filters let through).
#[derive(Clone, Debug)]
pub struct Release {
    pub version: String,
    /// The download size; absent from an answer that did not carry it.
    pub size: Option<u64>,
}

/// One device, as the check answered it.
#[derive(Clone, Debug)]
pub struct Device {
    pub name: String,
    pub version: Option<String>,
    pub state: UpdateState,
    /// The release to install, when the answer names one: this device is
    /// what the Update press will take.
    pub update: Option<Release>,
    /// fwupd's own sentence for a failed or blocked update — untranslated,
    /// and shown as it came, like a pacman error in a log.
    pub error: Option<String>,
}

impl Device {
    pub fn has_update(&self) -> bool {
        self.update.is_some()
    }
}

/// One check's whole answer.
#[derive(Clone, Debug)]
pub struct Answer {
    /// The devices fwupd answered about — in fwupd's answer, a listed
    /// device without `Releases` is up to date; one with them is waiting.
    pub devices: Vec<Device>,
    /// `check-reboot-needed`'s verdict: something was written and a restart
    /// will apply it.
    pub reboot_pending: bool,
    /// The metadata refresh failed, so this answer is as fresh as the
    /// metadata already on the machine. The page says so above the rows.
    pub refresh_failed: bool,
}

impl Answer {
    pub fn updates(&self) -> Vec<&Device> {
        self.devices.iter().filter(|d| d.has_update()).collect()
    }

    /// A restart that finishes something: the daemon's own verdict, or a
    /// device the answer says is waiting for one.
    pub fn needs_reboot(&self) -> bool {
        self.reboot_pending || self.devices.iter().any(|d| d.state.needs_reboot())
    }
}

/// Read `fwupdmgr get-updates --json`'s reply. Unknown keys are ignored and
/// missing optional keys are `None`: an answer with less in it than expected
/// is still an answer, and a body that is not this JSON is an error — an
/// empty device list must mean fwupd found nothing, never "I could not read
/// it" (the same rule the package databases hold to).
pub fn parse_get_updates(body: &str) -> Result<Vec<Device>, String> {
    let root: serde_json::Value =
        serde_json::from_str(body).map_err(|e| format!("fwupdmgr did not answer in JSON: {e}"))?;
    let devices = root
        .get("Devices")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "the firmware answer has no Devices list".to_string())?;
    let mut out = Vec::with_capacity(devices.len());
    for device in devices {
        let name = device
            .get("Name")
            .and_then(|v| v.as_str())
            .unwrap_or("(unnamed device)")
            .to_string();
        let update = device
            .get("Releases")
            .and_then(|v| v.as_array())
            .and_then(|rels| rels.first())
            .map(|rel| Release {
                version: rel
                    .get("Version")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?")
                    .to_string(),
                size: rel.get("Size").and_then(|v| v.as_u64()),
            });
        out.push(Device {
            name,
            version: device
                .get("Version")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            state: device
                .get("UpdateState")
                .and_then(|v| v.as_u64())
                .map(UpdateState::from_u64)
                .unwrap_or(UpdateState::Unknown),
            update,
            error: device
                .get("UpdateError")
                .and_then(|v| v.as_str())
                .map(str::to_string),
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn devices(body: &str) -> Vec<Device> {
        parse_get_updates(body).expect("parses")
    }

    #[test]
    fn a_real_get_updates_answer_is_read() {
        let body = include_str!("../tests/fixtures/fwupd_get_updates.json");
        let got = devices(body);
        assert_eq!(got.len(), 3);

        // The NVMe: the update the check exists for — versions and size in
        // hand, state up-to-date's absence (fwupd 2.x omits UpdateState for
        // a device that is simply current).
        let nvme = &got[0];
        assert_eq!(nvme.name, "WDC PC SN730 SDBPNTY-512G");
        assert_eq!(nvme.version.as_deref(), Some("73010000"));
        assert_eq!(nvme.state, UpdateState::Unknown);
        let rel = nvme.update.as_ref().expect("the release to install");
        assert_eq!(rel.version, "73021000");
        assert_eq!(rel.size, Some(5185536));
        assert!(nvme.has_update());
        assert!(nvme.error.is_none());

        // A device fwupd listed but has no release for is the answer's way
        // of saying "up to date" — it has no update and takes no press.
        let hub = &got[1];
        assert_eq!(hub.name, "ThinkPad Dock USB3Hub");
        assert!(hub.update.is_none());
        assert!(!hub.has_update());

        // The dbx waiting on a restart: the integer state, not a string.
        // Its update is written, so no release is offered — the restart is
        // the only work left, and the row says exactly that.
        let dbx = &got[2];
        assert_eq!(dbx.name, "UEFI dbx");
        assert_eq!(dbx.state, UpdateState::NeedsReboot);
        assert!(dbx.state.needs_reboot());
        assert!(!dbx.has_update());
    }

    #[test]
    fn a_failed_device_keeps_fwupds_own_sentence() {
        let body = include_str!("../tests/fixtures/fwupd_get_updates_failed.json");
        let got = devices(body);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].state, UpdateState::Failed);
        assert_eq!(got[0].error.as_deref(), Some("no space left on device"));
        assert!(got[0].has_update());
    }

    #[test]
    fn the_empty_answer_is_the_empty_list() {
        // "No updates available for any device" is fwupd's own silence; an
        // empty Devices array is the honest shape of it, not a failure.
        assert!(devices(r#"{"Devices":[]}"#).is_empty());
    }

    #[test]
    fn a_body_that_is_not_the_answer_is_an_error() {
        // Translated human text, or the daemon's D-Bus error — the page must
        // say "the check failed" and not show an empty list.
        for bad in ["", "Loading devices…", "{", r#"{"Updates":[]}"#] {
            assert!(parse_get_updates(bad).is_err(), "{bad:?} is not the answer");
        }
    }

    #[test]
    fn an_answer_with_less_in_it_is_still_an_answer() {
        // A device with neither a state, a version, nor a size still reads:
        // the row shows what is there, and an update still counts.
        let got = devices(r#"{"Devices":[{"Name":"Widget","Releases":[{"Version":"2"}]}]}"#);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].version, None);
        assert_eq!(got[0].state, UpdateState::Unknown);
        assert_eq!(got[0].update.as_ref().unwrap().size, None);
    }

    #[test]
    fn only_written_states_claim_a_restart() {
        for state in [UpdateState::Pending, UpdateState::NeedsReboot] {
            assert!(state.needs_reboot(), "{state:?} waits for a restart");
        }
        for state in [
            UpdateState::Unknown,
            UpdateState::Success,
            UpdateState::Failed,
            UpdateState::FailedTransient,
        ] {
            assert!(!state.needs_reboot(), "{state:?} does not");
        }
    }

    #[test]
    fn the_answer_adds_its_reboot_verdict_to_the_devices() {
        let one = Device {
            name: "d".into(),
            version: None,
            state: UpdateState::Pending,
            update: None,
            error: None,
        };
        let up = Answer {
            devices: vec![one],
            reboot_pending: false,
            refresh_failed: false,
        };
        assert!(up.needs_reboot());
        assert_eq!(up.updates().len(), 0);
    }
}
