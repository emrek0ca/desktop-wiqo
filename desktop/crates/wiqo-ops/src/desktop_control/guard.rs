//! The person comes first: waiting while they type, keeping off Wiqo's own windows, and asking before a step that cannot be taken back.

use super::*;

// ---------- the person comes first ----------

/// While the person's hands are on the keyboard or mouse, Wiqo waits; it carries on when they stop.
pub(super) const QUIET_FOR: f64 = 1.2;
/// Our own input can read as theirs for a moment after it lands.
pub(super) const OWN_INPUT_MARGIN: f64 = 0.4;
pub(super) fn last_action() -> &'static Mutex<Option<Instant>> {
    static LAST: OnceLock<Mutex<Option<Instant>>> = OnceLock::new();
    LAST.get_or_init(Default::default)
}

/// They touched the keyboard or mouse just now, and it was not our own input landing.
pub(super) fn person_is_active(idle: f64, since_ours: f64) -> bool {
    idle < QUIET_FOR && idle + OWN_INPUT_MARGIN < since_ours
}

pub(super) fn wait_for_the_person(engine: &dyn Engine) -> Result<()> {
    let started = Instant::now();
    let mut told = false;
    loop {
        let Some(idle) = engine.seconds_since_input() else {
            return Ok(());
        };
        let since_ours = last_action()
            .lock()
            .ok()
            .and_then(|last| last.map(|at| at.elapsed().as_secs_f64()))
            .unwrap_or(f64::INFINITY);
        if !person_is_active(idle, since_ours) {
            return Ok(());
        }
        if started.elapsed() >= LONGEST_WAIT {
            return Err(tool_failure(
                "DESKTOP_PERSON_ACTIVE",
                "Bilgisayarı sen kullanıyorsun; araya girmedim. Bırakınca devam edebilirim.",
            ));
        }
        if !told {
            told = true;
            show_cursor(None, "wait", "Sen kullanıyorsun, bekliyorum");
        }
        std::thread::sleep(Duration::from_millis(150));
    }
}

// ---------- Wiqo's own windows are the person's ----------

pub(super) fn is_wiqo(application: &str) -> bool {
    let name = fold(application.trim());
    name == "wiqo" || name == "wiqo-desktop" || name == "wiqo desktop"
}

pub(super) fn own_window_refused() -> wiqo_core::wire::WireError {
    permission(
        "DESKTOP_SELF_OBSERVATION_UNAVAILABLE",
        "Wiqo'nun kendi penceresine dokunulmaz: izin kartı orada. Hangi uygulamada çalışılacağını application ile söyle.",
    )
}

/// The agent never drives the window that holds its own approval card: it could say yes to itself. A click on a point lands on whatever is in front, so the target
/// app is brought forward first, and the click is refused if Wiqo is still there.
pub(super) fn keep_off_own_windows(
    engine: &dyn Engine,
    step: &Request,
    scene: &Scene,
) -> Result<()> {
    if is_wiqo(&scene.application) {
        return Err(own_window_refused());
    }
    let by_point = step.target_ref.is_none()
        && step.source_ref.is_none()
        && matches!(
            step.action.as_str(),
            "click" | "doubleClick" | "tripleClick" | "rightClick" | "scroll" | "dragDrop"
        );
    if by_point {
        engine.bring_to_front(&scene.application);
        if engine.frontmost().is_some_and(|front| is_wiqo(&front)) {
            return Err(own_window_refused());
        }
    }
    Ok(())
}

// ---------- what needs the person's yes ----------

/// A control whose press cannot be taken back, spelled as `fold` leaves it. This is the safety net under the model's judgement, on what the screen really says: a
/// wrong match only costs one card, a missed one is why the card rule is not the only check.
pub(super) const IRREVERSIBLE_WORDS: &[&str] = &[
    "gonder",
    "gonderin",
    "yolla",
    "send",
    "sil",
    "silin",
    "delete",
    "remove",
    "kaldir",
    "kaldirin",
    "erase",
    "buy",
    "purchase",
    "ode",
    "odeme",
    "odeyin",
    "pay",
    "checkout",
    "yayinla",
    "publish",
    "paylas",
    "share",
    "transfer",
    "havale",
    "eft",
    "bicimlendir",
    "format",
    "sifirla",
    "reset",
    "uninstall",
    "unsubscribe",
    "onayla",
    "confirm",
    "replace",
    "discard",
    "kaydetme",
    "logout",
];
pub(super) const IRREVERSIBLE_PHRASES: &[&str] = &[
    "satin al",
    "siparis ver",
    "place order",
    "copu bosalt",
    "empty trash",
    "cop kutusuna tasi",
    "move to trash",
    "oturumu kapat",
    "cikis yap",
    "sign out",
    "log out",
    "hesabi kapat",
    "aboneligi iptal",
    "uzerine yaz",
    "dont save",
    "don't save",
];
/// Longer than this it is a sentence on the screen, not a control's name.
pub(super) const LONGEST_CONTROL_NAME: usize = 40;

pub(super) fn fold(text: &str) -> String {
    text.chars()
        .flat_map(|letter| match letter {
            'İ' | 'I' | 'ı' => vec!['i'],
            'ş' | 'Ş' => vec!['s'],
            'ğ' | 'Ğ' => vec!['g'],
            'ü' | 'Ü' => vec!['u'],
            'ö' | 'Ö' => vec!['o'],
            'ç' | 'Ç' => vec!['c'],
            other => other.to_lowercase().collect(),
        })
        .collect()
}

pub(super) fn cannot_be_taken_back(name: &str) -> bool {
    if name.chars().count() > LONGEST_CONTROL_NAME {
        return false;
    }
    let folded = fold(name);
    let words: Vec<&str> = folded
        .split(|letter: char| !letter.is_alphanumeric() && letter != '\'')
        .filter(|word| !word.is_empty())
        .collect();
    let joined = words.join(" ");
    words.iter().any(|word| IRREVERSIBLE_WORDS.contains(word))
        || IRREVERSIBLE_PHRASES
            .iter()
            .any(|phrase| format!(" {joined} ").contains(&format!(" {phrase} ")))
}

/// The smallest control under a point: what a click there lands on.
pub(super) fn control_at(point: [f64; 2], scene: &Scene) -> Option<&Target> {
    let [x, y, w, h] = scene.screen;
    let (px, py) = ((point[0] - x) / w, (point[1] - y) / h);
    scene
        .targets
        .iter()
        .filter(|target| {
            let [fx, fy, fw, fh] = target.frame;
            px >= fx && px <= fx + fw && py >= fy && py <= fy + fh
        })
        .min_by(|a, b| {
            (a.frame[2] * a.frame[3])
                .partial_cmp(&(b.frame[2] * b.frame[3]))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
}

/// The command a menu path ends in: "Dosya > Kaydet" is "Kaydet".
pub(super) fn menu_item(step: &Request) -> Option<&str> {
    step.text
        .as_deref()?
        .rsplit('>')
        .next()
        .map(str::trim)
        .filter(|item| !item.is_empty())
}

/// What a finished step did, in words a person reads. Typed text is not repeated: this line is kept on the server.
pub(super) fn done_words(step: &Request, scene: &Scene) -> String {
    let name = named_target(scene, step.target_ref.as_ref())
        .or_else(|| named_target(scene, step.source_ref.as_ref()))
        .map(|target| target.name.chars().take(40).collect::<String>())
        .filter(|name| !name.is_empty());
    let on = |verb: &str| match &name {
        Some(name) => format!("“{name}” {verb}"),
        None => format!("Ekranda {verb}"),
    };
    match step.action.as_str() {
        "click" | "doubleClick" | "rightClick" => on("tıklandı"),
        "tripleClick" => on("seçildi"),
        "type" => "Yazı yazıldı".to_string(),
        "setValue" => on("değeri ayarlandı"),
        "scroll" => on("kaydırıldı"),
        "dragDrop" => on("sürüklendi"),
        "move" => on("imleç götürüldü"),
        "menu" => format!(
            "“{}” menü komutu çalıştırıldı",
            menu_item(step).unwrap_or_default()
        ),
        "hotkey" => format!(
            "{} kısayolu basıldı",
            step.keys.as_deref().unwrap_or_default().join("+")
        ),
        _ => "Beklendi".to_string(),
    }
}

/// Shortcuts that send or delete at once.
pub(super) fn irreversible_keys(keys: &[String]) -> bool {
    let has = |name: &str| keys.iter().any(|key| key.eq_ignore_ascii_case(name));
    let enter = has("enter") || has("return");
    let delete = has("delete") || has("backspace");
    ((has("command") || has("control")) && enter) || ((has("command") || has("shift")) && delete)
}

/// The sentence on the person's card when a step needs their yes. Nothing has been sent when this is said.
pub(super) fn needs_a_yes(step: &Request, scene: &Scene) -> Option<String> {
    match step.action.as_str() {
        "click" | "doubleClick" | "tripleClick" => {
            let target = named_target(scene, step.target_ref.as_ref()).or_else(|| {
                screen_point(step.normalized_x, step.normalized_y, scene.screen)
                    .and_then(|point| control_at(point, scene))
            })?;
            cannot_be_taken_back(&target.name).then(|| {
                format!(
                    "{} uygulamasında “{}” düğmesine basılacak.",
                    scene.application, target.name
                )
            })
        }
        "menu" => {
            let item = menu_item(step)?;
            cannot_be_taken_back(item).then(|| {
                format!(
                    "{} uygulamasında “{item}” menü komutu çalıştırılacak.",
                    scene.application
                )
            })
        }
        "hotkey" => step
            .keys
            .as_deref()
            .filter(|keys| irreversible_keys(keys))
            .map(|keys| {
                format!(
                    "{} uygulamasında {} tuşlarına basılacak.",
                    scene.application,
                    keys.join("+")
                )
            }),
        _ => None,
    }
}
