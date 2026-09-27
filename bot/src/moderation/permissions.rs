//! Pure permission and role-hierarchy checks. No Discord calls happen here.

use serenity::model::{Permissions, id::RoleId};

/// Whether a member may use RageGuard's commands.
pub fn has_moderation_permission(permissions: Permissions) -> bool {
    permissions.contains(Permissions::MODERATE_MEMBERS)
        || permissions.contains(Permissions::ADMINISTRATOR)
}

/// The parts of a role that matter for moderation decisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RoleInfo {
    pub id: RoleId,
    pub position: u16,
    pub permissions: Permissions,
}

/// Guild-level permissions following Discord's algorithm (channel overwrites do not affect
/// timeouts, so they are ignored).
pub fn guild_permissions(
    is_owner: bool,
    everyone: Permissions,
    member_roles: &[RoleInfo],
) -> Permissions {
    if is_owner {
        return Permissions::all();
    }
    let combined = member_roles
        .iter()
        .fold(everyone, |acc, role| acc | role.permissions);
    if combined.contains(Permissions::ADMINISTRATOR) {
        Permissions::all()
    } else {
        combined
    }
}

/// Position of the member's highest role; 0 (the `@everyone` position) if they have none.
pub fn highest_role_position(member_roles: &[RoleInfo]) -> u16 {
    member_roles.iter().map(|r| r.position).max().unwrap_or(0)
}

/// Everything needed to decide whether the bot may time out the target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeoutFacts {
    pub target_in_guild: bool,
    pub target_is_owner: bool,
    pub target_permissions: Permissions,
    pub target_top_role: u16,
    pub bot_permissions: Permissions,
    pub bot_top_role: u16,
}

/// Why a timeout cannot be applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TimeoutBlocker {
    #[error("the user is no longer in the server")]
    TargetLeftGuild,
    #[error("RageGuard is missing the Moderate Members permission")]
    MissingModerateMembers,
    #[error(
        "RageGuard's highest role (position {bot}) is not above the user's highest role (position {target}); move the RageGuard role higher in Server Settings → Roles"
    )]
    RoleHierarchy { bot: u16, target: u16 },
    #[error("the user is the server owner, who cannot be timed out")]
    TargetIsOwner,
    #[error(
        "the user has the Administrator permission, which Discord never allows to be timed out"
    )]
    TargetIsAdministrator,
}

/// Runs the pre-timeout checks in order: membership, bot permission, role hierarchy, owner,
/// administrator.
pub fn check_timeout_allowed(facts: &TimeoutFacts) -> Result<(), TimeoutBlocker> {
    if !facts.target_in_guild {
        return Err(TimeoutBlocker::TargetLeftGuild);
    }
    if !has_moderation_permission(facts.bot_permissions) {
        return Err(TimeoutBlocker::MissingModerateMembers);
    }
    if facts.bot_top_role <= facts.target_top_role {
        return Err(TimeoutBlocker::RoleHierarchy {
            bot: facts.bot_top_role,
            target: facts.target_top_role,
        });
    }
    if facts.target_is_owner {
        return Err(TimeoutBlocker::TargetIsOwner);
    }
    if facts
        .target_permissions
        .contains(Permissions::ADMINISTRATOR)
    {
        return Err(TimeoutBlocker::TargetIsAdministrator);
    }
    Ok(())
}
