// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::{Generation, Outer, PtyPair, Slave, invalid, owned, tty_identity};
use std::collections::VecDeque;
use std::io;
use std::os::fd::{AsRawFd, BorrowedFd, RawFd};
use std::time::{Duration, Instant};

/// Hard payload/chunk limits; caller-owned origin storage is additional.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Maximum queued input payload bytes.
    pub input_bytes: usize,
    /// Maximum queued input origins/chunks.
    pub input_chunks: usize,
    /// Maximum queued output bytes.
    pub output_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            input_bytes: 65_536,
            input_chunks: 128,
            output_bytes: 65_536,
        }
    }
}

/// The caller's current decision about the exact saved origin.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Delivery {
    /// Permit this one write attempt. The next partial write is checked again.
    Allow,
    /// Keep the original packet and wait for caller-visible authority progress.
    Hold,
    /// Permanently retire this packet; never rebind it to a different origin.
    Refuse,
}

/// A packet returned intact when enqueue could not accept it.
#[derive(Debug)]
pub struct InputRefusal<O> {
    /// The original caller-owned origin.
    pub origin: O,
    /// The original payload, not partially enqueued.
    pub bytes: Vec<u8>,
}

/// Pending input retired after refusal or terminal target closure.
#[derive(Debug)]
pub struct Retirement<O> {
    /// The original caller-owned origin.
    pub origin: O,
    /// Bytes not written to the tty driver.
    pub remaining: Vec<u8>,
    /// Prefix already written to the driver; vendor consumption is unknown.
    pub written: usize,
}

/// A complete packet written to the tty driver, retaining its original origin.
/// This receipt is explicitly not a vendor-consumption acknowledgement.
#[derive(Debug)]
pub struct Written<O> {
    /// Original origin, unchanged by the relay.
    pub origin: O,
    /// Total packet bytes written to the tty driver.
    pub bytes: usize,
}

struct Input<O> {
    origin: O,
    bytes: Box<[u8]>,
    written: usize,
}

/// Results of one bounded reactor step. Counters do not establish consumption.
#[derive(Debug)]
pub struct Step<O> {
    /// Bytes read from the outer tty during this step.
    pub read_from_outer: usize,
    /// Bytes written to the inner tty driver during this step.
    pub written_to_inner: usize,
    /// Bytes forwarded to the outer tty driver during this step.
    pub written_to_outer: usize,
    /// Extra descriptors with events, in the supplied order (native poll bits).
    pub extra_events: Vec<libc::c_short>,
    /// Refused or undeliverable input, retaining original origins and receipts.
    pub retired: Vec<Retirement<O>>,
    /// Complete packets written to the inner tty driver (not consumed proof).
    pub written: Vec<Written<O>>,
    /// The guard held the front packet; poll did not wait on writable master.
    pub held: bool,
    /// No more bytes can be read from the inner master.
    pub inner_eof: bool,
    /// No more bytes can be read from outer input.
    pub outer_eof: bool,
    /// Present only after proved producer settlement, forwarding, anchor close,
    /// and native EOF. This is not a proof about a live vendor's consumption.
    pub output_drained: Option<OutputDrained>,
}

/// An I/O failure retaining all receipts already produced by this step.
#[derive(Debug)]
pub struct StepError<O> {
    /// The validation or terminal I/O error.
    pub error: io::Error,
    /// Partial progress and original-origin receipts; queued input stays owned.
    pub step: Box<Step<O>>,
}

/// Positive writer/cohort settlement. No public constructor or buffer-derived
/// proof exists. Future native keeper custody must produce this sealed token.
#[derive(Debug)]
pub struct WriterSettlement {
    pub(super) generation: Generation,
    pub(super) device: (u64, u64, u64),
}

/// All output captured from one settled cohort was forwarded before anchor EOF.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OutputDrained {
    /// The exact logical generation which was settled.
    generation: Generation,
}

impl OutputDrained {
    /// The exact logical generation which was settled.
    pub fn generation(self) -> Generation {
        self.generation
    }
}

/// One immutable generation's private endpoints and bounded queues.
/// This reactor is not connected to the supervisor or native host input route.
pub struct Relay<O> {
    generation: Generation,
    master: std::os::fd::OwnedFd,
    outer: Outer,
    limits: Limits,
    input: VecDeque<Input<O>>,
    input_bytes: usize,
    output: VecDeque<u8>,
    inner_eof: bool,
    outer_eof: bool,
    failed: bool,
    // Passive ownership only: never read, written, or used to change modes.
    anchor: Option<std::os::fd::OwnedFd>,
    device: (u64, u64, u64),
    producer_finished: bool,
}

impl<O> Relay<O> {
    /// Consume a fresh pair; return the slave for an independently owned spawn.
    pub fn new(pair: PtyPair, outer: Outer, limits: Limits) -> io::Result<(Self, Slave)> {
        if limits.input_bytes == 0
            || limits.input_chunks == 0
            || limits.output_bytes == 0
            || limits.input_bytes > 1_048_576
            || limits.input_chunks > 4096
            || limits.output_bytes > 1_048_576
        {
            return Err(invalid("relay queue limits are outside supported bounds"));
        }
        let PtyPair {
            master,
            slave,
            generation,
        } = pair;
        let device = tty_identity(slave.as_raw_fd())?;
        // SAFETY: duplicate the newly allocated slave solely as a passive
        // CLOEXEC anchor. No flags on the shared open description are changed.
        let anchor = owned(unsafe { libc::fcntl(slave.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 3) })?;
        Ok((
            Self {
                generation,
                master,
                outer,
                limits,
                input: VecDeque::new(),
                input_bytes: 0,
                output: VecDeque::new(),
                inner_eof: false,
                outer_eof: false,
                failed: false,
                anchor: Some(anchor),
                device,
                producer_finished: false,
            },
            Slave {
                fd: slave,
                generation,
                device,
            },
        ))
    }

    /// This relay's unchangeable generation.
    pub fn generation(&self) -> Generation {
        self.generation
    }

    /// Queue a packet atomically. A stale generation or closed target refuses it.
    pub fn enqueue(
        &mut self,
        generation: Generation,
        origin: O,
        bytes: Vec<u8>,
    ) -> Result<(), InputRefusal<O>> {
        if generation != self.generation
            || self.inner_eof
            || self.failed
            || bytes.is_empty()
            || self.input.len() >= self.limits.input_chunks
            || bytes.len() > self.limits.input_bytes.saturating_sub(self.input_bytes)
        {
            return Err(InputRefusal { origin, bytes });
        }
        self.input_bytes += bytes.len();
        self.input.push_back(Input {
            origin,
            bytes: bytes.into_boxed_slice(),
            written: 0,
        });
        Ok(())
    }

    /// Retire all queued input, including any previously written prefix receipts.
    pub fn retire_input(&mut self) -> Vec<Retirement<O>> {
        self.input_bytes = 0;
        self.input
            .drain(..)
            .map(|packet| Retirement {
                origin: packet.origin,
                remaining: packet.bytes[packet.written..].to_vec(),
                written: packet.written,
            })
            .collect()
    }

    /// Explicit native death proof can stop input before the final master EOF.
    /// Output remains readable and must still be drained.
    pub fn target_died(&mut self) -> Vec<Retirement<O>> {
        self.failed = true;
        self.retire_input()
    }

    /// Admit positive native custody evidence for this exact writer cohort.
    /// Merely observing EOF, empty queues, or FIONREAD cannot create this token.
    /// The passive anchor stays open until a quiet nonblocking master read and
    /// complete forwarding; subsequent native EOF completes output retirement.
    pub fn producer_finished(
        &mut self,
        proof: WriterSettlement,
    ) -> Result<Vec<Retirement<O>>, WriterSettlement> {
        if proof.generation != self.generation
            || proof.device != self.device
            || self.producer_finished
        {
            return Err(proof);
        }
        self.producer_finished = true;
        Ok(self.target_died())
    }

    /// EOF plus no queued output or undeliverable input. Not a child-reap proof.
    pub fn drained(&self) -> bool {
        self.producer_finished
            && self.anchor.is_none()
            && self.inner_eof
            && self.output.is_empty()
            && self.input.is_empty()
    }

    /// Pending payload counts, excluding already written prefixes.
    pub fn pending(&self) -> (usize, usize) {
        (
            self.input
                .iter()
                .map(|packet| packet.bytes.len() - packet.written)
                .sum(),
            self.output.len(),
        )
    }

    /// Poll once, preserving one monotonic deadline across interrupted syscalls.
    /// `terminal_origin` labels raw outer bytes explicitly; it grants no authority.
    /// The guard must be nonblocking and must revalidate before every write.
    /// Extra descriptors are borrowed, never read, and limited to 32.
    pub fn poll_step(
        &mut self,
        timeout: Duration,
        extra: &[BorrowedFd<'_>],
        mut terminal_origin: impl FnMut() -> O,
        mut guard: impl FnMut(&O) -> Delivery,
    ) -> Result<Step<O>, StepError<O>> {
        let mut result = Step {
            read_from_outer: 0,
            written_to_inner: 0,
            written_to_outer: 0,
            extra_events: vec![0; extra.len().min(32)],
            retired: Vec::new(),
            written: Vec::new(),
            held: false,
            inner_eof: self.inner_eof,
            outer_eof: self.outer_eof,
            output_drained: None,
        };
        if extra.len() > 32 {
            return Err(StepError {
                error: invalid("too many relay wake descriptors"),
                step: Box::new(result),
            });
        }
        // Refusal/hold is evaluated before poll so a writable fd cannot make a
        // held packet spin. Readiness is not permission: Allow is checked again.
        while let Some(front) = self.input.front() {
            match guard(&front.origin) {
                Delivery::Allow => break,
                Delivery::Hold => {
                    result.held = true;
                    break;
                }
                Delivery::Refuse => result.retired.push(self.retire_front()),
            }
        }
        let mut fds = vec![
            pollfd(
                self.master.as_raw_fd(),
                if self.inner_eof {
                    0
                } else {
                    (if self.output.len() < self.limits.output_bytes {
                        libc::POLLIN
                    } else {
                        0
                    }) | (if !self.failed && !result.held && !self.input.is_empty() {
                        libc::POLLOUT
                    } else {
                        0
                    })
                },
            ),
            pollfd(
                self.outer.input.as_raw_fd(),
                if !self.outer_eof
                    && !self.failed
                    && self.input.len() < self.limits.input_chunks
                    && self.input_bytes < self.limits.input_bytes
                {
                    libc::POLLIN
                } else {
                    0
                },
            ),
            pollfd(
                self.outer.output.as_raw_fd(),
                if self.output.is_empty() {
                    0
                } else {
                    libc::POLLOUT
                },
            ),
        ];
        fds.extend(extra.iter().map(|fd| pollfd(fd.as_raw_fd(), libc::POLLIN)));
        let Some(deadline) = Instant::now().checked_add(timeout) else {
            return Err(StepError {
                error: invalid("poll timeout overflow"),
                step: Box::new(result),
            });
        };
        if let Err(error) = poll_until(&mut fds, deadline) {
            return Err(self.terminal(error, result));
        }
        for fd in &fds {
            if fd.revents & libc::POLLNVAL != 0 {
                return Err(
                    self.terminal(io::Error::other("relay descriptor became invalid"), result)
                );
            }
        }
        if fds[1].revents & libc::POLLERR != 0
            || fds[2].revents & (libc::POLLERR | libc::POLLHUP) != 0
            || fds[3..].iter().any(|fd| fd.revents & libc::POLLERR != 0)
        {
            return Err(self.terminal(io::Error::other("relay endpoint poll error"), result));
        }
        result.extra_events = fds[3..].iter().map(|fd| fd.revents).collect();
        let mut quiet = false;
        if (self.producer_finished
            || fds[0].revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0)
            && !self.inner_eof
        {
            let mut bytes = [0; 8192];
            let room = bytes
                .len()
                .min(self.limits.output_bytes - self.output.len());
            if room > 0 {
                match read(self.master.as_raw_fd(), &mut bytes[..room]) {
                    Ok(0) => self.inner_eof = true,
                    Ok(n) => self.output.extend(&bytes[..n]),
                    Err(error) if error.raw_os_error() == Some(libc::EIO) => self.inner_eof = true,
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        if fds[0].revents & libc::POLLERR != 0 {
                            return Err(self.terminal(error, result));
                        }
                        quiet = true;
                        if fds[0].revents & libc::POLLHUP != 0 {
                            self.inner_eof = true;
                        }
                    }
                    Err(error) => {
                        return Err(self.terminal(error, result));
                    }
                }
            }
        }
        if self.inner_eof {
            result.retired.extend(self.retire_input());
        }
        if fds[1].revents & (libc::POLLIN | libc::POLLHUP) != 0
            && !self.outer_eof
            && !self.inner_eof
            && !self.failed
        {
            let mut bytes = [0; 8192];
            let room = bytes.len().min(self.limits.input_bytes - self.input_bytes);
            if room > 0 && self.input.len() < self.limits.input_chunks {
                match read(self.outer.input.as_raw_fd(), &mut bytes[..room]) {
                    Ok(0) => self.outer_eof = true,
                    Ok(n) => {
                        self.input_bytes += n;
                        self.input.push_back(Input {
                            origin: terminal_origin(),
                            bytes: bytes[..n].into(),
                            written: 0,
                        });
                        result.read_from_outer = n;
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        if fds[1].revents & libc::POLLHUP != 0 {
                            self.outer_eof = true;
                        }
                    }
                    Err(error) => {
                        return Err(self.terminal(error, result));
                    }
                }
            }
        }
        if fds[0].revents & libc::POLLOUT != 0
            && !self.inner_eof
            && !self.failed
            && let Some(front) = self.input.front_mut()
        {
            match guard(&front.origin) {
                Delivery::Hold => result.held = true,
                Delivery::Refuse => result.retired.push(self.retire_front()),
                Delivery::Allow => {
                    match write(self.master.as_raw_fd(), &front.bytes[front.written..]) {
                        Ok(n) => {
                            front.written += n;
                            result.written_to_inner = n;
                            if front.written == front.bytes.len() {
                                let packet = self.input.pop_front().expect("front checked");
                                self.input_bytes -= packet.bytes.len();
                                result.written.push(Written {
                                    origin: packet.origin,
                                    bytes: packet.written,
                                });
                            }
                        }
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                        Err(error) => {
                            return Err(self.terminal(error, result));
                        }
                    }
                }
            }
        }
        if fds[2].revents & libc::POLLOUT != 0 {
            let bytes = self.output.as_slices().0;
            match write(self.outer.output.as_raw_fd(), bytes) {
                Ok(n) => {
                    self.output.drain(..n);
                    result.written_to_outer = n;
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                Err(error) => {
                    return Err(self.terminal(error, result));
                }
            }
        }
        if self.producer_finished && (quiet || self.inner_eof) && self.output.is_empty() {
            self.anchor.take();
        }
        result.inner_eof = self.inner_eof;
        result.outer_eof = self.outer_eof;
        if self.drained() {
            result.output_drained = Some(OutputDrained {
                generation: self.generation,
            });
        }
        Ok(result)
    }

    fn terminal(&mut self, error: io::Error, mut step: Step<O>) -> StepError<O> {
        self.failed = true;
        step.inner_eof = self.inner_eof;
        step.outer_eof = self.outer_eof;
        StepError {
            error,
            step: Box::new(step),
        }
    }

    fn retire_front(&mut self) -> Retirement<O> {
        let packet = self
            .input
            .pop_front()
            .expect("front checked before retirement");
        let remaining = packet.bytes[packet.written..].to_vec();
        self.input_bytes -= packet.bytes.len();
        Retirement {
            origin: packet.origin,
            remaining,
            written: packet.written,
        }
    }
}

fn pollfd(fd: RawFd, events: libc::c_short) -> libc::pollfd {
    // poll reports HUP even when events=0; disable inactive descriptors entirely.
    libc::pollfd {
        fd: if events == 0 { -1 } else { fd },
        events,
        revents: 0,
    }
}

pub(super) fn poll_until(fds: &mut [libc::pollfd], deadline: Instant) -> io::Result<()> {
    poll_until_with(fds, deadline, Instant::now, |fds, millis| {
        // SAFETY: fds is live writable storage for exactly nfds poll records.
        if unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, millis) } >= 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    })
}

pub(super) fn poll_until_with(
    fds: &mut [libc::pollfd],
    deadline: Instant,
    mut now: impl FnMut() -> Instant,
    mut poll: impl FnMut(&mut [libc::pollfd], i32) -> io::Result<()>,
) -> io::Result<()> {
    loop {
        for fd in fds.iter_mut() {
            fd.revents = 0;
        }
        let left = deadline.saturating_duration_since(now());
        let millis = left
            .as_millis()
            .saturating_add(u128::from(!left.subsec_nanos().is_multiple_of(1_000_000)))
            .min(i32::MAX as u128) as i32;
        let error = match poll(fds, millis) {
            Ok(()) => return Ok(()),
            Err(error) => error,
        };
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
        if now() >= deadline {
            for fd in fds.iter_mut() {
                fd.revents = 0;
            }
            return Ok(());
        }
    }
}

fn read(fd: RawFd, bytes: &mut [u8]) -> io::Result<usize> {
    // SAFETY: fd is live and nonblocking; bytes is a writable bounded slice.
    let n = unsafe { libc::read(fd, bytes.as_mut_ptr().cast(), bytes.len()) };
    if n < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(n as usize)
    }
}

fn write(fd: RawFd, bytes: &[u8]) -> io::Result<usize> {
    // SAFETY: fd is live and nonblocking; bytes is a readable bounded slice.
    let n = unsafe { libc::write(fd, bytes.as_ptr().cast(), bytes.len()) };
    if n < 0 {
        Err(io::Error::last_os_error())
    } else if n == 0 && !bytes.is_empty() {
        Err(io::Error::new(
            io::ErrorKind::WriteZero,
            "relay made no write progress",
        ))
    } else {
        Ok(n as usize)
    }
}
