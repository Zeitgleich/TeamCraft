use std::process::ExitCode;
use std::time::Duration;

use anyhow::Context;
use teamcraft_import::args::{self, Command, Inputs};
use teamcraft_import::export::Export;
use teamcraft_import::mapping::Mapping;
use teamcraft_import::progress::Progress;
use teamcraft_import::run::{Importer, RunOptions};
use teamcraft_import::{app, auth, files, report};
use tracing::{error, info, warn};

/// How often `serve` looks for people who set up the app and conversations it can leave.
const TEND_INTERVAL: Duration = Duration::from_secs(5 * 60);
/// How often `serve` keeps the gateway and its heartbeats going.
const GATEWAY_TICK: Duration = Duration::from_secs(5);

fn main() -> ExitCode {
  tracing_subscriber::fmt()
    .with_writer(std::io::stderr)
    .with_env_filter(
      tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
    )
    .init();
  let parsed = match args::parse(std::env::args().skip(1)) {
    Ok(parsed) => parsed,
    Err(problem) => {
      files::say(&format!("{problem}\n\n{}", args::USAGE));
      return ExitCode::from(2);
    },
  };
  let runtime = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
    Ok(runtime) => runtime,
    Err(problem) => {
      error!(%problem, "no async runtime");
      return ExitCode::FAILURE;
    },
  };
  match runtime.block_on(execute(parsed)) {
    Ok(()) => ExitCode::SUCCESS,
    Err(problem) => {
      error!("{problem:#}");
      ExitCode::FAILURE
    },
  }
}

async fn execute(parsed: args::Args) -> anyhow::Result<()> {
  files::make_state_dir(&parsed.state).context("the state directory")?;
  let state = parsed.state;
  let (inputs, command) = match parsed.command {
    Command::Login { server } => {
      auth::login(&server, &state).await?;
      let session = auth::resume(&state).await?;
      let client = app::open_client(&state, session).await?;
      files::say(&format!(
        "The import device is ready (account {}). An owner confirms it in the workspace roster before the import.",
        client.user_id()
      ));
      return Ok(());
    },
    Command::Plan(inputs) => (inputs, "plan"),
    Command::Run(inputs) => (inputs, "run"),
    Command::Serve(inputs) => (inputs, "serve"),
    Command::Report(inputs) => (inputs, "report"),
  };
  let (export, mapping) = read_inputs(&inputs)?;
  let mut progress = Progress::open(&state.join("progress.sqlite")).context("the progress database")?;
  let session = auth::resume(&state).await?;
  let mut client = app::open_client(&state, session).await?;
  client.full_sync().await.context("the first sync")?;
  if command == "plan" {
    let plan = teamcraft_import::run::dry_plan(&mut client, &export, &mapping, &progress).await?;
    return write_report(
      &state,
      "report-plan.md",
      &report::render(&plan, &export, &progress, true)?,
    );
  }
  let options = RunOptions {
    deleted_name: inputs.language.deleted_name().to_owned(),
    join_wait: teamcraft_import::run::DEFAULT_JOIN_WAIT,
    maybe_max_file_bytes: inputs.maybe_max_file_bytes,
  };
  let mut importer = Importer::start(&mut client, &export, &mapping, &mut progress, options).await?;
  match command {
    "run" => {
      let plan = tokio::select! {
        result = importer.run() => result?,
        _ = tokio::signal::ctrl_c() => {
          warn!("stopped; run again to go on");
          return Ok(());
        },
      };
      importer.tend(&plan).await?;
      let text = report::render(&plan, &export, importer.progress, false)?;
      write_report(&state, "report.md", &text)
    },
    "serve" => {
      let plan = importer.plan();
      let mut last_tend = std::time::Instant::now() - TEND_INTERVAL;
      loop {
        if last_tend.elapsed() >= TEND_INTERVAL {
          last_tend = std::time::Instant::now();
          if let Err(problem) = importer.tend(&plan).await {
            warn!(%problem, "tending the import failed; trying again later");
          }
          let text = report::render(&plan, &export, importer.progress, false)?;
          write_report(&state, "report.md", &text)?;
        }
        tokio::select! {
          () = tokio::time::sleep(GATEWAY_TICK) => importer.keep_gateway().await,
          _ = tokio::signal::ctrl_c() => {
            info!("stopped");
            return Ok(());
          },
        }
      }
    },
    _ => {
      let plan = importer.plan();
      write_report(
        &state,
        "report.md",
        &report::render(&plan, &export, importer.progress, false)?,
      )
    },
  }
}

fn read_inputs(inputs: &Inputs) -> anyhow::Result<(Export, Mapping)> {
  let export = Export::open(&inputs.export, inputs.maybe_uploads.as_deref()).context("reading the export")?;
  let mapping = Mapping::read(&inputs.channels, &inputs.users).context("reading the mappings")?;
  info!(
    channels = export.channels.len(),
    users = export.users.len(),
    mapped_users = mapping.users.len(),
    "read the export and the mappings"
  );
  Ok((export, mapping))
}

fn write_report(state: &std::path::Path, name: &str, text: &str) -> anyhow::Result<()> {
  let path = state.join(name);
  files::write_private(&path, text.as_bytes()).with_context(|| format!("writing {}", path.display()))?;
  files::say(text);
  files::say(&format!("Written to {}", path.display()));
  Ok(())
}
