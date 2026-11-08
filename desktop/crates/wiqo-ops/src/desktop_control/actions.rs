//! Performing one step on the screen: clicks, keys, menus and typing, through the helper.

use super::*;

/// One step as helper commands. Every ref must be one the model saw in the latest observation; it is turned back into the helper's own ref here.
/// Keys go to the observed app by name, never to whatever window happens to be in front (often Wiqo's own, where the request was typed).
pub(super) fn perform(engine: &dyn Engine, step: &Request, scene: &Scene) -> Result<Answer> {
    let find = |reference: Option<&String>| -> Result<&Target> {
        let reference = reference.ok_or_else(|| {
            tool_failure(
                "DESKTOP_ACTION_INVALID",
                "Bu işlem için bir hedef gerekiyor.",
            )
        })?;
        named_target(scene, Some(reference)).ok_or_else(|| {
            tool_failure(
                "DESKTOP_TARGET_NOT_FOUND",
                format!("{reference} hedefi güncel görünümde yok."),
            )
        })
    };
    // A point on screen, from the model's 0-1 coordinates: the last resort when a control has no ref.
    let point = || screen_point(step.normalized_x, step.normalized_y, scene.screen);
    let mouse = |at: [f64; 2], count: u8, button: &str| -> Vec<String> {
        let mut args = vec!["mouse-click".to_string()];
        args.extend(coordinates("--xy", at));
        args.extend([
            "--count".into(),
            count.to_string(),
            "--button".into(),
            button.into(),
            "--headed".into(),
        ]);
        args
    };
    let click = |command: &str, count: u8, button: &str| -> Result<Vec<String>> {
        if step.target_ref.is_some() {
            let target = find(step.target_ref.as_ref())?;
            refuse_secure(target)?;
            // A word read from the screen has no helper ref: the click lands on its middle.
            if target.helper.is_empty() {
                return Ok(mouse(middle_of(target.frame, scene.screen), count, button));
            }
            return Ok(match count {
                3 => vec![
                    "triple-click".into(),
                    target.helper.clone(),
                    "--headed".into(),
                ],
                2 => vec![
                    "double-click".into(),
                    target.helper.clone(),
                    "--headed".into(),
                ],
                _ => vec![command.into(), target.helper.clone()],
            });
        }
        let at = point().ok_or_else(|| {
            tool_failure(
                "DESKTOP_TARGET_REQUIRED",
                "Bir hedef ref ya da 0-1 arası koordinat gerekiyor.",
            )
        })?;
        Ok(mouse(at, count, button))
    };
    // One end of a drag: a control the helper knows by ref, otherwise a point (a word read from the screen, a canvas, a slider's track).
    let drag_end = |flag: &str,
                    reference: Option<&String>,
                    x: Option<f64>,
                    y: Option<f64>|
     -> Result<Vec<String>> {
        if reference.is_some() {
            let target = find(reference)?;
            refuse_secure(target)?;
            if !target.helper.is_empty() {
                return Ok(vec![flag.to_string(), target.helper.clone()]);
            }
            return Ok(coordinates(
                &format!("{flag}-xy"),
                middle_of(target.frame, scene.screen),
            ));
        }
        let at = screen_point(x, y, scene.screen).ok_or_else(|| {
            tool_failure(
                "DESKTOP_TARGET_REQUIRED",
                "Sürüklemenin iki ucu için ref ya da 0-1 arası koordinat gerekiyor.",
            )
        })?;
        Ok(coordinates(&format!("{flag}-xy"), at))
    };
    let args: Vec<String> = match step.action.as_str() {
        "click" => click("click", 1, "left")?,
        "doubleClick" => click("click", 2, "left")?,
        "tripleClick" => click("click", 3, "left")?,
        "rightClick" => click("right-click", 1, "right")?,
        "move" => {
            let at = step
                .target_ref
                .is_some()
                .then(|| find(step.target_ref.as_ref()))
                .transpose()?
                .map(|target| middle_of(target.frame, scene.screen))
                .or_else(point)
                .ok_or_else(|| {
                    tool_failure(
                        "DESKTOP_TARGET_REQUIRED",
                        "İmleç için bir hedef ya da 0-1 arası koordinat gerekiyor.",
                    )
                })?;
            let mut args = vec!["mouse-move".to_string()];
            args.extend(coordinates("--xy", at));
            args.push("--headed".into());
            args
        }
        "type" => return type_text(engine, step, scene),
        "setValue" => {
            let target = find(step.target_ref.as_ref())?;
            refuse_secure(target)?;
            let value = step
                .text
                .clone()
                .filter(|value| !value.is_empty())
                .ok_or_else(|| tool_failure("DESKTOP_ACTION_INVALID", "Yazılacak değer yok."))?;
            vec!["set-value".into(), target.helper.clone(), value]
        }
        "scroll" => {
            let direction = step.direction.clone().unwrap_or_else(|| "down".into());
            if !["up", "down", "left", "right"].contains(&direction.as_str()) {
                return Err(tool_failure(
                    "DESKTOP_ACTION_INVALID",
                    "Kaydırma yönü geçersiz.",
                ));
            }
            let amount = step.amount.unwrap_or(3).clamp(1, 20);
            let target = step
                .target_ref
                .is_some()
                .then(|| find(step.target_ref.as_ref()))
                .transpose()?;
            match target.filter(|target| !target.helper.is_empty()) {
                Some(target) => vec![
                    "scroll".into(),
                    target.helper.clone(),
                    "--direction".into(),
                    direction,
                    "--amount".into(),
                    amount.to_string(),
                ],
                // No control to name (a web page, a canvas, a word read from the screen): the wheel, where the step aims.
                None => {
                    let at = aim(step, scene).unwrap_or_else(|| {
                        middle_of(scene.window.unwrap_or([0.0, 0.0, 1.0, 1.0]), scene.screen)
                    });
                    let lines = amount as i64;
                    let (dx, dy) = match direction.as_str() {
                        "up" => (0, lines),
                        "down" => (0, -lines),
                        "right" => (lines, 0),
                        _ => (-lines, 0),
                    };
                    vec![
                        "mouse-wheel".into(),
                        format!("--x={}", at[0].round()),
                        format!("--y={}", at[1].round()),
                        format!("--dx={dx}"),
                        format!("--dy={dy}"),
                        "--headed".into(),
                    ]
                }
            }
        }
        "hotkey" => {
            let keys = step
                .keys
                .clone()
                .filter(|keys| !keys.is_empty())
                .ok_or_else(|| tool_failure("DESKTOP_ACTION_INVALID", "Kısayol tuşları yok."))?;
            let combo = keys
                .iter()
                .map(|key| helper_key(key))
                .collect::<Vec<_>>()
                .join("+");
            return press(engine, &combo, &scene.application);
        }
        "dragDrop" => {
            let mut args = vec!["drag".to_string()];
            args.extend(drag_end(
                "--from",
                step.source_ref.as_ref(),
                step.normalized_x,
                step.normalized_y,
            )?);
            args.extend(drag_end(
                "--to",
                step.destination_ref.as_ref(),
                step.to_normalized_x,
                step.to_normalized_y,
            )?);
            args.extend(["--duration".into(), "350".into(), "--headed".into()]);
            args
        }
        "menu" => return run_menu(engine, step, scene),
        // Until the words appear, or for a set time.
        "wait" => match step.text.as_deref().filter(|text| !text.is_empty()) {
            Some(text) => vec![
                "wait".into(),
                "--text".into(),
                text.into(),
                "--app".into(),
                scene.application.clone(),
                "--timeout".into(),
                step.amount.unwrap_or(5000).clamp(50, 15000).to_string(),
            ],
            None => vec![
                "wait".into(),
                step.amount.unwrap_or(500).clamp(50, 5000).to_string(),
            ],
        },
        _ => {
            return Err(tool_failure(
                "DESKTOP_ACTION_UNSUPPORTED",
                "Bu masaüstü işlemi desteklenmiyor.",
            ))
        }
    };
    call(engine, &args)
}

/// A command from the app's menu bar: how an expert reaches what has no button on screen. It works in any app and needs no look at the window.
pub(super) fn run_menu(engine: &dyn Engine, step: &Request, scene: &Scene) -> Result<Answer> {
    let item = menu_item(step)
        .ok_or_else(|| tool_failure("DESKTOP_ACTION_INVALID", "Menü komutunun adı yok."))?;
    engine.bring_to_front(&scene.application);
    let find = |exact: bool| {
        let mut args = vec![
            "find".to_string(),
            "--app".into(),
            scene.application.clone(),
            "--surface".into(),
            "menubar".into(),
            "--role".into(),
            "menuitem".into(),
            "--name".into(),
            item.to_string(),
            "--first".into(),
        ];
        if exact {
            args.push("--exact".into());
        }
        engine.call(&args).ok().filter(|answer| answer.ok)
    };
    let found = find(true).or_else(|| find(false)).ok_or_else(|| {
        tool_failure(
            "DESKTOP_TARGET_NOT_FOUND",
            format!(
                "{} menüsünde “{item}” yok; menü adları uygulamanın dilinde olur.",
                scene.application
            ),
        )
    })?;
    let matched = &found.data["match"];
    let reference = matched["ref_id"]
        .as_str()
        .ok_or_else(|| tool_failure("DESKTOP_TARGET_NOT_FOUND", format!("“{item}” bulunamadı.")))?;
    let disabled = matched["states"]
        .as_array()
        .is_some_and(|states| states.iter().any(|state| state == "disabled"));
    if disabled {
        return Err(tool_failure(
            "DESKTOP_TARGET_NOT_FOUND",
            format!("“{item}” şu an kullanılamıyor (gri)."),
        ));
    }
    call(engine, &["click".into(), reference.to_string()])
}

/// A point as the helper takes it; a negative one (a display left of the primary) must be written with `=`.
pub(super) fn coordinates(flag: &str, at: [f64; 2]) -> Vec<String> {
    let value = format!("{},{}", at[0].round(), at[1].round());
    if at[0] < 0.0 || at[1] < 0.0 {
        vec![format!("{flag}={value}")]
    } else {
        vec![flag.to_string(), value]
    }
}

/// Keys as a person presses them: the app is brought to the front first (it may have no window yet, like TextEdit with no document), and the keys
/// go only once the system says it is in front, so they never land in another app.
pub(super) fn press(engine: &dyn Engine, combo: &str, application: &str) -> Result<Answer> {
    if !application.is_empty() && !engine.bring_to_front(application) {
        return Err(tool_failure(
            "DESKTOP_APP_UNAVAILABLE",
            format!("{application} öne getirilemedi; tuşlar başka bir uygulamaya gitmesin diye gönderilmedi."),
        ));
    }
    call(engine, &["press".into(), combo.to_string()])
}

pub(super) fn call(engine: &dyn Engine, args: &[String]) -> Result<Answer> {
    let answer = engine.call(args).map_err(unavailable)?;
    if answer.ok {
        Ok(answer)
    } else {
        Err(contract_error(&answer))
    }
}

/// Text goes in the way a person would put it there. Into the named field, or the focused one when no field is named, through the accessibility
/// call that the app reads back; a field that refuses it (many web and custom editors) gets the text pasted: put on the clipboard, command/control+V
/// to the app, and the person's own clipboard put back.
pub(super) fn type_text(engine: &dyn Engine, step: &Request, scene: &Scene) -> Result<Answer> {
    let text = step
        .text
        .clone()
        .filter(|text| !text.is_empty())
        .ok_or_else(|| tool_failure("DESKTOP_ACTION_INVALID", "Yazılacak metin yok."))?;
    let target = match step.target_ref.as_ref() {
        Some(reference) => Some(
            scene
                .targets
                .iter()
                .find(|target| &target.reference == reference)
                .ok_or_else(|| {
                    tool_failure(
                        "DESKTOP_TARGET_NOT_FOUND",
                        format!("{reference} hedefi güncel görünümde yok."),
                    )
                })?,
        ),
        None => scene.targets.iter().find(|target| target.focused),
    };
    if target.is_some_and(|target| target.secure) {
        return Err(tool_failure(
            "DESKTOP_SECURE_FIELD_BLOCKED",
            "Parola alanına yazılmaz; kullanıcı kendisi girmeli.",
        ));
    }
    // A field read from the screen's text: click into it the way a person would, then paste.
    if let Some(target) = target.filter(|target| target.helper.is_empty()) {
        let mut args = vec!["mouse-click".to_string()];
        args.extend(coordinates("--xy", middle_of(target.frame, scene.screen)));
        args.push("--headed".into());
        call(engine, &args)?;
        return paste(engine, &text, &scene.application);
    }
    if let Some(target) = target {
        match call(
            engine,
            &["type".into(), target.helper.clone(), text.clone()],
        ) {
            Ok(answer) => return Ok(answer),
            // Only a refusal that sent nothing falls back to pasting; anything that may have typed is reported as it is.
            Err(error)
                if error.code != "DESKTOP_ACTION_UNSUPPORTED"
                    && error.code != "DESKTOP_UNAVAILABLE" =>
            {
                return Err(error)
            }
            Err(_) => {
                let _ = call(engine, &["focus".into(), target.helper.clone()]);
            }
        }
    }
    paste(engine, &text, &scene.application)
}

pub(super) fn paste(engine: &dyn Engine, text: &str, application: &str) -> Result<Answer> {
    let mut clipboard = arboard::Clipboard::new()
        .map_err(|_| tool_failure("DESKTOP_INPUT_UNAVAILABLE", "Pano kullanılamıyor."))?;
    let saved = clipboard.get_text().ok();
    // Text or a picture goes back afterwards; other kinds (files, rich text) cannot be read through this and are lost to the paste.
    let saved_picture = saved
        .is_none()
        .then(|| clipboard.get_image().ok())
        .flatten();
    clipboard
        .set_text(text.to_string())
        .map_err(|_| tool_failure("DESKTOP_INPUT_UNAVAILABLE", "Pano kullanılamıyor."))?;
    let modifier = if cfg!(target_os = "macos") {
        "cmd"
    } else {
        "ctrl"
    };
    let pasted = press(engine, &format!("{modifier}+v"), application);
    // The app reads the clipboard when it handles the keys; give it a moment before the person's own text goes back.
    std::thread::sleep(Duration::from_millis(250));
    if let Some(saved) = saved {
        let _ = clipboard.set_text(saved);
    } else if let Some(picture) = saved_picture {
        let _ = clipboard.set_image(picture);
    }
    pasted
}

pub(super) fn refuse_secure(target: &Target) -> Result<()> {
    if target.secure {
        return Err(permission(
            "DESKTOP_SECURE_FIELD_BLOCKED",
            "Parola alanıyla etkileşilmez; kullanıcı kendisi yapmalı.",
        ));
    }
    Ok(())
}

pub(super) fn content(
    request: &Request,
    observation: Option<&Observation>,
    access: bool,
    changed: bool,
    evidence: Vec<Value>,
) -> Value {
    let targets: Vec<Value> = observation
        .map(|observation| observation.targets.iter().map(Target::json).collect())
        .unwrap_or_default();
    data(
        "computerDesktop",
        json!({
            "taskId": request.task_id,
            "action": request.action,
            "snapshotId": observation.map(|observation| observation.snapshot_id.clone()),
            "applicationName": observation.map(|observation| observation.application.clone()),
            "windowTitle": observation.map(|observation| observation.window_title.clone()),
            "targets": targets,
            "addedTargets": [],
            "changedTargets": [],
            "removedTargetRefs": [],
            "accessGranted": access,
            "changed": changed,
            "verified": true,
            "evidence": evidence,
        }),
    )
}
