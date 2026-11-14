// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Finding the vendor build in the atpkg store, never through `PATH`.
//!
//! The layout is aterm's (aterm's main of 2026-10-24): the prefix is
//! `~/Library/Application Support/aterm/pkg` on macOS and
//! `~/.local/share/aterm/pkg` elsewhere (`crates/atpkg/src/platform/unix.rs:22-34`);
//! a shipped atpkg ignores any configured `[packages] prefix`
//! (`crates/atpkg/src/config.rs:510-549`, `admit_packages`), so this harness reads
//! only the default. A program's builds live at `store/<program>/<build>/`, with
//! `store/<program>/current -> <build dir>` (`crates/atpkg/src/store.rs:70-125`),
//! each build marked usable by `<build>.ready` (first line `ok`, an optional
//! `platform=<arch>-<os>` that must be this machine's; `store.rs:366-445`), its
//! provenance in `<build>.vendor` (`version = "…"`), and the environment its shim
//! exports in `<build>.shim-env` (`crates/atpkg/src/shim_env.rs`).
//!
//! The harness execs `<build>/bin/<command>` by the build's own path, as the
//! `agents/` twin does, so a `current` flip during a launch cannot race it, and
//! it refuses a binary that is itself, or that it cannot compare with itself,
//! so it can never exec itself.

use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

/// The atpkg prefix under `home` (see the module docs).
#[must_use]
pub fn default_prefix(home: &Path) -> PathBuf {
    if cfg!(target_os = "macos") {
        home.join("Library")
            .join("Application Support")
            .join("aterm")
            .join("pkg")
    } else {
        home.join(".local").join("share").join("aterm").join("pkg")
    }
}

/// The platform word atpkg records in `.ready` (`store.rs:381-383`, `running_platform`).
#[must_use]
pub fn running_platform() -> String {
    format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS)
}

/// A store build directory name: a decimal number with no leading zero
/// (`store.rs:1143-1151`, `parse_build_name`).
#[must_use]
pub fn parse_build_name(name: &str) -> Option<u64> {
    if name.is_empty() || !name.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if name.len() > 1 && name.starts_with('0') {
        return None;
    }
    name.parse().ok()
}

/// Whether a `.ready` marker admits its build on `running` (`store.rs:425-433`).
#[must_use]
pub fn ready_text_accepts(text: &str, running: &str) -> bool {
    let first_ok = matches!(text.lines().next(), Some(first) if first.trim() == "ok");
    if !first_ok {
        return false;
    }
    let recorded = text
        .lines()
        .filter_map(|line| line.trim().strip_prefix("platform="))
        .map(str::trim)
        .find(|p| !p.is_empty());
    recorded.is_none_or(|p| p == running)
}

/// The `version = "…"` line of a `.vendor` provenance record.
#[must_use]
pub fn vendor_version(text: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let rest = line.trim().strip_prefix("version")?.trim_start();
        let rest = rest.strip_prefix('=')?.trim();
        let value = rest.strip_prefix('"')?.strip_suffix('"')?;
        let ok = !value.is_empty() && value.bytes().all(|b| b.is_ascii_graphic() && b != b'"');
        ok.then(|| value.to_string())
    })
}

/// Whether a `.vendor` record's version `recorded` names build `number`: it is
/// exactly the version the number encodes ([`decode_vendor_build`]), so the
/// record and the build directory agree before either is believed (aterm's store
/// reads a build the same way). The number is decoded only to check the record,
/// never to stand in for one.
#[must_use]
pub fn record_names_build(recorded: &str, number: u64) -> bool {
    decode_vendor_build(number).as_deref() == Some(recorded)
}

/// The smallest vendor build number (`0.0.0`); an ALab build number is below it.
const VENDOR_BASE: u64 = 1_000_000_000_000_000_000;

/// The vendor version a store build number encodes (aterm
/// `crates/atpkg/src/vendor_direct/version.rs`): `x.y.z` is
/// `10^18 + x·10^12 + y·10^6 + z`, and `x.y.z-R<n>[.<s>]` is
/// `2·10^18 + x·10^15 + y·10^12 + z·10^9 + n·10^3 + s'` (`s'` is 0, or `s + 1`).
/// `None` below `10^18` (an ALab build) or from `3·10^18` on.
#[must_use]
pub fn decode_vendor_build(number: u64) -> Option<String> {
    let tier = number / VENDOR_BASE;
    let v = number % VENDOR_BASE;
    match tier {
        1 => Some(format!(
            "{}.{}.{}",
            v / 1_000_000_000_000,
            v / 1_000_000 % 1_000_000,
            v % 1_000_000
        )),
        2 => {
            let (x, y, z) = (
                v / 1_000_000_000_000_000 % 1_000,
                v / 1_000_000_000_000 % 1_000,
                v / 1_000_000_000 % 1_000,
            );
            let (n, sub) = (v / 1_000 % 1_000_000, v % 1_000);
            Some(match sub.checked_sub(1) {
                None => format!("{x}.{y}.{z}-R{n}"),
                Some(s) => format!("{x}.{y}.{z}-R{n}.{s}"),
            })
        }
        _ => None,
    }
}

/// Names a shim never sets (`shim_env.rs:17-38`, `NEVER_SET`).
const NEVER_SET: &[&str] = &[
    "PATH",
    "HOME",
    "IFS",
    "ENV",
    "BASH_ENV",
    "SHELLOPTS",
    "SHELL",
    "PWD",
    "USER",
    "LOGNAME",
    "TMPDIR",
    "PATHEXT",
    "COMSPEC",
    "SYSTEMROOT",
    "NODE_OPTIONS",
    "PYTHONPATH",
    "PYTHONSTARTUP",
    "PERL5OPT",
    "RUBYOPT",
];

/// Name prefixes a shim never sets (`shim_env.rs:41`).
const NEVER_SET_PREFIXES: &[&str] = &["LD_", "DYLD_"];

/// The most entries a shim environment may hold (`shim_env.rs:12`).
const MAX_SHIM_ENV: usize = 8;

/// The longest entry (`shim_env.rs:15`).
const MAX_ENTRY_BYTES: usize = 256;

/// Parse a `.shim-env` sidecar the way atpkg does: one `NAME=VALUE` per line,
/// every entry under atpkg's rule, and the whole sidecar refused (no
/// environment) when any entry breaks it (`shim_env.rs:168-235`, `from_lines`).
#[must_use]
pub fn parse_shim_env(text: &str) -> Vec<(String, String)> {
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect();
    if lines.len() > MAX_SHIM_ENV {
        return Vec::new();
    }
    let mut out: Vec<(String, String)> = Vec::new();
    for entry in lines {
        let Some((name, value)) = admit_entry(entry) else {
            return Vec::new();
        };
        if out.iter().any(|(n, _)| n == name) {
            return Vec::new();
        }
        out.push((name.to_string(), value.to_string()));
    }
    out
}

fn admit_entry(entry: &str) -> Option<(&str, &str)> {
    if entry.len() > MAX_ENTRY_BYTES {
        return None;
    }
    let (name, value) = entry.split_once('=')?;
    let name_ok = !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
        && !name.as_bytes().first().is_some_and(u8::is_ascii_digit)
        && !NEVER_SET.contains(&name)
        && !NEVER_SET_PREFIXES.iter().any(|p| name.starts_with(p));
    let value_ok = !value.is_empty()
        && !value.chars().any(char::is_control)
        && value.trim() == value
        && !value.contains('"')
        && !value.contains('%');
    (name_ok && value_ok).then_some((name, value))
}

/// One usable vendor build.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Build {
    /// The build number (the store directory's name).
    pub number: u64,
    /// `store/<program>/<number>`.
    pub dir: PathBuf,
    /// `<dir>/bin/<command>`, the file the harness execs.
    pub exe: PathBuf,
    /// The vendor version `<number>.vendor` records, only where it names this very
    /// build (the version the number encodes, [`record_names_build`]); `None` when
    /// the record is missing, unreadable or names another version. Never decoded
    /// from the number alone.
    pub version: Option<String>,
    /// The environment the build's shim exports (`<number>.shim-env`).
    pub env: Vec<(String, String)>,
}

/// Why no vendor build can be run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Missing {
    /// No `current` link: atpkg has not installed the program.
    NotInstalled(PathBuf),
    /// `current` exists but does not name a usable build; the text says why.
    Unusable(PathBuf, String),
    /// The build's binary is this harness itself.
    IsSelf(PathBuf),
    /// The build's binary could not be compared with this harness (the harness
    /// does not know its own executable, or one of the two files could not be
    /// read for the comparison), so it is not known not to be the harness; the
    /// text says which.
    Uncompared(PathBuf, String),
}

impl Missing {
    /// The journal's word for this refusal (`refused <word> …`).
    #[must_use]
    pub const fn word(&self) -> &'static str {
        match self {
            Missing::NotInstalled(_) => "vendor-not-installed",
            Missing::Unusable(..) => "vendor-unusable",
            Missing::IsSelf(_) => "vendor-is-self",
            Missing::Uncompared(..) => "vendor-uncompared",
        }
    }
}

impl std::fmt::Display for Missing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Missing::NotInstalled(link) => write!(
                f,
                "the vendor CLI is not installed by atpkg ({} does not exist); install it with `aterm pkg install {}`",
                link.display(),
                crate::profile::PACKAGE
            ),
            Missing::Unusable(link, why) => write!(
                f,
                "the atpkg build {} names is not usable: {why}; `aterm pkg doctor` checks the store",
                link.display()
            ),
            Missing::IsSelf(exe) => write!(
                f,
                "refusing to run {}: it is this harness itself, not the vendor CLI",
                exe.display()
            ),
            Missing::Uncompared(exe, why) => write!(
                f,
                "refusing to run {}: cannot check that it is not this harness itself ({why})",
                exe.display()
            ),
        }
    }
}

/// The store of one program under one prefix.
#[derive(Clone, Debug)]
pub struct Store {
    /// `<prefix>/store/<program>`.
    pub program_dir: PathBuf,
    /// The command inside a build's `bin/`.
    pub command: String,
}

impl Store {
    /// The store of `program` (whose binary is `bin/<command>`) under `prefix`.
    #[must_use]
    pub fn new(prefix: &Path, program: &str, command: &str) -> Self {
        Self {
            program_dir: prefix.join("store").join(program),
            command: command.to_string(),
        }
    }

    /// `store/<program>/current`.
    #[must_use]
    pub fn current_link(&self) -> PathBuf {
        self.program_dir.join("current")
    }

    /// The build number `current` names, read without validating the build: the
    /// cheap check the update watch runs on every tick.
    #[must_use]
    pub fn current_number(&self) -> Option<u64> {
        let target = std::fs::read_link(self.current_link()).ok()?;
        parse_build_name(target.file_name()?.to_str()?)
    }

    /// The build `current` names, validated: a build directory of this program
    /// whose `.ready` admits it on this machine and whose binary is a regular,
    /// executable file that is proved not to be `own_exe`, this harness's own
    /// executable. The proof is required: with no `own_exe`, or when either
    /// file cannot be read for the comparison, the build is refused
    /// ([`Missing::Uncompared`]) rather than run uncompared.
    ///
    /// # Errors
    /// [`Missing`] says which check refused.
    pub fn current(&self, own_exe: Option<&Path>) -> Result<Build, Missing> {
        let link = self.current_link();
        let target = match std::fs::read_link(&link) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(Missing::NotInstalled(link));
            }
            Err(e) => {
                return Err(Missing::Unusable(
                    link,
                    format!("cannot read the link: {e}"),
                ));
            }
        };
        let unusable = |why: String| Missing::Unusable(link.clone(), why);
        let number = target
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(parse_build_name)
            .ok_or_else(|| unusable(format!("{} is not a build directory", target.display())))?;
        // atpkg writes `current` as an absolute link into the same program store;
        // a relative one resolves against the link's own directory.
        let dir = if target.is_absolute() {
            target.clone()
        } else {
            self.program_dir.join(&target)
        };
        if dir.parent() != Some(self.program_dir.as_path())
            || dir.components().any(|c| matches!(c, Component::ParentDir))
        {
            return Err(unusable(format!(
                "{} is outside {}",
                dir.display(),
                self.program_dir.display()
            )));
        }
        self.build(number, own_exe)
            .map_err(|refusal| match refusal {
                Refusal::IsSelf(exe) => Missing::IsSelf(exe),
                Refusal::Uncompared(exe, why) => Missing::Uncompared(exe, why),
                Refusal::Unusable(why) => unusable(why),
            })
    }

    /// Validate build `number` of this store.
    ///
    /// # Errors
    /// Why the build is not usable.
    fn build(&self, number: u64, own_exe: Option<&Path>) -> Result<Build, Refusal> {
        use Refusal::Unusable;
        let name = number.to_string();
        let dir = self.program_dir.join(&name);
        let meta = std::fs::symlink_metadata(&dir)
            .map_err(|e| Unusable(format!("{}: {e}", dir.display())))?;
        if !meta.is_dir() {
            return Err(Unusable(format!("{} is not a directory", dir.display())));
        }
        let ready = self.program_dir.join(format!("{name}.ready"));
        let text = read_small(&ready)
            .ok_or_else(|| Unusable(format!("{} is missing", ready.display())))?;
        if !ready_text_accepts(&text, &running_platform()) {
            return Err(Unusable(format!(
                "{} does not admit it on {}",
                ready.display(),
                running_platform()
            )));
        }
        let bin_dir = dir.join("bin");
        let bin_meta = std::fs::symlink_metadata(&bin_dir)
            .map_err(|e| Unusable(format!("{}: {e}", bin_dir.display())))?;
        if !bin_meta.is_dir() {
            return Err(Unusable(format!(
                "{} is not a directory",
                bin_dir.display()
            )));
        }
        let exe = bin_dir.join(&self.command);
        let exe_meta =
            std::fs::metadata(&exe).map_err(|e| Unusable(format!("{}: {e}", exe.display())))?;
        if !exe_meta.is_file() || !is_executable(&exe_meta) {
            return Err(Unusable(format!(
                "{} is not an executable file",
                exe.display()
            )));
        }
        // Fail closed: a vendor binary that cannot be compared with the
        // harness is not known to be something else.
        let Some(own) = own_exe else {
            return Err(Refusal::Uncompared(
                exe,
                "the harness cannot determine its own executable".to_string(),
            ));
        };
        match same_file(&exe, own) {
            Ok(false) => {}
            Ok(true) => return Err(Refusal::IsSelf(exe)),
            Err(e) => {
                return Err(Refusal::Uncompared(
                    exe,
                    format!("comparing it with {}: {e}", own.display()),
                ));
            }
        }
        let version = read_small(&self.program_dir.join(format!("{name}.vendor")))
            .as_deref()
            .and_then(vendor_version)
            .filter(|v| record_names_build(v, number));
        let env = read_small(&self.program_dir.join(format!("{name}.shim-env")))
            .as_deref()
            .map(parse_shim_env)
            .unwrap_or_default();
        Ok(Build {
            number,
            dir,
            exe,
            version,
            env,
        })
    }
}

/// Why `Store::build` refused a build.
enum Refusal {
    /// The binary is this harness.
    IsSelf(PathBuf),
    /// The binary could not be compared with this harness; the text says why.
    Uncompared(PathBuf, String),
    /// The build is not usable; the text says why.
    Unusable(String),
}

/// Whether `a` and `b` are the same file (device and inode, after symlinks).
///
/// # Errors
/// Either file cannot be read for the comparison: the answer is then unknown,
/// never "different".
pub fn same_file(a: &Path, b: &Path) -> std::io::Result<bool> {
    use std::os::unix::fs::MetadataExt as _;
    let (x, y) = (std::fs::metadata(a)?, std::fs::metadata(b)?);
    Ok(x.dev() == y.dev() && x.ino() == y.ino())
}

fn is_executable(meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    meta.permissions().mode() & 0o111 != 0
}

/// Read a small metadata file (≤ 64 KiB, regular, not a symlink) as UTF-8.
fn read_small(path: &Path) -> Option<String> {
    read_bounded(path, 64 * 1024, false)
}

/// Read `path` as UTF-8 when it is a regular file of at most `max` bytes, with
/// the type check, the size check and the read all on ONE open handle: a path
/// checked and then opened again could name a different file by the time of
/// the read (a file swapped in between, or a symlink to `/dev/zero`, would
/// have been read without bound). A symlink at `path` is followed only when
/// `follow` is set; otherwise the open itself refuses it (`O_NOFOLLOW`). The
/// open never waits on a FIFO (`O_NONBLOCK`, which a regular file's read
/// ignores), and the read stops one byte past `max` even if the file grows
/// after the check.
pub(crate) fn read_bounded(path: &Path, max: u64, follow: bool) -> Option<String> {
    use std::io::Read as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let mut flags = libc::O_NONBLOCK;
    if !follow {
        flags |= libc::O_NOFOLLOW;
    }
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(flags)
        .open(path)
        .ok()?;
    let meta = file.metadata().ok()?;
    if !meta.is_file() || meta.len() > max {
        return None;
    }
    let mut text = String::new();
    file.take(max.saturating_add(1))
        .read_to_string(&mut text)
        .ok()?;
    (u64::try_from(text.len()).ok()? <= max).then_some(text)
}

/// The user's home: `$HOME` when absolute; when `$HOME` is unset or empty, the
/// password database's entry (std's `home_dir`, which reads `getpwuid_r`); a
/// relative `$HOME` names no place this harness can trust, so it is refused.
#[must_use]
pub fn home_dir(env_home: Option<OsString>) -> Option<PathBuf> {
    match env_home.map(PathBuf::from) {
        Some(h) if h.is_absolute() => Some(h),
        Some(h) if !h.as_os_str().is_empty() => None,
        _ => std::env::home_dir().filter(|p| p.is_absolute()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_is_atpkgs_default() {
        let p = default_prefix(Path::new("/h/u"));
        if cfg!(target_os = "macos") {
            assert_eq!(p, Path::new("/h/u/Library/Application Support/aterm/pkg"));
        } else {
            assert_eq!(p, Path::new("/h/u/.local/share/aterm/pkg"));
        }
    }

    #[test]
    fn a_record_names_its_build_only_with_the_version_the_number_encodes() {
        let number = 1_000_002_000_001_000_310;
        assert_eq!(decode_vendor_build(number).as_deref(), Some("2.1.310"));
        assert!(record_names_build("2.1.310", number));
        for other in ["2.1.311", "2.1.0310", "02.1.310", "2.1", "", "2.1.310-R1"] {
            assert!(!record_names_build(other, number), "{other:?}");
        }
        assert_eq!(
            decode_vendor_build(2_001_004_013_006_996_002).as_deref(),
            Some("1.4.13-R6996.1")
        );
        assert_eq!(
            decode_vendor_build(2_001_004_013_006_996_000).as_deref(),
            Some("1.4.13-R6996")
        );
        assert_eq!(decode_vendor_build(VENDOR_BASE).as_deref(), Some("0.0.0"));
        assert_eq!(decode_vendor_build(VENDOR_BASE - 1), None, "an ALab build");
        assert_eq!(decode_vendor_build(2_026_102_001), None, "an ALab build");
        assert_eq!(decode_vendor_build(3 * VENDOR_BASE), None);
        assert_eq!(decode_vendor_build(u64::MAX), None);
    }

    #[test]
    fn build_names_are_plain_decimal() {
        assert_eq!(
            parse_build_name("1000002000001000310"),
            Some(1_000_002_000_001_000_310)
        );
        assert_eq!(parse_build_name("0"), Some(0));
        for bad in ["", "01", "12a", "-1", "1.5", "99999999999999999999999"] {
            assert_eq!(parse_build_name(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn ready_needs_ok_first_and_this_platform() {
        let here = "aarch64-macos";
        assert!(ready_text_accepts(
            "ok\nplatform=aarch64-macos\ncontents=bin:claude\n",
            here
        ));
        assert!(ready_text_accepts("ok\n", here));
        assert!(!ready_text_accepts("ok\nplatform=x86_64-linux\n", here));
        assert!(!ready_text_accepts(
            "pending\nplatform=aarch64-macos\n",
            here
        ));
        assert!(!ready_text_accepts("", here));
    }

    #[test]
    fn vendor_version_reads_the_provenance_line() {
        let text =
            "schema = 1\nprogram = \"claude\"\nversion = \"2.1.310\"\nvendor = \"Anthropic\"\n";
        assert_eq!(vendor_version(text).as_deref(), Some("2.1.310"));
        assert_eq!(vendor_version("version = \"\"\n"), None);
        assert_eq!(vendor_version("version = 2\n"), None);
        assert_eq!(vendor_version("versions = \"1\"\n"), None);
    }

    #[test]
    fn shim_env_follows_atpkgs_rule_and_refuses_whole() {
        assert_eq!(
            parse_shim_env("DISABLE_AUTOUPDATER=1\nCLAUDE_CODE_DISABLE_FEEDBACK_SURVEY=1\n"),
            [
                ("DISABLE_AUTOUPDATER".to_string(), "1".to_string()),
                (
                    "CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY".to_string(),
                    "1".to_string()
                )
            ]
        );
        for bad in [
            "DISABLE_AUTOUPDATER=1\nPATH=/tmp\n",
            "DYLD_INSERT_LIBRARIES=/x.dylib\n",
            "lower=1\n",
            "1X=1\n",
            "A=\n",
            "A=1\nA=2\n",
            "A=has\"quote\n",
            "A=100%\n",
            "NODE_OPTIONS=--require /x.js\n",
        ] {
            assert!(parse_shim_env(bad).is_empty(), "{bad:?} must be refused");
        }
        let nine: String = (0..9).map(|i| format!("V{i}=1\n")).collect();
        assert!(parse_shim_env(&nine).is_empty());
    }

    #[test]
    fn small_files_are_read_from_one_handle_and_bounded() {
        let root = std::env::temp_dir().join(format!(
            "claude-alab-read-small-{}-{}",
            std::process::id(),
            line!()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("mkdir");
        let file = root.join("2.vendor");
        std::fs::write(&file, "version = \"1\"\n").expect("write");
        assert_eq!(read_small(&file).as_deref(), Some("version = \"1\"\n"));
        // A symlink is refused at the open, even to a good file.
        let link = root.join("3.vendor");
        std::os::unix::fs::symlink(&file, &link).expect("symlink");
        assert_eq!(read_small(&link), None, "symlink");
        assert_eq!(
            read_bounded(&link, 64, true).as_deref(),
            Some("version = \"1\"\n"),
            "followed when asked"
        );
        // Over the bound, a directory, a missing file: refused.
        let big = root.join("4.vendor");
        std::fs::write(&big, vec![b'a'; 64 * 1024 + 1]).expect("write");
        assert_eq!(read_small(&big), None, "too big");
        assert_eq!(read_bounded(&root, 64, true), None, "directory");
        assert_eq!(read_small(&root.join("5.vendor")), None, "missing");
        // A FIFO is refused without waiting for a writer.
        let fifo = root.join("6.vendor");
        let c_fifo = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).expect("cstr");
        // SAFETY: `c_fifo` is a valid NUL-terminated path in our private test directory.
        assert_eq!(unsafe { libc::mkfifo(c_fifo.as_ptr(), 0o600) }, 0);
        assert_eq!(read_small(&fifo), None, "fifo");
        assert_eq!(read_bounded(&fifo, 64, true), None, "fifo followed");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_build_that_cannot_be_compared_with_the_harness_is_refused() {
        use std::os::unix::fs::PermissionsExt as _;
        let root = std::env::temp_dir().join(format!(
            "claude-alab-uncompared-{}-{}",
            std::process::id(),
            line!()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let store = Store::new(&root, "claude", "claude");
        let exe = store.program_dir.join("7/bin/claude");
        std::fs::create_dir_all(exe.parent().expect("bin")).expect("mkdir");
        std::fs::write(&exe, "vendor\n").expect("vendor");
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        std::fs::write(store.program_dir.join("7.ready"), "ok\n").expect("ready");
        std::os::unix::fs::symlink(store.program_dir.join("7"), store.current_link())
            .expect("current");
        let own = root.join("claude-alab");
        std::fs::write(&own, "harness\n").expect("own");

        // Another file than the harness: usable.
        assert_eq!(store.current(Some(&own)).map(|b| b.number), Ok(7));
        // The harness itself, by its path and through a link to it.
        assert_eq!(store.current(Some(&exe)), Err(Missing::IsSelf(exe.clone())));
        let link = root.join("link");
        std::os::unix::fs::symlink(&exe, &link).expect("link");
        assert_eq!(
            store.current(Some(&link)),
            Err(Missing::IsSelf(exe.clone()))
        );
        // The harness does not know its own executable.
        let unknown = store
            .current(None)
            .expect_err("refused with no own executable");
        assert!(
            matches!(&unknown, Missing::Uncompared(path, _) if *path == exe),
            "{unknown:?}"
        );
        assert_eq!(unknown.word(), "vendor-uncompared");
        assert!(
            unknown
                .to_string()
                .contains("cannot check that it is not this harness itself"),
            "{unknown}"
        );
        // Its own executable cannot be read for the comparison.
        let gone = root.join("gone");
        let unread = store
            .current(Some(&gone))
            .expect_err("refused when unreadable");
        assert!(
            matches!(&unread, Missing::Uncompared(path, why)
                if *path == exe && why.contains(&gone.display().to_string())),
            "{unread:?}"
        );
        assert!(same_file(&exe, &gone).is_err() && same_file(&gone, &exe).is_err());
        assert_eq!(same_file(&exe, &link).ok(), Some(true));
        assert_eq!(same_file(&exe, &own).ok(), Some(false));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn home_prefers_an_absolute_env_home() {
        assert_eq!(
            home_dir(Some(OsString::from("/h"))),
            Some(PathBuf::from("/h"))
        );
    }

    #[test]
    fn a_relative_env_home_is_refused() {
        assert_eq!(home_dir(Some(OsString::from("h/u"))), None);
    }
}
