use notify_debouncer_mini::new_debouncer;
use notify_debouncer_mini::notify;
use notify_debouncer_mini::DebounceEventResult;
use std::path::Path;
use std::time::Duration;
use tokio::sync::mpsc;

/// Starts a debounced file watcher for the given paths.
/// When a change is detected (after the 250ms debounce window),
/// sends the changed path string on `change_tx`.
pub fn start_watcher(
    paths: Vec<String>,
    change_tx: mpsc::Sender<String>,
) -> anyhow::Result<notify_debouncer_mini::Debouncer<notify::RecommendedWatcher>> {
    // new_debouncer takes a closure callback, not a channel.
    // We use blocking_send because the callback runs on a non-async thread.
    let mut debouncer = new_debouncer(
        Duration::from_millis(250),
        move |result: DebounceEventResult| {
            if let Ok(events) = result {
                for event in events {
                    let path = event.path.to_string_lossy().to_string();
                    let _ = change_tx.blocking_send(path);
                }
            }
        },
    )?;

    for path in &paths {
        debouncer
            .watcher()
            .watch(Path::new(path), notify::RecursiveMode::Recursive)?;
    }

    Ok(debouncer)
}
