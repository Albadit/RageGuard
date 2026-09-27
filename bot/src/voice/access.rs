//! Checks, before joining, whether Discord will let RageGuard into a voice channel.
//!
//! Discord silently ignores join requests it refuses, so without this check a missing permission
//! only shows up as "gateway response from Discord timed out".

use std::fmt;

use serenity::model::Permissions;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VoiceAccessProblem {
    /// Permissions RageGuard lacks in the channel, by their Discord names.
    Missing(Vec<&'static str>),
    /// The channel's user limit is reached (and RageGuard can't bypass it).
    ChannelFull { limit: u32 },
}

impl VoiceAccessProblem {
    /// What a moderator should do about it.
    pub fn hint(&self) -> &'static str {
        match self {
            Self::Missing(_) => {
                "Open that channel's settings → Permissions and allow them for the RageGuard role (or for a role RageGuard has)."
            }
            Self::ChannelFull { .. } => {
                "Raise the channel's user limit, or give RageGuard the Move Members permission."
            }
        }
    }
}

impl fmt::Display for VoiceAccessProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing(names) => {
                let names: Vec<String> = names.iter().map(|n| format!("**{n}**")).collect();
                write!(
                    f,
                    "RageGuard is missing the {} permission{} there",
                    names.join(" and "),
                    if names.len() == 1 { "" } else { "s" }
                )
            }
            Self::ChannelFull { limit } => {
                write!(f, "the channel is full ({limit} user limit)")
            }
        }
    }
}

/// Decides from RageGuard's effective permissions in a voice channel whether it can join.
pub fn check_voice_access(
    permissions: Permissions,
    user_limit: Option<u32>,
    occupants: usize,
) -> Result<(), VoiceAccessProblem> {
    let mut missing = Vec::new();
    if !permissions.contains(Permissions::VIEW_CHANNEL) {
        missing.push("View Channel");
    }
    if !permissions.contains(Permissions::CONNECT) {
        missing.push("Connect");
    }
    if !missing.is_empty() {
        return Err(VoiceAccessProblem::Missing(missing));
    }
    // Members with Move Members may join full channels.
    if let Some(limit) = user_limit.filter(|&limit| limit > 0)
        && occupants >= limit as usize
        && !permissions.contains(Permissions::MOVE_MEMBERS)
    {
        return Err(VoiceAccessProblem::ChannelFull { limit });
    }
    Ok(())
}
