use steamworks::{Friend, SteamId};

use super::ConnectedSteam;
use crate::util::main_thread_forbidden;

#[derive(Clone, Debug)]
pub struct SteamUser {
    pub steamid: SteamId,
    pub name: String,
    pub avatar: Option<crate::RgbaImage>,

    pub dead: bool,
}

impl From<Friend> for SteamUser {
    fn from(friend: Friend) -> Self {
        Self {
            steamid: friend.id(),
            name: friend.name(),
            avatar: friend
                .medium_avatar()
                .map(|buf| crate::rgba_image::RgbaImage::new(buf, 64, 64)),
            dead: false,
        }
    }
}

impl ConnectedSteam<'_> {
    pub fn current_user(&self) -> SteamUser {
        self.fetch_user(self.interface.steam_id)
    }

    pub fn fetch_user(&self, steamid: SteamId) -> SteamUser {
        let mut latest = None;
        self.fetch_user_streaming(steamid, |user| latest = Some(user));
        latest.unwrap_or_else(|| {
            SteamUser::from(self.interface.client().friends().get_friend(steamid))
        })
    }

    /// Emits the cached/persona result as soon as it is useful, then emits a
    /// second result if waiting briefly produces the real avatar bytes.
    pub fn fetch_user_streaming(&self, steamid: SteamId, mut on_user: impl FnMut(SteamUser)) {
        main_thread_forbidden!();

        let client = self.interface;

        if let Some(cached) = self.steam.users.read().get(&steamid).cloned() {
            let complete = cached.avatar.is_some();
            on_user(cached);
            if complete {
                return;
            }
        }

        let _slot = self.steam.persona_slot.claim();
        // Another serialized fetch may have completed while this caller was
        // waiting for the callback slot. Reuse it instead of starting the
        // same persona/avatar wait again.
        if let Some(cached) = self.steam.users.read().get(&steamid).cloned()
            && cached.avatar.is_some()
        {
            on_user(cached);
            return;
        }

        // Registered before the request so an event delivered in between
        // cannot be missed.
        let (event_tx, event_rx) = std::sync::mpsc::channel();
        let _persona_cb =
            self.interface
                .register_callback(move |p: steamworks::PersonaStateChange| {
                    if p.steam_id == steamid {
                        let _ = event_tx.send(());
                    }
                });

        if client
            .client()
            .friends()
            .request_user_information(steamid, false)
        {
            // First event for this user: the persona (name) is loaded. At
            // this point Steam serves its default avatar bytes,
            // indistinguishable from a real avatar by presence alone — the
            // downloaded image lands with a later event, observable only as
            // the bytes changing. So baseline here and wait for a change,
            // using further events as wakeups. A user genuinely on the
            // default avatar never changes and rides out the deadline.
            let _ = event_rx.recv_timeout(std::time::Duration::from_secs(10));
            let persona = SteamUser::from(client.client().friends().get_friend(steamid));
            self.steam.users.write().insert(steamid, persona.clone());
            on_user(persona);

            let avatar_baseline = client
                .client()
                .friends()
                .get_friend(steamid)
                .medium_avatar();
            let deadline = std::time::Instant::now() + std::time::Duration::from_millis(2500);
            while std::time::Instant::now() < deadline
                && client
                    .client()
                    .friends()
                    .get_friend(steamid)
                    .medium_avatar()
                    == avatar_baseline
            {
                let _ = event_rx.recv_timeout(std::time::Duration::from_millis(100));
            }
        }

        let user = SteamUser::from(client.client().friends().get_friend(steamid));

        {
            let user = user.clone();
            self.steam.users.write().insert(user.steamid, user);
        }
        on_user(user);
    }
}

pub fn fetch_steam_user(steam: ConnectedSteam<'_>, steamid: SteamId) -> SteamUser {
    steam.fetch_user(steamid)
}

pub fn fetch_steam_user_streaming(
    steam: ConnectedSteam<'_>,
    steamid: SteamId,
    on_user: impl FnMut(SteamUser),
) {
    steam.fetch_user_streaming(steamid, on_user);
}
