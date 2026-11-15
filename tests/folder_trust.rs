// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Explicit jailed real-vendor qualifier. No production policy or input authority.

use claude_code_alab::{folder_trust, json, store, sys};
use std::fs::{self, OpenOptions};
use std::io::{self, Write as _};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const ROOT: &str = "ALAB_FOLDER_QUAL_ROOT";
const VENDOR: &str = "ALAB_FOLDER_QUAL_VENDOR";

fn hex(path: &Path) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    for byte in path.as_os_str().as_bytes() {
        write!(out, "{byte:02x}").unwrap();
    }
    out
}

fn gate(args: &[String]) -> io::Result<()> {
    let [
        harness,
        trust,
        gate,
        cwd_flag,
        cwd,
        vendor_flag,
        vendor,
        session_flag,
        session,
        tty_flag,
        tty,
    ] = args
    else {
        return Err(io::Error::other("closed qualifier gate arguments"));
    };
    let root =
        PathBuf::from(std::env::var_os(ROOT).ok_or_else(|| io::Error::other("private root"))?);
    let selected =
        PathBuf::from(std::env::var_os(VENDOR).ok_or_else(|| io::Error::other("private vendor"))?);
    if harness != "harness"
        || trust != "launch-trust"
        || gate != "gate"
        || cwd_flag != "--cwd-hex"
        || vendor_flag != "--vendor-hex"
        || session_flag != "--session"
        || tty_flag != "--stdout-tty"
        || tty != "1"
        || *cwd != hex(&root.join("work"))
        || *vendor != hex(&selected)
        || session != "t0000000000"
    {
        return Err(io::Error::other(
            "gate is restricted to its exact private experiment",
        ));
    }
    println!(
        "{{\"schema\":1,\"operation\":\"folder-trust-gate\",\"status\":\"allow\",\"cwd_hex\":\"{cwd}\",\"vendor_hex\":\"{vendor}\",\"session\":\"{session}\",\"stdout_tty\":true,\"rule\":\"private-qualification-folder\",\"reason\":null}}"
    );
    Ok(())
}

fn handshake(expected: &[u8]) -> io::Result<()> {
    let until = Instant::now() + Duration::from_secs(60);
    let mut got = Vec::new();
    while got.len() < expected.len() {
        let left = until
            .checked_duration_since(Instant::now())
            .ok_or_else(|| io::Error::from(io::ErrorKind::TimedOut))?;
        let mut fd = libc::pollfd {
            fd: 0,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: a live stdin descriptor and one bounded native poll output record.
        let ready = unsafe {
            libc::poll(
                &raw mut fd,
                1,
                left.as_millis().min(i32::MAX as u128) as i32,
            )
        };
        if ready < 0 {
            if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(io::Error::last_os_error());
        }
        if ready == 0 {
            return Err(io::Error::from(io::ErrorKind::TimedOut));
        }
        let mut byte = [0];
        // Keep the readiness check and read on the same unbuffered descriptor.
        // Stdin's internal read-ahead could otherwise retain the rest of GO or
        // RELEASE while the next native poll waits on an already empty pipe.
        // SAFETY: byte is writable one-byte storage; stdin remains caller-owned.
        let read = unsafe { libc::read(0, byte.as_mut_ptr().cast(), byte.len()) };
        if read < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        if read != 1 {
            return Err(io::Error::from(io::ErrorKind::UnexpectedEof));
        }
        got.push(byte[0]);
        if !expected.starts_with(&got) {
            return Err(io::Error::other("private handshake mismatch"));
        }
    }
    if Instant::now() >= until {
        return Err(io::Error::from(io::ErrorKind::TimedOut));
    }
    Ok(())
}

fn write(root: &Path, name: &str, bytes: &[u8]) -> io::Result<()> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(root.join(name))?
        .write_all(bytes)
}

fn probe(args: &[String]) -> io::Result<()> {
    let [vendor_key, vendor, root_key, root] = args else {
        return Err(io::Error::other("--probe --vendor ABS --root ABS"));
    };
    if vendor_key != "--vendor"
        || root_key != "--root"
        || !Path::new(vendor).is_absolute()
        || !Path::new(root).is_absolute()
    {
        return Err(io::Error::other("absolute exact qualifier inputs required"));
    }
    let vendor = Path::new(vendor).canonicalize()?;
    let root = Path::new(root).canonicalize()?;
    println!("alab-qualifier-ready-v1");
    io::stdout().flush()?;
    handshake(b"GO\n")?;
    sys::ensure_single_threaded()?;
    let home = root.join("home");
    let config = root.join("config");
    let work = root.join("work");
    let records = root.join("exchange");
    for dir in [&home, &config, &work, &records, &work.join(".claude")] {
        fs::create_dir(dir)?;
    }
    let hook = root.join("hook-ran");
    let mcp = root.join("mcp-ran");
    // These private configuration commands are deliberately present. The actual
    // owner's quarantine must keep them from executing; no script file is created.
    write(&work.join(".claude"),"settings.json",format!("{{\"hooks\":{{\"SessionStart\":[{{\"hooks\":[{{\"type\":\"command\",\"command\":\"/usr/bin/touch {}\"}}]}}]}}}}",hook.display()).as_bytes())?;
    write(&work,".mcp.json",format!("{{\"mcpServers\":{{\"must-not-start\":{{\"command\":\"/usr/bin/touch\",\"args\":[\"{}\"]}}}}}}",mcp.display()).as_bytes())?;
    // SAFETY: this explicit test executable just proved it has one native thread;
    // all process environment choices precede every child and fork.
    unsafe {
        std::env::set_var("HOME", &home);
        std::env::set_var("CLAUDE_CONFIG_DIR", &config);
        std::env::set_var("XDG_STATE_HOME", root.join("state"));
        std::env::set_var("XDG_CACHE_HOME", root.join("cache"));
        std::env::set_var("ATERM_HARNESS_HOST", std::env::current_exe()?);
        std::env::set_var("ATERM_PARENT_SESSION_ID", "t0000000000");
        std::env::set_var(ROOT, &root);
        std::env::set_var(VENDOR, &vendor);
        std::env::remove_var("ATERM_NO_HARNESS");
        std::env::remove_var("CLAUDE_CODE_CUSTOM_OAUTH_URL");
    }
    std::env::set_current_dir(&work)?;
    let build = store::Build {
        number: 0,
        dir: vendor.parent().unwrap().into(),
        exe: vendor,
        version: None,
        env: Vec::new(),
    };
    let result = match folder_trust::settle_recorded(&build, &[], true, &records) {
        Ok(result) => result,
        Err(retained) => {
            let retained = std::mem::ManuallyDrop::new(retained);
            eprintln!(
                "folder trust qualifier retains native custody: {}",
                retained.error
            );
            std::process::exit(70);
        }
    };
    if !matches!(result, folder_trust::Outcome::Recorded { .. }) {
        return Err(io::Error::other(result.description()));
    }
    if hook.exists() || mcp.exists() {
        return Err(io::Error::other("quarantined project command executed"));
    }
    let profile = fs::read(config.join(".claude.json"))?;
    write(&root, "profile-after.json", &profile)?;
    let parsed = json::parse(std::str::from_utf8(&profile).map_err(io::Error::other)?)
        .ok_or_else(|| io::Error::other("profile is not strict JSON"))?;
    if parsed
        .get("projects")
        .and_then(|projects| projects.get(work.to_str().unwrap()))
        .and_then(|entry| entry.get("hasTrustDialogAccepted"))
        .and_then(json::Value::as_bool)
        != Some(true)
    {
        return Err(io::Error::other(
            "the exact private directory lacks recorded trust",
        ));
    }
    write(&root,"folder-trust-observation.json",b"{\"schema\":1,\"cold_folder_recorded\":true,\"native_cohort_settled\":true,\"project_hooks_ran\":false,\"project_mcp_ran\":false,\"tui_authority\":false,\"full_lead\":false}\n")?;
    println!("alab-qualifier-complete-v1");
    io::stdout().flush()?;
    handshake(b"RELEASE\n")
}

fn main() {
    let args = std::env::args_os()
        .skip(1)
        .map(|arg| {
            arg.into_string()
                .map_err(|_| io::Error::other("native argument is not UTF-8"))
        })
        .collect::<io::Result<Vec<_>>>();
    let result = args.and_then(|args| match args.first().map(String::as_str) {
        Some("harness") => gate(&args),
        Some("--probe") => probe(&args[1..]),
        None => {
            println!("real-vendor folder-trust qualification requires explicit --probe inputs");
            Ok(())
        }
        _ => Err(io::Error::other("unknown explicit qualifier role")),
    });
    if let Err(error) = result {
        eprintln!("folder-trust qualifier failed: {error}");
        std::process::exit(1)
    }
}
