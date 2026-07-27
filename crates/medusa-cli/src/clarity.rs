#![forbid(unsafe_code)]

use std::{
    fs,
    io::{self, Read},
    path::PathBuf,
    process::ExitCode,
};

use clap::{Parser, ValueEnum};
use medusa_clearops::{ClarityConfig, TextKind, analyze};

#[derive(Clone, Copy, Debug, ValueEnum)]
enum Kind {
    Procedural,
    Descriptive,
}

impl From<Kind> for TextKind {
    fn from(value: Kind) -> Self {
        match value {
            Kind::Procedural => Self::Procedural,
            Kind::Descriptive => Self::Descriptive,
        }
    }
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum OutputFormat {
    Text,
    Json,
}

#[derive(Debug, Parser)]
#[command(
    name = "medusa-clarity",
    about = "Lint agent plans and technical text for clear, verifiable communication"
)]
struct Cli {
    /// Read text from this file. Omit the path or use '-' to read standard input.
    path: Option<PathBuf>,

    /// Apply procedural or descriptive sentence limits.
    #[arg(long, value_enum, default_value_t = Kind::Descriptive)]
    kind: Kind,

    /// Override the default sentence word limit.
    #[arg(long)]
    max_words: Option<usize>,

    /// Select human-readable or JSON output.
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    format: OutputFormat,

    /// Return exit code 1 when the linter finds a problem.
    #[arg(long)]
    fail_on_findings: bool,
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(error) => {
            eprintln!("medusa-clarity: {error}");
            ExitCode::from(2)
        }
    }
}

fn run() -> Result<ExitCode, Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let text = read_input(cli.path.as_ref())?;
    let report = analyze(
        &text,
        &ClarityConfig {
            kind: cli.kind.into(),
            max_words: cli.max_words,
        },
    );

    match cli.format {
        OutputFormat::Json => println!("{}", serde_json::to_string_pretty(&report)?),
        OutputFormat::Text => print_text_report(&report),
    }

    if cli.fail_on_findings && !report.passed() {
        Ok(ExitCode::FAILURE)
    } else {
        Ok(ExitCode::SUCCESS)
    }
}

fn read_input(path: Option<&PathBuf>) -> io::Result<String> {
    match path {
        Some(path) if path.as_os_str() != "-" => fs::read_to_string(path),
        _ => {
            let mut text = String::new();
            io::stdin().read_to_string(&mut text)?;
            Ok(text)
        }
    }
}

fn print_text_report(report: &medusa_clearops::Report) {
    if report.passed() {
        println!(
            "PASS: no clarity findings (word limit: {}).",
            report.word_limit
        );
        return;
    }

    println!(
        "FAIL: {} clarity finding(s) (word limit: {}).",
        report.findings.len(),
        report.word_limit
    );
    for finding in &report.findings {
        println!(
            "{:?} {:?} line {}: {}\n  {}",
            finding.rule, finding.severity, finding.line, finding.message, finding.excerpt
        );
    }
}
