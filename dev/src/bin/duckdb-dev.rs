use std::{
    env,
    ffi::OsString,
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    process::{Command, ExitCode, ExitStatus, Stdio},
    time::{Instant, SystemTime, UNIX_EPOCH},
};
use tracing_subscriber::{Registry, layer::SubscriberExt};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace")
        .to_owned()
}

fn directory(root: &Path, argument: Option<&OsString>) -> io::Result<PathBuf> {
    let latest = || {
        fs::read_to_string(root.join("target/dev-traces/latest"))
            .map(|value| PathBuf::from(value.trim()))
    };
    let Some(argument) = argument else {
        return latest();
    };
    let path = PathBuf::from(argument);
    if path.is_dir() {
        return Ok(path);
    }
    if path.is_file() {
        return Ok(PathBuf::from(fs::read_to_string(path)?.trim()));
    }
    let statement = latest()?.join("statements").join(argument);
    if statement.is_dir() {
        return Ok(statement);
    }
    Err(io::Error::other(
        "trace directory or execution ID not found",
    ))
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            eprintln!("dev: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> io::Result<u8> {
    let mut arguments = env::args_os().skip(1);
    let command = arguments.next().unwrap_or_else(|| "help".into());
    let arguments = arguments.collect::<Vec<_>>();
    let root = root();
    let _reader = matches!(
        command.to_str(),
        Some("statements" | "log" | "span" | "sql" | "index")
    )
    .then(|| duckdb_dev::artifacts::read_lease(&root))
    .transpose()?;
    match command.to_str() {
        Some("clean") => {
            if !arguments.is_empty() {
                return Err(io::Error::other("usage: cargo dev clean"));
            }
            duckdb_dev::artifacts::clean(&root)?;
            eprintln!("dev: telemetry removed");
            Ok(0)
        }
        Some("trace") => {
            let mut arguments = arguments.into_iter().peekable();
            let keep = arguments.peek().is_some_and(|arg| arg == "--keep");
            if keep {
                arguments.next();
            }
            let action = arguments.next().ok_or_else(|| {
                io::Error::other("usage: cargo dev trace [--keep] run|test|check|clippy|build ...")
            })?;
            if !matches!(
                action.to_str(),
                Some("run" | "test" | "check" | "clippy" | "build")
            ) {
                return Err(io::Error::other(
                    "trace expects run, test, check, clippy or build",
                ));
            }
            let session = duckdb_dev::artifacts::Session::begin(&root)?;
            let result = execute(&root, action, arguments.collect());
            session.finish(keep)?;
            result
        }
        Some("coverage") => {
            let write = arguments.as_slice() == [OsString::from("--write")];
            if !arguments.is_empty() && !write {
                return Err(io::Error::other("usage: cargo dev coverage [--write]"));
            }
            let report = duckdb_dev::coverage::audit(&root, write)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            Ok(u8::from(!report.missing.is_empty() && !write))
        }
        Some("statements") => {
            if arguments.len() > 1 {
                return Err(io::Error::other(
                    "usage: cargo dev statements [RUN_DIRECTORY]",
                ));
            }
            let directory = directory(&root, arguments.first())?;
            let mut statements = Vec::new();
            for trace in duckdb_dev::report::trace_files(&directory)? {
                let path = trace
                    .parent()
                    .expect("trace directory")
                    .join("statement.json");
                if path.is_file() {
                    statements.push(serde_json::from_slice::<serde_json::Value>(&fs::read(
                        path,
                    )?)?);
                }
            }
            println!("{}", serde_json::to_string_pretty(&statements)?);
            Ok(0)
        }
        Some("sql") => {
            if !(1..=2).contains(&arguments.len()) {
                return Err(io::Error::other(
                    "usage: cargo dev sql [TRACE_DIRECTORY_OR_EXECUTION_ID] SQL",
                ));
            }
            let directory = directory(&root, (arguments.len() == 2).then(|| &arguments[0]))?;
            let sql = arguments
                .last()
                .expect("SQL")
                .to_str()
                .ok_or_else(|| io::Error::other("SQL must be UTF-8"))?;
            duckdb_dev::analytics::query(&directory, sql)?;
            Ok(0)
        }
        Some("index") => {
            if arguments.len() > 1 {
                return Err(io::Error::other(
                    "usage: cargo dev index [TRACE_DIRECTORY_OR_EXECUTION_ID]",
                ));
            }
            duckdb_dev::analytics::index(&directory(&root, arguments.first())?)?;
            Ok(0)
        }
        Some("log") => {
            if arguments.len() > 2 {
                return Err(io::Error::other(
                    "usage: cargo dev log [TRACE_DIRECTORY_OR_EXECUTION_ID] [OPERATION_SUBSTRING]",
                ));
            }
            let directory = directory(&root, arguments.first())?;
            let filter = arguments
                .get(1)
                .map(|arg| arg.to_string_lossy().into_owned());
            let (summary, pending) = duckdb_dev::report::cached(&directory, filter.as_deref())?;
            let mut value = if filter.is_some() {
                serde_json::to_value(&summary)?
            } else {
                duckdb_dev::report::overview(&summary)
            };
            value["bytes_after_snapshot"] = pending.into();
            println!("{}", serde_json::to_string_pretty(&value)?);
            Ok(0)
        }
        Some("span") => {
            if arguments.len() != 2 {
                return Err(io::Error::other(
                    "usage: cargo dev span PROCESS_LOG SPAN_ID",
                ));
            }
            let id = arguments[1]
                .to_string_lossy()
                .parse()
                .map_err(io::Error::other)?;
            duckdb_dev::report::span(Path::new(&arguments[0]), id, &mut io::stdout().lock())?;
            Ok(0)
        }
        Some("test" | "run" | "check" | "clippy" | "build") => {
            duckdb_dev::artifacts::clean(&root)?;
            let started = Instant::now();
            let status = Command::new("cargo")
                .current_dir(&root)
                .arg(command)
                .args(["--package", "duckdb-rust", "--no-default-features"])
                .args(arguments)
                .env_remove("DUCKDB_DEV_LOG_DIR")
                .env_remove("DUCKDB_DEV_RUN")
                .env_remove("DUCKDB_DEV_SOURCE")
                .env_remove("DUCKDB_DEV_PROFILE")
                .env_remove("DUCKDB_DEV_LEASE")
                .status()?;
            eprintln!(
                "dev: {:.3} s (tracing off)",
                started.elapsed().as_secs_f64()
            );
            Ok(u8::from(!status.success()))
        }
        _ => {
            println!(
                "cargo dev test|run|check|clippy|build [cargo arguments] (tracing off)\ncargo dev trace [--keep] test|run|check|clippy|build [cargo arguments]\ncargo dev statements [RUN_DIRECTORY]\ncargo dev log [TRACE_DIRECTORY_OR_EXECUTION_ID] [OPERATION_SUBSTRING]\ncargo dev sql [TRACE_DIRECTORY_OR_EXECUTION_ID] SQL\ncargo dev index [TRACE_DIRECTORY_OR_EXECUTION_ID]\ncargo dev span PROCESS_LOG SPAN_ID\ncargo dev coverage [--write]\ncargo dev clean\n\nTrace only focused reproductions. Temporary telemetry is deleted at command completion.\n--keep retains one run until the next execution or cargo dev clean. Never commit telemetry.\nUse --profile dev-trace for optimized diagnostics; production uses --release without dev."
            );
            Ok(0)
        }
    }
}

fn execute(root: &Path, action: OsString, arguments: Vec<OsString>) -> io::Result<u8> {
    let report = duckdb_dev::coverage::audit(root, false)?;
    if !report.missing.is_empty() {
        eprintln!("{}", serde_json::to_string_pretty(&report)?);
        return Err(io::Error::other(
            "uninstrumented operations; run cargo dev coverage --write and review generated macros",
        ));
    }
    if arguments.iter().any(|argument| argument == "--release") {
        return Err(io::Error::other(
            "production excludes dev tracing; use --profile dev-trace for optimized diagnostics",
        ));
    }
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos();
    let run_id = format!("{stamp}-{}", std::process::id());
    let directory = root.join("target/dev-traces").join(&run_id);
    fs::create_dir_all(&directory)?;
    fs::write(
        root.join("target/dev-traces/latest"),
        directory.to_string_lossy().as_bytes(),
    )?;
    let revision = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(root)
        .output()?;
    let revision = String::from_utf8_lossy(&revision.stdout).trim().to_owned();
    let source = duckdb_dev::source::fingerprint(root)?;
    let status = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(root)
        .output()?;
    let profile = arguments
        .windows(2)
        .find_map(|args| (args[0] == "--profile").then(|| args[1].to_string_lossy().into_owned()))
        .or_else(|| {
            arguments
                .iter()
                .find_map(|arg| arg.to_str()?.strip_prefix("--profile=").map(str::to_owned))
        })
        .unwrap_or_else(|| "dev".into());
    let cargo_arguments = [
        vec![
            action,
            "--package".into(),
            "duckdb-rust".into(),
            "--features".into(),
            "dev".into(),
        ],
        arguments,
    ]
    .concat();
    let mut manifest = serde_json::json!({"run": run_id, "source": source, "revision": revision,
        "worktree": String::from_utf8_lossy(&status.stdout), "profile": profile,
        "command": cargo_arguments.iter().map(|arg| arg.to_string_lossy()).collect::<Vec<_>>(),
        "coverage": report, "status": "running"});
    fs::write(
        directory.join("run.json"),
        serde_json::to_vec_pretty(&manifest)?,
    )?;
    eprintln!(
        "dev run: {}\ninspect: cargo dev log {}",
        directory.display(),
        directory.display()
    );
    let started = Instant::now();
    let log =
        duckdb_dev::FileLog::create(&directory.join(format!("{}.jsonl", std::process::id())))?;
    let execution = tracing::subscriber::with_default(
        Registry::default().with(duckdb_dev::TraceLayer::new(log)),
        || {
            let operation = duckdb_dev::Operation::enter(tracing::trace_span!(
                "dev.iteration",
                run = run_id.as_str(),
                source = source.as_str(),
                profile = profile.as_str(),
                outcome = tracing::field::Empty,
                error = tracing::field::Empty
            ));
            let result = execute_cargo(
                root,
                &cargo_arguments,
                &directory,
                &run_id,
                &source,
                &profile,
            );
            match &result {
                Ok(exit) if exit.success() => operation.result(&Ok::<_, String>(())),
                Ok(exit) => operation.result(&Err::<(), _>(format!("cargo terminated: {exit}"))),
                Err(error) => operation.result(&Err::<(), _>(error)),
            }
            result
        },
    );
    manifest["elapsed_ns"] = serde_json::json!(started.elapsed().as_nanos());
    manifest["status"] = if execution.as_ref().is_ok_and(ExitStatus::success) {
        "passed"
    } else {
        "failed"
    }
    .into();
    manifest["exit_code"] = serde_json::json!(execution.as_ref().ok().and_then(ExitStatus::code));
    manifest["execution_error"] =
        serde_json::json!(execution.as_ref().err().map(ToString::to_string));
    // Publish the actual exit status before any subsequent reporting can fail.
    fs::write(
        directory.join("run.json"),
        serde_json::to_vec_pretty(&manifest)?,
    )?;
    let unchanged = source == duckdb_dev::source::fingerprint(root)?;
    manifest["source_unchanged"] = unchanged.into();
    let (summary, pending) = match duckdb_dev::report::cached(&directory, None) {
        Ok(summary) => summary,
        Err(error) => {
            manifest["trace_complete"] = false.into();
            manifest["summary_error"] = error.to_string().into();
            fs::write(
                directory.join("run.json"),
                serde_json::to_vec_pretty(&manifest)?,
            )?;
            return Err(error);
        }
    };
    manifest["trace_complete"] = (summary.incomplete.is_empty() && pending == 0).into();
    manifest["bytes_after_snapshot"] = pending.into();
    fs::write(
        directory.join("run.json"),
        serde_json::to_vec_pretty(&manifest)?,
    )?;
    fs::write(
        directory.join("summary.json"),
        serde_json::to_vec_pretty(&summary)?,
    )?;
    eprintln!(
        "dev: {} completed operations, {} error returns, {} panics, {} open spans\nsummary: {}",
        summary.completed,
        summary.errors,
        summary.panics,
        summary.incomplete.len(),
        directory.join("summary.json").display()
    );
    eprintln!(
        "{}",
        serde_json::to_string_pretty(&duckdb_dev::report::overview(&summary))?
    );
    // Crash-injection tests deliberately leave child spans open. Preserve that
    // evidence separately from the harness's verified expected exit status.
    Ok(if execution?.success() && unchanged {
        0
    } else {
        1
    })
}

fn forward(mut input: impl Read, path: &Path, mut console: impl Write) -> io::Result<u64> {
    let mut file = fs::File::create(path)?;
    let mut buffer = [0; 8192];
    let mut bytes = 0;
    loop {
        let count = input.read(&mut buffer)?;
        if count == 0 {
            return Ok(bytes);
        }
        file.write_all(&buffer[..count])?;
        console.write_all(&buffer[..count])?;
        console.flush()?;
        bytes += count as u64;
    }
}

fn execute_cargo(
    root: &Path,
    arguments: &[OsString],
    directory: &Path,
    run: &str,
    source: &str,
    profile: &str,
) -> io::Result<ExitStatus> {
    let mut child = Command::new("cargo")
        .args(arguments)
        .current_dir(root)
        .env("DUCKDB_DEV_LOG_DIR", directory)
        .env("DUCKDB_DEV_RUN", run)
        .env("DUCKDB_DEV_SOURCE", source)
        .env("DUCKDB_DEV_PROFILE", profile)
        .env("DUCKDB_DEV_LEASE", duckdb_dev::artifacts::lease_path(root))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    duckdb_dev::value("cargo.pid", &child.id());
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("missing child stdout"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| io::Error::other("missing child stderr"))?;
    std::thread::scope(|scope| {
        let output =
            scope.spawn(|| forward(stdout, &directory.join("stdout.log"), io::stdout().lock()));
        let errors =
            scope.spawn(|| forward(stderr, &directory.join("stderr.log"), io::stderr().lock()));
        let status = child.wait();
        for (name, writer) in [("stdout_bytes", output), ("stderr_bytes", errors)] {
            let bytes = writer
                .join()
                .map_err(|_| io::Error::other("output recorder panicked"))??;
            duckdb_dev::value(name, &bytes);
        }
        status
    })
}
