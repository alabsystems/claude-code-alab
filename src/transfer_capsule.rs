// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Bounded binary transfer data for one forward self-exec and one rollback.
//!
//! The own-image move uses this module: a default launch outside aterm moves
//! onto a new harness build through it, and nothing else enters it. A valid
//! frame, anonymous data descriptor, checksum, or descriptor number in an
//! environment variable proves no process, image, claim, child or terminal.
//! The runtime must validate those facts independently, including the current
//! image for the phase, every surviving descriptor's identity and purpose,
//! the original same-process child, and fresh authority proof. A malformed
//! adoption entry must never fall through to the normal child-spawn path.
//!
//! The header keeps the same old/new image pair in both phases. Forward enters
//! the new image once; rollback enters the old image once. The opaque payload
//! belongs to the runtime. Encoder/decoder primitives preserve Unix OS-string
//! bytes; they do not decide whether those bytes form a valid exec argument.

use crate::procs::{Birth, Executable};
use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, RawFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{DirBuilderExt, FileExt, MetadataExt, OpenOptionsExt};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// The only capsule ABI this decoder accepts.
pub const ABI_VERSION: u16 = 1;
/// Maximum total header size, including the fixed frame prefix.
pub const MAX_HEADER_BYTES: usize = 64 * 1024;
/// Maximum opaque runtime payload size and public primitive-codec size.
pub const MAX_PAYLOAD_BYTES: usize = 1024 * 1024;
/// Maximum retained authority sockets, including an ambiguous rebind socket.
pub const MAX_AUTHORITIES: usize = 4;
/// Maximum shared GC retention gates, one for each image.
pub const MAX_RETENTION_GATES: usize = 2;

const MAGIC: &[u8; 8] = b"CLACAP\0\0";
const PREFIX_BYTES: usize = 32;
const CHECKSUM_AT: usize = 24;
const MAX_FRAME_BYTES: usize = MAX_HEADER_BYTES + MAX_PAYLOAD_BYTES;
const MAX_IMAGE_PATH: usize = 16 * 1024;
const MAX_VERSION: usize = 128;
const MAX_FILE_CALLS: usize = 1024;

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn argument(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn truncated() -> io::Error {
    io::Error::new(io::ErrorKind::UnexpectedEof, "truncated transfer data")
}

/// A bounded little-endian encoder for the runtime's opaque payload.
#[derive(Debug)]
pub struct Encoder {
    bytes: Vec<u8>,
    limit: usize,
}

impl Default for Encoder {
    fn default() -> Self {
        Self::new()
    }
}

impl Encoder {
    /// Start an empty encoder, bounded by `MAX_PAYLOAD_BYTES`.
    #[must_use]
    pub fn new() -> Self {
        Self::with_limit(MAX_PAYLOAD_BYTES)
    }

    fn with_limit(limit: usize) -> Self {
        Self {
            bytes: Vec::new(),
            limit,
        }
    }

    fn room(&mut self, count: usize) -> io::Result<()> {
        if self
            .bytes
            .len()
            .checked_add(count)
            .is_none_or(|size| size > self.limit)
        {
            return Err(argument("encoded transfer data exceeds its bound"));
        }
        self.bytes
            .try_reserve_exact(count)
            .map_err(|_| io::Error::new(io::ErrorKind::OutOfMemory, "transfer allocation refused"))
    }

    fn raw(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.room(bytes.len())?;
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }

    /// Append one byte.
    ///
    /// # Errors
    /// The encoder's bound or allocation refuses the value.
    pub fn u8(&mut self, value: u8) -> io::Result<()> {
        self.raw(&[value])
    }

    /// Append an unsigned 32-bit little-endian integer.
    ///
    /// # Errors
    /// The encoder's bound or allocation refuses the value.
    pub fn u32(&mut self, value: u32) -> io::Result<()> {
        self.raw(&value.to_le_bytes())
    }

    /// Append an unsigned 64-bit little-endian integer.
    ///
    /// # Errors
    /// The encoder's bound or allocation refuses the value.
    pub fn u64(&mut self, value: u64) -> io::Result<()> {
        self.raw(&value.to_le_bytes())
    }

    /// Append exactly zero or one.
    ///
    /// # Errors
    /// The encoder's bound or allocation refuses the value.
    pub fn boolean(&mut self, value: bool) -> io::Result<()> {
        self.u8(u8::from(value))
    }

    /// Append a bounded collection count as an unsigned 32-bit integer.
    ///
    /// # Errors
    /// The count exceeds `maximum`, cannot fit, or the encoder refuses it.
    pub fn count(&mut self, value: usize, maximum: usize) -> io::Result<()> {
        if value > maximum {
            return Err(argument("transfer count exceeds its bound"));
        }
        self.u32(u32::try_from(value).map_err(|_| argument("transfer count does not fit"))?)
    }

    /// Append a length-prefixed byte string.
    ///
    /// # Errors
    /// The string length cannot fit, or the complete value exceeds the encoder's bound.
    pub fn bytes(&mut self, value: &[u8]) -> io::Result<()> {
        let length =
            u32::try_from(value.len()).map_err(|_| argument("transfer string does not fit"))?;
        self.room(
            value
                .len()
                .checked_add(4)
                .ok_or_else(|| argument("transfer string length overflows"))?,
        )?;
        self.raw(&length.to_le_bytes())?;
        self.raw(value)
    }

    /// Append Unix OS-string bytes without UTF-8 conversion or argument interpretation.
    ///
    /// # Errors
    /// The complete value exceeds the encoder's bound.
    pub fn os_string(&mut self, value: &OsStr) -> io::Result<()> {
        self.bytes(value.as_bytes())
    }

    /// Finish the bounded payload after every append succeeded.
    #[must_use]
    pub fn finish(self) -> Vec<u8> {
        self.bytes
    }
}

/// A borrowing, bounded decoder for one complete runtime payload.
#[derive(Debug)]
pub struct Decoder<'a> {
    remaining: &'a [u8],
}

impl<'a> Decoder<'a> {
    /// Decode bytes no larger than `MAX_PAYLOAD_BYTES`, without copying them.
    ///
    /// # Errors
    /// The payload exceeds its bound.
    pub fn new(bytes: &'a [u8]) -> io::Result<Self> {
        if bytes.len() > MAX_PAYLOAD_BYTES {
            return Err(invalid("transfer payload exceeds its bound"));
        }
        Ok(Self { remaining: bytes })
    }

    fn take(&mut self, count: usize) -> io::Result<&'a [u8]> {
        let value = self.remaining.get(..count).ok_or_else(truncated)?;
        self.remaining = self.remaining.get(count..).ok_or_else(truncated)?;
        Ok(value)
    }

    /// Read one byte.
    ///
    /// # Errors
    /// The payload is truncated.
    pub fn u8(&mut self) -> io::Result<u8> {
        self.take(1)?.first().copied().ok_or_else(truncated)
    }

    /// Read an unsigned 32-bit little-endian integer.
    ///
    /// # Errors
    /// The payload is truncated.
    pub fn u32(&mut self) -> io::Result<u32> {
        Ok(u32::from_le_bytes(
            self.take(4)?.try_into().map_err(|_| truncated())?,
        ))
    }

    /// Read an unsigned 64-bit little-endian integer.
    ///
    /// # Errors
    /// The payload is truncated.
    pub fn u64(&mut self) -> io::Result<u64> {
        Ok(u64::from_le_bytes(
            self.take(8)?.try_into().map_err(|_| truncated())?,
        ))
    }

    /// Read exactly zero or one, rejecting every other boolean spelling.
    ///
    /// # Errors
    /// The payload is truncated or the byte is not zero/one.
    pub fn boolean(&mut self) -> io::Result<bool> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(invalid("transfer boolean is not zero or one")),
        }
    }

    /// Read a collection count without allocating or accepting a count above `maximum`.
    ///
    /// # Errors
    /// The count is truncated, cannot fit, or exceeds its bound.
    pub fn count(&mut self, maximum: usize) -> io::Result<usize> {
        let count =
            usize::try_from(self.u32()?).map_err(|_| invalid("transfer count does not fit"))?;
        if count > maximum {
            return Err(invalid("transfer count exceeds its bound"));
        }
        Ok(count)
    }

    /// Borrow one length-prefixed string after checking its individual bound.
    ///
    /// # Errors
    /// The length/string is truncated or exceeds `maximum`.
    pub fn bytes(&mut self, maximum: usize) -> io::Result<&'a [u8]> {
        let count = self.count(maximum)?;
        self.take(count)
    }

    /// Read a bounded OS string, preserving Unix bytes exactly.
    ///
    /// # Errors
    /// The length/string is truncated or exceeds `maximum`.
    pub fn os_string(&mut self, maximum: usize) -> io::Result<OsString> {
        Ok(OsString::from_vec(self.bytes(maximum)?.to_vec()))
    }

    /// Require that the runtime consumed the complete payload.
    ///
    /// # Errors
    /// Any trailing data remains.
    pub fn finish(self) -> io::Result<()> {
        if self.remaining.is_empty() {
            Ok(())
        } else {
            Err(invalid("trailing transfer data"))
        }
    }
}

/// Which member of the fixed image pair this exec enters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// Enter the new image after the sole forward attempt.
    Forward,
    /// Enter the old image after the sole rollback attempt.
    Rollback,
}

/// An image and compiled version claimed by transfer data, requiring independent validation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    /// Absolute Unix image path, device and inode.
    pub executable: Executable,
    /// `MAJOR.MINOR.0`, optionally followed by the harness's `+g<hex>` build stamp.
    pub version: String,
}

/// Exact descriptor roles, never a grant to adopt an arbitrary environment-named descriptor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FdRoles {
    /// This anonymous capsule data descriptor.
    pub capsule: RawFd,
    /// The signal pipe's read end.
    pub signal_read: RawFd,
    /// The signal pipe's write end.
    pub signal_write: RawFd,
    /// Old authority custody and a small set of possibly-current rebind sockets.
    pub authorities: Vec<RawFd>,
    /// Shared GC gates retaining the old and new images through the bounded handoff.
    pub retention_gates: Vec<RawFd>,
}

/// Bounded structural facts carried beside the runtime's opaque state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Header {
    /// Forward enters `new_image`; rollback enters `old_image`.
    pub phase: Phase,
    /// Exactly one forward attempt has been made.
    pub forward_attempt: u8,
    /// Zero on forward, exactly one on rollback.
    pub rollback_attempt: u8,
    /// The same harness process must survive both execs.
    pub self_pid: u32,
    /// The harness process's kernel birth stamp.
    pub self_birth: Birth,
    /// The fixed original image, which must itself support this capsule ABI.
    pub old_image: Image,
    /// The fixed forward target image.
    pub new_image: Image,
    /// The complete surviving nonstandard descriptor roles.
    pub fds: FdRoles,
}

/// A structurally valid capsule; no field is proven by decoding it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Capsule {
    /// The transfer's structural header.
    pub header: Header,
    /// Runtime-owned state which must be decoded and independently validated.
    pub payload: Vec<u8>,
}

fn valid_version(version: &str) -> bool {
    if version.len() > MAX_VERSION {
        return false;
    }
    let (base, stamp) = version
        .split_once('+')
        .map_or((version, None), |(base, stamp)| (base, Some(stamp)));
    if let Some(stamp) = stamp {
        let Some(hex) = stamp.strip_prefix('g') else {
            return false;
        };
        if !(9..=40).contains(&hex.len())
            || !hex
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        {
            return false;
        }
    }
    let mut parts = base.split('.');
    let decimal = |value: &str| {
        !value.is_empty()
            && (value == "0" || !value.starts_with('0'))
            && value.bytes().all(|byte| byte.is_ascii_digit())
            && value.parse::<u32>().is_ok()
    };
    matches!((parts.next(), parts.next(), parts.next(), parts.next()), (Some(major), Some(minor), Some("0"), None) if decimal(major) && decimal(minor))
}

fn validate_header(header: &Header) -> io::Result<()> {
    let attempts = (header.forward_attempt, header.rollback_attempt);
    if header.self_pid <= 1
        || !matches!(
            (header.phase, attempts),
            (Phase::Forward, (1, 0)) | (Phase::Rollback, (1, 1))
        )
    {
        return Err(argument("invalid transfer phase, attempts or self PID"));
    }
    if matches!(header.self_birth, Birth::MacOs { microseconds, .. } if microseconds >= 1_000_000) {
        return Err(argument("invalid kernel birth microseconds"));
    }
    for image in [&header.old_image, &header.new_image] {
        let path = image.executable.path.as_os_str().as_bytes();
        if !image.executable.path.is_absolute()
            || path.len() > MAX_IMAGE_PATH
            || path.contains(&0)
            || !valid_version(&image.version)
        {
            return Err(argument("invalid transfer image path or version"));
        }
    }
    if header.fds.authorities.len() > MAX_AUTHORITIES
        || header.fds.retention_gates.len() > MAX_RETENTION_GATES
    {
        return Err(argument("too many transfer descriptor roles"));
    }
    let mut seen = BTreeSet::new();
    for fd in [
        header.fds.capsule,
        header.fds.signal_read,
        header.fds.signal_write,
    ]
    .into_iter()
    .chain(header.fds.authorities.iter().copied())
    .chain(header.fds.retention_gates.iter().copied())
    {
        if fd < 3 || !seen.insert(fd) {
            return Err(argument("standard or duplicate transfer descriptor"));
        }
    }
    Ok(())
}

fn put_image(encoder: &mut Encoder, image: &Image) -> io::Result<()> {
    encoder.os_string(image.executable.path.as_os_str())?;
    encoder.u64(image.executable.device)?;
    encoder.u64(image.executable.inode)?;
    encoder.bytes(image.version.as_bytes())
}

fn get_image(decoder: &mut Decoder<'_>) -> io::Result<Image> {
    let path = PathBuf::from(decoder.os_string(MAX_IMAGE_PATH)?);
    let device = decoder.u64()?;
    let inode = decoder.u64()?;
    let version = String::from_utf8(decoder.bytes(MAX_VERSION)?.to_vec())
        .map_err(|_| invalid("transfer version is not UTF-8"))?;
    Ok(Image {
        executable: Executable {
            path,
            device,
            inode,
        },
        version,
    })
}

fn checksum(frame: &[u8]) -> u64 {
    // FNV-1a detects accidental corruption only. An attacker can recompute it.
    frame
        .get(..CHECKSUM_AT)
        .unwrap_or_default()
        .iter()
        .chain(frame.get(PREFIX_BYTES..).unwrap_or_default())
        .fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x100_0000_01b3)
        })
}

/// Encode a strict ABI-1 frame. This does not prove any header or payload fact.
///
/// # Errors
/// A header fact, attempt, descriptor role, version or size is structurally invalid.
pub fn encode(header: &Header, payload: &[u8]) -> io::Result<Vec<u8>> {
    validate_header(header)?;
    if payload.len() > MAX_PAYLOAD_BYTES {
        return Err(argument("transfer payload exceeds its bound"));
    }
    let mut body = Encoder::with_limit(MAX_HEADER_BYTES - PREFIX_BYTES);
    body.u32(header.self_pid)?;
    match header.self_birth {
        Birth::MacOs {
            seconds,
            microseconds,
        } => {
            body.u8(1)?;
            body.u64(seconds)?;
            body.u32(microseconds)?;
        }
        Birth::Linux(ticks) => {
            body.u8(2)?;
            body.u64(ticks)?;
        }
    }
    put_image(&mut body, &header.old_image)?;
    put_image(&mut body, &header.new_image)?;
    for fd in [
        header.fds.capsule,
        header.fds.signal_read,
        header.fds.signal_write,
    ] {
        body.u32(fd as u32)?;
    }
    body.count(header.fds.authorities.len(), MAX_AUTHORITIES)?;
    for fd in &header.fds.authorities {
        body.u32(*fd as u32)?;
    }
    body.count(header.fds.retention_gates.len(), MAX_RETENTION_GATES)?;
    for fd in &header.fds.retention_gates {
        body.u32(*fd as u32)?;
    }
    let body = body.finish();
    let header_length = PREFIX_BYTES
        .checked_add(body.len())
        .ok_or_else(|| invalid("capsule header exceeds its bound"))?;
    let mut frame = Encoder::with_limit(MAX_FRAME_BYTES);
    frame.raw(MAGIC)?;
    frame.raw(&ABI_VERSION.to_le_bytes())?;
    frame.u8(match header.phase {
        Phase::Forward => 1,
        Phase::Rollback => 2,
    })?;
    frame.u8(header.forward_attempt)?;
    frame.u8(header.rollback_attempt)?;
    frame.raw(&[0; 3])?;
    frame.u32(header_length as u32)?;
    frame.u32(payload.len() as u32)?;
    frame.u64(0)?;
    frame.raw(&body)?;
    frame.raw(payload)?;
    let mut frame = frame.finish();
    let check = checksum(&frame).to_le_bytes();
    frame
        .get_mut(CHECKSUM_AT..PREFIX_BYTES)
        .ok_or_else(truncated)?
        .copy_from_slice(&check);
    Ok(frame)
}

fn frame_u32(frame: &[u8], offset: usize) -> io::Result<u32> {
    let end = offset.checked_add(4).ok_or_else(truncated)?;
    Ok(u32::from_le_bytes(
        frame
            .get(offset..end)
            .ok_or_else(truncated)?
            .try_into()
            .map_err(|_| truncated())?,
    ))
}

fn get_fd(decoder: &mut Decoder<'_>) -> io::Result<RawFd> {
    i32::try_from(decoder.u32()?).map_err(|_| invalid("transfer descriptor does not fit"))
}

/// Decode one complete frame with bounded allocations, strict attempts and no trailing data.
///
/// # Errors
/// The frame is malformed, truncated, oversized, uses another ABI, or fails its non-provenance checksum.
pub fn decode(frame: &[u8]) -> io::Result<Capsule> {
    if frame.len() < PREFIX_BYTES {
        return Err(truncated());
    }
    if frame.len() > MAX_FRAME_BYTES {
        return Err(invalid("transfer frame exceeds its bound"));
    }
    if frame.get(..8) != Some(MAGIC.as_slice()) {
        return Err(invalid("transfer magic mismatch"));
    }
    let abi = u16::from_le_bytes(
        frame
            .get(8..10)
            .ok_or_else(truncated)?
            .try_into()
            .map_err(|_| truncated())?,
    );
    if abi != ABI_VERSION || frame.get(13..16) != Some([0_u8; 3].as_slice()) {
        return Err(invalid("unsupported transfer ABI or reserved fields"));
    }
    let phase = match frame[10] {
        1 => Phase::Forward,
        2 => Phase::Rollback,
        _ => return Err(invalid("invalid transfer phase")),
    };
    let header_length = frame_u32(frame, 16)? as usize;
    let payload_length = frame_u32(frame, 20)? as usize;
    if !(PREFIX_BYTES..=MAX_HEADER_BYTES).contains(&header_length)
        || payload_length > MAX_PAYLOAD_BYTES
        || header_length.checked_add(payload_length) != Some(frame.len())
    {
        return Err(invalid("invalid transfer lengths or trailing data"));
    }
    let recorded = u64::from_le_bytes(
        frame
            .get(CHECKSUM_AT..PREFIX_BYTES)
            .ok_or_else(truncated)?
            .try_into()
            .map_err(|_| truncated())?,
    );
    if recorded != checksum(frame) {
        return Err(invalid("transfer checksum mismatch"));
    }
    let mut decoder = Decoder::new(
        frame
            .get(PREFIX_BYTES..header_length)
            .ok_or_else(truncated)?,
    )?;
    let self_pid = decoder.u32()?;
    let self_birth = match decoder.u8()? {
        1 => Birth::MacOs {
            seconds: decoder.u64()?,
            microseconds: decoder.u32()?,
        },
        2 => Birth::Linux(decoder.u64()?),
        _ => return Err(invalid("invalid kernel birth kind")),
    };
    let old_image = get_image(&mut decoder)?;
    let new_image = get_image(&mut decoder)?;
    let capsule = get_fd(&mut decoder)?;
    let signal_read = get_fd(&mut decoder)?;
    let signal_write = get_fd(&mut decoder)?;
    let mut authorities = Vec::new();
    for _ in 0..decoder.count(MAX_AUTHORITIES)? {
        authorities.push(get_fd(&mut decoder)?);
    }
    let mut retention_gates = Vec::new();
    for _ in 0..decoder.count(MAX_RETENTION_GATES)? {
        retention_gates.push(get_fd(&mut decoder)?);
    }
    decoder.finish()?;
    let header = Header {
        phase,
        forward_attempt: frame[11],
        rollback_attempt: frame[12],
        self_pid,
        self_birth,
        old_image,
        new_image,
        fds: FdRoles {
            capsule,
            signal_read,
            signal_write,
            authorities,
            retention_gates,
        },
    };
    validate_header(&header).map_err(|_| invalid("invalid transfer header facts"))?;
    Ok(Capsule {
        header,
        payload: frame.get(header_length..).ok_or_else(truncated)?.to_vec(),
    })
}

fn fd_flags(fd: RawFd) -> io::Result<libc::c_int> {
    // SAFETY: F_GETFD observes the borrowed live descriptor and has no pointer argument.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(flags)
    }
}

fn set_fd_flags(fd: RawFd, flags: libc::c_int) -> io::Result<()> {
    // SAFETY: F_SETFD changes only descriptor flags on this borrowed live descriptor.
    if unsafe { libc::fcntl(fd, libc::F_SETFD, flags) } < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// File-status storage with every field zero; fstat fills it.
#[cfg(any(target_os = "macos", target_os = "ios"))]
fn blank_stat() -> libc::stat {
    libc::stat::default()
}

/// File-status storage with every field zero; fstat fills it.
#[cfg(not(any(target_os = "macos", target_os = "ios")))]
fn blank_stat() -> libc::stat {
    // SAFETY: zero initializes the scalar stat fields.
    unsafe { std::mem::zeroed() }
}

fn stat_fd(fd: BorrowedFd<'_>) -> io::Result<libc::stat> {
    let mut stat = blank_stat();
    // SAFETY: fd remains borrowed live, and stat is writable for the duration of the call.
    if unsafe { libc::fstat(fd.as_raw_fd(), &raw mut stat) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(stat)
}

fn validate_data(fd: BorrowedFd<'_>, inherited: bool) -> io::Result<usize> {
    let stat = stat_fd(fd)?;
    // SAFETY: geteuid is a getter with no pointer arguments.
    let uid = unsafe { libc::geteuid() };
    // SAFETY: F_GETFL observes flags on the live borrowed descriptor, with no pointer argument.
    let access = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFL) };
    if access < 0 {
        return Err(io::Error::last_os_error());
    }
    check_data(&stat, access, inherited, uid)
}

fn check_data(
    stat: &libc::stat,
    access: libc::c_int,
    inherited: bool,
    uid: libc::uid_t,
) -> io::Result<usize> {
    if stat.st_mode & libc::S_IFMT != libc::S_IFREG
        || stat.st_uid != uid
        || stat.st_mode & 0o7777 != 0o600
        || stat.st_nlink != 0
    {
        return Err(invalid(
            "capsule descriptor is not private anonymous regular data",
        ));
    }
    if access & libc::O_ACCMODE != libc::O_RDWR || access & libc::O_APPEND != 0 {
        return Err(invalid("capsule descriptor has unexpected access"));
    }
    let size = usize::try_from(stat.st_size).map_err(|_| invalid("invalid capsule file size"))?;
    if size > MAX_FRAME_BYTES || (inherited && size < PREFIX_BYTES) {
        return Err(invalid("capsule file size is outside its bound"));
    }
    Ok(size)
}

/// Read inherited anonymous data without taking ownership of the supplied descriptor.
/// CLOEXEC is restored on a nonstandard descriptor before any parsing or allocation.
/// The runtime must independently establish the full adoption context before owning other roles.
///
/// # Errors
/// Descriptor type, owner, mode, links, size, access, role binding, reads or capsule framing are invalid.
pub fn read_inherited(fd: BorrowedFd<'_>) -> io::Result<Capsule> {
    let raw = fd.as_raw_fd();
    if raw < 3 {
        return Err(argument("a capsule cannot use a standard descriptor"));
    }
    set_fd_flags(raw, fd_flags(raw)? | libc::FD_CLOEXEC)?;
    let size = validate_data(fd, true)?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(size)
        .map_err(|_| io::Error::new(io::ErrorKind::OutOfMemory, "capsule allocation refused"))?;
    bytes.resize(size, 0);
    let mut offset = 0;
    for _ in 0..MAX_FILE_CALLS {
        if offset >= size {
            break;
        }
        // SAFETY: bytes[offset..] is initialized writable storage, the borrowed FD stays live,
        // and the nonnegative bounded offset fits off_t on the supported platforms.
        let read = unsafe {
            libc::pread(
                raw,
                bytes[offset..].as_mut_ptr().cast(),
                size - offset,
                offset as libc::off_t,
            )
        };
        if read < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        let read = usize::try_from(read).map_err(|_| invalid("invalid capsule read size"))?;
        if read == 0 {
            return Err(truncated());
        }
        offset = match offset.checked_add(read) {
            Some(next) if next <= size => next,
            _ => return Err(invalid("capsule read exceeded its requested size")),
        };
    }
    if offset < size {
        return Err(invalid("capsule read exceeds its operation bound"));
    }
    if validate_data(fd, true)? != size {
        return Err(invalid("capsule file changed size during read"));
    }
    let capsule = decode(&bytes)?;
    if capsule.header.fds.capsule != raw {
        return Err(invalid(
            "capsule descriptor role does not match the supplied descriptor",
        ));
    }
    Ok(capsule)
}

/// A private, anonymous, nonclonable data file. It is never an executable image.
/// Borrow its descriptor through [`crate::sys::inherit_for_one_exec`] only after
/// blocking managed signals; that guard provides checked restoration before unmasking.
#[derive(Debug)]
pub struct AnonymousData {
    file: File,
}

struct EmptyDirectory(PathBuf);

impl Drop for EmptyDirectory {
    fn drop(&mut self) {
        // Only this freshly created directory, and only while empty; never its children.
        let _ = std::fs::remove_dir(&self.0);
    }
}

impl AsRawFd for AnonymousData {
    fn as_raw_fd(&self) -> RawFd {
        self.file.as_raw_fd()
    }
}
impl AsFd for AnonymousData {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.file.as_fd()
    }
}

impl AnonymousData {
    /// Create exclusive mode-0600 regular data in a fresh mode-0700 `.ubd` directory,
    /// unlink only that data file immediately, then remove its empty private directory.
    ///
    /// # Errors
    /// Private creation, descriptor flags, file identity checks or anonymous unlink fails.
    pub fn create() -> io::Result<Self> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let parent = std::env::temp_dir().join(".ubd");
        std::fs::create_dir_all(&parent)?;
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| argument("clock predates epoch"))?
            .as_nanos();
        let mut directory = None;
        for _ in 0..32 {
            let count = NEXT.fetch_add(1, Ordering::Relaxed);
            let candidate = parent.join(format!(
                "claude-capsule-{}-{stamp:x}-{count:x}.noindex",
                std::process::id()
            ));
            match std::fs::DirBuilder::new().mode(0o700).create(&candidate) {
                Ok(()) => {
                    directory = Some(candidate);
                    break;
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
        }
        let directory = EmptyDirectory(directory.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::AlreadyExists,
                "no exclusive capsule directory",
            )
        })?);
        let mode = std::fs::symlink_metadata(&directory.0)?;
        // SAFETY: geteuid is a getter with no pointer arguments.
        let uid = unsafe { libc::geteuid() };
        if !mode.is_dir() || mode.mode() & 0o7777 != 0o700 || mode.uid() != uid {
            return Err(invalid("capsule directory is not private"));
        }
        let path = directory.0.join("capsule.data");
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(&path)?;
        let created = file.metadata()?;
        let named = std::fs::symlink_metadata(&path)?;
        // Only this freshly created DATA file is ever unlinked. No executable is copied here.
        if !created.is_file()
            || !named.is_file()
            || created.dev() != named.dev()
            || created.ino() != named.ino()
            || created.nlink() != 1
            || created.mode() & 0o7777 != 0o600
            || created.uid() != mode.uid()
        {
            return Err(invalid("capsule data identity changed before unlink"));
        }
        set_fd_flags(
            file.as_raw_fd(),
            fd_flags(file.as_raw_fd())? | libc::FD_CLOEXEC,
        )?;
        std::fs::remove_file(&path)?;
        validate_data(file.as_fd(), false)?;
        Ok(Self { file })
    }

    /// Validate/encode before changing this data file. Invalid local arguments retain
    /// both descriptor ownership and any previously written valid frame.
    ///
    /// # Errors
    /// Local framing/role facts are invalid, or writing the bounded data file fails.
    pub fn write_capsule(&mut self, header: &Header, payload: &[u8]) -> io::Result<()> {
        if header.fds.capsule != self.as_raw_fd() {
            return Err(argument("capsule role names another descriptor"));
        }
        let bytes = encode(header, payload)?;
        validate_data(self.as_fd(), false)?;
        self.file.set_len(0)?;
        let mut offset = 0;
        for _ in 0..MAX_FILE_CALLS {
            let Some(rest) = bytes.get(offset..).filter(|rest| !rest.is_empty()) else {
                break;
            };
            match self.file.write_at(rest, offset as u64) {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::WriteZero,
                        "capsule accepted no bytes",
                    ));
                }
                Ok(written) => {
                    offset = match offset.checked_add(written) {
                        Some(next) if next <= bytes.len() => next,
                        _ => return Err(invalid("capsule write exceeded its requested size")),
                    };
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            }
        }
        if offset < bytes.len() {
            return Err(invalid("capsule write exceeds its operation bound"));
        }
        self.file.set_len(bytes.len() as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Seek, SeekFrom, Write};
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::UnixStream;

    fn header(capsule: RawFd) -> Header {
        Header {
            phase: Phase::Forward,
            forward_attempt: 1,
            rollback_attempt: 0,
            self_pid: 4242,
            self_birth: Birth::MacOs {
                seconds: 1_790_000_000,
                microseconds: 123_456,
            },
            old_image: Image {
                executable: Executable {
                    path: PathBuf::from(OsString::from_vec(b"/old/\xff/claude-alab".to_vec())),
                    device: 11,
                    inode: 111,
                },
                version: "0.1.0+g123456789".to_string(),
            },
            new_image: Image {
                executable: Executable {
                    path: PathBuf::from("/new/claude-alab"),
                    device: 22,
                    inode: 222,
                },
                version: "0.2.0+gabcdef123".to_string(),
            },
            fds: FdRoles {
                capsule,
                signal_read: capsule + 1,
                signal_write: capsule + 2,
                authorities: vec![capsule + 3, capsule + 4],
                retention_gates: vec![capsule + 5, capsule + 6],
            },
        }
    }

    fn restamp(frame: &mut [u8]) {
        let check = checksum(frame).to_le_bytes();
        frame[CHECKSUM_AT..PREFIX_BYTES].copy_from_slice(&check);
    }

    #[test]
    fn primitives_roundtrip_unix_bytes_and_require_complete_consumption() {
        let value = OsString::from_vec(b"flag=\xff\0\n".to_vec());
        let mut encoder = Encoder::default();
        encoder.u8(255).unwrap();
        encoder.u32(u32::MAX).unwrap();
        encoder.u64(u64::MAX).unwrap();
        encoder.boolean(true).unwrap();
        encoder.boolean(false).unwrap();
        encoder.count(3, 3).unwrap();
        encoder.os_string(&value).unwrap();
        let bytes = encoder.finish();
        let mut decoder = Decoder::new(&bytes).unwrap();
        assert_eq!(decoder.u8().unwrap(), 255);
        assert_eq!(decoder.u32().unwrap(), u32::MAX);
        assert_eq!(decoder.u64().unwrap(), u64::MAX);
        assert!(decoder.boolean().unwrap());
        assert!(!decoder.boolean().unwrap());
        assert_eq!(decoder.count(3).unwrap(), 3);
        assert_eq!(decoder.os_string(64).unwrap(), value);
        decoder.finish().unwrap();
        assert!(Decoder::new(&bytes).unwrap().finish().is_err());
    }

    #[test]
    fn primitive_counts_truncation_booleans_and_encoder_limits_fail_before_allocation() {
        assert!(Decoder::new(&[2]).unwrap().boolean().is_err());
        assert!(Decoder::new(&[1, 2, 3]).unwrap().u32().is_err());
        assert!(
            Decoder::new(&u32::MAX.to_le_bytes())
                .unwrap()
                .bytes(64)
                .is_err()
        );
        assert!(Decoder::new(&[3, 0, 0, 0, 1]).unwrap().bytes(3).is_err());
        assert!(Decoder::new(&[3, 0, 0, 0]).unwrap().count(2).is_err());
        let large = vec![0; MAX_PAYLOAD_BYTES + 1];
        assert!(Decoder::new(&large).is_err());
        let mut encoder = Encoder::new();
        encoder.u8(42).unwrap();
        assert!(encoder.bytes(&large).is_err());
        assert!(encoder.count(4, 3).is_err());
        assert_eq!(encoder.finish(), [42]);
    }

    #[test]
    fn both_phases_keep_the_same_image_pair_and_kernel_birth_kind() {
        for birth in [
            Birth::MacOs {
                seconds: 1,
                microseconds: 999_999,
            },
            Birth::Linux(u64::MAX),
        ] {
            let mut header = header(11);
            header.self_birth = birth;
            for (phase, rollback_attempt) in [(Phase::Forward, 0), (Phase::Rollback, 1)] {
                header.phase = phase;
                header.rollback_attempt = rollback_attempt;
                let frame = encode(&header, b"opaque\0runtime\xff").unwrap();
                let decoded = decode(&frame).unwrap();
                assert_eq!(decoded.header, header);
                assert_eq!(decoded.payload, b"opaque\0runtime\xff");
            }
        }
    }

    #[test]
    fn frame_words_reject_overflowing_and_truncated_offsets() {
        let frame = [1, 2, 3, 4, 5];
        assert_eq!(frame_u32(&frame, 1).unwrap(), 0x0504_0302);
        for offset in [2, frame.len(), usize::MAX - 3, usize::MAX] {
            assert!(frame_u32(&frame, offset).is_err(), "offset {offset}");
        }
    }

    #[test]
    fn truncated_frames_corruption_trailing_data_and_unknown_abis_are_refused() {
        let frame = encode(&header(11), b"payload").unwrap();
        for length in 0..frame.len() {
            assert!(decode(&frame[..length]).is_err(), "truncation {length}");
        }
        for position in [0, 8, 13, CHECKSUM_AT, frame.len() - 1] {
            let mut wrong = frame.clone();
            wrong[position] ^= 0x80;
            assert!(decode(&wrong).is_err(), "corruption {position}");
        }
        let mut trailing = frame.clone();
        trailing.push(0);
        assert!(decode(&trailing).is_err());
        for (offset, value) in [(16, u32::MAX), (20, u32::MAX), (16, 0)] {
            let mut wrong = frame.clone();
            wrong[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
            restamp(&mut wrong);
            assert!(decode(&wrong).is_err());
        }
        for (offset, value) in [(10, 3), (11, 0), (11, 2), (12, 1)] {
            let mut wrong = frame.clone();
            wrong[offset] = value;
            restamp(&mut wrong);
            assert!(decode(&wrong).is_err(), "phase/attempt {offset}={value}");
        }
    }

    #[test]
    fn header_validation_refuses_attempts_versions_and_duplicate_or_excess_roles() {
        let baseline = header(11);
        for version in [
            "0.1.1",
            "00.1.0",
            "0.1.0-pre",
            "0.1.0+g1234",
            "0.1.0+gABCDEF123",
            "0.1.0+garbage000",
        ] {
            let mut wrong = baseline.clone();
            wrong.new_image.version = version.to_string();
            assert!(encode(&wrong, &[]).is_err(), "version {version}");
        }
        let mut cases = Vec::new();
        let mut wrong = baseline.clone();
        wrong.self_pid = 1;
        cases.push(wrong);
        let mut wrong = baseline.clone();
        wrong.forward_attempt = 2;
        cases.push(wrong);
        let mut wrong = baseline.clone();
        wrong.phase = Phase::Rollback;
        cases.push(wrong);
        let mut wrong = baseline.clone();
        wrong.rollback_attempt = 1;
        cases.push(wrong);
        let mut wrong = baseline.clone();
        wrong.self_birth = Birth::MacOs {
            seconds: 1,
            microseconds: 1_000_000,
        };
        cases.push(wrong);
        let mut wrong = baseline.clone();
        wrong.old_image.executable.path = PathBuf::from("relative");
        cases.push(wrong);
        let mut wrong = baseline.clone();
        wrong.old_image.executable.path =
            PathBuf::from(OsString::from_vec(b"/old\0image".to_vec()));
        cases.push(wrong);
        let mut wrong = baseline.clone();
        wrong.fds.signal_read = 2;
        cases.push(wrong);
        let mut wrong = baseline.clone();
        wrong.fds.authorities.push(wrong.fds.signal_write);
        cases.push(wrong);
        let mut wrong = baseline.clone();
        wrong.fds.retention_gates[0] = wrong.fds.authorities[0];
        cases.push(wrong);
        let mut wrong = baseline.clone();
        wrong.fds.authorities = vec![20, 21, 22, 23, 24];
        cases.push(wrong);
        let mut wrong = baseline.clone();
        wrong.fds.retention_gates = vec![25, 26, 27];
        cases.push(wrong);
        for wrong in cases {
            assert!(encode(&wrong, &[]).is_err(), "{wrong:?}");
        }
    }

    #[test]
    fn recomputed_checksums_cannot_bypass_decoded_fd_counts_or_uniqueness() {
        let frame = encode(&header(11), b"state").unwrap();
        let header_length = frame_u32(&frame, 16).unwrap() as usize;
        let mut body = Decoder::new(&frame[PREFIX_BYTES..header_length]).unwrap();
        body.u32().unwrap();
        assert_eq!(body.u8().unwrap(), 1);
        body.u64().unwrap();
        body.u32().unwrap();
        get_image(&mut body).unwrap();
        get_image(&mut body).unwrap();
        let fds_at = header_length - body.remaining.len();
        for (offset, value) in [
            (fds_at + 4, 11),
            (fds_at, u32::MAX),
            (fds_at + 12, MAX_AUTHORITIES as u32 + 1),
            (fds_at + 16 + 2 * 4, MAX_RETENTION_GATES as u32 + 1),
        ] {
            let mut wrong = frame.clone();
            wrong[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
            restamp(&mut wrong);
            assert!(decode(&wrong).is_err(), "descriptor mutation at {offset}");
        }
    }

    #[test]
    fn integrity_checksum_is_not_process_or_payload_provenance() {
        let mut frame = encode(&header(11), b"old").unwrap();
        *frame.last_mut().unwrap() = b'x';
        assert!(decode(&frame).is_err());
        restamp(&mut frame);
        assert_eq!(decode(&frame).unwrap().payload, b"olx");
        let mut other = header(11);
        other.self_pid += 1;
        assert_eq!(
            decode(&encode(&other, b"state").unwrap())
                .unwrap()
                .header
                .self_pid,
            other.self_pid,
            "decoding never checks the kernel's same-process facts"
        );
    }

    #[test]
    fn anonymous_data_is_private_unlinked_and_reads_do_not_move_the_offset() {
        let mut data = AnonymousData::create().unwrap();
        let header = header(data.as_raw_fd());
        data.write_capsule(&header, b"state").unwrap();
        let stat = stat_fd(data.as_fd()).unwrap();
        assert_eq!(stat.st_nlink, 0);
        assert_eq!(stat.st_mode & 0o7777, 0o600);
        assert_ne!(fd_flags(data.as_raw_fd()).unwrap() & libc::FD_CLOEXEC, 0);
        data.file.seek(SeekFrom::Start(7)).unwrap();
        let decoded = read_inherited(data.as_fd()).unwrap();
        assert_eq!(decoded.header, header);
        assert_eq!(decoded.payload, b"state");
        assert_eq!(data.file.stream_position().unwrap(), 7);
        assert_ne!(fd_flags(data.as_raw_fd()).unwrap() & libc::FD_CLOEXEC, 0);
    }

    #[test]
    fn invalid_local_writes_retain_ownership_and_the_prior_frame() {
        let mut data = AnonymousData::create().unwrap();
        let original = data.as_raw_fd();
        let header = header(original);
        data.write_capsule(&header, b"prior").unwrap();
        let mut wrong = header.clone();
        wrong.forward_attempt = 2;
        assert!(data.write_capsule(&wrong, b"new").is_err());
        wrong = header.clone();
        wrong.fds.capsule += 10;
        assert!(data.write_capsule(&wrong, b"new").is_err());
        assert!(
            data.write_capsule(&header, &vec![0; MAX_PAYLOAD_BYTES + 1])
                .is_err()
        );
        assert_eq!(data.as_raw_fd(), original);
        assert_eq!(read_inherited(data.as_fd()).unwrap().payload, b"prior");
    }

    #[test]
    fn inherited_descriptor_refusals_do_not_consume_unknown_descriptors() {
        let (mut socket, mut peer) = UnixStream::pair().unwrap();
        set_fd_flags(
            socket.as_raw_fd(),
            fd_flags(socket.as_raw_fd()).unwrap() & !libc::FD_CLOEXEC,
        )
        .unwrap();
        assert!(read_inherited(socket.as_fd()).is_err());
        assert_ne!(fd_flags(socket.as_raw_fd()).unwrap() & libc::FD_CLOEXEC, 0);
        peer.write_all(b"x").unwrap();
        let mut byte = [0];
        socket.read_exact(&mut byte).unwrap();
        assert_eq!(byte, [b'x']);

        let mut data = AnonymousData::create().unwrap();
        let header = header(data.as_raw_fd());
        data.write_capsule(&header, b"valid").unwrap();
        data.file
            .set_permissions(std::fs::Permissions::from_mode(0o640))
            .unwrap();
        assert!(read_inherited(data.as_fd()).is_err());
        assert!(fd_flags(data.as_raw_fd()).is_ok());
        data.file
            .set_permissions(std::fs::Permissions::from_mode(0o600))
            .unwrap();
        assert_eq!(read_inherited(data.as_fd()).unwrap().payload, b"valid");
        let mut wrong = header;
        wrong.fds.capsule += 10;
        let frame = encode(&wrong, b"foreign role").unwrap();
        data.file.set_len(0).unwrap();
        data.file.write_all_at(&frame, 0).unwrap();
        assert!(read_inherited(data.as_fd()).is_err());
        assert!(fd_flags(data.as_raw_fd()).is_ok());
    }

    #[test]
    fn metadata_controls_reject_owner_links_access_and_size_before_reads() {
        let mut data = AnonymousData::create().unwrap();
        data.write_capsule(&header(data.as_raw_fd()), b"state")
            .unwrap();
        let stat = stat_fd(data.as_fd()).unwrap();
        let uid = stat.st_uid;
        assert!(check_data(&stat, libc::O_RDWR, true, uid).is_ok());
        assert!(check_data(&stat, libc::O_WRONLY, true, uid).is_err());
        assert!(check_data(&stat, libc::O_RDONLY, true, uid).is_err());
        assert!(check_data(&stat, libc::O_RDWR | libc::O_APPEND, true, uid).is_err());
        let mut wrong = stat;
        wrong.st_uid ^= 1;
        assert!(check_data(&wrong, libc::O_RDWR, true, uid).is_err());
        wrong = stat;
        wrong.st_nlink = 1;
        assert!(check_data(&wrong, libc::O_RDWR, true, uid).is_err());
        wrong = stat;
        wrong.st_size = -1;
        assert!(check_data(&wrong, libc::O_RDWR, true, uid).is_err());
        wrong = stat;
        wrong.st_size = MAX_FRAME_BYTES as libc::off_t + 1;
        assert!(check_data(&wrong, libc::O_RDWR, true, uid).is_err());
        wrong = stat;
        wrong.st_size = 0;
        assert!(check_data(&wrong, libc::O_RDWR, true, uid).is_err());
        assert!(check_data(&wrong, libc::O_RDWR, false, uid).is_ok());
    }
}
