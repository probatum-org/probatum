//! Verdict rendering: one glance tells you everything. Green = say it publicly.
//! Red = the cause is already on screen, no log spelunking.

use crate::runner::{RunReport, Status};

const RED: &str = "\x1b[31m";
const GREEN: &str = "\x1b[32m";
const DIM: &str = "\x1b[2m";
const BOLD: &str = "\x1b[1m";
const RESET: &str = "\x1b[0m";

pub fn print(report: &RunReport) {
    let violated = report.verdict == "violated";
    let (mark, color) = if violated { ("✗", RED) } else { ("✓", GREEN) };

    println!();
    println!(
        "{color}{BOLD}{mark} {} — {}{RESET}",
        report.promise_id, report.promise_statement.trim()
    );

    for step in &report.steps {
        match step.status {
            Status::Passed => {
                let extra = step
                    .detail
                    .as_deref()
                    .map(|d| format!(" {DIM}({d}){RESET}"))
                    .unwrap_or_default();
                println!("  {GREEN}✓{RESET} step {}/{} {}{extra}", step.index, report.steps.len(), step.label);
            }
            Status::Skipped => {
                println!("  {DIM}– step {}/{} {} (non exécuté){RESET}", step.index, report.steps.len(), step.label);
            }
            Status::Failed => {
                println!(
                    "  {RED}✗ step {}/{} {} — FAILED{RESET} {DIM}({}){RESET}",
                    step.index,
                    report.steps.len(),
                    step.label,
                    step.detail.as_deref().unwrap_or("")
                );
                if let Some(cause) = &step.cause {
                    println!("    ├─ cause : {}", cause.headline.trim());
                    for line in &cause.correlated {
                        println!("    │    {DIM}{line}{RESET}");
                    }
                }
            }
        }
    }

    for oracle in &report.oracles {
        if oracle.held {
            println!("  {GREEN}✓{RESET} oracle {}", oracle.label);
        } else {
            println!(
                "  {RED}✗ oracle {} — {}{RESET}",
                oracle.label,
                oracle.detail.as_deref().unwrap_or("violé")
            );
            for v in oracle.violations.iter().take(4) {
                println!("    │    {DIM}{v}{RESET}");
            }
        }
    }

    for note in &report.context_notes {
        println!("  ├─ contexte : {note}");
    }
    println!("  └─ artefacts : {}/  {DIM}(logs complets, manifeste, run.json, seed {}){RESET}", report.run_dir, report.seed);

    println!();
    if violated {
        println!("{RED}{BOLD}1 promesse violée{RESET} — rejouer : {BOLD}{}{RESET}", report.replay);
    } else {
        println!("{GREEN}{BOLD}promesse tenue{RESET} {DIM}— {}{RESET}", report.replay);
    }
    println!();
}
