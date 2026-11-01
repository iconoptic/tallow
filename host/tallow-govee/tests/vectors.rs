//! Every entry in `vectors.json` must come out of the codec byte for byte, refusals included.
//! The file is written by `tools/dump_h7020_vectors.py` from the pinned Python codec.
//! A vector kind this test does not know fails the test, so nothing is skipped silently.

use serde_json::{json, Value};
use tallow_govee::frame::{self, Frame, PaintGroup};
use tallow_govee::scene;
use tallow_govee::status::{self, ColourMode, Reply, Segment};
use tallow_govee::Refusal;

const VECTORS: &str = include_str!("vectors.json");

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn unhex(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&text[at..at + 2], 16).expect("vector hex is valid"))
        .collect()
}

fn packets(frames: &[Frame]) -> Value {
    json!({ "packets": frames.iter().map(|frame| hex(frame)).collect::<Vec<_>>() })
}

/// Refusals are compared with their Python text, `ValueError: ...`.
fn refused(error: &Refusal) -> Value {
    json!({ "error": format!("ValueError: {error}") })
}

fn int(input: &Value, key: &str) -> i64 {
    input[key].as_i64().expect("vector integer")
}

fn rgb(value: &Value) -> [i64; 3] {
    let channels: Vec<i64> = value
        .as_array()
        .expect("vector rgb list")
        .iter()
        .map(|channel| channel.as_i64().expect("vector channel"))
        .collect();
    channels.try_into().expect("three channels")
}

fn paint_groups(input: &Value) -> Vec<PaintGroup> {
    input["groups"]
        .as_array()
        .expect("vector groups")
        .iter()
        .map(|group| PaintGroup {
            bulbs: group["bulbs"]
                .as_array()
                .expect("vector bulbs")
                .iter()
                .map(|bulb| bulb.as_i64().expect("vector bulb"))
                .collect(),
            rgb: rgb(&group["rgb"]),
        })
        .collect()
}

fn speed_of(input: &Value) -> Option<i64> {
    input["speed"].as_i64()
}

fn query(name: &str) -> Frame {
    match name {
        "firmware" => frame::firmware_query(),
        "hardware" => frame::hardware_query(),
        "configured_strings" => frame::configured_strings_query(),
        "power" => frame::power_query(),
        "brightness" => frame::brightness_query(),
        other => panic!("unknown query vector {other}"),
    }
}

/// The colour-mode `fields` the dump records from the parsed body.
fn colour_fields(body: ColourMode) -> Value {
    match body {
        ColourMode::Static { sub, kelvin } => json!({
            "domain": "colormode",
            "mode": "static",
            "sub": sub,
            "kelvin": kelvin,
        }),
        ColourMode::Scene { scene_id } => json!({
            "domain": "colormode",
            "mode": "scene",
            "scene_id": scene_id,
        }),
        ColourMode::Diy { code } => json!({
            "domain": "colormode",
            "mode": "diy",
            "code": code,
        }),
        ColourMode::Music {
            mode_id,
            sensitivity,
            selector,
            has_fixed_colour,
            rgb,
        } => {
            let mut value = json!({
                "domain": "colormode",
                "has_fixed_colour": u8::from(has_fixed_colour),
                "mode": "music",
                "mode_id": mode_id,
                "sensitivity": sensitivity,
            });
            if let Some(selector) = selector {
                value["selector"] = json!(selector);
            }
            if let Some(rgb) = rgb {
                value["rgb"] = json!(rgb);
            }
            value
        }
        ColourMode::Unparsed { .. } => json!({ "domain": "colormode" }),
    }
}

/// The Python `fields` shape of a parsed reply, which the vectors record.
fn fields(reply: Option<Reply>) -> Value {
    let Some(reply) = reply else {
        return json!({ "fields": null });
    };
    let value = match reply {
        Reply::Power { is_on } => json!({ "domain": "power", "is_on": is_on }),
        Reply::Brightness { percent } => {
            json!({ "domain": "brightness", "brightness_pct": percent })
        }
        Reply::ColourMode(body) => colour_fields(body),
        Reply::FirmwareVersion { text } => json!({ "domain": "fw_version", "text": text }),
        Reply::HardwareVersion { text } => json!({ "domain": "hw_version", "text": text }),
        Reply::ConfiguredStrings { count } => {
            json!({ "domain": "configured_strings", "count": count })
        }
        Reply::PowerOffMemory { .. } => json!({ "domain": "power_off_memory" }),
        Reply::Segments { .. } => json!({ "domain": "segments" }),
        Reply::Unknown { domain } => json!({ "domain": domain.to_string() }),
    };
    json!({ "fields": value })
}

/// What the codec produces for one vector, shaped like the vector's `result`.
fn actual(vector: &Value) -> Value {
    let kind = vector["kind"].as_str().expect("vector kind");
    let input = &vector["input"];
    match kind {
        "query" => packets(&[query(input["query"].as_str().expect("query name"))]),
        "power" => packets(&[frame::power(input["on"].as_bool().expect("power flag"))]),
        "brightness" => packets(&[frame::brightness(int(input, "level"))]),
        "color" => match frame::colour(
            int(vector, "strings"),
            [int(input, "r"), int(input, "g"), int(input, "b")],
        ) {
            Ok(frame) => packets(&[frame]),
            Err(error) => refused(&error),
        },
        "paint" => match frame::paint(int(vector, "strings"), &paint_groups(input)) {
            Ok(frames) => packets(&frames),
            Err(error) => refused(&error),
        },
        "scene" => {
            let name = input["name"].as_str().expect("scene name");
            match scene::script(name, speed_of(input)) {
                Ok(script) => json!({
                    "name": script.name,
                    "category": script.category,
                    "speed": script.speed,
                    "packets": script.packets.iter().map(|frame| hex(frame)).collect::<Vec<_>>(),
                }),
                Err(error) => refused(&error),
            }
        }
        "status" => {
            let reply = unhex(input["reply"].as_str().expect("status reply hex"));
            fields(status::parse(&reply))
        }
        other => panic!("vector kind {other} has no check in this test"),
    }
}

#[test]
fn every_vector_in_vectors_json_matches_the_python_codec() {
    let file: Value = serde_json::from_str(VECTORS).expect("vectors.json parses");
    let vectors = file["vectors"].as_array().expect("vectors list");
    assert_eq!(
        file["counts"]["total"].as_u64(),
        Some(vectors.len() as u64),
        "counts.total in vectors.json matches its list"
    );

    let mut checked = 0usize;
    for vector in vectors {
        let id = vector["id"].as_str().expect("vector id");
        let produced = actual(vector);
        assert_eq!(
            produced, vector["result"],
            "vector {id} differs from the Python codec"
        );
        checked += 1;
    }

    println!(
        "checked {checked} of {} vectors from vectors.json",
        vectors.len()
    );
    assert_eq!(
        checked,
        vectors.len(),
        "every vector was checked, none skipped"
    );
}

#[test]
fn segment_reply_keeps_its_three_records_in_order() {
    let reply = unhex("aaa50a250102032604050627070809aabbccdd20");
    assert_eq!(
        status::parse(&reply),
        Some(Reply::Segments {
            group: 10,
            segments: [
                Segment {
                    brightness: 0x25,
                    rgb: [1, 2, 3],
                },
                Segment {
                    brightness: 0x26,
                    rgb: [4, 5, 6],
                },
                Segment {
                    brightness: 0x27,
                    rgb: [7, 8, 9],
                },
            ],
        })
    );
}
