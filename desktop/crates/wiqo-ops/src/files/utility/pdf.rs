//! PDF: merging several into one and splitting one into pages.

use super::*;

// ----- PDF -----

pub(super) fn load_pdf(path: &Path) -> Result<lopdf::Document> {
    lopdf::Document::load(path).map_err(|_| {
        fail(
            "NOT_A_PDF",
            format!("{} PDF olarak açılamadı.", name_of(path)),
        )
    })
}

/// Pages of several PDFs, in order, as one document (the lopdf merge recipe: renumber each, keep one catalog and one page tree).
pub(super) fn merged(documents: Vec<lopdf::Document>) -> Option<lopdf::Document> {
    use lopdf::{Document, Object, ObjectId};
    let mut max_id = 1;
    let mut pages: Vec<(ObjectId, Object)> = Vec::new();
    let mut objects: BTreeMap<ObjectId, Object> = BTreeMap::new();
    for mut document in documents {
        document.renumber_objects_with(max_id);
        max_id = document.max_id + 1;
        for (_, id) in document.get_pages() {
            pages.push((id, document.get_object(id).ok()?.clone()));
        }
        objects.extend(document.objects);
    }
    let mut output = Document::with_version("1.5");
    let mut catalog: Option<(ObjectId, Object)> = None;
    let mut tree: Option<(ObjectId, Object)> = None;
    for (id, object) in objects {
        match object.type_name().unwrap_or("") {
            "Catalog" => catalog = Some((catalog.map(|(first, _)| first).unwrap_or(id), object)),
            "Pages" => {
                if let Ok(dictionary) = object.as_dict() {
                    let mut dictionary = dictionary.clone();
                    if let Some((_, Object::Dictionary(previous))) = &tree {
                        dictionary.extend(previous);
                    }
                    tree = Some((
                        tree.map(|(first, _)| first).unwrap_or(id),
                        Object::Dictionary(dictionary),
                    ));
                }
            }
            "Page" | "Outlines" | "Outline" => {}
            _ => {
                output.objects.insert(id, object);
            }
        }
    }
    let (tree_id, tree_object) = tree?;
    let (catalog_id, catalog_object) = catalog?;
    for (id, page) in &pages {
        let mut dictionary = page.as_dict().ok()?.clone();
        dictionary.set("Parent", tree_id);
        output.objects.insert(*id, Object::Dictionary(dictionary));
    }
    let mut dictionary = tree_object.as_dict().ok()?.clone();
    dictionary.set("Count", pages.len() as u32);
    dictionary.set(
        "Kids",
        pages
            .iter()
            .map(|(id, _)| Object::Reference(*id))
            .collect::<Vec<_>>(),
    );
    output
        .objects
        .insert(tree_id, Object::Dictionary(dictionary));
    let mut dictionary = catalog_object.as_dict().ok()?.clone();
    dictionary.set("Pages", tree_id);
    dictionary.remove(b"Outlines");
    output
        .objects
        .insert(catalog_id, Object::Dictionary(dictionary));
    output.trailer.set("Root", catalog_id);
    output.max_id = output.objects.keys().map(|(id, _)| *id).max().unwrap_or(1);
    output.renumber_objects();
    output.compress();
    Some(output)
}

pub(super) fn save_pdf(document: &mut lopdf::Document, path: &Path) -> bool {
    let temporary = path.with_file_name(format!(".{}.wiqo-partial", name_of(path)));
    let saved = document.save(&temporary).is_ok() && fs::rename(&temporary, path).is_ok();
    if !saved {
        let _ = fs::remove_file(&temporary);
    }
    saved
}

pub(super) fn merge_pdf(request: &Request) -> Result<Done> {
    let sources: Vec<PathBuf> = request
        .paths
        .clone()
        .unwrap_or_default()
        .into_iter()
        .map(|raw| existing(&Some(raw)))
        .collect::<Result<_>>()?;
    if sources.len() < 2 {
        return Err(error(
            "INVALID_PAYLOAD",
            "Birleştirmek için en az iki PDF gerekir.",
            "validation",
        ));
    }
    let documents = sources
        .iter()
        .map(|path| load_pdf(path))
        .collect::<Result<Vec<_>>>()?;
    let parent = sources[0]
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();
    let output = target(&request.destination, &parent.join("Birleşik.pdf"))?;
    let mut document =
        merged(documents).ok_or_else(|| fail("MERGE_FAILED", "PDF'ler birleştirilemedi."))?;
    let count = document.get_pages().len();
    if count == 0 || !save_pdf(&mut document, &output) {
        return Err(fail("MERGE_FAILED", "PDF'ler birleştirilemedi."));
    }
    Ok(Done::new(
        format!(
            "{} PDF birleştirildi: {} ({count} sayfa).",
            sources.len(),
            name_of(&output)
        ),
        Some(listing(&[output])),
    ))
}

pub(super) fn split_pdf(request: &Request, limit: usize) -> Result<Done> {
    let source = existing(&request.path)?;
    let document = load_pdf(&source)?;
    let stem = stem_of(&source);
    let parent = source.parent().map(Path::to_path_buf).unwrap_or_default();
    let folder = target(
        &request.destination,
        &parent.join(format!("{stem} sayfalar")),
    )?;
    fs::create_dir_all(&folder)
        .map_err(|_| fail("SPLIT_FAILED", "Sayfalar için klasör açılamadı."))?;
    let total = document.get_pages().len() as u32;
    let mut outputs = 0;
    for page in 1..=total.min(limit as u32) {
        let mut single = document.clone();
        let others: Vec<u32> = (1..=total).filter(|other| *other != page).collect();
        single.delete_pages(&others);
        single.prune_objects();
        single.compress();
        if save_pdf(
            &mut single,
            &folder.join(format!("{stem}-sayfa-{page}.pdf")),
        ) {
            outputs += 1;
        }
    }
    if outputs == 0 {
        return Err(fail("SPLIT_FAILED", "PDF sayfalara ayrılamadı."));
    }
    Ok(Done::new(
        format!("{outputs} sayfa ayrı PDF oldu: {}.", name_of(&folder)),
        Some(listing(&[folder])),
    ))
}
