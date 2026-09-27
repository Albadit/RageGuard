use std::{collections::HashMap, sync::Arc, time::Duration};

use async_trait::async_trait;
use serenity::{
    all::{CreateAllowedMentions, CreateMessage, EditMember},
    cache::Cache,
    http::{Http, HttpError},
    model::{
        Permissions, Timestamp,
        guild::{Member, Role},
        id::{ChannelId, GuildId, RoleId, UserId},
    },
};

use crate::moderation::{
    ModerationBackend, ModerationError, RoleInfo, TimeoutFacts, guild_permissions,
    highest_role_position,
};

/// [`ModerationBackend`] backed by the Discord API. Serenity's HTTP client queues requests
/// behind Discord's rate limits automatically.
pub struct SerenityModeration {
    http: Arc<Http>,
    cache: Arc<Cache>,
}

impl SerenityModeration {
    pub fn new(http: Arc<Http>, cache: Arc<Cache>) -> Self {
        Self { http, cache }
    }

    /// Owner and roles, from the cache when possible.
    async fn guild_roles(
        &self,
        guild_id: GuildId,
    ) -> Result<(UserId, HashMap<RoleId, RoleInfo>), ModerationError> {
        let cached = self
            .cache
            .guild(guild_id)
            .map(|guild| (guild.owner_id, role_table(&guild.roles)));
        if let Some(found) = cached {
            return Ok(found);
        }
        let guild = self.http.get_guild(guild_id).await?;
        Ok((guild.owner_id, role_table(&guild.roles)))
    }
}

fn role_table(roles: &HashMap<RoleId, Role>) -> HashMap<RoleId, RoleInfo> {
    roles
        .iter()
        .map(|(id, role)| {
            (
                *id,
                RoleInfo {
                    id: *id,
                    position: role.position,
                    permissions: role.permissions,
                },
            )
        })
        .collect()
}

fn is_unknown_member(err: &serenity::Error) -> bool {
    matches!(
        err,
        serenity::Error::Http(HttpError::UnsuccessfulRequest(response))
            if response.status_code.as_u16() == 404
    )
}

#[async_trait]
impl ModerationBackend for SerenityModeration {
    async fn timeout_facts(
        &self,
        guild_id: GuildId,
        user_id: UserId,
    ) -> Result<TimeoutFacts, ModerationError> {
        // Always ask the API: without the privileged members intent the cache may still hold
        // members who already left.
        let target = match self.http.get_member(guild_id, user_id).await {
            Ok(member) => Some(member),
            Err(e) if is_unknown_member(&e) => None,
            Err(e) => return Err(e.into()),
        };
        let bot_id = self.cache.current_user().id;
        let bot = self.http.get_member(guild_id, bot_id).await?;
        let (owner_id, roles) = self.guild_roles(guild_id).await?;

        let everyone = roles
            .get(&guild_id.everyone_role())
            .map_or_else(Permissions::empty, |r| r.permissions);
        let member_roles = |member: &Member| -> Vec<RoleInfo> {
            member
                .roles
                .iter()
                .filter_map(|id| roles.get(id).copied())
                .collect()
        };

        let bot_roles = member_roles(&bot);
        let (target_in_guild, target_is_owner, target_permissions, target_top_role) = match &target
        {
            Some(member) => {
                let is_owner = member.user.id == owner_id;
                let target_roles = member_roles(member);
                (
                    true,
                    is_owner,
                    guild_permissions(is_owner, everyone, &target_roles),
                    highest_role_position(&target_roles),
                )
            }
            None => (false, false, Permissions::empty(), 0),
        };

        Ok(TimeoutFacts {
            target_in_guild,
            target_is_owner,
            target_permissions,
            target_top_role,
            bot_permissions: guild_permissions(bot_id == owner_id, everyone, &bot_roles),
            bot_top_role: highest_role_position(&bot_roles),
        })
    }

    async fn apply_timeout(
        &self,
        guild_id: GuildId,
        user_id: UserId,
        duration: Duration,
        reason: &str,
    ) -> Result<(), ModerationError> {
        let until = Timestamp::now().unix_timestamp() + duration.as_secs() as i64;
        let until = Timestamp::from_unix_timestamp(until)
            .map_err(|e| ModerationError(format!("invalid timeout end: {e}")))?;
        // Discord caps audit-log reasons at 512 characters.
        let reason: String = reason.chars().take(512).collect();
        guild_id
            .edit_member(
                &self.http,
                user_id,
                EditMember::new()
                    .disable_communication_until_datetime(until)
                    .audit_log_reason(&reason),
            )
            .await?;
        Ok(())
    }

    async fn notify(&self, channel_id: ChannelId, content: &str) -> Result<(), ModerationError> {
        channel_id
            .send_message(
                &self.http,
                CreateMessage::new()
                    .content(content)
                    .allowed_mentions(CreateAllowedMentions::new()),
            )
            .await?;
        Ok(())
    }
}
