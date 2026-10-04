use crate::{Model, ModelOption, ModelOptionChoice, ReasoningLevel, SandboxLevel};

pub(crate) const REASONING_LEVELS: &[ReasoningLevel] = &[
    ReasoningLevel::Minimal,
    ReasoningLevel::Low,
    ReasoningLevel::Medium,
    ReasoningLevel::High,
    ReasoningLevel::XHigh,
    ReasoningLevel::Max,
    ReasoningLevel::Ultra,
];

/// Codex rejects `minimal` when default tools like web_search are enabled.
pub(crate) fn to_effort(reasoning: Option<ReasoningLevel>) -> Option<&'static str> {
    Some(match reasoning? {
        ReasoningLevel::Minimal | ReasoningLevel::Low => "low",
        ReasoningLevel::Medium => "medium",
        ReasoningLevel::High => "high",
        ReasoningLevel::XHigh | ReasoningLevel::Ultracode | ReasoningLevel::Ultrathink => "xhigh",
        ReasoningLevel::Max => "max",
        ReasoningLevel::Ultra => "ultra",
    })
}

pub(crate) fn sandbox_mode(sandbox: SandboxLevel) -> &'static str {
    match sandbox {
        SandboxLevel::ReadOnly => "read-only",
        SandboxLevel::WorkspaceWrite => "workspace-write",
        SandboxLevel::DangerFullAccess => "danger-full-access",
    }
}

pub(crate) fn sandbox_policy_type(sandbox: SandboxLevel) -> &'static str {
    match sandbox {
        SandboxLevel::ReadOnly => "readOnly",
        SandboxLevel::WorkspaceWrite => "workspaceWrite",
        SandboxLevel::DangerFullAccess => "dangerFullAccess",
    }
}

pub(crate) fn sandbox_policy_value(sandbox: SandboxLevel) -> serde_json::Value {
    let mut policy = serde_json::Map::new();
    policy.insert("type".into(), sandbox_policy_type(sandbox).into());
    if matches!(sandbox, SandboxLevel::WorkspaceWrite) {
        policy.insert("networkAccess".into(), true.into());
    }
    serde_json::Value::Object(policy)
}

const ULTRA_LADDER: &[ReasoningLevel] = &[
    ReasoningLevel::Low,
    ReasoningLevel::Medium,
    ReasoningLevel::High,
    ReasoningLevel::XHigh,
    ReasoningLevel::Max,
    ReasoningLevel::Ultra,
];

const MAX_LADDER: &[ReasoningLevel] = &[
    ReasoningLevel::Low,
    ReasoningLevel::Medium,
    ReasoningLevel::High,
    ReasoningLevel::XHigh,
    ReasoningLevel::Max,
];

const XHIGH_LADDER: &[ReasoningLevel] = &[
    ReasoningLevel::Low,
    ReasoningLevel::Medium,
    ReasoningLevel::High,
    ReasoningLevel::XHigh,
];

fn service_tier() -> ModelOption {
    ModelOption {
        id: "serviceTier".into(),
        label: "Service Tier".into(),
        choices: vec![
            ModelOptionChoice {
                id: "default".into(),
                label: "Standard".into(),
            },
            ModelOptionChoice {
                id: "fast".into(),
                label: "Fast".into(),
            },
        ],
        default_choice: "default".into(),
    }
}

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

pub(crate) fn static_models() -> Vec<Model> {
    vec![
        model(
            "gpt-6-astra",
            "GPT-6-Astra",
            "Our most capable model for complex, demanding work.",
            ULTRA_LADDER,
            vec![service_tier()],
        ),
        model(
            "gpt-5.6-sol",
            "GPT-5.6-Sol",
            "Frontier reasoning flagship",
            ULTRA_LADDER,
            vec![service_tier()],
        ),
        model(
            "gpt-5.6-terra",
            "GPT-5.6-Terra",
            "Deep multi-step agentic work",
            ULTRA_LADDER,
            vec![service_tier()],
        ),
        model(
            "gpt-5.6-luna",
            "GPT-5.6-Luna",
            "Fast frontier model",
            MAX_LADDER,
            vec![service_tier()],
        ),
        // Codex rejects any serviceTier for this model.
        model(
            "gpt-daybreak-blue-latest",
            "Daybreak Blue",
            "Frontier model for defensive cybersecurity work",
            ULTRA_LADDER,
            Vec::new(),
        ),
        model(
            "gpt-5.5",
            "GPT-5.5",
            "Previous generation flagship",
            XHIGH_LADDER,
            vec![service_tier()],
        ),
        model(
            "gpt-5.4",
            "GPT-5.4",
            "Reliable general coding",
            XHIGH_LADDER,
            vec![service_tier()],
        ),
        model(
            "gpt-5.4-mini",
            "GPT-5.4-Mini",
            "Small, fast and capable",
            XHIGH_LADDER,
            vec![service_tier()],
        ),
        model(
            "gpt-5.3-codex-spark",
            "GPT-5.3-Codex-Spark",
            "Ultra-fast lightweight coding",
            XHIGH_LADDER,
            vec![service_tier()],
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effort_clamps_unsupported_levels() {
        assert_eq!(to_effort(None), None);
        assert_eq!(to_effort(Some(ReasoningLevel::Minimal)), Some("low"));
        assert_eq!(to_effort(Some(ReasoningLevel::Ultracode)), Some("xhigh"));
        assert_eq!(to_effort(Some(ReasoningLevel::Ultrathink)), Some("xhigh"));
        assert_eq!(to_effort(Some(ReasoningLevel::Max)), Some("max"));
        assert_eq!(to_effort(Some(ReasoningLevel::Ultra)), Some("ultra"));
    }

    #[test]
    fn catalog_is_newest_first_with_service_tiers() {
        let models = static_models();
        assert_eq!(models.len(), 9);
        assert_eq!(models[0].id, "gpt-6-astra");
        assert!(models[0].reasoning_levels.contains(&ReasoningLevel::Ultra));
        assert!(!models[5].reasoning_levels.contains(&ReasoningLevel::Max));
        for m in &models {
            let tier = m.options.iter().find(|o| o.id == "serviceTier");
            if m.id == "gpt-daybreak-blue-latest" {
                assert!(tier.is_none(), "{} must not carry serviceTier", m.id);
            } else {
                assert!(tier.is_some(), "{} missing serviceTier", m.id);
            }
        }
    }

    #[test]
    fn daybreak_blue_rides_the_full_ultra_ladder() {
        let models = static_models();
        let daybreak = models
            .iter()
            .find(|m| m.id == "gpt-daybreak-blue-latest")
            .expect("daybreak blue in catalog");
        assert_eq!(daybreak.label, "Daybreak Blue");
        assert!(daybreak.reasoning_levels.contains(&ReasoningLevel::Ultra));
        assert!(daybreak.options.is_empty());
    }
}
