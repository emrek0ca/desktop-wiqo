//! Reports on a folder: duplicate files, disk usage, the most recent files.

use super::*;

// ----- reports -----

pub(super) fn find_duplicates(request: &Request, limit: usize) -> Result<Done> {
    let folder = existing_folder(&request.path)?;
    let files = regular_files(&folder, true, 5_000);
    let mut by_size: HashMap<u64, Vec<PathBuf>> = HashMap::new();
    for file in &files {
        let bytes = size(file);
        if bytes > 0 {
            by_size.entry(bytes).or_default().push(file.clone());
        }
    }
    let mut groups: Vec<Vec<PathBuf>> = Vec::new();
    for (_, same) in by_size.into_iter().filter(|(_, same)| same.len() > 1) {
        let mut by_hash: HashMap<String, Vec<PathBuf>> = HashMap::new();
        for file in same {
            let Ok(mut handle) = fs::File::open(&file) else {
                continue;
            };
            let mut hasher = Sha256::new();
            if std::io::copy(&mut handle, &mut hasher).is_ok() {
                by_hash
                    .entry(hex::encode(hasher.finalize()))
                    .or_default()
                    .push(file);
            }
        }
        groups.extend(by_hash.into_values().filter(|group| group.len() > 1));
    }
    groups.sort_by_key(|group| std::cmp::Reverse(size(&group[0]) * group.len() as u64));
    if groups.is_empty() {
        return Ok(Done::new(
            "Kopya dosya yok.",
            Some(report(
                &fsx::abbreviate(&folder),
                &format!("{} dosyada kopya yok.", files.len()),
            )),
        ));
    }
    let lines: Vec<String> = groups
        .iter()
        .take(limit)
        .enumerate()
        .map(|(index, group)| {
            format!(
                "{}. {} × {}: {}",
                index + 1,
                human(size(&group[0])),
                group.len(),
                group
                    .iter()
                    .map(|path| short(path))
                    .collect::<Vec<_>>()
                    .join(" | ")
            )
        })
        .collect();
    let wasted: u64 = groups
        .iter()
        .map(|group| size(&group[0]) * (group.len() as u64 - 1))
        .sum();
    Ok(Done::new(
        format!(
            "{} kopya grubu bulundu; silinirse {} boşalır.",
            groups.len(),
            human(wasted)
        ),
        Some(report(&fsx::abbreviate(&folder), &lines.join("\n"))),
    ))
}

pub(super) fn disk_usage(request: &Request, limit: usize) -> Result<Done> {
    let folder = existing_folder(&request.path)?;
    let disks = sysinfo::Disks::new_with_refreshed_list();
    let disk = disks
        .list()
        .iter()
        .filter(|disk| folder.starts_with(disk.mount_point()))
        .max_by_key(|disk| disk.mount_point().as_os_str().len());
    let (free, total) = disk
        .map(|disk| (human(disk.available_space()), human(disk.total_space())))
        .unwrap_or_else(|| ("?".into(), "?".into()));
    let files = regular_files(&folder, true, 20_000);
    let mut largest: Vec<&PathBuf> = files.iter().collect();
    largest.sort_by_key(|path| std::cmp::Reverse(size(path)));
    let mut folders: HashMap<String, u64> = HashMap::new();
    for file in &files {
        if let Ok(relative) = file.strip_prefix(&folder) {
            let mut parts = relative.components();
            if let (Some(top), Some(_)) = (parts.next(), parts.next()) {
                *folders
                    .entry(top.as_os_str().to_string_lossy().to_string())
                    .or_default() += size(file);
            }
        }
    }
    let mut folders: Vec<(String, u64)> = folders.into_iter().collect();
    folders.sort_by_key(|(_, bytes)| std::cmp::Reverse(*bytes));
    let total_bytes: u64 = files.iter().map(|path| size(path)).sum();
    let mut lines = vec![
        format!("Disk: {free} boş / {total}"),
        format!("{} toplam: {}", short(&folder), human(total_bytes)),
        format!(
            "En büyük klasörler: {}",
            folders
                .iter()
                .take(8)
                .map(|(name, bytes)| format!("{name}: {}", human(*bytes)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        "En büyük dosyalar:".into(),
    ];
    lines.extend(
        largest
            .iter()
            .take(limit)
            .map(|path| format!("- {} {}", short(path), human(size(path)))),
    );
    Ok(Done::new(
        format!("Disk: {free} boş."),
        Some(report(&fsx::abbreviate(&folder), &lines.join("\n"))),
    ))
}

pub(super) fn recent_files(limit: usize) -> Result<Done> {
    let home = fsx::home();
    let mut files: Vec<PathBuf> = [
        dirs::desktop_dir(),
        dirs::document_dir(),
        dirs::download_dir(),
    ]
    .into_iter()
    .flatten()
    .chain(["Desktop", "Documents", "Downloads"].map(|name| home.join(name)))
    .collect::<std::collections::BTreeSet<_>>()
    .into_iter()
    .flat_map(|folder| regular_files(&folder, true, 3_000))
    .collect();
    files.sort_by_key(|path| std::cmp::Reverse(modified(path)));
    files.dedup();
    files.truncate(limit);
    Ok(Done::new(
        format!("Son değişen {} dosya.", files.len()),
        Some(listing(&files)),
    ))
}
