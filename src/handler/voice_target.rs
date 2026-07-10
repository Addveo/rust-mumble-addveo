use crate::client::ClientArc;
use crate::error::MumbleError;
use crate::handler::Handler;
use crate::proto::mumble::VoiceTarget;
use crate::state::ServerStateRef;

use super::MumbleResult;

impl Handler for VoiceTarget {
    async fn handle(&self, state: &ServerStateRef, client: &ClientArc) -> MumbleResult {
        // mumble spec limits the usable voice targets to 1..=30
        if self.get_id() < 1 || self.get_id() >= 31 {
            tracing::error!("invalid voice target id: {}", self.get_id());
            return Err(MumbleError::InvalidVoiceTarget.into());
        }

        let target_opt = { client.get_target(self.get_id() as u8) };

        // TODO: maybe swap this for raw access (just unwrap) since this shouldn't ever get past
        // the check above
        let target = match target_opt {
            Some(target) => target,
            None => {
                tracing::error!(
                    "{} tried to target voice target {} but the channel didn't exist",
                    client,
                    self.get_id()
                );
                return Ok(());
            }
        };

        target.sessions.clear_async().await;
        target.channels.clear_async().await;

        // Count every addressable target in the registration. In pma-voice each
        // player sits in their OWN channel, so proximity builds the target from
        // CHANNELS (one per nearby player) while radio/phone use SESSIONS. A
        // legit target holds a handful (nearby players / call / radio group); a
        // map-wide cheat registers everyone's channel (or session). Counting
        // channels + sessions is therefore ~= the number of players reached.
        let mut target_count: u32 = 0;

        for target_item in self.get_targets() {
            for session in target_item.get_session() {
                // we clear this above, we won't run into duplicate inserts.
                let _ = target.sessions.insert_async(*session, ()).await;
                target_count += 1;
            }

            if target_item.has_channel_id() {
                // we clear this above, we won't run into duplicate inserts.
                let _ = target.channels.insert_async(target_item.get_channel_id(), ()).await;
                target_count += 1;
            }
        }

        state.anticheat.observe_target_registration(state, client, target_count);

        Ok(())
    }
}
