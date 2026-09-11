use baoclaw_core::ipc;
use baoclaw_core::ipc::protocol::RequestId;

use super::WriterRef;
use crate::SharedState;

pub(super) async fn scm_model_list(writer: WriterRef<'_>, id: RequestId) {
    let result = ipc::handlers::model::handle_model_list();
    let mut conn_guard = writer.lock().await;
    let _ = conn_guard.send_response(id, result).await;
}

pub(super) async fn scm_model_route(writer: WriterRef<'_>, id: RequestId, task: String) {
    let result = ipc::handlers::model::handle_model_route(&task);
    let mut conn_guard = writer.lock().await;
    let _ = conn_guard.send_response(id, result).await;
}

pub(super) async fn scm_model_budget(writer: WriterRef<'_>, id: RequestId) {
    let result = ipc::handlers::model::handle_model_budget();
    let mut conn_guard = writer.lock().await;
    let _ = conn_guard.send_response(id, result).await;
}

pub(super) async fn scm_config_model(shared: &SharedState, writer: WriterRef<'_>, id: RequestId) {
    // Mask API key: show first 4 + last 4
    let mask_key = |key: &Option<String>| -> String {
        match key {
            Some(k) if k.len() > 8 => {
                let prefix = &k[..4];
                let suffix = &k[k.len() - 4..];
                format!("{}****{}", prefix, suffix)
            }
            Some(_k) => "****".to_string(),
            None => "(not configured)".to_string(),
        }
    };

    // Check if using model_profiles format
    let cfg = &shared.baoclaw_config;
    let primary_model = if let Some(ref pname) = cfg.primary_profile {
        cfg.model_profiles
            .get(pname)
            .map(|p| p.model.clone())
            .unwrap_or_else(|| cfg.model.clone())
    } else {
        cfg.model.clone()
    };

    let primary_api_type = if let Some(ref pname) = cfg.primary_profile {
        cfg.model_profiles
            .get(pname)
            .map(|p| p.api_type.clone())
            .unwrap_or_else(|| cfg.api_type.clone())
    } else {
        cfg.api_type.clone()
    };

    let primary_key = if let Some(ref pname) = cfg.primary_profile {
        cfg.model_profiles
            .get(pname)
            .and_then(|p| p.api_key.clone())
    } else {
        // Check env for legacy key
        std::env::var("ANTHROPIC_API_KEY").ok()
    };

    let primary_base_url = if let Some(ref pname) = cfg.primary_profile {
        cfg.model_profiles
            .get(pname)
            .and_then(|p| p.base_url.clone())
            .or_else(|| std::env::var("ANTHROPIC_BASE_URL").ok())
    } else {
        cfg.openai_base_url
            .clone()
            .or_else(|| std::env::var("ANTHROPIC_BASE_URL").ok())
    };

    let primary_window = if let Some(ref pname) = cfg.primary_profile {
        cfg.model_profiles
            .get(pname)
            .map(|p| p.context_window)
            .unwrap_or(cfg.context_window)
    } else {
        cfg.context_window
    };

    let primary_threshold = if let Some(ref pname) = cfg.primary_profile {
        cfg.model_profiles
            .get(pname)
            .map(|p| p.auto_compact_threshold_ratio)
            .unwrap_or(cfg.auto_compact_threshold_ratio)
    } else {
        cfg.auto_compact_threshold_ratio
    };

    // Build fallback chain
    let fallback_chain: Vec<serde_json::Value> = if !cfg.fallback_profiles.is_empty() {
        cfg.fallback_profiles
            .iter()
            .filter_map(|name| {
                cfg.model_profiles.get(name).map(|p| {
                    serde_json::json!({
                        "name": name,
                        "model": p.model,
                        "api_type": p.api_type,
                        "context_window": p.context_window,
                        "api_key_masked": mask_key(&p.api_key),
                    })
                })
            })
            .collect()
    } else {
        cfg.fallback_models
            .iter()
            .map(|m| {
                serde_json::json!({
                    "name": m,
                    "model": m,
                })
            })
            .collect()
    };

    let result = serde_json::json!({
        "primary_model": primary_model,
        "primary_api_type": primary_api_type,
        "primary_api_key_masked": mask_key(&primary_key),
        "primary_base_url": primary_base_url,
        "primary_context_window": primary_window,
        "primary_threshold_ratio": primary_threshold,
        "fallback_chain": fallback_chain,
        "max_retries_per_model": cfg.max_retries_per_model,
    });
    let mut conn_guard = writer.lock().await;
    let _ = conn_guard.send_response(id, result).await;
}

pub(super) async fn scm_config_show(shared: &SharedState, writer: WriterRef<'_>, id: RequestId) {
    // Serialize config with secret values masked
    let mut config_json =
        serde_json::to_value(&shared.baoclaw_config).unwrap_or(serde_json::json!({}));
    mask_secret_values(&mut config_json);

    let mut conn_guard = writer.lock().await;
    let _ = conn_guard
        .send_response(id, serde_json::json!({"config": config_json}))
        .await;
}

pub(super) async fn scm_evolution_rate_trajectory(
    shared: &SharedState,
    writer: WriterRef<'_>,
    id: RequestId,
    rating: String,
) {
    use baoclaw_core::engine::evolution::TrajectoryRating;
    let parsed = match rating.to_ascii_lowercase().as_str() {
        "good" => TrajectoryRating::Good,
        "bad" => TrajectoryRating::Bad,
        "neutral" => TrajectoryRating::Neutral,
        other => {
            let mut conn_guard = writer.lock().await;
            let _ = conn_guard
                .send_error(
                    Some(id),
                    -32000,
                    format!("invalid rating '{}': expected good, bad, or neutral", other),
                )
                .await;
            return;
        }
    };
    shared.evolution_engine.rate_last_trajectory(parsed).await;
    let mut conn_guard = writer.lock().await;
    let _ = conn_guard
        .send_response(
            id,
            serde_json::json!({
                "success": true,
                "message": "rating recorded for the last trajectory",
            }),
        )
        .await;
}

/// Recursively mask string values under secret-bearing keys (api_key /
/// token / secret / password) anywhere in the config tree — including the
/// flattened `extra` map, where third-party sections like `telegram` keep
/// their credentials. Numbers and non-secret keys pass through untouched.
pub(crate) fn mask_secret_values(value: &mut serde_json::Value) {
    const SHORT_MASK: &str = "****";
    match value {
        serde_json::Value::Object(map) => {
            for (key, v) in map.iter_mut() {
                let key = key.to_lowercase();
                let secret_key = key.contains("api_key")
                    || key.contains("apikey")
                    || key.contains("api-key")
                    || key.contains("token")
                    || key.contains("secret")
                    || key.contains("password");
                if secret_key {
                    if let serde_json::Value::String(s) = v {
                        if !s.is_empty() && !s.contains(SHORT_MASK) {
                            *s = if s.chars().count() > 8 {
                                let chars: Vec<char> = s.chars().collect();
                                let head: String = chars[..4].iter().collect();
                                let tail: String = chars[chars.len() - 4..].iter().collect();
                                format!("{}{}{}", head, SHORT_MASK, tail)
                            } else {
                                SHORT_MASK.to_string()
                            };
                        }
                        continue;
                    }
                }
                mask_secret_values(v);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items.iter_mut() {
                mask_secret_values(item);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
pub(crate) mod mask_tests {
    use super::mask_secret_values;
    use serde_json::json;

    #[test]
    fn test_masks_nested_extra_secrets() {
        // Configs flatten unknown sections (e.g. telegram) into `extra`;
        // their credentials must not survive config.show.
        let mut v = json!({
            "model": "m",
            "extra": {
                "telegram": { "token": "123456:ABC-DEF-GHI", "allowedChatIds": [1] },
                "feishu": { "app_secret": "supersecret" }
            }
        });
        mask_secret_values(&mut v);
        assert_eq!(v["extra"]["telegram"]["token"], "1234****-GHI");
        assert_eq!(v["extra"]["feishu"]["app_secret"], "supe****cret");
        assert_eq!(v["extra"]["telegram"]["allowedChatIds"][0], 1);
    }

    #[test]
    fn test_masks_api_keys_everywhere() {
        let mut v = json!({
            "api_key": "sk-ant-verylongkeyvalue",
            "model_profiles": {
                "inferx": { "api_key": "ark-live-key-12345678", "model": "gpt" }
            }
        });
        mask_secret_values(&mut v);
        assert_eq!(v["api_key"], "sk-a****alue");
        assert_eq!(v["model_profiles"]["inferx"]["api_key"], "ark-****5678");
        assert_eq!(v["model_profiles"]["inferx"]["model"], "gpt");
    }

    #[test]
    fn test_leaves_non_secrets_untouched() {
        let mut v = json!({
            "max_tokens": 16384,
            "model": "claude",
            "openai_base_url": "https://api.example.com"
        });
        mask_secret_values(&mut v);
        // Numeric token counts are not secrets.
        assert_eq!(v["max_tokens"], 16384);
        assert_eq!(v["model"], "claude");
        assert_eq!(v["openai_base_url"], "https://api.example.com");
    }

    #[test]
    fn test_substring_key_match_is_conservative() {
        // Any key containing "token"/"secret"/... is treated as secret when
        // the value is a string — over-masking config.show is acceptable,
        // under-masking is not.
        let mut v = json!({ "token_count_hint": "aggregate only" });
        mask_secret_values(&mut v);
        assert_eq!(v["token_count_hint"], "aggr****only");
    }

    #[test]
    fn test_masks_inside_arrays_and_skips_already_masked() {
        let mut v = json!({
            "profiles": [ { "password": "hunter2" }, { "password": "correct-horse-battery" } ]
        });
        mask_secret_values(&mut v);
        assert_eq!(v["profiles"][0]["password"], "****");
        assert_eq!(v["profiles"][1]["password"], "corr****tery");

        let mut twice = json!({ "token": "abcd****wxyz" });
        mask_secret_values(&mut twice);
        assert_eq!(
            twice["token"], "abcd****wxyz",
            "double-masking must not mangle"
        );
    }
}
