// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Closed read-only host policy exchange. No host process runs the vendor.

use std::ffi::OsString;
use std::io;
use std::io::Write as _;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::Path;

pub(super) const MAX_REPLY: usize = 320 * 1024;

pub(super) struct Request {
    cwd: String,
    vendor: String,
    session: String,
    terminal: bool,
}

pub(super) fn hex(path: &Path) -> io::Result<String> {
    let bytes = path.as_os_str().as_bytes();
    if !path.is_absolute() || bytes.len() > super::MAX_PATH || bytes.contains(&0) {
        return Err(super::invalid("invalid native gate path"));
    }
    use std::fmt::Write as _;
    let mut out = String::with_capacity(2 * bytes.len());
    for byte in bytes {
        write!(out, "{byte:02x}").expect("String write");
    }
    Ok(out)
}

impl Request {
    pub(super) fn new(
        cwd: &Path,
        vendor: &Path,
        session: &str,
        terminal: bool,
    ) -> io::Result<Self> {
        if session.is_empty() || session.len() > 64 || !safe(session) {
            return Err(super::invalid("invalid gate session"));
        }
        Ok(Self {
            cwd: hex(cwd)?,
            vendor: hex(vendor)?,
            session: session.into(),
            terminal,
        })
    }

    fn arguments(&self) -> Vec<OsString> {
        [
            "harness",
            "launch-trust",
            "gate",
            "--cwd-hex",
            &self.cwd,
            "--vendor-hex",
            &self.vendor,
            "--session",
            &self.session,
            "--stdout-tty",
            if self.terminal { "1" } else { "0" },
        ]
        .into_iter()
        .map(OsString::from)
        .collect()
    }

    pub(super) fn decode(&self, text: &str) -> Option<String> {
        use crate::json::Value;
        if text.len() > MAX_REPLY
            || !text.ends_with('\n')
            || text[..text.len() - 1].contains(['\n', '\r'])
        {
            return None;
        }
        let Value::Object(fields) = crate::json::parse(text)? else {
            return None;
        };
        let names = [
            "schema",
            "operation",
            "status",
            "cwd_hex",
            "vendor_hex",
            "session",
            "stdout_tty",
            "rule",
            "reason",
        ];
        if fields.len() != names.len() || names.iter().any(|k| !fields.contains_key(*k)) {
            return None;
        }
        let s = |key| fields.get(key)?.as_str();
        if fields.get("schema")?.as_u64() != Some(1)
            || s("operation")? != "folder-trust-gate"
            || s("cwd_hex")? != self.cwd
            || s("vendor_hex")? != self.vendor
            || s("session")? != self.session
            || fields.get("stdout_tty")?.as_bool() != Some(self.terminal)
        {
            return None;
        }
        let status = s("status")?;
        match status {
            "allow" => {
                let rule = s("rule")?;
                (self.terminal
                    && !rule.is_empty()
                    && rule.len() <= 4096
                    && safe(rule)
                    && fields.get("reason") == Some(&Value::Null))
                .then(|| rule.into())
            }
            "refused" | "unread" => {
                let reason = s("reason")?;
                if reason.is_empty()
                    || reason.len() > 4096
                    || !safe(reason)
                    || fields.get("rule") != Some(&Value::Null)
                {
                    return None;
                }
                None
            }
            _ => None,
        }
    }
}

fn safe(text: &str) -> bool {
    !text
        .chars()
        .any(|c| c.is_control() || ('\u{80}'..='\u{9f}').contains(&c))
}

pub(super) fn query(
    host: &Path,
    request: &Request,
    changes: &[(OsString, OsString)],
    records: Option<&Path>,
) -> Result<Option<String>, crate::sys::ProbeSpawnRefusal> {
    let arguments = request.arguments();
    let evidence = |name, bytes: &[u8]| -> io::Result<()> {
        if let Some(root) = records {
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(root.join(name))?
                .write_all(bytes)?;
        }
        Ok(())
    };
    let words = arguments
        .iter()
        .map(|word| super::quote(word.to_str().expect("gate argv is constructed UTF-8")))
        .collect::<Vec<_>>()
        .join(",");
    if evidence("gate-argv.json", format!("[{words}]\n").as_bytes()).is_err() {
        return Ok(None);
    }
    let reply = crate::launch::bounded_output_limit(
        host,
        &arguments,
        changes,
        std::time::Duration::from_secs(3),
        MAX_REPLY,
    )?;
    let Some(reply) = reply else { return Ok(None) };
    if evidence("gate-response.json", reply.as_bytes()).is_err() {
        return Ok(None);
    }
    Ok(request.decode(&reply))
}
