//! `lumbre-code` — Lumbre Código on the command line.
//!
//! Everything except `ask` and `status` works on its own, without the daemon or a model. Nothing
//! is written or executed unless the user granted it for the repository with `allow`.

use std::io::{IsTerminal, Read, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use lumbre_api::{Answer, ChatEvent, ChatRequest, Client, Paths};
use lumbre_code::patch::{self, ChangeKind};
use lumbre_code::{Grant, Permissions, Workspace};

#[derive(Debug, Parser)]
#[command(
    name = "lumbre-code",
    version,
    about = "Lumbre Código: understand a repository, review and apply diffs, run what you allowed."
)]
struct Cli {
    /// Repository to work in (default: the current directory)
    #[arg(short = 'C', long, global = true, value_name = "DIR")]
    dir: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Summarise the repository: size, languages, build manifests
    Overview,
    /// Find files whose path contains every word
    Files { query: Vec<String> },
    /// Search file contents with a regular expression
    Grep {
        pattern: String,
        /// Stop after this many matching lines
        #[arg(long, default_value_t = 200)]
        max: usize,
    },
    /// Find definitions (functions, types, classes) by name; heuristic, see the docs
    Symbols { query: Option<String> },
    /// Check or apply a unified diff (from a file, or `-` for standard input)
    Diff {
        #[command(subcommand)]
        action: DiffAction,
    },
    /// Run a command in the repository, if you allowed it: `lumbre-code run -- cargo test`
    Run {
        #[arg(trailing_var_arg = true, required = true)]
        command: Vec<String>,
    },
    /// Read-only git (status, diff, log, show, branch); anything else needs `allow exec`
    Git {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Show what is allowed in this repository
    Permissions,
    /// Allow writing files or running a command prefix in this repository
    Allow {
        #[command(subcommand)]
        what: Scope,
    },
    /// Take a permission back
    Revoke {
        #[command(subcommand)]
        what: Scope,
    },
    /// Ask the local model about this repository (needs the Lumbre daemon and an installed model)
    Ask {
        question: Vec<String>,
        /// Include a file's contents as context (repeatable)
        #[arg(long = "file", value_name = "PATH")]
        files: Vec<PathBuf>,
    },
    /// Show the state of the Lumbre daemon
    Status,
}

#[derive(Debug, Subcommand)]
enum DiffAction {
    /// Verify that the diff applies cleanly and show what it would change; writes nothing
    Check { source: String },
    /// Apply the diff (needs `allow write`); asks for confirmation unless --yes
    Apply {
        source: String,
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Debug, Subcommand)]
enum Scope {
    /// Applying diffs: creating, modifying and deleting files
    Write,
    /// Running commands that start with this prefix, e.g. "cargo test"
    Exec { prefix: String },
}

type Outcome = Result<ExitCode, String>;

fn text(error: impl std::fmt::Display) -> String {
    error.to_string()
}

fn read_source(source: &str) -> Result<String, String> {
    if source == "-" {
        let mut diff = String::new();
        std::io::stdin().read_to_string(&mut diff).map_err(text)?;
        Ok(diff)
    } else {
        std::fs::read_to_string(source).map_err(|error| format!("{source}: {error}"))
    }
}

fn print_plan(plan: &patch::Plan) {
    for change in &plan.changes {
        let kind = match change.kind {
            ChangeKind::Create => "create",
            ChangeKind::Modify => "modify",
            ChangeKind::Delete => "delete",
        };
        println!("{kind:>7}  {}  (+{} -{})", change.path.display(), change.added, change.removed);
    }
}

fn print_grant(workspace: &Workspace, grant: &Grant, permissions: &Permissions) {
    println!("Repository: {}", workspace.root().display());
    println!("  read:  always (files not ignored by the repository)");
    println!("  write: {}", if grant.write { "allowed (apply diffs)" } else { "not allowed" });
    match grant.exec.as_slice() {
        [] => println!("  exec:  nothing (read-only git is always available)"),
        prefixes => {
            for prefix in prefixes {
                println!("  exec:  {prefix} …");
            }
        }
    }
    println!("Stored in {}", permissions.file().display());
}

fn daemon(paths: &Paths) -> Result<Client, String> {
    Client::discover(paths).map_err(text)
}

fn ask(workspace: &Workspace, paths: &Paths, question: &str, files: &[PathBuf]) -> Outcome {
    let overview = workspace.overview();
    let mut prompt = format!(
        "You are helping with the code repository at `{}` ({} files, {} lines; manifests: {}).\n",
        workspace.root().file_name().map_or_else(|| ".".into(), |name| name.to_string_lossy()),
        overview.files,
        overview.lines,
        overview.manifests.iter().map(|path| path.display().to_string()).collect::<Vec<_>>().join(", "),
    );
    for file in files {
        let path = workspace.resolve(file).map_err(text)?;
        let contents =
            std::fs::read_to_string(&path).map_err(|error| format!("{}: {error}", file.display()))?;
        prompt.push_str(&format!("\nFile `{}`:\n```\n{contents}\n```\n", file.display()));
    }
    prompt.push_str(&format!(
        "\nQuestion about the code: {question}\nIf you propose changes, write them as a unified diff."
    ));

    let client = daemon(paths)?;
    let mut out = std::io::stdout();
    let result = client.chat(&ChatRequest { text: prompt, ..Default::default() }, &mut |event| {
        match event {
            ChatEvent::Loading { model, quant } => eprintln!("[loading {model}:{quant}…]"),
            ChatEvent::Token { text } => {
                let _ = write!(out, "{text}");
                let _ = out.flush();
            }
            _ => {}
        }
        true
    });
    match result.map_err(text)? {
        Some((Answer::Model { model, quant, stats, downgraded, .. }, _)) => {
            println!();
            let speed = stats.tokens_per_sec().map_or(String::new(), |speed| format!(", {speed:.1} tok/s"));
            let note = if downgraded { ", smaller tier than requested" } else { "" };
            eprintln!("[{model}:{quant}, {} ms{speed}{note}]", stats.total_ms);
            Ok(ExitCode::SUCCESS)
        }
        Some((Answer::Tool { text, .. }, _)) => {
            println!("{text}");
            Ok(ExitCode::SUCCESS)
        }
        Some((Answer::NoModel { detail, .. }, _)) => Err(format!("no model available: {detail}")),
        Some((Answer::BackendMissing { detail }, _)) => Err(detail),
        Some((Answer::Deferred { reasons, retry_after_secs }, _)) => {
            let retry = retry_after_secs.map_or(String::new(), |secs| format!("; retry in {secs} s"));
            Err(format!("the machine is busy, nothing was loaded ({reasons:?}){retry}"))
        }
        Some((Answer::Error { message }, _)) => Err(message),
        None => Err("the daemon closed the stream without an answer".into()),
    }
}

fn status(paths: &Paths) -> Outcome {
    let status = daemon(paths)?.status().map_err(text)?;
    let gib = |bytes: u64| bytes as f64 / f64::from(1u32 << 30);
    println!(
        "Lumbre {} (API {}), mode {:?}, up {} s",
        status.version, status.api, status.mode, status.uptime_secs
    );
    println!(
        "Memory: {:.1} of {:.1} GiB available, pressure {:?}",
        gib(status.snapshot.memory.available_bytes),
        gib(status.snapshot.memory.total_bytes),
        status.snapshot.memory.pressure
    );
    println!(
        "Budget: {:.1} GiB for a model, {} threads, verdict {:?}",
        gib(status.budget.model_bytes),
        status.budget.threads,
        status.budget.verdict
    );
    for reason in &status.budget.reasons {
        println!("  because: {reason:?}");
    }
    match &status.loaded {
        Some(model) => println!(
            "Loaded: {}:{} ({}), idle {} s",
            model.id,
            model.quant,
            model
                .memory_bytes
                .map_or("memory unknown".into(), |bytes| format!("{:.2} GiB resident", gib(bytes))),
            model.idle_secs
        ),
        None => println!("Loaded: nothing"),
    }
    match &status.backend.detail {
        None => println!("Backend: {}", status.backend.name),
        Some(detail) => println!("Backend: {} — unavailable: {detail}", status.backend.name),
    }
    if let Some(rss) = status.snapshot.self_rss_bytes {
        println!("Daemon: {:.1} MiB resident", rss as f64 / f64::from(1u32 << 20));
    }
    Ok(ExitCode::SUCCESS)
}

fn execute(cli: Cli) -> Outcome {
    let paths = Paths::discover();
    if matches!(cli.command, Command::Status) {
        return status(&paths);
    }
    let workspace = Workspace::open(cli.dir.unwrap_or_else(|| PathBuf::from("."))).map_err(text)?;
    let mut permissions = Permissions::load(paths.config_dir.join("code-permissions.toml")).map_err(text)?;
    let grant = permissions.grant(workspace.root());

    match cli.command {
        Command::Overview => {
            let overview = workspace.overview();
            println!("{}", workspace.root().display());
            println!(
                "{} files, {} lines of text{}",
                overview.files,
                overview.lines,
                if overview.is_git { ", git repository" } else { "" }
            );
            let mut languages: Vec<_> = overview.by_extension.iter().collect();
            languages.sort_by_key(|(_, (_, lines))| std::cmp::Reverse(*lines));
            for (extension, (files, lines)) in languages.iter().take(12) {
                println!("  .{extension:<10} {files:>5} files {lines:>8} lines");
            }
            for manifest in &overview.manifests {
                println!("  manifest: {}", manifest.display());
            }
        }
        Command::Files { query } => {
            for file in workspace.find_files(&query.join(" ")) {
                println!("{}", file.display());
            }
        }
        Command::Grep { pattern, max } => {
            let matches = workspace.grep(&pattern, max).map_err(text)?;
            for found in &matches {
                println!("{}:{}: {}", found.path.display(), found.line, found.text);
            }
            if matches.len() == max {
                eprintln!("(stopped at {max} lines; raise --max to see more)");
            }
        }
        Command::Symbols { query } => {
            for symbol in workspace.symbols(query.as_deref().unwrap_or("")) {
                println!("{}:{}: {} {}", symbol.path.display(), symbol.line, symbol.kind, symbol.name);
            }
        }
        Command::Diff { action: DiffAction::Check { source } } => {
            let plan = patch::plan(&workspace, &read_source(&source)?).map_err(text)?;
            print_plan(&plan);
            println!("The diff applies cleanly. Nothing was written.");
        }
        Command::Diff { action: DiffAction::Apply { source, yes } } => {
            let plan = patch::plan(&workspace, &read_source(&source)?).map_err(text)?;
            print_plan(&plan);
            if !grant.write {
                return Err(
                    "writing is not allowed in this repository; run `lumbre-code allow write` first".into()
                );
            }
            if !yes {
                // With the diff on stdin there is no way to ask; require an explicit --yes.
                if source == "-" || !std::io::stdin().is_terminal() {
                    return Err("not applied: pass --yes to apply without a prompt".into());
                }
                print!("Apply these changes? [y/N] ");
                std::io::stdout().flush().map_err(text)?;
                let mut reply = String::new();
                std::io::stdin().read_line(&mut reply).map_err(text)?;
                if !matches!(reply.trim(), "y" | "Y" | "yes" | "s" | "S" | "sí" | "si") {
                    println!("Nothing was written.");
                    return Ok(ExitCode::FAILURE);
                }
            }
            plan.write(&workspace).map_err(text)?;
            println!("Applied {} file change(s).", plan.changes.len());
        }
        Command::Run { command } => return run(&workspace, &command, &grant),
        Command::Git { args } => {
            let mut command = vec!["git".to_owned()];
            command.extend(args);
            return run(&workspace, &command, &grant);
        }
        Command::Permissions => print_grant(&workspace, &grant, &permissions),
        Command::Allow { what } => {
            let grant = permissions
                .update(workspace.root(), |grant| match what {
                    Scope::Write => grant.write = true,
                    Scope::Exec { prefix } => {
                        grant.exec.push(prefix.split_whitespace().collect::<Vec<_>>().join(" "))
                    }
                })
                .map_err(text)?;
            print_grant(&workspace, &grant, &permissions);
        }
        Command::Revoke { what } => {
            let grant = permissions
                .update(workspace.root(), |grant| match what {
                    Scope::Write => grant.write = false,
                    Scope::Exec { prefix } => {
                        let prefix = prefix.split_whitespace().collect::<Vec<_>>().join(" ");
                        grant.exec.retain(|existing| *existing != prefix);
                    }
                })
                .map_err(text)?;
            print_grant(&workspace, &grant, &permissions);
        }
        Command::Ask { question, files } => return ask(&workspace, &paths, &question.join(" "), &files),
        Command::Status => unreachable!("handled above"),
    }
    Ok(ExitCode::SUCCESS)
}

fn run(workspace: &Workspace, command: &[String], grant: &Grant) -> Outcome {
    let output = lumbre_code::run(workspace, command, grant).map_err(text)?;
    print!("{}", output.stdout);
    eprint!("{}", output.stderr);
    // Pass the command's own exit code through, so `lumbre-code run` composes in scripts.
    Ok(output.code.and_then(|code| u8::try_from(code).ok()).map_or(ExitCode::FAILURE, ExitCode::from))
}

fn main() -> ExitCode {
    match execute(Cli::parse()) {
        Ok(code) => code,
        Err(message) => {
            eprintln!("lumbre-code: {message}");
            ExitCode::FAILURE
        }
    }
}
