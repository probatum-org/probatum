//! Output: one glance. Green checks, and for reds the extracted cause — nothing
//! else. No log spelunking.

use crate::runner::{RunReport, Status};

const RED: &str = "\x1b[31m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const DIM: &str = "\x1b[2m";
const BOLD: &str = "\x1b[1m";
const RESET: &str = "\x1b[0m";
const WITHHELD_HINT: &str =
    "response/output excerpts withheld: this scenario captures or consumes values";

pub fn print(r: &RunReport) {
    println!();
    let show_scenarios = r.checks.iter().any(|c| c.scenario != "default");
    let mut scenario = None;
    for c in &r.checks {
        if show_scenarios && scenario != Some(&c.scenario) {
            let prerequisite = if r.prerequisites.contains(&c.scenario) {
                " (prerequisite)"
            } else {
                ""
            };
            println!("{BOLD}[{}]{prerequisite}{RESET}", c.scenario);
            scenario = Some(&c.scenario);
        }
        match c.status {
            Status::Passed => {
                let extra = c
                    .detail
                    .as_deref()
                    .map(|d| format!(" {DIM}({d}){RESET}"))
                    .unwrap_or_default();
                // Time is shown only when it is worth noticing. Every duration
                // is in run.json regardless — this line stays a verdict, not a
                // report.
                let slow = if c.duration_ms >= 1000 {
                    format!(" {DIM}{:.1}s{RESET}", c.duration_ms as f64 / 1000.0)
                } else {
                    String::new()
                };
                println!("  {GREEN}✓{RESET} {}{extra}{slow}", c.label);
            }
            Status::Errored => {
                println!(
                    "  {YELLOW}⚠{RESET} {} {DIM}({}){RESET}",
                    c.label,
                    c.detail.as_deref().unwrap_or("couldn't run")
                );
                if c.output_withheld {
                    println!("      {DIM}{WITHHELD_HINT}{RESET}");
                }
            }
            Status::Skipped | Status::Excluded => {
                println!(
                    "  {DIM}– {} ({}){RESET}",
                    c.label,
                    c.detail.as_deref().unwrap_or("not executed")
                );
            }
            Status::Failed => {
                println!(
                    "  {RED}✗ {}{RESET} {DIM}({}){RESET}",
                    c.label,
                    c.detail.as_deref().unwrap_or("failed")
                );
                if let Some(cause) = &c.cause {
                    println!("      {}", cause.headline.trim());
                    for line in &cause.correlated {
                        println!("        {DIM}{line}{RESET}");
                    }
                } else if c.output_withheld {
                    println!("      {DIM}{WITHHELD_HINT}{RESET}");
                }
            }
        }
    }

    println!();
    match r.verdict.as_str() {
        "pass" => {
            println!(
                "{GREEN}{BOLD}✓ all passed{RESET} {DIM}({} checks){RESET}",
                r.executed
            );
        }
        "couldn't-run" => {
            if r.reason == Some("dependency_unavailable") {
                println!("{YELLOW}{BOLD}⚠ couldn't run — required captures unavailable{RESET}");
            } else if r.executed == 0 {
                println!("{YELLOW}{BOLD}⚠ nothing verified — no applicable checks{RESET}");
            } else {
                println!(
                "{YELLOW}{BOLD}⚠ couldn't run{RESET} {DIM}({} check(s) — no failures observed){RESET}",
                r.errored
                );
            }
        }
        _ => {
            let mut parts = vec![format!("{} failed", r.failed)];
            if r.errored > 0 {
                parts.push(format!("{} couldn't run", r.errored));
            }
            if r.skipped > 0 {
                parts.push(format!("{} skipped", r.skipped));
            }
            println!("{RED}{BOLD}✗ {}{RESET}", parts.join(" · "));
        }
    }
    if r.excluded > 0 || r.skipped > 0 {
        println!(
            "{DIM}  {} evaluated · {} excluded · {} skipped{RESET}",
            r.executed, r.excluded, r.skipped
        );
    }
    println!("{DIM}  logs: {}{RESET}", r.run_dir);
    println!();
}
