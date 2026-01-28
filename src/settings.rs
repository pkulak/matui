use config::{Config};
use log::{info, warn};
use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};
use ruma::RoomId;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::channel;
use std::sync::{LazyLock, Mutex, RwLock, RwLockReadGuard};
use std::time::Duration;
use std::{fs, thread};

const DEFAULT_CONFIG: &str = "reactions = [ \"❤️\", \"👍\", \"👎\", \"😂\", \"‼️\", \"❓️\"]\n";

static SETTINGS: LazyLock<RwLock<Config>> = LazyLock::new(|| RwLock::new(build_settings()));

static OVERRIDES: LazyLock<Mutex<HashMap<String, HashSet<String>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn get_path() -> PathBuf {
    let mut path = dirs::config_dir().expect("no config directory");
    path.push("matui");
    path.push("config.toml");

    path
}

fn build_settings() -> Config {
    let config = Config::builder()
        .add_source(config::File::from(get_path().as_path()))
        .build();

    match config {
        Ok(c) => c,
        Err(error) => {
            info!("could not load config: {}", error);
            Config::default()
        }
    }
}

pub fn get_settings() -> RwLockReadGuard<'static, Config> {
    SETTINGS.read().unwrap()
}

fn with_override_set<F, R>(key: &str, f: F) -> R
where
    F: FnOnce(&mut std::collections::HashSet<String>) -> R
{
    let mut overrides = OVERRIDES.lock().unwrap();
    let set = overrides.entry(key.to_string()).or_default();
    f(set)
}

fn overridden(key: &str, value: &str) -> bool {
    with_override_set(key, |set| {
        set.contains(value)
    })
}

pub fn is_muted(room: &RoomId) -> bool {
    if overridden("muted", room.as_ref()) {
        return true;
    }

    if overridden("unmuted", room.as_ref()) {
        return false;
    }

    let muted: Vec<String> = get_settings().get("muted").unwrap_or_default();
    muted.contains(&room.to_string())
}

pub fn toggle_mute(room: &RoomId) {
    if is_muted(room) {
        unmute(room);
    } else {
        mute(room);
    }
}

fn mute(room: &RoomId) {
    with_override_set("muted", |set| {
        set.insert(room.to_string());
    });

    with_override_set("unmuted", |set| {
        set.remove(&room.to_string());
    });
}

fn unmute(room: &RoomId) {
    with_override_set("unmuted", |set| {
        set.insert(room.to_string());
    });

    with_override_set("muted", |set| {
        set.remove(&room.to_string());
    });
}

pub fn clean_vim() -> bool {
    get_settings().get("clean_vim").unwrap_or_default()
}

pub fn blur_delay() -> i64 {
    get_settings().get("blur_delay").unwrap_or(30)
}

pub fn max_events() -> usize {
    let max: Option<i32> = get_settings().get("max_events").ok();

    match max {
        Some(-1) => usize::MAX,
        Some(i) if i > 0 => i as usize,
        _ => {
            warn!("invalid max_events; setting to 8192");
            1024
        }
    }
}

fn watch_internal() {
    let (tx, rx) = channel();

    let mut watcher: RecommendedWatcher = Watcher::new(
        tx,
        notify::Config::default().with_poll_interval(Duration::from_secs(2)),
    )
    .unwrap();

    watcher
        .watch(get_path().parent().unwrap(), RecursiveMode::NonRecursive)
        .unwrap();

    loop {
        match rx.recv() {
            Ok(Ok(Event {
                kind: notify::event::EventKind::Modify(_),
                ..
            })) => {
                info!("config.toml written; refreshing configuration");
                *SETTINGS.write().unwrap() = build_settings();
            }
            Err(e) => warn!("watch error: {:?}", e),
            _ => {}
        }
    }
}

pub fn watch_settings_forever() {
    // Create the config if it isn't there.
    let path = get_path();

    if !path.exists() {
        let dir = path.parent().unwrap();
        fs::create_dir_all(dir).unwrap();
        fs::write(path, DEFAULT_CONFIG).unwrap();
    }

    // Spawn a thread to keep an eye on it
    thread::spawn(|| {
        watch_internal();
    });
}
