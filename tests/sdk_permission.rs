// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Actual owned-pipe SDK controls and a developer-only real-vendor qualifier.
//! This binary supplies no interactive TUI permission or restart authority.

#[cfg(target_os = "macos")]
use claude_code_alab::sys;
use claude_code_alab::{
    json::{self, Value},
    permission::Mode,
    sdk_permission::{MAX_OUTPUT, Session, WITHIN},
};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead as _, Read as _, Write as _};
use std::os::unix::fs::OpenOptionsExt as _;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn require(ok: bool, why: &str) -> io::Result<()> {
    if ok {
        Ok(())
    } else {
        Err(io::Error::other(why))
    }
}

fn root() -> io::Result<PathBuf> {
    let mut random = [0; 8];
    File::open("/dev/urandom")?.read_exact(&mut random)?;
    let path = PathBuf::from(format!(
        "/tmp/.ubd/sdk-permission-{}-{:016x}.noindex",
        std::process::id(),
        u64::from_ne_bytes(random)
    ));
    fs::create_dir_all("/tmp/.ubd")?;
    fs::create_dir(&path)?;
    Ok(path)
}

fn start(vendor: &Path, records: &Path, until: Instant) -> io::Result<Session> {
    match Session::start_until(vendor, records, until) {
        Ok(session) => Ok(session),
        Err(mut failure) => {
            let message = failure.to_string();
            if !failure.settle() {
                eprintln!("SDK start retains unsettled custody: {failure:?}");
                std::mem::forget(failure);
                std::process::exit(70);
            }
            Err(io::Error::other(message))
        }
    }
}

fn settled(session: &mut Session) -> io::Result<()> {
    if session.cancel() {
        return Ok(());
    }
    // Only this executable boundary may retain an unsettled native child/mask
    // through terminal exit. The library never terminates its embedding process.
    eprintln!("SDK qualifier retains unsettled child custody; no success recorded");
    std::process::exit(70);
}

fn handshake(expected: &[u8]) -> io::Result<()> {
    let until = Instant::now() + WITHIN;
    let mut got = Vec::new();
    while got.len() < expected.len() {
        let left = until
            .checked_duration_since(Instant::now())
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::TimedOut, "qualifier handshake deadline")
            })?;
        let mut poll = [libc::pollfd {
            fd: 0,
            events: libc::POLLIN,
            revents: 0,
        }];
        // SAFETY: live stdin and one bounded poll record; absolute deadline is
        // checked again after every interrupt and before accepting the handshake.
        let ready = unsafe {
            libc::poll(
                poll.as_mut_ptr(),
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
            continue;
        }
        let mut byte = [0];
        // SAFETY: poll established readiness on this owned stdin. Read without
        // a Rust buffer, whose unread tail would be invisible to the next poll.
        let count = unsafe { libc::read(0, byte.as_mut_ptr().cast(), 1) };
        if count < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted
                || error.kind() == io::ErrorKind::WouldBlock
            {
                continue;
            }
            return Err(error);
        }
        if count == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "qualifier handshake input ended",
            ));
        }
        got.push(byte[0]);
        require(expected.starts_with(&got), "qualifier handshake mismatch")?;
    }
    require(Instant::now() < until, "qualifier handshake expired")
}

fn probe(args: &[String]) -> io::Result<()> {
    let [vendor_key, vendor, root_key, root] = args else {
        return Err(io::Error::other("--probe --vendor ABS --root ABS"));
    };
    require(
        vendor_key == "--vendor"
            && root_key == "--root"
            && Path::new(vendor).is_absolute()
            && Path::new(root).is_absolute(),
        "absolute qualifier inputs required",
    )?;
    println!("alab-qualifier-ready-v1");
    io::stdout().flush()?;
    handshake(b"GO\n")?;
    let mut session = start(Path::new(vendor), Path::new(root), Instant::now() + WITHIN)?;
    let result: io::Result<()> = (|| {
        let observed = session.qualify()?;
        require(
            observed.len() == 7
                && observed
                    .iter()
                    .all(|value| value.source() == observed[0].source()),
            "all seven SDK observations must retain the original native image",
        )?;
        session.finish()?;
        let summary = format!(
            "{{\"ack_count\":7,\"headless_sdk_only\":true,\"tui_qualified\":false,\"full_lead\":false,\"native\":\"{}\"}}\n",
            observed[0].source().diagnostic()
        );
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(Path::new(root).join("sdk-observations.json"))?
            .write_all(summary.as_bytes())?;
        Ok(())
    })();
    if result.is_err() {
        settled(&mut session)?;
    }
    result?;
    println!("alab-qualifier-complete-v1");
    io::stdout().flush()?;
    handshake(b"RELEASE\n")
}

fn emit(id: &str, body: &str) -> io::Result<String> {
    let line = format!(
        "{{\"type\":\"control_response\",\"response\":{{\"request_id\":\"{id}\",\"subtype\":\"success\",\"response\":{body}}}}}\n"
    );
    io::stdout().write_all(line.as_bytes())?;
    io::stdout().flush()?;
    Ok(line)
}

fn fake() -> io::Result<()> {
    let case = std::env::var("ALAB_SDK_FAKE_CASE").unwrap_or_else(|_| "good".into());
    if case == "reexec"
        && let Ok(id) = std::env::var("ALAB_SDK_PENDING_ID")
    {
        emit(&id, r#"{"current_permission_mode":"default"}"#)?;
        // Keep the same new image alive for the client's rejecting native read.
        std::thread::sleep(Duration::from_secs(2));
        return Ok(());
    }
    if case == "preexisting" {
        emit("unsolicited", r#"{"current_permission_mode":"default"}"#)?;
    }
    if case == "stderr-eof" {
        // SAFETY: the isolated fake SDK owns stderr and deliberately proves EOF.
        unsafe {
            libc::close(2);
        }
    }
    let mut mode = "default".to_owned();
    for line in io::stdin().lock().lines() {
        let line = line?;
        if let Some(path) = std::env::var_os("ALAB_SDK_INPUT_LOG") {
            OpenOptions::new()
                .create(true)
                .append(true)
                .mode(0o600)
                .open(path)?
                .write_all(format!("{line}\n").as_bytes())?;
        }
        let value =
            json::parse(&line).ok_or_else(|| io::Error::other("fake received invalid JSON"))?;
        let id = value
            .get("request_id")
            .and_then(Value::as_str)
            .ok_or_else(|| io::Error::other("missing id"))?;
        let request = value
            .get("request")
            .ok_or_else(|| io::Error::other("missing request"))?;
        if case == "reexec" {
            let error = std::process::Command::new(std::env::current_exe()?)
                .arg("--print")
                .env("ALAB_SDK_PENDING_ID", id)
                .exec();
            return Err(error);
        }
        if case == "late" {
            std::thread::sleep(Duration::from_millis(150));
        }
        if case == "flood" {
            io::stdout().write_all(&vec![b'x'; MAX_OUTPUT / 2])?;
            io::stderr().write_all(&vec![b'x'; MAX_OUTPUT / 2 + 1])?;
            io::stderr().flush()?;
            continue;
        }
        if case == "truncated" {
            io::stdout().write_all(b"{\"type\":")?;
            return Ok(());
        }
        if case == "passive" {
            println!("{{\"type\":\"system\",\"permissionMode\":\"auto\",\"timestamp\":\"2099\"}}");
            return Ok(());
        }
        if case == "stderr-eof" {
            std::thread::sleep(Duration::from_millis(20));
        }
        let id = if case == "wrong-id" { "other" } else { id };
        let subtype = request.get("subtype").and_then(Value::as_str).unwrap_or("");
        if subtype == "set_permission_mode" {
            let wanted = request.get("mode").and_then(Value::as_str).unwrap_or("");
            if Mode::of_setting(wanted).is_none() {
                println!(
                    "{{\"type\":\"control_response\",\"response\":{{\"request_id\":\"{id}\",\"subtype\":\"error\",\"error\":\"invalid mode\",\"error_code\":\"invalid_mode\"}}}}"
                );
                io::stdout().flush()?;
                continue;
            }
            mode = wanted.to_owned();
            emit(id, &format!("{{\"mode\":\"{mode}\"}}"))?;
        } else {
            let body = if case == "unknown-mode" {
                r#"{"current_permission_mode":"future"}"#.to_owned()
            } else {
                format!("{{\"current_permission_mode\":\"{mode}\"}}")
            };
            if case == "duplicate" {
                let ack = format!(
                    "{{\"type\":\"control_response\",\"response\":{{\"request_id\":\"{id}\",\"subtype\":\"success\",\"response\":{body}}}}}\n"
                );
                io::stdout().write_all(format!("{ack}{ack}").as_bytes())?;
                io::stdout().flush()?;
            } else {
                emit(id, &body)?;
            }
        }
    }
    Ok(())
}

fn controls() -> io::Result<()> {
    let vendor = std::env::current_exe()?;
    let root = root()?;
    #[cfg(not(target_os = "macos"))]
    {
        let mut refusal = Session::start(&vendor, &root)
            .err()
            .ok_or_else(|| io::Error::other("unknown exact exec must refuse"))?;
        require(
            refusal.settle(),
            "unsupported-platform start must settle without a child",
        )?;
        println!("PASS unavailable exact-exec refuses; native SDK cases not run on this platform");
        Ok(())
    }
    #[cfg(target_os = "macos")]
    {
        let refused_records = root.join("refused-start");
        fs::create_dir(&refused_records)?;
        let denied = root.join("not-executable");
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&denied)?
            .write_all(b"not an executable\n")?;
        let mut refusal = Session::start(&denied, &refused_records)
            .err()
            .ok_or_else(|| io::Error::other("nonexecutable SDK must retain a refused start"))?;
        require(
            refusal.settle(),
            "boxed refused SDK child must positively settle",
        )?;
        require(
            fs::read(refused_records.join("requests.jsonl"))?.is_empty(),
            "refused source must receive no SDK request",
        )?;
        require(
            String::from_utf8_lossy(&fs::read(refused_records.join("stderr.log"))?)
                .contains("probe exec failed"),
            "refused startup output remains retained",
        )?;
        require(
            claude_code_alab::procs::own_children() == Some(Vec::new()),
            "refused SDK child leaves no unreaped ownership",
        )?;
        for case in [
            "good",
            "wrong-id",
            "duplicate",
            "preexisting",
            "truncated",
            "passive",
            "unknown-mode",
            "reexec",
            "late",
            "flood",
            "stderr-eof",
        ] {
            let records = root.join(case);
            fs::create_dir(&records)?;
            sys::ensure_single_threaded()?;
            // SAFETY: this isolated test has one native thread and no installed
            // environment-reading handlers; the fork receives these explicit cases.
            unsafe {
                std::env::set_var("ALAB_SDK_FAKE_CASE", case);
                std::env::set_var("ALAB_SDK_INPUT_LOG", records.join("vendor-input.jsonl"));
                std::env::remove_var("ALAB_SDK_PENDING_ID");
            }
            let until = Instant::now()
                + if case == "late" {
                    Duration::from_millis(100)
                } else {
                    WITHIN
                };
            let mut session = start(&vendor, &records, until)?;
            let good = case == "good" || case == "stderr-eof";
            let observed = session.qualify();
            if good {
                require(observed?.len() == 7, "positive SDK seven")?;
                session.finish()?;
            } else {
                require(
                    observed.is_err(),
                    &format!("{case} must not supply a SDK observation sequence"),
                )?;
                let failure = observed.unwrap_err();
                fs::write(records.join("refusal.txt"), failure.to_string())?;
                if case == "reexec" {
                    require(
                        failure
                            .to_string()
                            .contains("native executable or lifetime changed"),
                        "same-image exec must fail its native generation witness",
                    )?;
                }
                if case == "late" {
                    require(
                        failure.kind() == io::ErrorKind::TimedOut,
                        "late ACK must fail the monotonic deadline",
                    )?;
                }
                let before = fs::read(records.join("requests.jsonl"))?;
                if case == "duplicate" {
                    require(
                        before.iter().filter(|byte| **byte == b'\n').count() == 1,
                        "same-read duplicate must refuse before a second request",
                    )?;
                }
                require(
                    session.current_mode().is_err(),
                    "uncertain SDK operation cannot be retried",
                )?;
                require(
                    fs::read(records.join("requests.jsonl"))? == before,
                    "poisoned SDK must not send another request",
                )?;
                settled(&mut session)?;
            }
            let retained = fs::metadata(records.join("stdout.jsonl"))?.len()
                + fs::metadata(records.join("stderr.log"))?.len();
            require(
                retained <= MAX_OUTPUT as u64,
                "aggregate SDK retained output bound",
            )?;
        }
        println!(
            "PASS 12 owned SDK transport cases; evidence {}",
            root.display()
        );
        Ok(())
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("--print") => fake(),
        Some("--probe") => probe(&args[1..]),
        _ => controls(),
    };
    if let Err(error) = result {
        eprintln!("SDK observation qualification refused: {error}");
        std::process::exit(1);
    }
}
