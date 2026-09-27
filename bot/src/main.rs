use std::process::ExitCode;

use rageguard::{
    app,
    config::{AppConfig, EnvConfig},
    logging,
};
use tracing::{error, info, warn};

#[tokio::main]
async fn main() -> ExitCode {
    // `.env` is optional (variables may come from the real environment), but a malformed one
    // is an error.
    if let Err(e) = dotenvy::dotenv()
        && !e.not_found()
    {
        eprintln!("error: failed to read .env: {e}");
        return ExitCode::FAILURE;
    }

    let env = match EnvConfig::from_env() {
        Ok(env) => env,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    logging::init(&env.log_level);

    let config = match AppConfig::load(env) {
        Ok(config) => config,
        Err(e) => {
            error!("{e}");
            return ExitCode::FAILURE;
        }
    };

    match &config.file_source {
        Some(path) => info!(path = %path.display(), "loaded detection config"),
        None => warn!(
            path = %config.env.config_path.display(),
            "detection config file not found; using built-in defaults"
        ),
    }
    let rules = &config.file.anger_detection;
    info!(
        threshold = %rules.threshold,
        required_detections = rules.required_detections,
        window_seconds = rules.window_seconds,
        timeout_minutes = rules.timeout_minutes,
        segment_seconds = %rules.segment_seconds,
        ai_service = %config.env.ai_service_url,
        "configuration"
    );
    for warning in rules.warnings() {
        warn!("config: {warning}");
    }
    if config.env.monitor_only {
        warn!("MONITOR_ONLY is enabled: RageGuard will log timeouts but never apply them");
    } else {
        warn!("MONITOR_ONLY is disabled: RageGuard WILL apply real Discord timeouts");
    }

    match app::run(config).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            error!("{e:#}");
            ExitCode::FAILURE
        }
    }
}
