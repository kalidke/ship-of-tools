// update.check and update.apply, and the update's state (update.state, the hello's and the version answer's `update`)

use super::*;

// ─── Auto-update (ADR 0030 §4, Phase C) ─────────────────────────────────

/// `update.check` request — no fields today (the backend always checks its
/// configured repo against its own embedded version). Kept as a struct so a
/// future forced-channel / forced-repo override can land additively.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UpdateCheckReq {}

/// `update.check` response. `current` is the running product version
/// (`app_version()`); `latest` is the newest release's version with the tag's
/// leading `v` stripped (empty when the check couldn't run). `update_available`
/// is true only when `latest` is a strictly newer semver than `current`.
/// `staged` is true once the platform asset for `latest` has been downloaded,
/// sha256-verified, and unpacked into the staging dir. `status` is a
/// human/structured string: `"ok"`, `"disabled: dev build"`,
/// `"disabled: update mode off"`, `"disabled: the install is a dev build"`, `"disabled: pinned at <version>: ..."`,
/// `"disabled: install.json cannot be read at <path> (<cause>); ..."` or, on Windows, `"disabled: install.json is missing at
/// <path> ..."` (an unreadable or missing record may hold a pin; the sentence names the installer run that repairs or
/// rewrites it), `"update blocked: <why>"` or `"check unavailable: <why>"`.
/// The Phase-C identity fields (`tag` … `asset_sha256`) pin the exact release
/// the backend saw so a frontend on another machine can stage the SAME
/// release rather than re-resolving "latest" (which may have moved). All
/// default so older peers interoperate.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UpdateCheckRes {
    pub current: String,
    pub latest: String,
    pub update_available: bool,
    pub staged: bool,
    pub status: String,
    /// Full release tag (`vX.Y.Z`) of `latest`; empty when no check ran.
    #[serde(default)]
    pub tag: String,
    /// `owner/repo` the backend checks against.
    #[serde(default)]
    pub repo: String,
    /// The BACKEND's release-matrix platform (its own asset's target).
    #[serde(default)]
    pub target: String,
    /// sha256 of the backend-platform asset, from the release's SHA256SUMS.
    #[serde(default)]
    pub asset_sha256: String,
    /// True once the versioned checkout + Julia envs for `latest` are
    /// prepared on the backend host (Phase C2 transactional prepare).
    #[serde(default)]
    pub prepared: bool,
    /// True once the pending pointer arms `latest` for apply at next launch.
    #[serde(default)]
    pub armed: bool,
}

/// `update.apply` request — no fields (applies whatever is armed).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UpdateApplyReq {}

/// `update.apply` response, sent BEFORE the daemon exits. `ok` false means
/// nothing was armed (or the arm failed validation) and the daemon stays up;
/// `status` says why. On `ok` true, `tag` is the release being applied and
/// `will_restart` tells the frontend whether the backend comes back on its
/// own (systemd) or needs the user's next launch.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UpdateApplyRes {
    pub ok: bool,
    pub tag: String,
    pub will_restart: bool,
    pub status: String,
}

// ─── The update's state (guarantee 16) ──────────────────────────────────

/// The most bytes of [`UpdateState::version`] and [`UpdateState::armed`].
pub const MAX_UPDATE_VERSION: usize = 64;
/// The most bytes of [`UpdateState::detail`].
pub const MAX_UPDATE_DETAIL: usize = 300;

/// What an updater says of the newest release. A phase a later release adds reads as `other`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdatePhase {
    /// Checks are on and none has run yet in this process.
    Unchecked,
    /// The last check found nothing above the install.
    None,
    /// A check found a release above the install, and nothing has begun to stage it.
    Found,
    /// The release is being downloaded, checked out and its environments built.
    Preparing,
    /// The release is armed: the next start through its apply owner applies it, unless the start's own checks drop it.
    Ready,
    /// A found release was not made ready, or an armed one would be refused.
    NotApplied,
    /// The install record cannot be read, so automatic updates are off until a person repairs it.
    Blocked,
    /// Automatic updates are off by the person's own choice (a pin, the update mode, a dev build).
    Off,
    /// The last check could not run.
    CheckFailed,
    #[serde(other)]
    Other,
}

/// The state of one computer's update, as its owner tells it: the payload of the `update.state` evt and the `update`
/// field of a hello reply and of a version answer. `version` is the release the phase is about (bare, no leading `v`;
/// empty when there is none); `detail` one sentence a person can act on; `armed` the version a start would apply now,
/// sent only when it is another than `version`; `checked_s` the seconds since the last check that ran, counted by the
/// teller at each send; `unread` true when the teller could not read the armed update from disk within its bound for
/// this answer, so the state is what it last read. The strings are bounded and cleaned ([`bounded`]) when written, when
/// read and when worded, so no reader prints a control character a file or a child's output held.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateState {
    pub phase: UpdatePhase,
    #[serde(default, skip_serializing_if = "String::is_empty", serialize_with = "cut_out::<_, MAX_UPDATE_VERSION>", deserialize_with = "cut_in::<_, MAX_UPDATE_VERSION>")]
    pub version: String,
    #[serde(default, skip_serializing_if = "String::is_empty", serialize_with = "cut_out::<_, MAX_UPDATE_DETAIL>", deserialize_with = "cut_in::<_, MAX_UPDATE_DETAIL>")]
    pub detail: String,
    #[serde(default, skip_serializing_if = "String::is_empty", serialize_with = "cut_out::<_, MAX_UPDATE_VERSION>", deserialize_with = "cut_in::<_, MAX_UPDATE_VERSION>")]
    pub armed: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checked_s: Option<u64>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub unread: bool,
}

/// What stands where a text past its bound was cut.
const CUT_MARK: &str = " \u{2026} ";

/// `text` as a terminal may print it, within `max` bytes. Each control character (the ESC of an escape sequence, a
/// newline, a carriage return) becomes a space. A text past the bound keeps its beginning and its end with
/// [`CUT_MARK`] between them: a sentence names what to run at its end, and a cause chain its fault.
fn bounded(text: &str, max: usize) -> String {
    let clean: String = text.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    if clean.len() <= max {
        return clean;
    }
    let keep = max.saturating_sub(CUT_MARK.len());
    let (mut head, mut tail) = (keep / 2, clean.len() - (keep - keep / 2));
    while !clean.is_char_boundary(head) {
        head -= 1;
    }
    while !clean.is_char_boundary(tail) {
        tail += 1;
    }
    format!("{}{CUT_MARK}{}", &clean[..head], &clean[tail..])
}

fn cut_out<S: serde::Serializer, const N: usize>(text: &str, ser: S) -> Result<S::Ok, S::Error> {
    ser.serialize_str(&bounded(text, N))
}

fn cut_in<'de, D: serde::Deserializer<'de>, const N: usize>(d: D) -> Result<String, D::Error> {
    Ok(bounded(&String::deserialize(d)?, N))
}

/// `deserialize_with` of a carrier's `update` (`HelloRes`, `DaemonVersion`): a value that is not an update state reads
/// as none. The state is told to a person and decides nothing, so it is never a reason to refuse a hello reply, which
/// would be a window that cannot connect.
pub(super) fn lenient_update<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<UpdateState>, D::Error> {
    Ok(serde_json::from_value(serde_json::Value::deserialize(d)?).ok())
}

/// How long ago, in the one unit a person reads at a glance.
fn ago(secs: u64) -> String {
    match secs {
        0..=59 => "just now".to_string(),
        60..=3599 => format!("{} min ago", secs / 60),
        3600..=172_799 => format!("{} h ago", secs / 3600),
        _ => format!("{} d ago", secs / 86_400),
    }
}

impl UpdateState {
    /// A state of `phase` and nothing else.
    pub fn of(phase: UpdatePhase) -> Self {
        Self { phase, version: String::new(), detail: String::new(), armed: String::new(), checked_s: None, unread: false }
    }

    /// This state as the wire carries it: each string clean and within its bound.
    fn bounded(&self) -> Self {
        Self {
            version: bounded(&self.version, MAX_UPDATE_VERSION),
            detail: bounded(&self.detail, MAX_UPDATE_DETAIL),
            armed: bounded(&self.armed, MAX_UPDATE_VERSION),
            ..self.clone()
        }
    }

    /// The standing mark a window draws for this state, its long words and its short ones, or `None` for a state that
    /// has no mark: only a release found, preparing, ready or not applied, and a blocked install record, have one. The
    /// one text of each, as `ChatCode::text` is a code's.
    pub fn mark(&self) -> Option<(String, String)> {
        let v = &bounded(&self.version, MAX_UPDATE_VERSION);
        let about = |long: &str, short: &str| (!v.is_empty()).then(|| (format!("update {v} {long}"), format!("{v} {short}")));
        match self.phase {
            UpdatePhase::Found => about("found", "found"),
            UpdatePhase::Preparing => about("preparing", "preparing"),
            UpdatePhase::Ready => about("ready: applies at next start", "ready"),
            UpdatePhase::NotApplied => about("not applied: see sotd status", "not applied"),
            UpdatePhase::Blocked => Some(("updates off: install record unreadable".to_string(), "updates off".to_string())),
            UpdatePhase::Unchecked | UpdatePhase::None | UpdatePhase::Off | UpdatePhase::CheckFailed | UpdatePhase::Other => None,
        }
    }

    /// The one line that says this state whole: the `UPDATE` cell of `sotd status`, and what a daemon sends beside
    /// the state for a reader that cannot call this (`DaemonVersion.update_line`). Its strings are bounded and clean.
    pub fn line(&self) -> String {
        let this = self.bounded();
        let v = if this.version.is_empty() { "a release" } else { this.version.as_str() };
        let d = &this.detail;
        let with = |head: String| if d.is_empty() { head } else { format!("{head}: {d}") };
        let checked = |head: &str| this.checked_s.map_or_else(|| head.to_string(), |s| format!("{head} {}", ago(s)));
        let meanwhile = match this.armed.as_str() {
            "" => String::new(),
            armed if armed == this.version => String::new(),
            armed => format!("; {armed} is ready meanwhile"),
        };
        let said = match this.phase {
            UpdatePhase::Unchecked => "not checked yet".to_string(),
            UpdatePhase::None => match this.checked_s {
                Some(s) => format!("up to date (checked {})", ago(s)),
                None => "up to date".to_string(),
            },
            UpdatePhase::Found => with(format!("{v} found")) + &meanwhile,
            UpdatePhase::Preparing if meanwhile.is_empty() => format!("{v} preparing (can take many minutes)"),
            UpdatePhase::Preparing => format!("{v} preparing{meanwhile}"),
            UpdatePhase::Ready => with(format!("{v} ready")),
            UpdatePhase::NotApplied if this.version.is_empty() => {
                let head = "an armed update cannot be read; the next start drops it";
                if d.is_empty() { head.to_string() } else { format!("{head} ({d})") }
            }
            UpdatePhase::NotApplied => with(format!("{v} not applied")) + &meanwhile,
            UpdatePhase::Blocked | UpdatePhase::Off => with("off".to_string()),
            UpdatePhase::CheckFailed => with(checked("check could not run")),
            UpdatePhase::Other if d.is_empty() => "state not known to this sotd".to_string(),
            UpdatePhase::Other => d.clone(),
        };
        if this.unread { format!("{said}; as last read, the disk did not answer in time") } else { said }
    }
}

#[cfg(test)]
#[path = "update_tests.rs"]
mod tests;
