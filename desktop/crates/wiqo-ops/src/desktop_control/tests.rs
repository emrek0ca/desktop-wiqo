use super::*;

/// Answers helper commands from a script: the first rule whose words all appear in the command wins.
struct Script(Mutex<Vec<(Vec<&'static str>, Value)>>);

impl Script {
    fn new(rules: Vec<(Vec<&'static str>, Value)>) -> Arc<Script> {
        Arc::new(Script(Mutex::new(rules)))
    }
}

impl Engine for Script {
    fn bring_to_front(&self, _: &str) -> bool {
        true
    }

    fn call(&self, args: &[String]) -> std::result::Result<Answer, String> {
        let line = args.join(" ");
        let mut rules = self.0.lock().unwrap();
        let position = rules
            .iter()
            .position(|(words, _)| words.iter().all(|word| line.contains(word)))
            .ok_or_else(|| format!("no rule for {line}"))?;
        let envelope = if rules
            .iter()
            .filter(|(words, _)| words.iter().all(|word| line.contains(word)))
            .count()
            > 1
        {
            rules.remove(position).1
        } else {
            rules[position].1.clone()
        };
        parse_envelope(&envelope.to_string())
    }
}

fn screen(snapshot: &str, button_name: &str) -> Value {
    json!({ "ok": true, "data": { "app": "Hesap", "snapshot_id": snapshot, "window": { "title": "Hesap" }, "tree": {
        "role": "window", "children": [
            { "ref_id": format!("@{snapshot}:e1"), "role": "button", "name": button_name, "available_actions": ["Click"], "bounds": { "x": 100.0, "y": 50.0, "width": 40.0, "height": 20.0 } },
            { "ref_id": format!("@{snapshot}:e2"), "role": "textfield", "name": "Parola", "states": ["secure"], "available_actions": ["SetValue"], "bounds": { "x": 0.0, "y": 0.0, "width": 80.0, "height": 20.0 } },
            { "ref_id": format!("@{snapshot}:e3"), "role": "group", "name": "kap", "bounds": { "x": 0.0, "y": 0.0, "width": 80.0, "height": 20.0 } }
        ] } } })
}

fn displays() -> (Vec<&'static str>, Value) {
    (
        vec!["list-displays"],
        json!({ "ok": true, "data": [{ "is_primary": true, "bounds": { "width": 1000.0, "height": 500.0 } }] }),
    )
}

fn request(task: &str, action: &str, extra: Value) -> Value {
    let mut value = json!({ "taskId": task, "action": action });
    for (key, item) in extra.as_object().unwrap() {
        value[key] = item.clone();
    }
    json!({ "kind": "computerDesktop", "value": value })
}

fn result(done: Result<Done>) -> (Value, Option<String>) {
    match done {
        Ok(done) => (
            done.data.unwrap_or(Value::Null),
            done.error.map(|error| error.code),
        ),
        Err(error) => (Value::Null, Some(error.code)),
    }
}

#[test]
fn observing_lists_only_real_controls_with_normalised_frames_and_hides_secure_names() {
    let engine = Script::new(vec![displays(), (vec!["snapshot"], screen("s1", "7"))]);
    let (data, code) = result(control(
        engine.as_ref(),
        &request("t-obs", "observe", json!({})),
    ));
    assert_eq!(code, None);
    let targets = data["value"]["targets"].as_array().unwrap();
    assert_eq!(targets.len(), 2, "the anonymous group is scaffolding");
    // The server accepts only `d-` and 8 hex digits as a desktop ref.
    let shape = regex::Regex::new("^d-[0-9a-f]{8}$").unwrap();
    assert!(targets
        .iter()
        .all(|target| shape.is_match(target["ref"].as_str().unwrap())));
    assert_eq!(targets[0]["frame"]["x"], 0.1);
    assert_eq!(targets[0]["frame"]["width"], 0.04);
    assert_eq!(targets[1]["secure"], true);
    assert_eq!(targets[1]["name"], "");
}

#[test]
fn a_click_needs_a_recent_observation_that_the_model_named() {
    let engine = Script::new(vec![displays(), (vec!["snapshot"], screen("s2", "7"))]);
    let none = result(control(
        engine.as_ref(),
        &request(
            "t-none",
            "click",
            json!({ "targetRef": contract_ref("button|7|0") }),
        ),
    ));
    assert_eq!(none.1.as_deref(), Some("DESKTOP_OBSERVATION_REQUIRED"));
    control(engine.as_ref(), &request("t-snap", "observe", json!({}))).unwrap();
    let wrong = result(control(
        engine.as_ref(),
        &request(
            "t-snap",
            "click",
            json!({ "targetRef": contract_ref("button|7|0"), "snapshotId": "other" }),
        ),
    ));
    // A look the model read before the last one is taken again, not refused: the ref still names the same control.
    assert_ne!(wrong.1.as_deref(), Some("DESKTOP_STALE_OBSERVATION"));
    assert_ne!(wrong.1.as_deref(), Some("DESKTOP_TARGET_NOT_FOUND"));
    let unseen = result(control(
        engine.as_ref(),
        &request(
            "t-snap",
            "click",
            json!({ "targetRef": contract_ref("button|yok|0") }),
        ),
    ));
    assert_eq!(unseen.1.as_deref(), Some("DESKTOP_TARGET_NOT_FOUND"));
}

#[test]
fn a_click_is_verified_by_reading_the_screen_again() {
    let engine = Script::new(vec![
        displays(),
        (vec!["snapshot"], screen("s3", "7")),
        (
            vec!["click", "@s3:e1"],
            json!({ "ok": true, "data": { "action": "click", "disposition": { "delivery": "delivered_unverified", "retry": "unsafe" } } }),
        ),
        (vec!["snapshot"], screen("s4", "8")),
    ]);
    control(engine.as_ref(), &request("t-click", "observe", json!({}))).unwrap();
    let (data, code) = result(control(
        engine.as_ref(),
        &request(
            "t-click",
            "click",
            json!({ "targetRef": contract_ref("button|7|0") }),
        ),
    ));
    assert_eq!(code, None);
    assert_eq!(
        data["value"]["verified"], true,
        "the button's name changed on screen"
    );
    assert_eq!(data["value"]["changed"], true);
    assert_eq!(data["value"]["snapshotId"], "s4");
}

#[test]
fn a_click_whose_effect_cannot_be_seen_is_sent_but_not_called_verified() {
    let engine = Script::new(vec![
        displays(),
        (vec!["snapshot"], screen("s5", "7")),
        (
            vec!["click"],
            json!({ "ok": true, "data": { "action": "click", "disposition": { "delivery": "delivered_unverified", "retry": "unsafe" } } }),
        ),
        (vec!["snapshot"], screen("s5", "7")),
    ]);
    control(engine.as_ref(), &request("t-quiet", "observe", json!({}))).unwrap();
    let (data, code) = result(control(
        engine.as_ref(),
        &request(
            "t-quiet",
            "click",
            json!({ "targetRef": contract_ref("button|7|0") }),
        ),
    ));
    assert_eq!(code, None);
    assert_eq!(data["value"]["verified"], false);
}

#[test]
fn the_helpers_delivery_decides_whether_a_failure_may_have_changed_anything() {
    let untouched = parse_envelope(r#"{"ok":false,"error":{"code":"TIMEOUT","message":"x","disposition":{"delivery":"not_delivered","retry":"safe"}}}"#).unwrap();
    assert_eq!(contract_error(&untouched).code, "DESKTOP_UNAVAILABLE");
    let uncertain = parse_envelope(r#"{"ok":false,"error":{"code":"TIMEOUT","message":"x","disposition":{"delivery":"unknown","retry":"unknown"}}}"#).unwrap();
    assert_eq!(contract_error(&uncertain).code, "COMMAND_TIMEOUT");
    let stale = parse_envelope(r#"{"ok":false,"error":{"code":"STALE_REF","message":"x","disposition":{"delivery":"not_delivered","retry":"safe"}}}"#).unwrap();
    assert_eq!(contract_error(&stale).code, "DESKTOP_STALE_OBSERVATION");
    let permission =
        parse_envelope(r#"{"ok":false,"error":{"code":"PERM_DENIED","message":"x"}}"#).unwrap();
    assert_eq!(
        contract_error(&permission).code,
        "ACCESSIBILITY_PERMISSION_REQUIRED"
    );
}

#[test]
fn a_password_field_is_never_typed_into_or_clicked() {
    let engine = Script::new(vec![displays(), (vec!["snapshot"], screen("s6", "7"))]);
    control(engine.as_ref(), &request("t-secure", "observe", json!({}))).unwrap();
    let typed = result(control(
        engine.as_ref(),
        &request(
            "t-secure",
            "type",
            json!({ "targetRef": contract_ref("textfield|Parola|0"), "text": "x" }),
        ),
    ));
    assert_eq!(typed.1.as_deref(), Some("DESKTOP_SECURE_FIELD_BLOCKED"));
}

#[test]
fn a_missing_helper_is_reported_as_unavailable_not_as_a_failed_click() {
    struct Missing;
    impl Engine for Missing {
        fn call(&self, _: &[String]) -> std::result::Result<Answer, String> {
            Err("helper missing".into())
        }
    }
    let (_, code) = result(control(
        &Missing,
        &request("t-missing", "observe", json!({})),
    ));
    assert_eq!(code.as_deref(), Some("DESKTOP_ADAPTER_UNAVAILABLE"));
}

#[test]
fn steps_stop_when_one_cannot_be_seen_to_work() {
    let engine = Script::new(vec![
        displays(),
        (vec!["snapshot"], screen("s7", "7")),
        (
            vec!["click"],
            json!({ "ok": true, "data": { "action": "click", "disposition": { "delivery": "delivered_unverified", "retry": "unsafe" } } }),
        ),
        (vec!["snapshot"], screen("s7", "7")),
    ]);
    control(engine.as_ref(), &request("t-steps", "observe", json!({}))).unwrap();
    let steps = json!({ "steps": [{ "action": "click", "targetRef": contract_ref("button|7|0") }, { "action": "click", "targetRef": contract_ref("button|7|0") }] });
    let (data, code) = result(control(
        engine.as_ref(),
        &request("t-steps", "runSteps", steps),
    ));
    assert_eq!(code.as_deref(), Some("DESKTOP_TYPE_UNVERIFIED"));
    assert_eq!(
        data["value"]["evidence"].as_array().unwrap().len(),
        1,
        "the second click was not sent"
    );
}

#[test]
fn a_shortcut_reaches_the_helper_in_its_own_key_names() {
    let engine = Script::new(vec![
        displays(),
        (vec!["snapshot"], screen("s8", "7")),
        (
            vec!["press", "cmd+n"],
            json!({ "ok": true, "data": { "action": "press", "disposition": { "delivery": "delivered_unverified", "retry": "unsafe" } } }),
        ),
        (
            vec!["press", "up"],
            json!({ "ok": true, "data": { "action": "press", "disposition": { "delivery": "delivered_unverified", "retry": "unsafe" } } }),
        ),
    ]);
    control(engine.as_ref(), &request("t-keys", "observe", json!({}))).unwrap();
    let (_, code) = result(control(
        engine.as_ref(),
        &request("t-keys", "hotkey", json!({ "keys": ["cmd", "n"] })),
    ));
    assert_eq!(code, None);
    let (_, code) = result(control(
        engine.as_ref(),
        &request("t-keys", "hotkey", json!({ "keys": ["arrowup"] })),
    ));
    assert_eq!(code, None, "arrowup is the helper's up");
}

fn editor(snapshot: &str, text: &str) -> Value {
    json!({ "ok": true, "data": { "app": "TextEdit", "snapshot_id": snapshot, "window": { "title": "Adsız" }, "tree": {
        "role": "window", "children": [
            { "ref_id": format!("@{snapshot}:e2"), "role": "textfield", "value": text, "states": ["focused"], "available_actions": ["SetValue", "TypeText"], "bounds": { "x": 0.0, "y": 40.0, "width": 800.0, "height": 400.0 } }
        ] } } })
}

#[test]
fn text_without_a_named_field_goes_into_the_focused_one_and_is_read_back() {
    let engine = Script::new(vec![
        displays(),
        (vec!["snapshot"], editor("s9", "")),
        (
            vec!["type", "@s9:e2", "merhaba"],
            json!({ "ok": true, "data": { "action": "type", "disposition": { "delivery": "delivered_verified", "retry": "unsafe" } } }),
        ),
        (vec!["snapshot"], editor("s10", "merhaba")),
    ]);
    control(engine.as_ref(), &request("t-type", "observe", json!({}))).unwrap();
    let (data, code) = result(control(
        engine.as_ref(),
        &request("t-type", "type", json!({ "text": "merhaba" })),
    ));
    assert_eq!(code, None);
    assert_eq!(data["value"]["verified"], true);
    // The same screen: only what changed comes back, the field with its new text.
    assert_eq!(data["value"]["changedTargets"][0]["name"], "merhaba");
}

#[test]
fn a_click_without_a_ref_lands_on_the_point_the_model_named() {
    let engine = Script::new(vec![
        displays(),
        (vec!["snapshot"], screen("s11", "7")),
        (
            vec!["mouse-click", "--xy 500,250", "--headed"],
            json!({ "ok": true, "data": { "action": "mouse_click", "disposition": { "delivery": "delivered_unverified", "retry": "unsafe" } } }),
        ),
        (vec!["snapshot"], screen("s12", "8")),
    ]);
    control(engine.as_ref(), &request("t-point", "observe", json!({}))).unwrap();
    let (data, code) = result(control(
        engine.as_ref(),
        &request(
            "t-point",
            "click",
            json!({ "normalizedX": 0.5, "normalizedY": 0.5 }),
        ),
    ));
    assert_eq!(code, None);
    assert_eq!(data["value"]["verified"], true);
}

#[test]
fn a_dialogs_buttons_come_before_its_sidebar_and_repeats_are_dropped() {
    let target = |role: &str, name: &str| Target {
        reference: contract_ref(name),
        helper: String::new(),
        role: role.into(),
        name: name.into(),
        frame: [0.0; 4],
        actions: vec!["Click".into()],
        enabled: true,
        secure: false,
        focused: false,
        value: String::new(),
        states: String::new(),
        identity: name.to_string(),
    };
    let ranked = ranked(vec![
        target("treeitem", "Desktop"),
        target("cell", "Desktop"),
        target("scrollarea", ""),
        target("textfield", "Ara"),
        target("button", "Yeni Belge"),
    ]);
    let names: Vec<&str> = ranked.iter().map(|target| target.name.as_str()).collect();
    assert_eq!(names, ["Yeni Belge", "Ara", "Desktop"]);
}

fn acting_scene(names: &[&str]) -> Scene {
    Scene {
        targets: names
            .iter()
            .enumerate()
            .map(|(index, name)| Target {
                reference: format!("d-{index}"),
                helper: format!("@s:e{index}"),
                role: "button".into(),
                name: (*name).into(),
                frame: [0.1 * index as f64, 0.1, 0.05, 0.05],
                actions: vec!["Click".into()],
                enabled: true,
                secure: false,
                focused: false,
                value: String::new(),
                states: String::new(),
                identity: (*name).into(),
            })
            .collect(),
        application: "Mail".into(),
        screen: [1000.0, 100.0, 2000.0, 1000.0],
        window: Some([0.0, 0.0, 0.5, 0.5]),
    }
}

fn step(value: Value) -> Request {
    serde_json::from_value(value).unwrap()
}

/// The helper's arguments for one step, as one line.
fn sent(step: &Request, scene: &Scene) -> String {
    struct Echo(Mutex<String>);
    impl Engine for Echo {
        fn call(&self, args: &[String]) -> std::result::Result<Answer, String> {
            *self.0.lock().unwrap() = args.join(" ");
            Ok(Answer {
                ok: true,
                data: json!({}),
                code: String::new(),
                message: String::new(),
                delivery: None,
            })
        }
    }
    let echo = Echo(Mutex::new(String::new()));
    perform(&echo, step, scene)
        .map_err(|error| error.code)
        .unwrap();
    let line = echo.0.lock().unwrap().clone();
    line
}

#[test]
fn a_button_that_cannot_be_taken_back_needs_a_yes_whatever_the_language_or_the_way_to_it() {
    let scene = acting_scene(&["Gönder", "Kaydet", "Satın Al", "Silindir çiz", "Delete"]);
    let click = |target: &str| step(json!({ "action": "click", "targetRef": target }));
    assert!(needs_a_yes(&click("d-0"), &scene)
        .unwrap()
        .contains("“Gönder”"));
    assert!(
        needs_a_yes(&click("d-1"), &scene).is_none(),
        "saving is not"
    );
    assert!(needs_a_yes(&click("d-2"), &scene).is_some());
    assert!(
        needs_a_yes(&click("d-3"), &scene).is_none(),
        "a word that only starts the same"
    );
    assert!(needs_a_yes(&click("d-4"), &scene).is_some());
    // The same button by its point on the screen.
    let by_point = step(json!({ "action": "click", "normalizedX": 0.02, "normalizedY": 0.12 }));
    assert!(needs_a_yes(&by_point, &scene).is_some());
    let keys = |keys: Value| step(json!({ "action": "hotkey", "keys": keys }));
    assert!(needs_a_yes(&keys(json!(["command", "enter"])), &scene).is_some());
    assert!(needs_a_yes(&keys(json!(["command", "delete"])), &scene).is_some());
    assert!(needs_a_yes(&keys(json!(["command", "s"])), &scene).is_none());
    assert!(needs_a_yes(&keys(json!(["enter"])), &scene).is_none());
}

#[test]
fn the_refusal_comes_before_anything_is_sent_and_a_signed_yes_or_the_persons_own_tap_passes() {
    let snapshot = "s-gate";
    let rules = || {
        Script::new(vec![
            displays(),
            (
                vec!["list-surfaces"],
                json!({ "ok": true, "data": { "surfaces": [] } }),
            ),
            (vec!["snapshot"], screen(snapshot, "Gönder")),
            (
                vec!["click"],
                json!({ "ok": true, "data": { "disposition": { "delivery": "delivered_verified" } } }),
            ),
        ])
    };
    for (task, extra, refused) in [
        ("t-gate-model", json!({}), true),
        ("t-gate-card", json!({ "cardApproved": true }), false),
        ("t-gate-person", json!({ "person": true }), false),
    ] {
        let engine = rules();
        let (data, _) = result(control(
            engine.as_ref(),
            &request(task, "observe", json!({ "text": "Mail" })),
        ));
        let target = data["value"]["targets"][0]["ref"].clone();
        let mut extra = extra;
        extra["targetRef"] = target;
        let (_, code) = result(control(engine.as_ref(), &request(task, "click", extra)));
        assert_eq!(
            code.as_deref() == Some("DESKTOP_APPROVAL_REQUIRED"),
            refused,
            "{task}: {code:?}"
        );
    }
}

#[test]
fn a_stopped_task_does_nothing_more_and_a_new_one_runs() {
    let engine = Script::new(vec![
        displays(),
        (
            vec!["list-surfaces"],
            json!({ "ok": true, "data": { "surfaces": [] } }),
        ),
        (vec!["snapshot"], screen("s-stop", "Tamam")),
    ]);
    let (_, code) = result(control(
        engine.as_ref(),
        &request("t-stopped", "observe", json!({ "text": "Mail" })),
    ));
    assert_eq!(code, None);
    stop_tasks(|task| task == "t-stopped");
    let (_, code) = result(control(
        engine.as_ref(),
        &request("t-stopped", "observe", json!({ "text": "Mail" })),
    ));
    assert_eq!(code.as_deref(), Some("DESKTOP_STOPPED_BY_PERSON"));
    let (_, code) = result(control(
        engine.as_ref(),
        &request("t-after-stop", "observe", json!({ "text": "Mail" })),
    ));
    assert_eq!(code, None);
}

#[test]
fn the_person_is_active_only_when_the_input_is_recent_and_not_our_own() {
    assert!(
        person_is_active(0.1, f64::INFINITY),
        "their hand, before we did anything"
    );
    assert!(
        person_is_active(0.2, 5.0),
        "their hand, long after our last step"
    );
    assert!(!person_is_active(0.2, 0.3), "our own input landing");
    assert!(!person_is_active(5.0, 9.0), "they stopped");
}

#[test]
fn with_nobody_at_the_computer_it_goes_on_at_once() {
    struct Away;
    impl Engine for Away {
        fn call(&self, _: &[String]) -> std::result::Result<Answer, String> {
            Err("unused".into())
        }
        fn seconds_since_input(&self) -> Option<f64> {
            Some(5.0)
        }
    }
    assert!(wait_for_the_person(&Away).is_ok());
}

#[test]
fn an_open_menu_comes_first_and_the_window_is_on_the_display_that_holds_it() {
    let engine = Script::new(vec![
        (
            vec!["list-displays"],
            json!({ "ok": true, "data": [
                { "bounds": { "x": 0.0, "y": 0.0, "width": 1000.0, "height": 500.0 }, "is_primary": true },
                { "bounds": { "x": 1000.0, "y": 0.0, "width": 2000.0, "height": 1000.0 }, "is_primary": false }
            ] }),
        ),
        (
            vec!["list-surfaces"],
            json!({ "ok": true, "data": { "surfaces": [{ "kind": "menu" }, { "kind": "window" }] } }),
        ),
        (
            vec!["snapshot", "--surface", "menu"],
            json!({ "ok": true, "data": { "snapshot_id": "s-menu", "tree": { "children": [
                { "ref_id": "@s-menu:e1", "role": "menuitem", "name": "Kopyala", "available_actions": ["Click"],
                  "bounds": { "x": 1200.0, "y": 100.0, "width": 100.0, "height": 20.0 } }
            ] } } }),
        ),
        (
            vec!["snapshot", "--app"],
            json!({ "ok": true, "data": { "snapshot_id": "s-win", "app": "Notlar", "tree": {
                "bounds": { "x": 1100.0, "y": 50.0, "width": 800.0, "height": 600.0 },
                "children": [
                    { "ref_id": "@s-win:e1", "role": "button", "name": "Yeni", "available_actions": ["Click"],
                      "bounds": { "x": 1200.0, "y": 60.0, "width": 40.0, "height": 20.0 } }
                ] } } }),
        ),
    ]);
    let observation = observe_controls(engine.as_ref(), Some("Notlar")).unwrap();
    assert_eq!(observation.display, 1);
    assert_eq!(observation.targets[0].name, "Kopyala");
    assert_eq!(observation.targets[1].name, "Yeni");
    assert_eq!(
        observation.targets[1].frame[0], 0.1,
        "a fraction of its own display"
    );
}

#[test]
fn the_hand_on_the_mouse_reaches_what_a_ref_cannot() {
    let mut scene = acting_scene(&["Kaydırıcı"]);
    scene.targets.push(Target {
        helper: String::new(),
        reference: "d-word".into(),
        name: "Rapor".into(),
        frame: [0.5, 0.5, 0.1, 0.1],
        ..scene.targets[0].clone()
    });
    // A drag between two points, and from a word on the screen to a control.
    let line = sent(
        &step(
            json!({ "action": "dragDrop", "normalizedX": 0.0, "normalizedY": 0.0, "toNormalizedX": 0.5, "toNormalizedY": 0.5 }),
        ),
        &scene,
    );
    assert!(
        line.contains("--from-xy 1000,100")
            && line.contains("--to-xy 2000,600")
            && line.contains("--headed"),
        "{line}"
    );
    let line = sent(
        &step(json!({ "action": "dragDrop", "sourceRef": "d-word", "destinationRef": "d-0" })),
        &scene,
    );
    assert!(
        line.contains("--from-xy 2100,650") && line.contains("--to @s:e0"),
        "{line}"
    );
    // A scroll with no control is the wheel over the window, in any direction.
    let line = sent(
        &step(json!({ "action": "scroll", "direction": "left", "amount": 2 })),
        &scene,
    );
    assert!(
        line.starts_with("mouse-wheel")
            && line.contains("--dx=-2")
            && line.contains("--x=1500")
            && line.contains("--y=350"),
        "{line}"
    );
    let line = sent(
        &step(json!({ "action": "scroll", "targetRef": "d-word", "direction": "down" })),
        &scene,
    );
    assert!(
        line.starts_with("mouse-wheel") && line.contains("--dy=-3"),
        "{line}"
    );
    assert!(sent(
        &step(json!({ "action": "tripleClick", "targetRef": "d-0" })),
        &scene
    )
    .starts_with("triple-click @s:e0"));
    assert!(sent(
        &step(json!({ "action": "setValue", "targetRef": "d-0", "text": "75" })),
        &scene
    )
    .starts_with("set-value @s:e0 75"));
}

#[test]
fn the_agent_cannot_reach_its_own_approval_card() {
    struct WiqoInFront;
    impl Engine for WiqoInFront {
        fn call(&self, args: &[String]) -> std::result::Result<Answer, String> {
            Err(format!("nothing should reach the helper: {args:?}"))
        }
        fn frontmost(&self) -> Option<String> {
            Some("Wiqo".into())
        }
    }
    // Looking at it, by name or because it is in front.
    for extra in [json!({ "text": "wiqo-desktop" }), json!({})] {
        let (_, code) = result(control(&WiqoInFront, &request("t-self", "observe", extra)));
        assert_eq!(
            code.as_deref(),
            Some("DESKTOP_SELF_OBSERVATION_UNAVAILABLE")
        );
    }
    // A click on a point while it stays in front, whichever app was observed.
    let scene = acting_scene(&["Yeni"]);
    let by_point = step(json!({ "action": "click", "normalizedX": 0.5, "normalizedY": 0.5 }));
    assert!(keep_off_own_windows(&WiqoInFront, &by_point, &scene).is_err());
    let by_ref = step(json!({ "action": "click", "targetRef": "d-0" }));
    assert!(
        keep_off_own_windows(&WiqoInFront, &by_ref, &scene).is_ok(),
        "a named control of the other app is that app's"
    );
    let mut own = acting_scene(&["İzin ver"]);
    own.application = "Wiqo".into();
    assert!(keep_off_own_windows(&WiqoInFront, &by_ref, &own).is_err());
}

#[test]
fn a_ref_stays_the_same_across_looks_and_while_a_field_fills_up() {
    let look = |snapshot: &str, field: &str| {
        let mut out = Vec::new();
        collect(
            &json!({ "children": [
                { "ref_id": format!("@{snapshot}:e1"), "role": "button", "name": "Tamam", "available_actions": ["Click"], "bounds": { "x": 1.0, "y": 1.0, "width": 9.0, "height": 9.0 } },
                { "ref_id": format!("@{snapshot}:e2"), "role": "button", "name": "Tamam", "available_actions": ["Click"], "bounds": { "x": 20.0, "y": 1.0, "width": 9.0, "height": 9.0 } },
                { "ref_id": format!("@{snapshot}:e3"), "role": "textarea", "value": field, "available_actions": ["Click"], "bounds": { "x": 1.0, "y": 20.0, "width": 9.0, "height": 9.0 } }
            ] }),
            [0.0, 0.0, 100.0, 100.0],
            &mut out,
        );
        assign_references(&mut out);
        out
    };
    let (first, second) = (look("s1", ""), look("s2", "merhaba"));
    assert_ne!(
        first[0].reference, first[1].reference,
        "two of the same are two refs"
    );
    for (a, b) in first.iter().zip(&second) {
        assert_eq!(a.reference, b.reference);
    }
    let (added, changed, removed) = changes_between(&first, &second);
    assert!(added.is_empty() && removed.is_empty());
    assert_eq!(changed.len(), 1, "only the field that filled up");
}

#[test]
fn a_menu_command_is_found_clicked_and_refused_when_grey_or_final() {
    let scene = acting_scene(&[]);
    let engine = Script::new(vec![
        (
            vec!["find", "--surface menubar", "Kaydet"],
            json!({ "ok": true, "data": { "match": { "ref_id": "@m:e1", "role": "menuitem", "states": [] } } }),
        ),
        (vec!["click @m:e1"], json!({ "ok": true, "data": {} })),
        (
            vec!["find", "--surface menubar", "Yazdır"],
            json!({ "ok": true, "data": { "match": { "ref_id": "@m:e2", "role": "menuitem", "states": ["disabled"] } } }),
        ),
    ]);
    let menu = |path: &str| step(json!({ "action": "menu", "text": path }));
    assert!(perform(engine.as_ref(), &menu("Dosya > Kaydet"), &scene).is_ok());
    let grey = perform(engine.as_ref(), &menu("Dosya > Yazdır"), &scene)
        .map(|_| ())
        .unwrap_err();
    assert_eq!(grey.code, "DESKTOP_TARGET_NOT_FOUND");
    assert!(
        needs_a_yes(&menu("Dosya > Çöp Kutusunu Boşalt"), &scene).is_none(),
        "the list is by whole phrase"
    );
    assert!(needs_a_yes(&menu("Posta > Gönder"), &scene).is_some());
}

#[test]
fn a_step_says_what_it_did_and_never_what_was_typed() {
    let scene = acting_scene(&["Yeni"]);
    assert_eq!(
        done_words(
            &step(json!({ "action": "click", "targetRef": "d-0" })),
            &scene
        ),
        "“Yeni” tıklandı"
    );
    assert_eq!(
        done_words(
            &step(json!({ "action": "type", "text": "gizli not" })),
            &scene
        ),
        "Yazı yazıldı"
    );
    assert_eq!(
        done_words(
            &step(json!({ "action": "hotkey", "keys": ["command", "n"] })),
            &scene
        ),
        "command+n kısayolu basıldı"
    );
}

#[test]
fn a_change_in_a_value_or_a_state_counts_as_an_effect() {
    let before = acting_scene(&["Ses"]).targets;
    let mut after = before.clone();
    after[0].value = "75".into();
    assert!(differs(&before, &after));
    let mut after = before.clone();
    after[0].states = "checked".into();
    assert!(differs(&before, &after));
    assert!(!differs(&before, &before.clone()));
}

/// Against the real helper on this computer: `WIQO_AGENT_DESKTOP=<path> WIQO_LIVE_APP=<app> cargo test live -- --ignored`.
#[test]
#[ignore]
fn live_observation_names_real_controls() {
    let (Some(app), Some(_)) = (
        std::env::var_os("WIQO_LIVE_APP"),
        std::env::var_os("WIQO_AGENT_DESKTOP"),
    ) else {
        return;
    };
    let engine = Helper::locate();
    let (data, code) = result(control(
        &engine,
        &request(
            "t-live",
            "observe",
            json!({ "text": app.to_string_lossy() }),
        ),
    ));
    assert_eq!(code, None, "{data}");
    let targets = data["value"]["targets"].as_array().unwrap();
    assert!(!targets.is_empty());
    println!("{} targets, first: {}", targets.len(), targets[0]);
}

/// The task that failed in the chat, against the real helper and TextEdit:
/// `WIQO_AGENT_DESKTOP=<path> cargo test -p wiqo-ops live_textedit -- --ignored --nocapture`.
#[test]
#[ignore]
fn live_textedit_new_document_and_text() {
    if std::env::var_os("WIQO_AGENT_DESKTOP").is_none() {
        return;
    }
    let engine = Helper::locate();
    let task = "t-live-textedit";
    let (_, code) = result(control(
        &engine,
        &request(task, "observe", json!({ "text": "TextEdit" })),
    ));
    assert_eq!(code, None);
    let (data, code) = result(control(
        &engine,
        &request(task, "hotkey", json!({ "keys": ["command", "n"] })),
    ));
    println!("command+n: {code:?} verified={}", data["value"]["verified"]);
    assert_eq!(code, None);
    let (data, code) = result(control(
        &engine,
        &request(task, "type", json!({ "text": "merhaba" })),
    ));
    println!(
        "type: {code:?} verified={} targets={}",
        data["value"]["verified"], data["value"]["targets"]
    );
    assert_eq!(code, None);
    assert_eq!(data["value"]["verified"], true);
}
