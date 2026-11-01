//! The H7020 scene catalogue and the scene script builder.
//!
//! `scenes.json` is the table `tools/dump_h7020_vectors.py` writes from the pinned codec's
//! `OFFICIAL_MODEL_SCENES`. Its `param` is the raw type-2 body, before any speed is applied.
//! A speed option is applied here, as `apply_scene_speed` does in `native_scenes.py`.

use std::sync::OnceLock;

use serde_json::Value;

use crate::frame::{self, Frame};
use crate::layered::{self, Layer};
use crate::{b64, refuse, Refusal};

const CATALOGUE_JSON: &str = include_str!("../scenes.json");

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrightnessSpeed {
    pub block: i64,
    pub values: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page {
    pub page: i64,
    pub move_in: Vec<u8>,
    pub move_all: Vec<u8>,
    pub colour_speed: Vec<u8>,
    pub brightness: Vec<BrightnessSpeed>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Speed {
    pub default_index: i64,
    pub option_count: i64,
    pub pages: Vec<Page>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scene {
    pub key: String,
    pub name: String,
    pub category: String,
    pub code: i64,
    pub scene_type: i64,
    pub selector_only: bool,
    /// The raw type-2 body, before speed.
    pub param: Vec<u8>,
    pub speed: Option<Speed>,
}

/// The catalogue, parsed once from `scenes.json`.
pub fn catalogue() -> Result<&'static [Scene], Refusal> {
    static CATALOGUE: OnceLock<Result<Vec<Scene>, Refusal>> = OnceLock::new();
    match CATALOGUE.get_or_init(|| parse_catalogue(CATALOGUE_JSON)) {
        Ok(scenes) => Ok(scenes.as_slice()),
        Err(error) => Err(error.clone()),
    }
}

fn parse_catalogue(text: &str) -> Result<Vec<Scene>, Refusal> {
    let root: Value =
        serde_json::from_str(text).map_err(|error| Refusal(format!("scenes.json: {error}")))?;
    if root.get("schema").and_then(Value::as_i64) != Some(1)
        || root.get("model").and_then(Value::as_str) != Some("H7020")
    {
        return refuse("scenes.json is not the H7020 catalogue, schema 1");
    }
    let Some(entries) = root.get("scenes").and_then(Value::as_array) else {
        return refuse("scenes.json has no scene list");
    };
    entries
        .iter()
        .map(|entry| {
            parse_scene(entry).ok_or_else(|| Refusal("scenes.json: malformed scene".into()))
        })
        .collect()
}

fn parse_scene(entry: &Value) -> Option<Scene> {
    let param = match entry.get("param")?.as_str()? {
        "" => Vec::new(),
        text => b64::decode(text)?,
    };
    let speed = match entry.get("speed") {
        None | Some(Value::Null) => None,
        Some(speed) => Some(parse_speed(speed)?),
    };
    Some(Scene {
        key: entry.get("key")?.as_str()?.to_owned(),
        name: entry.get("name")?.as_str()?.to_owned(),
        category: entry.get("category")?.as_str()?.to_owned(),
        code: entry.get("code")?.as_i64()?,
        scene_type: entry.get("scene_type")?.as_i64()?,
        selector_only: entry.get("selector_only")?.as_bool()?,
        param,
        speed,
    })
}

fn parse_speed(value: &Value) -> Option<Speed> {
    let pages = value
        .get("pages")?
        .as_array()?
        .iter()
        .map(parse_page)
        .collect::<Option<Vec<_>>>()?;
    Some(Speed {
        default_index: value.get("default_index")?.as_i64()?,
        option_count: value.get("option_count")?.as_i64()?,
        pages,
    })
}

fn parse_page(value: &Value) -> Option<Page> {
    let brightness = value
        .get("brightness")?
        .as_array()?
        .iter()
        .map(|block| {
            Some(BrightnessSpeed {
                block: block.get("block")?.as_i64()?,
                values: bytes(block.get("values")?)?,
            })
        })
        .collect::<Option<Vec<_>>>()?;
    Some(Page {
        page: value.get("page")?.as_i64()?,
        move_in: bytes(value.get("move_in")?)?,
        move_all: bytes(value.get("move_all")?)?,
        colour_speed: bytes(value.get("colour_speed")?)?,
        brightness,
    })
}

fn bytes(value: &Value) -> Option<Vec<u8>> {
    value
        .as_array()?
        .iter()
        .map(|item| u8::try_from(item.as_u64()?).ok())
        .collect()
}

/// Lower-cases and collapses whitespace, the way `scene_entry` compares names.
fn normalise(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Finds a scene by its key or its label, ignoring case and extra spaces.
pub fn find(name: &str) -> Result<&'static Scene, Refusal> {
    if name.trim().is_empty() {
        return refuse("scene name is required");
    }
    let wanted = normalise(name);
    catalogue()?
        .iter()
        .find(|scene| scene.key == wanted || normalise(&scene.name) == wanted)
        .ok_or_else(|| Refusal(format!("unknown scene {name}")))
}

/// The frames to send for one scene. `speed` is None for the default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Script {
    pub name: String,
    pub category: String,
    /// The speed index used, or None for a scene with no speed control.
    pub speed: Option<i64>,
    pub packets: Vec<Frame>,
}

pub fn script(name: &str, speed: Option<i64>) -> Result<Script, Refusal> {
    let scene = find(name)?;
    let packets = frames_for(scene, speed)?;
    let resolved = match speed {
        Some(index) => Some(index),
        None => scene.speed.as_ref().map(|speed| speed.default_index),
    };
    Ok(Script {
        name: scene.name.clone(),
        category: scene.category.clone(),
        speed: resolved,
        packets,
    })
}

fn frames_for(scene: &Scene, speed: Option<i64>) -> Result<Vec<Frame>, Refusal> {
    if scene.selector_only {
        if speed.is_some() {
            return refuse("selector-only native scenes do not accept speed_index");
        }
        return Ok(vec![frame::scene_activation(scene.code)]);
    }
    let body = upload_body(scene, speed)?;
    let mut frames = if body.is_empty() {
        Vec::new()
    } else {
        let Ok(scene_type) = u8::try_from(scene.scene_type) else {
            return refuse(format!("scene type {} is not a byte", scene.scene_type));
        };
        frame::upload(scene_type, &body)?
    };
    frames.push(frame::scene_activation(scene.code));
    Ok(frames)
}

/// The body to upload, with the requested speed applied.
fn upload_body(scene: &Scene, speed: Option<i64>) -> Result<Vec<u8>, Refusal> {
    let Some(catalogue_speed) = &scene.speed else {
        if speed.is_some() {
            return refuse("This scene does not expose a documented Speed control");
        }
        return Ok(scene.param.clone());
    };
    let index = speed.unwrap_or(catalogue_speed.default_index);
    if !(0..catalogue_speed.option_count).contains(&index) {
        return refuse(format!(
            "scene speed index {index} outside 0..{}",
            catalogue_speed.option_count - 1
        ));
    }
    apply_speed(&scene.param, catalogue_speed, index)
}

/// Writes one speed option into the body's speed bytes. An index past the end
/// of an option list is clamped to its last entry, as the Python does.
fn apply_speed(param: &[u8], speed: &Speed, index: i64) -> Result<Vec<u8>, Refusal> {
    let (mut layers, padding) = layered::decode(param)?;
    for page in &speed.pages {
        let Some(layer) = usize::try_from(page.page)
            .ok()
            .and_then(|at| layers.get_mut(at))
        else {
            continue;
        };
        apply_page(layer, page, index);
    }
    layered::encode(&layers, padding)
}

fn apply_page(layer: &mut Layer, page: &Page, index: i64) {
    for block in &page.brightness {
        let Some(pattern) = usize::try_from(block.block)
            .ok()
            .and_then(|at| layer.brightness.get_mut(at))
        else {
            continue;
        };
        if !block.values.is_empty() {
            pattern.change_speed = option(&block.values, index);
        }
    }
    if !page.move_in.is_empty() {
        layer.selected_movement.speed = option(&page.move_in, index);
    }
    if !page.move_all.is_empty() {
        layer.overall_movement.speed = option(&page.move_all, index);
    }
    if !page.colour_speed.is_empty() {
        layer.colour_speed = option(&page.colour_speed, index);
    }
}

/// `options[index]`, clamped to the list. Callers only pass non-empty lists.
fn option(options: &[u8], index: i64) -> u8 {
    let last = options.len().saturating_sub(1);
    options
        .get((index.max(0) as usize).min(last))
        .copied()
        .unwrap_or(0)
}
