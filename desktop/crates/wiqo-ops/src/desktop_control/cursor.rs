//! What the person sees while Wiqo works: its own cursor, moved to each target before the action.

use super::*;

// ---------- what the person sees: Wiqo's own cursor ----------

pub(super) static CURSOR_SINK: OnceLock<CursorSink> = OnceLock::new();
/// The cursor reaches the target before the action lands, so the person sees where it goes.
pub(super) const CURSOR_TRAVEL: Duration = Duration::from_millis(280);

/// The window that draws the cursor. Without one (the command-line agent, tests) nothing is drawn and nothing waits.
pub fn set_cursor_sink(sink: CursorSink) {
    let _ = CURSOR_SINK.set(sink);
}

pub(super) fn show_cursor(at: Option<[f64; 2]>, action: &'static str, label: &str) {
    let Some(sink) = CURSOR_SINK.get() else {
        return;
    };
    sink(CursorEvent {
        x: at.map(|point| point[0]),
        y: at.map(|point| point[1]),
        action,
        label: label.to_string(),
    });
    if at.is_some() {
        std::thread::sleep(CURSOR_TRAVEL);
    }
}

pub(super) fn middle_of(frame: [f64; 4], screen: [f64; 4]) -> [f64; 2] {
    [
        screen[0] + (frame[0] + frame[2] / 2.0) * screen[2],
        screen[1] + (frame[1] + frame[3] / 2.0) * screen[3],
    ]
}

pub(super) fn named_target<'a>(scene: &'a Scene, reference: Option<&String>) -> Option<&'a Target> {
    let reference = reference?;
    scene
        .targets
        .iter()
        .chain(&scene.hidden)
        .find(|target| &target.reference == reference)
}

/// The point on screen a fraction of the display names.
pub(super) fn screen_point(x: Option<f64>, y: Option<f64>, screen: [f64; 4]) -> Option<[f64; 2]> {
    let (x, y) = (x?, y?);
    ((0.0..=1.0).contains(&x) && (0.0..=1.0).contains(&y))
        .then(|| [screen[0] + x * screen[2], screen[1] + y * screen[3]])
}

/// Where the step acts: its control, or the point it names, or (a scroll with neither) the middle of the window.
pub(super) fn aim(step: &Request, scene: &Scene) -> Option<[f64; 2]> {
    let reference = if step.action == "dragDrop" {
        step.source_ref.as_ref()
    } else {
        step.target_ref.as_ref()
    };
    named_target(scene, reference)
        .map(|target| middle_of(target.frame, scene.screen))
        .or_else(|| screen_point(step.normalized_x, step.normalized_y, scene.screen))
        .or_else(|| {
            (step.action == "scroll")
                .then(|| middle_of(scene.window.unwrap_or([0.0, 0.0, 1.0, 1.0]), scene.screen))
        })
}

pub(super) fn cursor_words(step: &Request, scene: &Scene) -> (&'static str, String) {
    let name = named_target(scene, step.target_ref.as_ref())
        .or_else(|| named_target(scene, step.source_ref.as_ref()))
        .map(|target| target.name.as_str())
        .unwrap_or_default();
    let (action, doing) = match step.action.as_str() {
        "click" => ("click", "Tıklıyor"),
        "doubleClick" => ("click", "Çift tıklıyor"),
        "tripleClick" => ("click", "Seçiyor"),
        "rightClick" => ("click", "Sağ tıklıyor"),
        "type" | "setValue" | "fill" => ("type", "Yazıyor"),
        "scroll" => ("move", "Kaydırıyor"),
        "dragDrop" => ("drag", "Sürüklüyor"),
        "hotkey" => ("type", "Kısayol"),
        "menu" => ("type", "Menü"),
        "wait" => ("move", "Bekliyor"),
        _ => ("move", "Gidiyor"),
    };
    let name = if name.is_empty() && step.action == "fill" {
        step.label.as_deref().unwrap_or_default()
    } else {
        name
    };
    let label = if name.is_empty() {
        doing.to_string()
    } else {
        format!("{doing}: {}", name.chars().take(40).collect::<String>())
    };
    (action, label)
}
