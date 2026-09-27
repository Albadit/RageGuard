//! The pre-join check that turns Discord's silent refusals into clear messages.

use rageguard::voice::{VoiceAccessProblem, check_voice_access};
use serenity::model::Permissions;

const CAN_JOIN: Permissions = Permissions::VIEW_CHANNEL.union(Permissions::CONNECT);

#[test]
fn view_and_connect_are_enough() {
    assert_eq!(check_voice_access(CAN_JOIN, None, 3), Ok(()));
    assert_eq!(check_voice_access(Permissions::all(), Some(2), 5), Ok(()));
}

#[test]
fn missing_permissions_are_named() {
    assert_eq!(
        check_voice_access(Permissions::empty(), None, 0),
        Err(VoiceAccessProblem::Missing(vec!["View Channel", "Connect"]))
    );
    let only_view = check_voice_access(Permissions::VIEW_CHANNEL, None, 0).unwrap_err();
    assert_eq!(only_view, VoiceAccessProblem::Missing(vec!["Connect"]));
    assert_eq!(
        only_view.to_string(),
        "RageGuard is missing the **Connect** permission there"
    );
    assert!(only_view.hint().contains("RageGuard role"));
}

#[test]
fn full_channel_blocks_unless_bot_can_move_members() {
    assert_eq!(
        check_voice_access(CAN_JOIN, Some(2), 2),
        Err(VoiceAccessProblem::ChannelFull { limit: 2 })
    );
    assert_eq!(check_voice_access(CAN_JOIN, Some(2), 1), Ok(()));
    assert_eq!(
        check_voice_access(CAN_JOIN | Permissions::MOVE_MEMBERS, Some(2), 2),
        Ok(())
    );
    // A limit of 0 means unlimited.
    assert_eq!(check_voice_access(CAN_JOIN, Some(0), 50), Ok(()));
}
