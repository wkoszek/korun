use notify_debouncer_mini::new_debouncer;
use notify_debouncer_mini::notify;
use notify_debouncer_mini::DebounceEventResult;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;

pub type WatcherHandle = Arc<Mutex<notify_debouncer_mini::Debouncer<notify::RecommendedWatcher>>>;

#[derive(Clone)]
struct WatchTarget {
    root: PathBuf,
    match_path: Option<PathBuf>,
    recursive: notify::RecursiveMode,
}

/// Starts a debounced file watcher for the given paths.
/// When a change is detected (after the 250ms debounce window),
/// sends the changed path string on `change_tx`.
pub fn start_watcher(
    paths: Vec<String>,
    change_tx: mpsc::Sender<String>,
) -> anyhow::Result<WatcherHandle> {
    let targets: Vec<WatchTarget> = paths
        .into_iter()
        .map(|path| {
            let path = normalize_path(PathBuf::from(path));
            if path.is_file() {
                let root = path
                    .parent()
                    .map(PathBuf::from)
                    .unwrap_or_else(|| PathBuf::from("."));
                WatchTarget {
                    root,
                    match_path: Some(path),
                    recursive: notify::RecursiveMode::NonRecursive,
                }
            } else {
                WatchTarget {
                    root: path,
                    match_path: None,
                    recursive: notify::RecursiveMode::Recursive,
                }
            }
        })
        .collect();
    let filter_targets = targets.clone();

    // new_debouncer takes a closure callback, not a channel.
    // We use blocking_send because the callback runs on a non-async thread.
    let mut debouncer = new_debouncer(
        Duration::from_millis(250),
        move |result: DebounceEventResult| {
            if let Ok(events) = result {
                for event in events {
                    let event_path = normalize_path(event.path);
                    if !matches_target(&event_path, &filter_targets) {
                        continue;
                    }
                    let path = event_path.to_string_lossy().to_string();
                    let _ = change_tx.blocking_send(path);
                }
            }
        },
    )?;

    for target in &targets {
        debouncer
            .watcher()
            .watch(Path::new(&target.root), target.recursive)?;
    }

    Ok(Arc::new(Mutex::new(debouncer)))
}

fn matches_target(path: &Path, targets: &[WatchTarget]) -> bool {
    targets.iter().any(|target| {
        if let Some(file_path) = &target.match_path {
            path == file_path || file_path.starts_with(path)
        } else {
            path.starts_with(&target.root)
        }
    })
}

fn normalize_path(path: PathBuf) -> PathBuf {
    std::fs::canonicalize(&path).unwrap_or(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_directory_targets_by_prefix() {
        let root = normalize_path(PathBuf::from("/tmp"));
        let targets = vec![WatchTarget {
            root: root.clone(),
            match_path: None,
            recursive: notify::RecursiveMode::Recursive,
        }];

        assert!(matches_target(&root.join("child.txt"), &targets));
    }

    #[test]
    fn matches_file_targets_by_exact_path_or_parent() {
        let file = normalize_path(PathBuf::from("/tmp/korun-watch-file.txt"));
        let parent = file.parent().unwrap().to_path_buf();
        let targets = vec![WatchTarget {
            root: parent.clone(),
            match_path: Some(file.clone()),
            recursive: notify::RecursiveMode::NonRecursive,
        }];

        assert!(matches_target(&file, &targets));
        assert!(matches_target(&parent, &targets));
        assert!(!matches_target(&parent.join("sibling.txt"), &targets));
    }
}
