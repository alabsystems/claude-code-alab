// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Original OS child and existing socket fixture; unavailable reads grant nothing.

use std::io::{self, Read as _, Write as _};
use std::os::fd::{FromRawFd as _, OwnedFd, RawFd};
use std::os::unix::fs::DirBuilderExt as _;
use std::os::unix::process::ExitStatusExt as _;
use std::process::{Child as OsChild, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

use super::*;
use crate::test_aterm::{FakeAterm, INFO_ROSTERED};

fn root() -> std::path::PathBuf {
    static SERIAL: AtomicU64 = AtomicU64::new(0);
    let parent = std::env::temp_dir().join(".ubd");
    std::fs::create_dir_all(&parent).unwrap();
    let root = parent.join(format!(
        "claude-link-native-{}-{}-{}.noindex",
        std::process::id(),
        super::super::now_ms(),
        SERIAL.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&root)
        .unwrap();
    root
}

struct NativeChild {
    child: OwnedOsChild,
    input: Option<ChildStdin>,
    exit: OwnedFd,
    proof: aterm::ChildProof,
    identity: crate::procs::ChildIdentity,
    deadline: Instant,
}

struct OwnedOsChild(Option<OsChild>);

impl Drop for OwnedOsChild {
    fn drop(&mut self) {
        if let Some(child) = self.0.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl NativeChild {
    fn spawn() -> io::Result<Self> {
        let deadline = Instant::now() + Duration::from_secs(60);
        let mut owned = OwnedOsChild(Some(
            Command::new("/bin/cat")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()?,
        ));
        let child = owned.0.as_mut().unwrap();
        let prepared = (|| {
            let exit = exit_watch(child.id())?;
            let mut input = child.stdin.take().unwrap();
            let mut output = child.stdout.take().unwrap();
            // SAFETY: the descriptor is this test's pipe read end, not inherited terminal flags.
            let flags = unsafe { libc::fcntl(output.as_raw_fd(), libc::F_GETFL) };
            if flags < 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: the same owned pipe read end and its original flags.
            if unsafe { libc::fcntl(output.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) }
                < 0
            {
                return Err(io::Error::last_os_error());
            }
            let marker = b"CLAUDE-LINK-OWNED-BODY\n";
            input.write_all(marker)?;
            let mut echoed = vec![0; marker.len()];
            let mut offset = 0;
            while offset < echoed.len() {
                wait_readable(output.as_raw_fd(), deadline)?;
                match output.read(&mut echoed[offset..]) {
                    Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
                    Ok(n) => offset += n,
                    Err(error)
                        if matches!(
                            error.kind(),
                            io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                        ) => {}
                    Err(error) => return Err(error),
                }
            }
            assert_eq!(echoed, marker);
            let identity = crate::procs::capture_child(child.id())
                .expect("native original child after body proof");
            let proof = aterm::ChildProof::from_identity(&identity).unwrap();
            Ok((input, exit, identity, proof))
        })();
        let (input, exit, identity, proof) = prepared?;
        Ok(Self {
            child: owned,
            input: Some(input),
            exit,
            proof,
            identity,
            deadline,
        })
    }

    fn finish(mut self) -> io::Result<()> {
        drop(self.input.take());
        wait_readable(self.exit.as_raw_fd(), self.deadline)?;
        let status = self.child.0.as_mut().unwrap().wait()?;
        self.child.0 = None;
        assert!(status.success());
        Ok(())
    }

    fn end_abnormally(mut self) -> io::Result<crate::sys::ChildEvent> {
        self.child.0.as_mut().unwrap().kill()?;
        wait_readable(self.exit.as_raw_fd(), self.deadline)?;
        let status = self.child.0.as_mut().unwrap().wait()?;
        self.child.0 = None;
        let signal = status
            .signal()
            .expect("the original Child handle sent SIGKILL");
        assert_eq!(signal, libc::SIGKILL);
        Ok(crate::sys::ChildEvent::Signaled(signal))
    }
}

fn wait_readable(fd: RawFd, deadline: Instant) -> io::Result<()> {
    loop {
        let left = deadline
            .checked_duration_since(Instant::now())
            .ok_or(io::ErrorKind::TimedOut)?;
        let mut event = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let millis = i32::try_from(left.as_millis().saturating_add(1)).unwrap_or(i32::MAX);
        // SAFETY: one live poll slot and a bounded monotonic remainder.
        let count = unsafe { libc::poll(&mut event, 1, millis) };
        if count < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        if count == 0 {
            return Err(io::ErrorKind::TimedOut.into());
        }
        if event.revents & (libc::POLLERR | libc::POLLNVAL) != 0 {
            return Err(io::Error::other("native fixture descriptor failed"));
        }
        if event.revents & (libc::POLLIN | libc::POLLHUP) != 0 {
            return Ok(());
        }
        return Err(io::Error::other("unexpected native fixture readiness"));
    }
}

#[cfg(target_os = "macos")]
fn exit_watch(pid: u32) -> io::Result<OwnedFd> {
    // SAFETY: kqueue takes no arguments and returns a new owned descriptor or an error.
    let raw = unsafe { libc::kqueue() };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the successful call returned a fresh descriptor with exactly one owner.
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    // SAFETY: set only the new watch's descriptor flags.
    if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
        return Err(io::Error::last_os_error());
    }
    let change = libc::kevent {
        ident: pid as libc::uintptr_t,
        filter: libc::EVFILT_PROC,
        flags: libc::EV_ADD | libc::EV_ENABLE | libc::EV_ONESHOT,
        fflags: libc::NOTE_EXIT,
        data: 0,
        udata: std::ptr::null_mut(),
    };
    // SAFETY: one complete change; the original owned cat is blocked on its open stdin.
    if unsafe {
        libc::kevent(
            fd.as_raw_fd(),
            &change,
            1,
            std::ptr::null_mut(),
            0,
            std::ptr::null(),
        )
    } < 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(fd)
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn exit_watch(pid: u32) -> io::Result<OwnedFd> {
    // SAFETY: pidfd_open reads only this original unreaped child's pid and zero flags.
    let raw = unsafe { libc::syscall(libc::SYS_pidfd_open, libc::c_long::from(pid), 0u32) };
    let raw = i32::try_from(raw).map_err(io::Error::other)?;
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the successful syscall returned a fresh close-on-exec descriptor.
    Ok(unsafe { OwnedFd::from_raw_fd(raw) })
}

#[test]
fn a_known_proof_only_child_survives_an_unavailable_reconnect_read() -> io::Result<()> {
    let root = root();
    let fake = FakeAterm::new(&root);
    fake.set_info(&format!("{INFO_ROSTERED} child-proof=1"));
    let child = NativeChild::spawn()?;
    let claimed = claim(
        &fake.locator(),
        std::process::id(),
        VERSION,
        None,
        Some(child.proof),
        None,
        CONNECT_TIMEOUT,
    )
    .unwrap();
    assert!(claimed.child_proof && !claimed.child_start);
    let mut supervisor = super::super::tests::supervisor_of_an_unsignallable_child(&root);
    supervisor.child.pid = i32::try_from(child.identity.pid).unwrap();
    supervisor.child.identity = Some(child.identity.clone());
    supervisor.link = Link::Held(claimed);
    supervisor.link_lost("controlled request loss");
    let before = fake.verbs().len();
    let due = now_ms().saturating_add(RECLAIM_EVERY_MS);
    supervisor.link_tick_with(due, |_| None);
    assert!(
        supervisor.link.lost(),
        "unknown observations cannot forget a known native lifetime"
    );
    assert!(
        !fake
            .verbs()
            .iter()
            .skip(before)
            .any(|verb| verb.starts_with("meta set supervisor "))
    );
    supervisor.link_tick_with(due, live_child_proof);
    assert!(matches!(&supervisor.link, Link::Held(claimed) if claimed.child == Some(child.proof)));
    supervisor.link_release();
    child.finish()
}

#[test]
fn a_consumed_original_child_end_allows_reclaim_without_a_stale_native_read() -> io::Result<()> {
    let root = root();
    let fake = FakeAterm::new(&root);
    fake.set_info(&format!("{INFO_ROSTERED} child-proof=1"));
    let child = NativeChild::spawn()?;
    let claimed = claim(
        &fake.locator(),
        std::process::id(),
        VERSION,
        None,
        Some(child.proof),
        None,
        CONNECT_TIMEOUT,
    )
    .unwrap();
    let mut supervisor = super::super::tests::supervisor_of_an_unsignallable_child(&root);
    supervisor.child.pid = i32::try_from(child.identity.pid).unwrap();
    supervisor.child.identity = Some(child.identity.clone());
    supervisor.link = Link::Held(claimed);
    supervisor.link_lost("controlled request loss before owned exit");
    let ended = child.end_abnormally()?;
    supervisor.ending = super::super::tests::crashed(ended);
    let due = now_ms().saturating_add(RECLAIM_EVERY_MS);
    supervisor.link_tick_with(due, |_| panic!("a reaped pid must not be queried"));
    assert!(matches!(&supervisor.link, Link::Held(claimed) if claimed.child.is_none()));
    assert!(
        fake.status_line()
            .split_ascii_whitespace()
            .any(|word| word == "supervisor_child=-")
    );
    supervisor.link_release();
    Ok(())
}

#[test]
fn a_new_owned_attempt_retires_old_child_knowledge_before_a_different_child_reclaims()
-> io::Result<()> {
    let root = root();
    let fake = FakeAterm::new(&root);
    fake.set_info(&format!("{INFO_ROSTERED} child-proof=1"));
    let first = NativeChild::spawn()?;
    let claimed = claim(
        &fake.locator(),
        std::process::id(),
        VERSION,
        None,
        Some(first.proof),
        None,
        CONNECT_TIMEOUT,
    )
    .unwrap();
    let mut supervisor = super::super::tests::supervisor_of_an_unsignallable_child(&root);
    supervisor.child.pid = i32::try_from(first.identity.pid).unwrap();
    supervisor.child.identity = Some(first.identity.clone());
    supervisor.link = Link::Held(claimed);
    supervisor.link_lost("controlled request loss before new owned attempt");
    first.finish()?;
    supervisor.link.before_spawn(&supervisor.journal, None);
    let second = NativeChild::spawn()?;
    supervisor.child.pid = i32::try_from(second.identity.pid).unwrap();
    supervisor.child.identity = Some(second.identity.clone());
    let due = now_ms().saturating_add(RECLAIM_EVERY_MS);
    supervisor.link_tick_with(due, live_child_proof);
    assert!(matches!(&supervisor.link, Link::Held(claimed)
        if claimed.child == Some(second.proof) && claimed.generation.is_none()));
    supervisor.link_release();
    second.finish()
}
