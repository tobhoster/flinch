//! A Radarr that keeps what it is sent, starting from `fixtures/radarr`, and
//! refuses a profile as Radarr's validators do: every quality and every custom
//! format must be in it, and `minUpgradeFormatScore` must be at least 1
//! (<https://github.com/Radarr/Radarr/blob/develop/src/Radarr.Api.V3/Profiles/Quality/QualityProfileController.cs>).
//! A new custom format joins every profile at score 0, as in Radarr.

use super::fake::{self, Request};
use serde_json::{json, Value};
use std::path::PathBuf;

pub const KEY: &str = "radarr-key";

pub struct State {
    pub custom_formats: Vec<Value>,
    pub profiles: Vec<Value>,
    pub definitions: Vec<Value>,
    pub schema: Value,
    pub languages: Value,
    /// Answer every request with a 500.
    pub down: bool,
}

fn fixture(name: &str) -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/trash/fixtures/radarr").join(name);
    serde_json::from_str(&std::fs::read_to_string(path).expect("a Radarr fixture")).expect("fixture JSON")
}

fn rows(value: Value) -> Vec<Value> {
    value.as_array().cloned().expect("a fixture array")
}

impl State {
    pub fn fixtures() -> Self {
        Self {
            custom_formats: rows(fixture("customformat.json")),
            profiles: rows(fixture("qualityprofile.json")),
            definitions: rows(fixture("qualitydefinition.json")),
            schema: fixture("qualityprofile-schema.json"),
            languages: fixture("language.json"),
            down: false,
        }
    }

    fn quality_ids(items: &Value, into: &mut Vec<i64>) {
        for item in items.as_array().into_iter().flatten() {
            if let Some(id) = item.pointer("/quality/id").and_then(Value::as_i64) {
                into.push(id);
            }
            Self::quality_ids(&item["items"], into);
        }
    }

    fn refuse_profile(&self, profile: &Value) -> Option<&'static str> {
        let (mut wanted, mut given) = (Vec::new(), Vec::new());
        Self::quality_ids(&self.schema["items"], &mut wanted);
        Self::quality_ids(&profile["items"], &mut given);
        if wanted.iter().any(|id| !given.contains(id)) {
            return Some("Must contain all qualities");
        }
        let formats: Vec<u64> = profile["formatItems"].as_array().into_iter().flatten().filter_map(|f| f["format"].as_u64()).collect();
        if self.custom_formats.iter().any(|cf| !formats.contains(&cf["id"].as_u64().unwrap_or(0))) {
            return Some("All Custom Formats and no extra ones need to be present inside your Profile! Try refreshing your browser.");
        }
        if profile["minUpgradeFormatScore"].as_i64().unwrap_or(0) < 1 {
            return Some("'Min Upgrade Format Score' must be greater than or equal to '1'.");
        }
        None
    }

    fn next_id(rows: &[Value]) -> u64 {
        rows.iter().filter_map(|row| row["id"].as_u64()).max().unwrap_or(0) + 1
    }

    fn handle(&mut self, request: &Request) -> (u16, String) {
        if self.down {
            return (500, r#"{"message":"database is locked"}"#.to_string());
        }
        if request.header("x-api-key") != Some(KEY) {
            return (401, String::new());
        }
        let body: Value = serde_json::from_str(&request.body).unwrap_or(Value::Null);
        let path = request.path.split('?').next().unwrap_or_default();
        let id = path.rsplit('/').next().and_then(|id| id.parse::<u64>().ok());
        let ok = |value: &Value| (200, value.to_string());
        match (request.method.as_str(), path) {
            ("GET", "/api/v3/customformat") => ok(&Value::Array(self.custom_formats.clone())),
            ("GET", "/api/v3/qualityprofile") => ok(&Value::Array(self.profiles.clone())),
            ("GET", "/api/v3/qualityprofile/schema") => ok(&self.schema),
            ("GET", "/api/v3/qualitydefinition") => ok(&Value::Array(self.definitions.clone())),
            ("GET", "/api/v3/language") => ok(&self.languages),
            ("POST", "/api/v3/customformat") => {
                let mut created = body;
                let id = Self::next_id(&self.custom_formats);
                created["id"] = json!(id);
                for profile in &mut self.profiles {
                    if let Some(items) = profile["formatItems"].as_array_mut() {
                        items.push(json!({ "format": id, "name": created["name"], "score": 0 }));
                    }
                }
                self.custom_formats.push(created.clone());
                (201, created.to_string())
            }
            ("PUT", p) if p.starts_with("/api/v3/customformat/") => {
                let Some(row) = self.custom_formats.iter_mut().find(|cf| cf["id"].as_u64() == id) else { return (404, String::new()) };
                *row = body;
                (202, row.to_string())
            }
            ("DELETE", p) if p.starts_with("/api/v3/qualityprofile/") => {
                let before = self.profiles.len();
                self.profiles.retain(|profile| profile["id"].as_u64() != id);
                if self.profiles.len() == before {
                    return (404, String::new());
                }
                (200, String::new())
            }
            ("DELETE", p) if p.starts_with("/api/v3/customformat/") => {
                self.custom_formats.retain(|cf| cf["id"].as_u64() != id);
                for profile in &mut self.profiles {
                    if let Some(items) = profile["formatItems"].as_array_mut() {
                        items.retain(|item| item["format"].as_u64() != id);
                    }
                }
                (200, String::new())
            }
            ("POST", "/api/v3/qualityprofile") | ("PUT", _) if path.starts_with("/api/v3/qualityprofile") => {
                if let Some(reason) = self.refuse_profile(&body) {
                    return (400, json!([{ "errorMessage": reason }]).to_string());
                }
                let mut profile = body;
                if request.method == "POST" {
                    profile["id"] = json!(Self::next_id(&self.profiles));
                    self.profiles.push(profile.clone());
                    return (201, profile.to_string());
                }
                let Some(row) = self.profiles.iter_mut().find(|p| p["id"].as_u64() == id) else { return (404, String::new()) };
                *row = profile;
                (202, row.to_string())
            }
            ("PUT", "/api/v3/qualitydefinition/update") => {
                for update in body.as_array().into_iter().flatten() {
                    if let Some(row) = self.definitions.iter_mut().find(|d| d["id"] == update["id"]) {
                        *row = update.clone();
                    }
                }
                (202, body.to_string())
            }
            _ => (404, String::new()),
        }
    }
}

pub fn serve(mut state: State) -> fake::Server {
    fake::serve(move |request| state.handle(request))
}
