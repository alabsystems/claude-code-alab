// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The checkout-local canonical tree check; upstream `tools/check-pin` also
//! binds its tree to the named tag. Archives cannot establish that comparison.

#[path = "support/pin.rs"]
mod pin;

#[test]
fn the_canonical_alab_harness_is_the_pinned_tree_unchanged() {
    pin::hold(&pin::checkout()).expect("the canonical copy and pin hold");
}

#[test]
fn pin_syntax_is_closed_and_canonical_even_without_git_metadata() {
    let good = "tag=v0.9.1\ntree=0123456789abcdef0123456789abcdef01234567\n";
    assert!(pin::fields(good).is_ok());
    for bad in [
        good.replace("tag=", "name="),
        good.replace("v0.9.1", "v00.9.1"),
        good.replace("v0.9.1", "v0.9"),
        good.replace("01234567", "0123456A"),
        good.replace("tree=", "tag=v0.9.1\ntree="),
        good.replace('\n', "\r\n"),
        good.trim_end().to_string(),
        format!("{good}extra=value\n"),
    ] {
        assert!(pin::fields(&bad).is_err(), "{bad:?}");
    }
}
