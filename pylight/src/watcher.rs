//! File system watcher with debouncing support

use crate::Result;
use crossbeam_channel::{self, Receiver, RecvTimeoutError, Sender};
use notify::{Event, EventKind, RecursiveMode, Watcher};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};
use tracing::info;

/// Configuration for the file watcher
#[derive(Debug, Clone)]
pub struct WatcherConfig {
    /// Debounce period in milliseconds
    pub debounce_ms: u64,
    /// Maximum time to wait for debounce timeout in milliseconds
    pub timeout_ms: u64,
    /// Number of files that triggers a full re-index instead of incremental updates
    pub batch_threshold: usize,
}

impl Default for WatcherConfig {
    fn default() -> Self {
        Self {
            debounce_ms: 100,
            timeout_ms: 60000,
            batch_threshold: 10,
        }
    }
}

/// File system event type after processing
#[derive(Debug, Clone)]
pub enum FileEvent {
    /// Single file was modified or created
    FileChanged(PathBuf),
    /// Multiple files changed, triggering a full re-index
    BulkChange(Vec<PathBuf>),
    /// File was removed
    FileRemoved(PathBuf),
}

/// Manages file system watching with debouncing
pub struct FileWatcher {
    _config: WatcherConfig,
    watcher: notify::RecommendedWatcher,
    _event_handler: Arc<dyn FileEventHandler + Send + Sync>,
    _shutdown_tx: Sender<()>,
    _debouncer_handle: Option<thread::JoinHandle<()>>,
}

/// Trait for handling file system events
pub trait FileEventHandler: Send + Sync {
    /// Handle a file system event
    fn handle_event(&self, event: FileEvent);

    /// Check if a path should be watched (e.g., is it a Python file?)
    fn should_watch(&self, path: &Path) -> bool;

    /// Get the workspace root path
    fn workspace_root(&self) -> &Path;
}

impl FileWatcher {
    /// Create a new file watcher with the given configuration
    pub fn new(
        config: WatcherConfig,
        event_handler: Arc<dyn FileEventHandler + Send + Sync>,
    ) -> Result<Self> {
        let (event_tx, event_rx) = crossbeam_channel::unbounded::<notify::Event>();
        let (shutdown_tx, shutdown_rx) = crossbeam_channel::bounded::<()>(1);

        // Create the notify watcher
        let watcher = notify::recommended_watcher(move |res: notify::Result<Event>| {
            if let Ok(event) = res {
                // Send event to our debouncer
                let _ = event_tx.send(event);
            }
        })?;

        // Spawn the debouncer thread
        let event_handler_clone = event_handler.clone();
        let config_clone = config.clone();
        let handle = thread::spawn(move || {
            Self::debouncer_thread(config_clone, event_handler_clone, event_rx, shutdown_rx);
        });

        Ok(Self {
            _config: config,
            watcher,
            _event_handler: event_handler,
            _shutdown_tx: shutdown_tx,
            _debouncer_handle: Some(handle),
        })
    }

    /// The debouncer thread that implements sliding window behavior
    fn debouncer_thread(
        config: WatcherConfig,
        event_handler: Arc<dyn FileEventHandler + Send + Sync>,
        event_rx: Receiver<notify::Event>,
        shutdown_rx: Receiver<()>,
    ) {
        let mut pending_events = HashSet::new();
        let mut last_event_time = Instant::now();
        let mut first_event_time = None;

        let debounce_duration = Duration::from_millis(config.debounce_ms);
        let max_timeout = Duration::from_millis(config.timeout_ms);

        loop {
            // Calculate timeout for receiving events
            let timeout = if pending_events.is_empty() {
                // No pending events, wait longer for the next event
                Duration::from_secs(1)
            } else {
                // We have pending events, check if debounce period has elapsed
                let elapsed = last_event_time.elapsed();
                if elapsed >= debounce_duration {
                    // Process immediately
                    Duration::from_millis(0)
                } else {
                    // Wait for remaining debounce time
                    debounce_duration - elapsed
                }
            };

            // Try to receive events with timeout
            match event_rx.recv_timeout(timeout) {
                Ok(event) => {
                    // Process the event
                    for path in event.paths {
                        match event.kind {
                            EventKind::Create(_) | EventKind::Modify(_) => {
                                if event_handler.should_watch(&path) {
                                    pending_events.insert((path, false)); // false = not removed
                                }
                            }
                            EventKind::Remove(_) => {
                                if event_handler.should_watch(&path) {
                                    pending_events.insert((path, true)); // true = removed
                                }
                            }
                            _ => {}
                        }
                    }

                    // Update timing - this resets the debounce timer
                    if !pending_events.is_empty() {
                        last_event_time = Instant::now();
                        if first_event_time.is_none() {
                            first_event_time = Some(last_event_time);
                        }
                    }

                    // Check if we've exceeded the maximum timeout
                    if let Some(first_time) = first_event_time {
                        if first_time.elapsed() >= max_timeout {
                            info!(
                                "Maximum timeout reached, processing {} events",
                                pending_events.len()
                            );
                            Self::process_events(&config, &event_handler, &mut pending_events);
                            first_event_time = None;
                        }
                    }
                }
                Err(RecvTimeoutError::Timeout) => {
                    // Timeout occurred, check if we need to process events
                    if !pending_events.is_empty() && last_event_time.elapsed() >= debounce_duration
                    {
                        info!(
                            "Debounce period expired, processing {} events",
                            pending_events.len()
                        );
                        Self::process_events(&config, &event_handler, &mut pending_events);
                        first_event_time = None;
                    }
                }
                Err(RecvTimeoutError::Disconnected) => {
                    info!("Event channel disconnected, shutting down debouncer");
                    break;
                }
            }

            // Check for shutdown signal (non-blocking)
            if shutdown_rx.try_recv().is_ok() {
                info!("File watcher debouncer shutting down");
                break;
            }
        }
    }

    /// Process accumulated events
    fn process_events(
        config: &WatcherConfig,
        event_handler: &Arc<dyn FileEventHandler + Send + Sync>,
        pending_events: &mut HashSet<(PathBuf, bool)>,
    ) {
        let total_events = pending_events.len();

        // If we have many changes (including removals), just trigger a bulk re-index
        if total_events >= config.batch_threshold {
            info!(
                "Detected {} file changes (including removals), triggering bulk re-index",
                total_events
            );
            // For bulk changes, we don't need to pass the paths since we'll do a full re-index
            event_handler.handle_event(FileEvent::BulkChange(Vec::new()));
            pending_events.clear();
            return;
        }

        // Otherwise, handle changes individually
        let mut changed_paths = HashSet::new();
        let mut removed_paths = HashSet::new();

        // Separate events by type
        for (path, is_removed) in pending_events.drain() {
            if is_removed {
                removed_paths.insert(path);
            } else if path.exists() {
                // Double-check the file still exists
                changed_paths.insert(path);
            }
        }

        // Handle removed files
        for path in removed_paths {
            event_handler.handle_event(FileEvent::FileRemoved(path));
        }

        // Handle changed files individually
        for path in changed_paths {
            info!("File changed: {}", path.display());
            event_handler.handle_event(FileEvent::FileChanged(path));
        }
    }

    /// Start watching a directory recursively
    pub fn watch(&mut self, path: &Path) -> Result<()> {
        info!(
            "Starting file watcher for: {} (recursive mode)",
            path.display()
        );
        self.watcher.watch(path, RecursiveMode::Recursive)?;
        Ok(())
    }

    /// Stop watching a directory
    pub fn unwatch(&mut self, path: &Path) -> Result<()> {
        info!("Stopping file watcher for: {}", path.display());
        self.watcher.unwatch(path)?;
        Ok(())
    }
}

impl Drop for FileWatcher {
    fn drop(&mut self) {
        // Signal shutdown
        let _ = self._shutdown_tx.send(());

        // Wait for debouncer thread to finish
        if let Some(handle) = self._debouncer_handle.take() {
            let _ = handle.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use parking_lot::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[allow(dead_code)]
    struct TestEventHandler {
        events: Arc<Mutex<Vec<FileEvent>>>,
        counter: Arc<AtomicUsize>,
    }

    impl FileEventHandler for TestEventHandler {
        fn handle_event(&self, event: FileEvent) {
            self.events.lock().push(event);
            self.counter.fetch_add(1, Ordering::SeqCst);
        }

        fn should_watch(&self, path: &Path) -> bool {
            path.extension()
                .and_then(|s| s.to_str())
                .map(|s| s == "py")
                .unwrap_or(false)
        }

        fn workspace_root(&self) -> &Path {
            Path::new(".")
        }
    }

    #[test]
    fn test_watcher_config_default() {
        let config = WatcherConfig::default();
        assert_eq!(config.debounce_ms, 100);
        assert_eq!(config.timeout_ms, 60000);
        assert_eq!(config.batch_threshold, 10);
    }
}
