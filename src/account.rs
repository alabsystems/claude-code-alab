// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The person's accounts, as this harness meets them (docs/DECISIONS.md 63).
//!
//! aterm keeps the accounts: their registry, their logins and their tokens,
//! and the choice of which account a session runs on and when it moves. This
//! harness keeps none of them, reads no token and picks no account. It
//! carries a move out:
//!
//! A claim that takes `accounts` ([`crate::aterm::Scope::Accounts`], contract
//! 4) is sent `restart cause=account to=<key> home=<dir|->`. At a safe point
//! of its own judging, and within the offer's bound, the child is ended and
//! started again on the same conversation with the account's login directory
//! ([`LOGIN_ENV`]) and without any credential variable ([`environment`]),
//! through aterm's token helper named the account's key
//! ([`crate::launch::vendor_command_as`]), which serves that account or
//! starts nothing. Claude Code's transcripts stay where they were
//! (`CLAUDE_CONFIG_DIR` is the session's, not the account's), so `--resume
//! <id>` finds the conversation under any account. Once the vendor runs, the
//! account is the session's: every later start of the harness's own (a
//! crash, an update, a model move) keeps its login directory and names no
//! account, and every claim says `login=<key>`. A start the helper would not
//! make leaves the session on the account it had.
//!
//! A usage limit that is the account's own is aterm's to see and to answer:
//! nothing is asked of aterm here, and no move is this harness's idea.
//! Fable's own weekly limit is not the account's: the model moves
//! ([`crate::limit`]).
//!
//! Nothing here performs I/O.

use std::ffi::{OsStr, OsString};

use crate::aterm::Account;

/// The variable Claude Code reads its login directory from: the keychain
/// entry it signs in with (aterm's `accounts::LOGIN_ENV`). Set to an
/// account's home, it runs on that account; unset, on the machine's default
/// login.
pub const LOGIN_ENV: &str = "CLAUDE_SECURESTORAGE_CONFIG_DIR";

/// Every variable that carries a credential or routes Claude Code around its
/// stored login (aterm's `accounts::CREDENTIAL_KEYS`), and the marks aterm's
/// token helper leaves on a child it served (`token_route::MARKER_ENV` and
/// `MINTED_ENV`). A launch moved onto an account carries none of them: the
/// helper refuses an account whose launch names its own credential, and
/// serves the account's token itself.
pub const CREDENTIAL_KEYS: [&str; 15] = [
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "CLAUDE_CODE_OAUTH_TOKEN",
    "CLAUDE_CODE_OAUTH_REFRESH_TOKEN",
    "CLAUDE_CODE_API_KEY_FILE_DESCRIPTOR",
    "CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR",
    "CLAUDE_CODE_GATEWAY_TOKEN_FILE_DESCRIPTOR",
    "CLAUDE_CODE_USE_ANTHROPIC_AWS",
    "CLAUDE_CODE_USE_ANTHROPIC_GOOGLE_CLOUD",
    "CLAUDE_CODE_USE_MANTLE",
    "CLAUDE_CODE_USE_GATEWAY",
    "CLAUDE_CODE_PROVIDER_MANAGED_BY_HOST",
    "CLAUDE_CODE_HOST_AUTH_ENV_VAR",
    "ATERM_CLAUDE_TOKEN",
    "ATERM_CLAUDE_TOKEN_MINTED",
];

/// Put `account` in a child's `environment`: its login directory in
/// [`LOGIN_ENV`] (or none, for the default login), and every
/// [`CREDENTIAL_KEYS`] variable out. Nothing else changes; the values taken
/// out are dropped, never read.
pub fn environment(environment: &mut Vec<(OsString, OsString)>, account: &Account) {
    environment.retain(|(key, _)| {
        key.as_os_str() != OsStr::new(LOGIN_ENV)
            && !CREDENTIAL_KEYS
                .iter()
                .any(|credential| key.as_os_str() == OsStr::new(credential))
    });
    if let Some(home) = &account.home {
        environment.push((LOGIN_ENV.into(), home.clone().into_os_string()));
    }
    environment.sort();
}

#[cfg(test)]
#[path = "account_tests.rs"]
mod tests;
