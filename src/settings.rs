use config::Config;
use log::{info, warn};
use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};
use ruma::RoomId;
use std::path::PathBuf;
use std::sync::mpsc::channel;
use std::sync::{RwLock, RwLockReadGuard};
use std::time::Duration;
use std::{fs, thread};

const DEFAULT_CONFIG: &str = "reactions = [ \"❤️\", \"👍\", \"👎\", \"😂\", \"‼️\", \"❓️\"]\n";

lazy_static::lazy_static! {
    static ref SETTINGS: RwLock<Config> = RwLock::new(build_settings());
}

fn get_path() -> PathBuf {
    let mut path = dirs::config_dir().expect("no config directory");
    path.push("matui");
    path.push("config.toml");

    path
}

fn build_settings() -> Config {
    Config::builder()
        .add_source(config::File::from(get_path().as_path()))
        .build()
        .expect("could not build settings")
}

pub fn get_settings() -> RwLockReadGuard<'static, Config> {
    SETTINGS.read().unwrap()
}

pub fn is_muted(room: &RoomId) -> bool {
    let muted: Vec<String> = get_settings().get("muted").unwrap_or_default();
    muted.contains(&room.to_string())
}

pub fn clean_vim() -> bool {
    get_settings().get("clean_vim").unwrap_or_default()
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
