//! Permission checks, timeout decision logic and monitor-only behaviour, with Discord mocked.

mod common;

use std::time::{Duration, Instant};

use common::{COMMAND_CHANNEL, GUILD, MockBackend, TARGET, allowed_facts};
use rageguard::{
    detection::{AngerDetection, Trigger},
    moderation::{
        ModerationError, ModerationOutcome, Moderator, RoleInfo, TimeoutBlocker, TimeoutFacts,
        TimeoutRequest, check_timeout_allowed, format_duration, guild_permissions,
        has_moderation_permission, highest_role_position, percent, timeout_message,
    },
};
use serenity::model::{Permissions, id::RoleId};

fn trigger(confidences: &[f32]) -> Trigger {
    let t0 = Instant::now();
    let detections: Vec<_> = confidences
        .iter()
        .enumerate()
        .map(|(i, &confidence)| AngerDetection {
            timestamp: t0 + Duration::from_secs(3 * i as u64),
            confidence,
        })
        .collect();
    Trigger {
        peak_confidence: confidences.iter().copied().fold(0.0, f32::max),
        mean_confidence: confidences.iter().sum::<f32>() / confidences.len() as f32,
        span: Duration::from_secs(3 * (confidences.len() as u64 - 1)),
        detections,
    }
}

fn request() -> TimeoutRequest {
    TimeoutRequest {
        guild_id: GUILD,
        user_id: TARGET,
        user_name: "Username".into(),
        notify_channel: Some(COMMAND_CHANNEL),
        trigger: trigger(&[0.86, 0.89, 0.91]),
        required_detections: 3,
        window: Duration::from_secs(15),
        duration: Duration::from_secs(300),
    }
}

// --- moderator permission checks --------------------------------------------------------------

#[test]
fn moderate_members_or_administrator_may_use_commands() {
    assert!(has_moderation_permission(Permissions::MODERATE_MEMBERS));
    assert!(has_moderation_permission(Permissions::ADMINISTRATOR));
    assert!(has_moderation_permission(
        Permissions::MODERATE_MEMBERS | Permissions::SEND_MESSAGES
    ));
}

#[test]
fn other_permissions_may_not_use_commands() {
    assert!(!has_moderation_permission(Permissions::empty()));
    assert!(!has_moderation_permission(
        Permissions::KICK_MEMBERS | Permissions::BAN_MEMBERS | Permissions::MANAGE_MESSAGES
    ));
    assert!(!has_moderation_permission(Permissions::MUTE_MEMBERS));
}

#[test]
fn invite_link_requests_exactly_the_documented_permissions() {
    // Keep in sync with the README's "Bot permissions" table.
    let url = rageguard::discord::invite_url(42);
    assert!(url.contains("client_id=42"));
    assert!(url.contains("permissions=1099512695808"));
    assert!(url.contains("scope=bot%20applications.commands"));
    assert!(!rageguard::discord::required_permissions().contains(Permissions::ADMINISTRATOR));
}

// --- permission computation -------------------------------------------------------------------

fn role(id: u64, position: u16, permissions: Permissions) -> RoleInfo {
    RoleInfo {
        id: RoleId::new(id),
        position,
        permissions,
    }
}

#[test]
fn guild_permissions_combine_everyone_and_roles() {
    let roles = [
        role(1, 3, Permissions::MODERATE_MEMBERS),
        role(2, 1, Permissions::CONNECT),
    ];
    let perms = guild_permissions(false, Permissions::VIEW_CHANNEL, &roles);
    assert_eq!(
        perms,
        Permissions::VIEW_CHANNEL | Permissions::MODERATE_MEMBERS | Permissions::CONNECT
    );
}

#[test]
fn administrator_and_owner_get_everything() {
    let admin = [role(1, 3, Permissions::ADMINISTRATOR)];
    assert_eq!(
        guild_permissions(false, Permissions::empty(), &admin),
        Permissions::all()
    );
    assert_eq!(
        guild_permissions(true, Permissions::empty(), &[]),
        Permissions::all()
    );
}

#[test]
fn highest_role_position_defaults_to_everyone() {
    assert_eq!(highest_role_position(&[]), 0);
    let roles = [
        role(1, 3, Permissions::empty()),
        role(2, 7, Permissions::empty()),
        role(3, 5, Permissions::empty()),
    ];
    assert_eq!(highest_role_position(&roles), 7);
}

// --- timeout decision logic -------------------------------------------------------------------

#[test]
fn allowed_when_all_checks_pass() {
    assert_eq!(check_timeout_allowed(&allowed_facts()), Ok(()));
}

#[test]
fn blocked_when_target_left_guild() {
    let facts = TimeoutFacts {
        target_in_guild: false,
        ..allowed_facts()
    };
    assert_eq!(
        check_timeout_allowed(&facts),
        Err(TimeoutBlocker::TargetLeftGuild)
    );
}

#[test]
fn blocked_without_moderate_members() {
    let facts = TimeoutFacts {
        bot_permissions: Permissions::CONNECT | Permissions::KICK_MEMBERS,
        ..allowed_facts()
    };
    assert_eq!(
        check_timeout_allowed(&facts),
        Err(TimeoutBlocker::MissingModerateMembers)
    );
}

#[test]
fn blocked_by_role_hierarchy_including_equal_positions() {
    for target_top_role in [5, 9] {
        let facts = TimeoutFacts {
            bot_top_role: 5,
            target_top_role,
            ..allowed_facts()
        };
        assert_eq!(
            check_timeout_allowed(&facts),
            Err(TimeoutBlocker::RoleHierarchy {
                bot: 5,
                target: target_top_role
            })
        );
    }
}

#[test]
fn owner_is_never_timed_out() {
    let facts = TimeoutFacts {
        target_is_owner: true,
        target_top_role: 0,
        ..allowed_facts()
    };
    assert_eq!(
        check_timeout_allowed(&facts),
        Err(TimeoutBlocker::TargetIsOwner)
    );
}

#[test]
fn administrators_are_never_timed_out() {
    let facts = TimeoutFacts {
        target_permissions: Permissions::all(),
        ..allowed_facts()
    };
    assert_eq!(
        check_timeout_allowed(&facts),
        Err(TimeoutBlocker::TargetIsAdministrator)
    );
}

#[test]
fn checks_run_in_documented_order() {
    // Everything wrong at once: membership is reported first.
    let facts = TimeoutFacts {
        target_in_guild: false,
        target_is_owner: true,
        target_permissions: Permissions::all(),
        target_top_role: 10,
        bot_permissions: Permissions::empty(),
        bot_top_role: 1,
    };
    assert_eq!(
        check_timeout_allowed(&facts),
        Err(TimeoutBlocker::TargetLeftGuild)
    );
}

// --- monitor-only mode ------------------------------------------------------------------------

#[tokio::test]
async fn monitor_only_never_applies_a_timeout() {
    let backend = MockBackend::allowed();
    let moderator = Moderator::new(backend.clone(), true);

    let outcome = moderator.handle_trigger(&request()).await;

    assert_eq!(outcome, ModerationOutcome::Simulated { precheck: Ok(()) });
    assert_eq!(
        backend.timeout_count(),
        0,
        "monitor-only must not time anyone out"
    );
    let notices = backend.notices();
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].0, COMMAND_CHANNEL);
    let text = &notices[0].1;
    assert!(text.contains("MONITOR ONLY"));
    assert!(text.contains(&format!("<@{TARGET}>")));
    assert!(text.contains("Confidence:** 91%"));
    assert!(text.contains("Detections:** 3/3"));
    assert!(text.contains("would have been allowed"));
}

#[tokio::test]
async fn monitor_only_reports_what_would_have_blocked_a_timeout() {
    let backend = MockBackend::new(TimeoutFacts {
        bot_top_role: 1,
        target_top_role: 4,
        ..allowed_facts()
    });
    let outcome = Moderator::new(backend.clone(), true)
        .handle_trigger(&request())
        .await;
    let ModerationOutcome::Simulated {
        precheck: Err(reason),
    } = outcome
    else {
        panic!("expected a simulated, blocked outcome: {outcome:?}");
    };
    assert!(reason.contains("highest role"));
    assert_eq!(backend.timeout_count(), 0);
    assert!(backend.notices()[0].1.contains("would have been blocked"));
}

#[tokio::test]
async fn monitor_only_survives_discord_errors_during_precheck() {
    let backend = MockBackend::allowed();
    *backend.facts.lock() = Err(ModerationError("Discord API error: 500".into()));
    let outcome = Moderator::new(backend.clone(), true)
        .handle_trigger(&request())
        .await;
    assert!(
        matches!(outcome, ModerationOutcome::Simulated { precheck: Err(e) } if e.contains("could not verify"))
    );
    assert_eq!(backend.timeout_count(), 0);
}

// --- enforcement mode -------------------------------------------------------------------------

#[tokio::test]
async fn enforcement_applies_timeout_and_notifies() {
    let backend = MockBackend::allowed();
    let outcome = Moderator::new(backend.clone(), false)
        .handle_trigger(&request())
        .await;

    assert_eq!(outcome, ModerationOutcome::TimedOut);
    let timeouts = backend.timeouts.lock().clone();
    assert_eq!(timeouts.len(), 1);
    assert_eq!(timeouts[0].guild_id, GUILD);
    assert_eq!(timeouts[0].user_id, TARGET);
    assert_eq!(timeouts[0].duration, Duration::from_secs(300));
    assert!(timeouts[0].reason.starts_with("RageGuard:"));
    assert!(timeouts[0].reason.len() <= 512);

    let notices = backend.notices();
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].1, timeout_message(&request()));
}

#[tokio::test]
async fn enforcement_respects_prechecks() {
    let backend = MockBackend::new(TimeoutFacts {
        target_is_owner: true,
        ..allowed_facts()
    });
    let outcome = Moderator::new(backend.clone(), false)
        .handle_trigger(&request())
        .await;
    assert_eq!(
        outcome,
        ModerationOutcome::Blocked(TimeoutBlocker::TargetIsOwner)
    );
    assert_eq!(backend.timeout_count(), 0);
    assert!(backend.notices()[0].1.contains("**not** applied"));
}

#[tokio::test]
async fn enforcement_does_not_guess_when_prechecks_fail() {
    let backend = MockBackend::allowed();
    *backend.facts.lock() = Err(ModerationError("rate limited".into()));
    let outcome = Moderator::new(backend.clone(), false)
        .handle_trigger(&request())
        .await;
    assert!(matches!(outcome, ModerationOutcome::Failed(_)));
    assert_eq!(backend.timeout_count(), 0);
}

#[tokio::test]
async fn discord_rejection_is_reported_not_panicked() {
    let backend = MockBackend::allowed();
    *backend.apply_result.lock() = Err(ModerationError("Missing Permissions".into()));
    let outcome = Moderator::new(backend.clone(), false)
        .handle_trigger(&request())
        .await;
    assert_eq!(
        outcome,
        ModerationOutcome::Failed("Missing Permissions".into())
    );
    assert!(backend.notices()[0].1.contains("timeout failed"));
}

#[tokio::test]
async fn no_notice_without_a_channel() {
    let backend = MockBackend::allowed();
    let request = TimeoutRequest {
        notify_channel: None,
        ..request()
    };
    Moderator::new(backend.clone(), true)
        .handle_trigger(&request)
        .await;
    assert!(backend.notices().is_empty());
}

// --- formatting -------------------------------------------------------------------------------

#[test]
fn timeout_message_matches_spec_layout() {
    let expected = format!(
        "⚠️ **RageGuard**\n\n<@{TARGET}> has been timed out for 5 minutes.\n\n**Reason:**\nRepeated angry voice detection\n\n**Confidence:**\n91%\n\n**Detections:**\n3 within 15 seconds"
    );
    assert_eq!(timeout_message(&request()), expected);
}

#[test]
fn durations_and_percentages_read_naturally() {
    assert_eq!(format_duration(Duration::from_secs(300)), "5 minutes");
    assert_eq!(format_duration(Duration::from_secs(60)), "1 minute");
    assert_eq!(
        format_duration(Duration::from_secs(5400)),
        "1 hour 30 minutes"
    );
    assert_eq!(format_duration(Duration::from_secs(15)), "15 seconds");
    assert_eq!(format_duration(Duration::from_secs(1)), "1 second");
    assert_eq!(format_duration(Duration::from_secs(86_400 * 2)), "2 days");
    assert_eq!(format_duration(Duration::ZERO), "0 seconds");
    assert_eq!(percent(0.913), "91%");
    assert_eq!(percent(0.8), "80%");
    assert_eq!(percent(1.2), "100%");
}
