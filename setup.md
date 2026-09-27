# Setting up RageGuard on your Discord server

This guide takes you from nothing to RageGuard running on your server in **monitor-only mode**,
where it logs what it would do but never times anyone out. It takes about 15 minutes.

You will need:

- A Discord account with **Manage Server** permission on the server you're adding the bot to.
- This repository on the computer that will run the bot.
- The AI service running (see [Step 6](#6-start-the-ai-service)).

---

## 1. Create the Discord application

1. Open the [Discord Developer Portal](https://discord.com/developers/applications) and sign in.
2. Click **New Application**, name it `RageGuard`, accept the terms and click **Create**.
3. On the **General Information** page, copy the **Application ID** and keep it for the invite link in Step 3.

## 2. Create the bot and copy its token

1. Open the **Bot** page in the left sidebar.
2. Click **Reset Token**, confirm, and **copy the token**. Discord shows it only once; if you lose
   it, reset it again.
3. Under **Privileged Gateway Intents**, leave **all three switches off**. RageGuard doesn't need
   them.
4. Optional but recommended: turn **Public Bot** off so only you can add the bot to servers. If
   Discord refuses because *private applications cannot have a default authorization link*, open
   the **Installation** page, set **Install Link** to **None**, save, and try again.

> [!CAUTION]
> The token is the bot's password. Anyone who has it can control the bot on every server it's in.
> Never paste it in Discord, screenshots, issues or commits. If it leaks, click **Reset Token**
> immediately. The old token stops working, and you then put the new one in `.env`.

## 3. Invite the bot to your server

1. Open **OAuth2 → OAuth2 URL Generator**.
2. Under **Scopes**, tick `bot` and `applications.commands`.
3. Under **Bot Permissions**, tick:

   | Permission | Used for |
   | --- | --- |
   | View Channels | Seeing voice and text channels |
   | Send Messages | Moderation notices |
   | Embed Links | The `/anger-status` panel |
   | Connect | Joining the monitored member's voice channel |
   | Moderate Members | Applying timeouts |

   Don't tick Administrator; RageGuard doesn't need it.
4. Under **Integration Type**, choose **Guild Install**. (*User Install* is for apps people add to
   their own account; RageGuard has to be a member of your server.)
5. Check the **Generated URL** at the bottom. It should contain `permissions=1099512695808` and
   `scope=bot+applications.commands`. If it only says `scope=bot`, go back and tick
   `applications.commands`.
6. Copy the URL, open it in your browser, pick your server and click **Authorize**.

Shortcut: this link requests exactly those permissions. Replace `YOUR_APPLICATION_ID` with the ID
from Step 1:

```text
https://discord.com/oauth2/authorize?client_id=YOUR_APPLICATION_ID&permissions=1099512695808&integration_type=0&scope=bot
```

The bot appears offline in the member list until you start it in Step 7.

## 4. Put the RageGuard role above the members it moderates

Discord only lets a bot time out members whose highest role is **below** the bot's highest role.

1. Open **Server Settings → Roles**.
2. Drag the **RageGuard** role (created automatically when you invited the bot) **above** the roles
   of the members it might need to time out.
3. Click **Save Changes**.

Two limits apply regardless of role order: Discord never allows timing out the **server owner** or
members with the **Administrator** permission.

### Private channels

If some channels are hidden from `@everyone`, RageGuard can't see them either. Give the
**RageGuard** role access (channel or category → **Edit Channel** → **Permissions** → add
RageGuard):

| Where | Allow |
| --- | --- |
| Every voice channel where members may be monitored (or their category) | **View Channel**, **Connect** |
| The channel you'll use as the log channel ([Step 8](#8-choose-the-moderation-log-channel)) | **View Channel**, **Send Messages** |

Without this, RageGuard says *"I can't join #channel: RageGuard is missing the View Channel and
Connect permissions there"* or *"I can't post in #channel"*.

## 5. Fill in `.env`

In the repository root, copy the example file:

```bash
cp .env.example .env          # Windows PowerShell: Copy-Item .env.example .env
```

Open `.env` and fill it in:

```env
DISCORD_TOKEN=paste-the-token-from-step-2

AI_SERVICE_URL=http://127.0.0.1:8000
AI_SERVICE_PORT=8000

MONITOR_ONLY=true
LOG_LEVEL=info
```

- Paste the token exactly as copied: no quotes, no spaces, and no `Bot ` in front.
- Keep `MONITOR_ONLY=true` for now.
- You don't need any server or channel IDs: the log channel is chosen inside Discord in
  [Step 8](#8-choose-the-moderation-log-channel).
- If something else on your computer already uses port 8000, choose another port, for example
  `AI_SERVICE_PORT=8010` and `AI_SERVICE_URL=http://127.0.0.1:8010`.

`.env` is listed in `.gitignore`, so it won't be committed. Keep it that way.

## 6. Start the AI service

With Docker, from the repository root:

```bash
docker compose up -d
```

The first start downloads the emotion model (about 380 MB). Wait until the health check reports
it as loaded (use your port if you changed it):

```bash
curl http://127.0.0.1:8000/health
# {"status":"ok","model_loaded":true}
```

To run it without Docker, see *Python setup* in [README.md](README.md#python-setup).

## 7. Start the bot

Choose one:

**On your computer** (needs Rust, CMake and a C compiler; see
[Prerequisites](README.md#prerequisites)):

```bash
cd bot
cargo run --release
```

**In Docker** (no Rust install needed; also the way around Windows Smart App Control blocking the
build):

```bash
docker compose --profile bot up -d --build
docker compose logs -f bot
```

A successful start looks like this:

```text
WARN MONITOR_ONLY is enabled: RageGuard will log timeouts but never apply them
INFO connected to Discord bot=RageGuard guilds=1 monitor_only=true
INFO registered 5 slash commands
INFO AI service is ready
```

The bot now shows as online on your server. The first time, the slash commands can take a few
minutes to appear; press **Ctrl+R** in Discord to refresh.

## 8. Choose the moderation log channel

The first time RageGuard starts on your server, it posts a setup message in your server's system
channel (or the first text channel it can write in):

> 👋 **Thanks for adding RageGuard!** Pick the channel where I should post moderation notices…
> `[ Choose the moderation log channel ▾ ]`

1. Create a private channel for moderators first if you don't have one, and give the RageGuard
   role **View Channel** and **Send Messages** in it.
2. Open the dropdown in the setup message and pick that channel. Only members with **Moderate
   Members** can choose.

   The dropdown only lists channels RageGuard can post in. **If your channel isn't in the list**,
   RageGuard can't see it yet: give the RageGuard role **View Channel** and **Send Messages** in
   that channel ([Private channels](#private-channels)), then run `/anger-setup` again and it will
   appear.
3. RageGuard posts a short confirmation in the chosen channel, and the setup message changes to
   ✅ *"RageGuard will post moderation notices in #your-channel"*.

The choice is saved, so it survives restarts. To change it later, or if you missed the setup
message, run **`/anger-setup`** and pick a new channel from the same dropdown.

This channel becomes the **server log** your moderators read. It only gets warnings: timeout
notices (or *would have* timed out, in monitor-only mode), and problems such as RageGuard being
unable to join a voice channel or the AI service being down. Everything else (monitoring started,
joining voice, each angry clip) stays in your own **bot log**: run the **docker compose logs
(bot)** task, or `docker compose logs -f bot`.

## 9. Check that it works

Do this with your own account, or a test account in a voice channel.

1. Join any voice channel.
2. In a text channel, run `/anger-monitor user:@you`. RageGuard joins your voice channel (muted)
   and replies privately with the active rules. The order doesn't matter: if you run the command
   before joining voice, RageGuard waits and joins as soon as you enter a voice channel.
3. Talk for a little while, then run `/anger-status`. **Recent emotion** and **Recent confidence**
   should be filled in, and **Monitor-only** should say *On*.
4. To see a notice, speak loudly and angrily for about 10–15 seconds. RageGuard posts a
   **MONITOR ONLY** notice to your log channel (or to the channel where you ran the command) saying
   a timeout *would* have been applied. Nobody is actually timed out.
5. Run `/anger-stop user:@you`. The bot leaves the voice channel.

The notice also says whether a real timeout would have been allowed. If you test on the **server
owner's** account it will say the timeout would have been blocked, because Discord never lets
anyone time out the owner. That's expected.

Only members with the **Moderate Members** permission can use RageGuard's commands.

## 10. Before turning on real timeouts

Voice emotion detection is probabilistic and can be wrong. In testing, a calmly read sentence
produced "Angry" scores above 90% on some segments. Before enforcing:

1. Tell your members that a moderator may enable voice-emotion monitoring, and follow your local
   rules on consent. See [Privacy considerations](README.md#privacy-considerations).
2. Leave it in monitor-only mode for a while and review the notices. Tune the rules with
   `/anger-config`, for example a higher `threshold` or more `detections`.
3. Confirm the notices say ✅ *"A real timeout would have been allowed"*. If they say ⛔ blocked,
   fix what they report: usually the role order from Step 4, or the Moderate Members permission.
4. Set `MONITOR_ONLY=false` in `.env` and restart the bot. The startup log then warns
   `MONITOR_ONLY is disabled: RageGuard WILL apply real Discord timeouts`.

To go back to safe mode at any time, set `MONITOR_ONLY=true` and restart.

---

## Quick troubleshooting

| Problem | Fix |
| --- | --- |
| `missing required environment variable DISCORD_TOKEN` | `.env` is missing or `DISCORD_TOKEN` is empty. The bot looks for `.env` in the folder you start it from and its parent folders. |
| `Discord rejected DISCORD_TOKEN` | The token is wrong or was reset. Reset it again (Step 2) and paste the new one. |
| Bot is offline on the server | The bot process isn't running. Check its console or `docker compose logs bot`. |
| Slash commands don't show up | Wait a few minutes after the first start and press Ctrl+R in Discord. Check that the invite included `applications.commands` (Step 3) and that you have Moderate Members. |
| Status says *Waiting (user is not in a voice channel)* | Normal: RageGuard joins as soon as the member enters a voice channel. |
| "I can't join #channel: … missing the View Channel and Connect permissions" | Give the RageGuard role those permissions in that voice channel or its category ([Private channels](#private-channels)). |
| "I can't join #channel: the channel is full" | Raise the channel's user limit, or give RageGuard **Move Members**. |
| "Couldn't join … gateway response from Discord timed out" | Discord ignored the join. Usually a permission set on a role or category that RageGuard couldn't check; review the channel's permissions for the RageGuard role. |
| `/anger-status` shows no emotion | The member hasn't spoken yet, or the AI service is down (see the **AI service** field). |
| Notice says blocked by role position | Move the RageGuard role higher (Step 4). |
| No setup message appeared | RageGuard only asks once per server. Run `/anger-setup` instead. |
| "I can't post in #channel" after picking | Give the RageGuard role **View Channel** and **Send Messages** in that channel, then pick again. |

More detail is in [README.md → Troubleshooting](README.md#troubleshooting).
