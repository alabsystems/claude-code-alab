// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `claude-alab`: the ALab harness for Claude Code ([`claude_code_alab`]).

fn main() {
    use std::io::Write as _;
    match claude_code_alab::cli::main() {
        Ok(never) => match never {},
        Err(retained) => {
            // This executable ends with every unsettled native resource still held.
            // The error is not a child-end receipt and no library terminates its caller.
            let retained = std::mem::ManuallyDrop::new(retained);
            let _ = writeln!(
                std::io::stderr(),
                "claude-alab: launch preparation custody remains unsettled: {}",
                retained.error
            );
            std::process::exit(70);
        }
    }
}
