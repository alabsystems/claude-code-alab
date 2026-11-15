// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! End-to-end tests of `claude-alab` against a fake vendor `claude`.
//!
//! This test binary is its own fake vendor: each scenario lays it (by hard
//! link) into a private atpkg store under a private `HOME` as
//! `store/claude/<build>/bin/claude`, and when the harness runs it with
//! `FAKE_CLAUDE_DIR` set, `main` acts as the vendor instead of running tests.
//! The fake writes Claude Code's own session record (`sessions/<pid>.json`),
//! logs its start, arguments and signals, and takes commands from files the
//! test writes. Scenarios that need a terminal run the harness on a fresh pty
//! as its session leader, as a shell would run it in a terminal tab.
//!
//! The harness supervises only a launch it can move onto another build: a
//! terminal on stdin and a command line that can be resumed. Every scenario
//! about supervision (signals, the exit status, the journal's `start` line, an
//! update, the claim, a crash) therefore gives the harness a pty and such a
//! command line. A launch without a terminal, a one-shot, a command line that
//! cannot be resumed and `--alab-no-live-update` run the fake in the harness's
//! own process, and their scenarios check exactly that ([`assert_ran_in_place`]).
//!
//! The fake aterm control socket (`support/aterm.rs`) is part of this binary.
//! The `aterm_*` scenarios start the harness in its tab ([`start_in_aterm`]):
//! the claim before the first vendor start, the safe point on aterm's word, the
//! fallback to the harness's own readings, the relaunch of a crashed vendor and
//! the restart aterm asks for. Its self-tests, listed last, drive it through
//! this crate's own client with no harness process.
//!
//! For the crash scenarios the fake vendor takes three things beyond Claude
//! Code's own behaviour: the command `crash:<code>` (exit with that code and
//! leave the session record, as a crash does; `exit:<code>` removes it, as a
//! graceful exit does), the command `die:<signal>` (die by that signal and
//! leave the record; `raise:<signal>` removes it first), and the control file
//! `record-hold` (while it exists the fake writes no session record, as a
//! vendor that has not got that far yet).
//!
//! Tests run one at a time (`harness = false`): they share nothing, but
//! terminal and process-group signals are easier to read one scenario at a time.

use std::ffi::OsString;
use std::io::{Read as _, Write as _};
use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use claude_code_alab::cli::VERSION;

#[path = "support/child_start.rs"]
mod child_start;
#[path = "support/aterm.rs"]
mod fake_aterm;
#[path = "support/inspect.rs"]
mod inspect;
#[path = "support/launch_host.rs"]
mod launch_host;
#[path = "support/launch_query_entry.rs"]
mod launch_query_entry;
#[path = "support/prime.rs"]
mod prime;

const HARNESS: &str = env!("CARGO_BIN_EXE_claude-alab");
const TMP: &str = env!("CARGO_TARGET_TMPDIR");
const SID: &str = "0f0f0f0f-1111-2222-3333-444455556666";
const BUILD_1: u64 = 1_000_009_000_009_000_001;
const BUILD_2: u64 = 1_000_009_000_009_000_002;
const BUILD_3: u64 = 1_000_009_000_009_000_003;
static SCENARIO_SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn main() {
    launch_host::signal_parent_entry();
    launch_query_entry::entry();
    launch_host::entry();
    if let Some(dir) = std::env::var_os("FAKE_CLAUDE_DIR") {
        launch_host::record_signal_state(&PathBuf::from(&dir), "vendor-signals");
        fake::run(&PathBuf::from(dir));
    }
    // The controller is the only waiter; PTY reader threads never reap. Keep
    // terminal children pinned until that waiter consumes their event.
    // SAFETY: initialized native action owned by this private test controller.
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    action.sa_sigaction = libc::SIG_DFL;
    // SAFETY: initialize the action mask before installing this controller's policy.
    let empty = unsafe { libc::sigemptyset(&raw mut action.sa_mask) };
    assert_eq!(empty, 0);
    // SAFETY: no child has started; this clears inherited SIG_IGN/SA_NOCLDWAIT.
    let installed =
        unsafe { libc::sigaction(libc::SIGCHLD, &raw const action, std::ptr::null_mut()) };
    assert_eq!(installed, 0);
    let filter: Option<String> = std::env::args().skip(1).find(|a| !a.starts_with('-'));
    let tests: &[(&str, fn())] = &[
        ("defaults_inspection_is_read_only_explicit_and_keeps_native_entry_guards", inspect::is_read_only_explicit_and_keeps_native_entry_guards),
        ("defaults_inspection_reports_closed_bounded_unavailable_observations", inspect::reports_closed_bounded_unavailable_observations),
        (
            "legacy_cleanup_uses_the_same_guards_and_a_lossless_backup_receipt",
            prime::legacy_cleanup_uses_the_same_guards_and_a_lossless_backup_receipt,
        ),
        (
            "priming_retains_native_entry_guards_and_reports_only_its_closed_result",
            prime::retains_native_entry_guards_and_reports_only_its_closed_result,
        ),
        (
            "managed_usage_query_keeps_inherited_wait_policy_and_vendor_signal_state",
            launch_host::managed_usage_query_keeps_inherited_wait_policy_and_vendor_signal_state,
        ),
        (
            "managed_usage_query_is_bounded_in_the_actual_single_threaded_cli",
            launch_host::managed_usage_query_is_bounded_in_the_actual_single_threaded_cli,
        ),
        (
            "managed_usage_query_retains_a_foreign_native_wait_at_the_executable_boundary",
            launch_host::managed_usage_query_retains_a_foreign_native_wait_at_the_executable_boundary,
        ),
        (
            "aterm_child_start_precedes_every_vendor_body_and_keeps_bound_generation_on_reconnect",
            child_start::precedes_every_vendor_body_and_keeps_bound_generation_on_reconnect,
        ),
        (
            "aterm_child_start_uses_a_fresh_generation_after_an_unannounced_dead_attempt",
            child_start::uses_a_fresh_generation_after_an_unannounced_dead_attempt,
        ),
        (
            "aterm_a_refused_child_start_drops_authority_and_keeps_vendor_liveness",
            child_start::a_refused_child_start_drops_authority_and_keeps_vendor_liveness,
        ),
        (
            "managed_launch_owns_arguments_renderer_and_host_services",
            launch_host::managed_launch_owns_arguments_renderer_and_host_services,
        ),
        (
            "launch_only_executes_the_vendor_in_place_even_inside_a_rostered_aterm",
            launch_host::launch_only_executes_the_vendor_in_place_even_inside_a_rostered_aterm,
        ),
        (
            "renderer_honors_explicit_config_without_home",
            launch_host::renderer_honors_explicit_config_without_home,
        ),
        (
            "managed_child_proof_and_image_pinning_follow_the_token_exec",
            launch_host::managed_child_proof_and_image_pinning_follow_the_token_exec,
        ),
        (
            "managed_update_needs_no_installed_vendor",
            launch_host::managed_update_needs_no_installed_vendor,
        ),
        (
            "managed_token_route_survives_vendor_replacement",
            launch_host::managed_token_route_survives_vendor_replacement,
        ),
        (
            "managed_information_queries_never_invoke_host_services",
            launch_host::managed_information_queries_never_invoke_host_services,
        ),
        (
            "managed_option_values_stay_exact_and_keep_host_services",
            launch_host::managed_option_values_stay_exact_and_keep_host_services,
        ),
        (
            "managed_host_services_share_the_exact_no_harness_boundary",
            launch_host::managed_host_services_share_the_exact_no_harness_boundary,
        ),
        (
            "leading_word_metadata_opens_no_host_vendor_or_store_state",
            launch_host::leading_word_metadata_opens_no_host_vendor_or_store_state,
        ),
        (
            "version_names_the_harness_and_the_vendor",
            version_names_the_harness_and_the_vendor,
        ),
        (
            "version_never_asks_the_vendor",
            version_never_asks_the_vendor,
        ),
        (
            "version_says_when_the_vendor_is_not_installed",
            version_says_when_the_vendor_is_not_installed,
        ),
        (
            "refuses_when_the_vendor_is_missing",
            refuses_when_the_vendor_is_missing,
        ),
        (
            "refuses_a_build_atpkg_has_not_marked_ready",
            refuses_a_build_atpkg_has_not_marked_ready,
        ),
        ("refuses_to_exec_itself", refuses_to_exec_itself),
        (
            "a_copy_of_the_harness_as_the_vendor_refuses_at_once",
            a_copy_of_the_harness_as_the_vendor_refuses_at_once,
        ),
        (
            "passes_arguments_stdio_env_and_exit_status_through",
            passes_arguments_stdio_env_and_exit_status_through,
        ),
        (
            "the_twin_envelope_reaches_the_vendor_with_exactly_those_arguments",
            the_twin_envelope_reaches_the_vendor_with_exactly_those_arguments,
        ),
        (
            "no_harness_is_read_only_as_one_and_only_inside_aterm",
            no_harness_is_read_only_as_one_and_only_inside_aterm,
        ),
        (
            "no_harness_in_an_aterm_tab_says_nothing_to_aterm",
            no_harness_in_an_aterm_tab_says_nothing_to_aterm,
        ),
        (
            "the_twin_envelope_does_not_get_a_loop_past_the_guards",
            the_twin_envelope_does_not_get_a_loop_past_the_guards,
        ),
        (
            "int_and_quit_belong_to_the_child",
            int_and_quit_belong_to_the_child,
        ),
        (
            "term_is_passed_on_and_the_session_ends",
            term_is_passed_on_and_the_session_ends,
        ),
        (
            "hup_is_passed_on_and_the_session_ends",
            hup_is_passed_on_and_the_session_ends,
        ),
        (
            "a_child_killed_by_a_signal_kills_the_harness_the_same_way",
            a_child_killed_by_a_signal_kills_the_harness_the_same_way,
        ),
        (
            "the_harness_follows_the_child_into_a_stop",
            the_harness_follows_the_child_into_a_stop,
        ),
        (
            "a_tstp_sent_to_the_harness_is_followed",
            a_tstp_sent_to_the_harness_is_followed,
        ),
        (
            "a_continued_vendor_does_not_receive_a_second_cont",
            a_continued_vendor_does_not_receive_a_second_cont,
        ),
        (
            "a_harness_only_cont_continues_its_stopped_vendor",
            a_harness_only_cont_continues_its_stopped_vendor,
        ),
        (
            "relaunches_on_a_new_build_with_the_resume_argv",
            relaunches_on_a_new_build_with_the_resume_argv,
        ),
        (
            "selection_changes_after_term_do_not_replace_the_pinned_plan",
            selection_changes_after_term_do_not_replace_the_pinned_plan,
        ),
        (
            "never_relaunches_while_the_child_is_busy",
            never_relaunches_while_the_child_is_busy,
        ),
        (
            "never_relaunches_over_typed_input",
            never_relaunches_over_typed_input,
        ),
        (
            "never_relaunches_over_unread_typing",
            never_relaunches_over_unread_typing,
        ),
        (
            "without_a_terminal_the_vendor_runs_in_the_harness_place",
            without_a_terminal_the_vendor_runs_in_the_harness_place,
        ),
        (
            "a_one_shot_launch_runs_the_vendor_in_the_harness_place",
            a_one_shot_launch_runs_the_vendor_in_the_harness_place,
        ),
        (
            "an_unresumable_launch_runs_the_vendor_in_the_harness_place",
            an_unresumable_launch_runs_the_vendor_in_the_harness_place,
        ),
        (
            "non_utf8_arguments_run_the_vendor_in_the_harness_place",
            non_utf8_arguments_run_the_vendor_in_the_harness_place,
        ),
        (
            "no_live_update_runs_the_vendor_in_the_harness_place",
            no_live_update_runs_the_vendor_in_the_harness_place,
        ),
        (
            "a_killed_in_place_run_leaves_no_orphan",
            a_killed_in_place_run_leaves_no_orphan,
        ),
        (
            "an_in_place_run_that_leads_back_to_the_harness_stops_at_the_depth_limit",
            an_in_place_run_that_leads_back_to_the_harness_stops_at_the_depth_limit,
        ),
        (
            "a_copy_of_the_harness_run_in_place_refuses_as_itself",
            a_copy_of_the_harness_run_in_place_refuses_as_itself,
        ),
        (
            "version_never_runs_a_store_build_that_is_the_harness",
            version_never_runs_a_store_build_that_is_the_harness,
        ),
        (
            "a_wrapper_that_runs_the_harness_again_is_bounded",
            a_wrapper_that_runs_the_harness_again_is_bounded,
        ),
        (
            "an_update_waits_for_background_work",
            an_update_waits_for_background_work,
        ),
        (
            "an_abandoned_update_is_never_signalled_again",
            an_abandoned_update_is_never_signalled_again,
        ),
        (
            "a_relaunch_resumes_the_conversations_permission_mode",
            a_relaunch_resumes_the_conversations_permission_mode,
        ),
        (
            "a_relaunch_preserves_option_values_that_spell_permission_flags",
            a_relaunch_preserves_option_values_that_spell_permission_flags,
        ),
        (
            "the_journal_names_the_childs_model_effort_and_key",
            the_journal_names_the_childs_model_effort_and_key,
        ),
        (
            "a_broken_stderr_does_not_lose_the_session",
            a_broken_stderr_does_not_lose_the_session,
        ),
        (
            "a_relaunch_fallback_marks_the_target_abandoned",
            a_relaunch_fallback_marks_the_target_abandoned,
        ),
        (
            "a_move_onto_a_build_that_refuses_to_start_falls_back_and_abandons_it",
            a_move_onto_a_build_that_refuses_to_start_falls_back_and_abandons_it,
        ),
        (
            "a_move_onto_a_build_that_crashes_before_taking_the_terminal_falls_back",
            a_move_onto_a_build_that_crashes_before_taking_the_terminal_falls_back,
        ),
        (
            "a_moved_build_that_took_the_terminal_is_not_fallen_back",
            a_moved_build_that_took_the_terminal_is_not_fallen_back,
        ),
        (
            "a_signal_death_restores_the_terminal_modes",
            a_signal_death_restores_the_terminal_modes,
        ),
        (
            "the_vendor_always_runs_with_the_updater_and_survey_off",
            the_vendor_always_runs_with_the_updater_and_survey_off,
        ),
        (
            "a_fallback_that_cannot_restart_the_previous_build_claims_no_resume",
            a_fallback_that_cannot_restart_the_previous_build_claims_no_resume,
        ),
        (
            "a_failed_start_whose_fallback_is_refused_claims_no_resume",
            a_failed_start_whose_fallback_is_refused_claims_no_resume,
        ),
        (
            "a_failed_signal_setup_over_a_broken_stderr_is_a_refusal_not_a_panic",
            a_failed_signal_setup_over_a_broken_stderr_is_a_refusal_not_a_panic,
        ),
        (
            "inside_aterm_a_new_harness_build_is_held_and_never_probed",
            inside_aterm_a_new_harness_build_is_held_and_never_probed,
        ),
        (
            "launch_refusals_are_journalled_and_version_and_help_are_not",
            launch_refusals_are_journalled_and_version_and_help_are_not,
        ),
        (
            "a_private_image_word_after_a_vendor_word_is_the_vendors",
            a_private_image_word_after_a_vendor_word_is_the_vendors,
        ),
        (
            "a_malformed_adoption_without_a_child_is_refused",
            a_malformed_adoption_without_a_child_is_refused,
        ),
        (
            "a_malformed_adoption_follows_its_one_child",
            a_malformed_adoption_follows_its_one_child,
        ),
        (
            "a_malformed_adoption_follows_the_last_of_several_children",
            a_malformed_adoption_follows_the_last_of_several_children,
        ),
        (
            "aterm_native_child_target_follows_the_first_start_restarts_and_vendor_moves",
            aterm_native_child_target_follows_the_first_start_restarts_and_vendor_moves,
        ),
        (
            "aterm_claim_precedes_the_first_vendor_start",
            aterm_claim_precedes_the_first_vendor_start,
        ),
        (
            "aterm_policy_off_leaves_the_session_unclaimed",
            aterm_policy_off_leaves_the_session_unclaimed,
        ),
        (
            "aterm_without_contract_3_leaves_the_session_unclaimed",
            aterm_without_contract_3_leaves_the_session_unclaimed,
        ),
        (
            "aterm_without_the_roster_row_is_never_sent_a_claim",
            aterm_without_the_roster_row_is_never_sent_a_claim,
        ),
        (
            "aterm_claim_refused_busy_leaves_the_session_unclaimed",
            aterm_claim_refused_busy_leaves_the_session_unclaimed,
        ),
        (
            "aterm_claim_not_proven_is_released",
            aterm_claim_not_proven_is_released,
        ),
        (
            "aterm_moves_on_aterms_word_over_what_the_tracker_calls_a_draft",
            aterm_moves_on_aterms_word_over_what_the_tracker_calls_a_draft,
        ),
        (
            "aterm_holds_on_each_field_until_it_clears",
            aterm_holds_on_each_field_until_it_clears,
        ),
        (
            "aterm_claim_keeps_the_record_and_background_work_checks",
            aterm_claim_keeps_the_record_and_background_work_checks,
        ),
        (
            "aterm_restart_falls_back_reclaims_and_reannounces_the_vendor",
            aterm_restart_falls_back_reclaims_and_reannounces_the_vendor,
        ),
        (
            "aterm_event_wakes_the_look_inside_a_long_poll",
            aterm_event_wakes_the_look_inside_a_long_poll,
        ),
        (
            "aterm_policy_turned_off_releases_the_claim",
            aterm_policy_turned_off_releases_the_claim,
        ),
        (
            "aterm_a_crash_is_relaunched_on_the_same_build_after_the_ladders_pause",
            aterm_a_crash_is_relaunched_on_the_same_build_after_the_ladders_pause,
        ),
        (
            "aterm_an_exit_of_zero_is_not_a_crash",
            aterm_an_exit_of_zero_is_not_a_crash,
        ),
        (
            "aterm_an_exit_that_removed_its_record_is_not_a_crash",
            aterm_an_exit_that_removed_its_record_is_not_a_crash,
        ),
        (
            "aterm_a_record_removed_while_settling_is_not_a_crash",
            aterm_a_record_removed_while_settling_is_not_a_crash,
        ),
        (
            "aterm_a_crash_is_passed_on_when_the_policy_forbids_a_relaunch",
            aterm_a_crash_is_passed_on_when_the_policy_forbids_a_relaunch,
        ),
        (
            "aterm_a_crash_is_passed_on_under_a_hold",
            aterm_a_crash_is_passed_on_under_a_hold,
        ),
        (
            "aterm_a_crash_is_passed_on_under_a_drivers_hand",
            aterm_a_crash_is_passed_on_under_a_drivers_hand,
        ),
        (
            "aterm_a_crash_is_passed_on_when_a_person_just_typed",
            aterm_a_crash_is_passed_on_when_a_person_just_typed,
        ),
        (
            "aterm_a_crash_is_passed_on_when_the_claim_is_no_longer_proven",
            aterm_a_crash_is_passed_on_when_the_claim_is_no_longer_proven,
        ),
        (
            "aterm_a_crash_after_the_claim_was_released_is_passed_on",
            aterm_a_crash_after_the_claim_was_released_is_passed_on,
        ),
        (
            "aterm_a_crash_with_no_claim_ends_the_session_as_it_does_outside_aterm",
            aterm_a_crash_with_no_claim_ends_the_session_as_it_does_outside_aterm,
        ),
        (
            "aterm_a_signal_during_the_crash_pause_ends_the_harness_as_the_child_ended",
            aterm_a_signal_during_the_crash_pause_ends_the_harness_as_the_child_ended,
        ),
        (
            "aterm_a_crash_with_the_link_lost_waits_for_the_claim_to_be_proven_again",
            aterm_a_crash_with_the_link_lost_waits_for_the_claim_to_be_proven_again,
        ),
        (
            "aterm_a_crash_whose_claim_is_not_proven_again_is_passed_on",
            aterm_a_crash_whose_claim_is_not_proven_again_is_passed_on,
        ),
        (
            "aterm_a_restart_request_ends_the_child_once_and_resumes_on_the_same_build",
            aterm_a_restart_request_ends_the_child_once_and_resumes_on_the_same_build,
        ),
        (
            "aterm_a_restart_the_child_outlives_is_made_with_sigkill",
            aterm_a_restart_the_child_outlives_is_made_with_sigkill,
        ),
        (
            "aterm_a_restart_grace_does_not_run_while_the_job_is_stopped",
            aterm_a_restart_grace_does_not_run_while_the_job_is_stopped,
        ),
        (
            "aterm_a_restart_request_waits_for_the_session_record",
            aterm_a_restart_request_waits_for_the_session_record,
        ),
        (
            "aterm_a_restart_request_is_ignored_under_a_drivers_hand_or_without_the_claim",
            aterm_a_restart_request_is_ignored_under_a_drivers_hand_or_without_the_claim,
        ),
        (
            "aterm_a_restart_request_is_made_whatever_the_relaunch_switch_says",
            aterm_a_restart_request_is_made_whatever_the_relaunch_switch_says,
        ),
        (
            "aterm_a_restart_aterm_cannot_answer_yet_waits_for_its_answer",
            aterm_a_restart_aterm_cannot_answer_yet_waits_for_its_answer,
        ),
        (
            "aterm_a_crash_waits_while_aterm_cannot_answer",
            aterm_a_crash_waits_while_aterm_cannot_answer,
        ),
        (
            "aterm_a_launch_aterm_cannot_answer_yet_is_claimed_once_it_can",
            aterm_a_launch_aterm_cannot_answer_yet_is_claimed_once_it_can,
        ),
        (
            "aterm_a_launch_aterm_did_not_answer_in_time_is_claimed_once_it_does",
            aterm_a_launch_aterm_did_not_answer_in_time_is_claimed_once_it_does,
        ),
        (
            "aterm_a_launch_whose_subscription_aterm_refuses_is_not_claimed_again",
            aterm_a_launch_whose_subscription_aterm_refuses_is_not_claimed_again,
        ),
        (
            "aterm_a_launch_whose_subscription_lanes_are_full_is_claimed_once_one_frees",
            aterm_a_launch_whose_subscription_lanes_are_full_is_claimed_once_one_frees,
        ),
        (
            "aterm_a_reclaim_whose_subscription_aterm_refuses_ends_the_link",
            aterm_a_reclaim_whose_subscription_aterm_refuses_ends_the_link,
        ),
        (
            "aterm_a_halt_holds_the_move_and_the_claim",
            aterm_a_halt_holds_the_move_and_the_claim,
        ),
        (
            "aterm_a_policy_back_on_without_an_event_is_claimed_again",
            aterm_a_policy_back_on_without_an_event_is_claimed_again,
        ),
        (
            "aterm_a_hold_stands_over_a_reply_that_comes_too_late",
            aterm_a_hold_stands_over_a_reply_that_comes_too_late,
        ),
        (
            "aterm_a_crash_gives_the_terminal_back_while_it_waits_for_aterm",
            aterm_a_crash_gives_the_terminal_back_while_it_waits_for_aterm,
        ),
        (
            "aterm_typing_at_a_dead_screen_reaches_no_relaunched_child",
            aterm_typing_at_a_dead_screen_reaches_no_relaunched_child,
        ),
        (
            "aterm_a_signal_during_the_launch_claim_ends_the_harness_before_any_start",
            aterm_a_signal_during_the_launch_claim_ends_the_harness_before_any_start,
        ),
        (
            "aterm_a_signal_at_the_end_of_the_crash_pause_ends_the_harness",
            aterm_a_signal_at_the_end_of_the_crash_pause_ends_the_harness,
        ),
        (
            "aterm_a_policy_turned_off_quietly_is_released_at_a_look_and_claimed_again",
            aterm_a_policy_turned_off_quietly_is_released_at_a_look_and_claimed_again,
        ),
        (
            "aterm_a_claim_made_again_after_a_look_released_it_is_journalled",
            aterm_a_claim_made_again_after_a_look_released_it_is_journalled,
        ),
        (
            "aterm_a_restart_asked_with_autopilot_off_is_made",
            aterm_a_restart_asked_with_autopilot_off_is_made,
        ),
        (
            "aterm_a_launch_with_autopilot_off_is_claimed_once_it_is_on",
            aterm_a_launch_with_autopilot_off_is_claimed_once_it_is_on,
        ),
        (
            "aterm_a_launch_under_a_halt_is_claimed_once_the_halt_lifts",
            aterm_a_launch_under_a_halt_is_claimed_once_the_halt_lifts,
        ),
        (
            "aterm_a_hold_on_a_lost_link_gives_way_to_aterms_refusal",
            aterm_a_hold_on_a_lost_link_gives_way_to_aterms_refusal,
        ),
        (
            "aterm_a_policy_event_keeps_the_claim_through_a_halt_and_a_busy_aterm",
            aterm_a_policy_event_keeps_the_claim_through_a_halt_and_a_busy_aterm,
        ),
        (
            "aterm_a_restart_sent_for_the_child_an_update_replaced_is_ignored",
            aterm_a_restart_sent_for_the_child_an_update_replaced_is_ignored,
        ),
        (
            "aterm_ctrl_z_during_the_launch_claim_stops_the_job",
            aterm_ctrl_z_during_the_launch_claim_stops_the_job,
        ),
        (
            "aterm_a_restart_whose_relaunch_cannot_start_gives_the_keys_back",
            aterm_a_restart_whose_relaunch_cannot_start_gives_the_keys_back,
        ),
        (
            "aterm_a_crash_of_a_background_job_is_passed_on",
            aterm_a_crash_of_a_background_job_is_passed_on,
        ),
        (
            "aterm_a_crash_whose_build_was_taken_away_in_the_pause_is_passed_on",
            aterm_a_crash_whose_build_was_taken_away_in_the_pause_is_passed_on,
        ),
        (
            "the_fake_aterm_is_found_and_authenticates_this_crates_client",
            fake_aterm::the_fake_aterm_is_found_and_authenticates_this_crates_client,
        ),
        (
            "the_fake_aterms_policy_reads_as_a_policy",
            fake_aterm::the_fake_aterms_policy_reads_as_a_policy,
        ),
        (
            "the_fake_aterms_info_reply_reads_through_the_roster_guard",
            fake_aterm::the_fake_aterms_info_reply_reads_through_the_roster_guard,
        ),
        (
            "a_claim_on_the_fake_aterm_reads_back_proven_in_status",
            fake_aterm::a_claim_on_the_fake_aterm_reads_back_proven_in_status,
        ),
        (
            "a_refused_or_contended_claim_on_the_fake_aterm_stores_nothing",
            fake_aterm::a_refused_or_contended_claim_on_the_fake_aterm_stores_nothing,
        ),
        (
            "closing_the_claims_connection_releases_it_on_the_fake_aterm",
            fake_aterm::closing_the_claims_connection_releases_it_on_the_fake_aterm,
        ),
        (
            "events_from_the_fake_aterm_reach_this_crates_reader",
            fake_aterm::events_from_the_fake_aterm_reach_this_crates_reader,
        ),
        (
            "a_restart_event_from_the_fake_aterm_parses",
            fake_aterm::a_restart_event_from_the_fake_aterm_parses,
        ),
        (
            "restarting_the_fake_aterm_drops_every_connection_and_the_claim",
            fake_aterm::restarting_the_fake_aterm_drops_every_connection_and_the_claim,
        ),
    ];
    let mut failed = Vec::new();
    let mut ran = 0;
    for (name, test) in tests {
        if filter.as_deref().is_some_and(|f| !name.contains(f)) {
            continue;
        }
        ran += 1;
        print!("test {name} ... ");
        let _ = std::io::stdout().flush();
        let started = Instant::now();
        match std::panic::catch_unwind(*test) {
            Ok(()) => println!("ok ({} ms)", started.elapsed().as_millis()),
            Err(_) => {
                println!("FAILED");
                failed.push(*name);
            }
        }
    }
    println!(
        "\ntest result: {}. {} passed; {} failed",
        if failed.is_empty() { "ok" } else { "FAILED" },
        ran - failed.len(),
        failed.len()
    );
    if !failed.is_empty() {
        for name in &failed {
            println!("    {name}");
        }
        std::process::exit(101);
    }
}

// ───────────────────────────── scenario plumbing ─────────────────────────────

fn prefix_rel() -> &'static str {
    if cfg!(target_os = "macos") {
        "Library/Application Support/aterm/pkg"
    } else {
        ".local/share/aterm/pkg"
    }
}

/// A private world: a home with an atpkg store, a fake's control directory, a
/// working directory, and the harness's state directory.
struct Scenario {
    root: PathBuf,
    home: PathBuf,
    fake: PathBuf,
    work: PathBuf,
    state: PathBuf,
    store: PathBuf,
    /// Exact owned group leaders, whose non-consuming wait must still succeed
    /// before cleanup signals a group. A reaped PID is never a retained owner.
    groups: Mutex<Vec<claude_code_alab::procs::ChildIdentity>>,
}

impl Scenario {
    fn new(name: &str) -> Self {
        let parent = Path::new(TMP).join(".ubd/scenarios.noindex");
        std::fs::create_dir_all(&parent).expect("scenario parent");
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("scenario clock")
            .as_nanos();
        let sequence = SCENARIO_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let root = parent.join(format!(
            "{name}-{}-{stamp}-{sequence}.noindex",
            std::process::id()
        ));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&root)
            .expect("fresh exclusive scenario");
        eprintln!("preserved scenario: {}", root.display());
        let home = root.join("home");
        let store = home.join(prefix_rel()).join("store").join("claude");
        let s = Self {
            fake: root.join("fake"),
            work: root.join("work"),
            state: root.join("state"),
            home,
            store,
            root,
            groups: Mutex::new(Vec::new()),
        };
        for dir in [
            &s.fake,
            &s.work,
            &s.state,
            &s.store,
            &s.home.join(".claude/sessions"),
        ] {
            std::fs::create_dir_all(dir).expect("scenario dir");
        }
        s
    }

    /// Lay a vendor build: this test binary as `bin/claude`, marked ready.
    fn add_build(&self, number: u64, version: Option<&str>) {
        let exe = std::env::current_exe().expect("test exe");
        self.add_build_of(number, version, &exe);
    }

    fn add_build_of(&self, number: u64, version: Option<&str>, exe: &Path) {
        let bin = self.store.join(number.to_string()).join("bin");
        std::fs::create_dir_all(&bin).expect("bin dir");
        let dest = bin.join("claude");
        if std::fs::hard_link(exe, &dest).is_err() {
            std::fs::copy(exe, &dest).expect("copy exe");
        }
        std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        let platform = format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS);
        std::fs::write(
            self.store.join(format!("{number}.ready")),
            format!("ok\nplatform={platform}\ncontents=bin:claude\n"),
        )
        .expect("ready");
        if let Some(v) = version {
            std::fs::write(
                self.store.join(format!("{number}.vendor")),
                format!("schema = 1\nprogram = \"claude\"\nversion = \"{v}\"\nvendor = \"Fake\"\n"),
            )
            .expect("vendor");
        }
        std::fs::write(
            self.store.join(format!("{number}.shim-env")),
            format!("FAKE_SHIM_BUILD={number}\n"),
        )
        .expect("shim-env");
    }

    /// Lay a vendor build whose `bin/claude` is a shell script (`body` after the
    /// counter preamble): each run adds one to `<root>/count`, and past 30 runs
    /// the script stops by itself, so a recursion the harness fails to stop
    /// still ends.
    fn add_build_script(&self, number: u64, body: &str) {
        let bin = self.store.join(number.to_string()).join("bin");
        std::fs::create_dir_all(&bin).expect("bin dir");
        let dest = bin.join("claude");
        let _ = std::fs::remove_file(&dest);
        std::fs::write(
            &dest,
            format!(
                "#!/bin/sh\nn=$(cat \"$ALAB_TEST_COUNT\" 2>/dev/null || echo 0)\nn=$((n+1))\n\
                 echo \"$n\" > \"$ALAB_TEST_COUNT\"\nif [ \"$n\" -gt 30 ]; then exit 99; fi\n{body}\n"
            ),
        )
        .expect("script");
        std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        let platform = format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS);
        std::fs::write(
            self.store.join(format!("{number}.ready")),
            format!("ok\nplatform={platform}\ncontents=bin:claude\n"),
        )
        .expect("ready");
    }

    /// How many times a script build ([`Scenario::add_build_script`]) ran.
    fn script_runs(&self) -> u32 {
        std::fs::read_to_string(self.root.join("count"))
            .ok()
            .and_then(|t| t.trim().parse().ok())
            .unwrap_or(0)
    }

    /// Point `current` at `number`, atomically, as atpkg flips it.
    fn set_current(&self, number: u64) {
        let tmp = self.store.join("current.tmp");
        let _ = std::fs::remove_file(&tmp);
        std::os::unix::fs::symlink(self.store.join(number.to_string()), &tmp).expect("symlink");
        std::fs::rename(&tmp, self.store.join("current")).expect("flip current");
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(HARNESS);
        cmd.args(args)
            .current_dir(&self.work)
            .env("HOME", &self.home)
            .env("XDG_STATE_HOME", &self.state)
            .env("FAKE_CLAUDE_DIR", &self.fake)
            .env("ALAB_TEST_COUNT", self.root.join("count"))
            .env("ALAB_TEST_HARNESS", HARNESS)
            .env_remove("CLAUDE_CONFIG_DIR")
            // A suite run from an aterm tab must never locate that tab's own
            // session; a scenario that wants an aterm names the fake's.
            .env_remove("ATERM_PARENT_SESSION_ID")
            // Nor may it inherit that tab's `claude --no-harness`.
            .env_remove("ATERM_NO_HARNESS")
            .env_remove("ATERM_HARNESS_HOST")
            .env_remove("ALAB_TEST_HOST_IMAGE")
            .env_remove("CLAUDE_ALAB_PID")
            .env_remove("CLAUDE_ALAB_DEPTH");
        cmd
    }

    /// Write a fake control file atomically.
    fn put(&self, name: &str, text: &str) {
        let tmp = self.fake.join(format!(".{name}.tmp"));
        std::fs::write(&tmp, text).expect("write control file");
        std::fs::rename(&tmp, self.fake.join(name)).expect("rename control file");
    }

    fn log(&self) -> Vec<String> {
        std::fs::read_to_string(self.fake.join("log"))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn starts(&self) -> Vec<FakeStart> {
        self.log()
            .iter()
            .filter_map(|l| FakeStart::parse(l))
            .collect()
    }

    fn journal(&self) -> String {
        std::fs::read_to_string(self.state.join("claude-code-alab/journal.log")).unwrap_or_default()
    }

    fn guard_group(&self, pgid: i32) {
        let leader = u32::try_from(pgid)
            .ok()
            .and_then(claude_code_alab::procs::capture_child)
            .filter(|leader| leader.pgid == leader.pid);
        if let Some(leader) = leader {
            self.groups.lock().expect("groups").push(leader);
        } else {
            // No numeric-only fallback: an unread or already-ended leader is
            // not proof that this process group still belongs to the scenario.
            eprintln!("cleanup retains unknown group ownership pid={pgid}; world preserved");
        }
    }

    fn with_owned_groups(&self, mut visit: impl FnMut(&claude_code_alab::procs::ChildIdentity)) {
        let groups = self
            .groups
            .lock()
            .map(|groups| groups.clone())
            .unwrap_or_default();
        for leader in &groups {
            if claude_code_alab::procs::observe_child(leader).is_some() {
                visit(leader);
            }
        }
    }

    fn report(&self) -> String {
        format!(
            "\n--- scenario {} ---\n--- fake log ---\n{}\n--- journal ---\n{}",
            self.root.display(),
            self.log().join("\n"),
            self.journal()
        )
    }
}

impl Drop for Scenario {
    fn drop(&mut self) {
        self.with_owned_groups(|leader| {
            // SAFETY: capture required group==PID. Fresh birth/parent and
            // WNOWAIT ownership prove this same unreaped child still pins
            // that group number; only this controller consumes its events.
            unsafe { libc::kill(-(leader.pid as i32), libc::SIGKILL) };
        });
    }
}

/// One `start` line of the fake's log.
#[derive(Debug)]
struct FakeStart {
    pid: i32,
    exe: String,
    args: Vec<String>,
    shim: String,
}

impl FakeStart {
    fn parse(line: &str) -> Option<Self> {
        let rest = line.strip_prefix("start\t")?;
        let mut f = rest.split('\t');
        let pid = f.next()?.parse().ok()?;
        let exe = f.next()?.to_string();
        let shim = f.next()?.to_string();
        let args = f.next()?;
        let args = if args.is_empty() {
            Vec::new()
        } else {
            args.split('\u{1f}').map(str::to_string).collect()
        };
        Some(Self {
            pid,
            exe,
            args,
            shim,
        })
    }

    fn build(&self) -> Option<u64> {
        let bin = Path::new(&self.exe).parent()?;
        bin.parent()?.file_name()?.to_str()?.parse().ok()
    }
}

fn wait_until(limit: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + limit;
    while Instant::now() < end {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    cond()
}

/// What `waitpid` says about the harness.
#[derive(Debug, PartialEq, Eq)]
enum Status {
    Exited(i32),
    Signaled(i32),
    Stopped,
    Continued,
}

fn wait_status(pid: i32, limit: Duration) -> Option<Status> {
    let end = Instant::now() + limit;
    loop {
        let mut status = 0;
        // SAFETY: `status` is a valid out-parameter; `pid` is this test's own child.
        let rc = unsafe {
            libc::waitpid(
                pid,
                &raw mut status,
                libc::WNOHANG | libc::WUNTRACED | libc::WCONTINUED,
            )
        };
        if rc == pid {
            return Some(if libc::WIFEXITED(status) {
                Status::Exited(libc::WEXITSTATUS(status))
            } else if libc::WIFSIGNALED(status) {
                Status::Signaled(libc::WTERMSIG(status))
            } else if libc::WIFSTOPPED(status) {
                Status::Stopped
            } else {
                Status::Continued
            });
        }
        if Instant::now() >= end {
            return None;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn signal(pid: i32, sig: i32) {
    // SAFETY: a plain signal to a process (or, negative, a group) this test created.
    let rc = unsafe { libc::kill(pid, sig) };
    assert_eq!(rc, 0, "kill({pid}, {sig})");
}

fn child_pid(child: &std::process::Child) -> i32 {
    i32::try_from(child.id()).expect("pid")
}

/// Reap `child` by its pid ([`wait_status`]), within `limit`: how it ended,
/// or `None` while it still runs (its group is the scenario's to kill).
fn reap(child: std::process::Child, limit: Duration) -> Option<Status> {
    wait_status(child_pid(&child), limit)
}

/// A pty: the master stays with the test, the slave becomes the harness's terminal.
struct Pty {
    master: OwnedFd,
    slave: OwnedFd,
    out: Arc<Mutex<Vec<u8>>>,
}

impl Pty {
    fn open() -> Self {
        // SAFETY: plain pty allocation calls; every returned descriptor is checked
        // and owned exactly once below; `ptsname`'s buffer is copied at once.
        unsafe {
            let m = libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY);
            assert!(m >= 0, "posix_openpt");
            assert_eq!(libc::grantpt(m), 0, "grantpt");
            assert_eq!(libc::unlockpt(m), 0, "unlockpt");
            let name = libc::ptsname(m);
            assert!(!name.is_null(), "ptsname");
            let name = std::ffi::CStr::from_ptr(name).to_owned();
            let s = libc::open(name.as_ptr(), libc::O_RDWR | libc::O_NOCTTY);
            assert!(s >= 0, "open slave");
            let master = OwnedFd::from_raw_fd(m);
            let slave = OwnedFd::from_raw_fd(s);
            let out = Arc::new(Mutex::new(Vec::new()));
            let reader = master.try_clone().expect("clone master");
            let sink = Arc::clone(&out);
            std::thread::spawn(move || {
                let mut f = std::fs::File::from(reader);
                let mut buf = [0u8; 4096];
                while let Ok(n) = f.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    sink.lock().expect("sink").extend_from_slice(&buf[..n]);
                }
            });
            Self { master, slave, out }
        }
    }

    /// Run `cmd` on the slave as its session leader with the pty as its terminal.
    fn spawn(&self, cmd: Command) -> std::process::Child {
        let err = Stdio::from(self.slave.try_clone().expect("clone slave"));
        self.spawn_with_stderr(cmd, err)
    }

    /// [`Pty::spawn`] with `stderr` in place of the terminal on fd 2.
    fn spawn_with_stderr(&self, mut cmd: Command, stderr: Stdio) -> std::process::Child {
        let fd = |p: &Self| Stdio::from(p.slave.try_clone().expect("clone slave"));
        cmd.stdin(fd(self)).stdout(fd(self)).stderr(stderr);
        // SAFETY: `setsid` and `ioctl(TIOCSCTTY)` are async-signal-safe and touch
        // only the child between fork and exec.
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        cmd.spawn().expect("spawn harness on pty")
    }

    fn type_text(&self, text: &str) {
        let mut f = std::fs::File::from(self.master.try_clone().expect("clone master"));
        f.write_all(text.as_bytes()).expect("type");
    }

    fn output(&self) -> String {
        String::from_utf8_lossy(&self.out.lock().expect("out")).into_owned()
    }

    /// Whether the terminal is in canonical (`ICANON`) mode now; `None` when
    /// its modes cannot be read. Read through the master: on macOS (BSD) the
    /// kernel revokes the slave's descriptors when the harness, the session
    /// leader, exits (Linux leaves a pty's slave open), and on both the master
    /// reports the terminal's modes as the last process left them.
    fn icanon(&self) -> Option<bool> {
        // SAFETY: a zeroed termios is a valid out-parameter, and `master` is an
        // open pty descriptor this `Pty` owns.
        unsafe {
            let mut t: libc::termios = std::mem::zeroed();
            (libc::tcgetattr(self.master.as_raw_fd(), &raw mut t) == 0)
                .then_some(t.c_lflag & libc::ICANON != 0)
        }
    }
}

// ───────────────────────────────── the tests ─────────────────────────────────

fn version_names_the_harness_and_the_vendor() {
    let s = Scenario::new("version");
    s.add_build(BUILD_1, Some("9.9.1"));
    s.set_current(BUILD_1);
    let out = s.command(&["--version"]).output().expect("run");
    assert!(out.status.success(), "{out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!("claude-code-alab {VERSION} (claude 9.9.1)\n")
    );
    assert!(
        s.starts().is_empty(),
        "the provenance record answers; the vendor is not run"
    );
}

/// `claude-code-alab <version> (claude <vendor>)`, as `--version` prints it.
fn version_line(vendor: &str) -> String {
    format!("claude-code-alab {VERSION} (claude {vendor})\n")
}

/// The version is the `.vendor` record's, and only where it names its build: a
/// record that is missing, garbage or names another version says `unknown`,
/// never the version the build number encodes and never the vendor's own answer
/// (the fake would answer `9.9.7`, and logs every `--version` asked of it).
fn version_never_asks_the_vendor() {
    let s = Scenario::new("version-unknown");
    s.add_build(BUILD_1, Some("9.9.1"));
    s.set_current(BUILD_1);
    let version = |s: &Scenario| {
        let out = s
            .command(&["-v"])
            .env("FAKE_VERSION", "9.9.7")
            .output()
            .expect("run");
        assert!(out.status.success(), "{out:?}");
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    assert_eq!(version(&s), version_line("9.9.1"));
    let record = s.store.join(format!("{BUILD_1}.vendor"));
    for (why, text) in [
        ("garbage", Some("not a record\n")),
        ("no version", Some("schema = 1\nprogram = \"claude\"\n")),
        ("another version", Some("version = \"9.9.2\"\n")),
        ("not canonical", Some("version = \"9.9.01\"\n")),
        ("missing", None),
    ] {
        match text {
            Some(text) => std::fs::write(&record, text).expect("record"),
            None => std::fs::remove_file(&record).expect("remove record"),
        }
        assert_eq!(version(&s), version_line("unknown"), "{why}");
    }
    // A build atpkg has not marked ready is not the build a run would start.
    std::fs::write(&record, "version = \"9.9.1\"\n").expect("record");
    assert_eq!(version(&s), version_line("9.9.1"));
    std::fs::write(s.store.join(format!("{BUILD_1}.ready")), "pending\n").expect("ready");
    assert_eq!(version(&s), version_line("unknown"), "not ready");
    // No home to find the store under: the harness's own version is still known.
    let out = s
        .command(&["--version"])
        .env("HOME", "relative/home")
        .output()
        .expect("run");
    assert!(out.status.success(), "{out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        version_line("unknown"),
        "a relative HOME"
    );
    assert!(
        s.starts().is_empty(),
        "the vendor never started{}",
        s.report()
    );
    assert!(
        !s.log().iter().any(|l| l == "version"),
        "--version was asked of the vendor{}",
        s.report()
    );
}

fn version_says_when_the_vendor_is_not_installed() {
    let s = Scenario::new("version-absent");
    let out = s.command(&["--version"]).output().expect("run");
    assert!(out.status.success(), "{out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        version_line("not installed")
    );
}

fn refuses_when_the_vendor_is_missing() {
    let s = Scenario::new("missing");
    let out = s.command(&["--model", "x"]).output().expect("run");
    assert_eq!(out.status.code(), Some(127), "{out:?}");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("not installed by atpkg"), "{err}");
    assert!(err.contains("aterm pkg install claude"), "{err}");
    assert!(s.starts().is_empty());
    assert!(
        s.journal()
            .contains(" refused vendor-not-installed the vendor CLI is not installed by atpkg"),
        "{}",
        s.report()
    );
}

fn refuses_a_build_atpkg_has_not_marked_ready() {
    let s = Scenario::new("not-ready");
    s.add_build(BUILD_1, Some("9.9.1"));
    std::fs::remove_file(s.store.join(format!("{BUILD_1}.ready"))).expect("unready");
    s.set_current(BUILD_1);
    let out = s.command(&[]).output().expect("run");
    assert_eq!(out.status.code(), Some(127), "{out:?}");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("not usable") && err.contains(".ready is missing"),
        "{err}"
    );
    assert!(s.starts().is_empty());
    let journal = s.journal();
    assert!(
        journal.contains(" refused vendor-unusable the atpkg build ")
            && journal.contains(".ready is missing"),
        "{}",
        s.report()
    );
}

fn refuses_to_exec_itself() {
    let s = Scenario::new("self");
    s.add_build_of(BUILD_1, Some("9.9.1"), Path::new(HARNESS));
    s.set_current(BUILD_1);
    let out = s.command(&[]).output().expect("run");
    assert_eq!(out.status.code(), Some(127), "{out:?}");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("it is this harness itself"), "{err}");
    assert!(
        s.journal()
            .contains(" refused vendor-is-self refusing to run "),
        "{}",
        s.report()
    );
}

/// Run the harness command `cmd` to its end on a fresh pty, as its session
/// leader, with its stderr in a file: how it ended, and what it and everything
/// it started wrote to stderr. For a launch that starts no fake vendor.
fn run_on_pty(s: &Scenario, cmd: Command, limit: Duration) -> (Option<Status>, String) {
    let pty = Pty::open();
    let said = s.root.join("stderr");
    let stderr = Stdio::from(std::fs::File::create(&said).expect("stderr file"));
    let child = pty.spawn_with_stderr(cmd, stderr);
    s.guard_group(child_pid(&child));
    let status = reap(child, limit);
    (status, text_of(&said))
}

/// What a launch that ran the vendor in the harness's own place must show.
/// The fake's start number `nth` (from 0) is the process `pid` the test
/// started as the harness, so nothing stands between the launcher and the
/// vendor. That start carries no harness pid and is `depth` levels deep, with
/// the build's shim environment. The journal has one line of that process,
/// `pass <reason> vendor-build=… vendor=…`, and no supervisor ever started
/// there.
fn assert_ran_in_place(s: &Scenario, nth: usize, pid: i32, reason: &str, depth: u32) {
    assert!(
        wait_until(Duration::from_secs(10), || marks(s).len() > nth),
        "start {nth} never logged its marks{}",
        s.report()
    );
    let start = &s.starts()[nth];
    assert_eq!(
        start.pid,
        pid,
        "the vendor took the harness's place{}",
        s.report()
    );
    assert_eq!(start.build(), Some(BUILD_1), "{}", s.report());
    assert_eq!(start.shim, format!("{BUILD_1}"), "the build's shim env");
    assert_eq!(
        marks(s)[nth],
        format!("marks\tCLAUDE_ALAB_PID=\tCLAUDE_ALAB_DEPTH={depth}"),
        "no harness pid, one level deeper{}",
        s.report()
    );
    let journal = s.journal();
    let own: Vec<&str> = journal
        .lines()
        .filter(|line| line.contains(&format!(" pid={pid} ")))
        .collect();
    assert_eq!(
        own.len(),
        1,
        "one journal line of that process{}",
        s.report()
    );
    assert!(
        own[0].ends_with(&format!(
            " pid={pid} pass {reason} vendor-build={BUILD_1} vendor=9.9.1"
        )),
        "{}",
        s.report()
    );
}

/// The harness starts a copy of itself that atpkg's store names as the vendor
/// (a session it supervises: a terminal and a command line it can resume). The
/// copy finds its parent's pid in the pid mark and refuses at once.
fn a_copy_of_the_harness_as_the_vendor_refuses_at_once() {
    let s = Scenario::new("self-copy");
    let copy = s.root.join("claude-alab-copy");
    std::fs::copy(HARNESS, &copy).expect("copy the harness");
    s.add_build_of(BUILD_1, Some("9.9.1"), &copy);
    s.set_current(BUILD_1);
    let (status, err) = run_on_pty(&s, s.command(&[]), Duration::from_secs(20));
    assert_eq!(status, Some(Status::Exited(127)), "{err}{}", s.report());
    assert!(err.contains("started as the vendor"), "{err}");
    let starts = s.journal().matches(" start harness=").count();
    assert_eq!(starts, 1, "exactly one harness level ran{}", s.report());
    // The copy's own refusal is a journal line of its own pid, naming its parent.
    let parent = s
        .journal()
        .lines()
        .find(|l| l.contains(" start harness="))
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|w| w.strip_prefix("pid="))
        .map(str::to_string)
        .expect("the outer harness's pid");
    assert!(
        s.journal().lines().any(|l| l
            .ends_with(&format!(" refused started-as-vendor parent={parent}"))
            && !l.contains(&format!(" pid={parent} "))),
        "{}",
        s.report()
    );
    assert!(!s.journal().contains(" pass "), "{}", s.report());
}

/// The same store without a terminal: the harness runs the vendor in its own
/// place, so the copy is not its child and no pid mark names anyone. The copy
/// is then a harness whose own executable is the store's `claude`, which the
/// same-file check refuses. One level, no supervisor, exit 127.
fn a_copy_of_the_harness_run_in_place_refuses_as_itself() {
    let s = Scenario::new("self-copy-in-place");
    let copy = s.root.join("claude-alab-copy");
    std::fs::copy(HARNESS, &copy).expect("copy the harness");
    s.add_build_of(BUILD_1, Some("9.9.1"), &copy);
    s.set_current(BUILD_1);
    let out = s.command(&[]).output().expect("run");
    assert_eq!(out.status.code(), Some(127), "{out:?}{}", s.report());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("it is this harness itself"), "{err}");
    let journal = s.journal();
    let events: Vec<&str> = journal
        .lines()
        .filter_map(|line| line.splitn(3, ' ').nth(2))
        .collect();
    assert_eq!(events.len(), 2, "{}", s.report());
    assert_eq!(
        events[0],
        format!("pass no-tty vendor-build={BUILD_1} vendor=9.9.1"),
        "{}",
        s.report()
    );
    assert!(
        events[1].starts_with("refused vendor-is-self refusing to run "),
        "{}",
        s.report()
    );
    // Both lines are one process's: the copy ran in the harness's place.
    let pids: Vec<&str> = journal
        .lines()
        .filter_map(|line| line.split(' ').nth(1))
        .collect();
    assert_eq!(pids[0], pids[1], "{}", s.report());
}

/// Without a terminal the vendor runs in the harness's own process: every
/// argument, the three standard streams, the build's environment and the exit
/// status are the vendor's own, with nothing in between.
fn passes_arguments_stdio_env_and_exit_status_through() {
    let s = Scenario::new("passthrough");
    s.add_build(BUILD_1, Some("9.9.1"));
    s.set_current(BUILD_1);
    s.put("mode", "echo:7");
    let args = ["--model", "x y", "", "ünï", "--alab-not-mine", "--", "-p"];
    let mut child = s
        .command(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn");
    let pid = child_pid(&child);
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(b"hello\n")
        .expect("feed stdin");
    let out = child.wait_with_output().expect("wait");
    assert_eq!(out.status.code(), Some(7), "{out:?}{}", s.report());
    assert_eq!(String::from_utf8_lossy(&out.stdout), "fake-out:hello\n");
    assert_eq!(String::from_utf8_lossy(&out.stderr), "fake-err\n");
    let starts = s.starts();
    assert_eq!(starts.len(), 1, "{}", s.report());
    assert_eq!(starts[0].args, args, "every argument, unchanged");
    assert_eq!(starts[0].build(), Some(BUILD_1));
    assert_eq!(
        starts[0].shim,
        format!("{BUILD_1}"),
        "the build's shim env is exported"
    );
    assert_ran_in_place(&s, 0, pid, "no-tty", 1);
    assert!(!s.journal().contains(" start harness="), "{}", s.report());
}

/// Start the harness in its own process group on an interactive fake and wait
/// for the fake to start as its child. The harness is reaped with `waitpid`
/// ([`wait_status`]), which also sees stops.
///
/// Its stdin is `pty`'s slave, which makes this a launch the harness
/// supervises; with no terminal there the fake would run in its place. The
/// pty is not the harness's controlling terminal and the harness leads no
/// session: its group stays in the test's own session, as a shell's job does,
/// so a stop signal's default action applies to it (the kernel discards
/// `SIGTSTP` in a session leader's group whose parent is in another session).
/// No update is made in this shape (the harness is no terminal's foreground
/// group); the scenarios that move a session use [`start_on_pty`].
fn start_grouped(s: &Scenario, pty: &Pty) -> (std::process::Child, i32) {
    s.add_build(BUILD_1, Some("9.9.1"));
    s.set_current(BUILD_1);
    let mut cmd = s.command(&["--alab-poll-ms=40"]);
    cmd.stdin(Stdio::from(pty.slave.try_clone().expect("clone slave")))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0);
    let child = cmd.spawn().expect("spawn");
    let pid = child_pid(&child);
    s.guard_group(pid);
    assert!(
        wait_until(Duration::from_secs(10), || !s.starts().is_empty()),
        "the fake never started{}",
        s.report()
    );
    assert_ne!(
        s.starts()[0].pid,
        pid,
        "the fake is the harness's child{}",
        s.report()
    );
    assert!(
        s.journal().contains(&format!(
            " pid={pid} start harness={VERSION} vendor-build={BUILD_1} vendor=9.9.1\n"
        )),
        "{}",
        s.report()
    );
    (child, pid)
}

fn int_and_quit_belong_to_the_child() {
    let s = Scenario::new("int");
    let pty = Pty::open();
    let (_child, pid) = start_grouped(&s, &pty);
    await_record(&s, 0);
    // INT and QUIT sent to the harness alone are dropped: neither it nor the child acts.
    signal(pid, libc::SIGINT);
    signal(pid, libc::SIGQUIT);
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(
        wait_status(pid, Duration::ZERO),
        None,
        "the harness survives INT/QUIT"
    );
    assert!(
        !s.log().iter().any(|l| l.starts_with("signal")),
        "{}",
        s.report()
    );
    // The terminal's INT goes to the whole group: the child acts on it, the harness
    // survives it and ends the way the child ended.
    signal(-pid, libc::SIGINT);
    assert_eq!(
        wait_status(pid, Duration::from_secs(10)),
        Some(Status::Exited(42)),
        "{}",
        s.report()
    );
    assert!(s.log().iter().any(|l| l == "signal INT"), "{}", s.report());
}

fn passed_on(sig: i32, word: &str, code: i32, name: &str) {
    let s = Scenario::new(name);
    let pty = Pty::open();
    let (_child, pid) = start_grouped(&s, &pty);
    // The early start receipt precedes handler setup. The interactive record
    // proves the fake can answer the forwarded signal with its own exit code.
    await_record(&s, 0);
    let mut owned = Vec::new();
    s.with_owned_groups(|leader| owned.push(leader.pid));
    assert_eq!(owned, [pid as u32], "a live owned leader permits cleanup");
    signal(pid, sig);
    assert_eq!(
        wait_status(pid, Duration::from_secs(10)),
        Some(Status::Exited(code)),
        "{}",
        s.report()
    );
    s.with_owned_groups(|_| panic!("a reaped leader cannot authorize a group signal"));
    assert!(
        s.log().iter().any(|l| l == &format!("signal {word}")),
        "{}",
        s.report()
    );
    assert_eq!(s.starts().len(), 1, "no relaunch after a passed-on signal");
    // The harness passed it on and then mirrored the child's exit.
    let journal = s.journal();
    assert!(
        journal.contains(&format!(" pid={pid} signal passed-on {sig}\n"))
            && journal.contains(&format!(" pid={pid} exit child=Exited({code})\n")),
        "{}",
        s.report()
    );
}

fn term_is_passed_on_and_the_session_ends() {
    passed_on(libc::SIGTERM, "TERM", 70, "term");
}

fn hup_is_passed_on_and_the_session_ends() {
    passed_on(libc::SIGHUP, "HUP", 71, "hup");
}

fn a_child_killed_by_a_signal_kills_the_harness_the_same_way() {
    let s = Scenario::new("killed");
    let pty = Pty::open();
    let (_child, pid) = start_grouped(&s, &pty);
    s.put("command", &format!("raise:{}", libc::SIGUSR1));
    assert_eq!(
        wait_status(pid, Duration::from_secs(10)),
        Some(Status::Signaled(libc::SIGUSR1)),
        "{}",
        s.report()
    );
}

fn the_harness_follows_the_child_into_a_stop() {
    let s = Scenario::new("stop");
    let pty = Pty::open();
    let (_child, pid) = start_grouped(&s, &pty);
    // The child suspends itself (as a TUI does on ^Z in raw mode).
    s.put("command", "stop");
    assert_eq!(
        wait_status(pid, Duration::from_secs(10)),
        Some(Status::Stopped),
        "the harness stops with its child{}",
        s.report()
    );
    // `fg`: the shell continues the job's group.
    signal(-pid, libc::SIGCONT);
    assert_eq!(
        wait_status(pid, Duration::from_secs(10)),
        Some(Status::Continued)
    );
    s.put("command", "exit:3");
    assert_eq!(
        wait_status(pid, Duration::from_secs(10)),
        Some(Status::Exited(3)),
        "{}",
        s.report()
    );
}

fn a_tstp_sent_to_the_harness_is_followed() {
    let s = Scenario::new("tstp");
    let pty = Pty::open();
    let (_child, pid) = start_grouped(&s, &pty);
    signal(pid, libc::SIGTSTP);
    assert_eq!(
        wait_status(pid, Duration::from_secs(10)),
        Some(Status::Stopped),
        "the harness passes TSTP on and stops with the child{}",
        s.report()
    );
    signal(-pid, libc::SIGCONT);
    assert_eq!(
        wait_status(pid, Duration::from_secs(10)),
        Some(Status::Continued)
    );
    s.put("command", "exit:4");
    assert_eq!(
        wait_status(pid, Duration::from_secs(10)),
        Some(Status::Exited(4)),
        "{}",
        s.report()
    );
}

fn a_continued_vendor_does_not_receive_a_second_cont() {
    let s = Scenario::new("cont-once");
    s.put("mode", "cont-count");
    let pty = Pty::open();
    let (_child, pid) = start_grouped(&s, &pty);
    let vendor = s.starts()[0].pid;
    s.put("command", "stop");
    assert_eq!(
        wait_status(pid, Duration::from_secs(60)),
        Some(Status::Stopped)
    );
    // A group CONT can schedule the vendor before its harness. Make that
    // ordering explicit: the first handler ran before continuing the harness.
    signal(vendor, libc::SIGCONT);
    assert!(wait_until(Duration::from_secs(60), || s
        .log()
        .iter()
        .any(|line| line == "cont-count 1")));
    assert_eq!(pty.icanon(), Some(false));
    signal(pid, libc::SIGCONT);
    assert_eq!(
        wait_status(pid, Duration::from_secs(60)),
        Some(Status::Continued)
    );
    assert!(wait_until(Duration::from_secs(60), || s
        .journal()
        .contains(" continued\n")));
    s.put("command", "cont-fence");
    assert!(wait_until(Duration::from_secs(60), || s
        .log()
        .iter()
        .any(|line| line.starts_with("cont-fence "))));
    assert!(
        s.log().iter().any(|line| line == "cont-fence 1"),
        "one native continuation must run one vendor handler{}",
        s.report()
    );
    assert_eq!(
        pty.icanon(),
        Some(false),
        "no second handler changes the vendor's modes"
    );
    s.put("command", "exit:4");
    assert_eq!(
        wait_status(pid, Duration::from_secs(60)),
        Some(Status::Exited(4))
    );
}

fn a_harness_only_cont_continues_its_stopped_vendor() {
    let s = Scenario::new("cont-harness");
    let pty = Pty::open();
    let (_child, pid) = start_grouped(&s, &pty);
    s.put("command", "stop");
    assert_eq!(
        wait_status(pid, Duration::from_secs(60)),
        Some(Status::Stopped)
    );
    signal(pid, libc::SIGCONT);
    assert_eq!(
        wait_status(pid, Duration::from_secs(60)),
        Some(Status::Continued)
    );
    assert!(wait_until(Duration::from_secs(60), || s
        .log()
        .iter()
        .any(|line| line == "continued")));
    s.put("command", "exit:4");
    assert_eq!(
        wait_status(pid, Duration::from_secs(60)),
        Some(Status::Exited(4))
    );
}

/// Start the harness on a pty with two builds laid, the first current. The
/// harness is reaped with `waitpid` ([`wait_status`]), which also sees stops.
fn start_on_pty(s: &Scenario, pty: &Pty, extra: &[&str]) -> (std::process::Child, i32) {
    start_on_pty_with(s, pty, extra, None)
}

/// [`start_on_pty`], with `stderr` (when given) in place of the terminal on fd 2.
fn start_on_pty_with(
    s: &Scenario,
    pty: &Pty,
    extra: &[&str],
    stderr: Option<Stdio>,
) -> (std::process::Child, i32) {
    s.add_build(BUILD_1, Some("9.9.1"));
    s.add_build(BUILD_2, Some("9.9.2"));
    s.set_current(BUILD_1);
    let mut args = vec!["--alab-quiet-ms=600", "--alab-poll-ms=40"];
    args.extend_from_slice(extra);
    spawn_on_pty(s, pty, s.command(&args), stderr)
}

/// Run the harness command `cmd` on `pty` with its process group guarded, and
/// return at once.
fn spawn_guarded(s: &Scenario, pty: &Pty, cmd: Command) -> (std::process::Child, i32) {
    let child = pty.spawn(cmd);
    let pid = child_pid(&child);
    s.guard_group(pid);
    (child, pid)
}

/// Run the harness command `cmd` on `pty` (with `stderr`, when given, in place
/// of the terminal on fd 2) and wait for the fake to start.
fn spawn_on_pty(
    s: &Scenario,
    pty: &Pty,
    cmd: Command,
    stderr: Option<Stdio>,
) -> (std::process::Child, i32) {
    let (child, pid) = match stderr {
        Some(err) => {
            let child = pty.spawn_with_stderr(cmd, err);
            let pid = child_pid(&child);
            s.guard_group(pid);
            (child, pid)
        }
        None => spawn_guarded(s, pty, cmd),
    };
    assert!(
        wait_until(Duration::from_secs(10), || !s.starts().is_empty()),
        "the fake never started{}\n--- pty ---\n{}",
        s.report(),
        pty.output()
    );
    (child, pid)
}

fn relaunches_on_a_new_build_with_the_resume_argv() {
    let s = Scenario::new("relaunch");
    let pty = Pty::open();
    let (_child, pid) = start_on_pty(
        &s,
        &pty,
        &[
            "--model",
            "opus",
            "--dangerously-skip-permissions",
            "first prompt",
        ],
    );
    let first = s.starts().remove(0);
    assert_eq!(first.build(), Some(BUILD_1));
    // atpkg installs a newer vendor build.
    s.set_current(BUILD_2);
    assert!(
        wait_until(Duration::from_secs(15), || s.starts().len() >= 2),
        "no relaunch{}\n--- pty ---\n{}",
        s.report(),
        pty.output()
    );
    let log = s.log();
    let term_at = log
        .iter()
        .position(|l| l == "signal TERM")
        .expect("the old child got SIGTERM");
    let second_at = log
        .iter()
        .rposition(|l| l.starts_with("start\t"))
        .expect("second start");
    assert!(term_at < second_at, "ended before relaunched{}", s.report());
    let second = s.starts().remove(1);
    assert_eq!(second.build(), Some(BUILD_2), "{}", s.report());
    assert_eq!(
        second.args,
        [
            "--model",
            "opus",
            "--dangerously-skip-permissions",
            "--resume",
            SID
        ],
        "the launch flags, without the first prompt, then the resume pair"
    );
    assert_eq!(
        second.shim,
        format!("{BUILD_2}"),
        "the new build's shim env"
    );
    assert_ne!(second.pid, first.pid);
    // The first child left the terminal raw (it never restores); the harness put
    // back the modes it found before the relaunch, so the new build finds them.
    assert!(
        wait_until(Duration::from_secs(5), || s
            .log()
            .iter()
            .filter(|l| l.starts_with("modes "))
            .count()
            >= 2),
        "{}",
        s.report()
    );
    let modes_now: Vec<String> = s
        .log()
        .into_iter()
        .filter(|l| l.starts_with("modes "))
        .collect();
    assert_eq!(
        modes_now,
        ["modes icanon=1", "modes icanon=1"],
        "each child starts on the terminal's own (cooked) modes{}",
        s.report()
    );
    // The harness never exited: the same process still runs, now on build 2.
    assert_eq!(
        wait_status(pid, Duration::ZERO),
        None,
        "the harness is still running"
    );
    let journal = s.journal();
    for word in ["update-seen", "end-for-update", "relaunch child="] {
        assert!(journal.contains(word), "journal lacks {word}{}", s.report());
    }
    assert!(
        wait_until(Duration::from_secs(5), || pty
            .output()
            .contains("9.9.1 -> 9.9.2")),
        "the relaunch notice\n--- pty ---\n{}",
        pty.output()
    );
    assert_every_start_is_clean(&s, pid);
    // The resumed session ends on its own: the harness ends with it.
    s.put("command", "exit:5");
    assert_eq!(
        wait_status(pid, Duration::from_secs(10)),
        Some(Status::Exited(5)),
        "{}",
        s.report()
    );
}

fn selection_changes_after_term_do_not_replace_the_pinned_plan() {
    let s = Scenario::new("pinned-plan");
    s.put("term", "ignore");
    let pty = Pty::open();
    let (_child, pid) = start_on_pty(&s, &pty, &["--model", "opus", "first prompt"]);
    s.add_build(BUILD_3, Some("9.9.3"));
    s.set_current(BUILD_2);
    assert!(
        wait_until(Duration::from_secs(60), || s
            .log()
            .iter()
            .any(|line| line == "signal TERM")),
        "{}",
        s.report()
    );
    // The original child still owns the terminal. Change selection and the
    // target's sidecar after the first effect, then let that exact child end.
    s.set_current(BUILD_3);
    std::fs::write(
        s.store.join(format!("{BUILD_2}.shim-env")),
        "FAKE_SHIM_BUILD=changed-after-term\n",
    )
    .expect("change sidecar");
    s.put("status", "busy");
    s.put("command", "exit:0");
    assert!(
        wait_until(Duration::from_secs(60), || s.starts().len() >= 2),
        "{}",
        s.report()
    );
    let second = &s.starts()[1];
    assert_eq!(second.build(), Some(BUILD_2), "{}", s.report());
    assert_eq!(
        second.shim,
        BUILD_2.to_string(),
        "the pre-signal environment was pinned"
    );
    assert_eq!(second.args, ["--model", "opus", "--resume", SID]);
    s.put("command", "exit:0");
    assert_eq!(
        wait_status(pid, Duration::from_secs(60)),
        Some(Status::Exited(0)),
        "{}",
        s.report()
    );
}

fn never_relaunches_while_the_child_is_busy() {
    let s = Scenario::new("busy");
    s.put("status", "busy");
    let pty = Pty::open();
    let (_child, pid) = start_on_pty(&s, &pty, &[]);
    let ready = s.store.join(format!("{BUILD_2}.ready"));
    let valid_ready = std::fs::read(&ready).expect("target ready");
    std::fs::write(&ready, "unfinished\n").expect("unfinished target");
    s.set_current(BUILD_2);
    assert!(
        wait_until(Duration::from_secs(60), || s
            .journal()
            .contains("wait busy")),
        "the busy gate did not hold before inspecting the pending build{}",
        s.report()
    );
    assert!(
        !s.journal().contains("update-unusable"),
        "the busy session needlessly inspected the pending build{}",
        s.report()
    );
    assert_eq!(s.starts().len(), 1, "relaunched a busy child{}", s.report());
    assert!(
        !s.log().iter().any(|l| l == "signal TERM"),
        "{}",
        s.report()
    );
    assert!(s.journal().contains("wait busy"), "{}", s.report());
    assert_eq!(wait_status(pid, Duration::ZERO), None);
    // The turn ends: only now inspect the pending build, and refuse it until
    // its ready marker admits it. No metadata read before the hold may be cached.
    s.put("status", "idle");
    assert!(
        wait_until(Duration::from_secs(60), || s
            .journal()
            .contains("update-unusable")),
        "the target was not validated once the busy gate cleared{}",
        s.report()
    );
    assert!(
        !s.log().iter().any(|l| l == "signal TERM"),
        "the unfinished target ended the running child{}",
        s.report()
    );
    std::fs::write(&ready, valid_ready).expect("finish target");
    assert!(
        wait_until(Duration::from_secs(60), || s.starts().len() >= 2),
        "no relaunch once idle{}",
        s.report()
    );
    assert_eq!(s.starts()[1].build(), Some(BUILD_2));
    assert_eq!(s.starts()[1].args, ["--resume", SID]);
    s.put("command", "exit:0");
    assert_eq!(
        wait_status(pid, Duration::from_secs(10)),
        Some(Status::Exited(0)),
        "{}",
        s.report()
    );
}

fn never_relaunches_over_unread_typing() {
    let s = Scenario::new("unread");
    let pty = Pty::open();
    let (_child, pid) = start_on_pty(&s, &pty, &[]);
    // The vendor stops reading its terminal and a person types: the keystrokes
    // wait in the input queue, where no input clock sees them.
    s.put("command", "deaf");
    assert!(
        wait_until(Duration::from_secs(5), || s
            .log()
            .iter()
            .any(|l| l == "deaf")),
        "the fake never stopped reading{}",
        s.report()
    );
    pty.type_text("queued keystrokes");
    s.set_current(BUILD_2);
    std::thread::sleep(Duration::from_millis(2500));
    assert_eq!(
        s.starts().len(),
        1,
        "relaunched over unread typing{}",
        s.report()
    );
    assert!(
        !s.log().iter().any(|l| l.starts_with("read ")),
        "the fake read while deaf{}",
        s.report()
    );
    assert!(s.journal().contains("wait unread-input"), "{}", s.report());
    // Once the vendor reads it, the same input is a draft and still holds.
    s.put("command", "hear");
    assert!(
        wait_until(Duration::from_secs(5), || s
            .log()
            .iter()
            .any(|l| l.starts_with("read "))),
        "the fake never read the queued typing{}",
        s.report()
    );
    std::thread::sleep(Duration::from_millis(1500));
    assert_eq!(s.starts().len(), 1, "relaunched over a draft{}", s.report());
    s.put("command", "exit:0");
    assert_eq!(
        wait_status(pid, Duration::from_secs(10)),
        Some(Status::Exited(0)),
        "{}",
        s.report()
    );
}

fn never_relaunches_over_typed_input() {
    let s = Scenario::new("typed");
    let pty = Pty::open();
    let (_child, pid) = start_on_pty(&s, &pty, &[]);
    // A person types into the composer and does not send it.
    std::thread::sleep(Duration::from_millis(200));
    pty.type_text("half a thought");
    assert!(
        wait_until(Duration::from_secs(5), || s
            .log()
            .iter()
            .any(|l| l.starts_with("read "))),
        "the fake never read the typing{}",
        s.report()
    );
    s.set_current(BUILD_2);
    std::thread::sleep(Duration::from_millis(2500));
    assert_eq!(s.starts().len(), 1, "relaunched over a draft{}", s.report());
    let journal = s.journal();
    assert!(
        journal.contains("wait draft") || journal.contains("wait typing"),
        "{}",
        s.report()
    );
    // The person ends the session while the update is still pending: no relaunch.
    s.put("command", "exit:0");
    assert_eq!(
        wait_status(pid, Duration::from_secs(10)),
        Some(Status::Exited(0)),
        "{}",
        s.report()
    );
    assert_eq!(s.starts().len(), 1);
}

// ───────────── launches the harness can never move: the vendor alone ─────────────

/// Where a launch that runs the vendor in the harness's place is started.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Stdin {
    /// A fresh pty, the harness its session leader ([`Pty::spawn`]).
    Terminal,
    /// No terminal: `/dev/null`, the harness in its own process group.
    Null,
}

/// The pids whose process group is `pgid`.
fn group_members(pgid: i32) -> Vec<i32> {
    let out = Command::new("/usr/bin/pgrep")
        .args(["-g", &pgid.to_string()])
        .output()
        .expect("pgrep");
    String::from_utf8_lossy(&out.stdout)
        .split_ascii_whitespace()
        .filter_map(|word| word.parse().ok())
        .collect()
}

/// Start one launch the harness can never move, in a tab of `fake` (an aterm
/// that would grant the claim), on an interactive fake vendor, and wait for the
/// vendor to start: the terminal when there is one, the process the test
/// started, and its pid, which is the harness's and must by now be the
/// vendor's.
fn start_unmoved(
    s: &Scenario,
    fake: &FakeAterm,
    stdin: Stdin,
    args: &[&str],
    raw: &[OsString],
) -> (Option<Pty>, std::process::Child, i32) {
    s.add_build(BUILD_1, Some("9.9.1"));
    s.add_build(BUILD_2, Some("9.9.2"));
    s.set_current(BUILD_1);
    let mut cmd = s.command(args);
    cmd.args(raw).envs(fake.env());
    match stdin {
        Stdin::Terminal => {
            let pty = Pty::open();
            let (child, pid) = spawn_on_pty(s, &pty, cmd, None);
            (Some(pty), child, pid)
        }
        Stdin::Null => {
            cmd.stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .process_group(0);
            let child = cmd.spawn().expect("spawn");
            let pid = child_pid(&child);
            s.guard_group(pid);
            assert!(
                wait_until(Duration::from_secs(10), || !s.starts().is_empty()),
                "the fake never started{}",
                s.report()
            );
            (None, child, pid)
        }
    }
}

/// One whole launch the harness can never move, for the journal's `reason`.
/// The vendor runs in the harness's own process with the arguments `got`, as
/// the only process of its group. Nothing is said to aterm, though the tab
/// would grant a claim; no supervisor starts; a newer build is not followed;
/// and the process ends with the vendor's own exit code.
fn runs_in_place(
    name: &str,
    stdin: Stdin,
    args: &[&str],
    raw: &[OsString],
    reason: &str,
    got: &[&str],
) {
    let s = Scenario::new(name);
    let fake = rostered_aterm(&s);
    let (_pty, _child, pid) = start_unmoved(&s, &fake, stdin, args, raw);
    await_record(&s, 0);
    assert_ran_in_place(&s, 0, pid, reason, 1);
    assert_eq!(s.starts()[0].args, got, "every vendor argument, unchanged");
    assert_eq!(
        group_members(pid),
        [pid],
        "the vendor alone: no second process{}",
        s.report()
    );
    assert_eq!(
        s.log()
            .into_iter()
            .filter(|l| l.starts_with("env\t"))
            .collect::<Vec<_>>(),
        ["env\tDISABLE_AUTOUPDATER=1\tCLAUDE_CODE_DISABLE_FEEDBACK_SURVEY=1"],
        "the vendor still never updates itself{}",
        s.report()
    );
    // No harness is left to move the session: a new current build is not followed.
    s.set_current(BUILD_2);
    std::thread::sleep(Duration::from_millis(1200));
    assert_eq!(s.starts().len(), 1, "{}", s.report());
    assert!(!got_term(&s), "{}", s.report());
    // A signal sent to the pid the launcher holds reaches the vendor itself.
    signal(pid, libc::SIGTERM);
    assert_eq!(
        wait_status(pid, Duration::from_secs(10)),
        Some(Status::Exited(70)),
        "the vendor's own answer to SIGTERM{}",
        s.report()
    );
    assert!(got_term(&s), "{}", s.report());
    assert_eq!(
        fake.verbs(),
        Vec::<String>::new(),
        "not a word to aterm{}",
        s.report()
    );
    let journal = s.journal();
    assert_eq!(journal.lines().count(), 1, "{}", s.report());
    assert!(
        !journal.contains(" start harness=")
            && !journal.contains(" aterm ")
            && !journal.contains(" exit child=")
            && !journal.contains(" signal passed-on "),
        "{}",
        s.report()
    );
}

/// stdin is not a terminal: there is no interactive session to resume, so the
/// launch is never moved, whatever its arguments.
fn without_a_terminal_the_vendor_runs_in_the_harness_place() {
    runs_in_place(
        "in-place-no-tty",
        Stdin::Null,
        &["--alab-poll-ms=40", "--model", "opus"],
        &[],
        "no-tty",
        &["--model", "opus"],
    );
}

/// `-p` and `--help` are one-shot runs, on a terminal too, and so is the
/// vendor's own `--version` beside another word.
fn a_one_shot_launch_runs_the_vendor_in_the_harness_place() {
    runs_in_place(
        "in-place-print",
        Stdin::Terminal,
        &["--alab-quiet-ms=600", "--alab-poll-ms=40", "-p", "hi"],
        &[],
        "one-shot",
        &["-p", "hi"],
    );
    runs_in_place(
        "in-place-help",
        Stdin::Terminal,
        &["run", "--", "--help"],
        &[],
        "one-shot",
        &["--help"],
    );
    runs_in_place(
        "in-place-version",
        Stdin::Terminal,
        &["--model", "opus", "--version"],
        &[],
        "one-shot",
        &["--model", "opus", "--version"],
    );
}

/// A command line the resume grammar refuses (`--worktree`), or does not know
/// (`--frob`), cannot be carried onto another build.
fn an_unresumable_launch_runs_the_vendor_in_the_harness_place() {
    runs_in_place(
        "in-place-worktree",
        Stdin::Terminal,
        &["--model", "opus", "--worktree"],
        &[],
        "unresumable not-resumable --worktree",
        &["--model", "opus", "--worktree"],
    );
    runs_in_place(
        "in-place-unknown-flag",
        Stdin::Terminal,
        &["run", "--", "--frob"],
        &[],
        "unresumable unknown-flag --frob",
        &["--frob"],
    );
}

/// An argument that is not UTF-8 reaches the vendor as it is; the resume
/// grammar reads text, so the launch is never moved. (The fake logs its
/// arguments lossily: the byte `0xff` reads back as U+FFFD.)
fn non_utf8_arguments_run_the_vendor_in_the_harness_place() {
    use std::os::unix::ffi::OsStringExt as _;
    runs_in_place(
        "in-place-not-utf8",
        Stdin::Terminal,
        &["--model", "opus"],
        &[OsString::from_vec(b"caf\xff".to_vec())],
        "argv-not-utf8",
        &["--model", "opus", "caf\u{fffd}"],
    );
}

/// `--alab-no-live-update` asks for a session that is never moved: on a
/// terminal, with a command line that could be resumed, the vendor runs alone.
fn no_live_update_runs_the_vendor_in_the_harness_place() {
    runs_in_place(
        "in-place-no-live-update",
        Stdin::Terminal,
        &[
            "--alab-quiet-ms=600",
            "--alab-no-live-update",
            "--alab-poll-ms=40",
            "--model",
            "opus",
        ],
        &[],
        "no-live-update",
        &["--model", "opus"],
    );
}

/// Whoever started the launch holds the vendor's own pid, so killing it
/// outright (`SIGKILL`, which nothing can pass on) leaves nothing running.
/// A supervising harness killed that way would leave the vendor behind.
fn a_killed_in_place_run_leaves_no_orphan() {
    for (name, stdin, args) in [
        ("in-place-killed-print", Stdin::Terminal, &["-p", "hi"][..]),
        ("in-place-killed-no-tty", Stdin::Null, &["--model", "opus"]),
    ] {
        let s = Scenario::new(name);
        let fake = rostered_aterm(&s);
        let (_pty, _child, pid) = start_unmoved(&s, &fake, stdin, args, &[]);
        let vendor = s.starts()[0].pid;
        assert_eq!(vendor, pid, "{name}{}", s.report());
        assert_eq!(group_members(pid), [pid], "{name}{}", s.report());
        assert_eq!(children_of(pid), Vec::<i32>::new(), "{name}{}", s.report());
        signal(pid, libc::SIGKILL);
        assert_eq!(
            wait_status(pid, Duration::from_secs(10)),
            Some(Status::Signaled(libc::SIGKILL)),
            "{name}{}",
            s.report()
        );
        // The pid is reaped and its group is empty: no vendor outlived it.
        assert_eq!(
            group_members(pid),
            Vec::<i32>::new(),
            "{name}: a process outlived the kill{}",
            s.report()
        );
        // SAFETY: a zero signal to a group only asks whether it has members.
        let rc = unsafe { libc::kill(-pid, 0) };
        assert_eq!(rc, -1, "{name}: the group still has a member");
        // The fake logs every signal it catches and its own exit: it did neither.
        std::thread::sleep(Duration::from_millis(300));
        assert!(
            !s.log()
                .iter()
                .any(|l| l.starts_with("signal ") || l.starts_with("exit ")),
            "{name}{}",
            s.report()
        );
        assert_eq!(s.starts().len(), 1, "{name}{}", s.report());
    }
}

fn version_never_runs_a_store_build_that_is_the_harness() {
    let s = Scenario::new("version-probe-self");
    // A store `claude` that execs the harness, with no `.vendor` record: the
    // line says `unknown` and the store build is never run.
    s.add_build_script(BUILD_1, "exec \"$ALAB_TEST_HARNESS\" \"$@\"");
    s.set_current(BUILD_1);
    let out = s.command(&["--version"]).output().expect("run");
    assert!(out.status.success(), "{out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        version_line("unknown")
    );
    assert_eq!(
        s.script_runs(),
        0,
        "--version reads the store and never runs it{}",
        s.report()
    );
}

fn a_wrapper_that_runs_the_harness_again_is_bounded() {
    let s = Scenario::new("wrapper-recursion");
    // A store `claude` that runs the harness as its own child (no `exec`): the
    // parent marker cannot see it, the nesting depth stops it. On a terminal,
    // so that every level is a supervising harness with the script as its child.
    s.add_build_script(BUILD_1, "\"$ALAB_TEST_HARNESS\" \"$@\"");
    s.set_current(BUILD_1);
    let (status, err) = run_on_pty(&s, s.command(&[]), Duration::from_secs(60));
    assert_eq!(status, Some(Status::Exited(127)), "{err}{}", s.report());
    assert!(err.contains("already nested 8 deep"), "{err}");
    assert_eq!(
        s.script_runs(),
        8,
        "eight nested levels, then a refusal, never the script's own stop at 30{}",
        s.report()
    );
    assert_eq!(
        s.journal().matches(" start harness=").count(),
        8,
        "every level supervised its script{}",
        s.report()
    );
    assert!(!s.journal().contains(" pass "), "{}", s.report());
    assert_eq!(
        s.journal()
            .matches(" refused nesting-depth CLAUDE_ALAB_DEPTH=8: ")
            .count(),
        1,
        "{}",
        s.report()
    );
    // `--version` never runs the wrapper at all.
    std::fs::remove_file(s.root.join("count")).expect("reset count");
    let out = s.command(&["--version"]).output().expect("run");
    assert!(out.status.success(), "{out:?}");
    assert_eq!(s.script_runs(), 0, "{}", s.report());
}

fn an_update_waits_for_background_work() {
    let s = Scenario::new("background");
    let pty = Pty::open();
    let (_child, pid) = start_on_pty(&s, &pty, &[]);
    // A turn left a background Bash task running; the record says idle.
    s.put("command", "bg-shell");
    assert!(
        wait_until(Duration::from_secs(5), || s
            .log()
            .iter()
            .any(|l| l.starts_with("bg-shell "))),
        "{}",
        s.report()
    );
    s.set_current(BUILD_2);
    std::thread::sleep(Duration::from_millis(3000));
    assert_eq!(
        s.starts().len(),
        1,
        "ended a child with background work{}",
        s.report()
    );
    assert!(
        !s.log().iter().any(|l| l == "signal TERM"),
        "{}",
        s.report()
    );
    assert!(
        s.journal().contains("wait background-work "),
        "{}",
        s.report()
    );
    // The background work ends: the update goes through.
    s.put("command", "bg-stop");
    assert!(
        wait_until(Duration::from_secs(15), || s.starts().len() >= 2),
        "no relaunch once the background work ended{}",
        s.report()
    );
    assert_eq!(s.starts()[1].build(), Some(BUILD_2));
    s.put("command", "exit:0");
    assert_eq!(
        wait_status(pid, Duration::from_secs(10)),
        Some(Status::Exited(0)),
        "{}",
        s.report()
    );
}

fn an_abandoned_update_is_never_signalled_again() {
    let s = Scenario::new("abandoned");
    // This child stays up after SIGTERM.
    s.put("term", "ignore");
    let pty = Pty::open();
    let (_child, pid) = start_on_pty(&s, &pty, &["--alab-term-grace-ms=1000"]);
    let terms = |s: &Scenario| s.log().iter().filter(|l| *l == "signal TERM").count();
    s.set_current(BUILD_2);
    assert!(
        wait_until(Duration::from_secs(10), || terms(&s) >= 1),
        "{}",
        s.report()
    );
    // Well past the grace and several safe points: still exactly one SIGTERM.
    std::thread::sleep(Duration::from_millis(4000));
    assert_eq!(
        terms(&s),
        1,
        "a second SIGTERM for one target{}",
        s.report()
    );
    let journal = s.journal();
    assert!(journal.contains("update-abandoned"), "{}", s.report());
    assert!(
        journal.contains(&format!("wait abandoned build={BUILD_2}")),
        "{}",
        s.report()
    );
    assert_eq!(s.starts().len(), 1, "{}", s.report());
    assert_eq!(
        wait_status(pid, Duration::ZERO),
        None,
        "the harness runs on"
    );
    // A newer build re-arms the update: one SIGTERM for it.
    s.add_build(BUILD_3, Some("9.9.3"));
    s.set_current(BUILD_3);
    assert!(
        wait_until(Duration::from_secs(10), || terms(&s) >= 2),
        "{}",
        s.report()
    );
    std::thread::sleep(Duration::from_millis(2500));
    assert_eq!(terms(&s), 2, "{}", s.report());
    assert!(
        s.journal()
            .contains(&format!("wait abandoned build={BUILD_3}")),
        "{}",
        s.report()
    );
    // `current` goes back to the build abandoned first: both abandoned builds
    // are remembered, so it is not signalled a second time.
    let waits = |s: &Scenario| {
        s.journal()
            .matches(&format!("wait abandoned build={BUILD_2}"))
            .count()
    };
    assert_eq!(waits(&s), 1, "{}", s.report());
    s.set_current(BUILD_2);
    assert!(
        wait_until(Duration::from_secs(10), || waits(&s) >= 2),
        "an abandoned build was forgotten once another was abandoned{}",
        s.report()
    );
    std::thread::sleep(Duration::from_millis(2500));
    assert_eq!(
        terms(&s),
        2,
        "a second SIGTERM for the build abandoned first{}",
        s.report()
    );
    // The session ends on its own long after: it is the session's end, not a relaunch.
    s.put("command", "exit:6");
    assert_eq!(
        wait_status(pid, Duration::from_secs(10)),
        Some(Status::Exited(6)),
        "{}",
        s.report()
    );
    assert_eq!(s.starts().len(), 1, "{}", s.report());
}

fn a_relaunch_resumes_the_conversations_permission_mode() {
    let s = Scenario::new("permission-mode");
    // Claude Code's transcript of the conversation: launched in bypass, then the
    // person switched to plan mode inside the session.
    let project = s.home.join(".claude/projects/-scenario-work");
    std::fs::create_dir_all(&project).expect("project dir");
    std::fs::write(
        project.join(format!("{SID}.jsonl")),
        format!(
            "{{\"type\":\"permission-mode\",\"permissionMode\":\"bypassPermissions\",\"sessionId\":\"{SID}\"}}\n\
             {{\"type\":\"user\",\"message\":{{\"role\":\"user\",\"content\":\"hi\"}}}}\n\
             {{\"type\":\"permission-mode\",\"permissionMode\":\"plan\",\"sessionId\":\"{SID}\"}}\n"
        ),
    )
    .expect("transcript");
    let pty = Pty::open();
    let (_child, pid) = start_on_pty(
        &s,
        &pty,
        &["--dangerously-skip-permissions", "--model", "opus"],
    );
    s.set_current(BUILD_2);
    assert!(
        wait_until(Duration::from_secs(15), || s.starts().len() >= 2),
        "no relaunch{}",
        s.report()
    );
    assert_eq!(
        s.starts()[1].args,
        [
            "--model",
            "opus",
            "--allow-dangerously-skip-permissions",
            "--permission-mode",
            "plan",
            "--resume",
            SID
        ],
        "resumed in plan mode, not back in the launch's bypass{}",
        s.report()
    );
    s.put("command", "exit:0");
    assert_eq!(
        wait_status(pid, Duration::from_secs(10)),
        Some(Status::Exited(0)),
        "{}",
        s.report()
    );
}

fn a_relaunch_preserves_option_values_that_spell_permission_flags() {
    let s = Scenario::new("permission-value");
    let project = s.home.join(".claude/projects/-scenario-work");
    std::fs::create_dir_all(&project).expect("project dir");
    std::fs::write(
        project.join(format!("{SID}.jsonl")),
        format!(
            "{{\"type\":\"permission-mode\",\"permissionMode\":\"auto\",\"sessionId\":\"{SID}\"}}\n"
        ),
    )
    .expect("transcript");
    let pty = Pty::open();
    let launch = [
        "--permission-mode",
        "default",
        "--append-system-prompt",
        "--dangerously-skip-permissions",
    ];
    let (_child, pid) = start_on_pty(&s, &pty, &launch);
    let _first = await_record(&s, 0);
    assert_eq!(s.starts()[0].args, launch);
    s.set_current(BUILD_2);
    let _second = await_record(&s, 1);
    assert_eq!(
        s.starts()[1].args,
        [
            "--append-system-prompt",
            "--dangerously-skip-permissions",
            "--permission-mode",
            "auto",
            "--resume",
            SID,
        ],
        "the new mode changes posture without consuming another flag's value{}",
        s.report()
    );
    end_session(&s, pid);
}

/// The supervisor journals the model, effort level and top-effort key its
/// child states in its transcript, one `facts` line per change. A row from
/// before the child started (the conversation's older model and level) is not
/// the child's: the first line names the child's own model and no level, and
/// no line names the older model. Rows appended later move the facts.
fn the_journal_names_the_childs_model_effort_and_key() {
    let s = Scenario::new("model-facts");
    let project = s.home.join(".claude/projects/-scenario-work");
    std::fs::create_dir_all(&project).expect("project dir");
    let transcript = project.join(format!("{SID}.jsonl"));
    // Stamped after the child's start whenever this runs.
    let later = "2099-01-01T00:00:00.000Z";
    std::fs::write(
        &transcript,
        format!(
            "{{\"type\":\"assistant\",\"timestamp\":\"2020-01-01T00:00:00.000Z\",\"effort\":\"low\",\"message\":{{\"model\":\"claude-fable-5-1\"}}}}\n\
             {{\"type\":\"assistant\",\"timestamp\":\"{later}\",\"message\":{{\"model\":\"claude-opus-5-5\"}}}}\n"
        ),
    )
    .expect("transcript");
    let pty = Pty::open();
    let (_child, pid) = start_on_pty(&s, &pty, &[]);
    await_record(&s, 0);
    await_journal(
        &s,
        Duration::from_secs(10),
        "facts model=Opus_5.5 effort=- key=-",
    );
    // Both rows in one write, so no look sees one without the other.
    let rows = format!(
        "{{\"type\":\"attachment\",\"timestamp\":\"{later}\",\"attachment\":{{\"type\":\"ultra_effort_enter\"}}}}\n\
         {{\"type\":\"assistant\",\"timestamp\":\"{later}\",\"effort\":\"high\",\"message\":{{\"model\":\"claude-opus-5-5\"}}}}\n"
    );
    std::fs::OpenOptions::new()
        .append(true)
        .open(&transcript)
        .expect("transcript")
        .write_all(rows.as_bytes())
        .expect("append");
    await_journal(
        &s,
        Duration::from_secs(10),
        "facts model=Opus_5.5 effort=high key=on",
    );
    let journal = s.journal();
    assert!(
        !journal.contains("Fable") && !journal.contains("effort=low"),
        "a row from before the child started is not its own{}",
        s.report()
    );
    assert_eq!(
        journal.matches(" facts ").count(),
        2,
        "one line per change{}",
        s.report()
    );
    end_session(&s, pid);
}

fn a_broken_stderr_does_not_lose_the_session() {
    let s = Scenario::new("broken-stderr");
    // stderr is a pipe nobody reads any more: every write to it fails (EPIPE).
    let (reader, writer) = std::io::pipe().expect("pipe");
    drop(reader);
    let pty = Pty::open();
    let (_child, pid) = start_on_pty_with(&s, &pty, &[], Some(Stdio::from(writer)));
    s.set_current(BUILD_2);
    assert!(
        wait_until(Duration::from_secs(15), || s.starts().len() >= 2),
        "the relaunch notice to a broken stderr lost the session{}",
        s.report()
    );
    assert_eq!(
        wait_status(pid, Duration::ZERO),
        None,
        "the harness runs on"
    );
    s.put("command", "exit:0");
    assert_eq!(
        wait_status(pid, Duration::from_secs(10)),
        Some(Status::Exited(0)),
        "{}",
        s.report()
    );
}

fn a_relaunch_fallback_marks_the_target_abandoned() {
    let s = Scenario::new("relaunch-fallback-abandoned");
    s.put("term", "ignore");
    let pty = Pty::open();
    let (_child, pid) = start_on_pty(&s, &pty, &["--model", "opus"]);
    s.set_current(BUILD_2);
    assert!(
        wait_until(Duration::from_secs(15), || s
            .log()
            .iter()
            .any(|l| l == "signal TERM")),
        "{}",
        s.report()
    );
    // Invalidate BUILD_2's pinned GC gate after SIGTERM so start(&plan.target)
    // fails at relaunch and falls back to BUILD_1, while BUILD_2 remains current.
    let gate = s
        .store
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("leases")
        .join(format!("store-claude-{BUILD_2}.gate"));
    std::fs::remove_file(&gate).expect("unlink target gate");
    s.put("term", "exit");
    s.put("command", "exit:0");
    assert!(
        wait_until(Duration::from_secs(15), || s.starts().len() >= 2),
        "never fell back to BUILD_1{}",
        s.report()
    );
    assert_eq!(s.starts()[1].build(), Some(BUILD_1), "{}", s.report());
    // A target that never ran falls back with the same notice and journal word
    // as one that refused to start (docs/DECISIONS.md, 28).
    assert!(
        s.journal().contains(&format!(
            "relaunch-fallback spawn-failed failed-build={BUILD_2} fallback-build={BUILD_1} session={SID}"
        )),
        "never journalled relaunch-fallback for the spawn failure{}",
        s.report()
    );
    assert!(
        wait_until(Duration::from_secs(5), || pty
            .output()
            .contains("9.9.2 failed to start; resuming conversation")),
        "missing fallback notice\n--- pty ---\n{}",
        pty.output()
    );
    // BUILD_2 was never started, so nothing may say the conversation resumed
    // on it: the fallback's line is the only one.
    let said = pty.output();
    assert!(
        !said.contains("on the new build"),
        "claimed a resume on a build that was never started\n--- pty ---\n{said}"
    );
    assert!(
        wait_until(Duration::from_secs(10), || s
            .journal()
            .contains(&format!("wait abandoned build={BUILD_2}"))),
        "never marked BUILD_2 abandoned after fallback{}",
        s.report()
    );
    assert_eq!(
        s.starts().len(),
        2,
        "must not loop trying BUILD_2 again{}",
        s.report()
    );
    // Installing BUILD_3 re-arms the update and moves cleanly.
    s.add_build(BUILD_3, Some("9.9.3"));
    s.set_current(BUILD_3);
    assert!(
        wait_until(Duration::from_secs(15), || s.starts().len() >= 3),
        "never moved to BUILD_3{}",
        s.report()
    );
    assert_eq!(s.starts()[2].build(), Some(BUILD_3), "{}", s.report());
    s.put("command", "exit:0");
    assert_eq!(
        wait_status(pid, Duration::from_secs(10)),
        Some(Status::Exited(0)),
        "{}",
        s.report()
    );
}

fn a_move_onto_a_build_that_refuses_to_start_falls_back_and_abandons_it() {
    let s = Scenario::new("move-refused-start-fallback");
    let pty = Pty::open();
    let (_child, pid) = start_on_pty(&s, &pty, &["--model", "opus"]);
    // Replace BUILD_2 with a broken vendor binary that spawns (`posix_spawn`
    // succeeds) and immediately exits non-zero before writing a session record.
    s.add_build_script(BUILD_2, "exit 2");
    s.set_current(BUILD_2);
    assert!(
        wait_until(Duration::from_secs(15), || s.starts().len() >= 2),
        "never fell back to BUILD_1 after BUILD_2 refused to start{}\n--- pty ---\n{}",
        s.report(),
        pty.output()
    );
    assert_eq!(
        s.script_runs(),
        1,
        "BUILD_2 ran once and refused to start{}",
        s.report()
    );
    assert_eq!(s.starts()[1].build(), Some(BUILD_1), "{}", s.report());
    assert_eq!(
        s.starts()[1].args,
        ["--model", "opus", "--resume", SID],
        "{}",
        s.report()
    );
    assert!(
        wait_until(Duration::from_secs(10), || {
            let j = s.journal();
            j.contains(&format!(
                "relaunch-fallback child=Exited(2) failed-build={BUILD_2} fallback-build={BUILD_1} session={SID}"
            )) && j.contains(&format!("wait abandoned build={BUILD_2}"))
        }),
        "never recorded relaunch-fallback and abandoned state{}",
        s.report()
    );
    assert!(
        wait_until(Duration::from_secs(5), || pty
            .output()
            .contains("9.9.2 failed to start; resuming conversation")),
        "missing fallback notice\n--- pty ---\n{}",
        pty.output()
    );
    assert_eq!(
        s.starts().len(),
        2,
        "must not loop trying BUILD_2 again{}",
        s.report()
    );
    // Installing BUILD_3 re-arms the update and moves cleanly.
    s.add_build(BUILD_3, Some("9.9.3"));
    s.set_current(BUILD_3);
    assert!(
        wait_until(Duration::from_secs(15), || s.starts().len() >= 3),
        "never moved to BUILD_3{}",
        s.report()
    );
    assert_eq!(s.starts()[2].build(), Some(BUILD_3), "{}", s.report());
    s.put("command", "exit:0");
    assert_eq!(
        wait_status(pid, Duration::from_secs(10)),
        Some(Status::Exited(0)),
        "{}",
        s.report()
    );
}

fn a_move_onto_a_build_that_crashes_before_taking_the_terminal_falls_back() {
    let s = Scenario::new("move-crash-fallback");
    let pty = Pty::open();
    let (_child, pid) = start_on_pty(&s, &pty, &["--model", "opus"]);
    // BUILD_2 dies by a crash signal at once, the terminal still canonical.
    s.add_build_script(BUILD_2, "kill -USR1 $$");
    s.set_current(BUILD_2);
    assert!(
        wait_until(Duration::from_secs(15), || s.starts().len() >= 2),
        "never fell back to BUILD_1 after BUILD_2 crashed{}\n--- pty ---\n{}",
        s.report(),
        pty.output()
    );
    assert_eq!(s.script_runs(), 1, "{}", s.report());
    assert_eq!(s.starts()[1].build(), Some(BUILD_1), "{}", s.report());
    assert!(
        s.journal().contains(&format!(
            "relaunch-fallback child=Signaled({}) failed-build={BUILD_2} fallback-build={BUILD_1} session={SID}",
            libc::SIGUSR1
        )),
        "{}",
        s.report()
    );
    s.put("command", "exit:0");
    assert_eq!(
        wait_status(pid, Duration::from_secs(10)),
        Some(Status::Exited(0)),
        "{}",
        s.report()
    );
}

fn a_moved_build_that_took_the_terminal_is_not_fallen_back() {
    let s = Scenario::new("move-took-terminal");
    let pty = Pty::open();
    let (_child, pid) = start_on_pty(&s, &pty, &["--model", "opus"]);
    // BUILD_2 takes the terminal out of canonical mode, as Claude Code's
    // interface does once it draws, then dies by a signal before writing its
    // session record: it had started, so its end is the session's (Decision 28).
    s.add_build_script(BUILD_2, "stty raw; kill -USR1 $$");
    s.set_current(BUILD_2);
    assert_eq!(
        wait_status(pid, Duration::from_secs(20)),
        Some(Status::Signaled(libc::SIGUSR1)),
        "the session ends the way the started build ended{}\n--- pty ---\n{}",
        s.report(),
        pty.output()
    );
    assert_eq!(s.script_runs(), 1, "{}", s.report());
    assert_eq!(s.starts().len(), 1, "no fallback to BUILD_1{}", s.report());
    assert!(!s.journal().contains("relaunch-fallback"), "{}", s.report());
    assert_eq!(
        pty.icanon(),
        Some(true),
        "the saved modes are restored on a signal death{}",
        s.report()
    );
}

fn a_signal_death_restores_the_terminal_modes() {
    let s = Scenario::new("killed-on-pty");
    let pty = Pty::open();
    let (_child, pid) = start_on_pty(&s, &pty, &[]);
    assert!(
        wait_until(Duration::from_secs(5), || pty.icanon() == Some(false)),
        "the fake never took the terminal{}",
        s.report()
    );
    s.put("command", &format!("raise:{}", libc::SIGUSR1));
    assert_eq!(
        wait_status(pid, Duration::from_secs(10)),
        Some(Status::Signaled(libc::SIGUSR1)),
        "{}",
        s.report()
    );
    assert_eq!(
        pty.icanon(),
        Some(true),
        "the harness put back the modes it saved before the child died{}",
        s.report()
    );
}

fn the_vendor_always_runs_with_the_updater_and_survey_off() {
    let s = Scenario::new("forced-env");
    for (number, version) in [(BUILD_1, "9.9.1"), (BUILD_2, "9.9.2")] {
        s.add_build(number, Some(version));
        // A sidecar that turns both back on: the harness's own values win.
        std::fs::write(
            s.store.join(format!("{number}.shim-env")),
            format!(
                "FAKE_SHIM_BUILD={number}\nDISABLE_AUTOUPDATER=0\nCLAUDE_CODE_DISABLE_FEEDBACK_SURVEY=0\n"
            ),
        )
        .expect("shim-env");
    }
    s.set_current(BUILD_1);
    let pty = Pty::open();
    let mut cmd = s.command(&["--alab-quiet-ms=600", "--alab-poll-ms=40"]);
    cmd.env("DISABLE_AUTOUPDATER", "0")
        .env("CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY", "0");
    let (_child, pid) = spawn_on_pty(&s, &pty, cmd, None);
    let envs = |s: &Scenario| -> Vec<String> {
        s.log()
            .into_iter()
            .filter(|l| l.starts_with("env\t"))
            .collect()
    };
    assert!(
        wait_until(Duration::from_secs(10), || !envs(&s).is_empty()),
        "the fake never logged its environment{}",
        s.report()
    );
    // The sidecar was accepted, so its `=0` entries really reached the command.
    assert_eq!(s.starts()[0].shim, format!("{BUILD_1}"), "{}", s.report());
    s.set_current(BUILD_2);
    assert!(
        wait_until(Duration::from_secs(15), || envs(&s).len() >= 2),
        "no relaunch{}",
        s.report()
    );
    assert_eq!(s.starts()[1].build(), Some(BUILD_2), "{}", s.report());
    let forced = "env\tDISABLE_AUTOUPDATER=1\tCLAUDE_CODE_DISABLE_FEEDBACK_SURVEY=1";
    assert_eq!(
        envs(&s),
        [forced, forced],
        "the first launch and the relaunch both run with the updater and survey off{}",
        s.report()
    );
    s.put("command", "exit:0");
    assert_eq!(
        wait_status(pid, Duration::from_secs(10)),
        Some(Status::Exited(0)),
        "{}",
        s.report()
    );
}

// ─────────────────────────── inside an aterm tab ───────────────────────────

use fake_aterm::{FakeAterm, INFO_ROSTERED, POLICY_ON};

/// A fake aterm for `s` whose roster carries this harness, as no released
/// aterm's does yet.
fn rostered_aterm(s: &Scenario) -> FakeAterm {
    let fake = FakeAterm::new(&s.root);
    fake.set_info(INFO_ROSTERED);
    fake
}

/// [`start_on_pty`] in `fake`'s tab, looking every `poll_ms`.
fn start_in_aterm(
    s: &Scenario,
    pty: &Pty,
    fake: &FakeAterm,
    poll_ms: u32,
) -> (std::process::Child, i32) {
    start_in_aterm_with(s, pty, fake, poll_ms, &[])
}

/// [`start_in_aterm`] with further harness options.
fn start_in_aterm_with(
    s: &Scenario,
    pty: &Pty,
    fake: &FakeAterm,
    poll_ms: u32,
    extra: &[&str],
) -> (std::process::Child, i32) {
    s.add_build(BUILD_1, Some("9.9.1"));
    s.add_build(BUILD_2, Some("9.9.2"));
    s.set_current(BUILD_1);
    let poll = format!("--alab-poll-ms={poll_ms}");
    let mut args = vec!["--alab-quiet-ms=600", poll.as_str()];
    args.extend_from_slice(extra);
    let mut cmd = s.command(&args);
    cmd.envs(fake.env());
    spawn_on_pty(s, pty, cmd, None)
}

/// The claim harness process `pid` sends while its child runs vendor `vendor`.
fn claim_text(pid: i32, vendor: &str) -> String {
    format!(
        "meta set supervisor claude-code-alab@{pid} contract=3 scope=relaunch,upgrade \
         version={VERSION} vendor={vendor}"
    )
}

/// The release harness process `pid` sends.
fn release_text(pid: i32) -> String {
    format!("meta unset supervisor holder=claude-code-alab@{pid}")
}

/// How many of `fake`'s requests were exactly `verb`.
fn asked(fake: &FakeAterm, verb: &str) -> usize {
    fake.verbs().iter().filter(|sent| *sent == verb).count()
}

/// How many claims `fake` was ever sent, by anyone, in any words.
fn claims_sent(fake: &FakeAterm) -> usize {
    fake.verbs()
        .iter()
        .filter(|sent| sent.starts_with("meta set supervisor"))
        .count()
}

/// Where `needle` first appears in `s`'s journal.
fn journal_at(s: &Scenario, needle: &str) -> usize {
    s.journal()
        .find(needle)
        .unwrap_or_else(|| panic!("the journal lacks {needle:?}{}", s.report()))
}

fn got_term(s: &Scenario) -> bool {
    s.log().iter().any(|l| l == "signal TERM")
}

/// End the session from inside and expect the harness to end with it.
fn end_session(s: &Scenario, pid: i32) {
    s.put("command", "exit:0");
    assert_eq!(
        wait_status(pid, Duration::from_secs(10)),
        Some(Status::Exited(0)),
        "{}",
        s.report()
    );
}

/// A negotiated host names the real vendor lifetime, not the harness or the
/// record's approximate startedAt. Every new child replaces the old target.
fn aterm_native_child_target_follows_the_first_start_restarts_and_vendor_moves() {
    let s = Scenario::new("aterm-child-proof");
    let fake = rostered_aterm(&s);
    fake.set_info(&format!("{INFO_ROSTERED} child-proof=1"));
    let pty = Pty::open();
    let (_child, pid) = start_in_aterm(&s, &pty, &fake, 40);
    let assert_child = |vendor: &str, child: i32| {
        let identity = claude_code_alab::procs::capture_process(u32::try_from(child).unwrap())
            .expect("live vendor native identity");
        let birth = match identity.birth {
            claude_code_alab::procs::Birth::MacOs {
                seconds,
                microseconds,
            } => seconds * 1_000_000 + u64::from(microseconds),
            claude_code_alab::procs::Birth::Linux(ticks) => ticks,
        };
        let expected = format!("{} child={child}:{birth}", claim_text(pid, vendor));
        assert!(
            wait_until(Duration::from_secs(60), || asked(&fake, &expected) == 1),
            "missing native child target {expected}{}",
            s.report()
        );
    };
    let first = await_record(&s, 0);
    assert_child("9.9.1", first);
    assert_eq!(
        asked(&fake, &claim_text(pid, "9.9.1")),
        1,
        "the initial claim precedes spawn"
    );
    fake.event("restart cause=asked");
    let second = await_record(&s, 1);
    assert_ne!(second, first);
    assert_child("9.9.1", second);
    s.set_current(BUILD_2);
    let third = await_record(&s, 2);
    assert_ne!(third, second);
    assert_child("9.9.2", third);
    assert_eq!(
        claims_sent(&fake),
        4,
        "one initial claim and one for each child"
    );
    end_session(&s, pid);
}

/// The whole claim sequence is over before the vendor exists, in order, and
/// the claim says `scope=relaunch,upgrade`, this harness's version and the
/// vendor build it is about to start. The claim is given back when the harness
/// exits. (What a restart request does is its own scenarios' matter, below.)
fn aterm_claim_precedes_the_first_vendor_start() {
    let s = Scenario::new("aterm-claim-first");
    let fake = rostered_aterm(&s);
    // The fake vendor's first act is to write its log. A slow aterm gives a
    // vendor started too early the time to do so before the claim arrives.
    fake.set_witness(&s.fake.join("log"));
    fake.set_reply_delay(Duration::from_millis(100));
    let pty = Pty::open();
    let (_child, pid) = start_in_aterm(&s, &pty, &fake, 40);
    fake.set_reply_delay(Duration::ZERO);
    let sent = fake.verbs_with_witness();
    assert_eq!(
        sent.get(..4),
        Some(
            &[
                ("harness policy".to_string(), false),
                ("native-act info".to_string(), false),
                (claim_text(pid, "9.9.1"), false),
                ("status".to_string(), false),
            ][..]
        ),
        "the claim sequence, each step before the vendor's start{}",
        s.report()
    );
    assert_eq!(
        fake.holder(),
        Some(format!("claude-code-alab@{pid}")),
        "{}",
        s.report()
    );
    assert!(
        wait_until(Duration::from_secs(5), || fake.subscriptions() == 1),
        "the event subscription{}",
        s.report()
    );
    let journal = s.journal();
    assert!(
        journal.contains(&format!(
            " aterm claimed holder=claude-code-alab@{pid} scope=relaunch,upgrade vendor=9.9.1\n"
        )) && !journal.contains("aterm unclaimed"),
        "{}",
        s.report()
    );
    assert!(
        journal_at(&s, " aterm claimed ") > journal_at(&s, " start harness="),
        "{}",
        s.report()
    );
    assert!(
        fake.status_line()
            .contains(" supervisor_scope=relaunch,upgrade "),
        "aterm reads both duties back: {}",
        fake.status_line()
    );
    std::thread::sleep(Duration::from_millis(500));
    assert!(!got_term(&s) && s.starts().len() == 1, "{}", s.report());
    assert_eq!(wait_status(pid, Duration::ZERO), None);
    end_session(&s, pid);
    assert!(
        wait_until(Duration::from_secs(5), || fake.holder().is_none()),
        "{}",
        s.report()
    );
    assert_eq!(
        asked(&fake, &release_text(pid)),
        1,
        "the claim is released by name at exit: {:?}",
        fake.verbs()
    );
    assert_eq!(claims_sent(&fake), 1, "{:?}", fake.verbs());
}

/// One launch whose claim sequence stops short of a proven claim: `prepare`
/// sets the fake up, `why` is what the one `aterm unclaimed` line says after
/// the harness's pid is put in for `{pid}`, and `sent` is every request the
/// fake must have been sent by the end of the session, with the same
/// substitution. The session then runs as it does outside aterm: a new vendor
/// build is moved onto on the harness's own readings, and aterm is asked
/// nothing more.
fn runs_unclaimed(name: &str, prepare: impl Fn(&FakeAterm), why: &str, sent: &[&str]) {
    let s = Scenario::new(name);
    let fake = FakeAterm::new(&s.root);
    prepare(&fake);
    let pty = Pty::open();
    let (_child, pid) = start_in_aterm(&s, &pty, &fake, 40);
    let named = |text: &str| text.replace("{pid}", &pid.to_string());
    let sent: Vec<String> = sent
        .iter()
        .map(|verb| named(verb).replace("{claim}", &claim_text(pid, "9.9.1")))
        .collect();
    // The line is written before the vendor starts.
    let journal = s.journal();
    assert!(
        journal.contains(&format!(" aterm unclaimed {}\n", named(why))),
        "{}",
        s.report()
    );
    assert_eq!(fake.verbs(), sent, "{}", s.report());
    assert_eq!(fake.holder(), None, "no claim is left standing");
    s.set_current(BUILD_2);
    assert!(
        wait_until(Duration::from_secs(15), || s.starts().len() >= 2),
        "no move on the harness's own readings{}",
        s.report()
    );
    assert_eq!(s.starts()[1].build(), Some(BUILD_2), "{}", s.report());
    assert_eq!(s.starts()[1].args, ["--resume", SID]);
    // More looks, and the end of the session: still nothing more for aterm.
    std::thread::sleep(Duration::from_millis(500));
    end_session(&s, pid);
    let journal = s.journal();
    assert_eq!(journal.matches(" aterm ").count(), 1, "{}", s.report());
    assert!(!journal.contains("wait aterm"), "{}", s.report());
    assert_eq!(
        fake.verbs(),
        sent,
        "an absent link is never retried{}",
        s.report()
    );
    assert_eq!(fake.subscriptions(), 0, "{}", s.report());
    assert_eq!(fake.holder(), None);
}

/// A policy off by a fact of the window (`headless`), which no setting turns
/// on while the session runs: the launch is unclaimed for good. (One switched
/// off by a setting is tried again:
/// `aterm_a_launch_with_autopilot_off_is_claimed_once_it_is_on`.)
fn aterm_policy_off_leaves_the_session_unclaimed() {
    runs_unclaimed(
        "aterm-policy-off",
        |fake| {
            fake.set_info(INFO_ROSTERED);
            fake.set_policy(
                &POLICY_ON.replace("effective=on why=enabled", "effective=off why=headless"),
            );
        },
        "policy effective=off why=headless",
        &["harness policy"],
    );
}

fn aterm_without_contract_3_leaves_the_session_unclaimed() {
    runs_unclaimed(
        "aterm-contract-2",
        |fake| {
            fake.set_info(INFO_ROSTERED);
            fake.set_policy(&POLICY_ON.replace("contract=2,3", "contract=2"));
        },
        "policy contract=2",
        &["harness policy"],
    );
}

/// Today's aterm: the policy admits a claim and the roster has no row for this
/// harness. The guard reads that and no claim is ever sent, not even one to be
/// refused: aterm would store it as a foreign claim that takes every duty.
fn aterm_without_the_roster_row_is_never_sent_a_claim() {
    // `runs_unclaimed` compares every request of the whole session, so the
    // absence of `meta set supervisor` is exact.
    runs_unclaimed(
        "aterm-unrostered",
        |_| {},
        "unrostered",
        &["harness policy", "native-act info"],
    );
}

fn aterm_claim_refused_busy_leaves_the_session_unclaimed() {
    runs_unclaimed(
        "aterm-busy",
        |fake| {
            fake.set_info(INFO_ROSTERED);
            fake.set_claim_reply("ERR busy supervisor=someone-else@77");
        },
        "claim reply=ERR:busy",
        &["harness policy", "native-act info", "{claim}"],
    );
}

/// aterm stored the claim and `status` does not show it proven: the harness
/// gives it back by name rather than act under it or leave it standing.
fn aterm_claim_not_proven_is_released() {
    runs_unclaimed(
        "aterm-unproven",
        |fake| {
            fake.set_info(INFO_ROSTERED);
            fake.set_proven(false);
        },
        "unproven supervisor=claude-code-alab@{pid} supervisor_proven=0",
        &[
            "harness policy",
            "native-act info",
            "{claim}",
            "status",
            "meta unset supervisor holder=claude-code-alab@{pid}",
        ],
    );
}

/// With a proven claim the composer is aterm's to read. Input the vendor read
/// and never submitted is a draft to the harness's own tracker
/// (`never_relaunches_over_typed_input`); aterm says `composer=empty`, and the
/// vendor moves. The last two requests before the signal are the fresh policy
/// and status, and the new build is announced afterwards.
fn aterm_moves_on_aterms_word_over_what_the_tracker_calls_a_draft() {
    let s = Scenario::new("aterm-word");
    let fake = rostered_aterm(&s);
    let pty = Pty::open();
    let (_child, pid) = start_in_aterm(&s, &pty, &fake, 40);
    std::thread::sleep(Duration::from_millis(200));
    pty.type_text("half a thought");
    assert!(
        wait_until(Duration::from_secs(5), || s
            .log()
            .iter()
            .any(|l| l.starts_with("read "))),
        "the fake never read the typing{}",
        s.report()
    );
    s.set_current(BUILD_2);
    assert!(
        wait_until(Duration::from_secs(15), || s.starts().len() >= 2),
        "no move on aterm's word{}",
        s.report()
    );
    assert_eq!(s.starts()[1].build(), Some(BUILD_2), "{}", s.report());
    let announce = claim_text(pid, "9.9.2");
    assert!(
        wait_until(Duration::from_secs(5), || asked(&fake, &announce) == 1),
        "the new vendor build was not announced: {:?}{}",
        fake.verbs(),
        s.report()
    );
    let verbs = fake.verbs();
    let at = verbs
        .iter()
        .position(|verb| *verb == announce)
        .expect("announced");
    assert_eq!(
        verbs.get(at - 2..at),
        Some(&["harness policy".to_string(), "status".to_string()][..]),
        "one fresh policy and status read is the last thing before the signal: {verbs:?}"
    );
    let journal = s.journal();
    assert!(
        !journal.contains("wait draft")
            && !journal.contains("wait typing")
            && !journal.contains("wait unread-input"),
        "{}",
        s.report()
    );
    assert!(
        journal.contains(" aterm reannounced vendor=9.9.2\n"),
        "{}",
        s.report()
    );
    assert_eq!(fake.holder(), Some(format!("claude-code-alab@{pid}")));
    end_session(&s, pid);
}

/// Every field aterm's safe point reads holds the move by name, one after the
/// other, with the record idle and nothing typed throughout; the vendor moves
/// only once the last has cleared.
fn aterm_holds_on_each_field_until_it_clears() {
    // (status key, a value that holds, the value at rest)
    const FIELDS: [(&str, &str, &str); 7] = [
        ("agent", "busy", "idle"),
        ("composer", "draft", "empty"),
        // Inside the policy's 120 s grace.
        ("human_ms", "1000", "-"),
        ("hand", "driver", "-"),
        ("hold", "1", "0"),
        ("input", "pending", "clear"),
        ("need", "approval", "-"),
    ];
    let s = Scenario::new("aterm-held-fields");
    let fake = rostered_aterm(&s);
    fake.set_status(FIELDS[0].0, Some(FIELDS[0].1));
    let pty = Pty::open();
    let (_child, pid) = start_in_aterm(&s, &pty, &fake, 40);
    s.set_current(BUILD_2);
    let held = |line: &str| {
        assert!(
            wait_until(Duration::from_secs(15), || s
                .journal()
                .contains(&format!(" {line}\n"))),
            "never journalled {line:?}{}",
            s.report()
        );
        // Several looks under the hold.
        std::thread::sleep(Duration::from_millis(300));
        assert!(
            !got_term(&s) && s.starts().len() == 1,
            "moved under {line:?}{}",
            s.report()
        );
    };
    for (index, (key, holds, _)) in FIELDS.iter().enumerate() {
        if let Some((before, _, rest)) = index.checked_sub(1).map(|at| FIELDS[at]) {
            // The next field holds before the last one clears, so no moment
            // between them is safe; the event ends the wait on the old hold.
            fake.set_status(key, Some(holds));
            fake.set_status(before, Some(rest));
            fake.event("status");
        }
        // A halt is read first in the policy, which aterm turns off for it
        // (`why=hold`) from the same fact, and the claim stands through it.
        if *key == "hold" {
            held("wait aterm policy effective=off why=hold");
            assert_eq!(fake.holder(), Some(format!("claude-code-alab@{pid}")));
        } else {
            held(&format!("wait aterm {key}={holds}"));
        }
    }
    // The policy holds too: a person says when.
    fake.set_policy(&POLICY_ON.replace("upgrade_ask=false", "upgrade_ask=true"));
    let (last, _, rest) = FIELDS[FIELDS.len() - 1];
    fake.set_status(last, Some(rest));
    fake.event("status");
    held("wait aterm upgrade_ask=true");
    assert_eq!(fake.holder(), Some(format!("claude-code-alab@{pid}")));
    // Everything at rest: the move.
    fake.set_policy(POLICY_ON);
    assert!(
        wait_until(Duration::from_secs(15), || s.starts().len() >= 2),
        "no move once every field cleared{}",
        s.report()
    );
    assert_eq!(s.starts()[1].build(), Some(BUILD_2), "{}", s.report());
    end_session(&s, pid);
}

/// The claim replaces the harness's reading of the terminal's input and
/// nothing else. A record that is not idle and background work under the child
/// hold exactly as they do unclaimed, while aterm calls the session quiet, and
/// they hold before aterm is asked at all.
fn aterm_claim_keeps_the_record_and_background_work_checks() {
    let s = Scenario::new("aterm-kernel-checks");
    let fake = rostered_aterm(&s);
    s.put("status", "busy");
    let pty = Pty::open();
    let (_child, pid) = start_in_aterm(&s, &pty, &fake, 40);
    s.set_current(BUILD_2);
    std::thread::sleep(Duration::from_millis(2000));
    assert!(
        !got_term(&s) && s.starts().len() == 1,
        "ended a busy child on aterm's word{}",
        s.report()
    );
    assert!(s.journal().contains(" wait busy\n"), "{}", s.report());
    s.put("command", "bg-shell");
    assert!(
        wait_until(Duration::from_secs(5), || s
            .log()
            .iter()
            .any(|l| l.starts_with("bg-shell "))),
        "{}",
        s.report()
    );
    s.put("status", "idle");
    assert!(
        wait_until(Duration::from_secs(15), || s
            .journal()
            .contains(" wait background-work ")),
        "{}",
        s.report()
    );
    std::thread::sleep(Duration::from_millis(1500));
    assert!(
        !got_term(&s) && s.starts().len() == 1,
        "ended a child with background work on aterm's word{}",
        s.report()
    );
    assert_eq!(
        asked(&fake, "status"),
        1,
        "only the launch's proof read: the kernel checks come first: {:?}",
        fake.verbs()
    );
    assert!(!s.journal().contains("wait aterm"), "{}", s.report());
    s.put("command", "bg-stop");
    assert!(
        wait_until(Duration::from_secs(15), || s.starts().len() >= 2),
        "no move once the background work ended{}",
        s.report()
    );
    assert_eq!(s.starts()[1].build(), Some(BUILD_2), "{}", s.report());
    end_session(&s, pid);
}

/// aterm restarts: every connection drops and the claim with them. The harness
/// falls back to its own readings (which call typed input a draft and hold),
/// claims again through the same guarded sequence once the restarted aterm's
/// roster carries it, sends no claim before that, and after the move that
/// aterm's word then allows, announces the new vendor build.
fn aterm_restart_falls_back_reclaims_and_reannounces_the_vendor() {
    let s = Scenario::new("aterm-restart");
    let fake = rostered_aterm(&s);
    let pty = Pty::open();
    let (_child, pid) = start_in_aterm(&s, &pty, &fake, 40);
    assert_eq!(fake.holder(), Some(format!("claude-code-alab@{pid}")));
    // The restarted aterm does not carry the row until the scenario says so,
    // which keeps the link lost for as long as the fallback is being watched.
    fake.set_info(fake_aterm::INFO_UNROSTERED);
    fake.restart();
    assert!(
        wait_until(Duration::from_secs(10), || s
            .journal()
            .contains(" aterm lost ")),
        "the dropped link was never noticed{}",
        s.report()
    );
    pty.type_text("half a thought");
    assert!(
        wait_until(Duration::from_secs(5), || s
            .log()
            .iter()
            .any(|l| l.starts_with("read "))),
        "the fake never read the typing{}",
        s.report()
    );
    s.set_current(BUILD_2);
    // Long enough for a reclaim attempt (every 5 s) to meet the missing row.
    assert!(
        wait_until(Duration::from_secs(15), || s
            .journal()
            .contains(" aterm unclaimed unrostered\n")),
        "{}",
        s.report()
    );
    let journal = s.journal();
    assert!(
        journal.contains(" wait draft\n") || journal.contains(" wait typing\n"),
        "the lost link did not fall back to the tracker{}",
        s.report()
    );
    assert!(
        !got_term(&s) && s.starts().len() == 1,
        "moved over a draft with no aterm to say otherwise{}",
        s.report()
    );
    assert_eq!(
        claims_sent(&fake),
        1,
        "no claim without the row, on a reclaim either: {:?}",
        fake.verbs()
    );
    fake.set_info(INFO_ROSTERED);
    assert!(
        wait_until(Duration::from_secs(20), || s.starts().len() >= 2),
        "no reclaim and move{}",
        s.report()
    );
    assert_eq!(s.starts()[1].build(), Some(BUILD_2), "{}", s.report());
    let announce = claim_text(pid, "9.9.2");
    assert!(
        wait_until(Duration::from_secs(5), || asked(&fake, &announce) == 1),
        "the new vendor build was not announced: {:?}{}",
        fake.verbs(),
        s.report()
    );
    assert_eq!(
        asked(&fake, &claim_text(pid, "9.9.1")),
        2,
        "the launch's claim and the reclaim: {:?}",
        fake.verbs()
    );
    assert_eq!(fake.subscriptions(), 2, "{}", s.report());
    assert_eq!(fake.holder(), Some(format!("claude-code-alab@{pid}")));
    let order = [
        " aterm lost ",
        " aterm unclaimed unrostered\n",
        " end-for-update ",
        " aterm reannounced vendor=9.9.2\n",
    ]
    .map(|line| journal_at(&s, line));
    assert!(order.is_sorted(), "{order:?}{}", s.report());
    let journal = s.journal();
    assert_eq!(
        journal.matches(" aterm claimed ").count(),
        2,
        "{}",
        s.report()
    );
    assert_eq!(
        journal.matches(" aterm unclaimed ").count(),
        1,
        "one line for a reason that did not change{}",
        s.report()
    );
    end_session(&s, pid);
}

/// An event from aterm makes the next look due at once. With the longest poll
/// there is (5 s), a hold that clears is acted on well inside it once aterm
/// says something happened, and not a poll and a recheck later.
fn aterm_event_wakes_the_look_inside_a_long_poll() {
    let s = Scenario::new("aterm-event-wake");
    let fake = rostered_aterm(&s);
    fake.set_status("agent", Some("busy"));
    let pty = Pty::open();
    let (_child, pid) = start_in_aterm(&s, &pty, &fake, 5000);
    s.set_current(BUILD_2);
    assert!(
        wait_until(Duration::from_secs(30), || s
            .journal()
            .contains(" wait aterm agent=busy\n")),
        "{}",
        s.report()
    );
    // That line was written by a look, so the next poll is some 5 s away, and
    // the hold's own recheck as far.
    fake.set_status("agent", Some("idle"));
    let cleared = Instant::now();
    fake.event("agent");
    assert!(
        wait_until(Duration::from_secs(2), || got_term(&s)),
        "the event did not wake the look{}",
        s.report()
    );
    assert!(
        cleared.elapsed() < Duration::from_millis(2500),
        "{:?}",
        cleared.elapsed()
    );
    assert!(
        wait_until(Duration::from_secs(15), || s.starts().len() >= 2),
        "{}",
        s.report()
    );
    assert_eq!(s.starts()[1].build(), Some(BUILD_2), "{}", s.report());
    end_session(&s, pid);
}

/// aterm's policy is turned off under a standing claim: the harness gives the
/// claim back and decides on its own readings; turned on again, it claims
/// again, with the vendor build the child runs by then.
fn aterm_policy_turned_off_releases_the_claim() {
    let s = Scenario::new("aterm-policy-flip");
    let fake = rostered_aterm(&s);
    let pty = Pty::open();
    let (_child, pid) = start_in_aterm(&s, &pty, &fake, 40);
    assert_eq!(fake.holder(), Some(format!("claude-code-alab@{pid}")));
    fake.set_policy(&POLICY_ON.replace("effective=on why=enabled", "effective=off why=disabled"));
    assert!(
        wait_until(Duration::from_secs(10), || fake.holder().is_none()),
        "the claim outlived the policy{}",
        s.report()
    );
    assert_eq!(asked(&fake, &release_text(pid)), 1, "{:?}", fake.verbs());
    assert!(
        wait_until(Duration::from_secs(5), || s
            .journal()
            .contains(" aterm released policy effective=off why=disabled\n")),
        "{}",
        s.report()
    );
    // Unclaimed, the session moves on the harness's own readings and asks
    // aterm for no safe point.
    s.set_current(BUILD_2);
    assert!(
        wait_until(Duration::from_secs(15), || s.starts().len() >= 2),
        "no move on the harness's own readings{}",
        s.report()
    );
    assert_eq!(s.starts()[1].build(), Some(BUILD_2), "{}", s.report());
    assert_eq!(asked(&fake, "status"), 1, "{:?}", fake.verbs());
    assert_eq!(claims_sent(&fake), 1, "{:?}", fake.verbs());
    assert!(!s.journal().contains("wait aterm"), "{}", s.report());
    fake.set_policy(POLICY_ON);
    assert!(
        wait_until(Duration::from_secs(10), || fake.holder()
            == Some(format!("claude-code-alab@{pid}"))),
        "no claim once the policy was on again{}",
        s.report()
    );
    assert_eq!(
        asked(&fake, &claim_text(pid, "9.9.2")),
        1,
        "{:?}",
        fake.verbs()
    );
    assert_eq!(fake.subscriptions(), 1, "the event stream was kept");
    end_session(&s, pid);
}

// ───────────────────── crash relaunch and aterm's restart ─────────────────────

/// Where the fake vendor process `pid` writes its session record in `s`.
fn record_of(s: &Scenario, pid: i32) -> PathBuf {
    s.home.join(format!(".claude/sessions/{pid}.json"))
}

/// Wait for the fake's start number `nth` (from 0) and its session record, and
/// return its pid. A crash is relaunched only from a record, so every crash
/// scenario waits for it before it ends the fake.
fn await_record(s: &Scenario, nth: usize) -> i32 {
    assert!(
        wait_until(Duration::from_secs(15), || s
            .starts()
            .get(nth)
            .is_some_and(|start| record_of(s, start.pid).exists())),
        "start {nth} never wrote its record{}",
        s.report()
    );
    s.starts()[nth].pid
}

/// Give the fake `command` and wait for its log to say `line`: the moment the
/// fake is about to end, from which a pause is measured.
fn command_logged(s: &Scenario, command: &str, line: &str) -> Instant {
    s.put("command", command);
    assert!(
        wait_until(Duration::from_secs(5), || s.log().iter().any(|l| l == line)),
        "the fake never logged {line:?}{}",
        s.report()
    );
    Instant::now()
}

/// When (the harness's own clock, epoch ms) each line of `s`'s journal that
/// contains `needle` was written.
fn journal_times(s: &Scenario, needle: &str) -> Vec<u64> {
    s.journal()
        .lines()
        .filter(|line| line.contains(needle))
        .map(|line| {
            line.split(' ')
                .next()
                .and_then(|ms| ms.parse().ok())
                .unwrap_or_else(|| panic!("a journal line without its time: {line:?}"))
        })
        .collect()
}

/// How long (ms, by the harness's own clock) passed between crash number
/// `nth` (from 0) being noticed and the relaunch that answered it.
fn crash_pause_ms(s: &Scenario, nth: usize) -> u64 {
    let noticed = journal_times(s, " crash child=");
    let relaunched = journal_times(s, " relaunch child=");
    relaunched[nth] - noticed[nth]
}

/// Wait for `line` (after its timestamp and pid) in `s`'s journal.
fn await_journal(s: &Scenario, limit: Duration, line: &str) {
    assert!(
        wait_until(limit, || s.journal().contains(&format!(" {line}\n"))),
        "never journalled {line:?}{}",
        s.report()
    );
}

/// The vendor crashes twice under a proven claim that takes `relaunch`. Each
/// time the harness stays, waits the back-off's pause (about 1 s, then about
/// 10 s), reads aterm before and after the pause, and starts the same build
/// with `--resume <conversation>` on a terminal back in the modes it found,
/// saying so in one line. No signal is sent to anyone.
fn aterm_a_crash_is_relaunched_on_the_same_build_after_the_ladders_pause() {
    let s = Scenario::new("aterm-crash-relaunch");
    let fake = rostered_aterm(&s);
    let pty = Pty::open();
    let (_child, pid) = start_in_aterm(&s, &pty, &fake, 40);
    let first = await_record(&s, 0);
    let crashed = command_logged(&s, "crash:7", "crash 7");
    assert!(
        wait_until(Duration::from_secs(8), || s.starts().len() >= 2),
        "the crash was not relaunched{}",
        s.report()
    );
    assert!(
        crashed.elapsed() < Duration::from_secs(4),
        "the first pause is about 1 s, not {:?}{}",
        crashed.elapsed(),
        s.report()
    );
    let starts = s.starts();
    assert_eq!(starts[1].build(), Some(BUILD_1), "{}", s.report());
    assert_eq!(starts[1].args, ["--resume", SID], "{}", s.report());
    assert_ne!(starts[1].pid, first);
    assert_eq!(wait_status(pid, Duration::ZERO), None, "the harness stayed");
    await_journal(
        &s,
        Duration::from_secs(5),
        &format!(
            "crash-relaunch pid={} build={BUILD_1} streak=1",
            starts[1].pid
        ),
    );
    // The pause is counted from the child's end, which the harness journals
    // within a millisecond or two of reading it.
    let paused = crash_pause_ms(&s, 0);
    assert!(
        (990..3000).contains(&paused),
        "the first pause is about 1 s, not {paused} ms{}",
        s.report()
    );
    let journal = s.journal();
    for line in [
        format!(" crash child=Exited(7) session={SID} settle-ms=250\n"),
        format!(" crash record-stands session={SID} streak=0 pause-ms=1000\n"),
        format!(" relaunch child=Exited(7) from-build={BUILD_1} to-build={BUILD_1} "),
    ] {
        assert!(journal.contains(&line), "no {line:?}{}", s.report());
    }
    let order = [
        " crash child=Exited(7) ",
        " crash record-stands ",
        " relaunch child=Exited(7) ",
        " crash-relaunch pid=",
    ]
    .map(|line| journal_at(&s, line));
    assert!(order.is_sorted(), "{order:?}{}", s.report());
    assert!(
        wait_until(Duration::from_secs(5), || pty.output().contains(&format!(
            "claude-alab: claude 9.9.1 ended abnormally (exit 7); resuming conversation {SID}"
        ))),
        "the terminal was not told\n--- pty ---\n{}{}",
        pty.output(),
        s.report()
    );
    // aterm was read after the exit and again after the pause, and the same
    // vendor build was announced again.
    let announce = claim_text(pid, "9.9.1");
    assert!(
        wait_until(Duration::from_secs(5), || asked(&fake, &announce) == 2),
        "{:?}{}",
        fake.verbs(),
        s.report()
    );
    assert_eq!(
        asked(&fake, "status"),
        3,
        "the launch's proof, the exit and the end of the pause: {:?}",
        fake.verbs()
    );
    assert_eq!(asked(&fake, "harness policy"), 3, "{:?}", fake.verbs());

    // The second crash, by a signal, one step up the back-off.
    await_record(&s, 1);
    let crashed = command_logged(&s, "die:9", "die 9");
    // A TSTP sent to the harness while it pauses finds no child to follow or
    // to pass it to: the harness does not stop, and the pause runs.
    await_journal(
        &s,
        Duration::from_secs(5),
        &format!("crash record-stands session={SID} streak=1 pause-ms=10000"),
    );
    signal(pid, libc::SIGTSTP);
    assert_eq!(
        wait_status(pid, Duration::from_millis(600)),
        None,
        "the harness stopped or ended{}",
        s.report()
    );
    assert!(
        wait_until(Duration::from_secs(16), || s.starts().len() >= 3),
        "the second crash was not relaunched{}",
        s.report()
    );
    assert!(
        crashed.elapsed() < Duration::from_secs(13),
        "the second pause is about 10 s, not {:?}{}",
        crashed.elapsed(),
        s.report()
    );
    let starts = s.starts();
    assert_eq!(starts[2].build(), Some(BUILD_1), "{}", s.report());
    assert_eq!(starts[2].args, ["--resume", SID], "{}", s.report());
    await_journal(
        &s,
        Duration::from_secs(5),
        &format!(
            "crash-relaunch pid={} build={BUILD_1} streak=2",
            starts[2].pid
        ),
    );
    let paused = crash_pause_ms(&s, 1);
    assert!(
        (9990..12_000).contains(&paused),
        "the second pause is about 10 s, not {paused} ms{}",
        s.report()
    );
    let journal = s.journal();
    assert!(
        journal.contains(&format!(
            " crash record-stands session={SID} streak=1 pause-ms=10000\n"
        )) && journal.contains(" crash child=Signaled(9) "),
        "{}",
        s.report()
    );
    assert!(
        wait_until(Duration::from_secs(5), || pty
            .output()
            .contains("ended abnormally (signal 9); resuming conversation")),
        "--- pty ---\n{}{}",
        pty.output(),
        s.report()
    );
    // Every child found the terminal as the harness found it, though each
    // crashed child left it raw; and nobody was signalled. The third child
    // logs its modes after the harness has written the notice above.
    assert!(
        wait_until(Duration::from_secs(5), || s
            .log()
            .iter()
            .filter(|l| l.starts_with("modes "))
            .count()
            >= 3),
        "{}",
        s.report()
    );
    let log = s.log();
    assert_eq!(
        log.iter().filter(|l| *l == "modes icanon=1").count(),
        3,
        "{}",
        s.report()
    );
    assert!(
        !log.iter().any(|l| l.starts_with("signal ")),
        "{}",
        s.report()
    );
    assert!(
        !journal.contains("crash-passed-on")
            && !journal.contains("signal passed-on")
            && !journal.contains("stopped following"),
        "{}",
        s.report()
    );
    assert_eq!(fake.holder(), Some(format!("claude-code-alab@{pid}")));
    assert_every_start_is_clean(&s, pid);
    // A HUP to the harness is passed on to the relaunched child, and the
    // session ends with it.
    await_record(&s, 2);
    signal(pid, libc::SIGHUP);
    assert_eq!(
        wait_status(pid, Duration::from_secs(10)),
        Some(Status::Exited(71)),
        "{}",
        s.report()
    );
    assert!(
        wait_until(Duration::from_secs(5), || fake.holder().is_none()),
        "{}",
        s.report()
    );
}

/// One session whose vendor ends by `command` (logged as `logged`) after
/// `prepare` changed what the fake aterm says, and is not brought back: the
/// harness ends as `status`, nothing was started a second time and nobody was
/// told of a resume. `why`, when given, is the one `crash-passed-on` line; when
/// absent the journal must not mention a crash at all.
fn crash_is_passed_on(
    name: &str,
    fake: &FakeAterm,
    s: &Scenario,
    prepare: impl FnOnce(&FakeAterm),
    (command, logged): (&str, &str),
    status: Status,
    why: Option<&str>,
) {
    let pty = Pty::open();
    let (_child, pid) = start_in_aterm(s, &pty, fake, 40);
    await_record(s, 0);
    prepare(fake);
    command_logged(s, command, logged);
    assert_eq!(
        wait_status(pid, Duration::from_secs(10)),
        Some(status),
        "{name}{}",
        s.report()
    );
    // Longer than the first pause: nothing comes back after the harness went.
    std::thread::sleep(Duration::from_millis(1500));
    assert_eq!(s.starts().len(), 1, "{name}{}", s.report());
    let journal = s.journal();
    assert!(
        !journal.contains("crash-relaunch") && !journal.contains(" relaunch "),
        "{name}{}",
        s.report()
    );
    match why {
        Some(why) => {
            assert!(
                journal.contains(&format!(" crash-passed-on {why}\n")),
                "{name}: no crash-passed-on {why:?}{}",
                s.report()
            );
            assert_eq!(journal.matches(" crash-passed-on ").count(), 1);
        }
        None => assert!(!journal.contains(" crash"), "{name}{}", s.report()),
    }
    assert!(
        !pty.output().contains("resuming conversation"),
        "{name}\n--- pty ---\n{}",
        pty.output()
    );
    assert!(!got_term(s), "{name}{}", s.report());
    assert!(
        wait_until(Duration::from_secs(5), || fake.holder().is_none()),
        "{name}: the claim outlived the harness{}",
        s.report()
    );
}

/// [`crash_is_passed_on`] in a fresh scenario under a proven claim.
fn claimed_crash_is_passed_on(
    name: &str,
    prepare: impl FnOnce(&FakeAterm),
    end: (&str, &str),
    status: Status,
    why: Option<&str>,
) {
    let s = Scenario::new(name);
    let fake = rostered_aterm(&s);
    crash_is_passed_on(name, &fake, &s, prepare, end, status, why);
}

/// An exit of 0 is the vendor leaving, even with its record left behind: no
/// crash is noticed and the harness exits 0.
fn aterm_an_exit_of_zero_is_not_a_crash() {
    claimed_crash_is_passed_on(
        "aterm-crash-exit-0",
        |_| {},
        ("crash:0", "crash 0"),
        Status::Exited(0),
        None,
    );
}

/// A non-zero exit that removed its own record is a graceful exit, as Claude
/// Code makes: nothing is waited for.
fn aterm_an_exit_that_removed_its_record_is_not_a_crash() {
    claimed_crash_is_passed_on(
        "aterm-crash-no-record",
        |_| {},
        ("exit:7", "exit 7"),
        Status::Exited(7),
        Some("no-record child=Exited(7)"),
    );
}

/// The record stood when the child was reaped and went within the settle: a
/// graceful exit that was still removing it. The harness has noticed the end
/// (its `crash child=` line) when this scenario removes the record, which
/// leaves it the 250 ms of the settle to do so.
fn aterm_a_record_removed_while_settling_is_not_a_crash() {
    let s = Scenario::new("aterm-crash-record-removed");
    let fake = rostered_aterm(&s);
    let pty = Pty::open();
    let (_child, pid) = start_in_aterm(&s, &pty, &fake, 40);
    let vendor = await_record(&s, 0);
    s.put("command", "crash:7");
    let until = Instant::now() + Duration::from_secs(10);
    while !s.journal().contains(" crash child=Exited(7) ") {
        assert!(Instant::now() < until, "no crash was noticed{}", s.report());
        std::thread::sleep(Duration::from_millis(1));
    }
    std::fs::remove_file(record_of(&s, vendor)).expect("remove the record");
    assert_eq!(
        wait_status(pid, Duration::from_secs(10)),
        Some(Status::Exited(7)),
        "{}",
        s.report()
    );
    std::thread::sleep(Duration::from_millis(1500));
    assert_eq!(s.starts().len(), 1, "{}", s.report());
    let journal = s.journal();
    assert!(
        journal.contains(" crash-passed-on record-removed\n")
            && !journal.contains("crash-relaunch")
            && !journal.contains(" crash record-stands "),
        "{}",
        s.report()
    );
    assert_eq!(
        asked(&fake, "status"),
        1,
        "aterm was not asked: {:?}",
        fake.verbs()
    );
}

/// `[harness] relaunch = false`: the crash is the session's end.
fn aterm_a_crash_is_passed_on_when_the_policy_forbids_a_relaunch() {
    claimed_crash_is_passed_on(
        "aterm-crash-limited",
        |fake| fake.set_policy(&POLICY_ON.replace("relaunch=true", "relaunch=false")),
        ("crash:7", "crash 7"),
        Status::Exited(7),
        Some("relaunch=false"),
    );
}

/// A halt stands on the session (`hold=1`, and so `effective=off why=hold` in
/// aterm's policy): the exit is the holder's.
fn aterm_a_crash_is_passed_on_under_a_hold() {
    claimed_crash_is_passed_on(
        "aterm-crash-held",
        |fake| fake.set_status("hold", Some("1")),
        ("die:9", "die 9"),
        Status::Signaled(9),
        Some("policy effective=off why=hold"),
    );
}

/// A driver holds the session (`hand=turn:<id>:<driver>`): the exit is the
/// driver's.
fn aterm_a_crash_is_passed_on_under_a_drivers_hand() {
    claimed_crash_is_passed_on(
        "aterm-crash-driven",
        |fake| fake.set_status("hand", Some("turn:7:s-12")),
        ("crash:7", "crash 7"),
        Status::Exited(7),
        Some("held hold=0 hand=turn:7:s-12 human_ms=-"),
    );
}

/// A person typed a second ago, inside the policy's 120 s grace: the exit is
/// theirs.
fn aterm_a_crash_is_passed_on_when_a_person_just_typed() {
    claimed_crash_is_passed_on(
        "aterm-crash-person",
        |fake| fake.set_status("human_ms", Some("1000")),
        ("crash:7", "crash 7"),
        Status::Exited(7),
        Some("person human_ms=1000"),
    );
}

/// The claim was proven at launch and `status` no longer shows it proven when
/// the vendor crashes: without the claim the relaunch is not this harness's
/// to make.
fn aterm_a_crash_is_passed_on_when_the_claim_is_no_longer_proven() {
    claimed_crash_is_passed_on(
        "aterm-crash-unproven",
        |fake| fake.set_proven(false),
        ("crash:7", "crash 7"),
        Status::Exited(7),
        Some("claim supervisor_proven=0"),
    );
}

/// aterm's policy was turned off under the claim and the harness gave the
/// claim back. A crash after that is not this harness's to answer, and aterm
/// is not asked: no claim stands to ask about.
fn aterm_a_crash_after_the_claim_was_released_is_passed_on() {
    let s = Scenario::new("aterm-crash-released");
    let fake = rostered_aterm(&s);
    crash_is_passed_on(
        "aterm-crash-released",
        &fake,
        &s,
        |fake| {
            fake.set_policy(
                &POLICY_ON.replace("effective=on why=enabled", "effective=off why=disabled"),
            );
            assert!(
                wait_until(Duration::from_secs(10), || fake.holder().is_none()),
                "the claim outlived the policy"
            );
        },
        ("crash:7", "crash 7"),
        Status::Exited(7),
        Some("no-claim"),
    );
    assert_eq!(
        asked(&fake, "status"),
        1,
        "only the launch's proof read: {:?}",
        fake.verbs()
    );
}

/// Today's aterm, whose roster has no row for this harness: no claim was ever
/// made, and a crash ends the session exactly as it does outside aterm, with
/// no settle, no question to aterm and no line about a crash.
fn aterm_a_crash_with_no_claim_ends_the_session_as_it_does_outside_aterm() {
    let s = Scenario::new("aterm-crash-unclaimed");
    let fake = FakeAterm::new(&s.root);
    crash_is_passed_on(
        "aterm-crash-unclaimed",
        &fake,
        &s,
        |_| {},
        ("crash:7", "crash 7"),
        Status::Exited(7),
        None,
    );
    assert_eq!(
        fake.verbs(),
        ["harness policy", "native-act info"],
        "nothing was asked at the exit{}",
        s.report()
    );
}

/// HUP, TERM, INT or QUIT reaches the harness while it pauses before a crash
/// relaunch. There is no child to pass it to, and whoever sent it wants the
/// session over: the harness ends with the crashed child's status and starts
/// nothing.
fn aterm_a_signal_during_the_crash_pause_ends_the_harness_as_the_child_ended() {
    for (sig, name) in [
        (libc::SIGHUP, "hup"),
        (libc::SIGTERM, "term"),
        (libc::SIGINT, "int"),
        (libc::SIGQUIT, "quit"),
    ] {
        let s = Scenario::new(&format!("aterm-crash-pause-{name}"));
        let fake = rostered_aterm(&s);
        let pty = Pty::open();
        let (_child, pid) = start_in_aterm(&s, &pty, &fake, 40);
        await_record(&s, 0);
        s.put("command", "crash:7");
        // The pause is running once the record is known to have survived.
        await_journal(
            &s,
            Duration::from_secs(5),
            &format!("crash record-stands session={SID} streak=0 pause-ms=1000"),
        );
        signal(pid, sig);
        assert_eq!(
            wait_status(pid, Duration::from_secs(5)),
            Some(Status::Exited(7)),
            "{name}{}",
            s.report()
        );
        std::thread::sleep(Duration::from_millis(1500));
        assert_eq!(s.starts().len(), 1, "{name}{}", s.report());
        let journal = s.journal();
        assert!(
            journal.contains(&format!(" crash-passed-on signal {sig}\n"))
                && journal.contains(" exit child=Exited(7)\n")
                && !journal.contains("crash-relaunch")
                && !journal.contains("signal passed-on"),
            "{name}{}",
            s.report()
        );
        // The fake logs every signal it is sent; it was sent none, alive or dead.
        assert!(
            !s.log().iter().any(|l| l.starts_with("signal ")),
            "{name}{}",
            s.report()
        );
        assert!(
            wait_until(Duration::from_secs(5), || fake.holder().is_none()),
            "{name}{}",
            s.report()
        );
    }
}

/// aterm restarted and the link is lost when the vendor crashes. The harness
/// waits for its claim to be proven again (the loop's own 5 s reclaim clock),
/// then asks, and relaunches. The pause ran meanwhile.
fn aterm_a_crash_with_the_link_lost_waits_for_the_claim_to_be_proven_again() {
    let s = Scenario::new("aterm-crash-reclaim");
    let fake = rostered_aterm(&s);
    let pty = Pty::open();
    let (_child, pid) = start_in_aterm(&s, &pty, &fake, 40);
    await_record(&s, 0);
    fake.restart();
    assert!(
        wait_until(Duration::from_secs(10), || s
            .journal()
            .contains(" aterm lost ")),
        "the dropped link was never noticed{}",
        s.report()
    );
    assert_eq!(fake.holder(), None);
    let crashed = command_logged(&s, "crash:7", "crash 7");
    await_journal(
        &s,
        Duration::from_secs(5),
        "crash waiting-for-claim hold-ms=45000 link-lost",
    );
    assert_eq!(s.starts().len(), 1, "started with no claim{}", s.report());
    assert!(
        wait_until(Duration::from_secs(15), || s.starts().len() >= 2),
        "no relaunch once the claim was proven again{}",
        s.report()
    );
    assert!(
        crashed.elapsed() >= Duration::from_millis(900),
        "{:?}",
        crashed.elapsed()
    );
    assert_eq!(s.starts()[1].build(), Some(BUILD_1), "{}", s.report());
    assert_eq!(s.starts()[1].args, ["--resume", SID], "{}", s.report());
    let journal = s.journal();
    let waiting = journal_at(&s, " crash waiting-for-claim ");
    let claimed_again = journal
        .rfind(" aterm claimed ")
        .expect("claimed at launch and again");
    assert!(
        journal_at(&s, " aterm lost ") < waiting
            && waiting < claimed_again
            && claimed_again < journal_at(&s, " relaunch child=Exited(7) "),
        "{}",
        s.report()
    );
    assert_eq!(
        journal.matches(" aterm claimed ").count(),
        2,
        "{}",
        s.report()
    );
    assert_eq!(journal.matches(" crash waiting-for-claim ").count(), 1);
    assert_eq!(fake.holder(), Some(format!("claude-code-alab@{pid}")));
    end_session(&s, pid);
}

/// The link is lost and the restarted aterm never proves the claim again (its
/// roster has no row): after 45 s the crash is passed on, and the harness
/// ends with the crashed child's status rather than wait childless for ever.
fn aterm_a_crash_whose_claim_is_not_proven_again_is_passed_on() {
    let s = Scenario::new("aterm-crash-reclaim-expired");
    let fake = rostered_aterm(&s);
    let pty = Pty::open();
    let (_child, pid) = start_in_aterm(&s, &pty, &fake, 40);
    await_record(&s, 0);
    fake.set_info(fake_aterm::INFO_UNROSTERED);
    fake.restart();
    assert!(
        wait_until(Duration::from_secs(10), || s
            .journal()
            .contains(" aterm lost ")),
        "the dropped link was never noticed{}",
        s.report()
    );
    let crashed = command_logged(&s, "crash:7", "crash 7");
    assert_eq!(
        wait_status(pid, Duration::from_secs(60)),
        Some(Status::Exited(7)),
        "{}",
        s.report()
    );
    let held = crashed.elapsed();
    assert!(
        held >= Duration::from_secs(44) && held < Duration::from_secs(55),
        "the hold is 45 s, not {held:?}{}",
        s.report()
    );
    assert_eq!(s.starts().len(), 1, "{}", s.report());
    let journal = s.journal();
    assert!(
        journal.contains(" crash waiting-for-claim hold-ms=45000 link-lost\n")
            && journal.contains(" crash-passed-on claim-not-proven-again link-lost\n")
            && journal.contains(" aterm unclaimed unrostered\n")
            && !journal.contains("crash-relaunch"),
        "{}",
        s.report()
    );
    assert_eq!(
        claims_sent(&fake),
        1,
        "no claim without the row: {:?}",
        fake.verbs()
    );
}

/// aterm asks for a restart (`restart cause=asked`). Under the proven claim
/// and a fresh permission read the harness sends the vendor exactly one
/// SIGTERM, at once, while the session is busy and aterm sees a draft (no safe
/// point is waited for), and starts the same build on the same conversation.
/// It is no crash: the next crash waits the back-off's first pause.
fn aterm_a_restart_request_ends_the_child_once_and_resumes_on_the_same_build() {
    let s = Scenario::new("aterm-restart-asked");
    let fake = rostered_aterm(&s);
    let pty = Pty::open();
    let (_child, pid) = start_in_aterm(&s, &pty, &fake, 40);
    let first = await_record(&s, 0);
    // Nothing an update would accept: a busy record, a busy agent, a draft, a
    // person at the keys.
    s.put("status", "busy");
    fake.set_status("agent", Some("busy"));
    fake.set_status("composer", Some("draft"));
    fake.set_status("human_ms", Some("1000"));
    std::thread::sleep(Duration::from_millis(200));
    let before = (asked(&fake, "harness policy"), asked(&fake, "status"));
    fake.send_restart("asked");
    assert!(
        wait_until(Duration::from_secs(8), || s.starts().len() >= 2),
        "the restart was not made{}",
        s.report()
    );
    let starts = s.starts();
    assert_eq!(starts[1].build(), Some(BUILD_1), "{}", s.report());
    assert_eq!(starts[1].args, ["--resume", SID], "{}", s.report());
    assert_eq!(
        s.log().iter().filter(|l| *l == "signal TERM").count(),
        1,
        "exactly one SIGTERM{}",
        s.report()
    );
    assert_eq!(wait_status(pid, Duration::ZERO), None, "the harness stayed");
    let journal = s.journal();
    assert!(
        journal.contains(&format!(
            " aterm restart cause=asked SIGTERM pid={first} session={SID} build={BUILD_1} \
             argv=--resume\\u{{1f}}{SID}\n"
        )),
        "{}",
        s.report()
    );
    assert!(
        journal.contains(&format!(
            " relaunch child=Exited(70) from-build={BUILD_1} to-build={BUILD_1} "
        )) && !journal.contains(" crash")
            && !journal.contains("ignored")
            && !journal.contains("end-for-update"),
        "{}",
        s.report()
    );
    assert!(
        wait_until(Duration::from_secs(5), || pty.output().contains(&format!(
            "claude-alab: claude 9.9.1 restarted at aterm's request (asked); resuming \
             conversation {SID}"
        ))),
        "--- pty ---\n{}{}",
        pty.output(),
        s.report()
    );
    // One fresh policy and status read stood between the request and the signal.
    assert_eq!(
        (
            asked(&fake, "harness policy") - before.0,
            asked(&fake, "status") - before.1
        ),
        (1, 1),
        "{:?}",
        fake.verbs()
    );
    let announce = claim_text(pid, "9.9.1");
    assert!(
        wait_until(Duration::from_secs(5), || asked(&fake, &announce) == 2),
        "{:?}{}",
        fake.verbs(),
        s.report()
    );
    // The back-off did not move: a crash now waits its first step.
    s.put("status", "idle");
    for (key, value) in [("agent", "idle"), ("composer", "empty"), ("human_ms", "-")] {
        fake.set_status(key, Some(value));
    }
    await_record(&s, 1);
    command_logged(&s, "crash:7", "crash 7");
    await_journal(
        &s,
        Duration::from_secs(5),
        &format!("crash record-stands session={SID} streak=0 pause-ms=1000"),
    );
    assert!(
        wait_until(Duration::from_secs(8), || s.starts().len() >= 3),
        "{}",
        s.report()
    );
    assert_eq!(
        s.log().iter().filter(|l| *l == "signal TERM").count(),
        1,
        "{}",
        s.report()
    );
    assert_every_start_is_clean(&s, pid);
    // A TERM to the harness is passed on to the child it started last, and
    // the session ends with it.
    await_record(&s, 2);
    signal(pid, libc::SIGTERM);
    assert_eq!(
        wait_status(pid, Duration::from_secs(10)),
        Some(Status::Exited(70)),
        "{}",
        s.report()
    );
    assert!(
        s.journal()
            .contains(&format!(" signal passed-on {}\n", libc::SIGTERM)),
        "{}",
        s.report()
    );
}

/// The vendor does not end at the restart's SIGTERM: a stalled program may be
/// unable to run its handler. When the restart's grace is over (5 s, or a
/// shorter `--alab-term-grace-ms`) the harness sends the vendor alone SIGKILL,
/// as aterm's own restart act does, and starts the same build on the same
/// conversation: the restart aterm promised its asker is made.
fn aterm_a_restart_the_child_outlives_is_made_with_sigkill() {
    let s = Scenario::new("aterm-restart-sigkill");
    let fake = rostered_aterm(&s);
    s.put("term", "ignore");
    let pty = Pty::open();
    let (_child, pid) = start_in_aterm_with(&s, &pty, &fake, 40, &["--alab-term-grace-ms=600"]);
    let first = await_record(&s, 0);
    fake.send_restart("stall");
    assert!(
        wait_until(Duration::from_secs(10), || s.starts().len() >= 2),
        "the child that outlived SIGTERM was never restarted{}",
        s.report()
    );
    let starts = s.starts();
    assert_eq!(starts[1].build(), Some(BUILD_1), "{}", s.report());
    assert_eq!(starts[1].args, ["--resume", SID], "{}", s.report());
    assert_eq!(wait_status(pid, Duration::ZERO), None, "the harness stayed");
    let journal = s.journal();
    assert!(
        journal.contains(&format!(
            " restart-escalated cause=stall SIGKILL pid={first} the child kept running after \
             SIGTERM\n"
        )) && journal.contains(&format!(
            " relaunch child=Signaled(9) from-build={BUILD_1} to-build={BUILD_1} "
        )),
        "{}",
        s.report()
    );
    assert!(
        !journal.contains("restart-abandoned") && !journal.contains(" crash"),
        "{}",
        s.report()
    );
    let order = [
        " aterm restart cause=stall SIGTERM ",
        " restart-escalated ",
        " relaunch child=Signaled(9) ",
    ]
    .map(|line| journal_at(&s, line));
    assert!(order.is_sorted(), "{order:?}{}", s.report());
    assert_eq!(
        s.log().iter().filter(|l| *l == "signal TERM").count(),
        1,
        "one SIGTERM, then SIGKILL{}",
        s.report()
    );
    assert!(
        wait_until(Duration::from_secs(5), || pty.output().contains(&format!(
            "claude-alab: claude 9.9.1 restarted at aterm's request (stall); resuming \
             conversation {SID}"
        ))),
        "--- pty ---\n{}{}",
        pty.output(),
        s.report()
    );
    std::fs::remove_file(s.fake.join("term")).expect("heed TERM again");
    await_record(&s, 1);
    end_session(&s, pid);
}

/// The vendor stops (a job stop) right after the restart's SIGTERM, which it
/// cannot act on while stopped, and the harness stops with it. The time the
/// job spent stopped is not counted against the restart's grace: once the job
/// is continued the vendor has the whole grace again before SIGKILL.
fn aterm_a_restart_grace_does_not_run_while_the_job_is_stopped() {
    let s = Scenario::new("aterm-restart-stopped");
    let fake = rostered_aterm(&s);
    s.put("term", "ignore");
    let pty = Pty::open();
    let (_child, pid) = start_in_aterm_with(&s, &pty, &fake, 40, &["--alab-term-grace-ms=1500"]);
    let vendor = await_record(&s, 0);
    fake.send_restart("asked");
    let asked_line = format!(" aterm restart cause=asked SIGTERM pid={vendor} ");
    assert!(
        wait_until(Duration::from_secs(5), || s.journal().contains(&asked_line)),
        "{}",
        s.report()
    );
    signal(vendor, libc::SIGSTOP);
    assert_eq!(
        wait_status(pid, Duration::from_secs(5)),
        Some(Status::Stopped),
        "the harness did not follow the stop{}",
        s.report()
    );
    // Longer than the grace, stopped.
    std::thread::sleep(Duration::from_millis(2500));
    signal(pid, libc::SIGCONT);
    assert_eq!(
        wait_status(pid, Duration::from_secs(5)),
        Some(Status::Continued)
    );
    await_journal(&s, Duration::from_secs(5), "continued");
    assert!(
        wait_until(Duration::from_secs(10), || s.starts().len() >= 2),
        "{}",
        s.report()
    );
    let continued = journal_times(&s, " continued")[0];
    let escalated = journal_times(&s, " restart-escalated ")[0];
    assert!(
        escalated.saturating_sub(continued) >= 1400,
        "SIGKILL {} ms after the job was continued, inside the grace{}",
        escalated.saturating_sub(continued),
        s.report()
    );
    std::fs::remove_file(s.fake.join("term")).expect("heed TERM again");
    await_record(&s, 1);
    end_session(&s, pid);
}

/// A restart is asked for before the vendor has written its session record:
/// there is no conversation to resume yet. The one request waits, journalled
/// once, with no signal sent; when the record appears it is made.
fn aterm_a_restart_request_waits_for_the_session_record() {
    let s = Scenario::new("aterm-restart-deferred");
    let fake = rostered_aterm(&s);
    s.put("record-hold", "");
    let pty = Pty::open();
    let (_child, pid) = start_in_aterm(&s, &pty, &fake, 40);
    assert!(
        wait_until(Duration::from_secs(5), || fake.subscriptions() == 1),
        "{}",
        s.report()
    );
    let first = s.starts()[0].pid;
    assert!(!record_of(&s, first).exists(), "{}", s.report());
    fake.send_restart("stall");
    await_journal(
        &s,
        Duration::from_secs(5),
        "aterm restart cause=stall deferred no-record",
    );
    // Many polls later: still waiting, still one line, nothing sent or asked.
    std::thread::sleep(Duration::from_millis(800));
    assert!(!got_term(&s) && s.starts().len() == 1, "{}", s.report());
    assert_eq!(
        s.journal().matches(" deferred ").count(),
        1,
        "{}",
        s.report()
    );
    assert_eq!(asked(&fake, "status"), 1, "{:?}", fake.verbs());
    std::fs::remove_file(s.fake.join("record-hold")).expect("release the record");
    assert!(
        wait_until(Duration::from_secs(8), || s.starts().len() >= 2),
        "the waiting restart was never made{}",
        s.report()
    );
    assert_eq!(s.starts()[1].build(), Some(BUILD_1), "{}", s.report());
    assert_eq!(s.starts()[1].args, ["--resume", SID], "{}", s.report());
    assert_eq!(
        s.log().iter().filter(|l| *l == "signal TERM").count(),
        1,
        "{}",
        s.report()
    );
    let journal = s.journal();
    assert!(
        journal.contains(&format!(
            " aterm restart cause=stall SIGTERM pid={first} session={SID} "
        )) && !journal.contains("ignored"),
        "{}",
        s.report()
    );
    // The request was made once: the new child is left alone.
    await_record(&s, 1);
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(s.starts().len(), 2, "{}", s.report());
    end_session(&s, pid);
}

/// A restart request is a request, never permission. With a driver's hand on
/// the session, which aterm's own restart act refuses and leaves a `relaunch`
/// claim holder to refuse, and again with a claim `status` no longer shows
/// proven, it is journalled as ignored with the reason and the vendor is not
/// signalled.
fn aterm_a_restart_request_is_ignored_under_a_drivers_hand_or_without_the_claim() {
    let s = Scenario::new("aterm-restart-ignored");
    let fake = rostered_aterm(&s);
    let pty = Pty::open();
    let (_child, pid) = start_in_aterm(&s, &pty, &fake, 40);
    await_record(&s, 0);
    fake.set_status("hand", Some("lease:driver@4"));
    fake.send_restart("asked");
    await_journal(
        &s,
        Duration::from_secs(5),
        "aterm restart cause=asked ignored held hold=0 hand=lease:driver@4 human_ms=-",
    );
    fake.set_status("hand", Some("-"));
    fake.set_proven(false);
    fake.send_restart("stall");
    await_journal(
        &s,
        Duration::from_secs(5),
        "aterm restart cause=stall ignored claim supervisor_proven=0",
    );
    std::thread::sleep(Duration::from_millis(500));
    assert!(!got_term(&s) && s.starts().len() == 1, "{}", s.report());
    assert_eq!(wait_status(pid, Duration::ZERO), None);
    assert!(!s.journal().contains("SIGTERM"), "{}", s.report());
    end_session(&s, pid);
}

/// `[harness] relaunch = false` limits crashes only: aterm's own host
/// relaunches an asked exit whatever the switch says, and has already told
/// the asker the restart is under way. The restart is made.
fn aterm_a_restart_request_is_made_whatever_the_relaunch_switch_says() {
    let s = Scenario::new("aterm-restart-switch-off");
    let fake = rostered_aterm(&s);
    let pty = Pty::open();
    let (_child, pid) = start_in_aterm(&s, &pty, &fake, 40);
    await_record(&s, 0);
    fake.set_policy(&POLICY_ON.replace("relaunch=true", "relaunch=false"));
    fake.send_restart("asked");
    assert!(
        wait_until(Duration::from_secs(8), || s.starts().len() >= 2),
        "the restart was not made{}",
        s.report()
    );
    assert_eq!(s.starts()[1].args, ["--resume", SID], "{}", s.report());
    assert!(!s.journal().contains("ignored"), "{}", s.report());
    // A crash under the same switch is still the session's end.
    await_record(&s, 1);
    command_logged(&s, "crash:7", "crash 7");
    assert_eq!(
        wait_status(pid, Duration::from_secs(10)),
        Some(Status::Exited(7)),
        "{}",
        s.report()
    );
    assert!(
        s.journal().contains(" crash-passed-on relaunch=false\n"),
        "{}",
        s.report()
    );
}

/// What aterm answers while it shows a dialog or its main thread is stalled.
const ATERM_BUSY: &str = "ERR aterm is showing a dialog; answer it, then retry";

/// aterm asks for a restart and then cannot read its own state for a moment
/// (a dialog is up): the request waits for its answer, journalled once, with
/// nothing sent, and is made once aterm answers.
fn aterm_a_restart_aterm_cannot_answer_yet_waits_for_its_answer() {
    let s = Scenario::new("aterm-restart-later");
    let fake = rostered_aterm(&s);
    let pty = Pty::open();
    let (_child, pid) = start_in_aterm(&s, &pty, &fake, 40);
    await_record(&s, 0);
    fake.set_policy_reply(ATERM_BUSY);
    fake.send_restart("asked");
    await_journal(
        &s,
        Duration::from_secs(5),
        "aterm restart cause=asked deferred aterm policy reply=ERR:aterm",
    );
    std::thread::sleep(Duration::from_millis(2000));
    assert!(!got_term(&s) && s.starts().len() == 1, "{}", s.report());
    assert_eq!(
        s.journal().matches(" deferred ").count(),
        1,
        "{}",
        s.report()
    );
    fake.clear_policy_reply();
    assert!(
        wait_until(Duration::from_secs(8), || s.starts().len() >= 2),
        "the waiting restart was never made{}",
        s.report()
    );
    assert_eq!(s.starts()[1].args, ["--resume", SID], "{}", s.report());
    let journal = s.journal();
    assert!(
        !journal.contains("ignored") && !journal.contains(" aterm lost "),
        "{}",
        s.report()
    );
    end_session(&s, pid);
}

/// The vendor crashes while aterm cannot read its own state (a dialog is up):
/// its `ERR … retry` is no refusal. The crash waits for aterm's answer, asking
/// again, with the claim and the link kept, and is relaunched once aterm
/// answers.
fn aterm_a_crash_waits_while_aterm_cannot_answer() {
    let s = Scenario::new("aterm-crash-later");
    let fake = rostered_aterm(&s);
    let pty = Pty::open();
    let (_child, pid) = start_in_aterm(&s, &pty, &fake, 40);
    await_record(&s, 0);
    fake.set_policy_reply(ATERM_BUSY);
    command_logged(&s, "crash:7", "crash 7");
    await_journal(
        &s,
        Duration::from_secs(5),
        "crash waiting-for-aterm hold-ms=45000 policy reply=ERR:aterm",
    );
    std::thread::sleep(Duration::from_millis(2500));
    assert_eq!(s.starts().len(), 1, "{}", s.report());
    assert_eq!(wait_status(pid, Duration::ZERO), None, "{}", s.report());
    let asked_meanwhile = asked(&fake, "harness policy");
    assert!(
        asked_meanwhile >= 3,
        "aterm was not asked again: {:?}",
        fake.verbs()
    );
    fake.clear_policy_reply();
    assert!(
        wait_until(Duration::from_secs(8), || s.starts().len() >= 2),
        "no relaunch once aterm answered{}",
        s.report()
    );
    assert_eq!(s.starts()[1].args, ["--resume", SID], "{}", s.report());
    let journal = s.journal();
    assert!(
        !journal.contains("crash-passed-on") && !journal.contains(" aterm lost "),
        "{}",
        s.report()
    );
    assert_eq!(fake.holder(), Some(format!("claude-code-alab@{pid}")));
    end_session(&s, pid);
}

/// aterm cannot read its own state when the session starts: the launch is
/// unclaimed for now, not for its whole life, and is claimed through the
/// whole guarded sequence once aterm answers.
fn aterm_a_launch_aterm_cannot_answer_yet_is_claimed_once_it_can() {
    let s = Scenario::new("aterm-launch-later");
    let fake = rostered_aterm(&s);
    fake.set_policy_reply(ATERM_BUSY);
    let pty = Pty::open();
    let (_child, pid) = start_in_aterm(&s, &pty, &fake, 40);
    assert!(
        s.journal()
            .contains(" aterm unclaimed policy reply=ERR:aterm\n"),
        "{}",
        s.report()
    );
    assert_eq!(fake.holder(), None);
    fake.clear_policy_reply();
    assert!(
        wait_until(Duration::from_secs(10), || fake.holder()
            == Some(format!("claude-code-alab@{pid}"))),
        "never claimed once aterm answered{}",
        s.report()
    );
    assert_eq!(
        asked(&fake, &claim_text(pid, "9.9.1")),
        1,
        "{:?}",
        fake.verbs()
    );
    assert_eq!(
        s.journal().matches(" aterm unclaimed ").count(),
        1,
        "{}",
        s.report()
    );
    end_session(&s, pid);
}

/// aterm answers the launch's policy read only after the request's bound (its
/// main thread busy, say): the timed-out connection is closed unread, and the
/// launch is unclaimed for now, not for its whole life. Once aterm answers in
/// time the whole guarded sequence claims on a fresh connection.
fn aterm_a_launch_aterm_did_not_answer_in_time_is_claimed_once_it_does() {
    let s = Scenario::new("aterm-launch-late");
    let fake = rostered_aterm(&s);
    fake.set_reply_delay(Duration::from_millis(800));
    let pty = Pty::open();
    let (_child, pid) = start_in_aterm(&s, &pty, &fake, 40);
    assert!(
        wait_until(Duration::from_secs(10), || s
            .journal()
            .contains(" aterm unclaimed policy ")),
        "the late policy read was not journalled{}",
        s.report()
    );
    assert_eq!(fake.holder(), None);
    fake.set_reply_delay(Duration::ZERO);
    assert!(
        wait_until(Duration::from_secs(15), || fake.holder()
            == Some(format!("claude-code-alab@{pid}"))),
        "never claimed once aterm answered in time{}",
        s.report()
    );
    await_journal(
        &s,
        Duration::from_secs(5),
        &format!("aterm claimed holder=claude-code-alab@{pid} scope=relaunch,upgrade vendor=9.9.1"),
    );
    assert_eq!(
        asked(&fake, &claim_text(pid, "9.9.1")),
        1,
        "{:?}",
        fake.verbs()
    );
    let journal = s.journal();
    assert_eq!(
        journal.matches(" aterm unclaimed ").count(),
        1,
        "{}",
        s.report()
    );
    assert_eq!(
        journal.matches(" aterm claimed ").count(),
        1,
        "{}",
        s.report()
    );
    end_session(&s, pid);
}

/// aterm's `subscribe` usage, which it answers a request it does not read
/// (`aterm/crates/aterm-gui/src/control.rs`, `SUBSCRIBE_USAGE`).
const SUBSCRIBE_USAGE: &str =
    "ERR usage: subscribe [@<sel>[,@<sel>...]|@*] <streams> [lines=N] [log_seq=N]";

/// What aterm answers `subscribe` while every subscription lane is taken
/// (`aterm/crates/aterm-gui/src/control_lanes.rs`, `SUBSCRIPTION_BUSY_LINE`).
const SUBSCRIBE_BUSY: &str = "ERR subscription capacity busy; retry";

/// Longer than one reclaim period (5 s), so a retry would have been made.
const PAST_A_RECLAIM: Duration = Duration::from_secs(7);

/// aterm refuses the launch's event subscription with an answer that stands.
/// The claim the sequence made is given back, once, and the session runs on
/// the harness's own readings for its whole life: claiming again every 5 s
/// would take aterm's engine's duties and return them each time.
fn aterm_a_launch_whose_subscription_aterm_refuses_is_not_claimed_again() {
    let s = Scenario::new("aterm-launch-unsubscribed");
    let fake = rostered_aterm(&s);
    fake.set_subscribe_reply(Some(SUBSCRIBE_USAGE));
    let pty = Pty::open();
    let (_child, pid) = start_in_aterm(&s, &pty, &fake, 40);
    await_journal(
        &s,
        Duration::from_secs(5),
        "aterm unclaimed events reply=ERR:usage:",
    );
    assert!(
        !wait_until(PAST_A_RECLAIM, || claims_sent(&fake) > 1),
        "claimed again after a refusal that stands: {:?}{}",
        fake.verbs(),
        s.report()
    );
    assert_eq!(
        asked(&fake, &claim_text(pid, "9.9.1")),
        1,
        "{:?}",
        fake.verbs()
    );
    assert_eq!(asked(&fake, &release_text(pid)), 1, "{:?}", fake.verbs());
    assert_eq!(fake.subscribe_asks(), 1, "{}", s.report());
    assert_eq!(fake.holder(), None);
    let journal = s.journal();
    assert_eq!(
        journal.matches(" aterm unclaimed ").count(),
        1,
        "{}",
        s.report()
    );
    assert!(!journal.contains(" aterm claimed "), "{}", s.report());
    end_session(&s, pid);
}

/// aterm's subscription lanes are all taken at launch (`ERR subscription
/// capacity busy; retry`): the claim is given back and the launch is unclaimed
/// for now. Once a lane frees the whole guarded sequence claims and subscribes.
fn aterm_a_launch_whose_subscription_lanes_are_full_is_claimed_once_one_frees() {
    let s = Scenario::new("aterm-launch-lanes-full");
    let fake = rostered_aterm(&s);
    fake.set_subscribe_reply(Some(SUBSCRIBE_BUSY));
    let pty = Pty::open();
    let (_child, pid) = start_in_aterm(&s, &pty, &fake, 40);
    await_journal(
        &s,
        Duration::from_secs(5),
        "aterm unclaimed events reply=ERR:subscription",
    );
    assert_eq!(fake.holder(), None);
    fake.set_subscribe_reply(None);
    let holder = Some(format!("claude-code-alab@{pid}"));
    assert!(
        wait_until(Duration::from_secs(15), || fake.holder() == holder
            && fake.subscriptions() == 1),
        "never claimed once a lane freed{}",
        s.report()
    );
    await_journal(
        &s,
        Duration::from_secs(5),
        &format!("aterm claimed holder=claude-code-alab@{pid} scope=relaunch,upgrade vendor=9.9.1"),
    );
    let claims = asked(&fake, &claim_text(pid, "9.9.1"));
    let releases = asked(&fake, &release_text(pid));
    assert!(claims >= 2, "{:?}", fake.verbs());
    assert_eq!(
        releases,
        claims - 1,
        "each refused subscription gave its claim back: {:?}",
        fake.verbs()
    );
    let journal = s.journal();
    assert_eq!(
        journal.matches(" aterm unclaimed ").count(),
        1,
        "{}",
        s.report()
    );
    assert_eq!(
        journal.matches(" aterm claimed ").count(),
        1,
        "{}",
        s.report()
    );
    end_session(&s, pid);
}

/// aterm restarts under a held link and the restarted aterm refuses the event
/// subscription with an answer that stands. The reclaim gives its claim back
/// once and the link ends there, as it would at launch, instead of claiming
/// and releasing every 5 s.
fn aterm_a_reclaim_whose_subscription_aterm_refuses_ends_the_link() {
    let s = Scenario::new("aterm-reclaim-unsubscribed");
    let fake = rostered_aterm(&s);
    let pty = Pty::open();
    let (_child, pid) = start_in_aterm(&s, &pty, &fake, 40);
    assert_eq!(fake.holder(), Some(format!("claude-code-alab@{pid}")));
    fake.set_subscribe_reply(Some(SUBSCRIBE_USAGE));
    fake.restart();
    assert!(
        wait_until(Duration::from_secs(10), || s
            .journal()
            .contains(" aterm lost ")),
        "the dropped link was never noticed{}",
        s.report()
    );
    await_journal(
        &s,
        Duration::from_secs(15),
        "aterm unclaimed events reply=ERR:usage:",
    );
    assert!(
        !wait_until(PAST_A_RECLAIM, || claims_sent(&fake) > 2),
        "claimed again after a refusal that stands: {:?}{}",
        fake.verbs(),
        s.report()
    );
    assert_eq!(
        asked(&fake, &claim_text(pid, "9.9.1")),
        2,
        "the launch's claim and the one reclaim: {:?}",
        fake.verbs()
    );
    assert_eq!(asked(&fake, &release_text(pid)), 1, "{:?}", fake.verbs());
    assert_eq!(fake.holder(), None);
    assert_eq!(
        s.journal().matches(" aterm unclaimed ").count(),
        1,
        "{}",
        s.report()
    );
    end_session(&s, pid);
}

/// A person halts the session under the standing claim and a new vendor build
/// lands. aterm turns its policy off for the halt (`why=hold`) and says
/// nothing. The claim stands, nothing moves on the harness's own readings,
/// and when the halt lifts (again without a word) the move is made under the
/// same claim.
fn aterm_a_halt_holds_the_move_and_the_claim() {
    let s = Scenario::new("aterm-halt");
    let fake = rostered_aterm(&s);
    let pty = Pty::open();
    let (_child, pid) = start_in_aterm(&s, &pty, &fake, 40);
    await_record(&s, 0);
    fake.set_status("hold", Some("1"));
    s.set_current(BUILD_2);
    await_journal(
        &s,
        Duration::from_secs(10),
        "wait aterm policy effective=off why=hold",
    );
    std::thread::sleep(Duration::from_millis(1500));
    assert!(
        !got_term(&s) && s.starts().len() == 1,
        "moved under a halt{}",
        s.report()
    );
    assert_eq!(fake.holder(), Some(format!("claude-code-alab@{pid}")));
    assert_eq!(asked(&fake, &release_text(pid)), 0, "{:?}", fake.verbs());
    fake.set_status("hold", Some("0"));
    assert!(
        wait_until(Duration::from_secs(15), || s.starts().len() >= 2),
        "no move once the halt lifted{}",
        s.report()
    );
    assert_eq!(s.starts()[1].build(), Some(BUILD_2), "{}", s.report());
    let journal = s.journal();
    assert!(
        !journal.contains("aterm released") && !journal.contains(" aterm lost "),
        "{}",
        s.report()
    );
    assert_eq!(
        journal.matches(" aterm claimed ").count(),
        1,
        "{}",
        s.report()
    );
    end_session(&s, pid);
}

/// The session's own Autopilot is turned off, the claim is given back, and
/// Autopilot is turned on again: aterm moves no generation and sends no event
/// for a session's own word, so the harness reads the policy again on its own
/// clock and claims again.
fn aterm_a_policy_back_on_without_an_event_is_claimed_again() {
    let s = Scenario::new("aterm-policy-quiet");
    let fake = rostered_aterm(&s);
    let pty = Pty::open();
    let (_child, pid) = start_in_aterm(&s, &pty, &fake, 40);
    fake.set_policy(&POLICY_ON.replace(
        "effective=on why=enabled",
        "effective=off why=autopilot-off",
    ));
    await_journal(
        &s,
        Duration::from_secs(10),
        "aterm released policy effective=off why=autopilot-off",
    );
    assert_eq!(fake.holder(), None);
    fake.set_policy_quietly(POLICY_ON);
    assert!(
        wait_until(Duration::from_secs(15), || fake.holder()
            == Some(format!("claude-code-alab@{pid}"))),
        "never claimed again{}",
        s.report()
    );
    // The server stores its holder before the client finishes guarded_claim's
    // status proof. Observe client completion separately before its journal.
    assert!(
        wait_until(Duration::from_secs(60), || s
            .journal()
            .matches(" aterm claimed ")
            .count()
            == 2),
        "client did not finish the reclaimed claim{}",
        s.report()
    );
    let journal = s.journal();
    assert_eq!(
        journal.matches(" aterm claimed ").count(),
        2,
        "{}",
        s.report()
    );
    assert_eq!(fake.subscriptions(), 1, "the event stream was kept");
    end_session(&s, pid);
}

/// aterm holds a move on a fact only it knows (`upgrade_ask=true`), then
/// answers too late, and the link is lost. aterm's last word was a hold, and
/// nothing changed it: the move waits for the claim to be proven again rather
/// than going ahead on the harness's own readings, and is made once aterm's
/// word allows it.
fn aterm_a_hold_stands_over_a_reply_that_comes_too_late() {
    let s = Scenario::new("aterm-late-hold");
    let fake = rostered_aterm(&s);
    let pty = Pty::open();
    let (_child, pid) = start_in_aterm(&s, &pty, &fake, 40);
    await_record(&s, 0);
    fake.set_policy(&POLICY_ON.replace("upgrade_ask=false", "upgrade_ask=true"));
    s.set_current(BUILD_2);
    await_journal(&s, Duration::from_secs(10), "wait aterm upgrade_ask=true");
    fake.set_reply_delay(Duration::from_millis(800));
    // The event ends the hold's wait, so the next look asks at once.
    fake.event("status");
    await_journal(
        &s,
        Duration::from_secs(10),
        "wait link-lost aterm upgrade_ask=true",
    );
    std::thread::sleep(Duration::from_millis(2000));
    assert!(
        !got_term(&s) && s.starts().len() == 1,
        "moved on the harness's own readings over aterm's hold{}",
        s.report()
    );
    fake.set_reply_delay(Duration::ZERO);
    fake.set_policy(POLICY_ON);
    assert!(
        wait_until(Duration::from_secs(20), || s.starts().len() >= 2),
        "no move once aterm's word allowed it{}",
        s.report()
    );
    assert_eq!(s.starts()[1].build(), Some(BUILD_2), "{}", s.report());
    let order = [
        " aterm lost ",
        " wait link-lost aterm upgrade_ask=true\n",
        " end-for-update ",
    ]
    .map(|line| journal_at(&s, line));
    assert!(order.is_sorted(), "{order:?}{}", s.report());
    assert!(
        s.journal().rfind(" aterm claimed ") < Some(journal_at(&s, " end-for-update ")),
        "{}",
        s.report()
    );
    end_session(&s, pid);
}

/// Drop `fake`'s link to the harness and keep it down: the restarted aterm's
/// roster lacks the row until the scenario puts it back.
fn lose_the_link(s: &Scenario, fake: &FakeAterm) {
    fake.set_info(fake_aterm::INFO_UNROSTERED);
    fake.restart();
    assert!(
        wait_until(Duration::from_secs(10), || s
            .journal()
            .contains(" aterm lost ")),
        "the dropped link was never noticed{}",
        s.report()
    );
}

/// The vendor crashes with the terminal raw (and, in aterm, its keyboard
/// reporting on) while the link is lost. During the wait for the claim no
/// child runs, so the terminal is put back at once: the harness's modes and
/// the keyboard-reporting resets are on it, and Ctrl-C typed at it ends the
/// harness with the crashed child's status.
fn aterm_a_crash_gives_the_terminal_back_while_it_waits_for_aterm() {
    let s = Scenario::new("aterm-crash-keys");
    let fake = rostered_aterm(&s);
    let pty = Pty::open();
    let (_child, pid) = start_in_aterm(&s, &pty, &fake, 40);
    await_record(&s, 0);
    assert_eq!(pty.icanon(), Some(false), "the fake runs raw");
    lose_the_link(&s, &fake);
    command_logged(&s, "crash:7", "crash 7");
    await_journal(
        &s,
        Duration::from_secs(5),
        "crash waiting-for-claim hold-ms=45000 link-lost",
    );
    assert_eq!(pty.icanon(), Some(true), "{}", s.report());
    let reset = String::from_utf8_lossy(claude_code_alab::sys::INPUT_MODE_RESET).into_owned();
    assert!(
        wait_until(Duration::from_secs(2), || pty.output().contains(&reset)),
        "the keyboard reporting was not reset\n--- pty ---\n{:?}{}",
        pty.output(),
        s.report()
    );
    pty.type_text("\u{3}");
    assert_eq!(
        wait_status(pid, Duration::from_secs(5)),
        Some(Status::Exited(7)),
        "Ctrl-C did not end the wait{}",
        s.report()
    );
    let journal = s.journal();
    assert!(
        journal.contains(&format!(" crash-passed-on signal {}\n", libc::SIGINT))
            && !journal.contains("crash-relaunch"),
        "{}",
        s.report()
    );
    assert_eq!(s.starts().len(), 1, "{}", s.report());
}

/// A person types a line at the dead screen while the harness waits to
/// relaunch a crashed vendor. The terminal is told the relaunch is coming, and
/// the line is discarded: the relaunched vendor never reads it as a prompt.
fn aterm_typing_at_a_dead_screen_reaches_no_relaunched_child() {
    let s = Scenario::new("aterm-crash-typing");
    let fake = rostered_aterm(&s);
    let pty = Pty::open();
    let (_child, pid) = start_in_aterm(&s, &pty, &fake, 40);
    await_record(&s, 0);
    lose_the_link(&s, &fake);
    command_logged(&s, "crash:7", "crash 7");
    await_journal(
        &s,
        Duration::from_secs(5),
        "crash waiting-for-claim hold-ms=45000 link-lost",
    );
    pty.type_text("make it faster\n");
    fake.set_info(INFO_ROSTERED);
    assert!(
        wait_until(Duration::from_secs(15), || s.starts().len() >= 2),
        "no relaunch once the claim was proven again{}",
        s.report()
    );
    assert!(
        wait_until(Duration::from_secs(5), || pty.output().contains(
            "claude-alab: claude 9.9.1 ended abnormally (exit 7); restarting in 0 s, Ctrl-C \
             ends the session"
        )),
        "the pause was not announced\n--- pty ---\n{}{}",
        pty.output(),
        s.report()
    );
    await_record(&s, 1);
    // Many of the relaunched fake's reads.
    std::thread::sleep(Duration::from_millis(1000));
    assert!(
        !s.log().iter().any(|l| l.starts_with("read ")),
        "the relaunched vendor read what was typed at the dead screen{}",
        s.report()
    );
    end_session(&s, pid);
}

/// Ctrl-C (here `SIGINT` to the harness) reaches the harness while the launch
/// claim sequence runs, before any vendor exists: the harness ends by that
/// signal, gives the claim back, and starts nothing.
fn aterm_a_signal_during_the_launch_claim_ends_the_harness_before_any_start() {
    let s = Scenario::new("aterm-launch-signal");
    let fake = rostered_aterm(&s);
    fake.set_reply_delay(Duration::from_millis(300));
    s.add_build(BUILD_1, Some("9.9.1"));
    s.set_current(BUILD_1);
    let pty = Pty::open();
    let mut cmd = s.command(&["--alab-quiet-ms=600", "--alab-poll-ms=40"]);
    cmd.envs(fake.env());
    let (_child, pid) = spawn_guarded(&s, &pty, cmd);
    assert!(
        wait_until(Duration::from_secs(10), || !fake.verbs().is_empty()),
        "the claim sequence never began{}",
        s.report()
    );
    signal(pid, libc::SIGINT);
    assert_eq!(
        wait_status(pid, Duration::from_secs(10)),
        Some(Status::Signaled(libc::SIGINT)),
        "{}",
        s.report()
    );
    std::thread::sleep(Duration::from_millis(500));
    assert!(s.starts().is_empty(), "a vendor was started{}", s.report());
    assert!(
        s.journal()
            .contains(&format!(" exit signal {} before-start\n", libc::SIGINT)),
        "{}",
        s.report()
    );
    assert!(
        wait_until(Duration::from_secs(5), || fake.holder().is_none()),
        "{}",
        s.report()
    );
}

/// `SIGINT` reaches the harness while it asks aterm at the end of a crash
/// pause, after the pause's own wait: the relaunch is not made, and the
/// harness ends with the crashed child's status.
fn aterm_a_signal_at_the_end_of_the_crash_pause_ends_the_harness() {
    let s = Scenario::new("aterm-crash-due-signal");
    let fake = rostered_aterm(&s);
    let pty = Pty::open();
    let (_child, pid) = start_in_aterm(&s, &pty, &fake, 40);
    await_record(&s, 0);
    command_logged(&s, "crash:7", "crash 7");
    // The exit's read is over: the launch's proof and the exit's.
    assert!(
        wait_until(Duration::from_secs(5), || asked(&fake, "status") == 2),
        "{:?}{}",
        fake.verbs(),
        s.report()
    );
    fake.set_reply_delay(Duration::from_millis(400));
    // The end of the pause's read has begun: the harness reads no signal
    // before it has started the child again.
    assert!(
        wait_until(Duration::from_secs(5), || asked(&fake, "harness policy")
            == 3),
        "{:?}{}",
        fake.verbs(),
        s.report()
    );
    signal(pid, libc::SIGINT);
    assert_eq!(
        wait_status(pid, Duration::from_secs(5)),
        Some(Status::Exited(7)),
        "{}",
        s.report()
    );
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(s.starts().len(), 1, "{}", s.report());
    assert!(
        s.journal()
            .contains(&format!(" crash-passed-on signal {}\n", libc::SIGINT)),
        "{}",
        s.report()
    );
}

/// The policy with the session's own Autopilot off.
fn autopilot_off() -> String {
    POLICY_ON.replace(
        "effective=on why=enabled",
        "effective=off why=autopilot-off",
    )
}

/// Every vendor start of `s` under harness `pid` found the same recursion
/// marks (this harness, one level deep) and inherited no descriptor above
/// stderr: a relaunched child is started as the first one was.
fn assert_every_start_is_clean(s: &Scenario, pid: i32) {
    let marks = marks(s);
    assert!(
        marks.len() >= 2
            && marks
                .iter()
                .all(|m| *m == format!("marks\tCLAUDE_ALAB_PID={pid}\tCLAUDE_ALAB_DEPTH=1")),
        "{marks:?}{}",
        s.report()
    );
    let fds: Vec<String> = s
        .log()
        .into_iter()
        .filter(|l| l.starts_with("fds\t"))
        .collect();
    assert!(
        fds.len() == marks.len() && fds.iter().all(|l| l == "fds\t-"),
        "a start inherited descriptors: {fds:?}{}",
        s.report()
    );
}

/// The session's own Autopilot is turned off with no word to the harness, and
/// a new build lands: the look reads the policy, gives the claim back and
/// moves on the harness's own readings. Autopilot is turned on again, again
/// with no word: the release set the policy's own clock, so the claim is made
/// again, with the build the child now runs.
fn aterm_a_policy_turned_off_quietly_is_released_at_a_look_and_claimed_again() {
    let s = Scenario::new("aterm-policy-quiet-look");
    let fake = rostered_aterm(&s);
    let pty = Pty::open();
    let (_child, pid) = start_in_aterm(&s, &pty, &fake, 40);
    await_record(&s, 0);
    fake.set_policy_quietly(&autopilot_off());
    s.set_current(BUILD_2);
    await_journal(
        &s,
        Duration::from_secs(10),
        "aterm released policy effective=off why=autopilot-off",
    );
    assert!(
        wait_until(Duration::from_secs(15), || s.starts().len() >= 2),
        "no move on the harness's own readings{}",
        s.report()
    );
    assert_eq!(s.starts()[1].build(), Some(BUILD_2), "{}", s.report());
    assert_eq!(fake.holder(), None);
    fake.set_policy_quietly(POLICY_ON);
    assert!(
        wait_until(Duration::from_secs(15), || fake.holder()
            == Some(format!("claude-code-alab@{pid}"))),
        "never claimed again{}",
        s.report()
    );
    let journal = s.journal();
    assert_eq!(
        journal.matches(" aterm claimed ").count(),
        2,
        "{}",
        s.report()
    );
    let order = [
        " aterm released policy effective=off why=autopilot-off\n",
        " end-for-update ",
        &format!(
            " aterm claimed holder=claude-code-alab@{pid} scope=relaunch,upgrade vendor=9.9.2\n"
        ),
    ]
    .map(|line| journal_at(&s, line));
    assert!(order.is_sorted(), "{order:?}{}", s.report());
    assert_eq!(fake.subscriptions(), 1, "the event stream was kept");
    end_session(&s, pid);
}

/// The claim is given back and made again on policy events, then given back
/// at a look (the session's Autopilot turned off with no word, a new build
/// pending) while typing the vendor has not read holds the move, and made
/// again when Autopilot is on. Every claim made is journalled, the last too,
/// though it names the same build as the one before the look's release.
fn aterm_a_claim_made_again_after_a_look_released_it_is_journalled() {
    let s = Scenario::new("aterm-reclaim-journal");
    let fake = rostered_aterm(&s);
    let pty = Pty::open();
    let (_child, pid) = start_in_aterm(&s, &pty, &fake, 40);
    await_record(&s, 0);
    let holder = Some(format!("claude-code-alab@{pid}"));
    fake.set_policy(&autopilot_off());
    assert!(
        wait_until(Duration::from_secs(10), || fake.holder().is_none()),
        "{}",
        s.report()
    );
    fake.set_policy(POLICY_ON);
    assert!(
        wait_until(Duration::from_secs(10), || fake.holder() == holder),
        "{}",
        s.report()
    );
    s.put("command", "deaf");
    assert!(
        wait_until(Duration::from_secs(5), || s
            .log()
            .iter()
            .any(|l| l == "deaf")),
        "{}",
        s.report()
    );
    pty.type_text("x");
    fake.set_policy_quietly(&autopilot_off());
    s.set_current(BUILD_2);
    assert!(
        wait_until(Duration::from_secs(10), || s
            .journal()
            .matches(" aterm released policy effective=off why=autopilot-off\n")
            .count()
            == 2),
        "the look did not give the claim back{}",
        s.report()
    );
    await_journal(&s, Duration::from_secs(5), "wait unread-input");
    // The build change woke the look. Retire it before reclaiming so the
    // cleanup command cannot race an unrelated vendor replacement.
    s.set_current(BUILD_1);
    fake.set_policy_quietly(POLICY_ON);
    assert!(
        wait_until(Duration::from_secs(15), || fake.holder() == holder),
        "never claimed again{}",
        s.report()
    );
    assert!(
        wait_until(Duration::from_secs(5), || s
            .journal()
            .matches(&format!(
                " aterm claimed holder=claude-code-alab@{pid} scope=relaunch,upgrade \
                 vendor=9.9.1\n"
            ))
            .count()
            == 3),
        "a claim made was not journalled{}",
        s.report()
    );
    s.put("command", "hear");
    end_session(&s, pid);
}

/// The session's own Autopilot is turned off, with no word to the harness,
/// and a person asks aterm to restart the session. aterm sends the request to
/// the claim without reading the policy, and its own host would make an asked
/// restart whatever the Autopilot says: the harness makes it.
fn aterm_a_restart_asked_with_autopilot_off_is_made() {
    let s = Scenario::new("aterm-restart-autopilot-off");
    let fake = rostered_aterm(&s);
    let pty = Pty::open();
    let (_child, pid) = start_in_aterm(&s, &pty, &fake, 40);
    await_record(&s, 0);
    fake.set_policy_quietly(&autopilot_off());
    fake.send_restart("asked");
    assert!(
        wait_until(Duration::from_secs(8), || s.starts().len() >= 2),
        "the asked restart was not made{}",
        s.report()
    );
    assert_eq!(s.starts()[1].args, ["--resume", SID], "{}", s.report());
    assert!(!s.journal().contains("ignored"), "{}", s.report());
    end_session(&s, pid);
}

/// A session starts with its own Autopilot off: unclaimed, and the claim
/// sequence is tried again on its clock, since aterm says nothing to a link
/// with no claim when a person turns Autopilot on. Once it is on, the claim is
/// made.
fn aterm_a_launch_with_autopilot_off_is_claimed_once_it_is_on() {
    let s = Scenario::new("aterm-launch-autopilot-off");
    let fake = rostered_aterm(&s);
    fake.set_policy_quietly(&autopilot_off());
    let pty = Pty::open();
    let (_child, pid) = start_in_aterm(&s, &pty, &fake, 40);
    assert!(
        s.journal()
            .contains(" aterm unclaimed policy effective=off why=autopilot-off\n"),
        "{}",
        s.report()
    );
    assert_eq!(fake.holder(), None);
    fake.set_policy_quietly(POLICY_ON);
    assert!(
        wait_until(Duration::from_secs(15), || fake.holder()
            == Some(format!("claude-code-alab@{pid}"))),
        "never claimed once Autopilot was on{}",
        s.report()
    );
    assert!(
        wait_until(Duration::from_secs(5), || fake.subscriptions() == 1),
        "{}",
        s.report()
    );
    let journal = s.journal();
    assert_eq!(
        journal.matches(" aterm claimed ").count(),
        1,
        "{}",
        s.report()
    );
    end_session(&s, pid);
}

/// A session starts under a halt (`hold=1`, which aterm's policy reads as
/// `effective=off why=hold`): unclaimed for now, and claimed once the halt
/// lifts, though nothing announces that.
fn aterm_a_launch_under_a_halt_is_claimed_once_the_halt_lifts() {
    let s = Scenario::new("aterm-launch-halt");
    let fake = rostered_aterm(&s);
    fake.set_status("hold", Some("1"));
    let pty = Pty::open();
    let (_child, pid) = start_in_aterm(&s, &pty, &fake, 40);
    assert!(
        s.journal()
            .contains(" aterm unclaimed policy effective=off why=hold\n"),
        "{}",
        s.report()
    );
    assert_eq!(fake.holder(), None);
    fake.set_status("hold", Some("0"));
    assert!(
        wait_until(Duration::from_secs(15), || fake.holder()
            == Some(format!("claude-code-alab@{pid}"))),
        "never claimed once the halt lifted{}",
        s.report()
    );
    end_session(&s, pid);
}

/// aterm holds a move (`upgrade_ask=true`) and the link is lost; while it is
/// being claimed again the person turns the session's Autopilot off, and the
/// claim sequence's policy read says so. aterm has answered, and its hold is
/// no longer its last word: the move is made on the harness's own readings,
/// as it would be over a held link that read the same policy.
fn aterm_a_hold_on_a_lost_link_gives_way_to_aterms_refusal() {
    let s = Scenario::new("aterm-lost-hold-refused");
    let fake = rostered_aterm(&s);
    let pty = Pty::open();
    let (_child, pid) = start_in_aterm(&s, &pty, &fake, 40);
    await_record(&s, 0);
    let asking = POLICY_ON.replace("upgrade_ask=false", "upgrade_ask=true");
    fake.set_policy(&asking);
    s.set_current(BUILD_2);
    await_journal(&s, Duration::from_secs(10), "wait aterm upgrade_ask=true");
    fake.set_reply_delay(Duration::from_millis(800));
    fake.event("status");
    await_journal(
        &s,
        Duration::from_secs(10),
        "wait link-lost aterm upgrade_ask=true",
    );
    fake.set_policy_quietly(&asking.replace(
        "effective=on why=enabled",
        "effective=off why=autopilot-off",
    ));
    fake.set_reply_delay(Duration::ZERO);
    assert!(
        wait_until(Duration::from_secs(20), || s.starts().len() >= 2),
        "the move waited on a hold aterm had replaced{}",
        s.report()
    );
    assert_eq!(s.starts()[1].build(), Some(BUILD_2), "{}", s.report());
    let order = [
        " wait link-lost aterm upgrade_ask=true\n",
        " aterm unclaimed policy effective=off why=autopilot-off\n",
        " end-for-update ",
    ]
    .map(|line| journal_at(&s, line));
    assert!(order.is_sorted(), "{order:?}{}", s.report());
    assert_eq!(
        s.journal().matches(" aterm claimed ").count(),
        1,
        "{}",
        s.report()
    );
    assert_eq!(fake.holder(), None);
    end_session(&s, pid);
}

/// A policy event arrives while the session is halted, and again while aterm
/// cannot read its own state (`ERR …; retry`): each is an answer for now
/// only, so the standing claim is kept, given back to nobody and not lost.
fn aterm_a_policy_event_keeps_the_claim_through_a_halt_and_a_busy_aterm() {
    let s = Scenario::new("aterm-policy-event-halt");
    let fake = rostered_aterm(&s);
    let pty = Pty::open();
    let (_child, pid) = start_in_aterm(&s, &pty, &fake, 40);
    await_record(&s, 0);
    let holder = Some(format!("claude-code-alab@{pid}"));
    fake.set_status("hold", Some("1"));
    let before = asked(&fake, "harness policy");
    fake.set_policy(POLICY_ON);
    assert!(
        wait_until(Duration::from_secs(5), || asked(&fake, "harness policy")
            > before),
        "the event was not followed by a policy read{}",
        s.report()
    );
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(fake.holder(), holder, "{}", s.report());
    fake.set_status("hold", Some("0"));
    fake.set_policy_reply("ERR main thread stalled 6s; retry");
    let before = asked(&fake, "harness policy");
    fake.event("harness-policy gen=90");
    assert!(
        wait_until(Duration::from_secs(5), || asked(&fake, "harness policy")
            > before),
        "the event was not followed by a policy read{}",
        s.report()
    );
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(fake.holder(), holder, "{}", s.report());
    let journal = s.journal();
    assert!(
        !journal.contains("aterm released")
            && !journal.contains(" aterm lost ")
            && !journal.contains("aterm unclaimed"),
        "{}",
        s.report()
    );
    fake.clear_policy_reply();
    end_session(&s, pid);
}

/// An update's SIGTERM ends the vendor, and aterm's restart request for that
/// same vendor is read in the pass that started the new build (here the
/// harness is stopped while both arrive). The request was for the child that
/// ended: the new one is not ended for it.
fn aterm_a_restart_sent_for_the_child_an_update_replaced_is_ignored() {
    let s = Scenario::new("aterm-restart-replaced");
    let fake = rostered_aterm(&s);
    s.put("term", "ignore");
    let pty = Pty::open();
    let (_child, pid) = start_in_aterm(&s, &pty, &fake, 40);
    await_record(&s, 0);
    s.set_current(BUILD_2);
    assert!(
        wait_until(Duration::from_secs(15), || got_term(&s)),
        "{}",
        s.report()
    );
    signal(pid, libc::SIGSTOP);
    assert_eq!(
        wait_status(pid, Duration::from_secs(5)),
        Some(Status::Stopped)
    );
    std::fs::remove_file(s.fake.join("term")).expect("heed TERM again");
    s.put("command", "exit:0");
    assert!(
        wait_until(Duration::from_secs(5), || s
            .log()
            .iter()
            .any(|l| l == "exit 0")),
        "{}",
        s.report()
    );
    fake.send_restart("stall");
    std::thread::sleep(Duration::from_millis(300));
    signal(pid, libc::SIGCONT);
    assert_eq!(
        wait_status(pid, Duration::from_secs(5)),
        Some(Status::Continued)
    );
    assert!(
        wait_until(Duration::from_secs(10), || s.starts().len() >= 2),
        "{}",
        s.report()
    );
    assert_eq!(s.starts()[1].build(), Some(BUILD_2), "{}", s.report());
    await_journal(
        &s,
        Duration::from_secs(5),
        "aterm restart cause=stall ignored child-replaced",
    );
    await_record(&s, 1);
    // Many looks after the new child's record appeared.
    std::thread::sleep(Duration::from_millis(1500));
    assert_eq!(s.starts().len(), 2, "{}", s.report());
    assert_eq!(
        s.log().iter().filter(|l| *l == "signal TERM").count(),
        1,
        "the new child was signalled for its predecessor's restart{}",
        s.report()
    );
    end_session(&s, pid);
}

/// `SIGTSTP` reaches the harness while the launch's claim sequence runs: the
/// vendor is started, and the job is then stopped as asked, not left
/// running. The harness runs as a shell's job does ([`start_grouped`]): in a
/// session leader's group the kernel would discard the stop.
fn aterm_ctrl_z_during_the_launch_claim_stops_the_job() {
    let s = Scenario::new("aterm-launch-tstp");
    let fake = rostered_aterm(&s);
    fake.set_reply_delay(Duration::from_millis(300));
    s.add_build(BUILD_1, Some("9.9.1"));
    s.set_current(BUILD_1);
    let pty = Pty::open();
    let mut cmd = s.command(&["--alab-poll-ms=40"]);
    cmd.envs(fake.env());
    let (_child, pid) = spawn_grouped(&s, &pty, cmd);
    assert!(
        wait_until(Duration::from_secs(10), || !fake.verbs().is_empty()),
        "the claim sequence never began{}",
        s.report()
    );
    signal(pid, libc::SIGTSTP);
    assert_eq!(
        wait_status(pid, Duration::from_secs(15)),
        Some(Status::Stopped),
        "the job was not stopped{}",
        s.report()
    );
    assert_eq!(s.starts().len(), 1, "{}", s.report());
    assert!(
        s.journal().contains(" signal passed-on TSTP\n")
            && s.journal().contains(" stopped following the child\n"),
        "{}",
        s.report()
    );
    fake.set_reply_delay(Duration::ZERO);
    signal(-pid, libc::SIGCONT);
    assert_eq!(
        wait_status(pid, Duration::from_secs(10)),
        Some(Status::Continued)
    );
    end_session(&s, pid);
}

/// The vendor crashes under a proven claim while the harness is a job that is
/// not the terminal's foreground group: it starts nothing on a terminal it
/// does not own, and ends as the vendor did.
fn aterm_a_crash_of_a_background_job_is_passed_on() {
    let s = Scenario::new("aterm-crash-background");
    let fake = rostered_aterm(&s);
    s.add_build(BUILD_1, Some("9.9.1"));
    s.set_current(BUILD_1);
    let pty = Pty::open();
    let mut cmd = s.command(&["--alab-poll-ms=40"]);
    cmd.envs(fake.env());
    let (_child, pid) = spawn_grouped(&s, &pty, cmd);
    await_record(&s, 0);
    assert_eq!(fake.holder(), Some(format!("claude-code-alab@{pid}")));
    command_logged(&s, "crash:7", "crash 7");
    assert_eq!(
        wait_status(pid, Duration::from_secs(10)),
        Some(Status::Exited(7)),
        "{}",
        s.report()
    );
    assert!(
        s.journal().contains(" crash-passed-on background\n"),
        "{}",
        s.report()
    );
    assert_eq!(s.starts().len(), 1, "{}", s.report());
}

/// The vendor crashes and the build it ran is taken away from the store
/// during the pause (its executable is gone): the same build cannot be
/// started again, and the crash is passed on with the vendor's status.
fn aterm_a_crash_whose_build_was_taken_away_in_the_pause_is_passed_on() {
    let s = Scenario::new("aterm-crash-unpinnable");
    let fake = rostered_aterm(&s);
    let pty = Pty::open();
    let (_child, pid) = start_in_aterm(&s, &pty, &fake, 40);
    await_record(&s, 0);
    command_logged(&s, "crash:7", "crash 7");
    await_journal(
        &s,
        Duration::from_secs(5),
        &format!("crash record-stands session={SID} streak=0 pause-ms=1000"),
    );
    std::fs::remove_file(&s.starts()[0].exe).expect("remove the build's executable");
    assert_eq!(
        wait_status(pid, Duration::from_secs(10)),
        Some(Status::Exited(7)),
        "{}",
        s.report()
    );
    let journal = s.journal();
    assert!(
        journal.contains(" crash-passed-on vendor-plan ")
            || journal.contains(" crash-passed-on relaunch-failed "),
        "{}",
        s.report()
    );
    assert_eq!(s.starts().len(), 1, "{}", s.report());
    assert!(
        !pty.output().contains("resuming conversation"),
        "--- pty ---\n{}",
        pty.output()
    );
}

/// Run the harness command `cmd` with `pty`'s slave as its stdin in a process
/// group of its own, as a shell runs a job ([`start_grouped`]), and return at
/// once.
fn spawn_grouped(s: &Scenario, pty: &Pty, mut cmd: Command) -> (std::process::Child, i32) {
    cmd.stdin(Stdio::from(pty.slave.try_clone().expect("clone slave")))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0);
    let child = cmd.spawn().expect("spawn");
    let pid = child_pid(&child);
    s.guard_group(pid);
    (child, pid)
}

/// A restart's vendor outlives SIGTERM and is sent SIGKILL, which leaves its
/// keyboard reporting on, and the same build can then not be started (its
/// pinned launch was taken away). The harness exits, and the terminal it
/// leaves to the shell has the keyboard-reporting resets on it, so Ctrl-C
/// there is a `SIGINT` again.
fn aterm_a_restart_whose_relaunch_cannot_start_gives_the_keys_back() {
    let s = Scenario::new("aterm-restart-unstartable");
    let fake = rostered_aterm(&s);
    s.put("term", "ignore");
    let pty = Pty::open();
    let (_child, pid) = start_in_aterm_with(&s, &pty, &fake, 40, &["--alab-term-grace-ms=600"]);
    await_record(&s, 0);
    fake.send_restart("stall");
    assert!(
        wait_until(Duration::from_secs(5), || s
            .journal()
            .contains(" aterm restart cause=stall SIGTERM ")),
        "{}",
        s.report()
    );
    std::fs::remove_file(vendor_gate(&s, BUILD_1)).expect("unlink the pinned gate");
    assert_eq!(
        wait_status(pid, Duration::from_secs(15)),
        Some(Status::Exited(127)),
        "{}\n--- pty ---\n{}",
        s.report(),
        pty.output()
    );
    let journal = s.journal();
    assert!(
        journal.contains(" restart-escalated cause=stall SIGKILL ")
            && journal.contains(" relaunch child=Signaled(9) ")
            && journal.contains(" exit relaunch-failed "),
        "{}",
        s.report()
    );
    let reset = String::from_utf8_lossy(claude_code_alab::sys::INPUT_MODE_RESET).into_owned();
    assert!(
        wait_until(Duration::from_secs(2), || pty.output().contains(&reset)),
        "the killed vendor's keyboard reporting was left on\n--- pty ---\n{:?}{}",
        pty.output(),
        s.report()
    );
    assert_eq!(s.starts().len(), 1, "{}", s.report());
}

// ───────────────────────── the atpkg twin's entry ─────────────────────────

/// The fake's `marks` lines: the harness's recursion marks as each vendor
/// start found them.
fn marks(s: &Scenario) -> Vec<String> {
    s.log()
        .into_iter()
        .filter(|l| l.starts_with("marks\t"))
        .collect()
}

/// What the twin sends, `run -- <the launch's arguments>`, reaches the vendor
/// as exactly those arguments, with the harness's own options still read ahead
/// of it and nothing read after it. `run` without its `--` is not the envelope
/// and is the vendor's own first word.
fn the_twin_envelope_reaches_the_vendor_with_exactly_those_arguments() {
    let s = Scenario::new("run-envelope");
    s.add_build(BUILD_1, Some("9.9.1"));
    s.set_current(BUILD_1);
    s.put("mode", "echo:7");
    let cases: &[(&[&str], &[&str])] = &[
        (
            &[
                "run",
                "--",
                "--model",
                "x y",
                "",
                "ünï",
                "--alab-not-mine",
                "--",
                "run",
                "--",
                "-p",
            ],
            &[
                "--model",
                "x y",
                "",
                "ünï",
                "--alab-not-mine",
                "--",
                "run",
                "--",
                "-p",
            ],
        ),
        (&["run", "--"], &["--dangerously-skip-permissions"]),
        (
            &["--alab-poll-ms=40", "run", "--", "--alab-poll-ms=41", "-c"],
            &["--alab-poll-ms=41", "-c"],
        ),
        (&["run", "--", "--alab-help"], &["--alab-help"]),
        (&["run", "--", "--version", "x"], &["--version", "x"]),
        // Not the envelope: every word is the vendor's.
        (&["run"], &["run"]),
        (&["run", "fix it"], &["run", "fix it"]),
        (&["-p", "run", "--", "x"], &["-p", "run", "--", "x"]),
    ];
    // No terminal: each launch runs the vendor in the harness's own process,
    // which is where the envelope's words have to arrive.
    for (at, (sent, got)) in cases.iter().enumerate() {
        let (pid, code, start) = launch_with(&s, sent, &[]);
        assert_eq!(code, Some(7), "{sent:?}{}", s.report());
        assert_eq!(s.starts().len(), at + 1, "{sent:?}{}", s.report());
        assert_eq!(start.args, *got, "{sent:?}");
        assert_ran_in_place(&s, at, pid, "no-tty", 1);
    }
    assert!(!s.journal().contains(" start harness="), "{}", s.report());
    // A lone `--version` inside the envelope is the vendor's too: the person
    // typed `claude --version`, and the harness's own line is `claude-alab --version`.
    let out = s
        .command(&["run", "--", "--version"])
        .output()
        .expect("run");
    assert!(out.status.success(), "{out:?}{}", s.report());
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "9.9.9 (Claude Code)\n"
    );
    assert!(s.log().iter().any(|l| l == "version"), "{}", s.report());
    // An unknown harness option ahead of the envelope is still refused.
    let out = s
        .command(&["--alab-frob", "run", "--", "x"])
        .output()
        .expect("run");
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    assert_eq!(s.starts().len(), cases.len(), "{}", s.report());
}

/// One launch without a terminal on an echoing fake, with `env` set and every
/// aterm variable not named in it removed: the harness's pid, its exit code,
/// and the vendor start it led to.
fn launch_with(s: &Scenario, args: &[&str], env: &[(&str, &str)]) -> (i32, Option<i32>, FakeStart) {
    let before = s.starts().len();
    let mut cmd = s.command(args);
    for name in [
        "TERM_PROGRAM",
        "ATERM_CHILD",
        "ATERM_SESSION_ID",
        "ATERM_NO_HARNESS",
    ] {
        cmd.env_remove(name);
    }
    cmd.envs(env.iter().copied())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = cmd.spawn().expect("spawn");
    let pid = child_pid(&child);
    let out = child.wait_with_output().expect("wait");
    let mut starts = s.starts();
    assert_eq!(
        starts.len(),
        before + 1,
        "{args:?} {env:?}: {out:?}{}",
        s.report()
    );
    (pid, out.status.code(), starts.remove(before))
}

/// One launch on a fresh pty, with `env` set and every aterm variable not
/// named in it removed, on an interactive fake that is told to exit 7 once it
/// has started: the harness's pid, how it ended, and the vendor start it led to.
fn launch_on_pty(
    s: &Scenario,
    args: &[&str],
    env: &[(&str, &str)],
) -> (i32, Option<Status>, FakeStart) {
    let before = s.starts().len();
    let mut cmd = s.command(args);
    for name in [
        "TERM_PROGRAM",
        "ATERM_CHILD",
        "ATERM_SESSION_ID",
        "ATERM_NO_HARNESS",
    ] {
        cmd.env_remove(name);
    }
    cmd.envs(env.iter().copied());
    let pty = Pty::open();
    let child = pty.spawn(cmd);
    let pid = child_pid(&child);
    s.guard_group(pid);
    assert!(
        wait_until(Duration::from_secs(10), || marks(s).len() > before),
        "{args:?} {env:?}: the fake never started{}\n--- pty ---\n{}",
        s.report(),
        pty.output()
    );
    s.put("command", "exit:7");
    let status = reap(child, Duration::from_secs(10));
    let mut starts = s.starts();
    assert_eq!(starts.len(), before + 1, "{args:?} {env:?}{}", s.report());
    (pid, status, starts.remove(before))
}

/// `ATERM_NO_HARNESS=1` inside aterm leaves the harness out as the sibling
/// harnesses do: the vendor runs in the harness's own process, with the
/// build's environment, without the harness's pid mark and one level deeper.
/// Any other value, and the same value outside aterm, is not read: on a
/// terminal the session is then supervised, and without one the vendor runs in
/// the harness's place for that reason (`pass no-tty`), not this one.
fn no_harness_is_read_only_as_one_and_only_inside_aterm() {
    let s = Scenario::new("no-harness-values");
    s.add_build(BUILD_1, Some("9.9.1"));
    s.set_current(BUILD_1);
    s.put("mode", "echo:7");
    let args = ["run", "--", "--model", "x y"];
    let mut passed = 0;
    let mut supervised = 0;
    let mut no_terminal = 0;
    // Left out: each way a launch is inside aterm, with and without the envelope.
    for (inside, args, managed) in [
        (("ATERM_CHILD", "1"), &args[..], true),
        (("TERM_PROGRAM", "aterm"), &args[..], true),
        (("ATERM_SESSION_ID", "s"), &args[..], true),
        (
            ("ATERM_CHILD", "1"),
            &["--alab-poll-ms=40", "--model", "x y"][..],
            false,
        ),
    ] {
        let env = [
            inside,
            ("ATERM_NO_HARNESS", "1"),
            ("CLAUDE_ALAB_PID", "1"),
            ("CLAUDE_ALAB_DEPTH", "2"),
        ];
        let (pid, code, start) = launch_with(&s, args, &env);
        passed += 1;
        assert_eq!(code, Some(7), "{env:?}{}", s.report());
        assert_eq!(
            start.pid, pid,
            "the vendor took the harness's place: {env:?}"
        );
        let expected = if managed {
            &["--dangerously-skip-permissions", "--model", "x y"][..]
        } else {
            &["--model", "x y"][..]
        };
        assert_eq!(start.args, expected, "managed={managed} {env:?}");
        assert_eq!(start.shim, format!("{BUILD_1}"), "the build's shim env");
        assert_eq!(
            marks(&s).last().map(String::as_str),
            Some("marks\tCLAUDE_ALAB_PID=\tCLAUDE_ALAB_DEPTH=3"),
            "no harness pid, one level deeper: {env:?}{}",
            s.report()
        );
    }
    // Not left out: any other value inside aterm, and `1` outside it.
    let not_left_out: [&[(&str, &str)]; 5] = [
        &[("ATERM_CHILD", "1")],
        &[("ATERM_CHILD", "1"), ("ATERM_NO_HARNESS", "")],
        &[("ATERM_CHILD", "1"), ("ATERM_NO_HARNESS", "0")],
        &[("ATERM_CHILD", "1"), ("ATERM_NO_HARNESS", "true")],
        &[("ATERM_NO_HARNESS", "1")],
    ];
    // Without a terminal such a launch still runs the vendor in the harness's
    // place, and the journal names the reason that applied.
    for env in not_left_out {
        let at = s.starts().len();
        let (pid, code, start) = launch_with(&s, &args, env);
        no_terminal += 1;
        assert_eq!(code, Some(7), "{env:?}{}", s.report());
        assert_eq!(
            start.args,
            ["--dangerously-skip-permissions", "--model", "x y"],
            "{env:?}"
        );
        assert_ran_in_place(&s, at, pid, "no-tty", 1);
    }
    // On a terminal it is supervised: the vendor is the harness's child and
    // carries the harness's pid.
    std::fs::remove_file(s.fake.join("mode")).expect("an interactive fake from here on");
    for env in not_left_out {
        let (pid, status, start) = launch_on_pty(&s, &args, env);
        supervised += 1;
        assert_eq!(status, Some(Status::Exited(7)), "{env:?}{}", s.report());
        assert_ne!(start.pid, pid, "the vendor is the harness's child: {env:?}");
        assert_eq!(
            start.args,
            ["--dangerously-skip-permissions", "--model", "x y"],
            "{env:?}"
        );
        assert_eq!(
            marks(&s).last().cloned(),
            Some(format!("marks\tCLAUDE_ALAB_PID={pid}\tCLAUDE_ALAB_DEPTH=1")),
            "{env:?}{}",
            s.report()
        );
    }
    // Either way the vendor never updates itself.
    let forced = "env\tDISABLE_AUTOUPDATER=1\tCLAUDE_CODE_DISABLE_FEEDBACK_SURVEY=1";
    assert!(
        s.log()
            .iter()
            .filter(|l| l.starts_with("env\t"))
            .all(|l| l == forced),
        "{}",
        s.report()
    );
    let journal = s.journal();
    assert_eq!(
        journal
            .matches(&format!(
                " pass no-harness vendor-build={BUILD_1} vendor=9.9.1\n"
            ))
            .count(),
        passed,
        "{}",
        s.report()
    );
    assert_eq!(
        journal
            .matches(&format!(
                " pass no-tty vendor-build={BUILD_1} vendor=9.9.1\n"
            ))
            .count(),
        no_terminal,
        "{}",
        s.report()
    );
    assert_eq!(
        journal.matches(" start harness=").count(),
        supervised,
        "only a launch on a terminal that is not left out starts supervising{}",
        s.report()
    );
    assert_eq!(
        journal.matches(" exit child=Exited(7)\n").count(),
        supervised,
        "{}",
        s.report()
    );
}

/// In an aterm tab that would grant the claim, an interactive launch with
/// `ATERM_NO_HARNESS=1` sends aterm nothing at all: the vendor holds the
/// terminal in the harness's own process and ends as itself.
fn no_harness_in_an_aterm_tab_says_nothing_to_aterm() {
    let s = Scenario::new("no-harness-aterm");
    let pty = Pty::open();
    let fake = rostered_aterm(&s);
    s.add_build(BUILD_1, Some("9.9.1"));
    s.add_build(BUILD_2, Some("9.9.2"));
    s.set_current(BUILD_1);
    let mut cmd = s.command(&[
        "--alab-quiet-ms=600",
        "--alab-poll-ms=40",
        "run",
        "--",
        "--model",
        "opus",
    ]);
    cmd.envs(fake.env()).env("ATERM_NO_HARNESS", "1");
    let (_child, pid) = spawn_on_pty(&s, &pty, cmd, None);
    let starts = s.starts();
    assert_eq!(starts[0].pid, pid, "exec'd in place{}", s.report());
    assert_eq!(
        starts[0].args,
        ["--dangerously-skip-permissions", "--model", "opus"],
        "{}",
        s.report()
    );
    assert!(
        wait_until(Duration::from_secs(10), || !marks(&s).is_empty()),
        "{}",
        s.report()
    );
    assert_eq!(
        marks(&s),
        ["marks\tCLAUDE_ALAB_PID=\tCLAUDE_ALAB_DEPTH=1"],
        "{}",
        s.report()
    );
    // No harness is left to move the session: a new current build is not followed.
    s.set_current(BUILD_2);
    std::thread::sleep(Duration::from_millis(1500));
    assert_eq!(s.starts().len(), 1, "{}", s.report());
    assert!(!got_term(&s), "{}", s.report());
    end_session(&s, pid);
    assert_eq!(
        fake.verbs(),
        Vec::<String>::new(),
        "not a word to aterm{}",
        s.report()
    );
    let journal = s.journal();
    assert!(
        journal.contains(&format!(
            " pid={pid} pass no-harness vendor-build={BUILD_1} vendor=9.9.1\n"
        )),
        "{}",
        s.report()
    );
    assert!(
        !journal.contains(" start harness=") && !journal.contains(" aterm "),
        "{}",
        s.report()
    );
}

/// One launch of a store `claude` that is the script `body`, with `env`, on a
/// fresh pty or without a terminal (`stdin`): the exit code, stderr, how often
/// the script ran, and the journal. The command line, `run -- -c`, is one the
/// harness can resume, so on a terminal every harness level supervises.
fn looped(
    name: &str,
    body: &str,
    env: &[(&str, &str)],
    stdin: Stdin,
) -> (Option<i32>, String, u32, String) {
    let s = Scenario::new(name);
    s.add_build_script(BUILD_1, body);
    s.set_current(BUILD_1);
    let mut cmd = s.command(&["run", "--", "-c"]);
    for name in [
        "TERM_PROGRAM",
        "ATERM_CHILD",
        "ATERM_SESSION_ID",
        "ATERM_NO_HARNESS",
    ] {
        cmd.env_remove(name);
    }
    cmd.envs(env.iter().copied());
    let (code, err) = match stdin {
        Stdin::Terminal => {
            let (status, err) = run_on_pty(&s, cmd, Duration::from_secs(60));
            let code = match status {
                Some(Status::Exited(code)) => Some(code),
                _ => None,
            };
            (code, err)
        }
        Stdin::Null => {
            let out = cmd.output().expect("run");
            (
                out.status.code(),
                String::from_utf8_lossy(&out.stderr).into_owned(),
            )
        }
    };
    (code, err, s.script_runs(), s.journal())
}

/// A store `claude` shaped like the twin: it hands every launch back as
/// `claude-alab run -- "$@"`, by `exec` or as a child.
const TWIN_EXEC: &str = "exec \"$ALAB_TEST_HARNESS\" run -- \"$@\"";
const TWIN_CHILD: &str = "\"$ALAB_TEST_HARNESS\" run -- \"$@\"";

/// The recursion guards run before the command line is read, so the twin's
/// envelope cannot get a loop past them. In a session the harness supervises,
/// a store `claude` shaped like the twin is refused by the parent mark when it
/// execs the harness and bounded by the depth when it runs it as a child. Left
/// out by `ATERM_NO_HARNESS=1`, the harness execs the store entry in its own
/// place, where no parent mark exists, and the depth alone bounds it.
fn the_twin_envelope_does_not_get_a_loop_past_the_guards() {
    let (code, err, runs, journal) = looped("twin-loop-exec", TWIN_EXEC, &[], Stdin::Terminal);
    assert_eq!(code, Some(127), "{err}\n{journal}");
    assert!(err.contains("started as the vendor"), "{err}");
    assert_eq!(runs, 1, "{journal}");
    assert_eq!(journal.matches(" start harness=").count(), 1, "{journal}");
    assert!(!journal.contains(" pass "), "{journal}");

    let (code, err, runs, journal) = looped("twin-loop-child", TWIN_CHILD, &[], Stdin::Terminal);
    assert_eq!(code, Some(127), "{err}\n{journal}");
    assert!(err.contains("already nested 8 deep"), "{err}");
    assert_eq!(runs, 8, "{journal}");
    assert_eq!(journal.matches(" start harness=").count(), 8, "{journal}");
    assert!(!journal.contains(" pass "), "{journal}");

    let left_out = [("ATERM_CHILD", "1"), ("ATERM_NO_HARNESS", "1")];
    for stdin in [Stdin::Terminal, Stdin::Null] {
        for (name, body) in [
            ("twin-loop-left-out-exec", TWIN_EXEC),
            ("twin-loop-left-out-child", TWIN_CHILD),
        ] {
            let (code, err, runs, journal) = looped(name, body, &left_out, stdin);
            bounded_in_place(name, "no-harness", (code, &err, runs, &journal));
        }
    }
}

/// What a loop through in-place runs must have come to: eight levels, each one
/// `pass <reason>` line, then the depth's refusal, and never a supervisor.
fn bounded_in_place(name: &str, reason: &str, got: (Option<i32>, &str, u32, &str)) {
    let (code, err, runs, journal) = got;
    assert_eq!(code, Some(127), "{name}: {err}\n{journal}");
    assert!(err.contains("already nested 8 deep"), "{name}: {err}");
    assert_eq!(
        runs, 8,
        "{name}: eight levels, never the script's own stop at 30\n{journal}"
    );
    assert_eq!(
        journal.matches(&format!(" pass {reason} ")).count(),
        8,
        "{name}: {journal}"
    );
    assert_eq!(journal.matches(" pass ").count(), 8, "{name}: {journal}");
    assert_eq!(
        journal
            .matches(" refused nesting-depth CLAUDE_ALAB_DEPTH=8: ")
            .count(),
        1,
        "{name}: {journal}"
    );
    assert!(!journal.contains(" start harness="), "{name}: {journal}");
    assert!(
        !journal.contains(" refused started-as-vendor "),
        "{name}: {journal}"
    );
}

/// A launch the harness can never move runs the store entry in the harness's
/// own place, where the pid mark is removed (the vendor is nobody's harness
/// child). A store entry that leads back to the harness is therefore bounded
/// by the depth alone, which counts on this path as on every other: eight
/// levels and a refusal, whether the entry execs the harness or runs it as a
/// child, and whichever reason kept the launch from being supervised.
fn an_in_place_run_that_leads_back_to_the_harness_stops_at_the_depth_limit() {
    // No terminal: every level is `pass no-tty`.
    for (name, body) in [
        ("in-place-loop-exec", TWIN_EXEC),
        ("in-place-loop-child", TWIN_CHILD),
    ] {
        let (code, err, runs, journal) = looped(name, body, &[], Stdin::Null);
        bounded_in_place(name, "no-tty", (code, &err, runs, &journal));
    }
    // On a terminal, a one-shot command line that the entry hands back whole.
    for (name, body) in [
        (
            "in-place-loop-print-exec",
            "exec \"$ALAB_TEST_HARNESS\" \"$@\"",
        ),
        ("in-place-loop-print-child", "\"$ALAB_TEST_HARNESS\" \"$@\""),
    ] {
        let s = Scenario::new(name);
        s.add_build_script(BUILD_1, body);
        s.set_current(BUILD_1);
        let (status, err) = run_on_pty(&s, s.command(&["-p", "hi"]), Duration::from_secs(60));
        let code = match status {
            Some(Status::Exited(code)) => Some(code),
            _ => None,
        };
        bounded_in_place(
            name,
            "one-shot",
            (code, &err, s.script_runs(), &s.journal()),
        );
    }
}

// ─────────────────────────────── the fake vendor ───────────────────────────────

mod fake {
    //! The fake vendor `claude`: Claude Code's record and signal behaviour, no TUI.

    use super::*;
    use std::sync::atomic::{AtomicI32, Ordering};

    static LAST: AtomicI32 = AtomicI32::new(0);
    static CONT: AtomicU64 = AtomicU64::new(0);

    extern "C" fn on_signal(sig: libc::c_int) {
        if sig == libc::SIGCONT {
            CONT.fetch_add(1, Ordering::Relaxed);
        } else {
            LAST.store(sig, Ordering::Relaxed);
        }
    }

    fn now_ms() -> u64 {
        u64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_millis(),
        )
        .expect("ms")
    }

    fn log(dir: &Path, line: &str) {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("log"))
            .expect("fake log");
        writeln!(f, "{line}").expect("fake log write");
    }

    fn record_path() -> PathBuf {
        let home = PathBuf::from(std::env::var_os("HOME").expect("HOME"));
        let config =
            std::env::var_os("CLAUDE_CONFIG_DIR").map_or(home.join(".claude"), PathBuf::from);
        config
            .join("sessions")
            .join(format!("{}.json", std::process::id()))
    }

    fn write_record(session: &str, status: &str, status_at: u64, started: u64) {
        let path = record_path();
        let cwd = std::env::current_dir().expect("cwd");
        let text = format!(
            r#"{{"pid":{},"sessionId":"{session}","cwd":"{}","startedAt":{started},"version":"9.9.9","kind":"interactive","entrypoint":"cli","status":"{status}","updatedAt":{status_at},"statusUpdatedAt":{status_at}}}"#,
            std::process::id(),
            cwd.display()
        );
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, text).expect("record");
        std::fs::rename(&tmp, &path).expect("record rename");
    }

    fn end(dir: &Path, code: i32) -> ! {
        let _ = std::fs::remove_file(record_path());
        log(dir, &format!("exit {code}"));
        std::process::exit(code)
    }

    fn handle(sig: libc::c_int) {
        // SAFETY: a zeroed sigaction with a handler is valid; installs for this process.
        unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
            libc::sigemptyset(&raw mut action.sa_mask);
            libc::sigaction(sig, &raw const action, std::ptr::null_mut());
        }
    }

    pub(super) fn run(dir: &Path) -> ! {
        let args: Vec<String> = std::env::args_os()
            .skip(1)
            .map(|a: OsString| a.to_string_lossy().into_owned())
            .collect();
        if args == ["--version"] {
            // Logged, so a test can show the harness never asks.
            log(dir, "version");
            let v = std::env::var("FAKE_VERSION").unwrap_or_else(|_| "9.9.9".into());
            println!("{v} (Claude Code)");
            std::process::exit(0);
        }
        let exe = std::env::args().next().unwrap_or_default();
        let shim = std::env::var("FAKE_SHIM_BUILD").unwrap_or_default();
        log(
            dir,
            &format!(
                "start\t{}\t{exe}\t{shim}\t{}",
                std::process::id(),
                args.join("\u{1f}")
            ),
        );
        log(
            dir,
            &format!(
                "env\tDISABLE_AUTOUPDATER={}\tCLAUDE_CODE_DISABLE_FEEDBACK_SURVEY={}",
                std::env::var("DISABLE_AUTOUPDATER").unwrap_or_default(),
                std::env::var("CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY").unwrap_or_default(),
            ),
        );
        log(
            dir,
            &format!(
                "marks\tCLAUDE_ALAB_PID={}\tCLAUDE_ALAB_DEPTH={}",
                std::env::var("CLAUDE_ALAB_PID").unwrap_or_default(),
                std::env::var("CLAUDE_ALAB_DEPTH").unwrap_or_default(),
            ),
        );
        // Every descriptor this start inherited above stderr, `-` for none.
        let inherited: Vec<String> = (3..256)
            // SAFETY: F_GETFD only asks whether the descriptor is open.
            .filter(|fd| unsafe { libc::fcntl(*fd, libc::F_GETFD) } != -1)
            .map(|fd: i32| fd.to_string())
            .collect();
        log(
            dir,
            &format!(
                "fds\t{}",
                if inherited.is_empty() {
                    "-".to_string()
                } else {
                    inherited.join(",")
                }
            ),
        );
        let mode = std::fs::read_to_string(dir.join("mode")).unwrap_or_default();
        if let Some(code) = mode.trim().strip_prefix("echo:") {
            let mut input = String::new();
            std::io::stdin().read_to_string(&mut input).expect("stdin");
            for line in input.lines() {
                println!("fake-out:{line}");
            }
            eprintln!("fake-err");
            end(dir, code.parse().expect("echo code"));
        }
        for sig in [libc::SIGINT, libc::SIGQUIT, libc::SIGHUP, libc::SIGTERM] {
            handle(sig);
        }
        // SAFETY: `isatty` only inspects fd 0.
        let tty = unsafe { libc::isatty(0) } == 1;
        let mut original_modes = None;
        if tty {
            // Raw mode, as a TUI runs: keys arrive one by one, no signals from keys.
            // The modes found at start are logged first: a relaunched child must
            // find the terminal as the harness found it, not as the last child left it.
            // SAFETY: a zeroed termios is a valid out-parameter; fd 0 is a terminal.
            unsafe {
                let mut t: libc::termios = std::mem::zeroed();
                if libc::tcgetattr(0, &raw mut t) == 0 {
                    original_modes = Some(t);
                    let icanon = t.c_lflag & libc::ICANON != 0;
                    log(dir, &format!("modes icanon={}", u8::from(icanon)));
                    libc::cfmakeraw(&raw mut t);
                    libc::tcsetattr(0, libc::TCSANOW, &raw const t);
                }
            }
        }
        if mode.trim() == "cont-count" {
            handle(libc::SIGCONT);
        }
        let mut background: Vec<std::process::Child> = Vec::new();
        // While deaf, typed input waits unread in the terminal's input queue.
        let mut deaf = false;
        let session = args
            .windows(2)
            .find(|w| w[0] == "--resume")
            .map_or_else(|| SID.to_string(), |w| w[1].clone());
        let started = now_ms();
        let mut status = String::new();
        let deadline = Instant::now() + Duration::from_secs(90);
        let mut continued = 0;
        loop {
            let count = CONT.load(Ordering::Relaxed);
            if count != continued {
                continued = count;
                if count > 1 {
                    // A duplicate handler can change terminal state after
                    // the first resume. The harness must send no duplicate,
                    // rather than later clobber that handler's chosen modes.
                    let original = original_modes.as_ref().expect("terminal modes");
                    // SAFETY: fd 0 is this fake's terminal; the modes came
                    // from tcgetattr before this fake entered raw mode.
                    assert_eq!(unsafe { libc::tcsetattr(0, libc::TCSANOW, original) }, 0);
                }
                log(dir, &format!("cont-count {count}"));
            }
            let want = std::fs::read_to_string(dir.join("status"))
                .map(|s| s.trim().to_string())
                .unwrap_or_else(|_| "idle".to_string());
            // A vendor that has not got as far as its record writes none.
            if want != status && !dir.join("record-hold").exists() {
                status = want;
                write_record(&session, &status, now_ms(), started);
            }
            match LAST.swap(0, Ordering::Relaxed) {
                0 => {}
                libc::SIGINT => {
                    log(dir, "signal INT");
                    end(dir, 42);
                }
                libc::SIGQUIT => {
                    log(dir, "signal QUIT");
                    end(dir, 43);
                }
                libc::SIGTERM => {
                    let ignore = std::fs::read_to_string(dir.join("term"))
                        .is_ok_and(|t| t.trim() == "ignore");
                    // The controller may change the disposition after this receipt.
                    log(dir, "signal TERM");
                    if !ignore {
                        end(dir, 70);
                    }
                }
                libc::SIGHUP => {
                    log(dir, "signal HUP");
                    end(dir, 71);
                }
                other => log(dir, &format!("signal {other}")),
            }
            if let Ok(cmd) = std::fs::read_to_string(dir.join("command")) {
                let _ = std::fs::remove_file(dir.join("command"));
                let cmd = cmd.trim();
                if cmd == "cont-fence" {
                    log(dir, &format!("cont-fence {}", CONT.load(Ordering::Relaxed)));
                } else if let Some(code) = cmd.strip_prefix("exit:") {
                    end(dir, code.parse().expect("exit code"));
                } else if let Some(code) = cmd.strip_prefix("crash:") {
                    // A crash: gone with this code, the session record left
                    // standing and the terminal left raw.
                    log(dir, &format!("crash {code}"));
                    std::process::exit(code.parse().expect("crash code"));
                } else if let Some(sig) = cmd.strip_prefix("die:") {
                    // A crash by signal: the record is left standing.
                    let sig: i32 = sig.parse().expect("signal");
                    log(dir, &format!("die {sig}"));
                    // SAFETY: default disposition, then the signal to this process.
                    unsafe {
                        libc::signal(sig, libc::SIG_DFL);
                        libc::raise(sig);
                    }
                } else if let Some(sig) = cmd.strip_prefix("raise:") {
                    let sig: i32 = sig.parse().expect("signal");
                    log(dir, &format!("raise {sig}"));
                    let _ = std::fs::remove_file(record_path());
                    // SAFETY: default disposition, then the signal to this process.
                    unsafe {
                        libc::signal(sig, libc::SIG_DFL);
                        libc::raise(sig);
                    }
                } else if cmd == "bg-shell" {
                    // A background Bash task: a shell under the vendor, still running
                    // after the turn that started it ended.
                    let sh = Command::new("/bin/sh")
                        .args(["-c", "sleep 60; :"])
                        .stdin(Stdio::null())
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .spawn()
                        .expect("background shell");
                    log(dir, &format!("bg-shell {}", sh.id()));
                    background.push(sh);
                } else if cmd == "bg-stop" {
                    for mut sh in background.drain(..) {
                        let _ = sh.kill();
                        let _ = sh.wait();
                    }
                    log(dir, "bg-stopped");
                } else if cmd == "deaf" || cmd == "hear" {
                    deaf = cmd == "deaf";
                    log(dir, cmd);
                } else if cmd == "stop" {
                    log(dir, "stop");
                    // SAFETY: stops this process until continued.
                    unsafe { libc::raise(libc::SIGSTOP) };
                    log(dir, "continued");
                }
            }
            if tty && !deaf {
                let mut pfd = libc::pollfd {
                    fd: 0,
                    events: libc::POLLIN,
                    revents: 0,
                };
                // SAFETY: one valid pollfd; a zero timeout never blocks.
                if unsafe { libc::poll(&raw mut pfd, 1, 0) } > 0 {
                    let mut buf = [0u8; 256];
                    // SAFETY: `buf` is valid for writes of its length.
                    let n = unsafe { libc::read(0, buf.as_mut_ptr().cast(), buf.len()) };
                    if n > 0 {
                        log(dir, &format!("read {n}"));
                    }
                }
            }
            if Instant::now() > deadline {
                log(dir, "lifetime cap");
                end(dir, 99);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

// ─────────────────────────── malformed adoption ───────────────────────────

/// Spawn the harness as a malformed adoption: the private adopt entry with a
/// capsule marker that names no capsule, under the managed signals blocked as
/// the old image's exec guard leaves them, so the entry holds rather than
/// refusing at once. The process first forks `children` children that each
/// run `/bin/sleep 120`, waiting until each exec happened (a close-on-exec
/// pipe reads end of file), so the harness inherits exactly those children.
/// The gate file a pinned vendor launch of `build` holds ([`Scenario::store`]'s
/// prefix): unlinking it makes that launch's revalidation refuse.
fn vendor_gate(s: &Scenario, build: u64) -> PathBuf {
    s.store
        .parent()
        .and_then(Path::parent)
        .expect("prefix")
        .join("leases")
        .join(format!("store-claude-{build}.gate"))
}

/// What a session whose fallback could not start the previous build must have
/// said and journalled: one line naming both failures, and never "resuming …
/// on" the previous build, which did not happen.
fn assert_no_resume_was_claimed(s: &Scenario, pty: &Pty, cause: &str) {
    assert!(
        wait_until(Duration::from_secs(5), || pty
            .output()
            .contains("9.9.2 failed to start and 9.9.1 could not be restarted")),
        "missing the both-failed notice\n--- pty ---\n{}{}",
        pty.output(),
        s.report()
    );
    let said = pty.output();
    assert!(
        !said.contains("failed to start; resuming conversation") && !said.contains("on 9.9.1"),
        "claimed a resume on the previous build that never started\n--- pty ---\n{said}"
    );
    assert_eq!(
        said.matches("9.9.2 failed to start").count(),
        1,
        "one line for the two failures\n--- pty ---\n{said}"
    );
    // A target that was started and then refused was announced when it
    // started; one that could not be spawned was never resumed on at all.
    assert_eq!(
        said.contains("on the new build"),
        cause != "spawn-failed",
        "the resume on the new build is claimed exactly when it was started\n--- pty ---\n{said}"
    );
    let journal = s.journal();
    assert!(
        journal.contains(&format!(
            "relaunch-fallback-failed {cause} failed-build={BUILD_2} fallback-build={BUILD_1} session={SID} "
        )) && journal.contains(" exit relaunch-failed "),
        "{}",
        s.report()
    );
    assert!(
        !journal.contains(&format!("relaunch-fallback {cause}")),
        "journalled a fallback that never started{}",
        s.report()
    );
}

/// The target never ran and the previous build's launch is refused as well:
/// the harness has no build to run the session on and must not say it resumes.
fn a_fallback_that_cannot_restart_the_previous_build_claims_no_resume() {
    let s = Scenario::new("fallback-refused-spawn");
    s.put("term", "ignore");
    let pty = Pty::open();
    let (_child, pid) = start_on_pty(&s, &pty, &["--model", "opus"]);
    s.set_current(BUILD_2);
    assert!(
        wait_until(Duration::from_secs(15), || s
            .log()
            .iter()
            .any(|l| l == "signal TERM")),
        "{}",
        s.report()
    );
    // Both pinned launches are invalidated after SIGTERM: the target cannot be
    // started, and neither can the build the session ran on.
    for build in [BUILD_1, BUILD_2] {
        std::fs::remove_file(vendor_gate(&s, build)).expect("unlink a pinned gate");
    }
    s.put("term", "exit");
    s.put("command", "exit:0");
    assert_eq!(
        wait_status(pid, Duration::from_secs(15)),
        Some(Status::Exited(127)),
        "{}\n--- pty ---\n{}",
        s.report(),
        pty.output()
    );
    assert_eq!(s.starts().len(), 1, "{}", s.report());
    assert_no_resume_was_claimed(&s, &pty, "spawn-failed");
}

/// The other order: the target started and refused, and by then the previous
/// build's launch is refused too.
fn a_failed_start_whose_fallback_is_refused_claims_no_resume() {
    let s = Scenario::new("fallback-refused-start");
    let pty = Pty::open();
    let (_child, pid) = start_on_pty(&s, &pty, &["--model", "opus"]);
    // BUILD_2 unlinks the previous build's pinned gate and then refuses to start.
    s.add_build_script(
        BUILD_2,
        &format!("rm -f '{}'\nexit 2", vendor_gate(&s, BUILD_1).display()),
    );
    s.set_current(BUILD_2);
    assert_eq!(
        wait_status(pid, Duration::from_secs(20)),
        Some(Status::Exited(127)),
        "{}\n--- pty ---\n{}",
        s.report(),
        pty.output()
    );
    assert_eq!(s.script_runs(), 1, "{}", s.report());
    assert_eq!(s.starts().len(), 1, "{}", s.report());
    assert_no_resume_was_claimed(&s, &pty, "child=Exited(2)");
}

/// A harness that cannot set up its signal pipe (one free descriptor, and a
/// pipe needs two) refuses with exit 1, also when stderr is broken: the notice
/// is written the way every other one is, and a failed write is not a panic.
/// Its stdin is a terminal, so the launch is one it supervises; only a
/// supervisor sets signal handling up at all.
fn a_failed_signal_setup_over_a_broken_stderr_is_a_refusal_not_a_panic() {
    let s = Scenario::new("signal-setup-refused");
    s.add_build(BUILD_1, Some("9.9.1"));
    s.set_current(BUILD_1);
    let pty = Pty::open();
    let run = |stderr: Stdio| {
        let mut cmd = s.command(&[]);
        cmd.stdin(Stdio::from(pty.slave.try_clone().expect("clone slave")))
            .stdout(Stdio::null())
            .stderr(stderr);
        // SAFETY: getrlimit and setrlimit are async-signal-safe and act only on
        // the child between fork and exec; `limit` is valid storage for both.
        unsafe {
            cmd.pre_exec(|| {
                let mut limit = libc::rlimit {
                    rlim_cur: 0,
                    rlim_max: 0,
                };
                if libc::getrlimit(libc::RLIMIT_NOFILE, &raw mut limit) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                // Descriptors 0, 1 and 2 are open: exactly one more may be.
                limit.rlim_cur = 4;
                if libc::setrlimit(libc::RLIMIT_NOFILE, &raw const limit) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        cmd.status().expect("run the harness").code()
    };
    let said = s.root.join("stderr");
    let code = run(Stdio::from(
        std::fs::File::create(&said).expect("stderr file"),
    ));
    let text = text_of(&said);
    assert!(
        text.contains("cannot set up signal handling"),
        "the descriptor limit did not reach the signal setup: {text}{}",
        s.report()
    );
    assert_eq!(code, Some(1), "{text}");
    assert!(
        s.journal().contains(" refused signal-setup "),
        "{}",
        s.report()
    );
    // The same refusal with a stderr nobody reads: every write to it fails.
    let (reader, writer) = std::io::pipe().expect("pipe");
    drop(reader);
    assert_eq!(
        run(Stdio::from(writer)),
        Some(1),
        "the notice to a broken stderr panicked{}",
        s.report()
    );
    assert!(s.starts().is_empty(), "{}", s.report());
}

/// Inside aterm the default host can never admit an own-image move, so a newly
/// selected harness build is held with one journal line and is never run as a
/// capability probe (which would also leave its first-start mark in the store).
fn inside_aterm_a_new_harness_build_is_held_and_never_probed() {
    let s = Scenario::new("aterm-never-probes");
    s.add_build(BUILD_1, Some("9.9.1"));
    s.set_current(BUILD_1);
    // Two complete builds of the harness's own package, the first selected and run.
    let program = s
        .store
        .parent()
        .expect("store")
        .join(claude_code_alab::self_image::PACKAGE);
    let own = |build: u64| {
        let bin = program.join(build.to_string()).join("bin");
        std::fs::create_dir_all(&bin).expect("own bin");
        let exe = bin.join(claude_code_alab::self_image::COMMAND);
        std::fs::copy(HARNESS, &exe).expect("copy the harness image");
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        std::fs::write(
            program.join(format!("{build}.ready")),
            format!(
                "ok\nplatform={}-{}\ncontents=bin:{}\n",
                std::env::consts::ARCH,
                std::env::consts::OS,
                claude_code_alab::self_image::COMMAND
            ),
        )
        .expect("own ready");
        std::fs::write(
            program.join(format!("{build}.version")),
            format!("version v1\n{VERSION}\n"),
        )
        .expect("own version");
        exe
    };
    let select = |build: u64| {
        let next = program.join("current.next");
        std::os::unix::fs::symlink(build.to_string(), &next).expect("own selection");
        std::fs::rename(&next, program.join("current")).expect("flip own selection");
    };
    let running = own(1);
    own(2);
    select(1);
    let mut cmd = s.command(&["--alab-quiet-ms=600", "--alab-poll-ms=40"]);
    let mut own_cmd = Command::new(&running);
    own_cmd.args(cmd.get_args()).current_dir(&s.work);
    for (key, value) in cmd.get_envs() {
        match value {
            Some(value) => own_cmd.env(key, value),
            None => own_cmd.env_remove(key),
        };
    }
    cmd = own_cmd;
    cmd.env("ATERM_SESSION_ID", "scenario")
        .env_remove("TERM_PROGRAM")
        .env_remove("ATERM_CHILD");
    let pty = Pty::open();
    let (_child, pid) = spawn_on_pty(&s, &pty, cmd, None);
    let held = |s: &Scenario| {
        s.journal()
            .matches("harness-image held host-never-admits")
            .count()
    };
    assert!(
        wait_until(Duration::from_secs(10), || held(&s) == 1),
        "the hold was never journalled{}",
        s.report()
    );
    select(2);
    // Many looks later: still one line, and build 2 was neither retained nor run.
    std::thread::sleep(Duration::from_millis(2000));
    let journal = s.journal();
    assert_eq!(held(&s), 1, "{}", s.report());
    assert!(
        !journal.contains("harness-image probing")
            && !journal.contains("target-host-unavailable")
            && !journal.contains("host-admission-unknown"),
        "a build that can never be adopted was probed{}",
        s.report()
    );
    let leftovers: Vec<_> = std::fs::read_dir(&program)
        .expect("own program store")
        .filter_map(|entry| entry.ok().map(|entry| entry.file_name()))
        .filter(|name| name.to_string_lossy().contains("unproven"))
        .collect();
    assert!(
        leftovers.is_empty(),
        "first-start marks were written: {leftovers:?}{}",
        s.report()
    );
    assert_eq!(
        wait_status(pid, Duration::ZERO),
        None,
        "the harness runs on"
    );
    s.put("command", "exit:0");
    assert_eq!(
        wait_status(pid, Duration::from_secs(10)),
        Some(Status::Exited(0)),
        "{}",
        s.report()
    );
}

/// Every refusal the command line makes is one `refused <reason>` journal line
/// (the store's refusals are checked beside their own scenarios); `--version`
/// and `--alab-help` are answers, not refusals, and journal nothing.
fn launch_refusals_are_journalled_and_version_and_help_are_not() {
    let s = Scenario::new("refusals-journalled");
    s.add_build(BUILD_1, Some("9.9.1"));
    s.set_current(BUILD_1);
    for answer in ["--version", "--alab-help"] {
        let out = s.command(&[answer]).output().expect("run");
        assert!(out.status.success(), "{out:?}");
    }
    assert_eq!(s.journal(), "", "an answer was journalled");

    let out = s.command(&["--alab-frob"]).output().expect("run");
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("unknown harness option `--alab-frob`") && err.contains("usage:"),
        "{err}"
    );
    assert!(
        s.journal()
            .ends_with(" refused usage unknown harness option `--alab-frob`\n"),
        "{}",
        s.report()
    );

    let out = s
        .command(&[])
        .env("CLAUDE_ALAB_DEPTH", "deep")
        .output()
        .expect("run");
    assert_eq!(out.status.code(), Some(127), "{out:?}");
    assert!(
        s.journal()
            .ends_with(" refused nesting-depth CLAUDE_ALAB_DEPTH=deep is not a nesting depth\n"),
        "{}",
        s.report()
    );

    // A private image entry with no capsule marker is malformed.
    let out = s
        .command(&["--alab-internal-image-adopt"])
        .env_remove("CLAUDE_ALAB_IMAGE_FD")
        .env_remove("CLAUDE_ALAB_IMAGE_PROBE")
        .output()
        .expect("run");
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("refusing private image entry: "), "{err}");
    assert!(
        s.journal()
            .lines()
            .last()
            .is_some_and(|l| l.contains(" refused private-image-entry ")),
        "{}",
        s.report()
    );
    assert_eq!(s.journal().lines().count(), 3, "{}", s.report());
    assert!(s.starts().is_empty(), "{}", s.report());
}

/// The private image words select a private entry only as the first argument.
/// After a vendor word they are the vendor's own arguments and reach it
/// unchanged. A marker in the environment still selects the private entry
/// whatever the arguments say, and a malformed one is refused.
fn a_private_image_word_after_a_vendor_word_is_the_vendors() {
    let s = Scenario::new("private-word-passthrough");
    s.add_build(BUILD_1, Some("9.9.1"));
    s.set_current(BUILD_1);
    s.put("mode", "echo:7");
    let args = [
        "-p",
        "--alab-internal-image-adopt",
        "--alab-internal-image-probe=0123456789abcdef0123456789abcdef",
    ];
    let out = s
        .command(&args)
        .env_remove("CLAUDE_ALAB_IMAGE_FD")
        .env_remove("CLAUDE_ALAB_IMAGE_PROBE")
        .stdin(Stdio::null())
        .output()
        .expect("run");
    assert_eq!(out.status.code(), Some(7), "{out:?}{}", s.report());
    let starts = s.starts();
    assert_eq!(starts.len(), 1, "{}", s.report());
    assert_eq!(starts[0].args, args, "every argument, unchanged");
    assert!(!s.journal().contains(" refused "), "{}", s.report());
    // No terminal and `-p`: the vendor ran in the harness's place.
    assert!(
        s.journal().ends_with(&format!(
            " pass no-tty vendor-build={BUILD_1} vendor=9.9.1\n"
        )) && !s.journal().contains(" start harness="),
        "{}",
        s.report()
    );

    // The same words under a marker are a private entry, and a malformed one.
    for (marker, value) in [
        (
            "CLAUDE_ALAB_IMAGE_PROBE",
            "0123456789abcdef0123456789abcdef",
        ),
        ("CLAUDE_ALAB_IMAGE_FD", "3"),
    ] {
        let out = s
            .command(&args)
            .env_remove("CLAUDE_ALAB_IMAGE_FD")
            .env_remove("CLAUDE_ALAB_IMAGE_PROBE")
            .env(marker, value)
            .stdin(Stdio::null())
            .output()
            .expect("run");
        assert_eq!(out.status.code(), Some(2), "{marker}: {out:?}");
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("refusing private image entry: "),
            "{marker}: {out:?}"
        );
        assert!(
            s.journal()
                .lines()
                .last()
                .is_some_and(|l| l.contains(" refused private-image-entry ")),
            "{marker}{}",
            s.report()
        );
    }
    // A leading private word with no marker stays a malformed private entry,
    // whatever follows it.
    for first in [
        "--alab-internal-image-adopt",
        "--alab-internal-image-probe=0123456789abcdef0123456789abcdef",
    ] {
        let out = s
            .command(&[first, "-p", "hi"])
            .env_remove("CLAUDE_ALAB_IMAGE_FD")
            .env_remove("CLAUDE_ALAB_IMAGE_PROBE")
            .stdin(Stdio::null())
            .output()
            .expect("run");
        assert_eq!(out.status.code(), Some(2), "{first}: {out:?}");
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("refusing private image entry: "),
            "{first}: {out:?}"
        );
    }
    assert_eq!(
        s.starts().len(),
        1,
        "no refusal ran the vendor{}",
        s.report()
    );
}

fn spawn_malformed(s: &Scenario, err: &Path, children: usize) -> (std::process::Child, i32) {
    use std::ffi::CString;
    // SAFETY: zeroed native set storage, initialized by sigemptyset below.
    let mut blocked: libc::sigset_t = unsafe { std::mem::zeroed() };
    // SAFETY: `blocked` is valid set storage; every member is a valid signal.
    unsafe {
        libc::sigemptyset(&raw mut blocked);
        for &sig in claude_code_alab::sys::HANDLED {
            libc::sigaddset(&raw mut blocked, sig);
        }
    }
    let program = CString::new("/bin/sleep").expect("path");
    let name = CString::new("sleep").expect("arg0");
    let seconds = CString::new("120").expect("arg1");
    let mut cmd = s.command(&["--alab-internal-image-adopt"]);
    cmd.env("CLAUDE_ALAB_IMAGE_FD", "2")
        .env_remove("CLAUDE_ALAB_IMAGE_PROBE")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(std::fs::File::create(err).expect("stderr file"));
    // SAFETY: between fork and exec this runs only async-signal-safe calls
    // (sigprocmask, pipe, fcntl, fork, close, execv, _exit, read) on storage
    // prepared before the fork.
    unsafe {
        cmd.pre_exec(move || {
            let fail = || Err(std::io::Error::last_os_error());
            if libc::sigprocmask(libc::SIG_BLOCK, &raw const blocked, std::ptr::null_mut()) != 0 {
                return fail();
            }
            for _ in 0..children {
                let mut ends = [0; 2];
                if libc::pipe(ends.as_mut_ptr()) != 0
                    || libc::fcntl(ends[1], libc::F_SETFD, libc::FD_CLOEXEC) != 0
                {
                    return fail();
                }
                match libc::fork() {
                    -1 => return fail(),
                    0 => {
                        libc::close(ends[0]);
                        let argv = [name.as_ptr(), seconds.as_ptr(), std::ptr::null()];
                        libc::execv(program.as_ptr(), argv.as_ptr());
                        libc::_exit(127)
                    }
                    _ => {
                        libc::close(ends[1]);
                        let mut byte = 0_u8;
                        while libc::read(ends[0], (&raw mut byte).cast(), 1) < 0
                            && std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR)
                        {
                        }
                        libc::close(ends[0]);
                    }
                }
            }
            Ok(())
        });
    }
    let child = cmd.spawn().expect("spawn the malformed adoption");
    let pid = child_pid(&child);
    (child, pid)
}

/// The text `path` holds now, or empty.
fn text_of(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

/// The pids whose parent is `pid`.
fn children_of(pid: i32) -> Vec<i32> {
    let out = Command::new("/usr/bin/pgrep")
        .args(["-P", &pid.to_string()])
        .output()
        .expect("pgrep");
    String::from_utf8_lossy(&out.stdout)
        .split_ascii_whitespace()
        .filter_map(|word| word.parse().ok())
        .collect()
}

/// Whether a wait status is an end, after which the pid is reaped.
fn ended(status: Option<&Status>) -> bool {
    matches!(status, Some(Status::Exited(_) | Status::Signaled(_)))
}

/// What a failed malformed-adoption test must not leave behind: the harness
/// (killed and reaped unless already reaped) and its inherited children (each
/// killed only while it is still the same process, by its birth stamp).
struct Leftovers {
    harness: i32,
    reaped: bool,
    children: Vec<(i32, claude_code_alab::procs::ProcessIdentity)>,
}

impl Leftovers {
    fn new(harness: i32) -> Self {
        Self {
            harness,
            reaped: false,
            children: Vec::new(),
        }
    }

    /// Prove and remember the harness's inherited `sleep` children.
    fn adopt(&mut self, pids: &[i32]) {
        for &pid in pids {
            let identity = claude_code_alab::procs::capture_process(pid as u32)
                .expect("inherited child identity");
            assert!(
                identity.executable.path.ends_with("sleep"),
                "an inherited child does not run sleep: {identity:?}"
            );
            self.children.push((pid, identity));
        }
    }

    /// Wait up to `limit` for the harness to change state, noting an end.
    fn wait(&mut self, limit: Duration) -> Option<Status> {
        let status = wait_status(self.harness, limit);
        self.reaped |= ended(status.as_ref());
        status
    }
}

impl Drop for Leftovers {
    fn drop(&mut self) {
        // A birth stamp survives the reparenting once the harness is gone.
        for (pid, identity) in &self.children {
            if claude_code_alab::procs::capture_process(*pid as u32)
                .is_some_and(|now| now.birth == identity.birth)
            {
                // SAFETY: a plain signal to the same process this test proved.
                unsafe { libc::kill(*pid, libc::SIGKILL) };
            }
        }
        if !self.reaped {
            // SAFETY: the harness is this test's own unreaped child.
            unsafe {
                libc::kill(self.harness, libc::SIGKILL);
                libc::waitpid(self.harness, std::ptr::null_mut(), 0);
            }
        }
    }
}

/// Wait for the malformed adoption to hold, and return its children.
fn held_children(s: &Scenario, err: &Path, left: &mut Leftovers) -> Vec<i32> {
    let held = wait_until(Duration::from_secs(10), || {
        text_of(err).contains("image handoff held: malformed adoption")
    });
    assert!(
        held,
        "the adoption never held: {}{}",
        text_of(err),
        s.report()
    );
    let children = children_of(left.harness);
    left.adopt(&children);
    assert_eq!(
        left.wait(Duration::from_millis(500)),
        None,
        "the hold ended while its children live: {}",
        text_of(err)
    );
    children
}

/// The hold follows `child` into a stop, continues it once continued itself,
/// and ends by its signal death.
fn follow_stop_and_signal_death(s: &Scenario, err: &Path, left: &mut Leftovers, child: i32) {
    let report = |why: &str| format!("{why}: {}{}", text_of(err), s.report());
    signal(child, libc::SIGSTOP);
    assert_eq!(
        left.wait(Duration::from_secs(10)),
        Some(Status::Stopped),
        "{}",
        report("the hold did not stop with its child")
    );
    signal(left.harness, libc::SIGCONT);
    assert_eq!(
        left.wait(Duration::from_secs(10)),
        Some(Status::Continued),
        "{}",
        report("the hold did not continue")
    );
    // The hold continues its child once it is continued itself.
    let running = wait_until(Duration::from_secs(5), || {
        Command::new("/bin/ps")
            .args(["-o", "stat=", "-p", &child.to_string()])
            .output()
            .is_ok_and(|out| {
                !String::from_utf8_lossy(&out.stdout)
                    .trim_start()
                    .starts_with('T')
            })
    });
    assert!(running, "{}", report("the hold did not continue its child"));
    signal(child, libc::SIGUSR2);
    assert_eq!(
        left.wait(Duration::from_secs(10)),
        Some(Status::Signaled(libc::SIGUSR2)),
        "{}",
        report("the hold did not end by its child's signal")
    );
}

/// A malformed adoption that owns no child has nothing to hold: it is refused
/// like any other malformed entry instead of waiting forever.
fn a_malformed_adoption_without_a_child_is_refused() {
    let s = Scenario::new("malformed-alone");
    let err = s.root.join("stderr");
    let (_child, pid) = spawn_malformed(&s, &err, 0);
    let mut left = Leftovers::new(pid);
    let status = left.wait(Duration::from_secs(10));
    let said = text_of(&err);
    assert_eq!(
        status,
        Some(Status::Exited(2)),
        "a childless malformed adoption is refused, not held: {said}{}",
        s.report()
    );
    assert!(
        said.contains("image handoff held: malformed adoption")
            && said.contains("refusing private image entry: no child to hold"),
        "{said}"
    );
    assert!(
        s.journal()
            .contains(" refused private-image-entry no child to hold"),
        "{}",
        s.report()
    );
}

/// A malformed adoption proved no saved state, but the kernel proves the one
/// child it inherited across the exec: the hold follows that child like a
/// recorded one.
fn a_malformed_adoption_follows_its_one_child() {
    let s = Scenario::new("malformed-child");
    let err = s.root.join("stderr");
    let (_child, pid) = spawn_malformed(&s, &err, 1);
    let mut left = Leftovers::new(pid);
    let [child] = held_children(&s, &err, &mut left)[..] else {
        panic!("the harness has not exactly one child: {}", text_of(&err));
    };
    follow_stop_and_signal_death(&s, &err, &mut left, child);
}

/// With several children a malformed adoption follows none yet, and reaps the
/// ones that end until one is left, which it then follows like the only one.
fn a_malformed_adoption_follows_the_last_of_several_children() {
    let s = Scenario::new("malformed-several");
    let err = s.root.join("stderr");
    let (_child, pid) = spawn_malformed(&s, &err, 2);
    let mut left = Leftovers::new(pid);
    let [first, last] = held_children(&s, &err, &mut left)[..] else {
        panic!(
            "the harness has not exactly two children: {}",
            text_of(&err)
        );
    };
    signal(first, libc::SIGKILL);
    let reaped = wait_until(Duration::from_secs(10), || children_of(pid) == [last]);
    assert!(
        reaped,
        "the hold did not reap the child that ended: {}{}",
        text_of(&err),
        s.report()
    );
    follow_stop_and_signal_death(&s, &err, &mut left, last);
}
