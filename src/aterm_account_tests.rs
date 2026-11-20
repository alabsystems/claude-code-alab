// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The account words: the contract-4 claim that takes `accounts`, its
//! `login=`, and an account restart's `to=` and `home=`.

use super::*;

fn policy(contract: &str) -> Policy {
    Policy::parse(&format!(
        "OK gen=1 effective=on contract={contract} relaunch=true upgrade=true \
         upgrade_ask=false human_grace_s=120"
    ))
    .expect("a policy")
}

fn parse(tail: &str) -> Option<Restart> {
    Restart::parse(tail.split_ascii_whitespace()).ok()
}

#[test]
fn accounts_is_claimed_only_where_the_policy_serves_contract_four() {
    let duties = Duties::BUILT.scopes();
    // A host that serves contract 3 alone gets the claim it always had.
    assert_eq!(claim_scopes(&policy("2,3"), &duties), duties);
    let mut with_accounts = duties.clone();
    with_accounts.push(Scope::Accounts);
    assert_eq!(claim_scopes(&policy("2,3,4"), &duties), with_accounts);
    // The policy decides afresh: a claim made again on a host that stopped
    // serving 4 drops the word, and one that names it already is not doubled.
    assert_eq!(claim_scopes(&policy("3"), &with_accounts), duties);
    assert_eq!(claim_scopes(&policy("3,4"), &with_accounts), with_accounts);
    // `accounts` is never a duty a launch switches by itself.
    assert_eq!(Duties::parse("accounts"), None);
    assert_eq!(Duties::parse("relaunch,accounts"), None);
    assert!(!Duties::BUILT.takes(Scope::Accounts));
    assert!(!Scope::Accounts.types());
    assert_eq!(Scope::from_word("accounts"), Some(Scope::Accounts));
    assert!(!Scope::ALL.contains(&Scope::Accounts));
}

#[test]
fn a_claim_that_takes_accounts_is_made_under_contract_four_with_its_login() {
    let scopes = claim_scopes(&policy("2,3,4"), &[Scope::Relaunch, Scope::Upgrade]);
    let verb = claim_verb(
        "s-0000000000000000001a",
        77,
        &scopes,
        "0.2.0",
        Some("2.1.311"),
    );
    assert_eq!(
        verb.as_deref(),
        Some(
            "@s-0000000000000000001a meta set supervisor claude-code-alab@77 contract=4 \
             scope=relaunch,upgrade,accounts version=0.2.0 vendor=2.1.311"
        )
    );
    assert_eq!(
        with_login(verb.clone(), Some("work")).as_deref(),
        Some(
            "@s-0000000000000000001a meta set supervisor claude-code-alab@77 contract=4 \
             scope=relaunch,upgrade,accounts version=0.2.0 vendor=2.1.311 login=work"
        )
    );
    assert_eq!(with_login(verb.clone(), None), verb);
    // Control: the same duties without `accounts` stay a contract-3 claim.
    assert_eq!(
        claim_verb(
            "s-0000000000000000001a",
            77,
            &[Scope::Relaunch, Scope::Upgrade],
            "0.2.0",
            None
        )
        .as_deref(),
        Some(
            "@s-0000000000000000001a meta set supervisor claude-code-alab@77 contract=3 \
             scope=relaunch,upgrade version=0.2.0"
        )
    );
    // A label aterm would refuse sends no claim at all.
    let (long, longest) = ("a".repeat(65), "a".repeat(64));
    for bad in ["", "Work", "-work", "help", "a b", "w@rk", long.as_str()] {
        assert!(!account_label(bad), "{bad:?}");
        assert_eq!(with_login(verb.clone(), Some(bad)), None, "{bad:?}");
    }
    for good in ["work", "0", "a.b_c-d", longest.as_str()] {
        assert!(account_label(good), "{good:?}");
    }
    assert_eq!(with_login(None, Some("work")), None);
}

#[test]
fn an_account_restart_names_the_account_and_its_login_directory() {
    assert_eq!(
        parse("cause=account to=work home=%2Fhomes%2Fa%2F.claude-work"),
        Some(Restart {
            cause: "account".to_string(),
            model: None,
            account: Some(Account {
                key: "work".to_string(),
                home: Some("/homes/a/.claude-work".into()),
            }),
        })
    );
    // `-` is the machine's default login.
    assert_eq!(
        parse("cause=account-ahead to=home home=-").and_then(|restart| restart.account),
        Some(Account {
            key: "home".to_string(),
            home: None,
        })
    );
    // A model rides along unchanged.
    assert_eq!(
        parse("cause=account to=work home=- model=claude-opus-5-5")
            .and_then(|restart| restart.model)
            .as_deref(),
        Some("claude-opus-5-5")
    );
    // Any other cause passes the account words over.
    assert_eq!(
        parse("cause=asked to=work home=%2Fh").and_then(|restart| restart.account),
        None
    );
    assert!(parse("cause=asked to=Bad").is_some());
}

#[test]
fn an_account_key_is_ac_and_sixteen_lower_hex_digits_and_nothing_else() {
    for key in ["ac-0123456789abcdef", "ac-ffffffffffffffff"] {
        assert!(account_key(key), "{key:?}");
        // Every key is a word a restart's `to=` and a claim's `login=` carry.
        assert!(account_label(key), "{key:?}");
    }
    for word in [
        "",
        "work",
        "ac-",
        "ac-0123456789abcde",
        "ac-0123456789abcdef0",
        "ac-0123456789ABCDEF",
        "ac-0123456789abcdeg",
        "AC-0123456789abcdef",
        "ac_0123456789abcdef",
        " ac-0123456789abcdef",
        "ac-0123456789abcdef ",
    ] {
        assert!(!account_key(word), "{word:?}");
    }
    // A restart whose `to=` is no key is still read: the supervisor refuses
    // it by name (`account-word`), where an unread one would say nothing of
    // which account was meant.
    assert_eq!(
        parse("cause=account to=ac-0000000000000001 home=-").and_then(|restart| restart.account),
        Some(Account {
            key: "ac-0000000000000001".to_string(),
            home: None,
        })
    );
}

#[test]
fn an_account_move_whose_account_cannot_be_read_is_not_acted_on() {
    for tail in [
        "cause=account",
        "cause=account to=work",
        "cause=account home=-",
        "cause=account to=Work home=-",
        "cause=account to=work home=relative%2Fdir",
        "cause=account to=work home=%2Fa%00b",
        "cause=account to=work home=%zz",
        "cause=account to=work to=play home=-",
        "cause=account to=work home=- home=-",
        "cause=account-ahead to=help home=-",
        "cause=asked to=a to=b",
    ] {
        assert_eq!(parse(tail), None, "{tail:?}");
    }
    // Control: the same words well formed are read.
    assert!(parse("cause=account to=work home=%2Fa").is_some());
}

#[test]
fn a_restart_that_names_its_cause_but_not_its_words_is_unacted_not_unnamed() {
    let read = |tail: &str| Restart::parse(tail.split_ascii_whitespace());
    for (tail, cause, why) in [
        ("cause=account to=work", "account", "account"),
        ("cause=account home=-", "account", "account"),
        (
            "cause=account-ahead to=Work home=-",
            "account-ahead",
            "account",
        ),
        ("cause=account to=work home=relative", "account", "account"),
        ("cause=account to=work to=play home=-", "account", "to"),
        ("cause=account to=work home=- home=-", "account", "home"),
        ("cause=model model=-x", "model", "model"),
        ("cause=model model=a model=b", "model", "model"),
    ] {
        assert_eq!(
            read(tail),
            Err(Unacted::Words {
                cause: cause.to_string(),
                why
            }),
            "{tail:?}"
        );
    }
    // A restart nobody can name, wherever an unreadable word stands beside it.
    for tail in [
        "",
        "cause=",
        "cause=a/b",
        "to=work cause=account",
        "cause=model model=-x cause=stall",
        "cause=account to=work home=- home=- cause=asked",
    ] {
        assert_eq!(read(tail), Err(Unacted::Cause), "{tail:?}");
    }
    // Control: the same words well formed are read.
    assert!(read("cause=account to=work home=-").is_ok());
    assert!(read("cause=model model=opus").is_ok());
}
