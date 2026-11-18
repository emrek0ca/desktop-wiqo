//! The desktop's own agent: PC work runs here, the way a coding agent runs on the developer's machine. The loop is local (model call → this computer's
//! tools → result → model call), the tools are the terminal session's own (rova-free: the same grants, policy, undo and full-access switch), and the only
//! thing that leaves the computer is each model call, through the server's gateway (`/agent/model`) with the run id the chat turn handed over.
//! Nothing waits on a round trip per step, and nothing depends on the chat's stream staying open.

use std::path::{Path, PathBuf};
use std::time::Instant;

use super::*;

/// How many model turns, and for how long, one request may run (the server's own bound is six hours and 600 model calls).
const MAX_TURNS: usize = 400;
const MAX_MINUTES: u64 = 345;
/// What one tool result may give the model.
const OUTPUT_CHARS: usize = 14_000;
/// What the working context may weigh (bytes of its JSON) before it is shortened: the model reads about 130 000 tokens, and the answer and the tools need
/// their share. Past it, old results are shortened first; if that is not enough, the older part of the work is replaced by a summary of it.
const CONTEXT_BYTES: usize = 280_000;
const CONTEXT_BYTES_LEAST: usize = 80_000;
const KEEP_FULL_RESULTS: usize = 8;
/// How long a model that is busy or out of reach is waited for before the run stops (seconds between tries; about twelve minutes in all).
const MODEL_WAITS: [u64; 12] = [2, 5, 10, 20, 30, 60, 60, 60, 120, 120, 120, 120];
/// How often the model may be turned back when it says "done" with work visibly open, and how many failures in a row end the run.
const GATE_TURNS: usize = 2;
const FAILURES_NOTED: usize = 8;
const FAILURES_MOST: usize = 24;
/// A session of a conversation is picked up again within this many days.
const SESSION_DAYS: i64 = 7;

/// How the loop itself works, said to the model once: what it can rely on and what is expected at the end.
const NOTES: &str = "UZUN İŞ\n- Üç adımdan uzun işte önce plan aracıyla adımlarını yaz, ilerledikçe güncelle. İş bitmeden \"bitti\" deme: planındaki her adım done ya da dropped olmalı.\n- Uzun çalışmada eski araç sonuçları kısaltılır ve eski bölüm özetlenir; önemli bulduğun şeyi (yol, sürüm, karar) planına ya da kısa bir cümleyle yanıtına yaz, sonradan lazım olanı dosyadan yeniden oku.\n- Aynı çağrı iki kez aynı hatayı verdiyse üçüncü kez deneme; hatanın nedenini araştır.\n- Bu sohbette daha önce yaptığın iş (yazdığın dosyalar, çalıştırdığın komutlar) yukarıdaki kayıtta durur; \"devam et\" denirse oradan sürdür, baştan başlama.\n\nYAZILIM İŞİ\n- Bir projede çalışırken her komutta workdir olarak proje klasörünü ver. Proje klasörüne ilk dokunuşunda sana PROJE özeti gelir (dosyalar, betikler, git durumu, kuralları, notların); oradaki kurallara uy.\n- Büyük kod tabanını baştan okuma: önce search ile yeri bul, sonra read_file ile o satır aralığını oku.\n- Değişikliği küçük yap (edit_file, birden çok dosyada apply_patch), sonra projenin kendi test/derleme/lint komutunu çalıştır; kırılırsa çıktıyı oku ve düzelt. Test etmediğin şeye \"çalışıyor\" deme.\n- Bir değişikliği geri almak gerekirse undo aracını, yazma sonucunda dönen undo_id ile kullan.\n- Git: kullanıcı istemedikçe commit, push, reset, dal silme yapma. Commit istenirse önce git status/diff ile neyi kaydettiğini gör.\n- Hata mesajı, paket belgesi, sürüm gibi bilmediğin şeyi web aracıyla ara ya da sayfayı oku; tahmin etme.\n- Arayüz yaptıysan açıp ekran araçlarıyla (computer_interface observe) gerçekten göründüğünü doğrula.\n- Projede sonradan işe yarayacak şeyi (çalıştırma/test komutu, önemli yol, karar) project_notes ile projenin notuna yaz; bir sonraki işte PROJE özetinde gelir.\n- sudo yönetici şifresi ister ve Wiqo şifre giremez; sudo'suz yol seç (brew, kullanıcı klasörüne kurulum) ya da komutu kullanıcıya ver.";

/// Said only when this computer's tools beyond the terminal are on offer.
const PC_NOTES: &str = "EKRAN VE SİSTEM ARAÇLARI\n- Dosya, komut, kurulum, kod işlerini terminal araçlarınla yap; onlar daha hızlı ve kesin.\n- computer_interface: yalnız ekranda görmek, tıklamak, yazmak gereken işler (bir uygulamanın penceresi, düğmesi, formu). Önce observe, sonra gözlemdeki ref'lerle çalış.\n- computer_files: terminalin yapamadığı dosya işleri (telefona gönder: send_to_phone, PDF'e çevir, Word/Excel/PDF içeriğini oku, çöpten geri al).\n- computer_system: ses, parlaklık, medya, pano, uygulama açma/kapama, ekran görüntüsü. Ekran görüntüsü bu çalışmada telefona kendiliğinden gitmez: saveTo ile dosyaya kaydet, sonra computer_files send_to_phone ile gönder.\n- Bu araçlardan biri reddederse nedenini oku; aynı işi terminalle yapabiliyorsan öyle yap.";

fn tools() -> Value {
    json!([
        { "type": "function", "function": { "name": "shell", "description": "Bu bilgisayarda bir kabuk komutu çalıştırır (macOS zsh, Windows PowerShell). workdir vermezsen ev klasöründe ya da son klasörde çalışır. Kurulum, derleme, indirme gibi uzun komutlar kendiliğinden arka plan işi olur ve job_id döner.",
            "parameters": { "type": "object", "properties": {
                "command": { "type": "string" },
                "workdir": { "type": "string", "description": "Tam klasör yolu, ör. ~/Desktop/proje" },
                "timeout_seconds": { "type": "integer", "description": "En çok 50; daha uzun işler kendiliğinden arka plana alınır" }
            }, "required": ["command"] } } },
        { "type": "function", "function": { "name": "job", "description": "Arka plan işi: wait (bitene ya da süre dolana kadar bekle ve çıktıyı oku), status, input (çalışan işe girdi yaz), cancel.",
            "parameters": { "type": "object", "properties": {
                "action": { "type": "string", "enum": ["wait", "status", "input", "cancel"] },
                "job_id": { "type": "string" },
                "text": { "type": "string" },
                "wait_seconds": { "type": "integer", "description": "5-45 arası" }
            }, "required": ["action", "job_id"] } } },
        { "type": "function", "function": { "name": "read_file", "description": "Bir metin dosyasını okur (isteğe bağlı satır aralığı).",
            "parameters": { "type": "object", "properties": {
                "path": { "type": "string" }, "start_line": { "type": "integer" }, "end_line": { "type": "integer" }
            }, "required": ["path"] } } },
        { "type": "function", "function": { "name": "write_file", "description": "Dosyayı tam içeriğiyle yazar; klasörü yoksa oluşturur. Var olan dosyayı baştan yazar. append: true ise içerik dosyanın sonuna eklenir (çok uzun dosyayı parça parça yazmak için: ilk parça normal, sonrakiler append).",
            "parameters": { "type": "object", "properties": { "path": { "type": "string" }, "content": { "type": "string" }, "append": { "type": "boolean" } }, "required": ["path", "content"] } } },
        { "type": "function", "function": { "name": "edit_file", "description": "Var olan dosyada bir metni başka bir metinle değiştirir; find dosyada tam ve tek olmalı (yoksa replace_all).",
            "parameters": { "type": "object", "properties": {
                "path": { "type": "string" }, "find": { "type": "string" }, "replace": { "type": "string" }, "replace_all": { "type": "boolean" }
            }, "required": ["path", "find", "replace"] } } },
        { "type": "function", "function": { "name": "apply_patch", "description": "Birden çok dosyada aynı anda değişiklik: her dosya için unified diff hunk'ları (@@ ile başlayan). Dosyalar workdir içinde var olmalı; biri uymazsa hiçbiri değişmez. En çok 10 dosya.",
            "parameters": { "type": "object", "properties": {
                "workdir": { "type": "string", "description": "Proje klasörü, ör. ~/Desktop/proje" },
                "edits": { "type": "array", "items": { "type": "object", "properties": {
                    "path": { "type": "string", "description": "workdir'e göre yol" }, "patch": { "type": "string" }
                }, "required": ["path", "patch"] } }
            }, "required": ["workdir", "edits"] } } },
        { "type": "function", "function": { "name": "undo", "description": "write_file, edit_file ya da apply_patch'in yaptığı değişikliği geri alır (sonuçtaki undo_id ile).",
            "parameters": { "type": "object", "properties": { "undo_id": { "type": "string" } }, "required": ["undo_id"] } } },
        { "type": "function", "function": { "name": "web", "description": "İnternette arar (query) ya da bir sayfayı metin olarak okur (url). Hata mesajı, belge, sürüm, örnek kod için.",
            "parameters": { "type": "object", "properties": { "query": { "type": "string" }, "url": { "type": "string" } } } } },
        { "type": "function", "function": { "name": "project_notes", "description": "Bir projenin Wiqo notunu baştan yazar (yalnız bu bilgisayarda durur): çalıştırma/test komutları, önemli yollar, kararlar. Sonraki işlerde PROJE özetinde gelir. Var olan notu koruyup ekleyerek tamamını gönder.",
            "parameters": { "type": "object", "properties": { "project": { "type": "string", "description": "Proje klasörü" }, "content": { "type": "string" } }, "required": ["project", "content"] } } },
        { "type": "function", "function": { "name": "search", "description": "Bir klasörde dosya içeriğinde metin ya da regex arar.",
            "parameters": { "type": "object", "properties": {
                "pattern": { "type": "string" }, "path": { "type": "string", "description": "Aranacak klasör; yoksa ev klasörü" }, "glob": { "type": "string" }
            }, "required": ["pattern"] } } },
        { "type": "function", "function": { "name": "plan", "description": "Üç adımdan uzun işlerde yapılacaklar listen. İşe başlarken adımları yaz; bir adım bitince ya da plan değişince listenin tamamını güncel durumlarıyla yeniden gönder. Kullanıcı bu listeyi görür ve iş uzadığında sen de buradan kaldığın yeri bulursun.",
            "parameters": { "type": "object", "properties": {
                "steps": { "type": "array", "description": "En çok 20 adım", "items": { "type": "object", "properties": {
                    "title": { "type": "string" }, "status": { "type": "string", "enum": ["pending", "doing", "done", "dropped"] }
                }, "required": ["title", "status"] } }
            }, "required": ["steps"] } } }
    ])
}

/// The note a rejected tool call leaves for the model; it is taken out again once a call goes through.
const MALFORMED_NOTE: &str = "Son araç çağrın sağlayıcı tarafından reddedildi ve çalıştırılmadı.";

fn is_malformed_note(message: &Value) -> bool {
    message["role"] == "user"
        && message["content"].as_str().is_some_and(|text| text.starts_with(MALFORMED_NOTE) || text.starts_with("Son araç çağrın bozuk biçimde yazıldı"))
}

/// "~/x" and absolute paths as a real path; a relative one stays relative.
fn expand(path: &str) -> PathBuf {
    let path = path.trim();
    if path == "~" {
        return dirs::home_dir().unwrap_or_default();
    }
    if let Some(rest) = path.strip_prefix("~/").or_else(|| path.strip_prefix("~\\")) {
        return dirs::home_dir().unwrap_or_default().join(rest);
    }
    PathBuf::from(path)
}

/// A file named by its full path is opened in its own folder: the session's grant covers that folder.
fn file_and_folder(path: &str) -> (String, Option<String>) {
    let full = expand(path);
    if full.is_absolute() {
        let folder = full.parent().map(|parent| parent.to_string_lossy().to_string());
        let name = full.file_name().map(|name| name.to_string_lossy().to_string()).unwrap_or_default();
        return (name, folder);
    }
    (path.to_string(), None)
}

fn environment() -> String {
    let os = match std::env::consts::OS {
        "macos" => "macOS (zsh)",
        "windows" => "Windows (PowerShell)",
        other => other,
    };
    let home = dirs::home_dir().map(|home| home.to_string_lossy().to_string()).unwrap_or_default();
    let access = if wiqo_ops::full_access() {
        "Tam yetki açık: komutlar ve dosya değişiklikleri sorulmadan çalışır."
    } else {
        "Sorarak modu: değiştiren her komut ve dosya yazımı kullanıcıya sorulur; reddedilirse başka yol öner."
    };
    format!(
        "ORTAM\n- İşletim sistemi: {os}\n- Ev klasörü: {home}\n- Şimdi: {}\n- {access}\n\n{NOTES}",
        chrono::Local::now().format("%Y-%m-%d %H:%M")
    )
}

fn text_of(value: &Value) -> String {
    value.as_str().unwrap_or("").to_string()
}

/// A long text keeps its start and, with the larger share, its end: a build says what went wrong in its last lines.
fn clip(text: &str, most: usize) -> String {
    if text.chars().count() <= most {
        return text.to_string();
    }
    let head: String = text.chars().take(most / 3).collect();
    let tail: String = text.chars().rev().take(most - most / 3).collect::<Vec<_>>().into_iter().rev().collect();
    format!("{head}\n…(arası kısaltıldı)…\n{tail}")
}

fn weight(messages: &[Value]) -> usize {
    messages.iter().map(|message| message.to_string().len()).sum()
}

/// An old tool call keeps what it was (which file, which command) and drops what it carried (a whole file's content).
fn lighten_call(call: &mut Value) {
    let Some(raw) = call["function"]["arguments"].as_str() else { return };
    if raw.len() < 1_200 {
        return;
    }
    let mut args = serde_json::from_str::<Value>(raw).unwrap_or_else(|_| json!({}));
    if let Some(object) = args.as_object_mut() {
        for value in object.values_mut() {
            if let Some(text) = value.as_str().filter(|text| text.len() > 400) {
                *value = json!(format!("(kısaltıldı: {} karakter)", text.chars().count()));
            }
        }
    }
    call["function"]["arguments"] = json!(args.to_string());
}

/// Everything before the last `keep` tool results is made light: results keep their first and last lines, calls lose their long arguments.
fn shorten_old(messages: &mut [Value], keep: usize) {
    let tools: Vec<usize> = messages.iter().enumerate().filter(|(_, message)| message["role"] == "tool").map(|(index, _)| index).collect();
    let boundary = tools.get(tools.len().saturating_sub(keep)).copied().unwrap_or(messages.len());
    for message in &mut messages[..boundary] {
        if message["role"] == "tool" {
            let content = text_of(&message["content"]);
            if content.len() > 700 {
                message["content"] = json!(format!("{} (eski sonuç kısaltıldı)", clip(&content, 500)));
            }
        } else if let Some(calls) = message.get_mut("tool_calls").and_then(Value::as_array_mut) {
            calls.iter_mut().for_each(lighten_call);
        }
    }
}

/// Where the part that stays whole begins: the newest messages that fit `budget`, never starting on a result whose call is gone.
fn tail_start(messages: &[Value], budget: usize) -> usize {
    let mut total = 0;
    let mut start = messages.len();
    while start > 1 {
        let size = messages[start - 1].to_string().len();
        if total + size > budget && start < messages.len() {
            break;
        }
        total += size;
        start -= 1;
    }
    while start < messages.len() && messages[start]["role"] == "tool" {
        start += 1;
    }
    start
}

/// The work so far as a reader would follow it: what was asked, what the agent said and called, what came back.
fn transcript(messages: &[Value]) -> String {
    let mut lines = Vec::new();
    for message in messages {
        match message["role"].as_str().unwrap_or("") {
            "user" => lines.push(format!("KULLANICI: {}", clip(&text_of(&message["content"]), 2_000))),
            "assistant" => {
                let said = text_of(&message["content"]);
                if !said.trim().is_empty() {
                    lines.push(format!("AJAN: {}", clip(&said, 1_200)));
                }
                for call in message["tool_calls"].as_array().map(Vec::as_slice).unwrap_or(&[]) {
                    lines.push(format!("  ARAÇ {}({})", text_of(&call["function"]["name"]), clip(&text_of(&call["function"]["arguments"]), 300)));
                }
            }
            "tool" => lines.push(format!("  SONUÇ: {}", clip(&text_of(&message["content"]), 700))),
            _ => {}
        }
    }
    clip(&lines.join("\n"), 150_000)
}

/// Every call has its result and every result its call: a session cut off in the middle of its tools (the app closed) is made whole before it is sent.
fn repair(messages: Vec<Value>) -> Vec<Value> {
    let lost = |id: &str| json!({ "role": "tool", "tool_call_id": id, "content": json!({ "status": "unknown", "message": "Uygulama kapandığı için bu çağrının sonucu bilinmiyor; yapıldı mı diye kontrol et." }).to_string() });
    let mut whole: Vec<Value> = Vec::new();
    let mut waiting: Vec<String> = Vec::new();
    for message in messages {
        let role = message["role"].as_str().unwrap_or("").to_string();
        if role == "tool" {
            let id = text_of(&message["tool_call_id"]);
            if let Some(at) = waiting.iter().position(|open| *open == id) {
                waiting.remove(at);
                whole.push(message);
            }
            continue;
        }
        for id in waiting.drain(..) {
            whole.push(lost(&id));
        }
        if !matches!(role.as_str(), "user" | "assistant") {
            continue;
        }
        if let Some(calls) = message["tool_calls"].as_array() {
            waiting = calls.iter().map(|call| text_of(&call["id"])).collect();
        }
        whole.push(message);
    }
    for id in waiting.drain(..) {
        whole.push(lost(&id));
    }
    whole
}

/// The chat's last turns that the working context does not hold yet, as plain messages. A turn is known when a message of the same role begins the
/// same way (the session keeps a request with its goal added, and a long answer shortened).
fn said_since(known: &[Value], taken: &mut Vec<(String, String)>, history: &Value) -> Vec<Value> {
    let begins = |text: &str| text.trim().chars().take(160).collect::<String>();
    let mut held: Vec<(String, String)> = known.iter().filter(|message| message["tool_calls"].is_null()).map(|message| (text_of(&message["role"]), begins(&text_of(&message["content"])))).collect();
    held.extend(taken.iter().cloned());
    let mut new = Vec::new();
    for item in history.as_array().map(|items| &items[items.len().saturating_sub(10)..]).unwrap_or(&[]) {
        let role = text_of(&item["role"]);
        let content = text_of(&item["content"]);
        let begin = begins(&content);
        if !matches!(role.as_str(), "user" | "assistant") || begin.is_empty() || held.iter().any(|(by, text)| *by == role && text.starts_with(&begin)) {
            continue;
        }
        new.push(json!({ "role": role, "content": clip(&content, 4_000) }));
        held.push((role.clone(), begin.clone()));
        taken.push((role, begin));
    }
    let most = 80;
    if taken.len() > most {
        taken.drain(..taken.len() - most);
    }
    new
}

fn plan_text(plan: &[Value]) -> String {
    if plan.is_empty() {
        return "(plan yazılmadı)".into();
    }
    plan.iter()
        .map(|step| {
            let mark = match step["status"].as_str().unwrap_or("pending") {
                "done" => "[x]",
                "doing" => "[>]",
                "dropped" => "[-]",
                _ => "[ ]",
            };
            format!("{mark} {}", text_of(&step["title"]))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn plan_open(plan: &[Value]) -> Vec<String> {
    plan.iter().filter(|step| matches!(step["status"].as_str().unwrap_or("pending"), "pending" | "doing")).map(|step| text_of(&step["title"])).collect()
}

/// What the person reads for a tool call while it runs.
fn label(name: &str, args: &Value) -> String {
    let one = |key: &str| text_of(&args[key]).chars().take(120).collect::<String>();
    match name {
        "shell" => format!("Komut: {}", one("command")),
        "job" => format!("Arka plan işi: {}", one("action")),
        "read_file" => format!("Okuyor: {}", one("path")),
        "write_file" if args["append"] == true => format!("Ekliyor: {}", one("path")),
        "write_file" => format!("Yazıyor: {}", one("path")),
        "edit_file" => format!("Düzenliyor: {}", one("path")),
        "search" => format!("Arıyor: {}", one("pattern")),
        "apply_patch" => format!("Yama: {}", one("workdir")),
        "undo" => "Geri alıyor".into(),
        "web" => format!("Web: {}", [one("query"), one("url")].into_iter().find(|text| !text.is_empty()).unwrap_or_default()),
        "project_notes" => format!("Proje notu: {}", one("project")),
        "plan" => "Plan".into(),
        "computer_interface" => format!("Ekran: {}", one("operation")),
        "computer_files" => format!("Dosya ({}): {}", one("operation"), [one("path"), one("query")].into_iter().find(|text| !text.is_empty()).unwrap_or_default()),
        "computer_system" => format!("Sistem: {}", one("operation")),
        other => other.to_string(),
    }
}

/// A step's data as the model reads it: the terminal's own fields, each long text cut to its start and end.
fn shown_output(data: &Value) -> String {
    let mut value = if data["kind"] == "terminalResult" { data["value"].clone() } else { data.clone() };
    if let Some(object) = value.as_object_mut() {
        object.remove("workingDirectory");
        if let Some(undo) = object.remove("undoId").filter(|undo| undo.is_string()) {
            object.insert("undo_id".into(), undo);
        }
        for field in object.values_mut() {
            if let Some(text) = field.as_str().filter(|text| text.len() > OUTPUT_CHARS) {
                *field = json!(clip(text, OUTPUT_CHARS));
            }
        }
    }
    match value.as_str() {
        Some(text) => clip(text, OUTPUT_CHARS),
        None => clip(&value.to_string(), OUTPUT_CHARS * 2),
    }
}

/// The working state of one request: what the model is sent, its plan, and a line per step for when the model cannot be asked for a summary.
struct Work {
    messages: Vec<Value>,
    plan: Vec<Value>,
    log: Vec<String>,
    goal: String,
    budget: usize,
    /// Every tool on offer in this run: the terminal's own and, when the server described them, this computer's others.
    tools: Value,
    pc: Pc,
    /// Files this run sent to the phone ({stepId, artifactId}): they are shown with its answer.
    sent: Vec<Value>,
    /// Turns of the chat this computer's record already took in (role and first words), so a turn summarised away is not added a second time.
    held: Vec<(String, String)>,
    reach: Reach,
}

/// What this run may do without asking again, and what it already knows of the projects it works in.
#[derive(Default)]
struct Reach {
    /// Folders the person said yes for in "Sorarak" mode: a change inside one is not asked again in this run.
    allowed: Vec<PathBuf>,
    /// Projects whose brief the model already has.
    briefed: Vec<PathBuf>,
    /// The folder each change's undo record belongs to (undo id, folder): an undo runs under the same folder's grant.
    undo: Vec<(String, String)>,
    /// The run started from someone else's Mission (the handoff says `untrusted`): every change is asked, full access or not, and no yes carries over.
    untrusted: bool,
}

/// What marks the root of a project.
const PROJECT_MARKERS: &[&str] = &[
    ".git", "package.json", "Cargo.toml", "pyproject.toml", "go.mod", "Package.swift", "pom.xml", "build.gradle", "build.gradle.kts", "Gemfile",
    "composer.json", "deno.json", "pubspec.yaml", "requirements.txt", "Makefile", "CMakeLists.txt",
];

/// A path with no ".." in it: a folder rule read on its text must not be walked out of.
fn plain(path: &Path) -> bool {
    !path.components().any(|part| part == std::path::Component::ParentDir)
}

/// The nearest folder at or above `folder` that is a project, below the home folder.
fn project_root(folder: &Path) -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    let mut current = Some(folder);
    while let Some(dir) = current {
        if dir == home || !dir.starts_with(&home) {
            return None;
        }
        if PROJECT_MARKERS.iter().any(|marker| dir.join(marker).exists()) {
            return Some(dir.to_path_buf());
        }
        current = dir.parent();
    }
    None
}

/// The folder a call works in, when it names one by a full path.
fn touched_folder(name: &str, args: &Value) -> Option<PathBuf> {
    let folder = match name {
        "shell" | "apply_patch" => args["workdir"].as_str().filter(|dir| !dir.trim().is_empty()).map(expand),
        "read_file" | "write_file" | "edit_file" => expand(&text_of(&args["path"])).parent().map(Path::to_path_buf),
        "search" => args["path"].as_str().filter(|dir| !dir.trim().is_empty()).map(expand),
        _ => None,
    }?;
    (folder.is_absolute() && plain(&folder)).then(|| std::fs::canonicalize(&folder).unwrap_or(folder))
}

/// What a yes for a change in `folder` covers for the rest of the run: its project, or the folder itself; never the home folder or a folder right
/// under it (Desktop, Downloads, Documents hold too much to allow at once).
fn allowable(folder: &Path) -> Option<PathBuf> {
    let home = std::fs::canonicalize(dirs::home_dir()?).ok()?;
    let scope = project_root(folder).unwrap_or_else(|| folder.to_path_buf());
    let depth = scope.strip_prefix(&home).ok()?.components().count();
    (depth >= 2).then_some(scope)
}

/// A command that reaches past the folder (the network, other processes, other apps): asked every time, whatever was allowed.
fn reaches_out(command: &str) -> bool {
    let lower = command.to_lowercase();
    let words: Vec<&str> = lower.split(|c: char| c.is_whitespace() || ";&|()".contains(c)).filter(|word| !word.is_empty()).collect();
    let has = |word: &str| words.contains(&word);
    (has("git") && (has("push") || has("reset") || has("clean"))) || (has("publish") && (has("npm") || has("pnpm") || has("yarn") || has("cargo")))
        || ["ssh", "scp", "rsync", "kill", "pkill", "killall", "osascript", "open", "gh", "sudo", "launchctl", "defaults", "crontab"].iter().any(|word| has(word))
        || ((has("curl") || has("wget")) && ["-x", "--request", "-d", "--data", "-f", "--form", "-t", "--upload-file"].iter().any(|flag| has(flag)))
        || (has("docker") && has("push"))
}

/// sudo asks for the person's password, which nobody types here: it runs only when it would not ask (a cached or passwordless sudo).
fn sudo_would_ask(command: &str) -> bool {
    let named = command.split(|c: char| c.is_whitespace() || ";&|(".contains(c)).any(|word| word == "sudo");
    if !named || cfg!(windows) {
        return false;
    }
    !std::process::Command::new("sudo")
        .args(["-n", "true"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn head(path: &Path, lines: usize, most: usize) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let text: String = text.lines().take(lines).collect::<Vec<_>>().join("\n");
    (!text.trim().is_empty()).then(|| text.chars().take(most).collect())
}

/// What the model is told the first time it touches a project: what it is built with, how it is run and tested, where git stands, its own rules and
/// what earlier work noted about it. Read from the files themselves, so it is never stale.
fn project_brief(root: &Path, notes: Option<String>) -> String {
    let mut parts = vec![format!("PROJE: {}", root.display())];
    let markers: Vec<&str> = PROJECT_MARKERS.iter().copied().filter(|marker| root.join(marker).exists()).collect();
    parts.push(format!("İşaretler: {}", markers.join(", ")));
    let manager = [("pnpm-lock.yaml", "pnpm"), ("yarn.lock", "yarn"), ("bun.lockb", "bun"), ("bun.lock", "bun"), ("package-lock.json", "npm")]
        .iter()
        .find(|(lock, _)| root.join(lock).exists())
        .map(|(_, manager)| *manager);
    if let Some(package) = std::fs::read_to_string(root.join("package.json")).ok().and_then(|text| serde_json::from_str::<Value>(&text).ok()) {
        if let Some(scripts) = package["scripts"].as_object() {
            let lines: Vec<String> = scripts.iter().take(20).map(|(name, command)| format!("  {name}: {}", text_of(command).chars().take(120).collect::<String>())).collect();
            parts.push(format!("Betikler ({} run <ad>):\n{}", manager.unwrap_or("npm"), lines.join("\n")));
        }
    }
    if root.join(".git").exists() {
        let status = std::process::Command::new("git").args(["-C", &root.to_string_lossy(), "status", "--porcelain=v1", "-b"]).stdin(std::process::Stdio::null()).output();
        if let Some(out) = status.ok().filter(|out| out.status.success()) {
            let text = String::from_utf8_lossy(&out.stdout);
            let mut lines = text.lines();
            let branch = lines.next().unwrap_or("").trim_start_matches("## ").to_string();
            let changed = lines.count();
            parts.push(format!("Git: {branch}; {changed} değişmiş dosya"));
        }
    }
    if let Some((name, rules)) = ["AGENTS.md", "CLAUDE.md", ".cursorrules", "CONTRIBUTING.md"].iter().find_map(|name| head(&root.join(name), 60, 3_000).map(|text| (*name, text))) {
        parts.push(format!("Kurallar ({name}):\n{rules}"));
    }
    if let Some(readme) = head(&root.join("README.md"), 25, 1_500) {
        parts.push(format!("README başı:\n{readme}"));
    }
    if let Some(notes) = notes.filter(|notes| !notes.trim().is_empty()) {
        parts.push(format!("Wiqo notların:\n{}", notes.chars().take(3_000).collect::<String>()));
    }
    clip(&parts.join("\n\n"), 8_000)
}

/// This computer's tools beyond the terminal (screen, system, file chores). The server describes them and turns a call into a step, by the one
/// definition a chat turn uses; the step runs here, with this computer's own guards and questions.
#[derive(Default)]
struct Pc {
    /// The computer as it describes itself with each call.
    device: Value,
    names: Vec<String>,
    /// The newest look at the screen: refs are good only inside it.
    snapshot: Option<String>,
}

/// Runs a step, and lets go of it a moment after the person stops the task (`None`: its result was not waited for).
async fn run_stoppable(engine: &Engine, step: &Value, card_approved: bool, send_approved: bool, stop: &Stop) -> Option<(Value, Option<Value>)> {
    let run = run_step_whole(engine, step, card_approved, send_approved);
    tokio::pin!(run);
    tokio::select! {
        done = &mut run => Some(done),
        _ = stop.asked() => tokio::time::timeout(STOP_GRACE, &mut run).await.ok(),
    }
}

impl Work {
    fn done_so_far(&self) -> String {
        if self.log.is_empty() {
            return "Henüz bir adım tamamlanmadı.".into();
        }
        let last: Vec<&str> = self.log.iter().rev().take(8).rev().map(String::as_str).collect();
        format!("Son adımlar:\n{}", last.join("\n"))
    }
}

/// Why a model call ended without an answer.
enum ModelStop {
    /// The person stopped the task (here or from another device).
    Cancelled,
    /// The run is over on the server: its time or its calls ran out.
    Gone(String),
    /// The context is larger than the model reads.
    TooLarge,
    /// The model wrote a tool call the provider could not read: the provider's reason and the start of what was written.
    Malformed(String),
    Failed(String),
}

struct ToolOutcome {
    ok: bool,
    /// What goes back to the model.
    output: String,
    /// One short line for the person and the log.
    brief: String,
    /// A background job this call left running.
    job: Option<String>,
}

fn refused(message: &str) -> ToolOutcome {
    ToolOutcome { ok: false, output: json!({ "status": "failed", "message": message }).to_string(), brief: message.chars().take(160).collect(), job: None }
}

/// The answer is the task's answer on every device: it is delivered until the server has it.
async fn deliver(http: reqwest::Client, server: String, token: String, run_id: String, reply: String, sent: Vec<Value>) {
    for wait in [0u64, 3, 10, 30, 60, 120, 300] {
        tokio::time::sleep(Duration::from_secs(wait)).await;
        let sent = http
            .post(format!("{server}/agent/finish"))
            .bearer_auth(&token)
            .timeout(Duration::from_secs(20))
            .json(&json!({ "runId": run_id, "reply": reply.chars().take(19_000).collect::<String>(), "sent": sent }))
            .send()
            .await;
        // Taken, or refused for good (the run is not this account's, or is unknown): either way there is nothing more to try.
        if sent.is_ok_and(|response| response.status().is_success() || matches!(response.status().as_u16(), 400 | 403 | 404)) {
            return;
        }
    }
}

impl Chat {
    /// The steps go to the server in small batches, in order, so the person's other devices see the work as it happens. A batch that does not arrive is
    /// not sent again: the next one carries the newer state, and the answer has its own delivery. Ends when the run lets go of its end of the channel.
    async fn report(&self, token: &str, run_id: &str, mut events: tokio::sync::mpsc::UnboundedReceiver<Value>) {
        while let Some(first) = events.recv().await {
            let mut batch = vec![first];
            tokio::time::sleep(Duration::from_millis(350)).await;
            while batch.len() < 40 {
                match events.try_recv() {
                    Ok(event) => batch.push(event),
                    Err(_) => break,
                }
            }
            // The newest token: a run of hours can outlive the one it started with.
            let bearer = self.token().unwrap_or_else(|_| token.to_string());
            let _ = self
                .http
                .post(format!("{}/agent/progress", self.server))
                .bearer_auth(&bearer)
                .timeout(Duration::from_secs(8))
                .json(&json!({ "runId": run_id, "events": batch }))
                .send()
                .await;
        }
    }

    /// Where a conversation's session is kept on this computer.
    fn session_file(&self, conversation: &str) -> Option<PathBuf> {
        let name: String = conversation.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').take(128).collect();
        if name.is_empty() {
            return None;
        }
        let folder = self.file.parent()?.join("local-sessions");
        std::fs::create_dir_all(&folder).ok()?;
        Some(folder.join(format!("{name}.json")))
    }

    /// The session is written after every turn (to a side file first, so a crash never leaves half of one): the app can close at any point and the
    /// next request of this conversation goes on from here.
    fn save_session(&self, conversation: &str, work: &Work, open: bool) {
        let Some(file) = self.session_file(conversation) else { return };
        let log: Vec<&String> = work.log.iter().rev().take(200).rev().collect();
        let held: Vec<Value> = work.held.iter().map(|(role, begin)| json!([role, begin])).collect();
        // `open`: the run is still going. A session found open when the app starts was cut off (the app closed, the computer restarted).
        let session = json!({ "v": 1, "savedAt": chrono::Utc::now().timestamp(), "conversation": conversation, "open": open, "goal": work.goal, "messages": work.messages.get(1..).unwrap_or(&[]), "plan": work.plan, "log": log, "held": held });
        let side = file.with_extension("json.tmp");
        if std::fs::write(&side, session.to_string()).is_ok() {
            let _ = std::fs::rename(&side, &file);
        }
    }

    fn load_session(&self, conversation: &str) -> Option<Value> {
        let file = self.session_file(conversation)?;
        let session: Value = serde_json::from_slice(&std::fs::read(file).ok()?).ok()?;
        let age = chrono::Utc::now().timestamp() - session["savedAt"].as_i64()?;
        (session["v"] == 1 && age < SESSION_DAYS * 86_400 && session["messages"].is_array()).then_some(session)
    }

    /// Work the app was closed in the middle of, newest first: the conversation and what was asked. Each is told once (its mark is taken off here), so
    /// the person is offered to go on with it and is not asked again at every start.
    pub fn interrupted(&self) -> Vec<Value> {
        let Some(folder) = self.file.parent().map(|parent| parent.join("local-sessions")) else { return Vec::new() };
        let now = chrono::Utc::now().timestamp();
        let mut found: Vec<(i64, Value)> = Vec::new();
        for entry in std::fs::read_dir(folder).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
                continue;
            }
            let Some(mut session) = std::fs::read(&path).ok().and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok()) else { continue };
            if session["open"] != true {
                continue;
            }
            session["open"] = json!(false);
            let side = path.with_extension("json.tmp");
            if std::fs::write(&side, session.to_string()).is_ok() {
                let _ = std::fs::rename(&side, &path);
            }
            let saved = session["savedAt"].as_i64().unwrap_or(0);
            let conversation = text_of(&session["conversation"]);
            if now - saved < 86_400 && !conversation.is_empty() {
                found.push((saved, json!({ "conversation": conversation, "goal": text_of(&session["goal"]).chars().take(200).collect::<String>() })));
            }
        }
        found.sort_by_key(|(saved, _)| std::cmp::Reverse(*saved));
        found.into_iter().map(|(_, session)| session).collect()
    }

    /// This computer's tools beyond the terminal, as the server describes them. None when the server cannot be asked or does not know them yet: the
    /// run then works with the terminal alone.
    async fn pc_tools(&self, token: &str, run_id: &str, device: &Value) -> Vec<Value> {
        let asked = self.http.post(format!("{}/agent/tools", self.server)).bearer_auth(token).timeout(Duration::from_secs(10)).json(&json!({ "runId": run_id, "device": device })).send().await;
        let Ok(response) = asked else { return Vec::new() };
        if !response.status().is_success() {
            return Vec::new();
        }
        response.json::<Value>().await.ok().and_then(|value| value["tools"].as_array().cloned()).unwrap_or_default()
    }

    /// One call of such a tool: the server writes the step, this computer asks what it must ask and runs it.
    #[allow(clippy::too_many_arguments)]
    async fn pc_tool(&self, engine: &Engine, run_id: &str, token: &str, pc: &mut Pc, sent: &mut Vec<Value>, name: &str, args: &Value, approve: &Approve, stop: &Stop) -> ToolOutcome {
        let bearer = self.token().unwrap_or_else(|_| token.to_string());
        let mut body = json!({ "runId": run_id, "device": pc.device, "name": name, "args": args });
        if let Some(snapshot) = &pc.snapshot {
            body["snapshotId"] = json!(snapshot);
        }
        let asked = self.http.post(format!("{}/agent/step", self.server)).bearer_auth(&bearer).timeout(Duration::from_secs(20)).json(&body).send().await;
        let Ok(response) = asked else {
            return refused("Sunucuya ulaşılamadı; bu araç şu an kullanılamıyor. İş terminalle yapılabiliyorsa öyle yap, değilse biraz sonra yeniden dene.");
        };
        let taken = response.status().is_success();
        let value: Value = response.json().await.unwrap_or(Value::Null);
        if !taken {
            return refused(value["error"]["message"].as_str().unwrap_or("Araç hazırlanamadı."));
        }
        if let Some(why) = value["refused"].as_str() {
            return refused(why);
        }
        let step = value["step"].clone();
        if !step.is_object() {
            return refused("Araç hazırlanamadı.");
        }
        let kind = text_of(&step["type"]);
        let summary = text_of(&step["summary"]);
        // The same questions a chat turn's step is asked with: a change, a send, anything that removes.
        let asked = needs_yes(&step);
        if asked {
            match approve(kind.clone(), with_facts(&step, with_preview(engine, &step, &summary))).await {
                Decision::Yes => {}
                Decision::No => return refused("Kullanıcı onay vermedi; çalıştırılmadı. Başka bir yol öner ya da ne yapmak istediğini sor."),
                Decision::TimedOut => return refused("Onay zamanında gelmedi; çalıştırılmadı."),
            }
        }
        let Some((mut result, mut data)) = run_stoppable(engine, &step, false, asked, stop).await else {
            return refused("Kullanıcı durdurdu; adımın sonucu beklenmedi.");
        };
        // A press that cannot be taken back is asked by the computer itself, with its own sentence; the rest of the step then runs carrying that yes.
        if result["errorCode"] == "DESKTOP_APPROVAL_REQUIRED" {
            match approve(kind.clone(), text_of(&result["error"])).await {
                Decision::Yes => {
                    let mut approved = rest_after_yes(&step, &result);
                    approved["id"] = json!(Uuid::new_v4().to_string());
                    match run_stoppable(engine, &approved, true, false, stop).await {
                        Some(again) => (result, data) = again,
                        None => return refused("Kullanıcı durdurdu; adımın sonucu beklenmedi."),
                    }
                }
                Decision::No => return refused("Kullanıcı bu geri alınamaz işleme onay vermedi; çalıştırılmadı."),
                Decision::TimedOut => return refused("Onay zamanında gelmedi; çalıştırılmadı."),
            }
        }
        let ok = result["status"] == "completed";
        if let Some(data) = &data {
            if data["kind"] == "computerDesktop" {
                if let Some(snapshot) = data["value"]["snapshotId"].as_str().filter(|id| !id.is_empty()) {
                    pc.snapshot = Some(snapshot.to_string());
                }
            }
            if ok && data["kind"] == "fileTransfer" {
                if let Some(artifact) = data["value"].as_str() {
                    sent.push(json!({ "stepId": step["id"], "artifactId": artifact }));
                }
            }
        }
        let mut shown = json!({ "status": result["status"], "did": summary, "message": result["message"] });
        if let Some(code) = result["errorCode"].as_str() {
            shown["errorCode"] = json!(code);
            shown["error"] = result["error"].clone();
        }
        if let Some(text) = result["dataText"].as_str() {
            shown["output"] = json!(clip(text, OUTPUT_CHARS));
        }
        if ok && kind == "captureScreen" {
            shown["note"] = json!("Görüntü telefona kendiliğinden gitmedi. Bir dosyaya kaydedildiyse (saveTo) computer_files send_to_phone ile gönder; kaydedilmediyse saveTo vererek yeniden al.");
        }
        let brief = result["error"].as_str().or(result["message"].as_str()).unwrap_or("").chars().take(160).collect();
        ToolOutcome { ok, output: shown.to_string(), brief, job: None }
    }

    /// Runs one handed-over request to its end on this computer. `handoff` is the chat turn's `localAgent`: run id, instructions, goal.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn local_agent(
        &self,
        engine: &Engine,
        device_id: &str,
        handoff: &Value,
        conversation: &str,
        history: &Value,
        text: &str,
        token: &str,
        approve: &Approve,
        stop: &Stop,
        emit: &impl Fn(Value),
    ) -> Result<()> {
        let run_id = handoff["runId"].as_str().unwrap_or("").to_string();
        if Uuid::parse_str(&run_id).is_err() {
            bail!("Yerel çalışma başlatılamadı.");
        }
        let goal = handoff["goal"].as_str().filter(|goal| !goal.trim().is_empty()).unwrap_or(text);
        let context = device_context(engine, device_id);
        let device = json!({
            "deviceId": device_id, "platform": if cfg!(windows) { "windows" } else { "macOS" }, "availableApps": context["availableApps"],
            "availableCapabilities": context["availableCapabilities"], "applicationTargets": context["applicationTargets"], "authorities": context["authorities"],
        });
        let extra = self.pc_tools(token, &run_id, &device).await;
        let names: Vec<String> = extra.iter().filter_map(|tool| tool["function"]["name"].as_str().map(str::to_string)).collect();
        let mut offered = tools();
        if let Some(list) = offered.as_array_mut() {
            list.extend(extra);
        }
        let notes = if names.is_empty() { String::new() } else { format!("\n\n{PC_NOTES}") };
        let mut work = Work {
            messages: Vec::new(), plan: Vec::new(), log: Vec::new(), goal: goal.to_string(), budget: CONTEXT_BYTES,
            tools: offered, pc: Pc { device, names, snapshot: None }, sent: Vec::new(), held: Vec::new(),
            reach: Reach { untrusted: handoff["untrusted"] == true, ..Reach::default() },
        };
        work.messages.push(json!({ "role": "system", "content": format!("{}\n\n{}{notes}", text_of(&handoff["instructions"]), environment()) }));
        // This conversation's earlier work on this computer, if any: the model goes on with what it wrote and ran, not only with what was said.
        match self.load_session(conversation) {
            Some(session) => {
                work.messages.extend(repair(session["messages"].as_array().cloned().unwrap_or_default()).into_iter().filter(|message| !is_malformed_note(message)));
                // The plan belongs to the request it was written for: it is carried on only when this request goes on with that one ("devam et").
                if handoff["resumes"] == true {
                    work.plan = session["plan"].as_array().cloned().unwrap_or_default();
                }
                work.log = session["log"].as_array().map(|lines| lines.iter().map(text_of).collect()).unwrap_or_default();
                work.held = session["held"].as_array().map(|turns| turns.iter().map(|turn| (text_of(&turn[0]), text_of(&turn[1]))).collect()).unwrap_or_default();
                shorten_old(&mut work.messages, 4);
            }
            None => {}
        }
        // What was said in the chat since (answers the server gave, not this computer): "az önce yazdığını dosyaya kaydet" needs them.
        let unknown = said_since(&work.messages, &mut work.held, history);
        work.messages.extend(unknown);
        work.messages.push(json!({ "role": "user", "content": if goal.trim() == text.trim() { text.to_string() } else { format!("{text}\n\n{goal}") } }));
        emit(json!({ "type": "reset" }));

        let (steps, received) = tokio::sync::mpsc::unbounded_channel::<Value>();
        let working = async {
            // The channel's sending end lives as long as the work: when the work ends, the reporter sends what is left and ends too.
            let steps = steps;
            let say = |event: Value| {
                let _ = steps.send(event.clone());
                emit(event);
            };
            self.work(engine, &run_id, conversation, &mut work, token, approve, stop, &say).await
        };
        let (reply, ()) = tokio::join!(working, self.report(token, &run_id, received));

        work.messages.push(json!({ "role": "assistant", "content": reply }));
        self.save_session(conversation, &work, false);
        emit(json!({ "type": "done", "reply": reply, "localRun": run_id }));
        // Delivered in the background: the turn ends for the person now, and a server that is out of reach gets the answer when it is back.
        let bearer = self.token().unwrap_or_else(|_| token.to_string());
        tokio::spawn(deliver(self.http.clone(), self.server.clone(), bearer, run_id, reply, std::mem::take(&mut work.sent)));
        Ok(())
    }

    /// The loop: ask the model, run what it calls, give the results back, until it answers in words and nothing is visibly left open.
    #[allow(clippy::too_many_arguments)]
    async fn work(&self, engine: &Engine, run_id: &str, conversation: &str, work: &mut Work, token: &str, approve: &Approve, stop: &Stop, say: &impl Fn(Value)) -> String {
        let started = Instant::now();
        let stopped = |work: &Work| format!("Durduruldu. {}", work.done_so_far());
        let mut thinking = 0usize;
        let mut gates = 0usize;
        let mut malformed = 0usize;
        let mut too_large = 0usize;
        let mut failures = 0usize;
        let mut failures_noted = false;
        let mut failed_calls: HashMap<String, usize> = HashMap::new();
        let mut last_failure: Option<String> = None;
        let mut jobs: Vec<String> = Vec::new();
        let mut same: (String, usize) = (String::new(), 0);
        for turn in 0..MAX_TURNS {
            if stop.is_asked() {
                self.cancel_jobs(engine, &jobs).await;
                return stopped(work);
            }
            if started.elapsed() > Duration::from_secs(MAX_MINUTES * 60) {
                return format!("Bu çalışma {} saati doldurdu ve burada durdu. {}\n\n\"devam et\" yazarsan kaldığım yerden sürdürürüm.", MAX_MINUTES / 60, work.done_so_far());
            }
            self.compact(run_id, work, token, stop, say).await;
            self.save_session(conversation, work, true);
            thinking += 1;
            let think_id = format!("local-think-{thinking}");
            let think_label = if turn == 0 { "Düşünüyor" } else { "Sonuca bakıyor" };
            let think = |label: &str, state: &str| say(json!({ "type": "activity", "activity": { "id": think_id, "phase": "plan", "label": label, "state": state } }));
            think(think_label, "running");
            let waiting = |words: String| think(&words, "running");
            let answer = match self.model_call(token, run_id, &work.messages, &work.tools, stop, &waiting).await {
                Ok(answer) => answer,
                Err(ModelStop::Cancelled) => {
                    think("Durduruldu", "failed");
                    self.cancel_jobs(engine, &jobs).await;
                    return stopped(work);
                }
                Err(ModelStop::TooLarge) => {
                    // The server's model reads less than was sent: the budget of this run is cut and the older work is summarised before the next try.
                    think("Bağlam kısaltılıyor", "done");
                    too_large += 1;
                    if too_large > 4 {
                        return format!("Çalışma bağlamı modelin okuyabileceğinden büyük kaldı ve kısaltılamadı. {}\n\n\"devam et\" yazarsan kaldığım yerden sürdürürüm.", work.done_so_far());
                    }
                    work.budget = (work.budget / 2).max(CONTEXT_BYTES_LEAST);
                    shorten_old(&mut work.messages, 3);
                    continue;
                }
                Err(ModelStop::Malformed(why)) => {
                    think("Araç çağrısı okunamadı; yeniden yazdırılıyor", "done");
                    malformed += 1;
                    // Only the newest note stays: piled up, they were saved with the session and "devam et" started from the same dead end.
                    work.messages.retain(|message| !is_malformed_note(message));
                    if malformed > 4 {
                        return format!("Model art arda okunamayan araç çağrıları yazdı; burada durdum. {}\n\n\"devam et\" yazarsan sürdürürüm.", work.done_so_far());
                    }
                    let said = if why.is_empty() { String::new() } else { format!("\nSağlayıcının nedeni ve yazdığın: {why}") };
                    work.messages.push(json!({ "role": "user", "content": format!("{MALFORMED_NOTE}{said}\nAraç adını listedeki gibi yaz (ör. shell, write_file), argümanları aracın alanlarıyla geçerli JSON olarak ver, listede olmayan alan ekleme. İçerik uzunsa write_file ile ilk bölümü yaz, kalanını append: true ile ekle.") }));
                    continue;
                }
                Err(ModelStop::Gone(why)) => {
                    think("Çalışma sunucuda kapandı", "failed");
                    return format!("Çalışma sunucuda sona erdi ({why}). {}\n\n\"devam et\" yazarsan kaldığım yerden sürdürürüm.", work.done_so_far());
                }
                Err(ModelStop::Failed(why)) => {
                    think("Model yanıt vermedi", "failed");
                    return format!("Model şu an yanıt vermiyor ({why}). {}\n\nYapılanlar bilgisayarda duruyor; \"devam et\" yazarsan kaldığım yerden sürdürürüm.", work.done_so_far());
                }
            };
            think(think_label, "done");
            if malformed > 0 {
                work.messages.retain(|message| !is_malformed_note(message));
            }
            malformed = 0;
            let content = text_of(&answer["content"]);
            let mut calls = answer["tool_calls"].as_array().cloned().unwrap_or_default();
            if calls.is_empty() {
                // Said "done" with work visibly open: turned back, at most GATE_TURNS times, with what is open. After that its words stand.
                let open = plan_open(&work.plan);
                let turn_back = if content.trim().is_empty() {
                    Some("Yanıtın boş geldi. İş bittiyse ne yaptığını ve sonucu yaz; bitmediyse araçlarla devam et.".to_string())
                } else if !open.is_empty() {
                    Some(format!("Planında bitmemiş adımlar var: {}. Bunları şimdi bitir. Yapılamayacak olanı plan aracıyla dropped işaretle ve yanıtında nedenini açıkça söyle.", open.join("; ")))
                } else {
                    last_failure.take().map(|failed| format!("Son adım başarısız olmuştu ({failed}). Düzeltip doğrula; düzeltilemiyorsa yanıtında bunun yapılamadığını ve nedenini açıkça yaz."))
                };
                if let Some(why) = turn_back.filter(|_| gates < GATE_TURNS) {
                    gates += 1;
                    if !content.trim().is_empty() {
                        work.messages.push(json!({ "role": "assistant", "content": content }));
                    }
                    work.messages.push(json!({ "role": "user", "content": why }));
                    continue;
                }
                if content.trim().is_empty() {
                    return format!("Çalışma bir özet yazılmadan bitti. {}", work.done_so_far());
                }
                // Turned back as often as allowed and the plan is still open: the answer stands, with what it left undone said under it.
                if !open.is_empty() {
                    return format!("{content}\n\nPlanda açık kalan adımlar:\n{}", open.iter().map(|title| format!("- {title}")).collect::<Vec<_>>().join("\n"));
                }
                return content;
            }
            for call in &mut calls {
                if call["id"].as_str().is_none_or(str::is_empty) {
                    call["id"] = json!(format!("call_{}", Uuid::new_v4().simple()));
                }
                call["type"] = json!("function");
            }
            work.messages.push(json!({ "role": "assistant", "content": content, "tool_calls": calls }));
            for call in &calls {
                let id = text_of(&call["id"]);
                let name = text_of(&call["function"]["name"]);
                let raw = call["function"]["arguments"].as_str().unwrap_or("{}").to_string();
                let args = serde_json::from_str::<Value>(if raw.trim().is_empty() { "{}" } else { &raw }).ok().filter(Value::is_object);
                if name == "plan" {
                    let output = match args.as_ref().and_then(|args| args["steps"].as_array()).filter(|steps| !steps.is_empty()) {
                        Some(steps) => {
                            work.plan = steps.iter().take(20).map(|step| json!({ "title": text_of(&step["title"]).chars().take(200).collect::<String>(), "status": step["status"].as_str().unwrap_or("pending") })).collect();
                            let done = work.plan.iter().filter(|step| matches!(step["status"].as_str(), Some("done" | "dropped"))).count();
                            let now = work.plan.iter().find(|step| step["status"] == "doing").or_else(|| work.plan.iter().find(|step| step["status"] == "pending")).map(|step| text_of(&step["title"]));
                            let words = match now {
                                Some(now) => format!("Plan {done}/{}: {}", work.plan.len(), now.chars().take(90).collect::<String>()),
                                None => format!("Plan {done}/{} tamam", work.plan.len()),
                            };
                            say(json!({ "type": "activity", "activity": { "id": "local-plan", "phase": "plan", "label": words, "state": if done == work.plan.len() { "done" } else { "running" } } }));
                            json!({ "status": "completed", "message": "Plan kaydedildi.", "plan": plan_text(&work.plan) }).to_string()
                        }
                        None => json!({ "status": "failed", "message": "steps gerekli: [{title, status}]." }).to_string(),
                    };
                    work.messages.push(json!({ "role": "tool", "tool_call_id": id, "content": output }));
                    continue;
                }
                let words = label(&name, args.as_ref().unwrap_or(&Value::Null));
                let step_id = format!("local-{}", Uuid::new_v4());
                say(json!({ "type": "step_update", "step": { "id": step_id, "summary": words, "status": "running" } }));
                let key = format!("{name}:{raw}");
                let tried = failed_calls.get(&key).copied().unwrap_or(0);
                let mut outcome = match &args {
                    None => refused("Araç çağrısının argümanları geçerli JSON değil (büyük olasılıkla çıktı sınırında kesildi); çalıştırılmadı. Çağrıyı daha küçük yaz: uzun dosyayı write_file ile ilk bölüm, sonra append: true ile kalan bölümler halinde gönder."),
                    Some(_) if stop.is_asked() => refused("Kullanıcı durdurdu; çalıştırılmadı."),
                    Some(_) if tried >= 3 => refused("Bu çağrı aynı haliyle üç kez başarısız oldu; yeniden çalıştırılmadı. Hatanın nedenini araştır (dosya var mı, yol doğru mu, araç kurulu mu, izin var mı) ve farklı bir yol dene."),
                    Some(args) if work.pc.names.contains(&name) => self.pc_tool(engine, run_id, token, &mut work.pc, &mut work.sent, &name, args, approve, stop).await,
                    Some(args) if name == "web" => self.web_tool(run_id, token, args).await,
                    Some(args) if name == "project_notes" => self.write_notes(args),
                    Some(args) => self.local_tool(engine, &name, args, &mut work.reach, approve, stop).await,
                };
                // The first touch of a project brings what it is and how it is worked on, once per run.
                if let Some(root) = args.as_ref().and_then(|args| touched_folder(&name, args)).and_then(|folder| project_root(&folder)) {
                    if !work.reach.briefed.contains(&root) {
                        work.reach.briefed.push(root.clone());
                        let notes = self.notes_file(&root).and_then(|file| std::fs::read_to_string(file).ok());
                        let brief = tokio::task::spawn_blocking(move || project_brief(&root, notes)).await.unwrap_or_default();
                        if !brief.is_empty() {
                            outcome.output = format!("{}\n\n{brief}", outcome.output);
                        }
                    }
                }
                if outcome.ok {
                    failures = 0;
                    last_failure = None;
                    // The same call with the same result, again and again, is a loop that learns nothing (waiting on a job is not: time passes there).
                    let seen = format!("{key}\n{}", outcome.output);
                    same = if same.0 == seen { (seen, same.1 + 1) } else { (seen, 1) };
                    if same.1 >= 3 && name != "job" {
                        outcome.output = format!("{}\n\nUYARI: Bu çağrı üst üste {} kez aynı sonucu verdi; yinelemek yeni bilgi getirmiyor. Elindeki sonuçla ilerle ya da farklı bir şeye bak.", outcome.output, same.1);
                    }
                } else {
                    failures += 1;
                    last_failure = Some(format!("{words}: {}", outcome.brief));
                    same = (String::new(), 0);
                    let count = failed_calls.entry(key).or_insert(0);
                    *count += 1;
                    if *count == 2 {
                        outcome.output = format!("{}\n\nUYARI: Bu çağrı aynı haliyle ikinci kez başarısız oldu. Üçüncü kez aynısını deneme; hatayı oku, nedenini bul, farklı bir yol seç.", outcome.output);
                    }
                }
                if let Some(job) = outcome.job.take() {
                    if !jobs.contains(&job) {
                        jobs.push(job);
                    }
                }
                let mut step = json!({ "id": step_id, "summary": words, "status": if outcome.ok { "completed" } else { "failed" } });
                if !outcome.ok && !outcome.brief.is_empty() {
                    step["message"] = json!(outcome.brief);
                }
                say(json!({ "type": "step_update", "step": step }));
                work.log.push(format!("{} {words}{}", if outcome.ok { "✓" } else { "✗" }, if outcome.ok { String::new() } else { format!(" ({})", outcome.brief) }));
                work.messages.push(json!({ "role": "tool", "tool_call_id": id, "content": outcome.output }));
            }
            if failures >= FAILURES_MOST {
                self.cancel_jobs(engine, &jobs).await;
                return format!("Art arda {failures} adım başarısız oldu; daha fazla denemeden burada durdum. {}\n\nNeyi farklı yapmamı istediğini yazarsan oradan devam ederim.", work.done_so_far());
            }
            if failures >= FAILURES_NOTED && !failures_noted {
                failures_noted = true;
                work.messages.push(json!({ "role": "user", "content": format!("Art arda {failures} adım başarısız oldu. Aynı yolda ısrar etme: dur, şu ana kadar ne bildiğini ve neyin tutmadığını bir cümleyle yaz, durumu incele (ls, sürüm, günlük) ve ona göre yeni bir yol seç. İş gerçekten yapılamıyorsa bunu açıkça söyleyerek bitir.") }));
            }
        }
        format!("Bu çalışma {MAX_TURNS} adım sınırına geldi ve burada durdu. {}\n\n\"devam et\" yazarsan kaldığım yerden sürdürürüm.", work.done_so_far())
    }

    /// Jobs this run left running are stopped with it.
    async fn cancel_jobs(&self, engine: &Engine, jobs: &[String]) {
        for job in jobs {
            let step = json!({ "id": Uuid::new_v4().to_string(), "type": "terminal", "summary": "Arka plan işini durdur", "payload": { "kind": "terminal", "value": { "action": "cancel_job", "jobId": job } } });
            let _ = run_step_as(engine, &step, false, false).await;
        }
    }

    /// Keeps the context within what the model reads. Old results are shortened first; if that is not enough, the older part of the work is replaced by
    /// a summary the model writes of it (or, when the model cannot be asked, by the list of steps), with the request and the plan said again.
    async fn compact(&self, run_id: &str, work: &mut Work, token: &str, stop: &Stop, say: &impl Fn(Value)) {
        if weight(&work.messages) <= work.budget {
            return;
        }
        shorten_old(&mut work.messages, KEEP_FULL_RESULTS);
        if weight(&work.messages) <= work.budget {
            return;
        }
        let start = tail_start(&work.messages, work.budget / 3);
        if start <= 2 {
            shorten_old(&mut work.messages, 2);
            return;
        }
        say(json!({ "type": "activity", "activity": { "id": "local-compact", "phase": "plan", "label": "Uzun çalışma özetleniyor", "state": "running" } }));
        let ask = vec![
            json!({ "role": "system", "content": "Bir bilgisayar ajanının çalışma kaydını, aynı ajan işe kaldığı yerden devam edebilsin diye özetliyorsun. Türkçe, kısa ve somut yaz. Başlıklar: HEDEF (kullanıcının istediği, kendi sözleriyle koşulları), YAPILANLAR (oluşturulan ve değiştirilen dosyaların tam yolları, kurulanlar, çalıştırılan önemli komutlar ve sonuçları), ÖĞRENİLENLER (ortam bilgisi, sürümler, karşılaşılan hatalar ve neyin çözdüğü, tutmayan yollar), ŞU ANKİ DURUM, KALAN İŞ. Kayıtta olmayan hiçbir şey ekleme." }),
            json!({ "role": "user", "content": format!("KULLANICININ İSTEĞİ:\n{}\n\nÇALIŞMA KAYDI:\n{}", clip(&work.goal, 4_000), transcript(&work.messages[1..start])) }),
        ];
        let quiet = |_: String| {};
        let summary = match self.model_call(token, run_id, &ask, &json!([]), stop, &quiet).await {
            Ok(answer) if !text_of(&answer["content"]).trim().is_empty() => clip(&text_of(&answer["content"]), 12_000),
            _ => format!("(Model özet yazamadı; adımların listesi)\n{}", work.log.iter().rev().take(80).rev().cloned().collect::<Vec<_>>().join("\n")),
        };
        let note = format!(
            "ÖNCEKİ ÇALIŞMANIN ÖZETİ (bağlam dolduğu için eski adımlar kaldırıldı; anlatılanlar senin kendi çalışman)\n\nKULLANICININ İSTEĞİ:\n{}\n\n{summary}\n\nPLAN:\n{}\n\nBuradan devam et. Bir dosyanın içeriği lazımsa yeniden oku; yapılmış işi yeniden yapma.",
            clip(&work.goal, 4_000),
            plan_text(&work.plan)
        );
        let mut next = vec![work.messages[0].clone(), json!({ "role": "user", "content": note })];
        next.extend(work.messages.drain(start..));
        work.messages = next;
        if weight(&work.messages) > work.budget {
            shorten_old(&mut work.messages, 3);
        }
        say(json!({ "type": "activity", "activity": { "id": "local-compact", "phase": "plan", "label": "Uzun çalışma özetlendi", "state": "done" } }));
    }

    /// One model call through the server's gateway. A model that is busy and a server that is out of reach are waited for (about twelve minutes in
    /// all, said to the person); what waiting cannot change ends the call with its reason.
    async fn model_call(&self, token: &str, run_id: &str, messages: &[Value], tools: &Value, stop: &Stop, waiting: &impl Fn(String)) -> Result<Value, ModelStop> {
        let body = json!({ "runId": run_id, "messages": messages, "tools": tools });
        let mut last = String::from("yanıt yok");
        let mut refused = 0usize;
        for attempt in 0..=MODEL_WAITS.len() {
            if attempt > 0 {
                let seconds = MODEL_WAITS[attempt - 1];
                waiting(format!("Model yanıt vermedi ({last}); {seconds} sn sonra yeniden deniyorum"));
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(seconds)) => {}
                    _ = stop.asked() => return Err(ModelStop::Cancelled),
                }
            }
            // The newest token: a run of hours can outlive the one it started with.
            let bearer = self.token().unwrap_or_else(|_| token.to_string());
            let send = self.http.post(format!("{}/agent/model", self.server)).bearer_auth(&bearer).timeout(Duration::from_secs(190)).json(&body).send();
            let sent = tokio::select! {
                sent = send => sent,
                _ = stop.asked() => return Err(ModelStop::Cancelled),
            };
            let response = match sent {
                Ok(response) => response,
                Err(_) => {
                    last = "sunucuya ulaşılamadı".into();
                    continue;
                }
            };
            let status = response.status().as_u16();
            let value: Value = response.json().await.unwrap_or(Value::Null);
            if (200..300).contains(&status) {
                return Ok(value["message"].clone());
            }
            let code = value["error"]["code"].as_str().unwrap_or("");
            let message = value["error"]["message"].as_str().or(value["error"].as_str()).unwrap_or("model hatası").to_string();
            match (status, code) {
                (_, "RUN_CANCELLED") => return Err(ModelStop::Cancelled),
                (_, "RUN_GONE" | "RUN_LIMIT") | (404, _) => return Err(ModelStop::Gone(message)),
                (_, "CONTEXT_TOO_LARGE") | (413, _) => return Err(ModelStop::TooLarge),
                (_, "TOOL_CALL_MALFORMED") => {
                    let reason = value["error"]["reason"].as_str().unwrap_or("").chars().take(400).collect::<String>();
                    let written = value["error"]["failedGeneration"].as_str().unwrap_or("").chars().take(500).collect::<String>();
                    return Err(ModelStop::Malformed(format!("{reason}\n{written}").trim().to_string()));
                }
                // An older server names no code: its run limit is the 429 that speaks of the run.
                (429, "") if message.contains("model çağrısı") => return Err(ModelStop::Gone(message)),
                (400 | 403, _) => return Err(ModelStop::Failed(message)),
                // Signed out, or a provider that refuses this very request: tried a few times, not for twelve minutes.
                (401, _) | (_, "MODEL_FAILED") => {
                    refused += 1;
                    if refused >= 3 {
                        return Err(ModelStop::Failed(message));
                    }
                }
                _ => {}
            }
            last = message;
        }
        Err(ModelStop::Failed(last))
    }

    /// A web search or one page, through the server's web layer.
    async fn web_tool(&self, run_id: &str, token: &str, args: &Value) -> ToolOutcome {
        let (query, url) = (text_of(&args["query"]), text_of(&args["url"]));
        let body = match (query.trim().is_empty(), url.trim().is_empty()) {
            (false, true) => json!({ "runId": run_id, "query": query.trim() }),
            (true, false) => json!({ "runId": run_id, "url": url.trim() }),
            _ => return refused("query ya da url ver (yalnız biri)."),
        };
        let bearer = self.token().unwrap_or_else(|_| token.to_string());
        let asked = self.http.post(format!("{}/agent/web", self.server)).bearer_auth(bearer).timeout(Duration::from_secs(60)).json(&body).send().await;
        let Ok(response) = asked else { return refused("Web'e ulaşılamadı.") };
        let ok = response.status().is_success();
        let answer = response.json::<Value>().await.unwrap_or_default();
        if !ok {
            return refused(answer["error"]["message"].as_str().unwrap_or("Web isteği reddedildi."));
        }
        let text = clip(&text_of(&answer["text"]), OUTPUT_CHARS);
        ToolOutcome { ok: true, output: json!({ "status": "completed", "output": text }).to_string(), brief: String::new(), job: None }
    }

    /// Where a project's notes live on this computer (never in the project, so nothing is added to its files or its git).
    fn notes_file(&self, root: &Path) -> Option<PathBuf> {
        use sha2::Digest;
        let digest = sha2::Sha256::digest(root.to_string_lossy().as_bytes());
        let name: String = digest.iter().take(12).map(|byte| format!("{byte:02x}")).collect();
        let folder = self.file.parent()?.join("project-notes");
        std::fs::create_dir_all(&folder).ok()?;
        Some(folder.join(format!("{name}.md")))
    }

    fn write_notes(&self, args: &Value) -> ToolOutcome {
        let project = expand(&text_of(&args["project"]));
        if !project.is_absolute() || !plain(&project) || !project.is_dir() {
            return refused("project var olan bir klasörün tam yolu olmalı.");
        }
        let root = project_root(&std::fs::canonicalize(&project).unwrap_or(project.clone())).unwrap_or(project);
        let content: String = text_of(&args["content"]).chars().take(8_000).collect();
        match self.notes_file(&root).map(|file| std::fs::write(file, &content)) {
            Some(Ok(())) => ToolOutcome { ok: true, output: json!({ "status": "completed", "message": format!("{} için not kaydedildi.", root.display()) }).to_string(), brief: String::new(), job: None },
            _ => refused("Not kaydedilemedi."),
        }
    }

    /// One tool call, run by this computer's terminal session; what goes back to the model is its status, its message and its output.
    async fn local_tool(&self, engine: &Engine, name: &str, args: &Value, reach: &mut Reach, approve: &Approve, stop: &Stop) -> ToolOutcome {
        let full = wiqo_ops::full_access() && !reach.untrusted;
        let mut outward = false;
        let (value, changes, question) = match name {
            "shell" => {
                let command = text_of(&args["command"]);
                if command.trim().is_empty() {
                    return refused("command gerekli.");
                }
                if command.split(|c: char| c.is_whitespace() || ";&|(".contains(c)).any(|word| word == "sudo") {
                    if !full {
                        return refused("sudo yalnız tam yetki açıkken çalışır; Sorarak modunda çalıştırılmadı. sudo'suz bir yol seç ya da komutu kullanıcıya ver.");
                    }
                    let asks = { let command = command.clone(); tokio::task::spawn_blocking(move || sudo_would_ask(&command)).await.unwrap_or(true) };
                    if asks {
                        return refused("Bu komut yönetici şifresi istiyor ve Wiqo şifre giremez; çalıştırılmadı. sudo'suz bir yol seç (brew, kullanıcı klasörüne kurulum) ya da komutu yanıtında kullanıcıya ver, kendi terminalinde çalıştırsın.");
                    }
                }
                outward = reaches_out(&command);
                let reads = wiqo_ops::is_read_only(&command);
                let mut value = json!({ "action": "run", "command": command, "mode": if full || !reads { "full" } else { "read" } });
                if let Some(workdir) = args["workdir"].as_str().filter(|dir| !dir.trim().is_empty()) {
                    value["workingDirectory"] = json!(workdir);
                }
                if let Some(seconds) = args["timeout_seconds"].as_u64() {
                    value["timeoutSeconds"] = json!(seconds.clamp(1, 50));
                }
                (value, !reads, format!("Komut çalıştırılsın mı?\n{command}"))
            }
            "job" => {
                let job = text_of(&args["job_id"]);
                if job.trim().is_empty() {
                    return refused("job_id gerekli (shell'in döndürdüğü job_id).");
                }
                let value = match args["action"].as_str().unwrap_or("") {
                    "wait" => json!({ "action": "wait_job", "jobId": job, "waitSeconds": args["wait_seconds"].as_u64().unwrap_or(45).clamp(5, 45) }),
                    "status" => json!({ "action": "get_job", "jobId": job }),
                    "input" => json!({ "action": "send_input", "jobId": job, "input": text_of(&args["text"]) }),
                    "cancel" => json!({ "action": "cancel_job", "jobId": job }),
                    _ => return refused("action wait, status, input ya da cancel olmalı."),
                };
                let writes = args["action"] == "input";
                (value, writes, format!("Çalışan işe girdi yazılsın mı?\n{}", text_of(&args["text"])))
            }
            "read_file" => {
                let (file, folder) = file_and_folder(&text_of(&args["path"]));
                if file.trim().is_empty() {
                    return refused("path gerekli.");
                }
                let mut value = json!({ "action": "read_file", "filePath": file, "mode": if full { "full" } else { "read" } });
                if let Some(folder) = folder {
                    value["workingDirectory"] = json!(folder);
                }
                for (from, to) in [("start_line", "startLine"), ("end_line", "endLine")] {
                    if let Some(line) = args[from].as_u64() {
                        value[to] = json!(line);
                    }
                }
                (value, false, String::new())
            }
            "write_file" => {
                let path = text_of(&args["path"]);
                let (file, folder) = file_and_folder(&path);
                if file.trim().is_empty() || !args["content"].is_string() {
                    return refused("path ve content gerekli.");
                }
                if let Some(folder) = &folder {
                    let _ = std::fs::create_dir_all(Path::new(folder));
                }
                let mut value = json!({ "action": "write_file", "filePath": file, "content": text_of(&args["content"]), "mode": "full" });
                if args["append"] == true {
                    value["append"] = json!(true);
                }
                if let Some(folder) = folder {
                    value["workingDirectory"] = json!(folder);
                }
                (value, true, format!("Dosya yazılsın mı?\n{path}"))
            }
            "edit_file" => {
                let path = text_of(&args["path"]);
                let (file, folder) = file_and_folder(&path);
                if file.trim().is_empty() || text_of(&args["find"]).is_empty() {
                    return refused("path ve find gerekli.");
                }
                let mut value = json!({ "action": "edit_file", "mode": "full", "replacements": [{
                    "filePath": file, "find": text_of(&args["find"]), "replace": text_of(&args["replace"]), "replaceAll": args["replace_all"] == true
                }] });
                if let Some(folder) = folder {
                    value["workingDirectory"] = json!(folder);
                }
                (value, true, format!("Dosya düzenlensin mi?\n{path}"))
            }
            "apply_patch" => {
                let workdir = text_of(&args["workdir"]);
                let edits: Vec<Value> = args["edits"].as_array().cloned().unwrap_or_default().iter()
                    .filter(|edit| !text_of(&edit["path"]).trim().is_empty() && !text_of(&edit["patch"]).is_empty())
                    .map(|edit| json!({ "filePath": text_of(&edit["path"]), "patch": text_of(&edit["patch"]) }))
                    .collect();
                if workdir.trim().is_empty() || edits.is_empty() {
                    return refused("workdir ve edits [{path, patch}] gerekli.");
                }
                let files: Vec<String> = edits.iter().map(|edit| text_of(&edit["filePath"])).collect();
                (json!({ "action": "apply_patch", "workingDirectory": workdir, "mode": "full", "edits": edits }), true, format!("Yama uygulansın mı? ({workdir})\n{}", files.join("\n")))
            }
            "undo" => {
                let id = text_of(&args["undo_id"]);
                let Some((_, folder)) = reach.undo.iter().find(|(known, _)| *known == id).cloned() else {
                    return refused("Bu undo_id bu çalışmada yapılmış bir değişikliğe ait değil.");
                };
                (json!({ "action": "undo", "undoId": id, "workingDirectory": folder, "mode": "full" }), true, format!("Değişiklik geri alınsın mı?\n{folder}"))
            }
            "search" => {
                if text_of(&args["pattern"]).is_empty() {
                    return refused("pattern gerekli.");
                }
                let folder = args["path"].as_str().filter(|path| !path.trim().is_empty()).unwrap_or("~");
                let mut value = json!({ "action": "search", "pattern": text_of(&args["pattern"]), "workingDirectory": folder, "mode": if full { "full" } else { "read" } });
                if let Some(glob) = args["glob"].as_str() {
                    value["glob"] = json!(glob);
                }
                (value, false, String::new())
            }
            other => return refused(&format!("{other} diye bir araç yok. Araçların: shell, job, read_file, write_file, edit_file, apply_patch, undo, search, web, project_notes, plan.")),
        };
        // Sorarak: a change is asked on this computer, with the exact command or file; under full access it runs. One yes covers the rest of the run's
        // changes in that project: files by their path, commands by their workdir only on macOS, where the sandbox keeps their writes in that folder.
        // A command that reaches past the folder (network sends, other processes, other apps) is asked every time.
        let folder = touched_folder(name, args);
        let covers = |allowed: &PathBuf| folder.as_ref().is_some_and(|folder| folder.starts_with(allowed));
        let confined = name != "shell" || (cfg!(target_os = "macos") && !outward);
        let covered = !reach.untrusted && name != "job" && confined && reach.allowed.iter().any(covers);
        if changes && !full && !covered {
            let scope = folder.as_deref().and_then(allowable).filter(|_| name != "job" && confined && !reach.untrusted);
            let question = match &scope {
                Some(scope) => format!("{question}\n\nOnaylarsan bu iş boyunca {} içindeki değişiklikler yeniden sorulmaz.", scope.display()),
                None => question,
            };
            match approve("terminal".into(), question).await {
                Decision::Yes => reach.allowed.extend(scope),
                Decision::No => return refused("Kullanıcı onay vermedi; çalıştırılmadı. Başka bir yol öner ya da ne yapmak istediğini sor."),
                Decision::TimedOut => return refused("Onay zamanında gelmedi; çalıştırılmadı."),
            }
        }
        let step = json!({ "id": Uuid::new_v4().to_string(), "type": "terminal", "summary": label(name, args), "payload": { "kind": "terminal", "value": value } });
        let Some((result, data)) = run_stoppable(engine, &step, false, false, stop).await else {
            return refused("Kullanıcı durdurdu; adımın sonucu beklenmedi.");
        };
        let ok = result["status"] == "completed";
        let mut shown = json!({ "status": result["status"], "message": result["message"] });
        if let Some(code) = result["errorCode"].as_str() {
            shown["errorCode"] = json!(code);
            shown["error"] = result["error"].clone();
        }
        let mut job = None;
        if let Some(data) = &data {
            shown["output"] = json!(shown_output(data));
            let value = if data["kind"] == "terminalResult" { &data["value"] } else { data };
            if let (Some(undo), Some(folder)) = (value["undoId"].as_str(), step["payload"]["value"]["workingDirectory"].as_str()) {
                reach.undo.push((undo.to_string(), folder.to_string()));
            }
            let value = &data["value"];
            if value["status"] == "running" {
                job = value["jobId"].as_str().map(str::to_string);
            }
        }
        let brief = result["error"].as_str().or(result["message"].as_str()).unwrap_or("").chars().take(160).collect();
        ToolOutcome { ok, output: shown.to_string(), brief, job }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(id: &str, name: &str, args: Value) -> Value {
        json!({ "id": id, "type": "function", "function": { "name": name, "arguments": args.to_string() } })
    }

    #[test]
    fn a_session_cut_off_in_its_tools_is_made_whole() {
        let saved = vec![
            json!({ "role": "tool", "tool_call_id": "orphan", "content": "x" }),
            json!({ "role": "user", "content": "site yap" }),
            json!({ "role": "assistant", "content": "", "tool_calls": [call("a", "shell", json!({ "command": "ls" })), call("b", "shell", json!({ "command": "pwd" }))] }),
            json!({ "role": "tool", "tool_call_id": "a", "content": "ok" }),
        ];
        let whole = repair(saved);
        let roles: Vec<&str> = whole.iter().map(|message| message["role"].as_str().unwrap()).collect();
        assert_eq!(roles, ["user", "assistant", "tool", "tool"]);
        assert_eq!(whole[3]["tool_call_id"], "b");
        assert!(text_of(&whole[3]["content"]).contains("bilinmiyor"));
    }

    #[test]
    fn old_work_is_made_light_and_the_newest_stays_whole() {
        let long = "x".repeat(5_000);
        let mut messages = vec![json!({ "role": "system", "content": "s" }), json!({ "role": "user", "content": "iş" })];
        for index in 0..6 {
            let id = format!("c{index}");
            messages.push(json!({ "role": "assistant", "content": "", "tool_calls": [call(&id, "write_file", json!({ "path": "/tmp/a.txt", "content": long }))] }));
            messages.push(json!({ "role": "tool", "tool_call_id": id, "content": long }));
        }
        shorten_old(&mut messages, 2);
        let first_args: Value = serde_json::from_str(messages[2]["tool_calls"][0]["function"]["arguments"].as_str().unwrap()).unwrap();
        assert_eq!(first_args["path"], "/tmp/a.txt");
        assert!(text_of(&first_args["content"]).starts_with("(kısaltıldı"));
        assert!(text_of(&messages[3]["content"]).len() < 1_000);
        assert_eq!(text_of(&messages[13]["content"]).len(), 5_000, "the newest results are untouched");
        assert!(messages[1].get("tool_calls").is_none(), "a message without calls gains no field");
    }

    #[test]
    fn the_kept_tail_never_starts_on_a_result_without_its_call() {
        let mut messages = vec![json!({ "role": "system", "content": "s" }), json!({ "role": "user", "content": "iş" })];
        for index in 0..5 {
            let id = format!("c{index}");
            messages.push(json!({ "role": "assistant", "content": "", "tool_calls": [call(&id, "shell", json!({ "command": "ls" }))] }));
            messages.push(json!({ "role": "tool", "tool_call_id": id, "content": "y".repeat(400) }));
        }
        for budget in [300, 700, 1_200, 5_000] {
            let start = tail_start(&messages, budget);
            assert!(start >= 1 && start <= messages.len());
            if start < messages.len() {
                assert_ne!(messages[start]["role"], "tool", "budget {budget}");
            }
        }
    }

    #[test]
    fn only_the_turns_the_session_does_not_hold_are_added_from_the_chat() {
        let known = vec![
            json!({ "role": "system", "content": "s" }),
            json!({ "role": "user", "content": "site yap\n\nHedef: site" }),
            json!({ "role": "assistant", "content": "", "tool_calls": [call("a", "shell", json!({ "command": "ls" }))] }),
            json!({ "role": "tool", "tool_call_id": "a", "content": "ok" }),
            json!({ "role": "assistant", "content": "Site hazır." }),
        ];
        let history = json!([
            { "role": "user", "content": "site yap" },
            { "role": "assistant", "content": "Site hazır." },
            { "role": "user", "content": "bir şiir yaz" },
            { "role": "assistant", "content": "Gül kokulu sabahlar…" },
        ]);
        let mut taken = Vec::new();
        let new = said_since(&known, &mut taken, &history);
        let said: Vec<String> = new.iter().map(|message| text_of(&message["content"])).collect();
        assert_eq!(said, ["bir şiir yaz", "Gül kokulu sabahlar…"]);
        // Summarised away since: what was taken in once is not added again.
        assert!(said_since(&known[..1], &mut taken, &json!([{ "role": "user", "content": "bir şiir yaz" }])).is_empty());
    }

    #[test]
    fn a_long_output_keeps_its_end_and_a_plan_says_what_is_open() {
        let output = format!("{}SON SATIR: error[E0308]", "satır\n".repeat(10_000));
        let shown = shown_output(&json!({ "kind": "terminalResult", "value": { "kind": "run", "workingDirectory": "/x", "stdout": output, "exitCode": 1 } }));
        assert!(shown.contains("error[E0308]"));
        assert!(shown.len() < OUTPUT_CHARS * 3);
        assert!(!shown.contains("workingDirectory"));
        let plan = vec![json!({ "title": "kur", "status": "done" }), json!({ "title": "derle", "status": "doing" }), json!({ "title": "eski yol", "status": "dropped" })];
        assert_eq!(plan_open(&plan), ["derle"]);
        assert!(plan_text(&plan).contains("[x] kur"));
    }
}
