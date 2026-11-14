// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Original-image local-provider qualification entry.

#[path = "support/image.rs"]
mod image;

fn main() {
    image::main(claude_code_alab::supervise::image::default_host);
}
