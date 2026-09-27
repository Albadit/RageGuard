//! Session registry concurrency, channel choice with several monitored members, and per-guild
//! settings.

mod common;

use std::sync::Arc;

use common::{GUILD, MODERATOR, TARGET, monitor_info};
use rageguard::{
    config::DetectionConfig,
    monitor::choose_channel,
    state::{GuildSettings, MonitorRegistry, MonitorSession, RegistryError, SettingsStore},
};
use serenity::model::{
    Timestamp,
    id::{ChannelId, GuildId, UserId},
};

fn session_for(guild: GuildId, user: UserId) -> Arc<MonitorSession> {
    let mut info = monitor_info();
    info.guild_id = guild;
    info.user_id = user;
    MonitorSession::new(info)
}

#[tokio::test]
async fn several_members_can_be_monitored_in_one_server() {
    let registry = MonitorRegistry::new();
    registry
        .try_insert(session_for(GUILD, TARGET))
        .await
        .unwrap();
    registry
        .try_insert(session_for(GUILD, UserId::new(999)))
        .await
        .unwrap();
    assert_eq!(registry.sessions(GUILD).await.len(), 2);
    assert_eq!(registry.len().await, 2);
}

#[tokio::test]
async fn the_same_member_cannot_be_monitored_twice() {
    let registry = MonitorRegistry::new();
    registry
        .try_insert(session_for(GUILD, TARGET))
        .await
        .unwrap();
    let err = registry
        .try_insert(session_for(GUILD, TARGET))
        .await
        .unwrap_err();
    assert_eq!(
        err,
        RegistryError::AlreadyMonitoring {
            user: TARGET,
            started_by: MODERATOR
        }
    );
    assert_eq!(registry.len().await, 1);
}

#[tokio::test]
async fn different_guilds_are_independent() {
    let registry = MonitorRegistry::new();
    registry
        .try_insert(session_for(GUILD, TARGET))
        .await
        .unwrap();
    registry
        .try_insert(session_for(GuildId::new(2), TARGET))
        .await
        .unwrap();
    assert_eq!(registry.len().await, 2);
    assert_eq!(registry.sessions(GUILD).await.len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn simultaneous_starts_for_one_member_have_exactly_one_winner() {
    let registry = Arc::new(MonitorRegistry::new());
    let attempts: Vec<_> = (0..32)
        .map(|_| {
            let registry = registry.clone();
            tokio::spawn(async move { registry.try_insert(session_for(GUILD, TARGET)).await })
        })
        .collect();
    let mut winners = 0;
    for attempt in attempts {
        if attempt.await.unwrap().is_ok() {
            winners += 1;
        }
    }
    assert_eq!(winners, 1);
    assert_eq!(registry.len().await, 1);
}

#[tokio::test]
async fn stopping_one_member_keeps_the_others() {
    let registry = MonitorRegistry::new();
    assert_eq!(
        registry.remove_user(GUILD, TARGET).await.unwrap_err(),
        RegistryError::NotMonitoring(TARGET)
    );

    let first = session_for(GUILD, TARGET);
    let other = UserId::new(12345);
    let second = session_for(GUILD, other);
    registry.try_insert(first.clone()).await.unwrap();
    registry.try_insert(second.clone()).await.unwrap();

    let removed = registry.remove_user(GUILD, TARGET).await.unwrap();
    assert!(Arc::ptr_eq(&removed, &first));
    assert!(first.is_cancelled(), "stopping cancels that member's tasks");
    assert!(!second.is_cancelled(), "other members keep being monitored");
    assert!(registry.get(GUILD, other).await.is_some());
    assert!(registry.get(GUILD, TARGET).await.is_none());

    registry.remove_user(GUILD, other).await.unwrap();
    assert!(registry.is_empty().await);
}

#[tokio::test]
async fn remove_session_only_removes_that_exact_session() {
    let registry = MonitorRegistry::new();
    let old = session_for(GUILD, TARGET);
    let new = session_for(GUILD, TARGET);
    registry.try_insert(new.clone()).await.unwrap();

    assert!(!registry.remove_session(&old).await);
    assert!(registry.get(GUILD, TARGET).await.is_some());

    assert!(registry.remove_session(&new).await);
    assert!(registry.get(GUILD, TARGET).await.is_none());
}

#[tokio::test]
async fn drain_cancels_everything() {
    let registry = MonitorRegistry::new();
    let a = session_for(GUILD, TARGET);
    let b = session_for(GUILD, UserId::new(2));
    let c = session_for(GuildId::new(2), TARGET);
    for s in [&a, &b, &c] {
        registry.try_insert(s.clone()).await.unwrap();
    }
    assert_eq!(registry.drain().await.len(), 3);
    assert!(a.is_cancelled() && b.is_cancelled() && c.is_cancelled());
    assert!(registry.is_empty().await);
}

// --- channel choice ---------------------------------------------------------------------------

fn at(seconds: i64) -> Timestamp {
    Timestamp::from_unix_timestamp(1_700_000_000 + seconds).unwrap()
}

#[test]
fn bot_goes_where_most_monitored_members_are() {
    let (a, b) = (ChannelId::new(1), ChannelId::new(2));
    let members = [(at(0), Some(a)), (at(10), Some(b)), (at(20), Some(b))];
    assert_eq!(choose_channel(&members), Some(b));
}

#[test]
fn ties_go_to_the_member_monitored_longest() {
    let (a, b) = (ChannelId::new(1), ChannelId::new(2));
    let members = [(at(10), Some(b)), (at(0), Some(a))];
    assert_eq!(choose_channel(&members), Some(a));
}

#[test]
fn members_not_in_voice_are_ignored() {
    let a = ChannelId::new(1);
    assert_eq!(choose_channel(&[(at(0), None), (at(5), Some(a))]), Some(a));
    assert_eq!(choose_channel(&[(at(0), None)]), None);
    assert_eq!(choose_channel(&[]), None);
}

// --- settings ---------------------------------------------------------------------------------

fn store() -> SettingsStore {
    SettingsStore::new(GuildSettings {
        detection: DetectionConfig::default(),
        log_channel: None,
    })
}

#[tokio::test]
async fn settings_updates_are_validated_and_per_guild() {
    let store = store();
    let updated = store
        .update(GUILD, |s| {
            s.detection.threshold = 0.9;
            s.log_channel = Some(ChannelId::new(7));
        })
        .await
        .unwrap();
    assert_eq!(updated.detection.threshold, 0.9);
    assert_eq!(store.get(GUILD).await.log_channel, Some(ChannelId::new(7)));
    assert_eq!(store.get(GuildId::new(2)).await, *store.defaults());

    let err = store
        .update(GUILD, |s| s.detection.timeout_minutes = 0)
        .await;
    assert!(err.is_err());
    assert_eq!(
        store.get(GUILD).await.detection.timeout_minutes,
        1,
        "invalid change must not be stored"
    );
}

#[tokio::test]
async fn settings_reset_restores_defaults() {
    let store = store();
    store
        .update(GUILD, |s| s.detection.required_detections = 5)
        .await
        .unwrap();
    assert_eq!(store.reset(GUILD).await, *store.defaults());
    assert_eq!(store.get(GUILD).await.detection.required_detections, 2);
}
