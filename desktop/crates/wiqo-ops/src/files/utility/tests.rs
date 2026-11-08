use super::*;

fn temp() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("wiqo-utility-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&dir).unwrap();
    dunce::canonicalize(dir).unwrap()
}

fn call(value: Value) -> Result<Done> {
    file_utility(
        &Context::for_tests(),
        &json!({ "kind": "fileUtility", "value": value }),
    )
}

#[test]
fn documents_move_all_or_nothing() {
    let dir = temp();
    let source = dir.join("in");
    fs::create_dir(&source).unwrap();
    for name in ["a.pdf", "b.docx", "c.png"] {
        fs::write(source.join(name), b"x").unwrap();
    }
    let destination = dir.join("out");
    fs::create_dir(&destination).unwrap();
    fs::write(destination.join("a.pdf"), b"taken").unwrap();
    let path = source.to_string_lossy().to_string();
    let target = destination.to_string_lossy().to_string();
    let refused = call(json!({ "action": "moveDocuments", "path": path, "destination": target }))
        .unwrap_err();
    assert_eq!(refused.code, "FILE_EXISTS");
    assert!(
        source.join("b.docx").exists(),
        "nothing moves when one name is taken"
    );
    fs::remove_file(destination.join("a.pdf")).unwrap();
    let done =
        call(json!({ "action": "moveDocuments", "path": path, "destination": target })).unwrap();
    assert!(done.message.starts_with("2 belge"));
    assert!(source.join("c.png").exists() && destination.join("b.docx").exists());
}

#[test]
fn zip_round_trip_and_names_never_overwrite() {
    let dir = temp();
    let folder = dir.join("rapor");
    fs::create_dir(&folder).unwrap();
    fs::write(folder.join("not.txt"), b"merhaba").unwrap();
    let path = folder.to_string_lossy().to_string();
    call(json!({ "action": "archive", "path": path })).unwrap();
    call(json!({ "action": "archive", "path": path })).unwrap();
    assert!(dir.join("rapor.zip").exists() && dir.join("rapor (2).zip").exists());
    let zip = dir.join("rapor.zip").to_string_lossy().to_string();
    let target = dir.join("açılan").to_string_lossy().to_string();
    call(json!({ "action": "extract", "path": zip, "destination": target })).unwrap();
    assert_eq!(
        fs::read(dir.join("açılan/rapor/not.txt")).unwrap(),
        b"merhaba"
    );
}

#[test]
fn pdfs_merge_and_split() {
    use lopdf::content::{Content, Operation};
    use lopdf::{dictionary, Document, Object, Stream};
    fn pdf(path: &Path, pages: usize) {
        let mut doc = Document::with_version("1.5");
        let tree = doc.new_object_id();
        let mut kids = Vec::new();
        for _ in 0..pages {
            let content = Content {
                operations: vec![Operation::new("BT", vec![]), Operation::new("ET", vec![])],
            };
            let stream = doc.add_object(Stream::new(dictionary! {}, content.encode().unwrap()));
            kids.push(Object::from(doc.add_object(dictionary! {
                "Type" => "Page", "Parent" => tree, "Contents" => stream,
                "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
            })));
        }
        doc.objects.insert(
            tree,
            Object::Dictionary(
                dictionary! { "Type" => "Pages", "Kids" => kids, "Count" => pages as u32 },
            ),
        );
        let catalog = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => tree });
        doc.trailer.set("Root", catalog);
        doc.save(path).unwrap();
    }
    let dir = temp();
    pdf(&dir.join("a.pdf"), 2);
    pdf(&dir.join("b.pdf"), 3);
    let paths = [dir.join("a.pdf"), dir.join("b.pdf")].map(|p| p.to_string_lossy().to_string());
    let done = call(json!({ "action": "mergePDF", "paths": paths })).unwrap();
    assert!(done.message.contains("5 sayfa"), "{}", done.message);
    let merged = dir.join("Birleşik.pdf").to_string_lossy().to_string();
    let done = call(json!({ "action": "splitPDF", "path": merged })).unwrap();
    assert!(done.message.starts_with("5 sayfa"), "{}", done.message);
    assert_eq!(
        Document::load(dir.join("Birleşik sayfalar/Birleşik-sayfa-4.pdf"))
            .unwrap()
            .get_pages()
            .len(),
        1
    );
}

#[test]
fn organize_and_rename_keep_every_file() {
    let dir = temp();
    for name in ["x.pdf", "y.jpg", "z.unknown"] {
        fs::write(dir.join(name), b"x").unwrap();
    }
    let path = dir.to_string_lossy().to_string();
    call(json!({ "action": "organize", "path": path })).unwrap();
    assert!(
        dir.join("PDF/x.pdf").exists()
            && dir.join("Görseller/y.jpg").exists()
            && dir.join("Diğer/z.unknown").exists()
    );
    let pdfs = dir.join("PDF").to_string_lossy().to_string();
    call(json!({ "action": "batchRename", "path": pdfs, "name": "Fatura {n}" })).unwrap();
    assert!(dir.join("PDF/Fatura 1.pdf").exists());
    assert_eq!(human(1_500_000), "1,5 MB");
}

fn undo_id_of(done: &Done) -> String {
    let data = done.data.clone().unwrap();
    data["value"][0]["undoId"].as_str().unwrap().to_string()
}

#[test]
fn a_tidied_folder_goes_back_with_one_undo() {
    let ctx = Context::for_tests();
    let dir = temp();
    for name in ["a.pdf", "b.png", "c.png"] {
        fs::write(dir.join(name), name).unwrap();
    }
    let path = dir.to_string_lossy().to_string();
    let request = json!({ "kind": "fileUtility", "value": { "action": "organize", "path": path } });
    let done = file_utility(&ctx, &request).unwrap();
    assert!(!dir.join("a.pdf").exists());
    let id = undo_id_of(&done);
    assert_eq!(
        crate::files::undo::undo_file(&ctx, &id).unwrap(),
        "3 dosya eski yerine döndü."
    );
    for name in ["a.pdf", "b.png", "c.png"] {
        assert_eq!(fs::read_to_string(dir.join(name)).unwrap(), name);
    }
    let left: Vec<_> = fs::read_dir(&dir).unwrap().collect();
    assert_eq!(left.len(), 3, "the folders the tidy made are gone");
    assert!(
        crate::files::undo::undo_file(&ctx, &id).is_err(),
        "an undo is used once"
    );
}

#[test]
fn a_batch_with_one_changed_file_is_left_whole() {
    let ctx = Context::for_tests();
    let dir = temp();
    for name in ["x.txt", "y.txt"] {
        fs::write(dir.join(name), name).unwrap();
    }
    let path = dir.to_string_lossy().to_string();
    let request = json!({ "kind": "fileUtility", "value": { "action": "batchRename", "path": path, "name": "Not {n}" } });
    let done = file_utility(&ctx, &request).unwrap();
    let id = undo_id_of(&done);
    let renamed: Vec<PathBuf> = fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(renamed.len(), 2);
    fs::write(&renamed[0], "changed since").unwrap();
    assert_eq!(
        crate::files::undo::undo_file(&ctx, &id).unwrap_err().code,
        "FILE_UNDO_STALE"
    );
    assert!(
        renamed.iter().all(|path| path.exists()),
        "nothing was moved"
    );
}
