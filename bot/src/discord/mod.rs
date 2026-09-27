//! Serenity integration: gateway event handling and the real moderation backend.

mod handler;
mod moderation_backend;
pub mod setup;

pub use handler::Handler;
pub use moderation_backend::SerenityModeration;

use serenity::model::Permissions;

/// Permissions RageGuard needs, used to build the invite link.
pub fn required_permissions() -> Permissions {
    Permissions::VIEW_CHANNEL
        | Permissions::SEND_MESSAGES
        | Permissions::EMBED_LINKS
        | Permissions::CONNECT
        | Permissions::MODERATE_MEMBERS
}

/// OAuth2 URL that adds the bot with the permissions above and slash-command scope.
pub fn invite_url(application_id: u64) -> String {
    format!(
        "https://discord.com/oauth2/authorize?client_id={application_id}&permissions={}&scope=bot%20applications.commands",
        required_permissions().bits()
    )
}
