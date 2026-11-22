//! The observation: what is on screen, as refs the model can name, and the text read off it.

use super::*;

// ---------- the observation: refs the model can name ----------

pub(super) fn collect(node: &Value, screen: [f64; 4], out: &mut Vec<Target>) {
    if let Some(reference) = node["ref_id"].as_str() {
        let states: Vec<&str> = node["states"]
            .as_array()
            .map(|list| list.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        let bounds = &node["bounds"];
        let (x, y, w, h) = (
            bounds["x"].as_f64().unwrap_or(0.0),
            bounds["y"].as_f64().unwrap_or(0.0),
            bounds["width"].as_f64().unwrap_or(0.0),
            bounds["height"].as_f64().unwrap_or(0.0),
        );
        let actions: Vec<String> = node["available_actions"]
            .as_array()
            .map(|list| {
                list.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        let name = node["name"]
            .as_str()
            .or_else(|| node["value"].as_str())
            .unwrap_or_default();
        let secure = states.contains(&"secure");
        // Anonymous, action-less, invisible rows are scaffolding, not targets.
        if !actions.is_empty()
            && w > 1.0
            && h > 1.0
            && !states.contains(&"offscreen")
            && out.len() < READ_MOST
        {
            let round = |value: f64| (value * 100_000.0).round() / 100_000.0;
            out.push(Target {
                reference: contract_ref(reference),
                helper: reference.to_string(),
                role: node["role"].as_str().unwrap_or("unknown").to_string(),
                // A secure field's content is never read back to the model.
                name: if secure {
                    String::new()
                } else {
                    name.chars().take(100).collect()
                },
                frame: [
                    round((x - screen[0]) / screen[2]),
                    round((y - screen[1]) / screen[3]),
                    round(w / screen[2]),
                    round(h / screen[3]),
                ],
                actions,
                enabled: !states.contains(&"disabled"),
                secure,
                focused: states.contains(&"focused"),
                value: if secure {
                    String::new()
                } else {
                    node["value"]
                        .as_str()
                        .unwrap_or_default()
                        .chars()
                        .take(100)
                        .collect()
                },
                states: states
                    .iter()
                    .filter(|state| **state != "focused")
                    .copied()
                    .collect::<Vec<_>>()
                    .join(","),
                identity: node["name"].as_str().unwrap_or_default().to_string(),
            });
        }
    }
    for child in node["children"].as_array().into_iter().flatten() {
        collect(child, screen, out);
    }
}

/// `d-` and the first 8 hex digits of the helper ref's SHA-256: the shape the server accepts, unique within one observation.
pub(super) fn contract_ref(helper: &str) -> String {
    use sha2::Digest;
    format!(
        "d-{}",
        &hex::encode(sha2::Sha256::digest(helper.as_bytes()))[..8]
    )
}

/// Refs that outlive one look: a control is its role, its name and which of that kind it is. The helper's own ref changes with every snapshot; this one lets the model
/// keep what it has read, send several steps at once, and be told only what changed.
pub(super) fn assign_references(targets: &mut [Target]) {
    let mut seen: HashMap<String, usize> = HashMap::new();
    for target in targets.iter_mut() {
        let name = if target.identity.is_empty() && target.helper.is_empty() {
            &target.name
        } else {
            &target.identity
        };
        let key = format!("{}|{}", target.role, name);
        let count = seen.entry(key.clone()).or_insert(0);
        target.reference = contract_ref(&format!("{key}|{count}"));
        *count += 1;
    }
}

/// The contract's key names, as the helper spells them.
pub(super) fn helper_key(key: &str) -> String {
    match key.trim().to_lowercase().as_str() {
        "arrowup" => "up".into(),
        "arrowdown" => "down".into(),
        "arrowleft" => "left".into(),
        "arrowright" => "right".into(),
        other => other.to_string(),
    }
}

/// The displays as the helper lists them, in points, with the primary one's place. The list's order is what its screenshot's `--screen` counts.
pub(super) fn displays(engine: &dyn Engine) -> (Vec<[f64; 4]>, usize) {
    let listed = engine
        .call(&["list-displays".into()])
        .ok()
        .and_then(|answer| answer.ok.then_some(answer.data));
    let entries = listed
        .as_ref()
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let rects: Vec<[f64; 4]> = entries
        .iter()
        .filter_map(|display| {
            let bounds = &display["bounds"];
            Some([
                bounds["x"].as_f64().unwrap_or(0.0),
                bounds["y"].as_f64().unwrap_or(0.0),
                bounds["width"].as_f64()?.max(1.0),
                bounds["height"].as_f64()?.max(1.0),
            ])
        })
        .collect();
    if rects.is_empty() {
        return (vec![[0.0, 0.0, 1920.0, 1080.0]], 0);
    }
    let primary = entries
        .iter()
        .position(|display| display["is_primary"].as_bool() == Some(true))
        .filter(|place| *place < rects.len())
        .unwrap_or(0);
    (rects, primary)
}

/// The display a window sits on: the one holding its middle.
pub(super) fn display_of(window: &Value, rects: &[[f64; 4]], primary: usize) -> usize {
    let (Some(x), Some(y), Some(w), Some(h)) = (
        window["x"].as_f64(),
        window["y"].as_f64(),
        window["width"].as_f64(),
        window["height"].as_f64(),
    ) else {
        return primary;
    };
    let (middle_x, middle_y) = (x + w / 2.0, y + h / 2.0);
    rects
        .iter()
        .position(|r| {
            middle_x >= r[0] && middle_x < r[0] + r[2] && middle_y >= r[1] && middle_y < r[1] + r[3]
        })
        .unwrap_or(primary)
}

/// What an open menu, sheet, popover or alert of the app shows. A window snapshot does not include them, and they are what the person acts on next.
pub(super) fn open_surfaces(
    engine: &dyn Engine,
    application: &str,
    screen: [f64; 4],
) -> Vec<Target> {
    let Some(listed) = engine
        .call(&["list-surfaces".into(), "--app".into(), application.into()])
        .ok()
        .and_then(|answer| answer.ok.then_some(answer.data))
    else {
        return Vec::new();
    };
    let mut targets = Vec::new();
    for surface in listed["surfaces"].as_array().into_iter().flatten() {
        let kind = surface
            .as_str()
            .or_else(|| surface["kind"].as_str())
            .or_else(|| surface["type"].as_str())
            .or_else(|| surface["surface"].as_str())
            .unwrap_or_default();
        if !["menu", "sheet", "popover", "alert"].contains(&kind) {
            continue;
        }
        let answer = engine.call(&[
            "snapshot".into(),
            "-i".into(),
            "--compact".into(),
            "--include-bounds".into(),
            "--app".into(),
            application.into(),
            "--surface".into(),
            kind.into(),
        ]);
        if let Some(answer) = answer.ok().filter(|answer| answer.ok) {
            collect(&answer.data["tree"], screen, &mut targets);
        }
    }
    targets
}

/// The words on screen, added when the controls alone say too little of what the window shows (a web page, an Electron or custom-drawn app). Only words inside
/// the observed window, and none a control already names. Local and free; without Screen Recording permission there are simply no words.
pub(super) fn add_screen_text(engine: &dyn Engine, observation: &mut Observation) {
    if observation.targets.len() >= READ_TEXT_BELOW {
        return;
    }
    let words = read_screen(engine, observation.display);
    let inside = |target: &Target| match observation.window {
        Some([x, y, w, h]) => {
            let (middle_x, middle_y) = (
                target.frame[0] + target.frame[2] / 2.0,
                target.frame[1] + target.frame[3] / 2.0,
            );
            middle_x >= x && middle_x <= x + w && middle_y >= y && middle_y <= y + h
        }
        None => true,
    };
    let fresh: Vec<Target> = words
        .into_iter()
        .filter(|word| inside(word))
        .filter(|word| {
            !observation
                .targets
                .iter()
                .any(|target| target.name == word.name)
        })
        .collect();
    observation.targets.extend(fresh);
}

/// How long an app that was just brought to the front gets to show its window.
#[cfg(not(test))]
const BROUGHT_FORWARD: Duration = Duration::from_millis(700);
#[cfg(test)]
const BROUGHT_FORWARD: Duration = Duration::from_millis(400);

/// The controls of an app as they are now, in whatever opens over its window first.
pub(super) fn observe_controls(
    engine: &dyn Engine,
    application: Option<&str>,
) -> Result<Observation> {
    let known = displays(engine);
    observe_on(engine, application, &known)
}

pub(super) fn observe_on(
    engine: &dyn Engine,
    application: Option<&str>,
    (rects, primary): &(Vec<[f64; 4]>, usize),
) -> Result<Observation> {
    let (rects, primary) = (rects.as_slice(), *primary);
    // The app is always named: the window in front is often Wiqo's own, where the request was typed, and a bare snapshot would read that.
    let named = application
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .or_else(crate::apps::frontmost);
    let mut args: Vec<String> = vec![
        "snapshot".into(),
        "-i".into(),
        "--compact".into(),
        "--include-bounds".into(),
    ];
    if let Some(name) = &named {
        args.extend(["--app".into(), name.clone()]);
    }
    let mut answer = engine.call(&args).map_err(unavailable)?;
    // An app whose windows are on another desktop (the person is in a full-screen app) shows none until it is in front: brought forward once and read again.
    if let (false, "WINDOW_NOT_FOUND", Some(name)) = (answer.ok, answer.code.as_str(), &named) {
        if !is_wiqo(name) && engine.bring_to_front(name) {
            std::thread::sleep(BROUGHT_FORWARD);
            answer = engine.call(&args).map_err(unavailable)?;
        }
    }
    // Several windows and none of them in front: the helper will not choose. The one the app used last (the first it lists, or the focused one) is read.
    if let (false, "AMBIGUOUS_TARGET", Some(name)) = (answer.ok, answer.code.as_str(), &named) {
        let listed = engine
            .call(&["list-windows".into(), "--app".into(), name.clone()])
            .ok()
            .filter(|listed| listed.ok)
            .and_then(|listed| listed.data.as_array().cloned())
            .unwrap_or_default();
        let window = listed
            .iter()
            .find(|window| window["is_focused"] == true)
            .or_else(|| listed.first())
            .and_then(|window| window["id"].as_str());
        if let Some(window) = window {
            let mut one = args.clone();
            one.extend(["--window-id".into(), window.to_string()]);
            answer = engine.call(&one).map_err(unavailable)?;
        }
    }
    if !answer.ok {
        // An app that is running with no window (TextEdit after its last document closed) is a real place to work: its shortcuts (command+n) still
        // reach it. It is reported as an app with nothing on screen yet, not as an app that is missing.
        if let (Some(name), "WINDOW_NOT_FOUND") = (&named, answer.code.as_str()) {
            return Ok(Observation {
                snapshot_id: format!("none-{}", uuid::Uuid::new_v4()),
                application: name.clone(),
                window_title: String::new(),
                targets: Vec::new(),
                screen: rects[primary],
                display: primary,
                window: None,
                hidden: Vec::new(),
            });
        }
        // A window the accessibility tree cannot see (a game, a canvas, a remote desktop) is read from its pixels instead.
        if answer.code == "ACTION_NOT_SUPPORTED" {
            let targets = read_screen(engine, primary);
            if !targets.is_empty() {
                return Ok(Observation {
                    snapshot_id: format!("ocr-{}", uuid::Uuid::new_v4()),
                    application: named.unwrap_or_default(),
                    window_title: String::new(),
                    targets,
                    screen: rects[primary],
                    display: primary,
                    window: None,
                    hidden: Vec::new(),
                });
            }
        }
        return Err(contract_error(&answer));
    }
    let bounds = &answer.data["tree"]["bounds"];
    let display = display_of(bounds, rects, primary);
    let screen = rects[display];
    let window = bounds["width"].as_f64().map(|width| {
        [
            (bounds["x"].as_f64().unwrap_or(screen[0]) - screen[0]) / screen[2],
            (bounds["y"].as_f64().unwrap_or(screen[1]) - screen[1]) / screen[3],
            width / screen[2],
            bounds["height"].as_f64().unwrap_or(screen[3]) / screen[3],
        ]
    });
    let application = answer.data["app"].as_str().unwrap_or_default().to_string();
    let mut collected = Vec::new();
    collect(&answer.data["tree"], screen, &mut collected);
    let over_window = open_surfaces(engine, named.as_deref().unwrap_or(&application), screen);
    let mut targets = ranked(over_window);
    targets.extend(ranked(collected));
    // Every control gets its ref before the list is cut, so a control beyond the cut keeps the same ref when a narrowed look brings it up.
    assign_references(&mut targets);
    let hidden = targets.split_off(targets.len().min(MAX_TARGETS));
    Ok(Observation {
        snapshot_id: answer.data["snapshot_id"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
        application,
        window_title: answer.data["window"]["title"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
        targets,
        screen,
        display,
        window,
        hidden,
    })
}

/// The window is mostly something its controls do not name (a canvas, a game, a page drawn by the app itself).
pub(super) fn says_little(observation: &Observation) -> bool {
    observation
        .targets
        .iter()
        .filter(|target| !target.helper.is_empty())
        .count()
        < READ_TEXT_BELOW
}

/// Letters and digits only, folded: how a name on the screen is compared with the words the model gave.
pub(super) fn plain(text: &str) -> String {
    fold(text)
        .split(|letter: char| !letter.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// The look, cut down to the controls whose name or content holds the words: how the model reaches a control a long window pushed off its list.
pub(super) fn narrow(observation: &mut Observation, words: &str) {
    let holds = |target: &Target| {
        plain(&target.name).contains(words) || plain(&target.value).contains(words)
    };
    let mut kept: Vec<Target> = Vec::new();
    let mut rest: Vec<Target> = Vec::new();
    for target in observation
        .targets
        .drain(..)
        .chain(observation.hidden.drain(..))
    {
        if holds(&target) && kept.len() < MAX_TARGETS {
            kept.push(target);
        } else {
            rest.push(target);
        }
    }
    observation.targets = kept;
    observation.hidden = rest;
}

/// The display as a JPEG small enough to travel with the result: what the server's eyes read when the controls say too little.
pub(super) fn picture_of(engine: &dyn Engine, display: usize) -> Option<Value> {
    use base64::Engine as _;
    use image::{codecs::jpeg::JpegEncoder, imageops::FilterType, DynamicImage};
    const MOST_BYTES: usize = 800 * 1024;
    let path = std::env::temp_dir().join(format!("wiqo-look-{}.png", uuid::Uuid::new_v4()));
    let taken = engine
        .call(&[
            "screenshot".into(),
            "--screen".into(),
            display.to_string(),
            path.to_string_lossy().to_string(),
        ])
        .is_ok_and(|answer| answer.ok);
    let picture = taken.then(|| image::open(&path).ok()).flatten();
    let _ = std::fs::remove_file(&path);
    let picture = picture?;
    for (width, quality) in [(1600u32, 68u8), (1600, 50), (1280, 50), (1024, 45)] {
        let sized = if picture.width() > width {
            picture.resize(width, u32::MAX, FilterType::Triangle)
        } else {
            picture.clone()
        };
        let rgb = DynamicImage::ImageRgb8(sized.to_rgb8());
        let mut bytes = Vec::new();
        if JpegEncoder::new_with_quality(&mut bytes, quality)
            .encode_image(&rgb)
            .is_ok()
            && bytes.len() <= MOST_BYTES
        {
            return Some(json!({
                "mimeType": "image/jpeg",
                "dataBase64": base64::engine::general_purpose::STANDARD.encode(&bytes),
                "width": rgb.width(),
                "height": rgb.height(),
            }));
        }
    }
    None
}

pub(super) fn observe(engine: &dyn Engine, application: Option<&str>) -> Result<Observation> {
    let mut observation = observe_controls(engine, application)?;
    add_screen_text(engine, &mut observation);
    assign_references(&mut observation.targets);
    Ok(observation)
}

/// What the model reads first: the controls a person acts with (buttons, fields, menus), then the rest. The result travels in a bounded message, and
/// a file dialog's sidebar listed first used to push its "Yeni Belge" button out of it. A cell that only repeats the row beside it, and nameless
/// scaffolding (scroll areas, bars, handles), are left out.
pub(super) fn ranked(targets: Vec<Target>) -> Vec<Target> {
    let rank = |role: &str| match role {
        "button" | "menubutton" | "popupbutton" | "splitbutton" => 0,
        "textfield" | "textarea" | "searchfield" | "combobox" => 1,
        "checkbox" | "radiobutton" | "switch" | "tab" | "menuitem" | "link" | "slider"
        | "stepper" => 2,
        "text" => 3,
        _ => 4,
    };
    let mut kept: Vec<Target> = Vec::new();
    for target in targets {
        let scaffolding = target.name.trim().is_empty()
            && matches!(
                target.role.as_str(),
                "scrollarea"
                    | "scrollbar"
                    | "handle"
                    | "group"
                    | "splitter"
                    | "ruler"
                    | "browser"
                    | "list"
                    | "outline"
            );
        let repeat = target.role == "cell"
            && kept
                .iter()
                .any(|other| other.name == target.name && other.role != "cell");
        if !scaffolding && !repeat {
            kept.push(target);
        }
    }
    kept.sort_by_key(|target| rank(&target.role));
    kept
}

/// Below this many controls the tree is taken to be blind to the window, and the screen's text is read as well.
/// Fewer controls than this and the window is mostly something the tree does not name, so its words are read too.
pub(super) const READ_TEXT_BELOW: usize = 25;
pub(super) const READ_TEXT_FOR: Duration = Duration::from_secs(3);
pub(super) const MAX_TEXT_TARGETS: usize = 80;

/// The words on the primary screen, each a target the model can click by ref (no helper ref: a click lands on the middle of the word).
pub(super) fn read_screen(engine: &dyn Engine, display: usize) -> Vec<Target> {
    let path = std::env::temp_dir().join(format!("wiqo-ocr-{}.png", uuid::Uuid::new_v4()));
    let taken = engine
        .call(&[
            "screenshot".into(),
            "--screen".into(),
            display.to_string(),
            path.to_string_lossy().to_string(),
        ])
        .is_ok_and(|answer| answer.ok);
    // Words are a help, never worth holding an action for: a late reading is dropped and the turn goes on with the controls alone.
    let words = if taken {
        let (send, receive) = std::sync::mpsc::channel();
        let picture = path.clone();
        std::thread::spawn(move || {
            let _ = send.send(crate::ocr::read(&picture));
            let _ = std::fs::remove_file(&picture);
        });
        receive.recv_timeout(READ_TEXT_FOR).unwrap_or_default()
    } else {
        Vec::new()
    };
    words
        .into_iter()
        .take(MAX_TEXT_TARGETS)
        .map(|word| {
            let round = |value: f64| (value * 100_000.0).round() / 100_000.0;
            Target {
                reference: contract_ref(&format!("ocr:{}:{:?}", word.text, word.frame)),
                helper: String::new(),
                role: "text".into(),
                name: word.text,
                frame: word.frame.map(round),
                actions: vec!["Click".into()],
                enabled: true,
                secure: false,
                focused: false,
                value: String::new(),
                states: String::new(),
                identity: String::new(),
            }
        })
        .collect()
}

pub(super) fn unavailable(reason: String) -> wiqo_core::wire::WireError {
    if reason == "helper missing" {
        tool_failure(
            "DESKTOP_ADAPTER_UNAVAILABLE",
            "Masaüstü kontrol bileşeni bu Wiqo sürümünde yok.",
        )
    } else if reason == "timeout" {
        wiqo_core::wire::WireError::new(
            "COMMAND_TIMEOUT",
            "Masaüstü işlemi zamanında bitmedi; sonucu doğrulanamadı.",
            "timeout",
        )
    } else {
        tool_failure(
            "DESKTOP_UNAVAILABLE",
            "Masaüstü kontrol bileşeni çalıştırılamadı.",
        )
    }
}
