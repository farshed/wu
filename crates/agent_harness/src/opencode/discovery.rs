use std::collections::{BTreeMap, HashMap};

use serde_json::Value;

use crate::{Model, ModelOption, ModelOptionChoice, ReasoningLevel, Skill, SlashCommand};

pub(super) const REASONING_LEVELS: &[ReasoningLevel] = &[
    ReasoningLevel::Low,
    ReasoningLevel::Medium,
    ReasoningLevel::High,
    ReasoningLevel::XHigh,
    ReasoningLevel::Max,
];

pub(super) const SKILL_PATH_PREFIX: &str = "opencode-skill:";

fn variant_candidates(reasoning: Option<ReasoningLevel>) -> &'static [&'static str] {
    match reasoning {
        None => &[],
        Some(ReasoningLevel::Minimal) => &["minimal", "low"],
        Some(ReasoningLevel::Low) => &["low", "minimal"],
        Some(ReasoningLevel::Medium) => &["medium"],
        Some(ReasoningLevel::High) => &["high"],
        Some(ReasoningLevel::XHigh) => &["xhigh", "x-high", "high"],
        Some(ReasoningLevel::Max) => &["max", "xhigh", "high"],
        Some(ReasoningLevel::Ultra | ReasoningLevel::Ultracode | ReasoningLevel::Ultrathink) => {
            &["ultra", "max", "high"]
        }
    }
}

fn variant_to_level(id: &str) -> Option<ReasoningLevel> {
    match id {
        "minimal" => Some(ReasoningLevel::Minimal),
        "low" => Some(ReasoningLevel::Low),
        "medium" => Some(ReasoningLevel::Medium),
        "high" => Some(ReasoningLevel::High),
        "xhigh" | "x-high" => Some(ReasoningLevel::XHigh),
        "max" => Some(ReasoningLevel::Max),
        _ => None,
    }
}

#[derive(Debug, Default, serde::Deserialize)]
pub(super) struct ProviderCatalog {
    pub(super) all: Option<Vec<Provider>>,
    pub(super) connected: Option<Vec<String>>,
    #[serde(rename = "default")]
    pub(super) defaults: Option<HashMap<String, Value>>,
}

#[derive(Debug, serde::Deserialize)]
pub(super) struct Provider {
    pub(super) id: Option<String>,
    pub(super) name: Option<String>,
    pub(super) models: Option<BTreeMap<String, ProviderModel>>,
}

#[derive(Debug, serde::Deserialize)]
pub(super) struct ProviderModel {
    #[serde(default)]
    pub(super) limit: ProviderLimit,
    pub(super) name: Option<String>,
    pub(super) variants: Option<BTreeMap<String, serde::de::IgnoredAny>>,
}

#[derive(Debug, Default, serde::Deserialize)]
pub(super) struct ProviderLimit {
    pub(super) context: Option<u64>,
}

pub(super) fn models_from_providers(providers: &ProviderCatalog) -> Vec<Model> {
    let connected: std::collections::HashSet<&str> = providers
        .connected
        .iter()
        .flatten()
        .map(String::as_str)
        .collect();
    let mut result = Vec::new();
    for provider in providers.all.iter().flatten() {
        let Some(provider_id) = provider.id.as_deref() else {
            continue;
        };
        if !connected.is_empty() && !connected.contains(provider_id) {
            continue;
        }
        let provider_name = provider.name.as_deref().unwrap_or(provider_id);
        let Some(models) = &provider.models else {
            continue;
        };
        let mut provider_models: Vec<Model> = models
            .iter()
            .map(|(model_id, model)| {
                let mut levels: Vec<ReasoningLevel> = model
                    .variants
                    .iter()
                    .flat_map(|variants| variants.keys())
                    .filter_map(|key| variant_to_level(key))
                    .collect();
                levels.sort();
                levels.dedup();
                Model {
                    id: format!("{provider_id}/{model_id}"),
                    label: model.name.as_deref().unwrap_or(model_id).to_owned(),
                    description: Some(provider_name.to_owned()),
                    reasoning_levels: levels,
                    options: Vec::new(),
                }
            })
            .collect();
        provider_models.sort_by(|a, b| a.label.cmp(&b.label));
        result.extend(provider_models);
    }
    result
}

pub(super) fn context_windows(providers: &ProviderCatalog) -> HashMap<String, u64> {
    providers
        .all
        .iter()
        .flatten()
        .flat_map(|provider| {
            provider
                .models
                .iter()
                .flat_map(|models| models.iter())
                .filter_map(|(id, model)| {
                    Some((
                        format!("{}/{}", provider.id.as_deref()?, id),
                        model.limit.context.filter(|limit| *limit > 0)?,
                    ))
                })
        })
        .collect()
}

pub(super) fn pick_variant(
    providers: &ProviderCatalog,
    provider_id: &str,
    model_id: &str,
    reasoning: Option<ReasoningLevel>,
) -> Option<String> {
    let candidates = variant_candidates(reasoning);
    if candidates.is_empty() {
        return None;
    }
    let variants = providers
        .all
        .as_ref()?
        .iter()
        .find(|provider| provider.id.as_deref() == Some(provider_id))?
        .models
        .as_ref()?
        .get(model_id)?
        .variants
        .as_ref()?;
    candidates
        .iter()
        .find(|candidate| variants.contains_key(**candidate))
        .map(|candidate| (*candidate).to_owned())
}

pub(super) fn agent_option(agents: &Value) -> ModelOption {
    let mut choices = vec![ModelOptionChoice {
        id: String::new(),
        label: "Server default".into(),
    }];
    for agent in agents
        .get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if agent.get("hidden").and_then(Value::as_bool) == Some(true)
            || agent.get("mode").and_then(Value::as_str) == Some("subagent")
        {
            continue;
        }
        if let (Some(id), Some(name)) = (
            agent.get("id").and_then(Value::as_str),
            agent.get("name").and_then(Value::as_str),
        ) && !id.is_empty()
        {
            choices.push(ModelOptionChoice {
                id: id.into(),
                label: name.into(),
            });
        }
    }
    ModelOption {
        id: "agent".into(),
        label: "Agent".into(),
        choices,
        default_choice: String::new(),
    }
}

fn is_skill(command: &Value) -> bool {
    command.get("source").and_then(Value::as_str) == Some("skill")
}

pub(super) fn command_names(commands: &Value) -> Vec<String> {
    commands
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|command| command.get("name").and_then(Value::as_str))
        .map(str::to_owned)
        .collect()
}

pub(super) fn commands_from_wire(commands: &Value) -> Vec<SlashCommand> {
    commands
        .as_array()
        .into_iter()
        .flatten()
        .filter(|command| !is_skill(command))
        .filter_map(|command| {
            Some(SlashCommand {
                name: command.get("name").and_then(Value::as_str)?.to_owned(),
                description: command
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                input_hint: None,
                is_skill: false,
            })
        })
        .collect()
}

fn valid_skill_name(name: &str) -> bool {
    !name.is_empty()
        && !name
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || matches!(c, '/' | '[' | ']'))
}

pub(super) fn skills_from_wire(commands: &Value) -> Vec<Skill> {
    commands
        .as_array()
        .into_iter()
        .flatten()
        .filter(|command| is_skill(command))
        .filter_map(|command| {
            let name = command
                .get("name")
                .and_then(Value::as_str)
                .filter(|name| valid_skill_name(name))?;
            Some(Skill {
                name: name.to_owned(),
                description: command
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                path: Some(format!("{SKILL_PATH_PREFIX}{name}")),
                enabled: true,
            })
        })
        .collect()
}

#[derive(Debug, serde::Deserialize)]
pub(super) struct V2ModelList {
    #[serde(default)]
    pub(super) data: Vec<V2Model>,
}

#[derive(Debug, serde::Deserialize)]
pub(super) struct V2Model {
    #[serde(rename = "providerID")]
    provider_id: String,
    id: String,
    name: Option<String>,
    #[serde(default)]
    limit: ProviderLimit,
    #[serde(default)]
    variants: Vec<V2Variant>,
    enabled: Option<bool>,
}

#[derive(Debug, serde::Deserialize)]
struct V2Variant {
    id: String,
}

pub(super) fn catalog_from_v2_models(models: Vec<V2Model>) -> ProviderCatalog {
    let mut order: Vec<String> = Vec::new();
    let mut grouped: HashMap<String, Vec<(String, ProviderModel)>> = HashMap::new();
    for model in models {
        if model.enabled == Some(false) {
            continue;
        }
        let entry = grouped.entry(model.provider_id.clone()).or_insert_with(|| {
            order.push(model.provider_id.clone());
            Vec::new()
        });
        entry.push((
            model.id,
            ProviderModel {
                limit: model.limit,
                name: model.name,
                variants: Some(
                    model
                        .variants
                        .into_iter()
                        .map(|variant| (variant.id, serde::de::IgnoredAny))
                        .collect(),
                ),
            },
        ));
    }
    let all = order
        .into_iter()
        .map(|id| {
            let models = grouped.remove(&id).unwrap_or_default();
            Provider {
                id: Some(id.clone()),
                name: Some(id),
                models: Some(models.into_iter().collect()),
            }
        })
        .collect();
    ProviderCatalog {
        all: Some(all),
        connected: None,
        defaults: None,
    }
}

pub(super) fn unwrap_data(value: Value) -> Value {
    value
        .get("data")
        .filter(|data| data.is_object() || data.is_array())
        .cloned()
        .unwrap_or(value)
}
