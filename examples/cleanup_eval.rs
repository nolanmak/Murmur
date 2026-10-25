//! Versioned synthetic cleanup corpus. Default mode has no provider/credential capability.
use murmur::{
    cleanup::{Plan, Privacy, apply_plan},
    cleanup_config::{NetworkPolicy, Settings},
};
#[tokio::main]
async fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let live = match args.as_slice() {
        [] => false,
        [flag] if flag == "--provider" => true,
        _ => return Err(
            "Usage: cleanup_eval [--provider]; provider mode requires separately enabled cleanup"
                .into(),
        ),
    };
    let service = murmur::cleanup_http::Service::default();
    let configured = if live {
        let policy = NetworkPolicy::load(&NetworkPolicy::path()?)?;
        let settings = Settings::load(&Settings::path()?)?;
        if policy.offline_all || !settings.enabled {
            return Err(
                "Provider evaluation requires opt-in cleanup and configured network policy".into(),
            );
        }
        Some(settings)
    } else {
        None
    };
    let corpus: serde_json::Value =
        serde_json::from_str(include_str!("../tests/fixtures/cleanup/v1.json"))
            .map_err(|_| "Invalid corpus")?;
    let mut passed = 0;
    let mut fallback = 0;
    let started = std::time::Instant::now();
    let cases = corpus["cases"].as_array().ok_or("Invalid corpus")?;
    for case in cases {
        let raw = case["raw"].as_str().ok_or("Invalid corpus")?;
        let language = case["language"].as_str().ok_or("Invalid corpus")?;
        let expected = case["cleaned"].as_str();
        let began = std::time::Instant::now();
        let actual = if let Some(settings) = &configured {
            let profile = murmur::vocabulary::Profile {
                language: language.into(),
                ..Default::default()
            };
            Some(
                service
                    .run(
                        raw,
                        &profile,
                        Ok(settings),
                        Privacy::Configured,
                        std::sync::Arc::new(std::sync::atomic::AtomicU8::new(1)),
                        murmur::config::load_named,
                    )
                    .await?
                    .text,
            )
        } else {
            let plan: Plan =
                serde_json::from_value(case["plan"].clone()).map_err(|_| "Invalid corpus plan")?;
            apply_plan(raw, language, &plan)
        };
        let matches = if live {
            actual.as_deref() == Some(expected.unwrap_or(raw))
        } else {
            actual.as_deref() == expected
        };
        passed += usize::from(matches);
        fallback += usize::from(actual.as_deref().is_none_or(|s| s == raw));
        println!(
            "{}",
            serde_json::json!({"case":case["name"],"golden_match":matches,"raw_or_rejected":actual.as_deref().is_none_or(|s|s==raw),"elapsed_us":began.elapsed().as_micros(),"provider_evaluated":live})
        );
    }
    println!(
        "{}",
        serde_json::json!({"corpus_version":1,"cases":cases.len(),"golden_matches":passed,"raw_or_rejected":fallback,"elapsed_us":started.elapsed().as_micros(),"provider_evaluated":live,"semantic_quality_proven":false})
    );
    if !live && passed != cases.len() {
        return Err("Fixture validation regression".into());
    }
    Ok(())
}
