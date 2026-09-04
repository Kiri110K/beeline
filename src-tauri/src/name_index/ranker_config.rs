//! The single editable Ranker Configuration (SPEC §6.4).
//!
//! `ranker.json` is strict and atomic at the activation boundary: an absent file selects the
//! embedded default, while any invalid file is left untouched and rejected as a whole. The
//! active config is held separately by `NameIndex`, so failed reloads cannot partially apply.

use std::{
    ffi::OsString,
    fs::{self, File},
    io::{self, BufRead, BufReader, Write},
    os::unix::{
        fs::PermissionsExt,
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    thread,
    time::Duration,
};

use serde::{Deserialize, Serialize};

pub const SCHEMA_VERSION: u32 = 1;
const CONTROL_SOCKET: &str = "ranker-control.sock";

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RankerConfig {
    pub schema_version: u32,
    pub global_retrieval_significant_chars: u32,
    pub text_match: TextMatchWeights,
    pub search_memory: SearchMemoryWeights,
    pub general_usage: GeneralUsageWeights,
    pub context: ContextWeights,
    pub alias: AliasWeights,
    pub item_kind: ItemKindWeights,
    pub penalties: PenaltyWeights,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TextMatchWeights {
    pub exact_name: i64,
    pub prefix_name: i64,
    pub substring_name: i64,
    pub typo_name: i64,
    pub typo_edit_penalty: i64,
    pub path_component: i64,
    pub all_tokens_in_name: i64,
    pub existing_path: i64,
    pub path_scope: i64,
    pub layout_correction_penalty: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SearchMemoryWeights {
    pub max: i64,
    pub usage_max: i64,
    pub saturation_points: u64,
    pub half_life_days: i64,
    pub action_menu_points: u32,
    pub quick_look_points: u32,
    pub completed_action_points: u32,
    pub exact_similarity_milli: i64,
    pub prefix_extension_similarity_milli: i64,
    pub prefix_contraction_similarity_milli: i64,
    pub token_delta_similarity_milli: i64,
    pub edit_similarity_milli: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GeneralUsageWeights {
    pub visit_max: i64,
    pub visit_frequency_cap: u32,
    pub visit_frequency_share_milli: i64,
    pub visit_recency_scale_days: i64,
    pub recents: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContextWeights {
    pub current_location: i64,
    pub pinned_anchor: i64,
    pub known_place: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AliasWeights {
    pub exact: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ItemKindWeights {
    pub file: i64,
    pub directory: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PenaltyWeights {
    pub hidden: i64,
    pub junk: i64,
    pub total_cap: i64,
}

impl Default for RankerConfig {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            global_retrieval_significant_chars: 5,
            text_match: TextMatchWeights {
                exact_name: 5_000_000,
                prefix_name: 4_000_000,
                substring_name: 2_000_000,
                typo_name: 1_500_000,
                typo_edit_penalty: 100_000,
                path_component: 1_000_000,
                all_tokens_in_name: 3_000_000,
                existing_path: 100_000_000,
                path_scope: 500_000,
                layout_correction_penalty: 100_000,
            },
            search_memory: SearchMemoryWeights {
                max: 3_000_000,
                usage_max: 200_000,
                saturation_points: 12,
                half_life_days: 90,
                action_menu_points: 1,
                quick_look_points: 3,
                completed_action_points: 8,
                exact_similarity_milli: 1_000,
                prefix_extension_similarity_milli: 850,
                prefix_contraction_similarity_milli: 700,
                token_delta_similarity_milli: 750,
                edit_similarity_milli: 650,
            },
            general_usage: GeneralUsageWeights {
                visit_max: 30_000,
                visit_frequency_cap: 10,
                visit_frequency_share_milli: 667,
                visit_recency_scale_days: 1,
                recents: 24_000,
            },
            context: ContextWeights {
                current_location: 1_500_000,
                pinned_anchor: 18_000,
                known_place: 20_000,
            },
            alias: AliasWeights { exact: 50_000_000 },
            item_kind: ItemKindWeights {
                file: 0,
                directory: 0,
            },
            penalties: PenaltyWeights {
                hidden: 8_000,
                junk: 40_000,
                total_cap: 48_000,
            },
        }
    }
}

impl RankerConfig {
    pub fn path(app_data_dir: &Path) -> PathBuf {
        app_data_dir.join("ranker.json")
    }

    pub fn parse(text: &str) -> Result<Self, String> {
        let config = serde_json::from_str::<Self>(text)
            .map_err(|error| format!("ranker.json is invalid JSON: {error}"))?;
        config.validate()?;
        Ok(config)
    }

    pub fn load(app_data_dir: &Path) -> Result<Option<Self>, String> {
        let path = Self::path(app_data_dir);
        match fs::read_to_string(&path) {
            Ok(text) => Self::parse(&text).map(Some),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(format!("cannot read {}: {error}", path.display())),
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(format!(
                "unsupported ranker schemaVersion {}",
                self.schema_version
            ));
        }
        if !(1..=64).contains(&self.global_retrieval_significant_chars) {
            return Err("globalRetrievalSignificantChars must be between 1 and 64".to_owned());
        }
        if self.search_memory.saturation_points == 0
            || self.search_memory.half_life_days <= 0
            || self.search_memory.action_menu_points == 0
            || self.search_memory.quick_look_points < self.search_memory.action_menu_points
            || self.search_memory.completed_action_points < self.search_memory.quick_look_points
        {
            return Err(
                "Search Memory curve and signal points must be positive and weak ≤ medium ≤ strong"
                    .to_owned(),
            );
        }
        if self.general_usage.visit_frequency_cap == 0
            || self.general_usage.visit_recency_scale_days <= 0
            || !(0..=1_000).contains(&self.general_usage.visit_frequency_share_milli)
        {
            return Err("General Usage visit curve is invalid".to_owned());
        }
        let similarities = [
            self.search_memory.exact_similarity_milli,
            self.search_memory.prefix_extension_similarity_milli,
            self.search_memory.prefix_contraction_similarity_milli,
            self.search_memory.token_delta_similarity_milli,
            self.search_memory.edit_similarity_milli,
        ];
        if similarities
            .into_iter()
            .any(|similarity| !(0..=1_000).contains(&similarity))
        {
            return Err("Search Memory similarities must be between 0 and 1000".to_owned());
        }
        if similarities[1..]
            .iter()
            .any(|similarity| *similarity > similarities[0])
        {
            return Err(
                "transferred Search Memory similarity cannot exceed exact similarity".to_owned(),
            );
        }
        let values = [
            ("textMatch.exactName", self.text_match.exact_name),
            ("textMatch.prefixName", self.text_match.prefix_name),
            ("textMatch.substringName", self.text_match.substring_name),
            ("textMatch.typoName", self.text_match.typo_name),
            (
                "textMatch.typoEditPenalty",
                self.text_match.typo_edit_penalty,
            ),
            ("textMatch.pathComponent", self.text_match.path_component),
            (
                "textMatch.allTokensInName",
                self.text_match.all_tokens_in_name,
            ),
            ("textMatch.existingPath", self.text_match.existing_path),
            ("textMatch.pathScope", self.text_match.path_scope),
            (
                "textMatch.layoutCorrectionPenalty",
                self.text_match.layout_correction_penalty,
            ),
            ("searchMemory.max", self.search_memory.max),
            ("searchMemory.usageMax", self.search_memory.usage_max),
            ("generalUsage.visitMax", self.general_usage.visit_max),
            ("generalUsage.recents", self.general_usage.recents),
            ("context.currentLocation", self.context.current_location),
            ("context.pinnedAnchor", self.context.pinned_anchor),
            ("context.knownPlace", self.context.known_place),
            ("alias.exact", self.alias.exact),
            ("itemKind.file", self.item_kind.file),
            ("itemKind.directory", self.item_kind.directory),
            ("penalties.hidden", self.penalties.hidden),
            ("penalties.junk", self.penalties.junk),
            ("penalties.totalCap", self.penalties.total_cap),
        ];
        if let Some((name, _)) = values.into_iter().find(|(_, value)| *value < 0) {
            return Err(format!("{name} must be non-negative"));
        }
        if !(self.text_match.exact_name >= self.text_match.prefix_name
            && self.text_match.prefix_name >= self.text_match.substring_name
            && self.text_match.substring_name >= self.text_match.typo_name
            && self.text_match.typo_name >= self.text_match.path_component)
        {
            return Err(
                "text match strengths must descend exactName → prefixName → substringName → typoName → pathComponent"
                    .to_owned(),
            );
        }
        if self.penalties.total_cap < self.penalties.hidden.max(self.penalties.junk) {
            return Err("penalties.totalCap must cover each individual penalty".to_owned());
        }
        Ok(())
    }

    pub fn fingerprint(&self) -> String {
        let bytes = serde_json::to_vec(self).expect("Ranker Configuration serializes");
        let mut hash = 0xcbf29ce484222325u64;
        for byte in bytes {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x100000001b3);
        }
        format!("{hash:016x}")
    }

    pub fn formatted_default() -> String {
        serde_json::to_string_pretty(&Self::default()).expect("default ranker config serializes")
    }
}

pub fn run_cli(arguments: impl IntoIterator<Item = OsString>) -> Result<(), String> {
    let mut arguments = arguments.into_iter();
    let command = arguments
        .next()
        .and_then(|value| value.into_string().ok())
        .ok_or_else(cli_usage)?;
    match command.as_str() {
        "default" => {
            no_more(&mut arguments)?;
            println!("{}", RankerConfig::formatted_default());
        }
        "validate" | "explain" => {
            let path = next_path(&mut arguments)?;
            no_more(&mut arguments)?;
            let config = load_explicit(&path)?;
            println!(
                "{}",
                serde_json::json!({
                    "valid": true,
                    "fingerprint": config.fingerprint(),
                    "config": config,
                })
            );
        }
        "compare" => {
            let left_path = next_path(&mut arguments)?;
            let right_path = next_path(&mut arguments)?;
            no_more(&mut arguments)?;
            let left = load_explicit(&left_path)?;
            let right = load_explicit(&right_path)?;
            println!(
                "{}",
                serde_json::json!({
                    "equal": left == right,
                    "leftFingerprint": left.fingerprint(),
                    "rightFingerprint": right.fingerprint(),
                    "left": left,
                    "right": right,
                })
            );
        }
        "apply" => {
            let source = next_path(&mut arguments)?;
            let app_data = next_path(&mut arguments)?;
            no_more(&mut arguments)?;
            let config = load_explicit(&source)?;
            fs::create_dir_all(&app_data)
                .map_err(|error| format!("cannot create {}: {error}", app_data.display()))?;
            let target = RankerConfig::path(&app_data);
            let temp = target.with_extension("json.tmp");
            let mut output = File::create(&temp)
                .map_err(|error| format!("cannot create {}: {error}", temp.display()))?;
            output
                .write_all(RankerConfig::formatted(&config).as_bytes())
                .and_then(|()| output.write_all(b"\n"))
                .and_then(|()| output.sync_all())
                .map_err(|error| format!("cannot write {}: {error}", temp.display()))?;
            fs::rename(&temp, &target).map_err(|error| {
                format!(
                    "cannot atomically replace {} from {}: {error}",
                    target.display(),
                    temp.display()
                )
            })?;
            let expected_fingerprint = config.fingerprint();
            let live = request_live_reload(&app_data);
            let (reload_confirmed, active_fingerprint, message) = match live {
                Ok(active) if active == expected_fingerprint => (
                    true,
                    Some(active),
                    "running Beeline confirmed the new Ranker Configuration".to_owned(),
                ),
                Ok(active) => (
                    false,
                    Some(active.clone()),
                    format!(
                        "running Beeline acknowledged reload but activated unexpected fingerprint {active}"
                    ),
                ),
                Err(error) => (
                    false,
                    None,
                    format!(
                        "ranker.json is authoritative for next launch; live reload was not confirmed: {error}"
                    ),
                ),
            };
            println!(
                "{}",
                serde_json::json!({
                    "applied": target,
                    "fingerprint": expected_fingerprint,
                    "reloadConfirmed": reload_confirmed,
                    "activeFingerprint": active_fingerprint,
                    "message": message,
                })
            );
        }
        "replay" => {
            let path = next_path(&mut arguments)?;
            let alternative = arguments.next().map(PathBuf::from);
            no_more(&mut arguments)?;
            let alternative = alternative.as_deref().map(load_explicit).transpose()?;
            println!(
                "{}",
                super::ranking_trace::replay(&path, alternative.as_ref())?
            );
        }
        _ => return Err(cli_usage()),
    }
    Ok(())
}

pub fn start_control_server<F>(app_data_dir: &Path, reload: F) -> io::Result<()>
where
    F: Fn() -> Result<String, String> + Send + Sync + 'static,
{
    fs::create_dir_all(app_data_dir)?;
    let socket_path = app_data_dir.join(CONTROL_SOCKET);
    match fs::remove_file(&socket_path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let listener = UnixListener::bind(&socket_path)?;
    fs::set_permissions(&socket_path, fs::Permissions::from_mode(0o600))?;
    thread::Builder::new()
        .name("beeline-ranker-control".to_owned())
        .spawn(move || {
            crate::qos::set_qos(0x11);
            for stream in listener.incoming() {
                match stream {
                    Ok(stream) => respond_to_control(stream, &reload),
                    Err(error) => eprintln!("ranker control listener failed: {error}"),
                }
            }
        })?;
    Ok(())
}

fn respond_to_control<F>(mut stream: UnixStream, reload: &F)
where
    F: Fn() -> Result<String, String>,
{
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let request = {
        let mut line = String::new();
        let mut reader = BufReader::new(&stream);
        reader.read_line(&mut line).map(|_| line)
    };
    let response = match request {
        Ok(request) if request.trim() == "reload" => match reload() {
            Ok(fingerprint) => serde_json::json!({
                "ok": true,
                "fingerprint": fingerprint,
            }),
            Err(error) => serde_json::json!({ "ok": false, "error": error }),
        },
        Ok(_) => serde_json::json!({ "ok": false, "error": "unknown command" }),
        Err(error) => serde_json::json!({ "ok": false, "error": error.to_string() }),
    };
    if serde_json::to_writer(&mut stream, &response).is_ok() {
        let _ = stream.write_all(b"\n");
        let _ = stream.flush();
    }
}

fn request_live_reload(app_data_dir: &Path) -> Result<String, String> {
    let socket_path = app_data_dir.join(CONTROL_SOCKET);
    let mut stream = UnixStream::connect(&socket_path)
        .map_err(|error| format!("cannot connect to {}: {error}", socket_path.display()))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .map_err(|error| format!("cannot set control timeout: {error}"))?;
    stream
        .write_all(b"reload\n")
        .and_then(|()| stream.flush())
        .map_err(|error| format!("cannot request live reload: {error}"))?;
    let mut response = String::new();
    BufReader::new(stream)
        .read_line(&mut response)
        .map_err(|error| format!("cannot read live reload response: {error}"))?;
    let response: serde_json::Value = serde_json::from_str(&response)
        .map_err(|error| format!("invalid live reload response: {error}"))?;
    if response.get("ok").and_then(serde_json::Value::as_bool) != Some(true) {
        return Err(response
            .get("error")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("running Beeline rejected reload")
            .to_owned());
    }
    response
        .get("fingerprint")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| "live reload response omitted fingerprint".to_owned())
}

impl RankerConfig {
    fn formatted(config: &Self) -> String {
        serde_json::to_string_pretty(config).expect("validated ranker config serializes")
    }
}

fn load_explicit(path: &Path) -> Result<RankerConfig, String> {
    let text = fs::read_to_string(path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    RankerConfig::parse(&text)
}

fn next_path(arguments: &mut impl Iterator<Item = OsString>) -> Result<PathBuf, String> {
    arguments.next().map(PathBuf::from).ok_or_else(cli_usage)
}

fn no_more(arguments: &mut impl Iterator<Item = OsString>) -> Result<(), String> {
    if arguments.next().is_some() {
        Err(cli_usage())
    } else {
        Ok(())
    }
}

fn cli_usage() -> String {
    "usage: beeline --ranker-config default | validate <file> | explain <file> | compare <left> <right> | apply <file> <app-data-dir> | replay <trace> [alternative-config]"
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_dir() -> PathBuf {
        let unique = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "beeline_ranker_control_{}_{}",
            std::process::id(),
            unique
        ));
        fs::create_dir_all(&path).expect("create temp dir");
        path
    }

    #[test]
    fn default_round_trips_and_has_a_stable_fingerprint() {
        let default = RankerConfig::default();
        let parsed = RankerConfig::parse(&RankerConfig::formatted_default()).expect("parse");
        assert_eq!(parsed, default);
        assert_eq!(parsed.fingerprint(), default.fingerprint());
    }

    #[test]
    fn rejects_unknown_missing_negative_and_non_monotone_values() {
        let text = RankerConfig::formatted_default();
        let unknown = text.replacen(
            "\"schemaVersion\": 1",
            "\"schemaVersion\": 1,\n  \"mystery\": 2",
            1,
        );
        assert!(RankerConfig::parse(&unknown).is_err());
        let missing = text.replacen("    \"exactName\": 5000000,\n", "", 1);
        assert!(RankerConfig::parse(&missing).is_err());
        let negative = text.replacen("\"hidden\": 8000", "\"hidden\": -1", 1);
        assert!(RankerConfig::parse(&negative).is_err());
        let non_monotone = text.replacen("\"prefixName\": 4000000", "\"prefixName\": 6000000", 1);
        assert!(RankerConfig::parse(&non_monotone).is_err());
    }

    #[test]
    fn live_control_returns_the_activated_fingerprint() {
        let dir = temp_dir();
        start_control_server(&dir, || Ok("active-config".to_owned())).expect("start control");
        assert_eq!(
            request_live_reload(&dir).expect("request reload"),
            "active-config"
        );
        let _ = fs::remove_dir_all(dir);
    }
}
