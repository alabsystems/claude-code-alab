// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

fn os(pairs: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
    pairs
        .iter()
        .map(|(key, value)| ((*key).into(), (*value).into()))
        .collect()
}

#[test]
fn an_account_puts_its_login_directory_in_and_every_credential_out() {
    let mut environment = os(&[
        ("PATH", "/bin"),
        ("CLAUDE_CONFIG_DIR", "/homes/a/.claude"),
        (LOGIN_ENV, "/homes/a/.claude-old"),
        ("ANTHROPIC_API_KEY", "a-key"),
        ("CLAUDE_CODE_OAUTH_TOKEN", "a-token"),
        ("ATERM_CLAUDE_TOKEN", "1"),
    ]);
    super::environment(
        &mut environment,
        &Account {
            key: "work".to_string(),
            home: Some("/homes/a/.claude-work".into()),
        },
    );
    assert_eq!(
        environment,
        os(&[
            ("CLAUDE_CONFIG_DIR", "/homes/a/.claude"),
            (LOGIN_ENV, "/homes/a/.claude-work"),
            ("PATH", "/bin"),
        ])
    );
    // The default login: the variable goes, and nothing names another.
    super::environment(
        &mut environment,
        &Account {
            key: "home".to_string(),
            home: None,
        },
    );
    assert_eq!(
        environment,
        os(&[("CLAUDE_CONFIG_DIR", "/homes/a/.claude"), ("PATH", "/bin")])
    );
    // Every credential aterm's helper refuses beside a label is named here.
    for key in CREDENTIAL_KEYS {
        let mut environment = os(&[(key, "x")]);
        super::environment(
            &mut environment,
            &Account {
                key: "work".to_string(),
                home: None,
            },
        );
        assert!(environment.is_empty(), "{key}");
    }
}
