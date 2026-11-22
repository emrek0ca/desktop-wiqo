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
                // A context menu asked for through the app's own call often never answers (the app is busy showing the menu) and the result is then
                // unknown: the secondary button is pressed on the control instead, as a hand would.
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
        "rightClick" => {
            let args = click("right-click", 1, "right")?;
            // Asked through the app's own call, a context menu often opens while the call itself never answers (the app is busy showing the menu):
            // a menu that is open is the answer. Only when none opened is the secondary button pressed on the control, as a hand would.
            if args[0] == "right-click" {
                return match call(engine, &args) {
                    Err(error)
                        if matches!(error.code.as_str(), "COMMAND_TIMEOUT" | "OUTCOME_UNKNOWN") =>
                    {
                        let menu_open = engine
                            .call(&[
                                "wait".into(),
                                "--menu".into(),
                                "--app".into(),
                                scene.application.clone(),
                                "--timeout".into(),
                                "900".into(),
                            ])
                            .is_ok_and(|answer| answer.ok);
                        if menu_open {
                            Ok(Answer {
                                ok: true,
                                data: json!({ "readBack": true }),
                                code: String::new(),
                                message: String::new(),
                                delivery: Some("delivered_verified".into()),
                            })
                        } else {
                            forward(engine, scene);
                            let mut physical = args.clone();
                            physical.push("--headed".into());
                            call(engine, &physical)
                        }
                    }
                    other => other,
                };
            }
            args
        }
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
        "fill" => return fill_field(engine, step, scene),
        // With words it scrolls until they are on the screen.
        "scroll"
            if step
                .text
                .as_deref()
                .is_some_and(|text| !text.trim().is_empty()) =>
        {
            return scroll_until(engine, step, scene)
        }
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
            // Some apps take what is dragged only after the pointer has rested on it, or over where it drops: the move's length and the rest are the step's to say.
            let duration = step.duration_ms.unwrap_or(350).clamp(100, 3_000);
            args.extend(["--duration".into(), duration.to_string()]);
            let rest = step.hold_ms.map(|hold| hold.clamp(0, 3_000));
            if let Some(rest) = rest {
                args.extend(["--drop-delay".into(), rest.to_string()]);
            }
            args.extend([
                "--timeout-ms".into(),
                (5_000 + duration + rest.unwrap_or(500)).to_string(),
                "--headed".into(),
            ]);
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
    // The pointer acts on whatever is in front: an app that is behind another one, or on another desktop, is brought forward before a press of the
    // mouse, a drag or a turn of the wheel.
    if args.iter().any(|arg| arg == "--headed") {
        forward(engine, scene);
    }
    call(engine, &args)
}

/// Brings the app the step works on to the front when something else is there, and gives its window a moment to take the pointer: a click on a
/// window that is behind only brings it forward, and what was meant for its control is lost.
pub(super) fn forward(engine: &dyn Engine, scene: &Scene) {
    if !scene.application.is_empty()
        && engine
            .frontmost()
            .is_some_and(|front| !front.eq_ignore_ascii_case(&scene.application))
        && engine.bring_to_front(&scene.application)
    {
        std::thread::sleep(Duration::from_millis(450));
    }
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
        Some(reference) => Some(named_target(scene, Some(reference)).ok_or_else(|| {
            tool_failure(
                "DESKTOP_TARGET_NOT_FOUND",
                format!("{reference} hedefi güncel görünümde yok."),
            )
        })?),
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
        forward(engine, scene);
        call(engine, &args)?;
        return paste(engine, &text, &scene.application);
    }
    if let Some(target) = target {
        match call(
            engine,
            &["type".into(), target.helper.clone(), text.clone()],
        ) {
            Ok(answer) => return Ok(answer),
            // The app took the call and the helper could not see the text arrive (a web page's field, a custom editor). The field itself says which:
            // it holds the text (done), it holds what it held before (nothing landed: clicked into and pasted, as a hand would), or something else
            // (unknown, and said so).
            Err(error) if error.code == "OUTCOME_UNKNOWN" => {
                let holds = call(
                    engine,
                    &[
                        "get".into(),
                        target.helper.clone(),
                        "--property".into(),
                        "value".into(),
                    ],
                )
                .ok()
                .and_then(|read| read.data["value"].as_str().map(str::to_string));
                match holds {
                    Some(value) if value.contains(text.trim()) => {
                        return Ok(Answer {
                            ok: true,
                            data: json!({ "readBack": true }),
                            code: String::new(),
                            message: String::new(),
                            delivery: Some("delivered_verified".into()),
                        })
                    }
                    Some(value) if value == target.value => {
                        let mut args = vec!["mouse-click".to_string()];
                        args.extend(coordinates("--xy", middle_of(target.frame, scene.screen)));
                        args.push("--headed".into());
                        forward(engine, scene);
                        call(engine, &args)?;
                    }
                    _ => return Err(error),
                }
            }
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

/// The field a form's entry names: by its ref, or by what it is called. A name that fits two fields is not guessed at.
pub(super) fn field_for<'a>(step: &Request, scene: &'a Scene) -> Result<&'a Target> {
    if step.target_ref.is_some() {
        return named_target(scene, step.target_ref.as_ref()).ok_or_else(|| {
            tool_failure(
                "DESKTOP_TARGET_NOT_FOUND",
                format!(
                    "{} hedefi güncel görünümde yok.",
                    step.target_ref.as_deref().unwrap_or_default()
                ),
            )
        });
    }
    let label = step.label.as_deref().map(str::trim).unwrap_or_default();
    let wanted = plain(label);
    if wanted.is_empty() {
        return Err(tool_failure(
            "DESKTOP_TARGET_REQUIRED",
            "Form alanı için targetRef ya da label gerekiyor.",
        ));
    }
    let takes_input = |target: &&Target| {
        target.enabled
            && matches!(
                target.role.as_str(),
                "textfield"
                    | "textarea"
                    | "searchfield"
                    | "combobox"
                    | "checkbox"
                    | "switch"
                    | "radiobutton"
                    | "popupbutton"
                    | "slider"
                    | "stepper"
                    | "datefield"
                    | "timefield"
                    | "incrementor"
            )
    };
    let fields: Vec<&Target> = scene
        .targets
        .iter()
        .chain(&scene.hidden)
        .filter(takes_input)
        .collect();
    let one = |found: Vec<&'a Target>| -> Option<Result<&'a Target>> {
        match found.len() {
            0 => None,
            1 => Some(Ok(found[0])),
            _ => Some(Err(tool_failure(
                "DESKTOP_TARGET_NOT_FOUND",
                format!("“{label}” adına uyan birden fazla alan var; targetRef ile söyle."),
            ))),
        }
    };
    let called = |target: &Target| plain(&target.name);
    if let Some(found) = one(fields
        .iter()
        .copied()
        .filter(|target| called(target) == wanted)
        .collect())
    {
        return found;
    }
    if let Some(found) = one(fields
        .iter()
        .copied()
        .filter(|target| {
            let name = called(target);
            name.len() >= 3 && (name.contains(&wanted) || wanted.contains(&name))
        })
        .collect())
    {
        return found;
    }
    // A field with no name of its own: the nearest one to the right of, or under, the words that label it on the screen.
    let words = scene
        .targets
        .iter()
        .filter(|target| !takes_input(target))
        .find(|target| plain(&target.name) == wanted)
        .or_else(|| {
            scene
                .targets
                .iter()
                .filter(|target| !takes_input(target))
                .find(|target| plain(&target.name).contains(&wanted))
        });
    if let Some(words) = words {
        let middle = |frame: [f64; 4]| (frame[0] + frame[2] / 2.0, frame[1] + frame[3] / 2.0);
        let (label_x, label_y) = middle(words.frame);
        let beside = fields
            .iter()
            .copied()
            .filter_map(|field| {
                let (x, y) = middle(field.frame);
                let same_row =
                    (y - label_y).abs() <= field.frame[3].max(words.frame[3]) && x > label_x;
                let under = y > label_y
                    && y - label_y <= 0.12
                    && field.frame[0] <= label_x
                    && label_x <= field.frame[0] + field.frame[2].max(0.02);
                (same_row || under)
                    .then(|| (field, (x - label_x).abs() + (y - label_y).abs() * 2.0))
            })
            .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
        if let Some((field, _)) = beside {
            return Ok(field);
        }
    }
    Err(tool_failure(
        "DESKTOP_TARGET_NOT_FOUND",
        format!("“{label}” alanı ekranda bulunamadı."),
    ))
}

/// "evet", "açık", "true"... for a box that is ticked or not.
fn ticked(value: &str) -> Option<bool> {
    match plain(value).as_str() {
        "true" | "evet" | "acik" | "on" | "yes" | "1" | "isaretli" | "secili" | "checked" => {
            Some(true)
        }
        "false" | "hayir" | "kapali" | "off" | "no" | "0" | "isaretsiz" | "bos" | "unchecked" => {
            Some(false)
        }
        _ => None,
    }
}

/// One field of a form, filled the way its kind takes it and read back: a box is ticked, a list is chosen from, a text field is emptied and typed into.
pub(super) fn fill_field(engine: &dyn Engine, step: &Request, scene: &Scene) -> Result<Answer> {
    let text = step
        .text
        .clone()
        .filter(|text| !text.is_empty())
        .ok_or_else(|| tool_failure("DESKTOP_ACTION_INVALID", "Yazılacak değer yok."))?;
    let target = field_for(step, scene)?;
    refuse_secure(target)?;
    let read_back = |mut answer: Answer, held: bool| {
        if held || answer.delivery.as_deref() == Some("delivered_verified") {
            if !answer.data.is_object() {
                answer.data = json!({});
            }
            answer.data["readBack"] = json!(true);
        }
        answer
    };
    let named = Request {
        action: "type".into(),
        target_ref: Some(target.reference.clone()),
        text: Some(text.clone()),
        ..Request::default()
    };
    // Seen only as words on the screen: clicked into and pasted, with nothing to read back.
    if target.helper.is_empty() {
        return type_text(engine, &named, scene);
    }
    match target.role.as_str() {
        "checkbox" | "switch" | "radiobutton" => {
            let on = ticked(&text).ok_or_else(|| {
                tool_failure(
                    "DESKTOP_ACTION_INVALID",
                    "İşaret kutusu için değer evet ya da hayır olmalı.",
                )
            })?;
            let command = if on { "check" } else { "uncheck" };
            return call(engine, &[command.into(), target.helper.clone()])
                .map(|answer| read_back(answer, false));
        }
        "popupbutton" => {
            return call(engine, &["select".into(), target.helper.clone(), text])
                .map(|answer| read_back(answer, false));
        }
        "slider" | "stepper" | "incrementor" => {
            return call(engine, &["set-value".into(), target.helper.clone(), text])
                .map(|answer| read_back(answer, false));
        }
        _ => {}
    }
    // What the field already holds is typed over, the way a hand does it: clicked into, everything in it selected, the text put in. (The app's own
    // "empty this field" call is not used: on a web page it emptied the field that had the keyboard, which was the one filled a moment before.)
    let answer = if !target.value.is_empty() && target.value != text {
        let mut click = vec!["mouse-click".to_string()];
        click.extend(coordinates("--xy", middle_of(target.frame, scene.screen)));
        click.push("--headed".into());
        forward(engine, scene);
        call(engine, &click)?;
        let modifier = if cfg!(target_os = "macos") {
            "cmd"
        } else {
            "ctrl"
        };
        press(engine, &format!("{modifier}+a"), &scene.application)?;
        paste(engine, &text, &scene.application)?
    } else {
        type_text(engine, &named, scene)?
    };
    let held = call(
        engine,
        &[
            "get".into(),
            target.helper.clone(),
            "--property".into(),
            "value".into(),
        ],
    )
    .ok()
    .and_then(|read| read.data["value"].as_str().map(str::to_string))
    .map(|value| value.trim() == text.trim());
    // A field that can be read says whether it holds the text; one that cannot leaves it to what the screen shows.
    let mut answer = read_back(answer, held == Some(true));
    if held == Some(false) {
        if !answer.data.is_object() {
            answer.data = json!({});
        }
        answer.data["readBack"] = json!(false);
    }
    Ok(answer)
}

/// The words are on the screen now: a control or a line of text that holds them inside the window, or (where the controls say little) among the words read off it.
fn shows(engine: &dyn Engine, scene: &Scene, wanted: &str, text: &str) -> bool {
    let found = engine
        .call(&[
            "find".into(),
            "--app".into(),
            scene.application.clone(),
            "--text".into(),
            text.to_string(),
            "--limit".into(),
            "30".into(),
        ])
        .ok()
        .filter(|answer| answer.ok);
    let [screen_x, screen_y, screen_w, screen_h] = scene.screen;
    let window = scene.window.unwrap_or([0.0, 0.0, 1.0, 1.0]);
    let (left, top) = (
        screen_x + window[0] * screen_w,
        screen_y + window[1] * screen_h,
    );
    let (right, bottom) = (left + window[2] * screen_w, top + window[3] * screen_h);
    for item in found
        .iter()
        .flat_map(|answer| answer.data["matches"].as_array().into_iter().flatten())
    {
        // The helper also lists what holds a match (the window, the page, the groups around it): only a thing that is itself called by the words counts.
        let called = |key: &str| plain(item[key].as_str().unwrap_or_default()).contains(wanted);
        if !(called("name") || called("value"))
            || matches!(
                item["role"].as_str().unwrap_or_default(),
                "window" | "application" | "webarea"
            )
        {
            continue;
        }
        let off_screen = item["states"]
            .as_array()
            .is_some_and(|states| states.iter().any(|state| state == "offscreen"));
        let reference = item["ref_id"].as_str();
        if off_screen {
            // A control the app knows but does not show yet: brought into view by the app's own means.
            if reference.is_some_and(|reference| {
                engine
                    .call(&["scroll-to".into(), reference.to_string()])
                    .is_ok_and(|answer| answer.ok)
            }) {
                return true;
            }
            continue;
        }
        let bounds = &item["bounds"];
        match (
            bounds["x"].as_f64(),
            bounds["y"].as_f64(),
            bounds["width"].as_f64(),
            bounds["height"].as_f64(),
        ) {
            (Some(x), Some(y), Some(w), Some(h)) => {
                let (middle_x, middle_y) = (x + w / 2.0, y + h / 2.0);
                if h > 1.0
                    && middle_x >= left
                    && middle_x <= right
                    && middle_y >= top
                    && middle_y <= bottom
                {
                    return true;
                }
            }
            // A control with no place given that is not off the screen is on it.
            _ if reference.is_some() => return true,
            _ => {}
        }
    }
    observe(engine, Some(&scene.application)).is_ok_and(|look| {
        look.targets.iter().any(|target| {
            plain(&target.name).contains(wanted) || plain(&target.value).contains(wanted)
        })
    })
}

/// How long a scroll looks for its words before it says they are not there.
const SCROLL_FOR: Duration = Duration::from_secs(30);

/// Scrolls a page at a time until the words are on the screen. It stops when they are, when the list no longer moves (its end), or when its time is up.
pub(super) fn scroll_until(engine: &dyn Engine, step: &Request, scene: &Scene) -> Result<Answer> {
    let text = step.text.as_deref().unwrap_or_default().trim().to_string();
    let wanted = plain(&text);
    if wanted.is_empty() {
        return Err(tool_failure("DESKTOP_ACTION_INVALID", "Aranacak yazı yok."));
    }
    let found = |pages: u32| Answer {
        ok: true,
        data: json!({ "readBack": true, "pages": pages }),
        code: String::new(),
        message: String::new(),
        delivery: Some(
            if pages == 0 {
                "not_delivered"
            } else {
                "delivered_verified"
            }
            .into(),
        ),
    };
    let pages = step.amount.unwrap_or(8).clamp(1, 15);
    let page = Request {
        action: "scroll".into(),
        text: None,
        amount: Some(5),
        ..step.clone()
    };
    // Where things are: a list that has reached its end shows the same things in the same places after another push.
    let places = |engine: &dyn Engine| {
        observe(engine, Some(&scene.application)).ok().map(|look| {
            look.targets
                .iter()
                .map(|target| {
                    (
                        target.name.clone(),
                        (target.frame[1] * 1000.0).round() as i64,
                    )
                })
                .collect::<Vec<_>>()
        })
    };
    let began = Instant::now();
    for turned in 0..pages {
        if shows(engine, scene, &wanted, &text) {
            return Ok(found(turned));
        }
        if began.elapsed() >= SCROLL_FOR {
            break;
        }
        let before = places(engine);
        perform(engine, &page, scene)?;
        std::thread::sleep(Duration::from_millis(250));
        if before.is_some() && before == places(engine) {
            return Err(tool_failure(
                "DESKTOP_TARGET_NOT_FOUND",
                format!(
                    "“{text}” bulunamadı: {} sayfa kaydırıldı ve listenin sonuna gelindi.",
                    turned + 1
                ),
            ));
        }
    }
    if shows(engine, scene, &wanted, &text) {
        return Ok(found(pages));
    }
    Err(tool_failure(
        "DESKTOP_TARGET_NOT_FOUND",
        format!(
            "“{text}” {pages} sayfa kaydırmada görünmedi; daha fazla kaydırmak için yeniden çağır."
        ),
    ))
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
            "moreTargets": observation.map(|observation| observation.hidden.len()).unwrap_or(0),
            "accessGranted": access,
            "changed": changed,
            "verified": true,
            "evidence": evidence,
        }),
    )
}
