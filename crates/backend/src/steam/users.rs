use rayon::iter::{IndexedParallelIterator, IntoParallelIterator, ParallelIterator};

use steamworks::{Friend, SteamId};

use super::Steam;

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

impl Steam {
    pub fn current_user(&self) -> SteamUser {
        let steamid = self
            .client()
            .expect("reached only through app-layer entry points that already checked steam_connected()")
            .steam_id;
        self.fetch_user(steamid)
    }

    pub fn fetch_user(&self, steamid: SteamId) -> SteamUser {
        main_thread_forbidden!();

        let client = self.client().expect(
            "reached only through app-layer entry points that already checked steam_connected()",
        );

        // See the field doc: overlapping persona waits clobber each other's
        // callback registration and turn into full timeouts.
        let _fetch_guard = self.persona_fetch.lock();

        // Registered before the request so an event delivered in between
        // cannot be missed.
        let (event_tx, event_rx) = std::sync::mpsc::channel();
        let _persona_cb = self.register_callback(move |p: steamworks::PersonaStateChange| {
            if p.steam_id == steamid {
                let _ = event_tx.send(());
            }
        });

        if client.friends().request_user_information(steamid, false) {
            // First event for this user: the persona (name) is loaded. At
            // this point Steam serves its default avatar bytes,
            // indistinguishable from a real avatar by presence alone — the
            // downloaded image lands with a later event, observable only as
            // the bytes changing. So baseline here and wait for a change,
            // using further events as wakeups. A user genuinely on the
            // default avatar never changes and rides out the deadline.
            let _ = event_rx.recv_timeout(std::time::Duration::from_secs(10));
            let avatar_baseline = client.friends().get_friend(steamid).medium_avatar();
            let deadline = std::time::Instant::now() + std::time::Duration::from_millis(2500);
            while std::time::Instant::now() < deadline
                && client.friends().get_friend(steamid).medium_avatar() == avatar_baseline
            {
                let _ = event_rx.recv_timeout(std::time::Duration::from_millis(100));
            }
        }

        let user = SteamUser::from(client.friends().get_friend(steamid));

        {
            let user = user.clone();
            self.users.write().insert(user.steamid, user);
        }

        user
    }

    pub fn fetch_users(&self, steamids: Vec<SteamId>) -> Vec<SteamUser> {
        let mut users = Vec::with_capacity(steamids.len());
        steamids
            .into_par_iter()
            .map(|steamid| self.fetch_user(steamid))
            .collect_into_vec(&mut users);
        users
    }
}

pub fn fetch_steam_user(steam: &Steam, steamid64: u64) -> SteamUser {
    steam.fetch_user(SteamId::from_raw(steamid64))
}
