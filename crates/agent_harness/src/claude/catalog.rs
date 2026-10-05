use crate::{Model, ModelOption, ModelOptionChoice, ReasoningLevel};

pub(crate) const ULTRATHINK_PREFIX: &str = "Ultrathink:\n";

pub(crate) fn apply_ultrathink(reasoning: Option<ReasoningLevel>, text: &str) -> String {
    if reasoning == Some(ReasoningLevel::Ultrathink) && crate::leading_command(text).is_none() {
        format!("{ULTRATHINK_PREFIX}{text}")
    } else {
        text.to_owned()
    }
}

fn contains_any(hay: &str, needles: &[&str]) -> bool {
    needles.iter().any(|n| hay.contains(n))
}

pub(crate) fn supports_xhigh(model: &str) -> bool {
    contains_any(
        model,
        &[
            "fable-5", "opus-4-7", "opus-4-8", "opus-4-9", "opus-5", "opus-6", "opus-7", "opus-8",
            "opus-9", "sonnet-5", "sonnet-6", "sonnet-7", "sonnet-8", "sonnet-9",
        ],
    )
}

pub(crate) fn to_effort(
    reasoning: Option<ReasoningLevel>,
    model: Option<&str>,
) -> Option<&'static str> {
    let base = match reasoning? {
        ReasoningLevel::Ultrathink => return None,
        ReasoningLevel::Minimal | ReasoningLevel::Low => "low",
        ReasoningLevel::Medium => "medium",
        ReasoningLevel::High => "high",
        ReasoningLevel::XHigh | ReasoningLevel::Ultracode => "xhigh",
        ReasoningLevel::Max | ReasoningLevel::Ultra => "max",
    };
    if base == "xhigh" && !model.is_some_and(supports_xhigh) {
        return Some("max");
    }
    Some(base)
}

fn toggle(id: &str, label: &str) -> ModelOption {
    ModelOption {
        id: id.into(),
        label: label.into(),
        choices: vec![
            ModelOptionChoice {
                id: "off".into(),
                label: "Off".into(),
            },
            ModelOptionChoice {
                id: "on".into(),
                label: "On".into(),
            },
        ],
        default_choice: "off".into(),
    }
}

pub(crate) fn context_window() -> ModelOption {
    ModelOption {
        id: CONTEXT_WINDOW_OPTION.into(),
        label: "Context Window".into(),
        choices: vec![
            ModelOptionChoice {
                id: "200k".into(),
                label: "200K".into(),
            },
            ModelOptionChoice {
                id: "1m".into(),
                label: "1M".into(),
            },
        ],
        default_choice: "1m".into(),
    }
}

const FULL_LADDER: &[ReasoningLevel] = &[
    ReasoningLevel::Low,
    ReasoningLevel::Medium,
    ReasoningLevel::High,
    ReasoningLevel::XHigh,
    ReasoningLevel::Max,
    ReasoningLevel::Ultracode,
    ReasoningLevel::Ultrathink,
];

const XHIGH_LADDER: &[ReasoningLevel] = &[
    ReasoningLevel::Low,
    ReasoningLevel::Medium,
    ReasoningLevel::High,
    ReasoningLevel::XHigh,
    ReasoningLevel::Max,
    ReasoningLevel::Ultrathink,
];

fn model(
    id: &str,
    label: &str,
    description: &str,
    ladder: &[ReasoningLevel],
    options: Vec<ModelOption>,
) -> Model {
    Model {
        id: id.into(),
        label: label.into(),
        description: (!description.is_empty()).then(|| description.into()),
        reasoning_levels: ladder.to_vec(),
        options,
    }
}

pub(crate) fn configured_models() -> Vec<Model> {
    let root = std::env::var_os("CLAUDE_CONFIG_DIR")
        .filter(|v| !v.is_empty())
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| crate::executable::home_or_current_dir().join(".claude"));
    models_with_settings(&root.join("settings.json"))
}

fn models_with_settings(path: &std::path::Path) -> Vec<Model> {
    let mut models = static_models();
    let settings = std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .unwrap_or_default();
    let ids = std::iter::once(settings.get("model")).chain(
        [
            "ANTHROPIC_MODEL",
            "ANTHROPIC_SMALL_FAST_MODEL",
            "ANTHROPIC_DEFAULT_OPUS_MODEL",
            "ANTHROPIC_DEFAULT_SONNET_MODEL",
            "ANTHROPIC_DEFAULT_HAIKU_MODEL",
        ]
        .into_iter()
        .map(|key| settings.get("env").and_then(|env| env.get(key))),
    );
    for id in ids
        .flatten()
        .filter_map(|v| v.as_str())
        .map(str::trim)
        .filter(|id| !id.is_empty())
    {
        if !models.iter().any(|m| m.id == id) {
            models.push(Model {
                id: id.into(),
                label: id.into(),
                description: None,
                reasoning_levels: FULL_LADDER.to_vec(),
                options: vec![],
            });
        }
    }
    models
}

#[cfg(test)]
#[test]
fn settings_models_keep_manifest_and_full_ladder() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    std::fs::write(
        &path,
        serde_json::json!({"model":"gateway/model", "env": {
            "ANTHROPIC_MODEL":"gateway/model", "ANTHROPIC_SMALL_FAST_MODEL":"fast",
            "ANTHROPIC_DEFAULT_OPUS_MODEL":"opus", "ANTHROPIC_DEFAULT_SONNET_MODEL":"sonnet",
            "ANTHROPIC_DEFAULT_HAIKU_MODEL":"haiku"
        }})
        .to_string(),
    )
    .unwrap();
    let models = models_with_settings(&path);
    assert_eq!(&models[..static_models().len()], static_models());
    assert_eq!(models.len(), static_models().len() + 5);
    for model in &models[static_models().len()..] {
        assert_eq!(model.label, model.id);
        assert_eq!(model.reasoning_levels, FULL_LADDER);
    }
    std::fs::write(&path, "invalid").unwrap();
    assert_eq!(models_with_settings(&path), static_models());
}

pub(super) fn with_discovered_models(
    mut models: Vec<Model>,
    response: &serde_json::Value,
) -> Result<Vec<Model>, crate::HarnessError> {
    let entries = response
        .get("response")
        .and_then(|v| v.get("models"))
        .and_then(serde_json::Value::as_array)
        .filter(|entries| !entries.is_empty())
        .ok_or_else(|| {
            crate::HarnessError::Protocol("Claude returned an empty model catalog".into())
        })?;
    let mut default = None;
    let mut valid = false;
    let mut sizes: Vec<(String, Vec<String>)> = Vec::new();
    for entry in entries {
        let text = |key: &str| {
            entry
                .get(key)
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|v| !v.is_empty())
        };
        let Some(full_id) = text("resolvedModel").or_else(|| text("value")) else {
            continue;
        };
        let (id, size) = split_context_size(full_id);
        let size = size.or_else(|| text("value").and_then(|value| split_context_size(value).1));
        // Bare aliases cannot be persisted as selections.
        if matches!(id, "default" | "opus" | "sonnet" | "haiku" | "fable") {
            continue;
        }
        valid = true;
        if text("value") == Some("default") {
            default = Some(id.to_owned());
        }
        if let Some(size) = size {
            match sizes.iter_mut().find(|(base, _)| base == id) {
                Some((_, found)) if !found.contains(&size) => found.push(size),
                Some(_) => {}
                None => sizes.push((id.to_owned(), vec![size])),
            }
        }
        if models.iter().any(|model| model.id == id) {
            continue;
        }
        let mut ladder = Vec::new();
        for effort in entry
            .get("supportedEffortLevels")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(serde_json::Value::as_str)
        {
            let level = match effort {
                "low" => ReasoningLevel::Low,
                "medium" => ReasoningLevel::Medium,
                "high" => ReasoningLevel::High,
                "xhigh" => ReasoningLevel::XHigh,
                "max" => ReasoningLevel::Max,
                _ => continue,
            };
            if !ladder.contains(&level) {
                ladder.push(level);
            }
        }
        if ladder.contains(&ReasoningLevel::XHigh) {
            ladder.extend([ReasoningLevel::Ultracode, ReasoningLevel::Ultrathink]);
        }
        models.push(Model {
            id: id.into(),
            label: text("displayName").unwrap_or(id).into(),
            description: text("description").map(str::to_owned),
            reasoning_levels: ladder,
            options: vec![],
        });
    }
    if !valid {
        return Err(crate::HarnessError::Protocol(
            "Claude returned an empty model catalog".into(),
        ));
    }
    for (base, found) in sizes {
        if let Some(model) = models.iter_mut().find(|model| model.id == base) {
            model
                .options
                .retain(|option| option.id != CONTEXT_WINDOW_OPTION);
            model.options.insert(0, discovered_context_window(found));
        }
    }
    if let Some(default) = default
        && let Some(index) = models.iter().position(|m| m.id == default)
    {
        let model = models.remove(index);
        models.insert(0, model);
    }
    Ok(models)
}

pub(crate) const CONTEXT_WINDOW_OPTION: &str = "contextWindow";
const STANDARD_CONTEXT: (&str, &str) = ("200k", "200K");

pub(crate) fn split_context_size(id: &str) -> (&str, Option<String>) {
    let parsed = id
        .strip_suffix(']')
        .and_then(|rest| rest.rsplit_once('['))
        .filter(|(base, size)| !base.is_empty() && context_tokens(size).is_some());
    match parsed {
        Some((base, size)) => (base, Some(size.to_ascii_lowercase())),
        None => (id, None),
    }
}

fn context_tokens(size: &str) -> Option<u64> {
    let size = size.to_ascii_lowercase();
    let (digits, scale) = match size.strip_suffix('m') {
        Some(digits) => (digits, 1_000_000),
        None => (size.strip_suffix('k')?, 1_000),
    };
    digits
        .parse::<u64>()
        .ok()
        .filter(|count| *count > 0)
        .map(|count| count * scale)
}

fn discovered_context_window(mut sizes: Vec<String>) -> ModelOption {
    sizes.sort_by_key(|size| context_tokens(size));
    let largest = sizes.last().cloned();
    let mut choices = vec![ModelOptionChoice {
        id: STANDARD_CONTEXT.0.into(),
        label: STANDARD_CONTEXT.1.into(),
    }];
    choices.extend(
        sizes
            .into_iter()
            .filter(|size| size != STANDARD_CONTEXT.0)
            .map(|size| ModelOptionChoice {
                label: size.to_ascii_uppercase(),
                id: size,
            }),
    );
    ModelOption {
        id: CONTEXT_WINDOW_OPTION.into(),
        label: "Context Window".into(),
        choices,
        default_choice: largest.unwrap_or_else(|| STANDARD_CONTEXT.0.into()),
    }
}

pub(crate) fn model_argument(
    model: &str,
    choice: Option<&str>,
    discovered: Option<&[Model]>,
) -> String {
    let (base, saved_size) = split_context_size(model);
    let known = discovered
        .and_then(|models| models.iter().find(|candidate| candidate.id == base))
        .cloned()
        .or_else(|| {
            configured_models()
                .into_iter()
                .find(|candidate| candidate.id == base)
        });
    let option = known.as_ref().and_then(|candidate| {
        candidate
            .options
            .iter()
            .find(|option| option.id == CONTEXT_WINDOW_OPTION)
            .cloned()
    });
    let offered = |size: &str| {
        option
            .as_ref()
            .is_some_and(|option| option.choices.iter().any(|offered| offered.id == size))
    };
    let size = match &option {
        Some(option) => choice
            .filter(|choice| offered(choice))
            .or(saved_size.as_deref().filter(|size| offered(size)))
            .unwrap_or(&option.default_choice)
            .to_owned(),
        // A model Wu knows to have a single size must not get a stale size from another model.
        None if known.is_some() => return base.to_owned(),
        None => match choice.map(str::to_ascii_lowercase).or(saved_size) {
            Some(size) => size,
            None => return base.to_owned(),
        },
    };
    if size == STANDARD_CONTEXT.0 || context_tokens(&size).is_none() {
        base.to_owned()
    } else {
        format!("{base}[{size}]")
    }
}

pub fn static_models() -> Vec<Model> {
    vec![
        model(
            "claude-fable-5-1",
            "Fable 5.1",
            "Most intelligent model for building agents",
            FULL_LADDER,
            vec![context_window()],
        ),
        model(
            "claude-fable-5",
            "Fable 5",
            "Previous generation Fable",
            FULL_LADDER,
            vec![context_window()],
        ),
        model(
            "claude-opus-5-5",
            "Opus 5.5",
            "Best for everyday, complex tasks",
            FULL_LADDER,
            vec![context_window(), toggle("fastMode", "Fast Mode")],
        ),
        model(
            "claude-opus-4-8",
            "Opus 4.8",
            "Previous generation Opus",
            FULL_LADDER,
            vec![toggle("fastMode", "Fast Mode")],
        ),
        model(
            "claude-opus-4-7",
            "Opus 4.7",
            "Older generation Opus",
            XHIGH_LADDER,
            vec![toggle("fastMode", "Fast Mode")],
        ),
        model(
            "claude-sonnet-5",
            "Sonnet 5",
            "Balanced speed and intelligence",
            XHIGH_LADDER,
            vec![context_window()],
        ),
        model(
            "claude-haiku-4-5",
            "Haiku 4.5",
            "Fastest model for everyday tasks",
            &[],
            vec![toggle("thinking", "Thinking")],
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effort_maps_special_modes() {
        assert_eq!(to_effort(None, None), None);
        assert_eq!(to_effort(Some(ReasoningLevel::Ultrathink), None), None);
        assert_eq!(
            to_effort(Some(ReasoningLevel::Minimal), Some("claude-fable-5")),
            Some("low")
        );
        assert_eq!(
            to_effort(Some(ReasoningLevel::Ultra), Some("claude-fable-5")),
            Some("max")
        );
        assert_eq!(
            to_effort(Some(ReasoningLevel::Ultracode), Some("claude-fable-5")),
            Some("xhigh")
        );
        assert_eq!(
            to_effort(Some(ReasoningLevel::XHigh), Some("claude-opus-4-5")),
            Some("max")
        );
        assert_eq!(to_effort(Some(ReasoningLevel::XHigh), None), Some("max"));
    }

    #[test]
    fn context_sizes_come_from_model_suffixes() {
        assert_eq!(
            split_context_size("claude-opus-5-5[1m]"),
            ("claude-opus-5-5", Some("1m".into()))
        );
        assert_eq!(
            split_context_size("claude-x[2M]"),
            ("claude-x", Some("2m".into()))
        );
        assert_eq!(
            split_context_size("claude-x[500k]"),
            ("claude-x", Some("500k".into()))
        );
        assert_eq!(
            split_context_size("gateway/model[beta]"),
            ("gateway/model[beta]", None)
        );
        assert_eq!(split_context_size("[1m]"), ("[1m]", None));

        let option = discovered_context_window(vec!["2m".into(), "1m".into()]);
        let ids: Vec<&str> = option
            .choices
            .iter()
            .map(|choice| choice.id.as_str())
            .collect();
        assert_eq!(ids, ["200k", "1m", "2m"]);
        assert_eq!(option.default_choice, "2m");
        assert_eq!(option.choices[2].label, "2M");

        let models = static_models();
        let selected = crate::view::selected_catalog_model(&models, Some("claude-opus-5-5[1m]"));
        assert_eq!(
            selected.map(|model| model.id.as_str()),
            Some("claude-opus-5-5")
        );
    }

    #[test]
    fn model_argument_uses_the_choice_or_the_largest_size() {
        let discovered = vec![Model {
            id: "claude-new".into(),
            label: "New".into(),
            description: None,
            reasoning_levels: Vec::new(),
            options: vec![discovered_context_window(vec!["1m".into(), "2m".into()])],
        }];
        let argument =
            |model: &str, choice: Option<&str>| model_argument(model, choice, Some(&discovered));
        assert_eq!(argument("claude-new", None), "claude-new[2m]");
        assert_eq!(argument("claude-new", Some("1m")), "claude-new[1m]");
        assert_eq!(argument("claude-new", Some("200k")), "claude-new");
        assert_eq!(argument("claude-new", Some("9m")), "claude-new[2m]");
        assert_eq!(argument("claude-new[1m]", None), "claude-new[1m]");
        assert_eq!(argument("claude-new[1m]", Some("200k")), "claude-new");
        assert_eq!(argument("claude-opus-5-5", None), "claude-opus-5-5[1m]");
        assert_eq!(argument("claude-haiku-4-5", Some("1m")), "claude-haiku-4-5");
        assert_eq!(argument("claude-haiku-4-5[1m]", None), "claude-haiku-4-5");
        assert_eq!(argument("gateway/model", Some("1m")), "gateway/model[1m]");
        assert_eq!(argument("gateway/model", None), "gateway/model");
    }

    #[test]
    fn xhigh_family_matching() {
        assert!(supports_xhigh("claude-fable-5"));
        assert!(supports_xhigh("claude-fable-5-1"));
        assert!(supports_xhigh("claude-opus-5"));
        assert!(supports_xhigh("claude-opus-5-5"));
        assert!(supports_xhigh("claude-opus-5-5[1m]"));
        assert!(supports_xhigh("claude-opus-4-7-20260101"));
        assert!(!supports_xhigh("claude-opus-4-5"));
        assert!(!supports_xhigh("claude-sonnet-4-5"));
    }

    #[test]
    fn ultrathink_preserves_leading_commands_and_arguments() {
        for command in ["/compact", "/review focus on tests"] {
            for prefix in ["", " ", "   ", "\n", "\r\n  "] {
                let text = format!("{prefix}{command}");
                assert_eq!(
                    apply_ultrathink(Some(ReasoningLevel::Ultrathink), &text),
                    text
                );
            }
        }
        for literal in [
            "    /compact",
            "\t/compact",
            "\n    /compact",
            "\u{a0}/compact",
        ] {
            assert_eq!(
                apply_ultrathink(Some(ReasoningLevel::Ultrathink), literal),
                format!("{ULTRATHINK_PREFIX}{literal}")
            );
        }
    }

    #[test]
    fn ultrathink_prefixes_prompt() {
        assert_eq!(
            apply_ultrathink(Some(ReasoningLevel::Ultrathink), "do it"),
            "Ultrathink:\ndo it"
        );
        assert_eq!(
            apply_ultrathink(Some(ReasoningLevel::Max), "do it"),
            "do it"
        );
    }
}
