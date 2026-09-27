//! The in-Discord log channel setup: where the setup message goes and what the picker offers.

use rageguard::discord::setup::{
    ChannelCandidate, LOG_CHANNEL_SELECT_ID, MAX_PICKER_OPTIONS, choose_welcome_channel,
    log_channel_picker, writable_channels,
};
use serenity::model::id::ChannelId;

fn candidate(id: u64, position: u16, can_send: bool) -> ChannelCandidate {
    ChannelCandidate {
        id: ChannelId::new(id),
        name: format!("channel-{id}"),
        position,
        can_send,
    }
}

#[test]
fn system_channel_is_preferred_when_writable() {
    let channels = [candidate(1, 0, true), candidate(2, 5, true)];
    assert_eq!(
        choose_welcome_channel(Some(ChannelId::new(2)), &channels),
        Some(ChannelId::new(2))
    );
}

#[test]
fn falls_back_to_top_writable_channel() {
    let channels = [
        candidate(1, 0, false), // read-only #rules
        candidate(2, 3, true),
        candidate(3, 1, true),
    ];
    // System channel exists but the bot can't post there.
    assert_eq!(
        choose_welcome_channel(Some(ChannelId::new(1)), &channels),
        Some(ChannelId::new(3))
    );
    assert_eq!(
        choose_welcome_channel(None, &channels),
        Some(ChannelId::new(3))
    );
}

#[test]
fn no_writable_channel_means_no_message_and_no_picker() {
    let channels = [candidate(1, 0, false), candidate(2, 1, false)];
    assert_eq!(choose_welcome_channel(None, &channels), None);
    assert!(log_channel_picker(&channels, None).is_none());
    assert!(log_channel_picker(&[], None).is_none());
}

#[test]
fn picker_lists_only_channels_rageguard_can_post_in() {
    let mut hidden = candidate(10, 0, false);
    hidden.name = "🤖┃bot".into();
    let channels = [hidden, candidate(20, 2, true), candidate(30, 1, true)];

    let json = serde_json::to_value(log_channel_picker(&channels, None).unwrap()).unwrap();
    let menu = &json["components"][0];
    assert_eq!(json["type"], 1, "action row");
    assert_eq!(menu["type"], 3, "string select");
    assert_eq!(menu["custom_id"], LOG_CHANNEL_SELECT_ID);
    assert_eq!(menu["min_values"], 1);
    assert_eq!(menu["max_values"], 1);

    let options = menu["options"].as_array().unwrap();
    let values: Vec<&str> = options
        .iter()
        .map(|o| o["value"].as_str().unwrap())
        .collect();
    assert_eq!(
        values,
        ["30", "20"],
        "channel-list order, hidden channel left out"
    );
    assert_eq!(options[0]["label"], "#channel-30");
}

#[test]
fn picker_preselects_the_current_channel() {
    let channels = [candidate(1, 0, true), candidate(2, 1, true)];
    let json =
        serde_json::to_value(log_channel_picker(&channels, Some(ChannelId::new(2))).unwrap())
            .unwrap();
    let options = json["components"][0]["options"].as_array().unwrap();
    assert_eq!(options[0]["default"], false);
    assert_eq!(options[1]["default"], true);
}

#[test]
fn picker_respects_discords_option_limit() {
    let channels: Vec<_> = (1..=40).map(|i| candidate(i, i as u16, true)).collect();
    assert_eq!(writable_channels(&channels).len(), MAX_PICKER_OPTIONS);
    let json = serde_json::to_value(log_channel_picker(&channels, None).unwrap()).unwrap();
    assert_eq!(
        json["components"][0]["options"].as_array().unwrap().len(),
        MAX_PICKER_OPTIONS
    );
}
