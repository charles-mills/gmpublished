// https://github.com/garrynewman/bootil/blob/beb4cec8ad29533965491b767b177dc549e62d23/src/3rdParty/globber.cpp
// https://github.com/Facepunch/gmad/blob/master/include/AddonWhiteList.h

use std::{sync::Arc, time::Duration};

use arc_swap::ArcSwap;

const ADDON_WHITELIST_OFFLINE: &[&str] = &[
    "lua/*.lua",
    "scenes/*.vcd",
    "particles/*.pcf",
    "resource/fonts/*.ttf",
    "scripts/vehicles/*.txt",
    "resource/localization/*/*.properties",
    "maps/*.bsp",
    "maps/*.lmp",
    "maps/*.nav",
    "maps/*.ain",
    "maps/thumb/*.png",
    "sound/*.wav",
    "sound/*.mp3",
    "sound/*.ogg",
    "materials/*.vmt",
    "materials/*.vtf",
    "materials/*.png",
    "materials/*.jpg",
    "materials/*.jpeg",
    "materials/colorcorrection/*.raw",
    "models/*.mdl",
    "models/*.phy",
    "models/*.ani",
    "models/*.vvd",
    "models/*.vtx",
    "!models/*.sw.vtx",
    "!models/*.360.vtx",
    "!models/*.xbox.vtx",
    "gamemodes/*/*.txt",
    "!gamemodes/*/*/*.txt",
    "gamemodes/*/*.fgd",
    "!gamemodes/*/*/*.fgd",
    "gamemodes/*/logo.png",
    "gamemodes/*/icon24.png",
    "gamemodes/*/gamemode/*.lua",
    "gamemodes/*/entities/effects/*.lua",
    "gamemodes/*/entities/weapons/*.lua",
    "gamemodes/*/entities/entities/*.lua",
    "gamemodes/*/backgrounds/*.png",
    "gamemodes/*/backgrounds/*.jpg",
    "gamemodes/*/backgrounds/*.jpeg",
    "gamemodes/*/content/models/*.mdl",
    "gamemodes/*/content/models/*.phy",
    "gamemodes/*/content/models/*.ani",
    "gamemodes/*/content/models/*.vvd",
    "gamemodes/*/content/models/*.vtx",
    "!gamemodes/*/content/models/*.sw.vtx",
    "!gamemodes/*/content/models/*.360.vtx",
    "!gamemodes/*/content/models/*.xbox.vtx",
    "gamemodes/*/content/materials/*.vmt",
    "gamemodes/*/content/materials/*.vtf",
    "gamemodes/*/content/materials/*.png",
    "gamemodes/*/content/materials/*.jpg",
    "gamemodes/*/content/materials/*.jpeg",
    "gamemodes/*/content/materials/colorcorrection/*.raw",
    "gamemodes/*/content/scenes/*.vcd",
    "gamemodes/*/content/particles/*.pcf",
    "gamemodes/*/content/resource/fonts/*.ttf",
    "gamemodes/*/content/scripts/vehicles/*.txt",
    "gamemodes/*/content/resource/localization/*/*.properties",
    "gamemodes/*/content/maps/*.bsp",
    "gamemodes/*/content/maps/*.nav",
    "gamemodes/*/content/maps/*.ain",
    "gamemodes/*/content/maps/thumb/*.png",
    "gamemodes/*/content/sound/*.wav",
    "gamemodes/*/content/sound/*.mp3",
    "gamemodes/*/content/sound/*.ogg",
    "data_static/*.txt",
    "data_static/*.dat",
    "data_static/*.json",
    "data_static/*.xml",
    "data_static/*.csv",
    "shaders/fxc/*.vcs",
];

pub const DEFAULT_IGNORE: &[&str] = &[
    ".git/*",
    "*.psd",
    "*.pdn",
    "*.xcf",
    "*.kra",
    "*.svn",
    "*.ini",
    "*.rtf",
    "*.pdf",
    "*.log",
    "*.prt",
    "*.vmf",
    "*.vmx",
    ".DS_Store",
    ".gitignore",
    ".gitmodules",
    ".gitattributes",
    ".vscode/*",
    ".github/*",
    ".vs/*",
    ".editorconfig",
    "LICENSE",
    "LICENSE.*",
    "license",
    "license.*",
    "README",
    "README.*",
    "readme",
    "readme.*",
    "addon.json",
    "addon.txt",
    "addon.jpg",
    "thumbs.db",
    "desktop.ini",
    "models/*.sw.vtx",
    "models/*.360.vtx",
    "models/*.xbox.vtx",
    "gamemodes/*/content/models/*.sw.vtx",
    "gamemodes/*/content/models/*.360.vtx",
    "gamemodes/*/content/models/*.xbox.vtx",
];

fn builtin_whitelist() -> Vec<String> {
    ADDON_WHITELIST_OFFLINE
        .iter()
        .map(|glob| (*glob).to_owned())
        .collect()
}

/// The addon whitelist: the built-in list, optionally refreshed from the
/// upstream gmad source. Cheap to clone (`Arc` internally).
#[derive(Clone, Debug)]
pub struct AddonWhitelist {
    list: Arc<ArcSwap<Vec<String>>>,
}

impl AddonWhitelist {
    #[must_use]
    pub fn new() -> Self {
        Self {
            list: Arc::new(ArcSwap::from_pointee(builtin_whitelist())),
        }
    }

    /// Current addon whitelist. Callers checking many entries should bind
    /// this once and reuse it rather than calling it per entry.
    #[must_use]
    pub fn snapshot(&self) -> Arc<Vec<String>> {
        self.list.load_full()
    }

    /// Fetches the up-to-date whitelist from the upstream gmad source and
    /// stores it, keeping the built-in list on any failure. Performs
    /// blocking HTTPS I/O: call from a background thread, never from
    /// construction.
    pub fn refresh_from_remote(&self) {
        if std::env::var_os("ADDON_WHITELIST_OFFLINE").is_some() {
            return;
        }

        match download_addon_whitelist() {
            Ok(wildcard) => {
                log::info!("Downloaded up to date addon whitelist: {wildcard:#?}");
                self.list.store(Arc::new(wildcard));
            }
            Err(err) => log::warn!("Failed to download addon whitelist: {err:#?}"),
        }
    }
}

impl Default for AddonWhitelist {
    fn default() -> Self {
        Self::new()
    }
}

fn download_addon_whitelist() -> Result<Vec<String>, std::io::Error> {
    // The workspace ureq build carries no bundled webpki roots; certificate
    // verification must go through the OS trust store (PlatformVerifier).
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .tls_config(
            ureq::tls::TlsConfig::builder()
                .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                .build(),
        )
        .timeout_global(Some(Duration::from_secs(2)))
        .build()
        .into();

    let body = agent
        .get("https://raw.githubusercontent.com/Facepunch/gmad/master/include/AddonWhiteList.h")
        .call()
        .map_err(std::io::Error::other)
        .and_then(|mut response| {
            response
                .body_mut()
                .read_to_string()
                .map_err(std::io::Error::other)
        })?;

    let mut wildcard = Vec::new();

    let captures = regex_lite::Regex::new(
        r"static +const +char\* +Wildcard\s*\[\s*\]\s*=\s*\{\s*([\s\S]*?)\s*NULL,?\s*};",
    )
    .unwrap()
    .captures(&body)
    .and_then(|captures| captures.get(1))
    .ok_or_else(|| std::io::Error::other("Failed to parse addon whitelist"))?;

    let line_regex = regex_lite::Regex::new(r#""(.+?)","#).unwrap();

    for line in captures.as_str().lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        if line == "NULL" {
            break;
        } else if let Some(capture) = line_regex.captures(line) {
            wildcard.push(capture.get(1).unwrap().as_str().to_owned());
        }
    }

    if wildcard.is_empty() {
        return Err(std::io::Error::other(
            "Failed to parse addon whitelist (empty)",
        ));
    }

    if !wildcard.iter().any(|glob| glob == "lua/*.lua") {
        // This should definitely be in there, so if it isn't, something has gone wrong. Probably.
        return Err(std::io::Error::other(
            "Failed to parse addon whitelist (missing lua/*.lua)",
        ));
    }

    Ok(wildcard)
}

const WILD_BYTE: u8 = b'*';
const QUESTION_BYTE: u8 = b'?';
const EXCLAMATION_BYTE: u8 = b'!';

fn globber(wild: &str, str: &str) -> bool {
    let wild = wild.as_bytes();
    let path = str.as_bytes();

    let mut w = 0usize;
    let mut s = 0usize;
    let mut star_w: Option<usize> = None;
    let mut star_s = 0usize;

    while s < path.len() {
        if w < wild.len() && wild[w] == WILD_BYTE {
            star_w = Some(w);
            star_s = s;
            w += 1;
        } else if w < wild.len() && (wild[w] == QUESTION_BYTE || wild[w] == path[s]) {
            w += 1;
            s += 1;
        } else if let Some(sw) = star_w {
            w = sw + 1;
            star_s += 1;
            s = star_s;
        } else {
            return false;
        }
    }

    while w < wild.len() && wild[w] == WILD_BYTE {
        w += 1;
    }

    w == wild.len()
}

/// Check if a path is allowed in a GMA file, against a whitelist snapshot
/// obtained from [`snapshot`].
pub fn is_whitelisted_in(whitelist: &[String], str: &str) -> bool {
    let mut valid = false;

    for glob in whitelist {
        if glob.as_bytes().first() == Some(&EXCLAMATION_BYTE) {
            if globber(&glob[1..], str) {
                valid = false;
            }
        } else if !valid && globber(glob, str) {
            valid = true;
        }
    }

    valid
}

/// Check if a path is allowed in a GMA file, against the built-in whitelist.
/// Test-only convenience: production code always goes through an
/// [`AddonWhitelist`] snapshot instead.
#[cfg(test)]
fn is_whitelisted(str: &str) -> bool {
    is_whitelisted_in(&builtin_whitelist(), str)
}

/// Check if a path matches the built-in default-ignore list
pub fn is_default_ignored(str: &str) -> bool {
    DEFAULT_IGNORE.iter().any(|glob| globber(glob, str))
}

/// Check if a path is ignored by a list of custom globs
pub fn is_ignored(str: &str, ignore: &[String]) -> bool {
    if ignore.is_empty() {
        return false;
    }

    for glob in ignore {
        if globber(glob, str) {
            return true;
        }
    }

    false
}

#[test]
fn test_whitelist() {
    let good: &'static [&'static str] = &[
        "lua/test.lua",
        "lua/lol/test.lua",
        "lua/lua/testing.lua",
        "gamemodes/test/something.txt",
        "gamemodes/test/content/sound/lol.wav",
        "materials/lol.jpeg",
        "gamemodes/the_gamemode_name/backgrounds/file_name.jpg",
        "gamemodes/my_base_defence/backgrounds/1.jpg",
    ];

    let bad: &'static [&'static str] = &[
        "test.lua",
        "lua/test.exe",
        "lua/lol/test.exe",
        "gamemodes/test",
        "gamemodes/test/something",
        "gamemodes/test/something/something.exe",
        "gamemodes/test/content/sound/lol.vvv",
        "materials/lol.vvv",
    ];

    for good in good {
        assert!(is_whitelisted(good), "{}", good);
    }

    let whitelist = builtin_whitelist();

    for glob in whitelist.iter().filter(|g| !g.starts_with('!')) {
        let path = glob.replace('*', "test");
        assert!(is_whitelisted(&path), "{}", path);
    }

    for glob in whitelist.iter().filter(|g| !g.starts_with('!')) {
        let path = glob.replace('*', "a");
        assert!(is_whitelisted(&path), "{}", path);
    }

    for bad in bad {
        assert!(!is_whitelisted(bad));
    }
}

#[test]
fn test_ignore() {
    let ignored: &'static [&'static str] = &[
        ".git/index",
        ".git/info/exclude",
        ".git/logs/head",
        ".git/logs/refs/heads/4.0.0",
        ".git/logs/refs/heads/master",
        ".git/logs/refs/remotes/origin/4.0.0",
        ".git/logs/refs/remotes/origin/cracker",
        ".git/logs/refs/remotes/origin/cracker-no-minigames",
        ".git/logs/refs/remotes/origin/master",
        ".git/objects/00/007c75922055623f4177467fd50a7d573c2c86",
        "blah.psd",
        "some/location/blah.psd",
        "some/blah/blah.pdn",
        "hi.xcf",
        "addon.jpg",
        "addon.json",
    ];

    for ignored in ignored {
        assert!(is_default_ignored(ignored));
    }

    let default_ignore: Vec<String> = DEFAULT_IGNORE
        .iter()
        .cloned()
        .map(std::string::ToString::to_string)
        .collect();
    for ignored in ignored {
        assert!(is_ignored(ignored, &default_ignore));
    }

    assert!(is_ignored("lol.txt", &["lol.txt".to_string()]));
    assert!(is_ignored("lua/hello.lua", &["lua/*.lua".to_string()]));
    assert!(is_ignored("lua/hello.lua", &["lua/*".to_string()]));
    assert!(!is_ignored("lol.txt", &[]));
}

#[test]
fn test_exclusions() {
    assert!(is_whitelisted("models/player.vtx"));
    assert!(is_whitelisted("models/weapons/gun.vtx"));

    assert!(!is_whitelisted("models/player.sw.vtx"));
    assert!(!is_whitelisted("models/player.360.vtx"));
    assert!(!is_whitelisted("models/player.xbox.vtx"));
    assert!(!is_whitelisted("models/weapons/gun.sw.vtx"));

    assert!(is_whitelisted("gamemodes/test/content/models/player.vtx"));
    assert!(!is_whitelisted(
        "gamemodes/test/content/models/player.sw.vtx"
    ));
    assert!(!is_whitelisted(
        "gamemodes/test/content/models/player.360.vtx"
    ));
    assert!(!is_whitelisted(
        "gamemodes/test/content/models/player.xbox.vtx"
    ));

    assert!(is_whitelisted("gamemodes/sandbox/info.txt"));
    assert!(is_whitelisted("gamemodes/sandbox/sandbox.fgd"));
    assert!(!is_whitelisted("gamemodes/sandbox/nested/info.txt"));
    assert!(!is_whitelisted(
        "gamemodes/sandbox/entities/weapons/info.txt"
    ));
}

#[test]
fn test_globber_no_oob() {
    assert!(!globber("LICENSE", "LICENSE.bak"));
    assert!(!globber(".gitignore", ".gitignore.bak"));
    assert!(!globber("addon.json", "addon.json.bak"));
    assert!(!globber("a", "abcdef"));
    assert!(globber("LICENSE", "LICENSE"));

    assert!(!is_default_ignored(".gitignore.bak"));
    assert!(!is_default_ignored("thumbs.db.bak"));
    assert!(is_default_ignored(".gitignore"));
    assert!(is_default_ignored("thumbs.db"));

    assert!(!is_ignored("LICENSE.bak", &["LICENSE".to_string()]));
    assert!(is_ignored("LICENSE", &["LICENSE".to_string()]));
}
