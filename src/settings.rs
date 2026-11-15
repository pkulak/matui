use config::Config;
use log::{info, warn};
use matrix_sdk::ruma::{EventId, OwnedEventId};
use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::mpsc::channel;
use std::sync::{LazyLock, Mutex, RwLock, RwLockReadGuard};
use std::time::Duration;
use std::{fs, thread};

const DEFAULT_CONFIG: &str = "reactions = [ \"❤️\", \"👍\", \"👎\", \"😂\", \"‼️\", \"❓️\"]\n";

static SETTINGS: LazyLock<RwLock<Config>> = LazyLock::new(|| RwLock::new(build_settings()));

static MUTED_THREADS: LazyLock<Mutex<HashSet<OwnedEventId>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

fn get_path() -> PathBuf {
    let mut path = dirs::config_dir().expect("no config directory");
    path.push("matui");
    path.push("config.toml");

    path
}

fn build_settings() -> Config {
    // tests run on the defaults, whatever the developer's config says
    if cfg!(test) {
        return Config::default();
    }

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

/// Threads are only muted with `:mute`, until restart.
pub fn is_thread_muted(root: &EventId) -> bool {
    MUTED_THREADS.lock().unwrap().contains(root)
}

pub fn set_thread_muted(root: &EventId, muted: bool) {
    let mut threads = MUTED_THREADS.lock().unwrap();

    if muted {
        threads.insert(root.to_owned());
    } else {
        threads.remove(root);
    }
}

pub fn clean_vim() -> bool {
    get_settings().get("clean_vim").unwrap_or_default()
}

pub fn max_events() -> usize {
    let max: Option<i32> = get_settings().get("max_events").ok();

    match max {
        Some(-1) => usize::MAX,
        Some(i) if i > 0 => i as usize,
        Some(_) => {
            warn!("invalid max_events; setting to 8192");
            8192
        }
        None => 8192,
    }
}

pub fn respect_notification_close_reason() -> bool {
    get_settings()
        .get("respect_notification_close_reason")
        .unwrap_or_default()
}

pub fn images() -> bool {
    get_settings().get("images").unwrap_or(true)
}

pub fn message_line_limit() -> usize {
    let limit: Option<i32> = get_settings().get("message_line_limit").ok();

    match limit {
        Some(i) if i > 0 => i as usize,
        _ => 20,
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
