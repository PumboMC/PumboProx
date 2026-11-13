//! `players`: player data, profiles and commands to players.

use std::sync::Arc;

use wasmtime::component::{Accessor, HasSelf};

use crate::bridge::PlayerCommand;
use crate::runtime::PluginState;
use crate::wit::types::{PlayerId, Text};
use crate::wit::{players, types};

impl players::Host for PluginState {
    fn all(&mut self) -> Vec<types::PlayerInfo> {
        self.touch();
        self.host
            .players
            .read()
            .map(|p| p.values().cloned().collect())
            .unwrap_or_default()
    }

    fn get(&mut self, id: PlayerId) -> Option<types::PlayerInfo> {
        self.touch();
        self.host.player(id)
    }

    fn find(&mut self, name: String) -> Option<types::PlayerInfo> {
        self.touch();
        self.host
            .players
            .read()
            .ok()?
            .values()
            .find(|p| p.profile.name.eq_ignore_ascii_case(&name))
            .cloned()
    }

    fn send_message(&mut self, id: PlayerId, msg: Text) {
        self.touch();
        let c = self.text(&msg, Some(id));
        self.send(id, PlayerCommand::Message(c));
    }

    fn send_action_bar(&mut self, id: PlayerId, msg: Text) {
        self.touch();
        let c = self.text(&msg, Some(id));
        self.send(id, PlayerCommand::ActionBar(c));
    }

    fn send_title(&mut self, id: PlayerId, title: Text, subtitle: Text, times: types::TitleTimes) {
        self.touch();
        let title = self.text(&title, Some(id));
        let subtitle = self.text(&subtitle, Some(id));
        self.send(
            id,
            PlayerCommand::Title {
                title,
                subtitle,
                times: times.into(),
            },
        );
    }

    fn clear_title(&mut self, id: PlayerId) {
        self.touch();
        self.send(id, PlayerCommand::ClearTitle);
    }

    fn play_sound(&mut self, id: PlayerId, sound: String, volume: f32, pitch: f32) {
        self.touch();
        if sound.len() <= 256 {
            self.send(
                id,
                PlayerCommand::Sound {
                    sound,
                    volume,
                    pitch,
                },
            );
        }
    }

    fn tab_header_footer(&mut self, id: PlayerId, header: Text, footer: Text) {
        self.touch();
        let header = self.text(&header, Some(id));
        let footer = self.text(&footer, Some(id));
        self.send(id, PlayerCommand::TabHeaderFooter { header, footer });
    }

    fn kick(&mut self, id: PlayerId, reason: Text) {
        self.touch();
        let c = self.text(&reason, Some(id));
        tracing::info!(plugin = %self.slot.id, player = id, "kick: {}", c.plain_text());
        self.send(id, PlayerCommand::Kick(c));
    }

    fn set_property(&mut self, id: PlayerId, prop: types::Property) -> Result<(), String> {
        self.touch();
        self.check_property(id, &prop.name)?;
        if prop.name.len() > 64
            || prop.value.len() > 32 * 1024
            || prop.signature.as_ref().is_some_and(|s| s.len() > 4096)
        {
            return Err("property too large".into());
        }
        if let Ok(mut players) = self.host.players.write()
            && let Some(p) = players.get_mut(&id)
        {
            p.profile.properties.retain(|q| q.name != prop.name);
            p.profile.properties.push(prop.clone());
        }
        self.send(id, PlayerCommand::SetProperty(prop));
        Ok(())
    }

    fn remove_property(&mut self, id: PlayerId, name: String) -> Result<(), String> {
        self.touch();
        self.check_property(id, &name)?;
        if let Ok(mut players) = self.host.players.write()
            && let Some(p) = players.get_mut(&id)
        {
            p.profile.properties.retain(|q| q.name != name);
        }
        self.send(id, PlayerCommand::RemoveProperty(name));
        Ok(())
    }
}

impl PluginState {
    fn check_property(&self, id: PlayerId, name: &str) -> Result<(), String> {
        if !self
            .slot
            .manifest
            .profile_properties
            .iter()
            .any(|p| p == name)
        {
            return Err(format!(
                "property \"{name}\" is not in profile-properties of the manifest"
            ));
        }
        let player = self.host.player(id).ok_or("unknown player")?;
        if player.online_mode && !self.host.cfg.plugins.allow_premium_profile_changes {
            return Err("profiles of premium players cannot be changed".into());
        }
        Ok(())
    }
}

impl players::HostWithStore<PluginState> for HasSelf<PluginState> {
    async fn connect(
        accessor: &Accessor<PluginState, Self>,
        id: PlayerId,
        server: String,
    ) -> Result<(), players::ConnectError> {
        let bridge = accessor.with(|mut a| {
            let s = a.get();
            s.waited();
            Arc::clone(&s.host.bridge)
        });
        bridge.connect(id, server).await
    }

    async fn reconnect(
        accessor: &Accessor<PluginState, Self>,
        id: PlayerId,
    ) -> Result<(), players::ConnectError> {
        let bridge = accessor.with(|mut a| {
            let s = a.get();
            s.waited();
            Arc::clone(&s.host.bridge)
        });
        bridge.reconnect(id).await
    }
}
