//! The measurement set: everyday tasks done on real apps through the same `control` the server's steps go through, each with a check that does not
//! take the step's own word for it (a file that moved on disk, a field read again). It says, per task, whether it worked, how many commands it took
//! and how long. It takes the screen for a few minutes and opens TextEdit, Calculator, Finder, Safari and System Settings (Notepad, Calculator,
//! Explorer and Edge on Windows):
//!
//! `WIQO_AGENT_DESKTOP=<path to agent-desktop> cargo test -p wiqo-ops measure_everyday_tasks -- --ignored --nocapture`
//!
//! `WIQO_MEASURE_ONLY=textedit` runs the tasks whose name starts with that. The table is also written to `target/screen-tasks.json`.

use super::*;

struct Run<'a> {
    engine: &'a dyn Engine,
    task: String,
    commands: usize,
    /// What the last command answered.
    seen: Value,
    notes: Vec<String>,
}

impl Run<'_> {
    /// One command; `Ok` holds its data when it ended without an error code.
    fn act(&mut self, action: &str, extra: Value) -> std::result::Result<Value, String> {
        let mut value = json!({ "taskId": self.task, "action": action });
        for (key, item) in extra.as_object().into_iter().flatten() {
            value[key] = item.clone();
        }
        self.commands += 1;
        match control(self.engine, &json!({ "kind": "computerDesktop", "value": value })) {
            Ok(done) => {
                let data = done.data.unwrap_or(Value::Null);
                self.merge(&data);
                match done.error {
                    Some(error) => Err(format!("{}: {}", error.code, error.message)),
                    None => Ok(data),
                }
            }
            Err(error) => Err(format!("{}: {}", error.code, error.message)),
        }
    }

    /// The list the model would hold: a whole look replaces it, a step's changes are applied to it.
    fn merge(&mut self, data: &Value) {
        let value = &data["value"];
        let whole = value["targets"].as_array().cloned().unwrap_or_default();
        let mut list = if whole.is_empty() && (value["addedTargets"].is_array() || value["changedTargets"].is_array()) && value["action"] != "observe" {
            self.seen.as_array().cloned().unwrap_or_default()
        } else {
            whole
        };
        let gone: Vec<&str> = value["removedTargetRefs"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
        list.retain(|target| !gone.contains(&target["ref"].as_str().unwrap_or_default()));
        for changed in value["changedTargets"].as_array().into_iter().flatten() {
            if let Some(place) = list.iter().position(|target| target["ref"] == changed["ref"]) {
                list[place] = changed.clone();
            }
        }
        list.extend(value["addedTargets"].as_array().into_iter().flatten().cloned());
        self.seen = Value::Array(list);
    }

    fn look(&mut self, application: &str) -> std::result::Result<Value, String> {
        self.act("observe", json!({ "text": application }))
    }

    /// The ref of the first control of that kind whose name or content holds the words (all of them when the words are empty).
    fn find(&self, role: &str, words: &str) -> Option<String> {
        let wanted = plain(words);
        self.seen.as_array()?.iter().find_map(|target| {
            let fits = (role.is_empty() || target["role"] == role)
                && (wanted.is_empty()
                    || plain(target["name"].as_str().unwrap_or_default()).contains(&wanted)
                    || plain(target["value"].as_str().unwrap_or_default()).contains(&wanted));
            fits.then(|| target["ref"].as_str().unwrap_or_default().to_string())
        })
    }

    fn holds(&self, words: &str) -> bool {
        self.find("", words).is_some()
    }

    fn need(&self, role: &str, words: &str) -> std::result::Result<String, String> {
        self.find(role, words).ok_or_else(|| format!("no {role} “{words}” on the screen"))
    }
}

type Check = std::result::Result<bool, String>;

/// The real helper, measured without the rule that waits for the person's hands to leave the keyboard: the set is run on a computer nobody is using,
/// and its own input would otherwise read as theirs. `WIQO_MEASURE_TRACE=1` prints every command the helper refused.
struct Hands(Helper);

impl Engine for Hands {
    fn call(&self, args: &[String]) -> std::result::Result<Answer, String> {
        let answer = self.0.call(args);
        if std::env::var_os("WIQO_MEASURE_TRACE").is_some() {
            match &answer {
                Ok(answer) if !answer.ok => println!("    ✗ {} → {} {} ({:?})", args.join(" ").chars().take(110).collect::<String>(), answer.code, answer.message, answer.delivery),
                Err(error) => println!("    ✗ {} → {error}", args.join(" ").chars().take(110).collect::<String>()),
                Ok(answer) => println!("    ✓ {} ({:?})", args.join(" ").chars().take(110).collect::<String>(), answer.delivery),
            }
        }
        answer
    }

    fn frontmost(&self) -> Option<String> {
        crate::apps::frontmost()
    }
}

/// Opens the app and says what this computer calls it (a Turkish Mac calls Calculator "Hesap Makinesi", and the helper knows it by that name).
fn open_app(name: &str) -> String {
    if cfg!(windows) {
        let _ = Helper::locate().call(&["launch".into(), name.into(), "--activate".into()]);
    } else {
        let _ = Command::new("/usr/bin/open").args(["-a", name]).status();
    }
    std::thread::sleep(Duration::from_millis(1800));
    crate::apps::frontmost().filter(|front| !front.is_empty()).unwrap_or_else(|| name.to_string())
}

fn open_path(path: &std::path::Path, with: Option<&str>) {
    let mut command = if cfg!(windows) {
        let mut command = Command::new("cmd");
        command.args(["/C", "start", ""]);
        if let Some(app) = with {
            command.arg(app);
        }
        command
    } else {
        let mut command = Command::new("/usr/bin/open");
        if let Some(app) = with {
            command.args(["-a", app]);
        }
        command
    };
    let _ = command.arg(path).status();
    std::thread::sleep(Duration::from_millis(2500));
}

const FORM_PAGE: &str = r#"<!doctype html><html lang="tr"><meta charset="utf-8"><title>Wiqo Ölçüm Formu</title>
<body style="font: 16px sans-serif; margin: 40px">
<h1>Wiqo Ölçüm Formu</h1>
<form onsubmit="return false">
<p><label for="ad">Ad Soyad</label><br><input id="ad" type="text"></p>
<p><label for="posta">E-posta</label><br><input id="posta" type="email" value="eski@ornek.com"></p>
<p><label for="sehir">Şehir</label><br><input id="sehir" type="text"></p>
<p><input id="bulten" type="checkbox"> <label for="bulten">Bülten istiyorum</label></p>
<p><label for="not">Not</label><br><textarea id="not" rows="3" cols="40"></textarea></p>
<p><button type="reset">Temizle</button> <button type="submit">Gönder</button></p>
</form>
<div style="height: 4200px"></div>
<h2>Sayfanın Dibindeki Başlık</h2>
<p><button type="button" onclick="this.textContent='Basıldı'">Dipteki Düğme</button></p>
</body></html>"#;

/// The tasks, in an order that leaves each app as the next one needs it. A name's first word is its app.
type Task<'a> = (&'static str, Box<dyn Fn(&mut Run) -> Check + 'a>);

fn tasks(home: &std::path::Path) -> Vec<Task<'_>> {
    let windows = cfg!(windows);
    let editor = if windows { "Notepad" } else { "TextEdit" };
    let calculator = "Calculator";
    let files = if windows { "File Explorer" } else { "Finder" };
    let browser = if windows { "Microsoft Edge" } else { "Safari" };
    let settings = if windows { "Settings" } else { "System Settings" };
    let command = if windows { "control" } else { "command" };
    let lines: String = (1..=220).map(|line| format!("satir numara {line}\n")).collect();
    // What this computer calls each app, learned when the app is opened.
    let called: &'static Mutex<HashMap<&'static str, String>> = Box::leak(Box::new(Mutex::new(HashMap::new())));
    let name_of = move |app: &'static str| called.lock().unwrap().get(app).cloned().unwrap_or_else(|| app.to_string());
    let opened = move |app: &'static str| {
        let name = open_app(app);
        called.lock().unwrap().insert(app, name);
    };
    let mut list: Vec<Task<'_>> = Vec::new();

    list.push(("editor: yeni belge aç ve yaz", Box::new(move |run| {
        opened(editor);
        run.look(&name_of(editor))?;
        if !windows {
            run.act("hotkey", json!({ "keys": [command, "n"] }))?;
        }
        let typed = run.act("type", json!({ "text": "merhaba wiqo" }))?;
        Ok(typed["value"]["verified"] == true && run.holds("merhaba wiqo"))
    })));
    list.push(("editor: üç adımı tek çağrıda yap (seç, yaz, satır başı)", Box::new(move |run| {
        run.look(&name_of(editor))?;
        let done = run.act("runSteps", json!({ "steps": [
            { "action": "hotkey", "keys": [command, "a"] },
            { "action": "type", "text": "birinci satir" },
            { "action": "hotkey", "keys": ["enter"] },
            { "action": "type", "text": "ikinci satir" }
        ] }))?;
        run.notes.push(format!("stepsLeft={}", done["value"]["stepsLeft"]));
        run.look(&name_of(editor))?;
        Ok(done["value"]["stepsLeft"] == 0 && run.holds("birinci satir") && run.holds("ikinci satir"))
    })));
    list.push(("editor: alanı adıyla değil ref ile doldur ve geri oku", Box::new(move |run| {
        run.look(&name_of(editor))?;
        let area = run.find("textarea", "").or_else(|| run.find("textfield", "")).or_else(|| run.find("document", "")).ok_or("no text area")?;
        let filled = run.act("fillForm", json!({ "fields": [{ "targetRef": area, "value": "form ile yazildi" }] }))?;
        Ok(filled["value"]["verified"] == true && run.holds("form ile yazildi") && !run.holds("birinci satir"))
    })));
    list.push(("editor: satırı üç tıkla seç ve değiştir", Box::new(move |run| {
        run.look(&name_of(editor))?;
        let area = run.find("textarea", "").or_else(|| run.find("textfield", "")).or_else(|| run.find("document", "")).ok_or("no text area")?;
        run.act("tripleClick", json!({ "targetRef": area }))?;
        run.act("type", json!({ "text": "ustune yazildi" }))?;
        run.look(&name_of(editor))?;
        Ok(run.holds("ustune yazildi") && !run.holds("form ile yazildi"))
    })));
    list.push(("editor: uzun metni yapıştır", Box::new(move |run| {
        run.look(&name_of(editor))?;
        run.act("hotkey", json!({ "keys": [command, "a"] }))?;
        run.act("type", json!({ "text": lines.clone() }))?;
        run.look(&name_of(editor))?;
        Ok(run.holds("satir numara 220") || run.holds("satir numara 1"))
    })));
    list.push(("editor: yazı görünene kadar yukarı kaydır", Box::new(move |run| {
        run.look(&name_of(editor))?;
        let found = run.act("scroll", json!({ "direction": "up", "text": "satir numara 3", "amount": 12 }))?;
        Ok(found["value"]["verified"] == true)
    })));
    list.push(("editor: olmayan yazıyı ararken dürüstçe dur", Box::new(move |run| {
        run.look(&name_of(editor))?;
        match run.act("scroll", json!({ "direction": "down", "text": "bu metin belgede hic yok", "amount": 15 })) {
            Err(error) => Ok(error.starts_with("DESKTOP_TARGET_NOT_FOUND")),
            Ok(_) => Ok(false),
        }
    })));
    list.push(("editor: yazının belirmesini bekle", Box::new(move |run| {
        run.look(&name_of(editor))?;
        let waited = run.act("wait", json!({ "text": "satir numara", "amount": 3000 }))?;
        Ok(waited["value"]["verified"] == true)
    })));

    list.push(("hesap: düğmelere basarak 7 + 8 hesapla", Box::new(move |run| {
        opened(calculator);
        run.look(&name_of(calculator))?;
        run.act("hotkey", json!({ "keys": ["escape"] })).ok();
        for key in ["7", "+", "8", "="] {
            let named = match key {
                "+" => run.find("button", "topla").or_else(|| run.find("button", "add")).or_else(|| run.find("button", "plus")).or_else(|| run.find("button", "arti")),
                "=" => run.find("button", "esittir").or_else(|| run.find("button", "equal")),
                digit => run.seen.as_array().and_then(|list| list.iter().find(|target| target["role"] == "button" && target["name"] == digit))
                    .and_then(|target| target["ref"].as_str().map(str::to_string)),
            };
            run.act("click", json!({ "targetRef": named.ok_or_else(|| format!("no button for {key}"))? }))?;
        }
        run.look(&name_of(calculator))?;
        Ok(run.holds("15"))
    })));
    list.push(("hesap: klavyeyle 12 × 12 hesapla (tek çağrı)", Box::new(move |run| {
        run.look(&name_of(calculator))?;
        run.act("runSteps", json!({ "steps": [
            { "action": "hotkey", "keys": ["escape"] },
            { "action": "hotkey", "keys": ["1"] }, { "action": "hotkey", "keys": ["2"] },
            { "action": "hotkey", "keys": ["shift", "8"] },
            { "action": "hotkey", "keys": ["1"] }, { "action": "hotkey", "keys": ["2"] },
            { "action": "hotkey", "keys": ["enter"] }
        ] }))?;
        run.look(&name_of(calculator))?;
        Ok(run.holds("144"))
    })));
    list.push(("hesap: resimle bak (görsel yedek için ekran resmi)", Box::new(move |run| {
        let looked = run.act("look", json!({ "text": name_of(calculator), "sight": true }))?;
        let picture = &looked["value"]["picture"];
        run.notes.push(format!("{}x{} {} KB", picture["width"], picture["height"], picture["dataBase64"].as_str().map(str::len).unwrap_or(0) * 3 / 4 / 1024));
        Ok(picture["mimeType"] == "image/jpeg")
    })));

    list.push(("dosyalar: klasörü aç ve dosyaları gör", Box::new(move |run| {
        open_path(home, None);
        run.look(&name_of(files))?;
        Ok(run.holds("rapor taslak") && run.holds("hedef klasor"))
    })));
    list.push(("dosyalar: listeyi bir ada daralt", Box::new(move |run| {
        let narrowed = run.act("observe", json!({ "text": name_of(files), "label": "butce tablosu" }))?;
        let found = narrowed["value"]["targets"].as_array().map(Vec::len).unwrap_or(0);
        run.notes.push(format!("{found} hedef"));
        Ok((1..=6).contains(&found))
    })));
    list.push(("dosyalar: sağ tıkla, menü açılsın, kapat", Box::new(move |run| {
        run.look(&name_of(files))?;
        let file = run.need("", "rapor taslak")?;
        let before = run.seen.as_array().map(Vec::len).unwrap_or(0);
        run.act("rightClick", json!({ "targetRef": file }))?;
        let opened = run.seen.as_array().map(Vec::len).unwrap_or(0) > before || run.find("menuitem", "").is_some();
        run.act("hotkey", json!({ "keys": ["escape"] })).ok();
        Ok(opened)
    })));
    list.push(("dosyalar: dosyayı klasöre sürükle (diskte doğrula)", Box::new(move |run| {
        run.look(&name_of(files))?;
        let file = run.need("", "tasinacak dosya")?;
        let folder = run.need("", "hedef klasor")?;
        run.act("dragDrop", json!({ "sourceRef": file, "destinationRef": folder, "durationMs": 700, "holdMs": 800 }))?;
        std::thread::sleep(Duration::from_millis(800));
        Ok(home.join("hedef klasor").join("tasinacak dosya.txt").exists() && !home.join("tasinacak dosya.txt").exists())
    })));

    list.push(("form: alanları adlarıyla tek çağrıda doldur", Box::new(move |run| {
        open_path(&home.join("form.html"), Some(browser));
        run.look(&name_of(browser))?;
        // The page takes a moment to be there after the window is.
        run.act("wait", json!({ "text": "Ad Soyad", "amount": 8000 })).ok();
        run.look(&name_of(browser))?;
        let filled = run.act("fillForm", json!({ "fields": [
            { "label": "Ad Soyad", "value": "Ayşe Yılmaz" },
            { "label": "E-posta", "value": "ayse@ornek.com" },
            { "label": "Şehir", "value": "İzmir" },
            { "label": "Bülten istiyorum", "value": "evet" },
            { "label": "Not", "value": "Öğleden sonra arayın." }
        ] }))?;
        run.notes.push(format!("verified={} evidence={}", filled["value"]["verified"], filled["value"]["evidence"]));
        run.look(&name_of(browser))?;
        Ok(run.holds("Ayşe Yılmaz") && run.holds("ayse@ornek.com") && run.holds("İzmir") && !run.holds("eski@ornek.com"))
    })));
    list.push(("form: geri alınamayan düğme onay istesin", Box::new(move |run| {
        run.look(&name_of(browser))?;
        let send = run.need("button", "Gönder")?;
        match run.act("click", json!({ "targetRef": send })) {
            Err(error) => Ok(error.starts_with("DESKTOP_APPROVAL_REQUIRED")),
            Ok(_) => Ok(false),
        }
    })));
    list.push(("form: sayfanın dibindeki başlığa kadar kaydır ve düğmeye bas", Box::new(move |run| {
        run.look(&name_of(browser))?;
        let found = run.act("scroll", json!({ "direction": "down", "text": "Dipteki Düğme", "amount": 15 }))?;
        if found["value"]["verified"] != true {
            return Ok(false);
        }
        let button = run.need("button", "Dipteki Düğme")?;
        run.act("click", json!({ "targetRef": button }))?;
        run.look(&name_of(browser))?;
        Ok(run.holds("Basıldı"))
    })));
    list.push(("form: temizle düğmesi alanları boşaltsın", Box::new(move |run| {
        run.look(&name_of(browser))?;
        run.act("scroll", json!({ "direction": "up", "text": "Temizle", "amount": 15 }))?;
        let reset = run.need("button", "Temizle")?;
        run.act("click", json!({ "targetRef": reset }))?;
        run.look(&name_of(browser))?;
        Ok(!run.holds("Ayşe Yılmaz"))
    })));

    list.push(("ayarlar: pencereyi oku", Box::new(move |run| {
        opened(settings);
        let looked = run.look(&name_of(settings))?;
        let seen = looked["value"]["targets"].as_array().map(Vec::len).unwrap_or(0);
        run.notes.push(format!("{seen} hedef, {} daha", looked["value"]["moreTargets"]));
        Ok(seen >= 5)
    })));
    list.push(("ayarlar: arama alanına yaz ve geri oku", Box::new(move |run| {
        run.look(&name_of(settings))?;
        let search = run.find("searchfield", "").or_else(|| run.find("textfield", "")).ok_or("no search field")?;
        let filled = run.act("fillForm", json!({ "fields": [{ "targetRef": search, "value": "wifi" }] }))?;
        Ok(filled["value"]["verified"] == true)
    })));
    list
}

#[test]
#[ignore]
fn measure_everyday_tasks() {
    if std::env::var_os("WIQO_AGENT_DESKTOP").is_none() {
        println!("WIQO_AGENT_DESKTOP is not set: nothing measured.");
        return;
    }
    let only = std::env::var("WIQO_MEASURE_ONLY").unwrap_or_default();
    let home = std::env::temp_dir().join(format!("wiqo-olcum-{}", &uuid::Uuid::new_v4().to_string()[..8]));
    std::fs::create_dir_all(home.join("hedef klasor")).unwrap();
    for name in ["rapor taslak.txt", "butce tablosu.txt", "tasinacak dosya.txt"] {
        std::fs::write(home.join(name), "wiqo ölçüm dosyası\n").unwrap();
    }
    std::fs::write(home.join("form.html"), FORM_PAGE).unwrap();
    let engine = Hands(Helper::locate());
    let mut rows = Vec::new();
    for (name, task) in tasks(&home) {
        if !only.is_empty() && !name.starts_with(only.as_str()) {
            continue;
        }
        let mut run = Run {
            engine: &engine,
            task: format!("olcum-{}", uuid::Uuid::new_v4()),
            commands: 0,
            seen: Value::Null,
            notes: Vec::new(),
        };
        let began = Instant::now();
        let outcome = task(&mut run);
        let seconds = (began.elapsed().as_secs_f64() * 10.0).round() / 10.0;
        let (ok, why) = match outcome {
            Ok(true) => (true, String::new()),
            Ok(false) => (false, "sonuç beklenen gibi değil".to_string()),
            Err(error) => (false, error),
        };
        println!("{} {name} | {} komut | {seconds} sn {} {}", if ok { "GEÇTİ " } else { "KALDI " }, run.commands, why, run.notes.join("; "));
        rows.push(json!({ "task": name, "ok": ok, "commands": run.commands, "seconds": seconds, "why": why, "notes": run.notes }));
    }
    let passed = rows.iter().filter(|row| row["ok"] == true).count();
    let seconds: f64 = rows.iter().filter_map(|row| row["seconds"].as_f64()).sum();
    let commands: u64 = rows.iter().filter_map(|row| row["commands"].as_u64()).sum();
    println!("\n{passed}/{} görev geçti, {commands} komut, {seconds:.0} sn (komut başına {:.1} sn)", rows.len(), seconds / commands.max(1) as f64);
    let report = json!({ "platform": std::env::consts::OS, "passed": passed, "total": rows.len(), "seconds": seconds, "commands": commands, "tasks": rows });
    let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/screen-tasks.json");
    let _ = std::fs::write(&out, serde_json::to_string_pretty(&report).unwrap());
    println!("rapor: {}", out.display());
    if !cfg!(windows) {
        let _ = Command::new("/usr/bin/osascript")
            .args(["-e", "tell application \"Finder\" to close (every Finder window whose name begins with \"wiqo-olcum\")"])
            .status();
    }
    println!("ölçüm dosyaları: {} (silinmedi; açık pencereleri kapatınca silebilirsin)", home.display());
}
