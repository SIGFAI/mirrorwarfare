use crate::MenuMapList;

pub fn map_label(map: &str) -> String {
    if let Some(name) = map.strip_prefix("mec:") {
        return format!("Catalyst: {}", assets::arena_title(name)).to_uppercase();
    }
    map.split_once(':')
        .map_or(map, |(_, name)| name)
        .trim_start_matches("mp_")
        .replace('_', " ")
        .to_uppercase()
}

pub fn map_preview(map: &str) -> String {
    match map.split_once(':').unwrap_or(("iw4", map)) {
        (_, "") | ("mec", _) => String::new(),
        ("t5", stem) => format!(
            "t5:material/menu_mp_map_select_{}_big",
            stem.trim_start_matches("mp_")
        ),
        (namespace, stem) => format!("{namespace}:material/preview_{stem}"),
    }
}

pub fn pack_maps(maps: &MenuMapList, pack: usize) -> &[String] {
    maps.0.get(pack).map_or(&[], |pack| pack.maps.as_slice())
}
