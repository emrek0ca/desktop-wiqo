//! What a task last saw, and stopping a task (or all of them) from the panel or the phone.

use super::*;

// ---------- what a task last saw ----------

pub(super) fn seen() -> &'static Mutex<HashMap<String, Seen>> {
    static SEEN: OnceLock<Mutex<HashMap<String, Seen>>> = OnceLock::new();
    SEEN.get_or_init(Default::default)
}

pub(super) fn remember(task: &str, observation: &Observation) {
    if let Ok(mut map) = seen().lock() {
        map.insert(
            task.to_string(),
            Seen {
                snapshot_id: observation.snapshot_id.clone(),
                at: Instant::now(),
                scene: Scene::of(observation),
            },
        );
    }
}

/// The tasks the person stopped. Their next command is refused, whatever it is: the model does not get to look again and carry on.
pub(super) fn stopped() -> &'static Mutex<HashSet<String>> {
    static STOPPED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    STOPPED.get_or_init(Default::default)
}

/// "Durdur": every task that has touched the screen stops here. A new request is a new task and runs.
pub fn stop_all() {
    stop_tasks(|_| true);
    show_cursor(None, "stop", "Durduruldu");
}

pub(super) fn stop_tasks(which: impl Fn(&str) -> bool) {
    if let (Ok(mut map), Ok(mut stopped)) = (seen().lock(), stopped().lock()) {
        let tasks: Vec<String> = map.keys().filter(|task| which(task)).cloned().collect();
        for task in tasks {
            map.remove(&task);
            stopped.insert(task);
        }
    }
}

pub(super) fn refuse_if_stopped(task: &str) -> Result<()> {
    if stopped().lock().is_ok_and(|stopped| stopped.contains(task)) {
        return Err(permission(
            "DESKTOP_STOPPED_BY_PERSON",
            "Bu görevi sen durdurdun; ekrana dokunmuyorum.",
        ));
    }
    Ok(())
}
