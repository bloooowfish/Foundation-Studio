use serde::Serialize;
use serde_json::Value;

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PresetFixtureEntry {
    pub kind_key: String,
    pub raw_kind: String,
    pub name: String,
    pub color: String,
    pub item_id: u32,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LocalPresetMeta {
    pub house_size_raw: String,
    pub normalized_plot_size: Option<u8>,
    pub is_apartment: bool,
    pub size_key: String,
    pub indoor_capacity_hint: Option<u16>,
    pub district_raw: String,
    pub style_key: String,
    pub indoor_count: usize,
    pub indoor_dyed: usize,
    pub indoor_material: usize,
    pub outdoor_count: usize,
    pub outdoor_dyed: usize,
    pub interior_scale: f64,
    pub exterior_scale: f64,
    pub interior_fixtures: Vec<PresetFixtureEntry>,
    pub exterior_fixtures: Vec<PresetFixtureEntry>,
}

/// Decodes UTF-8 / UTF-16 LE / UTF-16 BE preset file bytes cleanly using BOM inspection.
pub fn decode_bytes_with_bom(bytes: &[u8]) -> String {
    if bytes.starts_with(&[0xFF, 0xFE]) {
        let mut units = Vec::with_capacity((bytes.len() - 2) / 2);
        for chunk in bytes[2..].chunks_exact(2) {
            units.push(u16::from_le_bytes([chunk[0], chunk[1]]));
        }
        return String::from_utf16_lossy(&units);
    }
    if bytes.starts_with(&[0xFE, 0xFF]) {
        let mut units = Vec::with_capacity((bytes.len() - 2) / 2);
        for chunk in bytes[2..].chunks_exact(2) {
            units.push(u16::from_be_bytes([chunk[0], chunk[1]]));
        }
        return String::from_utf16_lossy(&units);
    }
    let slice = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    String::from_utf8_lossy(slice).into_owned()
}

pub fn normalize_interior_style_key(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return "unspecified".to_string();
    }
    let lower = trimmed.to_ascii_lowercase();
    // Prioritize "dark minimalist" before "minimalist" to avoid substring collision.
    if lower.contains("dark minimalist")
        || lower.contains("schlicht & dunkel")
        || lower.contains("sobre (noir)")
        || trimmed.contains("モノトーン")
    {
        return "dark_minimalist".to_string();
    }
    if lower.contains("minimalist")
        || lower.contains("schlicht")
        || lower.contains("sobre")
        || trimmed.contains("シンプル")
    {
        return "minimalist".to_string();
    }
    if lower.contains("lavender") || lower.contains("lavendel") || lower.contains("lavandi") {
        return "lavender_beds".to_string();
    }
    if lower.contains("mist")
        || lower.contains("dorf des nebels")
        || lower.contains("brumé")
        || lower.contains("brume")
    {
        return "mist".to_string();
    }
    if lower.contains("goblet") || lower.contains("kelchkuppe") || lower.contains("coupe") {
        return "goblet".to_string();
    }
    if lower.contains("shirogane") {
        return "shirogane".to_string();
    }
    if lower.contains("empyreum") || lower.contains("empyr") || trimmed.contains("エンピレアム")
    {
        return "empyreum".to_string();
    }
    "unknown".to_string()
}

pub fn parse_preset_metadata(raw_json: &str) -> Result<LocalPresetMeta, String> {
    let value: Value =
        serde_json::from_str(raw_json).map_err(|e| format!("Failed to parse JSON: {e}"))?;
    let Some(obj) = value.as_object() else {
        return Err("JSON root must be an object ({}).".to_string());
    };

    let house_size_raw = obj
        .get("houseSize")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let (normalized_plot_size, is_apartment, size_key, indoor_capacity_hint) =
        normalize_house_size(&house_size_raw);

    let interior_scale = obj
        .get("interiorScale")
        .and_then(Value::as_f64)
        .unwrap_or(1.0);
    let exterior_scale = obj
        .get("exteriorScale")
        .and_then(Value::as_f64)
        .unwrap_or(1.0);

    let mut district_raw = String::new();
    let mut interior_fixtures = Vec::new();
    if let Some(arr) = obj.get("interiorFixture").and_then(Value::as_array) {
        for item in arr {
            let Some(f) = item.as_object() else {
                continue;
            };
            let kind = f
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim();
            let name = f
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim();
            if name.is_empty() {
                continue;
            }
            if kind.eq_ignore_ascii_case("District") {
                district_raw = name.to_string();
                continue;
            }
            if kind.eq_ignore_ascii_case("Side Door")
                || kind.to_ascii_lowercase().contains("hidden")
            {
                continue;
            }
            let color = f.get("color").and_then(Value::as_str).unwrap_or_default();
            let item_id = f.get("itemId").and_then(Value::as_u64).unwrap_or(0) as u32;
            interior_fixtures.push(build_fixture_entry(kind, name, color, item_id));
        }
    }

    let mut exterior_fixtures = Vec::new();
    if let Some(arr) = obj.get("exteriorFixture").and_then(Value::as_array) {
        for item in arr {
            let Some(f) = item.as_object() else {
                continue;
            };
            let kind = f
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim();
            let name = f
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim();
            if name.is_empty() {
                continue;
            }
            let color = f.get("color").and_then(Value::as_str).unwrap_or_default();
            let item_id = f.get("itemId").and_then(Value::as_u64).unwrap_or(0) as u32;
            exterior_fixtures.push(build_fixture_entry(kind, name, color, item_id));
        }
    }

    let mut int_stats = FurnitureStats::default();
    if let Some(arr) = obj.get("interiorFurniture").and_then(Value::as_array) {
        count_furniture_recursive(arr, &mut int_stats);
    }

    let mut ext_stats = FurnitureStats::default();
    if let Some(arr) = obj.get("exteriorFurniture").and_then(Value::as_array) {
        count_furniture_recursive(arr, &mut ext_stats);
    }

    Ok(LocalPresetMeta {
        house_size_raw,
        normalized_plot_size,
        is_apartment,
        size_key,
        indoor_capacity_hint,
        style_key: normalize_interior_style_key(&district_raw),
        district_raw,
        indoor_count: int_stats.total,
        indoor_dyed: int_stats.dyed,
        indoor_material: int_stats.material,
        outdoor_count: ext_stats.total,
        outdoor_dyed: ext_stats.dyed,
        interior_scale,
        exterior_scale,
        interior_fixtures,
        exterior_fixtures,
    })
}

fn normalize_house_size(raw: &str) -> (Option<u8>, bool, String, Option<u16>) {
    let lower = raw.trim().to_ascii_lowercase();
    if lower.is_empty() {
        return (None, false, "unknown".to_string(), None);
    }
    if lower == "apt"
        || lower.contains("apart")
        || lower.contains("chamber")
        || lower.contains("fc room")
        || lower.contains("private room")
        || lower == "room"
    {
        return (Some(0), true, "apartment".to_string(), Some(100));
    }
    if lower == "l" || lower.contains("large") || lower.contains("mansion") {
        return (Some(2), false, "large".to_string(), Some(600));
    }
    if lower == "s" || lower.contains("small") || lower.contains("cottage") {
        return (Some(0), false, "small".to_string(), Some(300));
    }
    if lower == "m" || lower.contains("medium") || lower.contains("house") {
        return (Some(1), false, "medium".to_string(), Some(450));
    }
    (None, false, "other".to_string(), None)
}

#[derive(Default)]
struct FurnitureStats {
    total: usize,
    dyed: usize,
    material: usize,
}

fn count_furniture_recursive(arr: &[Value], stats: &mut FurnitureStats) {
    for item in arr {
        let Some(obj) = item.as_object() else {
            continue;
        };
        stats.total += 1;

        if let Some(props) = obj.get("properties").and_then(Value::as_object) {
            let has_color = props
                .get("color")
                .and_then(Value::as_str)
                .is_some_and(|c| !c.trim().is_empty());
            let has_stain = props
                .get("stainId")
                .and_then(Value::as_u64)
                .is_some_and(|id| id > 0)
                || props.contains_key("stainId")
                    && !has_color
                    && props.get("stainId").is_some_and(|v| v.as_u64() != Some(0));
            if has_color || has_stain {
                stats.dyed += 1;
            }
            if props.get("material").is_some_and(Value::is_object) {
                stats.material += 1;
            }
        }

        if let Some(attachments) = obj.get("attachments").and_then(Value::as_array) {
            count_furniture_recursive(attachments, stats);
        }
    }
}

fn build_fixture_entry(kind: &str, name: &str, color: &str, item_id: u32) -> PresetFixtureEntry {
    let raw_kind = kind.trim().to_string();
    let lower = raw_kind.to_ascii_lowercase();
    let kind_key = match lower.as_str() {
        "wall" | "walls" => "wall",
        "floor" | "flooring" => "flooring",
        "ceiling" | "light" | "lighting" | "chandelier" => "ceiling",
        "upper wall" => "upper_wall",
        "upper flooring" => "upper_flooring",
        "basement wall" => "basement_wall",
        "basement flooring" => "basement_flooring",
        "roof" => "roof",
        "exterior wall" => "exterior_wall",
        "window" | "windows" => "window",
        "door" => "door",
        "fence" => "fence",
        "awning" => "awning",
        "sign" | "placard" => "sign",
        "roof decoration" | "roof decor" => "roof_decoration",
        "wall decoration" | "exterior wall decor" | "wall decor" => "wall_decoration",
        _ => "",
    }
    .to_string();

    PresetFixtureEntry {
        kind_key,
        raw_kind,
        name: name.to_string(),
        color: color.trim().to_string(),
        item_id,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_utf16le_bom_nested_attachments_dyes_and_fixtures() {
        let json_str = r#"{
            "houseSize": "Large",
            "interiorScale": 100,
            "interiorFixture": [
                { "type": "District", "name": "Dark Minimalist" },
                { "type": "Side Door", "name": "Ignored Door" },
                { "type": "Wall", "name": "Blank White Interior Wall", "itemId": 20718 }
            ],
            "interiorFurniture": [
                {
                    "name": "Table",
                    "properties": { "color": "FFFFFF", "material": { "itemId": 100 } },
                    "attachments": [
                        {
                            "name": "Lamp",
                            "properties": { "stainId": 12 }
                        },
                        {
                            "name": "Book",
                            "properties": {}
                        }
                    ]
                }
            ]
        }"#;

        let mut utf16_bytes = vec![0xFFu8, 0xFEu8];
        for unit in json_str.encode_utf16() {
            utf16_bytes.extend_from_slice(&unit.to_le_bytes());
        }
        let decoded = decode_bytes_with_bom(&utf16_bytes);
        let meta = parse_preset_metadata(&decoded).unwrap();

        assert_eq!(meta.normalized_plot_size, Some(2));
        assert!(!meta.is_apartment);
        assert_eq!(meta.size_key, "large");
        assert_eq!(meta.indoor_capacity_hint, Some(600));
        assert_eq!(meta.style_key, "dark_minimalist");
        assert_eq!(meta.indoor_count, 3);
        assert_eq!(meta.indoor_dyed, 2);
        assert_eq!(meta.indoor_material, 1);
        assert_eq!(
            meta.interior_fixtures,
            vec![PresetFixtureEntry {
                kind_key: "wall".to_string(),
                raw_kind: "Wall".to_string(),
                name: "Blank White Interior Wall".to_string(),
                color: String::new(),
                item_id: 20718,
            }]
        );
    }

    #[test]
    fn prioritizes_dark_minimalist_before_minimalist() {
        assert_eq!(
            normalize_interior_style_key("Dark Minimalist"),
            "dark_minimalist"
        );
        assert_eq!(normalize_interior_style_key("Minimalist"), "minimalist");
        assert_eq!(
            normalize_interior_style_key("Lavender Beds"),
            "lavender_beds"
        );
        assert_eq!(normalize_interior_style_key(""), "unspecified");
    }
}
